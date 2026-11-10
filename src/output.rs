//! Keeping answers small. Many MCP clients spill a long tool result to a file the model has to read back, or cut it,
//! so every list pages and says where the next page starts.

use serde_json::{Map, Value, json};

use crate::schema::{Prop, array, integer, object};

/// The `limit` field of a list: how many to send, `default` when left out, at most `max`.
pub fn limit_field(default: i64, max: i64) -> Prop {
    integer().range(1, max).default(default).about("How many to send.")
}

/// The `offset` field of a list: how many to skip, 0 or more. Whatever a page returned as `next_offset` is always
/// accepted, so the last page of a long list can be read.
pub fn offset_field() -> Prop {
    integer().at_least(0).default(0).about("How many to skip; pass the last answer's next_offset to get the next page.")
}

/// The output schema of a page made by `page`, its items each shaped like `item`.
pub fn page_schema(item: Prop) -> Prop {
    object([
        ("items", array(item).required()),
        ("offset", integer().at_least(0).required()),
        ("returned", integer().at_least(0).required()),
        ("total", integer().at_least(0).required()),
        ("next_offset", integer().at_least(0).nullable().required().about("Where the next page starts; null on the last page.")),
    ])
}

/// A page of a list: its items, where it starts, how many the whole list has, and where the next page starts
/// (null on the last page).
pub fn page(items: Vec<Value>, offset: u64, total: u64) -> Value {
    let returned = items.len() as u64;
    let next = offset + returned;
    let mut page = Map::new();
    page.insert("items".into(), Value::Array(items));
    page.insert("offset".into(), json!(offset));
    page.insert("returned".into(), json!(returned));
    page.insert("total".into(), json!(total));
    page.insert("next_offset".into(), if returned > 0 && next < total { json!(next) } else { Value::Null });
    Value::Object(page)
}

/// `text` cut to `max` characters, with "…" when it was longer.
pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut short: String = text.chars().take(max).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::string;
    use crate::tool::ToolError;

    #[test]
    fn pages_say_where_the_next_one_starts() {
        assert_eq!(page(vec![json!(1), json!(2)], 0, 5)["next_offset"], json!(2));
        assert_eq!(page(vec![json!(5)], 4, 5)["next_offset"], Value::Null);
        assert_eq!(page(Vec::new(), 10, 5)["next_offset"], Value::Null);
    }

    #[test]
    fn a_page_matches_its_schema() -> Result<(), ToolError> {
        let schema = page_schema(string());
        schema.check(page(vec![json!("a")], 0, 3), "")?;
        schema.check(page(vec![json!("c")], 2, 3), "")?;
        assert!(schema.mistakes().is_empty());
        Ok(())
    }

    #[test]
    fn long_text_is_cut_with_a_mark() {
        assert_eq!(cut("short", 10), "short");
        assert_eq!(cut("abcdef", 3), "abc…");
    }
}
