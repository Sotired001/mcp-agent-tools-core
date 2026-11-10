//! What every tool must get right. A test runs `problems` over every registered tool, so a malformed tool fails
//! `cargo test` instead of confusing a model at run time.

use serde_json::Value;
use std::collections::HashSet;

use crate::tool::Tool;

/// Every problem found, each as `<tool>: <what's wrong>`. Empty when all is well.
pub fn problems<S>(tools: &[Tool<S>]) -> Vec<String> {
    let mut found = Vec::new();
    for (at, tool) in tools.iter().enumerate() {
        let spec = &tool.spec;
        let name = spec.name;
        let mut say = |problem: String| found.push(format!("{name}: {problem}"));
        if tools[..at].iter().any(|other| other.name() == name) {
            say("another tool has the same name".to_string());
        }
        if name.is_empty()
            || name.len() > 64
            || !name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            say("the name must be 1 to 64 characters of a-z, 0-9 and _".to_string());
        }
        if spec.title.trim().is_empty() || spec.description.trim().is_empty() {
            say("it needs a title and a description".to_string());
        }
        if spec.scopes.is_empty() && tool.any_of.is_empty() && !tool.open {
            say("it names no scopes; list every scope it needs, or mark it open_to_every_key()".to_string());
        }
        for scope in tool.scope_names() {
            if scope.is_empty() || scope.contains(char::is_whitespace) || scope.contains(',') {
                say(format!("the scope {scope:?} must be non-empty, with no spaces or commas"));
            }
        }
        for (what, schema) in [("input", &spec.input), ("output", &spec.output)] {
            let json = schema.to_json();
            if json.get("type").and_then(Value::as_str) != Some("object") || json.get("properties").is_none() {
                say(format!("its {what} must be schema::object([...])"));
            }
            for mistake in schema.mistakes() {
                say(format!("{what}: {mistake}"));
            }
        }
        let mut schema_fields = spec.input.field_names();
        schema_fields.sort_unstable();
        match (tool.args_fields)() {
            Some(fields) => {
                let mut fields = fields.to_vec();
                fields.sort_unstable();
                if fields != schema_fields {
                    say(format!("its argument struct reads {fields:?} but its input declares {schema_fields:?}"));
                }
            }
            None => say("its handler's argument type isn't a struct".to_string()),
        }
    }
    found.extend(wrong_tool_names(tools));
    found
}

/// Every word with an underscore in a tool's text (its description and its fields' descriptions).
fn words(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            let mut word = String::new();
            for character in text.chars().chain(std::iter::once(' ')) {
                if character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_' {
                    word.push(character);
                } else {
                    let trimmed = word.trim_matches('_');
                    if trimmed.contains('_') && trimmed.starts_with(|first: char| first.is_ascii_lowercase()) {
                        found.push(trimmed.to_string());
                    }
                    word.clear();
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| words(item, found)),
        Value::Object(fields) => fields.values().for_each(|item| words(item, found)),
        _ => {}
    }
}

/// Every field name any schema declares.
fn field_names(value: &Value, found: &mut HashSet<String>) {
    match value {
        Value::Object(fields) => {
            for (key, item) in fields {
                if key == "properties"
                    && let Value::Object(properties) = item
                {
                    found.extend(properties.keys().cloned());
                }
                field_names(item, found);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| field_names(item, found)),
        _ => {}
    }
}

/// A tool's text often names other tools to call; a name that doesn't exist sends the agent to a tool it can't call.
/// A word that is a tool's name with its prefix cut off (`stock_adjust` for `bike_stock_adjust`), or a tool's name
/// with a piece added, is reported, unless it is also a field some tool takes or returns.
fn wrong_tool_names<S>(tools: &[Tool<S>]) -> Vec<String> {
    let names: HashSet<&str> = tools.iter().map(Tool::name).collect();
    let mut fields = HashSet::new();
    for tool in tools {
        field_names(&tool.spec.input.to_json(), &mut fields);
        field_names(&tool.spec.output.to_json(), &mut fields);
    }
    let mut wrong = Vec::new();
    for tool in tools {
        let mut found = Vec::new();
        words(&tool.listing(), &mut found);
        for word in found {
            if names.contains(word.as_str()) || fields.contains(&word) {
                continue;
            }
            let cut_off = names.iter().any(|name| name.ends_with(&format!("_{word}")));
            let longer = names.iter().any(|name| word.starts_with(&format!("{name}_")));
            if cut_off || longer {
                wrong.push(format!("{}: its text names `{word}`, which isn't a tool", tool.name()));
            }
        }
    }
    wrong.sort();
    wrong.dedup();
    wrong
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{object, string};
    use crate::tool::{Ctx, Kind, Scope, Spec, ToolError};
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Args {
        sku: String,
    }

    fn run(_: &Ctx<'_, ()>, _: Args) -> Result<Value, ToolError> {
        Err(ToolError::failed("not run"))
    }

    fn spec(name: &'static str, description: &'static str, input: crate::schema::Prop) -> Spec {
        Spec {
            name,
            title: "t",
            description,
            kind: Kind::Read,
            scopes: &[Scope("catalog:read")],
            input,
            output: object([("ok", string().required())]),
        }
    }

    #[test]
    fn a_struct_and_schema_that_disagree_are_caught() {
        let tool = spec("bike_read", "d", object([("skus", string())])).handler(run);
        assert_eq!(problems(&[tool]), vec!["bike_read: its argument struct reads [\"sku\"] but its input declares [\"skus\"]".to_string()]);
    }

    #[test]
    fn a_text_naming_a_tool_that_doesnt_exist_is_caught() {
        let reader = spec("bike_read", "Read one bike. For every bike, call bike_read_all.", object([("sku", string())])).handler(run);
        let other = spec("bike_read_many", "Read several bikes; see read_many for paging.", object([("sku", string())])).handler(run);
        assert_eq!(
            problems(&[reader, other]),
            vec![
                "bike_read: its text names `bike_read_all`, which isn't a tool".to_string(),
                "bike_read_many: its text names `read_many`, which isn't a tool".to_string(),
            ]
        );
    }

    #[test]
    fn a_tool_with_no_scopes_must_say_it_is_open() {
        let mut bare = spec("bike_read", "d", object([("sku", string())]));
        bare.scopes = &[];
        assert_eq!(
            problems(&[bare.handler(run)]),
            vec!["bike_read: it names no scopes; list every scope it needs, or mark it open_to_every_key()".to_string()]
        );
        let mut open = spec("bike_read", "d", object([("sku", string())]));
        open.scopes = &[];
        assert!(problems(&[open.handler(run).open_to_every_key()]).is_empty());
    }
}
