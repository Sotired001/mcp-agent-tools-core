//! The access request flow: an agent that can't do its work, because no tool does the job or its key lacks a scope,
//! files one request saying what it needs and why, then tells the person it asked and carries on.
//!
//! The server points agents here: a call to a tool the key can't use answers with the missing scope and, when the
//! key can see `request_access`, a line telling it to ask. `request_access` is open to every key, so a key missing
//! a scope can still reach it.
//!
//! Where requests go is the host's business: implement `AccessRequests` (a ticket queue, a chat message, a row in
//! the host's database). `MemoryAccessRequests` keeps them in memory, for tests and the demo. This crate only files
//! requests; granting a scope or building a tool happens in the host application.

use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Mutex;

use crate::hash::sha256_hex;
use crate::schema::{boolean, object, one_of, string};
use crate::tool::{Ctx, Kind, Spec, Tool, ToolError};

/// The access request tool's name, which the server's hints point to.
pub const REQUEST_ACCESS: &str = "request_access";

/// What an agent's request is about.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NeedKind {
    /// Something that exists fails or does the wrong thing.
    Broken,
    /// A tool, scope or ability that doesn't exist yet.
    Missing,
}

impl NeedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NeedKind::Broken => "broken",
            NeedKind::Missing => "missing",
        }
    }
}

/// One request, as the store gets it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessRequest {
    /// The same need, however it is spaced or capitalised, has the same key.
    pub key: String,
    pub need: String,
    pub kind: NeedKind,
    /// The work the agent was trying to do, and the error it got, if any (redacted).
    pub why: String,
    /// The name of the API key that asked.
    pub asked_by: String,
}

/// What filing did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filed {
    pub id: String,
    /// False when an open request with the same key was already there; nothing was added to it.
    pub created: bool,
    /// For a request that was already there: whether its `why` is the same as this one's.
    pub same_words: bool,
}

/// Where access requests go.
pub trait AccessRequests {
    /// Files `request`. When an open request with the same `key` exists, returns it with `created: false` and
    /// changes nothing; otherwise (no request with that key, or only closed ones) files a new open one. Errs only
    /// when the request couldn't be filed.
    fn file(&self, request: AccessRequest) -> Result<Filed, String>;
}

/// A filed request and whether it is still open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredRequest {
    pub id: String,
    pub open: bool,
    pub request: AccessRequest,
}

/// Requests kept in memory, in the order they were filed.
#[derive(Default)]
pub struct MemoryAccessRequests {
    requests: Mutex<Vec<StoredRequest>>,
}

impl MemoryAccessRequests {
    pub fn new() -> MemoryAccessRequests {
        MemoryAccessRequests::default()
    }

    /// Every request filed so far.
    pub fn list(&self) -> Result<Vec<StoredRequest>, String> {
        Ok(self.requests.lock().map_err(|_| "the access request list is unusable after a crash".to_string())?.clone())
    }

    /// Closes the request `id` (it was granted, built or turned down). Errs when there is none.
    pub fn close(&self, id: &str) -> Result<(), String> {
        let mut requests = self.requests.lock().map_err(|_| "the access request list is unusable after a crash".to_string())?;
        let stored = requests.iter_mut().find(|stored| stored.id == id).ok_or(format!("no access request has the id {id}"))?;
        stored.open = false;
        Ok(())
    }
}

impl AccessRequests for MemoryAccessRequests {
    fn file(&self, request: AccessRequest) -> Result<Filed, String> {
        let mut requests = self.requests.lock().map_err(|_| "the access request list is unusable after a crash".to_string())?;
        if let Some(open) = requests.iter().find(|stored| stored.open && stored.request.key == request.key) {
            return Ok(Filed { id: open.id.clone(), created: false, same_words: open.request.why == request.why });
        }
        let id = format!("request-{}", requests.len() + 1);
        requests.push(StoredRequest { id: id.clone(), open: true, request });
        Ok(Filed { id, created: true, same_words: true })
    }
}

