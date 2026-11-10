//! A tool's input, declared once: the JSON Schema `tools/list` shows the model, the checks `tools/call` runs on the
//! arguments (each failure names its field and why), and the defaults filled in before the arguments are read into
//! the tool's own struct. The same builder declares a tool's output schema, which every result is checked against.
//!
//! ```ignore
//! use mcp_agent_tools_core::schema::*;
//! object([
//!     ("task_id", uuid().required().about("The task's id.")),
//!     ("text", string().len(1, 2000).required()),
//!     ("status", one_of(&["open", "done", "dismissed"])),
//!     ("limit", integer().range(1, 50).default(20)),
//! ])
//! ```
//!
//! Optional fields may be left out or sent as `null`; both mean "not given" (and get the default, if any).
//! A field marked `.nullable()` preserves null as a real value; `.required()` still requires its key.

use serde::de::{Deserialize, Deserializer, Visitor};
use serde_json::{Map, Value, json};
use std::cell::Cell;
use std::fmt;

use crate::tool::ToolError;

#[derive(Clone, Debug)]
enum Kind {
    String {
        one_of: Option<&'static [&'static str]>,
        uuid: bool,
    },
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
    Number {
        min: Option<f64>,
        max: Option<f64>,
    },
    Boolean {
        must_be_true: bool,
    },
    Array(Box<Prop>),
    Object(Vec<(&'static str, Prop)>),
    /// Any JSON object (e.g. GraphQL variables).
    FreeObject,
    /// Any JSON value.
    Any,
}

/// One input field (or the whole input, an `object`). Build with the functions below, then chain.
#[derive(Clone, Debug)]
pub struct Prop {
    kind: Kind,
    required: bool,
    /// Preserve an explicit JSON null instead of treating it as omitted.
    nullable: bool,
    about: Option<&'static str>,
    default: Option<Value>,
    /// String length (in UTF-16 units, as JavaScript counts) or array length.
    len: (Option<usize>, Option<usize>),
    /// A builder call that doesn't fit this kind of field; `mistakes()` reports them (a test fails on any).
    mistakes: Vec<String>,
}

fn prop(kind: Kind) -> Prop {
    Prop { kind, required: false, nullable: false, about: None, default: None, len: (None, None), mistakes: Vec::new() }
}

/// Any string.
pub fn string() -> Prop {
    prop(Kind::String { one_of: None, uuid: false })
}

/// An id: 8-4-4-4-12 hex.
pub fn uuid() -> Prop {
    prop(Kind::String { one_of: None, uuid: true })
}

/// One of these strings.
pub fn one_of(values: &'static [&'static str]) -> Prop {
    prop(Kind::String { one_of: Some(values), uuid: false })
}

/// A whole number.
pub fn integer() -> Prop {
    prop(Kind::Integer { min: None, max: None })
}

/// Any number.
pub fn number() -> Prop {
    prop(Kind::Number { min: None, max: None })
}

pub fn boolean() -> Prop {
    prop(Kind::Boolean { must_be_true: false })
}

/// `true` and nothing else: the "a person asked for this" switch on writes.
pub fn must_be_true() -> Prop {
    prop(Kind::Boolean { must_be_true: true })
}

/// A list of `item`.
pub fn array(item: Prop) -> Prop {
    prop(Kind::Array(Box::new(item)))
}

