//! A tool: what the model sees (name, title, description, input and output schemas, hints), the key scopes it needs,
//! and its handler. See README.md, "Adding a tool".

use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt;

use crate::access::AccessRequests;
use crate::redact::Redactor;
use crate::schema::{self, Prop};

/// One API key scope, as the host application writes it, e.g. `Scope("catalog:read")`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Scope(pub &'static str);

impl Scope {
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

/// The API key the server runs with: its name (for messages and access requests) and the scopes it holds. The host
/// application issues, scopes and revokes keys; this crate is told who the key is and trusts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    pub name: String,
    pub scopes: Vec<String>,
}

impl KeyInfo {
    pub fn new(name: &str, scopes: &[&str]) -> KeyInfo {
        KeyInfo { name: name.to_string(), scopes: scopes.iter().map(|scope| scope.to_string()).collect() }
    }

    pub fn holds(&self, scope: Scope) -> bool {
        self.scopes.iter().any(|held| held == scope.as_str())
    }
}

/// What a tool does, shown to the client as MCP's tool annotations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Reads only.
    Read,
    /// Changes something.
    Write,
    /// Changes something, and running it again does nothing more (a recheck, a mark-as-seen).
    IdempotentWrite,
    /// Can delete or replace data.
    DestructiveWrite,
}

impl Kind {
    pub fn annotations(self) -> Value {
        let (read_only, destructive, idempotent) = match self {
            Kind::Read => (true, false, true),
            Kind::Write => (false, false, false),
            Kind::IdempotentWrite => (false, false, true),
            Kind::DestructiveWrite => (false, true, false),
        };
        json!({ "readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": idempotent })
    }
}

/// Why a tool call failed, said plainly: the agent sees exactly this text (redacted) with `isError: true`. Each kind
/// carries what to do next, so the agent neither retries blindly nor files an access request for a rule working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// The arguments: which field, and what's wrong with it.
    Argument { field: String, why: String },
    /// The id, SKU or name sent matches nothing.
    NotFound(String),
    /// A rule said no. That is the rule working, not a missing tool.
    Refused(String),
    /// Anything else that stopped the tool.
    Failed(String),
}

impl ToolError {
    pub fn argument(field: &str, why: impl Into<String>) -> ToolError {
        ToolError::Argument { field: field.to_string(), why: why.into() }
    }

    pub fn not_found(what: impl Into<String>) -> ToolError {
        ToolError::NotFound(what.into())
    }

    pub fn refused(why: impl Into<String>) -> ToolError {
        ToolError::Refused(why.into())
    }

    pub fn failed(why: impl Into<String>) -> ToolError {
        ToolError::Failed(why.into())
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::Argument { field, why } => write!(formatter, "Bad argument `{field}`: {why}"),
            ToolError::NotFound(what) => write!(formatter, "{what} Check the id you sent; list or search to find the right one."),
            ToolError::Refused(why) => write!(
                formatter,
                "{why} This refusal is a rule, not a missing tool: tell the person what was refused and why. Don't file an access \
                 request about it."
            ),
            ToolError::Failed(why) => formatter.write_str(why),
        }
    }
}

impl std::error::Error for ToolError {}

/// What a handler gets besides its arguments.
pub struct Ctx<'a, S> {
    /// The host's own state: a database handle, an HTTP client, or (in the sample) an in-memory shop.
    pub state: &'a S,
    /// The key the server runs with.
    pub key: &'a KeyInfo,
    /// Where access requests go.
    pub requests: &'a dyn AccessRequests,
    /// For any outside text a tool passes on (an error an agent pasted, say).
    pub redactor: &'a Redactor,
}

/// For a tool that takes no arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

/// Everything about a tool but its handler; `.handler(run)` makes the `Tool`.
pub struct Spec {
    /// snake_case, unique across every tool.
    pub name: &'static str,
    pub title: &'static str,
    /// What the model reads to decide when and how to call it.
    pub description: &'static str,
    pub kind: Kind,
    /// Every scope the tool needs. A key without one of them doesn't see the tool.
    pub scopes: &'static [Scope],
    /// `schema::object([...])`, the same fields as the handler's argument struct.
    pub input: Prop,
    /// `schema::object([...])`: what every successful result looks like. Results that don't match fail loudly.
    pub output: Prop,
}

type Run<S> = Box<dyn Fn(&Ctx<'_, S>, Value) -> Result<Value, ToolError> + Send + Sync>;

pub struct Tool<S> {
    pub spec: Spec,
    run: Run<S>,
    /// At least one of these too, on top of `spec.scopes`.
    pub(crate) any_of: &'static [Scope],
    /// Every key sees it, whatever its scopes (the access request tool).
    pub(crate) open: bool,
    /// The argument struct's fields, for the check that they match `spec.input`.
    pub(crate) args_fields: fn() -> Option<&'static [&'static str]>,
}