/// The key a request for `need` files under.
pub fn request_key(need: &str) -> String {
    let words = need.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    format!("access-request:{}", &sha256_hex(words.as_bytes())[..24])
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestAccessArgs {
    need: String,
    kind: NeedKind,
    trying_to: String,
    error: Option<String>,
}

fn trimmed(field: &str, value: &str, min: usize) -> Result<String, ToolError> {
    let value = value.trim();
    if value.chars().count() < min {
        return Err(ToolError::argument(field, format!("must be at least {min} characters after trimming")));
    }
    Ok(value.to_string())
}

fn request_access<S>(ctx: &Ctx<'_, S>, args: RequestAccessArgs) -> Result<Value, ToolError> {
    let need = trimmed("need", &args.need, 3)?;
    let trying_to = trimmed("trying_to", &args.trying_to, 1)?;
    // An error the agent pastes may hold a key or token: never file one.
    let error = args.error.map(|error| error.trim().to_string()).filter(|error| !error.is_empty()).map(|error| ctx.redactor.redact(&error));
    let why = match error {
        Some(error) => format!("What I was trying to do: {trying_to}\n\nWhat I got: {error}"),
        None => format!("What I was trying to do: {trying_to}"),
    };
    let filed = ctx
        .requests
        .file(AccessRequest { key: request_key(&need), need, kind: args.kind, why, asked_by: ctx.key.name.clone() })
        .map_err(|why| ToolError::failed(format!("The access request couldn't be filed: {why}")))?;
    let note = match (filed.created, filed.same_words) {
        (true, _) => Value::Null,
        (false, true) => json!("This request is already open; nothing was added."),
        (false, false) => json!(
            "This need is already open under an earlier request; the words you wrote this time were not added. If it is a \
             different need, ask again with a different `need`."
        ),
    };
    Ok(json!({ "request_id": filed.id, "new": filed.created, "note": note }))
}

/// The access request tool, open to every key. Add it to the server's tools.
pub fn request_access_tool<S: 'static>() -> Tool<S> {
    Spec {
        name: REQUEST_ACCESS,
        title: "Ask for a tool or access you're missing",
        description: "Only for work you can't do at all: no tool does the job, or your key is missing a scope. need is what's \
missing (\"a tool to rename a bike\", \"my key needs catalog:write\"); trying_to is the work and for whom; error is \
the message you got, if any. kind: \"broken\" when something that exists fails or does the wrong thing; \"missing\" \
when you need a tool, scope or ability that doesn't exist yet. Not for: an error from what you sent (a wrong id, a \
value over a limit: fix it and call again), a not-found, or a refusal that states a rule (tell the person what was \
refused). File one per missing thing. Asking again for the same need adds nothing: the answer says new: false. Then \
tell the person you asked and carry on with what you can do.",
        kind: Kind::Write,
        scopes: &[],
        input: object([
            ("need", string().len(3, 140).required()),
            ("kind", one_of(&["broken", "missing"]).required()),
            ("trying_to", string().len(1, 1200).required()),
            ("error", string().len(0, 700)),
        ]),
        output: object([
            ("request_id", string().required()),
            ("new", boolean().required().about("False when the same need was already open; nothing was added.")),
            ("note", string().nullable().required()),
        ]),
    }
    .handler(request_access::<S>)
    .open_to_every_key()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::Redactor;
    use crate::tool::KeyInfo;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn call(requests: &MemoryAccessRequests, arguments: Value) -> Result<Value, ToolError> {
        let key = KeyInfo::new("helper-bot", &[]);
        let redactor = Redactor::new().secret("s3cret-key");
        request_access_tool::<()>().call(&Ctx { state: &(), key: &key, requests, redactor: &redactor }, arguments)
    }

    #[test]
    fn the_same_need_is_one_request_however_it_is_written() -> TestResult {
        let requests = MemoryAccessRequests::new();
        let first = call(&requests, json!({ "need": "My key needs catalog:write", "kind": "missing", "trying_to": "fix a count" }))?;
        assert_eq!(first, json!({ "request_id": "request-1", "new": true, "note": null }));
        let again = call(&requests, json!({ "need": "  my key   needs CATALOG:WRITE ", "kind": "missing", "trying_to": "fix a count" }))?;
        assert_eq!(again["new"], false);
        assert_eq!(again["note"], "This request is already open; nothing was added.");
        let reworded = call(&requests, json!({ "need": "my key needs catalog:write", "kind": "missing", "trying_to": "something else" }))?;
        assert_eq!((reworded["request_id"].clone(), reworded["new"].clone()), (json!("request-1"), json!(false)));
        assert!(reworded["note"].as_str().unwrap_or_default().starts_with("This need is already open under an earlier request"));
        assert_eq!(requests.list()?.len(), 1);
        Ok(())
    }

    #[test]
    fn a_closed_request_is_asked_again_as_a_new_one() -> TestResult {
        let requests = MemoryAccessRequests::new();
        call(&requests, json!({ "need": "a tool to rename a bike", "kind": "missing", "trying_to": "rename" }))?;
        requests.close("request-1")?;
        let again = call(&requests, json!({ "need": "a tool to rename a bike", "kind": "missing", "trying_to": "rename" }))?;
        assert_eq!(again, json!({ "request_id": "request-2", "new": true, "note": null }));
        assert!(requests.close("request-9").is_err());
        Ok(())
    }

    #[test]
    fn a_pasted_error_is_redacted_and_the_asker_is_the_key() -> TestResult {
        let requests = MemoryAccessRequests::new();
        call(
            &requests,
            json!({ "need": "bike_get fails", "kind": "broken", "trying_to": "read a bike", "error": "500 with Bearer abc and s3cret-key" }),
        )?;
        let stored = requests.list()?;
        let request = &stored[0].request;
        assert_eq!(request.asked_by, "helper-bot");
        assert_eq!(request.kind, NeedKind::Broken);
        assert_eq!(request.why, "What I was trying to do: read a bike\n\nWhat I got: 500 with [REDACTED] and [REDACTED]");
        Ok(())
    }

    #[test]
    fn a_blank_need_is_refused() {
        let requests = MemoryAccessRequests::new();
        let refused = call(&requests, json!({ "need": "  a  ", "kind": "missing", "trying_to": "x" }));
        assert_eq!(refused, Err(ToolError::argument("need", "must be at least 3 characters after trimming")));
    }
}