/// An object with exactly these fields (anything else is refused, by name).
pub fn object<const N: usize>(fields: [(&'static str, Prop); N]) -> Prop {
    prop(Kind::Object(fields.into_iter().collect()))
}

/// Any JSON object, not checked.
pub fn free_object() -> Prop {
    prop(Kind::FreeObject)
}

/// Any JSON value, not checked.
pub fn any() -> Prop {
    prop(Kind::Any)
}

fn expected(field: &str, why: impl Into<String>) -> ToolError {
    ToolError::Argument { field: if field.is_empty() { "arguments".to_string() } else { field.to_string() }, why: why.into() }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// An id in the 8-4-4-4-12 hex form, whatever its version and variant bits (ids minted by hand or by older systems
/// often lack them).
pub fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(at, byte)| if [8, 13, 18, 23].contains(&at) { *byte == b'-' } else { byte.is_ascii_hexdigit() })
}

impl Prop {
    /// The key must be given. Null is accepted only when nullable() is set.
    pub fn required(mut self) -> Prop {
        self.required = true;
        self
    }

    /// Null is an explicit value. With required(), the key must still be present.
    pub fn nullable(mut self) -> Prop {
        self.nullable = true;
        self
    }

    /// What the field is, for the model.
    pub fn about(mut self, text: &'static str) -> Prop {
        self.about = Some(text);
        self
    }

    /// Used when the field is left out, or null unless nullable() is set. The struct reads it as a given value.
    pub fn default(mut self, value: impl Into<Value>) -> Prop {
        self.default = Some(value.into());
        self
    }

    /// A string's length in characters, or a list's number of items, from `min` to `max`.
    pub fn len(mut self, min: usize, max: usize) -> Prop {
        match self.kind {
            Kind::String { one_of: None, uuid: false } | Kind::Array(_) => self.len = (Some(min), Some(max)),
            _ => self.mistakes.push("len() is for a string() or an array()".to_string()),
        }
        self
    }

    /// A number from `min` to `max`.
    pub fn range(mut self, min: i64, max: i64) -> Prop {
        match &mut self.kind {
            Kind::Integer { min: low, max: high } => {
                *low = Some(min);
                *high = Some(max);
            }
            Kind::Number { min: low, max: high } => {
                *low = Some(min as f64);
                *high = Some(max as f64);
            }
            _ => self.mistakes.push("range() is for an integer() or a number()".to_string()),
        }
        self
    }

    /// A whole number of `min` or more, with no top.
    pub fn at_least(mut self, min: i64) -> Prop {
        match &mut self.kind {
            Kind::Integer { min: low, max: high } => {
                *low = Some(min);
                *high = None;
            }
            _ => self.mistakes.push("at_least() is for an integer()".to_string()),
        }
        self
    }

    /// Builder calls that don't fit, and defaults that don't pass their own field's checks.
    pub fn mistakes(&self) -> Vec<String> {
        let mut found = self.mistakes.clone();
        if let Some(default) = &self.default
            && let Err(error) = self.check(default.clone(), "default")
        {
            found.push(format!("its default doesn't pass: {error}"));
        }
        match &self.kind {
            Kind::Array(item) => found.extend(item.mistakes().into_iter().map(|mistake| format!("items: {mistake}"))),
            Kind::Object(fields) => {
                for (at, (name, field)) in fields.iter().enumerate() {
                    if fields[..at].iter().any(|(other, _)| other == name) {
                        found.push(format!("{name} is declared twice"));
                    }
                    found.extend(field.mistakes().into_iter().map(|mistake| format!("{name}: {mistake}")));
                }
            }
            _ => {}
        }
        found
    }

    /// The names of an object's fields.
    pub fn field_names(&self) -> Vec<&'static str> {
        match &self.kind {
            Kind::Object(fields) => fields.iter().map(|(name, _)| *name).collect(),
            _ => Vec::new(),
        }
    }

    /// The JSON Schema the model sees.
    pub fn to_json(&self) -> Value {
        let mut schema = Map::new();
        match &self.kind {
            Kind::String { one_of, uuid } => {
                schema.insert("type".into(), json!("string"));
                if let Some(values) = one_of {
                    schema.insert("enum".into(), json!(values));
                }
                if *uuid {
                    schema.insert("format".into(), json!("uuid"));
                }
                if let Some(min) = self.len.0 {
                    schema.insert("minLength".into(), json!(min));
                }
                if let Some(max) = self.len.1 {
                    schema.insert("maxLength".into(), json!(max));
                }
            }
            Kind::Integer { min, max } => {
                schema.insert("type".into(), json!("integer"));
                if let Some(min) = min {
                    schema.insert("minimum".into(), json!(min));
                }
                if let Some(max) = max {
                    schema.insert("maximum".into(), json!(max));
                }
            }
            Kind::Number { min, max } => {
                schema.insert("type".into(), json!("number"));
                if let Some(min) = min {
                    schema.insert("minimum".into(), json!(min));
                }
                if let Some(max) = max {
                    schema.insert("maximum".into(), json!(max));
                }
            }
            Kind::Boolean { must_be_true } => {
                schema.insert("type".into(), json!("boolean"));
                if *must_be_true {
                    schema.insert("const".into(), json!(true));
                }
            }
            Kind::Array(item) => {
                schema.insert("type".into(), json!("array"));
                schema.insert("items".into(), item.to_json());
                if let Some(min) = self.len.0 {
                    schema.insert("minItems".into(), json!(min));
                }
                if let Some(max) = self.len.1 {
                    schema.insert("maxItems".into(), json!(max));
                }
            }
            Kind::Object(fields) => {
                schema.insert("type".into(), json!("object"));
                let properties: Map<String, Value> = fields.iter().map(|(name, field)| (name.to_string(), field.to_json())).collect();
                schema.insert("properties".into(), Value::Object(properties));
                let required: Vec<&str> = fields.iter().filter(|(_, field)| field.required).map(|(name, _)| *name).collect();
                if !required.is_empty() {
                    schema.insert("required".into(), json!(required));
                }
                schema.insert("additionalProperties".into(), json!(false));
            }
            Kind::FreeObject => {
                schema.insert("type".into(), json!("object"));
                schema.insert("additionalProperties".into(), json!(true));
            }
            Kind::Any => {}
        }
        if let Some(about) = self.about {
            schema.insert("description".into(), json!(about));
        }
        if let Some(default) = &self.default {
            schema.insert("default".into(), default.clone());
        }
        if self.nullable && !matches!(self.kind, Kind::Any) {
            json!({ "anyOf": [Value::Object(schema), { "type": "null" }] })
        } else {
            Value::Object(schema)
        }
    }

    /// Checks `value` against this field and fills in defaults. `field` names it in errors (`items[2].name`).
    pub fn check(&self, value: Value, field: &str) -> Result<Value, ToolError> {
        if self.nullable && value.is_null() {
            return Ok(Value::Null);
        }
        let length = |count: usize, unit: &str| -> Result<(), ToolError> {
            match self.len {
                (Some(min), Some(max)) if count < min || count > max => Err(expected(
                    field,
                    if min == max { format!("must be {min} {unit}") } else { format!("must be {min} to {max} {unit}") },
                )),
                _ => Ok(()),
            }
        };
        match (&self.kind, value) {
            (Kind::Any, value) => Ok(value),
            (Kind::String { one_of, uuid }, Value::String(text)) => {
                if let Some(values) = one_of
                    && !values.contains(&text.as_str())
                {
                    return Err(expected(field, format!("must be one of: {} (got \"{text}\")", values.join(", "))));
                }
                if *uuid && !is_uuid(&text) {
                    return Err(expected(field, format!("must be a UUID (got \"{text}\")")));
                }
                length(text.encode_utf16().count(), "characters")?;
                Ok(Value::String(text))
            }
            (Kind::Integer { min, max }, Value::Number(number)) => {
                let Some(whole) = number.as_i64() else {
                    return Err(expected(field, format!("must be a whole number (got {number})")));
                };
                match (min, max) {
                    (Some(min), Some(max)) if whole < *min || whole > *max => {
                        Err(expected(field, format!("must be from {min} to {max} (got {whole})")))
                    }
                    (Some(min), None) if whole < *min => Err(expected(field, format!("must be {min} or more (got {whole})"))),
                    _ => Ok(Value::Number(number)),
                }
            }
            (Kind::Number { min, max }, Value::Number(number)) => {
                let Some(float) = number.as_f64() else {
                    return Err(expected(field, format!("must be a number (got {number})")));
                };
                match (min, max) {
                    (Some(min), Some(max)) if float < *min || float > *max => {
                        Err(expected(field, format!("must be from {min} to {max} (got {number})")))
                    }
                    _ => Ok(Value::Number(number)),
                }
            }
            (Kind::Boolean { must_be_true }, Value::Bool(flag)) => {
                if *must_be_true && !flag {
                    return Err(expected(field, "must be true (set it when the person asked for this change)"));
                }
                Ok(Value::Bool(flag))
            }
            (Kind::Array(item), Value::Array(values)) => {
                length(values.len(), "items")?;
                values
                    .into_iter()
                    .enumerate()
                    .map(|(at, value)| item.check(value, &format!("{field}[{at}]")))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::Array)
            }
            (Kind::FreeObject, Value::Object(map)) => Ok(Value::Object(map)),
            (Kind::Object(fields), Value::Object(mut given)) => {
                let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
                if let Some(unknown) = given.keys().filter(|key| !names.contains(&key.as_str())).min() {
                    let at = if field.is_empty() { unknown.clone() } else { format!("{field}.{unknown}") };
                    return Err(expected(&at, format!("isn't a field here; the fields are: {}", names.join(", "))));
                }
                let mut checked = Map::new();
                for (name, rule) in fields {
                    let at = if field.is_empty() { name.to_string() } else { format!("{field}.{name}") };
                    match given.remove(*name).filter(|value| rule.nullable || !value.is_null()) {
                        Some(value) => {
                            checked.insert(name.to_string(), rule.check(value, &at)?);
                        }
                        None if rule.required => return Err(expected(&at, "is required")),
                        None => {
                            if let Some(default) = &rule.default {
                                checked.insert(name.to_string(), default.clone());
                            }
                        }
                    }
                }
                Ok(Value::Object(checked))
            }
            (kind, value) => {
                let wanted = match kind {
                    Kind::String { one_of: Some(values), .. } => format!("one of: {}", values.join(", ")),
                    Kind::String { uuid: true, .. } => "a UUID".to_string(),
                    Kind::String { .. } => "text".to_string(),
                    Kind::Integer { .. } => "a whole number".to_string(),
                    Kind::Number { .. } => "a number".to_string(),
                    Kind::Boolean { .. } => "true or false".to_string(),
                    Kind::Array(_) => "a list".to_string(),
                    Kind::Object(_) | Kind::FreeObject => "an object".to_string(),
                    Kind::Any => "anything".to_string(),
                };
                Err(expected(field, format!("must be {wanted} (got {})", type_name(&value))))
            }
        }
    }
}