impl Spec {
    /// The handler gets its arguments already checked against `input` (defaults filled in) and read into `A`, a
    /// `#[derive(Deserialize)] #[serde(deny_unknown_fields)]` struct with the same fields. It returns a JSON object
    /// that matches `output`.
    pub fn handler<S, A>(self, run: fn(&Ctx<'_, S>, A) -> Result<Value, ToolError>) -> Tool<S>
    where
        A: for<'de> Deserialize<'de> + 'static,
        S: 'static,
    {
        Tool {
            spec: self,
            run: Box::new(move |ctx, arguments| {
                let args = serde_json::from_value::<A>(arguments).map_err(|error| ToolError::argument("arguments", error.to_string()))?;
                run(ctx, args)
            }),
            any_of: &[],
            open: false,
            args_fields: schema::struct_fields::<A>,
        }
    }
}

impl<S> Tool<S> {
    pub fn name(&self) -> &'static str {
        self.spec.name
    }

    /// The tool as `tools/list` shows it.
    pub fn listing(&self) -> Value {
        json!({
            "name": self.spec.name,
            "title": self.spec.title,
            "description": self.spec.description,
            "inputSchema": self.spec.input.to_json(),
            "outputSchema": self.spec.output.to_json(),
            "annotations": self.spec.kind.annotations(),
        })
    }

    /// Checks the arguments, runs the handler, and checks its result against the output schema.
    pub fn call(&self, ctx: &Ctx<'_, S>, arguments: Value) -> Result<Value, ToolError> {
        let arguments = if arguments.is_null() { json!({}) } else { arguments };
        let checked = self.spec.input.check(arguments, "")?;
        let result = (self.run)(ctx, checked)?;
        self.spec.output.check(result, "").map_err(|error| {
            let why = match error {
                ToolError::Argument { field, why } => format!("`{field}` {why}"),
                other => other.to_string(),
            };
            ToolError::failed(format!("{} answered with a result that breaks its output schema: {why}", self.spec.name))
        })
    }

    /// The tool also needs one of `any`: a key holding none of them doesn't see it.
    pub fn or_scopes(mut self, any: &'static [Scope]) -> Tool<S> {
        self.any_of = any;
        self
    }

    /// Every key sees and may call this tool, whatever its scopes. For the access request tool, which a key missing
    /// a scope must still reach.
    pub fn open_to_every_key(mut self) -> Tool<S> {
        self.open = true;
        self
    }

    /// What this tool needs that `key` doesn't have, each as the key would name it ("catalog:write", or
    /// "a:write or b:write" for a choice). Empty when the key may use it.
    pub fn missing_scopes(&self, key: &KeyInfo) -> Vec<String> {
        if self.open {
            return Vec::new();
        }
        let mut missing: Vec<String> = self.spec.scopes.iter().filter(|scope| !key.holds(**scope)).map(|scope| scope.to_string()).collect();
        if !self.any_of.is_empty() && !self.any_of.iter().any(|scope| key.holds(*scope)) {
            missing.push(self.any_of.iter().map(|scope| scope.as_str()).collect::<Vec<_>>().join(" or "));
        }
        missing
    }

    /// Every scope this tool names, for a host that wants to warn about a key scope no tool uses.
    pub fn scope_names(&self) -> impl Iterator<Item = &'static str> {
        self.spec.scopes.iter().chain(self.any_of).map(|scope| scope.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::MemoryAccessRequests;
    use crate::schema::{integer, object, string};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        name: String,
    }

    fn greet(_: &Ctx<'_, ()>, args: Args) -> Result<Value, ToolError> {
        Ok(json!({ "greeting": format!("hello {}", args.name) }))
    }

    fn wrong(_: &Ctx<'_, ()>, _: Args) -> Result<Value, ToolError> {
        Ok(json!({ "greeting": 5 }))
    }

    fn spec(output: Prop) -> Spec {
        Spec {
            name: "greet",
            title: "Greet",
            description: "Says hello.",
            kind: Kind::Read,
            scopes: &[Scope("a:read")],
            input: object([("name", string().len(1, 20).required())]),
            output,
        }
    }

    fn call(tool: &Tool<()>, arguments: Value) -> Result<Value, ToolError> {
        let key = KeyInfo::new("tester", &[]);
        let requests = MemoryAccessRequests::new();
        let redactor = Redactor::new();
        tool.call(&Ctx { state: &(), key: &key, requests: &requests, redactor: &redactor }, arguments)
    }

    #[test]
    fn a_result_is_checked_against_the_output_schema() -> Result<(), ToolError> {
        let good = spec(object([("greeting", string().required())])).handler(greet);
        assert_eq!(call(&good, json!({ "name": "Ada" }))?, json!({ "greeting": "hello Ada" }));
        let bad = spec(object([("greeting", string().required())])).handler(wrong);
        assert_eq!(
            call(&bad, json!({ "name": "Ada" })).err().map(|error| error.to_string()).as_deref(),
            Some("greet answered with a result that breaks its output schema: `greeting` must be text (got a number)")
        );
        let extra = spec(object([("count", integer().required())])).handler(greet);
        assert!(call(&extra, json!({ "name": "Ada" })).is_err());
        Ok(())
    }

    #[test]
    fn scopes_decide_what_is_missing() {
        static EITHER: [Scope; 2] = [Scope("b:write"), Scope("c:write")];
        let tool = spec(object([("greeting", string().required())])).handler(greet).or_scopes(&EITHER);
        assert_eq!(tool.missing_scopes(&KeyInfo::new("k", &[])), vec!["a:read".to_string(), "b:write or c:write".to_string()]);
        assert!(tool.missing_scopes(&KeyInfo::new("k", &["a:read", "c:write"])).is_empty());
        let open = spec(object([("greeting", string().required())])).handler(greet).open_to_every_key();
        assert!(open.missing_scopes(&KeyInfo::new("k", &[])).is_empty());
    }

    #[test]
    fn errors_say_what_to_do_next() {
        assert_eq!(ToolError::argument("sku", "is required").to_string(), "Bad argument `sku`: is required");
        assert!(ToolError::not_found("No bike has the SKU X.").to_string().ends_with("list or search to find the right one."));
        assert!(ToolError::refused("Not allowed.").to_string().contains("Don't file an access request"));
    }
}
