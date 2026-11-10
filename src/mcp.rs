//! MCP over stdio, by hand on serde_json: one JSON-RPC 2.0 message per line in, one per line out. Nothing but
//! protocol messages goes to stdout; logs go to stderr.
//!
//! Answers `initialize`, `ping`, `tools/list`, `tools/call`, and empty `resources/list`,
//! `resources/templates/list` and `prompts/list`; any other request is -32601. Notifications
//! (`notifications/initialized`, `notifications/cancelled`, ...) get no answer, except that `notifications/initialized`
//! is followed by `notifications/tools/list_changed`, so a client that kept an old tool list across a restart
//! reads it again.
//!
//! `Server::handle` takes one line and returns the answer, so the transport is a plain read-line, write-line loop
//! (see `main.rs`) and every protocol path is testable without a process.

use serde_json::{Value, json};

use crate::access::{AccessRequests, REQUEST_ACCESS};
use crate::redact::Redactor;
use crate::tool::{Ctx, KeyInfo, Tool};

/// Newest first. The newest is answered when the client asks for one we don't know.
pub const PROTOCOL_VERSIONS: [&str; 5] = ["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// What the server runs with once the host has worked out who its key is.
pub struct Ready<S> {
    pub state: S,
    pub key: KeyInfo,
    pub requests: Box<dyn AccessRequests>,
}

impl<S> Ready<S> {
    /// What a handler gets.
    pub fn ctx<'a>(&'a self, redactor: &'a Redactor) -> Ctx<'a, S> {
        Ctx { state: &self.state, key: &self.key, requests: self.requests.as_ref(), redactor }
    }
}

pub struct Server<S> {
    name: String,
    version: String,
    tools: Vec<Tool<S>>,
    /// Why it couldn't start, if it couldn't: every request is answered with it, then the server stops.
    state: Result<Ready<S>, String>,
    redactor: Redactor,
}

/// The answer to one line, and whether the server must stop after sending it.
pub struct Handled {
    pub reply: Option<Value>,
    pub stop: bool,
}

fn answer(id: &Value, result: Value) -> Handled {
    Handled { reply: Some(json!({ "jsonrpc": "2.0", "id": id, "result": result })), stop: false }
}

fn error(id: &Value, code: i64, message: &str) -> Handled {
    Handled { reply: Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })), stop: false }
}

fn tool_result(text: String, structured: Option<Value>) -> Value {
    let mut result = json!({ "content": [{ "type": "text", "text": text }], "isError": structured.is_none() });
    if let Some(structured) = structured {
        result["structuredContent"] = structured;
    }
    result
}

impl<S> Server<S> {
    /// `name` and `version` are what `initialize` reports. `state` is `Err(why)` when the host couldn't start (no key,
    /// a bad setting): the server then answers the first request with the reason and stops, so the client shows it.
    pub fn new(name: &str, version: &str, tools: Vec<Tool<S>>, state: Result<Ready<S>, String>, redactor: Redactor) -> Server<S> {
        Server { name: name.to_string(), version: version.to_string(), tools, state, redactor }
    }

    /// The tools this key may use.
    fn allowed<'a>(&'a self, ready: &'a Ready<S>) -> impl Iterator<Item = &'a Tool<S>> {
        self.tools.iter().filter(move |tool| tool.missing_scopes(&ready.key).is_empty())
    }