/// The field names serde reads for `T` (a `#[derive(Deserialize)]` struct), so a test can check the struct and its
/// schema list the same fields. None for anything that isn't a struct.
pub fn struct_fields<T: for<'de> Deserialize<'de>>() -> Option<&'static [&'static str]> {
    let seen = Cell::new(None);
    let _ = T::deserialize(FieldsProbe(&seen));
    seen.get()
}

struct FieldsProbe<'a>(&'a Cell<Option<&'static [&'static str]>>);

#[derive(Debug)]
struct Probed;

impl fmt::Display for Probed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("only the field names were read")
    }
}

impl std::error::Error for Probed {}

impl serde::de::Error for Probed {
    fn custom<T: fmt::Display>(_message: T) -> Self {
        Probed
    }
}

impl<'de> Deserializer<'de> for FieldsProbe<'_> {
    type Error = Probed;

    fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Probed> {
        Err(Probed)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Probed> {
        self.0.set(Some(fields));
        Err(Probed)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option unit unit_struct
        newtype_struct seq tuple tuple_struct map enum identifier ignored_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Prop {
        object([
            ("task_id", uuid().required().about("The task.")),
            ("text", string().len(1, 5).required()),
            ("status", one_of(&["open", "done"])),
            ("limit", integer().range(1, 50).default(20)),
            ("tags", array(string().len(1, 10)).len(1, 2)),
            ("confirmed", must_be_true()),
        ])
    }

    fn why(result: Result<Value, ToolError>) -> String {
        match result {
            Ok(value) => format!("passed: {value}"),
            Err(error) => error.to_string(),
        }
    }

    const ID: &str = "0b7a1c2e-1f3d-4c5b-9a8e-123456789abc";

    #[test]
    fn explicit_null_is_distinct_from_a_missing_required_value() -> Result<(), ToolError> {
        let input = object([("value", any().nullable().required())]);
        assert_eq!(input.check(json!({"value": null}), "")?, json!({"value": null}));
        assert!(input.check(json!({}), "").is_err());
        let typed = string().nullable().required();
        assert_eq!(typed.check(Value::Null, "value")?, Value::Null);
        assert_eq!(typed.to_json()["anyOf"][1], json!({"type":"null"}));
        Ok(())
    }

    #[test]
    fn good_arguments_pass_with_defaults_filled() -> Result<(), ToolError> {
        let checked = input().check(json!({ "task_id": ID, "text": "hi", "status": null }), "")?;
        assert_eq!(checked, json!({ "task_id": ID, "text": "hi", "limit": 20 }));
        Ok(())
    }

    #[test]
    fn each_failure_names_its_field_and_why() {
        let check = |value: Value| why(input().check(value, ""));
        assert_eq!(check(json!({ "text": "hi" })), "Bad argument `task_id`: is required");
        assert_eq!(check(json!({ "task_id": "nope", "text": "hi" })), "Bad argument `task_id`: must be a UUID (got \"nope\")");
        assert_eq!(check(json!({ "task_id": ID, "text": "" })), "Bad argument `text`: must be 1 to 5 characters");
        assert_eq!(check(json!({ "task_id": ID, "text": 5 })), "Bad argument `text`: must be text (got a number)");
        assert_eq!(
            check(json!({ "task_id": ID, "text": "a", "status": "x" })),
            "Bad argument `status`: must be one of: open, done (got \"x\")"
        );
        assert_eq!(check(json!({ "task_id": ID, "text": "a", "limit": 51 })), "Bad argument `limit`: must be from 1 to 50 (got 51)");
        assert_eq!(check(json!({ "task_id": ID, "text": "a", "limit": 2.5 })), "Bad argument `limit`: must be a whole number (got 2.5)");
        assert_eq!(check(json!({ "task_id": ID, "text": "a", "tags": ["A", ""] })), "Bad argument `tags[1]`: must be 1 to 10 characters");
        assert_eq!(
            check(json!({ "task_id": ID, "text": "a", "confirmed": false })),
            "Bad argument `confirmed`: must be true (set it when the person asked for this change)"
        );
        assert_eq!(
            check(json!({ "task_id": ID, "text": "a", "taskId": ID })),
            "Bad argument `taskId`: isn't a field here; the fields are: task_id, text, status, limit, tags, confirmed"
        );
        assert_eq!(check(json!("text")), "Bad argument `arguments`: must be an object (got text)");
    }

    #[test]
    fn lengths_count_like_javascript() -> Result<(), ToolError> {
        // An emoji is two UTF-16 units, as JavaScript counts it.
        assert!(string().len(1, 1).check(json!("😀"), "x").is_err());
        string().len(2, 2).check(json!("😀"), "x")?;
        Ok(())
    }

    #[test]
    fn the_schema_is_what_the_model_sees() {
        let schema =
            object([("view", one_of(&["open", "done"]).default("open").about("Which list.")), ("id", uuid().required())]).to_json();
        assert_eq!(
            schema,
            json!({
                "type": "object",
                "properties": {
                    "view": { "type": "string", "enum": ["open", "done"], "description": "Which list.", "default": "open" },
                    "id": { "type": "string", "format": "uuid" },
                },
                "required": ["id"],
                "additionalProperties": false,
            })
        );
    }

    #[test]
    fn builder_mistakes_are_reported() {
        assert_eq!(boolean().len(1, 2).mistakes(), vec!["len() is for a string() or an array()".to_string()]);
        assert_eq!(object([("a", string()), ("a", string())]).mistakes(), vec!["a is declared twice".to_string()]);
        assert_eq!(
            integer().range(1, 5).default(9).mistakes(),
            vec!["its default doesn't pass: Bad argument `default`: must be from 1 to 5 (got 9)".to_string()]
        );
        assert!(input().mistakes().is_empty());
    }

    #[test]
    fn uuids_are_checked_by_shape_only() {
        assert!(is_uuid(ID));
        // No version or variant bits.
        assert!(is_uuid("aaaabbbb-cccc-0ddd-1eee-ffff00001111"));
        assert!(is_uuid("0b7a1c2e-1f3d-6c5b-7a8e-123456789abc"));
        assert!(!is_uuid("0b7a1c2e1f3d4c5b9a8e123456789abc"));
        assert!(!is_uuid("0b7a1c2e-1f3d-4c5b-9a8e-123456789abz"));
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Args {
        task_id: String,
        #[serde(rename = "text")]
        words: String,
    }

    #[test]
    fn a_structs_fields_are_read_without_data() {
        assert_eq!(struct_fields::<Args>(), Some(&["task_id", "text"][..]));
        assert_eq!(struct_fields::<String>(), None);
    }
}