    /// Handles one line from the client.
    pub fn handle(&self, line: &str) -> Handled {
        let none = Handled { reply: None, stop: false };
        if line.trim().is_empty() {
            return none;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return error(&Value::Null, PARSE_ERROR, "Parse error: each line must be one JSON-RPC message");
        };
        let Some(object) = message.as_object() else {
            return error(&Value::Null, INVALID_REQUEST, "Invalid Request: a message must be a JSON object");
        };
        let id = object.get("id");
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            // An answer to a request of ours (we send none), or nonsense.
            return match id {
                Some(_) if object.contains_key("result") || object.contains_key("error") => none,
                _ => error(id.unwrap_or(&Value::Null), INVALID_REQUEST, "Invalid Request: no method"),
            };
        };
        let Some(id) = id else {
            return match (method, &self.state) {
                ("notifications/initialized", Ok(_)) => {
                    Handled { reply: Some(json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" })), stop: false }
                }
                _ => none, // A notification.
            };
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        let ready = match &self.state {
            Ok(ready) => ready,
            Err(why) => {
                let message = self.redactor.redact(&format!("{} can't start: {why}", self.name));
                return Handled { stop: true, ..error(id, INTERNAL_ERROR, &message) };
            }
        };
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
                let version = PROTOCOL_VERSIONS.iter().find(|version| **version == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
                answer(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {
                            "tools": { "listChanged": true },
                            "resources": { "subscribe": false, "listChanged": false },
                            "prompts": { "listChanged": false },
                        },
                        "serverInfo": { "name": self.name, "version": self.version },
                    }),
                )
            }
            "ping" => answer(id, json!({})),
            "tools/list" => answer(id, json!({ "tools": self.allowed(ready).map(Tool::listing).collect::<Vec<_>>() })),
            "tools/call" => self.call(id, &params, ready),
            "resources/list" => answer(id, json!({ "resources": [] })),
            "resources/templates/list" => answer(id, json!({ "resourceTemplates": [] })),
            "prompts/list" => answer(id, json!({ "prompts": [] })),
            _ => error(id, METHOD_NOT_FOUND, &format!("Method not found: {method}")),
        }
    }

    fn call(&self, id: &Value, params: &Value, ready: &Ready<S>) -> Handled {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error(id, INVALID_PARAMS, "tools/call needs the tool's name");
        };
        let Some(tool) = self.tools.iter().find(|tool| tool.name() == name) else {
            return error(id, INVALID_PARAMS, &format!("Unknown tool: {name}"));
        };
        let missing = tool.missing_scopes(&ready.key);
        if !missing.is_empty() {
            let mut why =
                format!("{name} needs the API key scope {}, which this key ({}) doesn't have.", missing.join(", "), ready.key.name);
            if self.allowed(ready).any(|tool| tool.name() == REQUEST_ACCESS) {
                why.push_str(&format!(
                    " If the work needs it, call {REQUEST_ACCESS} once with the scope and what you were trying to do, and tell the \
                     person you asked."
                ));
            }
            return answer(id, tool_result(self.redactor.redact(&why), None));
        }
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        match tool.call(&ready.ctx(&self.redactor), arguments) {
            Ok(result) => answer(id, tool_result(result.to_string(), Some(result))),
            Err(failure) => answer(id, tool_result(self.redactor.redact(&failure.to_string()), None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{MemoryAccessRequests, request_access_tool};
    use crate::bike_shop::{self, BikeShop};

    const SECRET: &str = "demo_secret_key";

    fn server(scopes: &[&str]) -> Server<BikeShop> {
        let ready =
            Ready { state: BikeShop::seeded(), key: KeyInfo::new("helper-bot", scopes), requests: Box::new(MemoryAccessRequests::new()) };
        Server::new("bike-shop", "0.1.0", bike_shop::tools(), Ok(ready), Redactor::new().secret(SECRET))
    }

    fn reply<S>(server: &Server<S>, message: Value) -> Value {
        server.handle(&message.to_string()).reply.unwrap_or(Value::Null)
    }

    fn listed<S>(server: &Server<S>) -> Vec<Value> {
        reply(server, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))["result"]["tools"]
            .as_array()
            .map(|tools| tools.iter().map(|tool| tool["name"].clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn initialize_answers_the_clients_version_when_known() {
        let server = server(&[]);
        let known =
            reply(&server, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }));
        assert_eq!(known["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(known["result"]["serverInfo"], json!({ "name": "bike-shop", "version": "0.1.0" }));
        let unknown =
            reply(&server, json!({ "jsonrpc": "2.0", "id": "a", "method": "initialize", "params": { "protocolVersion": "2030-01-01" } }));
        assert_eq!((unknown["id"].clone(), unknown["result"]["protocolVersion"].clone()), (json!("a"), json!(PROTOCOL_VERSIONS[0])));
    }

    #[test]
    fn notifications_get_no_answer_and_unknown_methods_say_so() {
        let server = server(&[]);
        assert!(server.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled" }).to_string()).reply.is_none());
        assert!(server.handle("").reply.is_none());
        assert!(server.handle(&json!({ "jsonrpc": "2.0", "id": 4, "result": {} }).to_string()).reply.is_none());
        assert_eq!(reply(&server, json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }))["result"], json!({}));
        assert_eq!(reply(&server, json!({ "jsonrpc": "2.0", "id": 3, "method": "prompts/list" }))["result"], json!({ "prompts": [] }));
        assert_eq!(reply(&server, json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" }))["result"], json!({ "resources": [] }));
        assert_eq!(
            reply(&server, json!({ "jsonrpc": "2.0", "id": 5, "method": "sampling/createMessage" }))["error"]["code"],
            json!(-32601)
        );
        assert_eq!(server.handle("{nope").reply.map(|reply| reply["error"]["code"].clone()), Some(json!(-32700)));
        assert_eq!(server.handle("[1]").reply.map(|reply| reply["error"]["code"].clone()), Some(json!(-32600)));
        assert_eq!(reply(&server, json!({ "jsonrpc": "2.0", "id": 6 }))["error"]["code"], json!(-32600));
    }

    #[test]
    fn initialized_is_answered_with_tools_list_changed() {
        let server = server(&[]);
        let init = reply(&server, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }));
        assert_eq!(init["result"]["capabilities"]["tools"], json!({ "listChanged": true }));
        assert_eq!(
            reply(&server, json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })),
            json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" })
        );
    }

    #[test]
    fn tools_list_shows_only_what_the_key_can_use() {
        assert_eq!(listed(&server(&[])), vec![json!("request_access")], "a key with no scopes can still ask for access");
        let reader = listed(&server(&[bike_shop::CATALOG_READ.as_str()]));
        assert_eq!(reader, vec![json!("bikes_list"), json!("bike_get"), json!("request_access")]);
        let writer = server(&[bike_shop::CATALOG_READ.as_str(), bike_shop::STOCK_WRITE.as_str()]);
        assert_eq!(listed(&writer).len(), 4);
        let tools = reply(&writer, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))["result"]["tools"].clone();
        let adjust = tools.as_array().and_then(|tools| tools.iter().find(|tool| tool["name"] == "bike_stock_adjust")).cloned();
        let adjust = adjust.unwrap_or_default();
        assert_eq!(adjust["annotations"], json!({ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false }));
        assert_eq!(adjust["inputSchema"]["required"], json!(["sku", "change", "reason", "confirmed"]));
        assert_eq!(adjust["outputSchema"]["type"], "object");
    }

    #[test]
    fn a_tool_the_key_cant_use_says_which_scope_is_missing_and_how_to_ask() {
        let reader = server(&["catalog:read"]);
        let result = reply(
            &reader,
            json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": "bike_stock_adjust", "arguments": {} } }),
        )["result"]
            .clone();
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["content"][0]["text"],
            "bike_stock_adjust needs the API key scope stock:write, which this key (helper-bot) doesn't have. If the work needs \
             it, call request_access once with the scope and what you were trying to do, and tell the person you asked."
        );
        let unknown = reply(&reader, json!({ "jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": { "name": "nope" } }));
        assert_eq!(unknown["error"], json!({ "code": -32602, "message": "Unknown tool: nope" }));
    }

    #[test]
    fn without_a_request_tool_the_hint_is_left_out() {
        let ready = Ready { state: BikeShop::seeded(), key: KeyInfo::new("k", &[]), requests: Box::new(MemoryAccessRequests::new()) };
        let tools: Vec<_> = bike_shop::tools().into_iter().filter(|tool| tool.name() != REQUEST_ACCESS).collect();
        let server = Server::new("bike-shop", "0.1.0", tools, Ok(ready), Redactor::new());
        let text = reply(&server, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "bike_get" } }))["result"]
            ["content"][0]["text"]
            .clone();
        assert_eq!(text, "bike_get needs the API key scope catalog:read, which this key (k) doesn't have.");
    }

    #[test]
    fn a_call_answers_text_and_structured_content() {
        let reader = server(&["catalog:read"]);
        let result = reply(
            &reader,
            json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": "bike_get", "arguments": { "sku": "grv-700" } } }),
        )["result"]
            .clone();
        let structured = json!({ "bike": { "sku": "GRV-700", "name": "Gravel Runner 700c", "category": "gravel", "in_stock": 4 } });
        assert_eq!(
            result,
            json!({ "content": [{ "type": "text", "text": structured.to_string() }], "structuredContent": structured, "isError": false })
        );
    }

    #[test]
    fn a_failed_call_is_an_error_result_with_secrets_redacted() {
        let reader = server(&["catalog:read"]);
        let result = reply(
            &reader,
            json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": { "name": "bike_get", "arguments": { "sku": SECRET } } }),
        )["result"]
            .clone();
        assert_eq!(result["isError"], true);
        assert!(result.get("structuredContent").is_none());
        assert_eq!(
            result["content"][0]["text"],
            "No bike has the SKU [REDACTED]. Check the id you sent; list or search to find the right one."
        );
        let bad = reply(
            &reader,
            json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": "bike_get", "arguments": { "sku": 5 } } }),
        );
        assert_eq!(bad["result"]["content"][0]["text"], "Bad argument `sku`: must be text (got a number)");
    }

    #[test]
    fn state_changes_persist_between_calls() {
        let writer = server(&["catalog:read", "stock:write"]);
        let adjust = json!({ "sku": "TRL-29", "change": -1, "reason": "sold one", "confirmed": true });
        let call = |name: &str, arguments: Value| {
            reply(&writer, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": arguments } }))
                ["result"]["structuredContent"]
                .clone()
        };
        assert_eq!(call("bike_stock_adjust", adjust)["after"], 1);
        assert_eq!(call("bike_get", json!({ "sku": "TRL-29" }))["bike"]["in_stock"], 1);
    }

    #[test]
    fn a_server_that_couldnt_start_says_why_and_stops() {
        let server: Server<()> = Server::new(
            "bike-shop",
            "0.1.0",
            vec![request_access_tool()],
            Err(format!("the key {SECRET} was refused")),
            Redactor::new().secret(SECRET),
        );
        let handled = server.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }).to_string());
        assert!(handled.stop);
        assert_eq!(
            handled.reply.map(|reply| reply["error"].clone()),
            Some(json!({ "code": -32603, "message": "bike-shop can't start: the key [REDACTED] was refused" }))
        );
    }
}
