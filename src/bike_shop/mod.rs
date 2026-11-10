//! The sample domain: a made-up bike shop with a handful of bikes in memory. It exists to show how a tool is defined,
//! scoped and tested; none of it is a real business rule. Copy this file's shape for your own tools.
//!
//! - `bikes_list` (catalog:read): a page of bikes, filtered by category.
//! - `bike_get` (catalog:read): one bike by SKU.
//! - `bike_stock_adjust` (stock:write): change one bike's count, with a reason.
//! - `request_access` (every key): ask for a tool or scope the key lacks (see `access.rs`).

use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Mutex, MutexGuard};

use crate::access::{MemoryAccessRequests, request_access_tool};
use crate::mcp::Ready;
use crate::output::{limit_field, offset_field, page, page_schema};
use crate::schema::{Prop, boolean, integer, must_be_true, object, one_of, string};
use crate::tool::{Ctx, KeyInfo, Kind, Scope, Spec, Tool, ToolError};

pub const CATALOG_READ: Scope = Scope("catalog:read");
pub const STOCK_WRITE: Scope = Scope("stock:write");

const CATEGORIES: &[&str] = &["gravel", "mountain", "city", "kids"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bike {
    pub sku: String,
    pub name: String,
    pub category: &'static str,
    pub in_stock: i64,
}

impl Bike {
    fn to_json(&self) -> Value {
        json!({ "sku": self.sku, "name": self.name, "category": self.category, "in_stock": self.in_stock })
    }
}

/// The shop's state. Tools get `&BikeShop`, so changes go through the lock.
pub struct BikeShop {
    bikes: Mutex<Vec<Bike>>,
}

impl BikeShop {
    pub fn new(bikes: Vec<Bike>) -> BikeShop {
        BikeShop { bikes: Mutex::new(bikes) }
    }

    /// The demo's five bikes. The contract examples in `contract/` are written against these.
    pub fn seeded() -> BikeShop {
        let bike = |sku: &str, name: &str, category: &'static str, in_stock: i64| Bike {
            sku: sku.to_string(),
            name: name.to_string(),
            category,
            in_stock,
        };
        BikeShop::new(vec![
            bike("GRV-700", "Gravel Runner 700c", "gravel", 4),
            bike("TRL-29", "Trail Badger 29", "mountain", 2),
            bike("CTY-3", "City Glide 3-speed", "city", 0),
            bike("KID-16", "Puddle Jumper 16", "kids", 7),
            bike("TND-2", "Tandem Twin", "city", 1),
        ])
    }

    fn bikes(&self) -> Result<MutexGuard<'_, Vec<Bike>>, ToolError> {
        self.bikes.lock().map_err(|_| ToolError::failed("the bike list is unusable after a crash"))
    }
}

/// A seeded shop for `key`, with access requests kept in memory.
pub fn demo_ready(key: KeyInfo) -> Ready<BikeShop> {
    Ready { state: BikeShop::seeded(), key, requests: Box::new(MemoryAccessRequests::new()) }
}

fn bike_schema() -> Prop {
    object([
        ("sku", string().required()),
        ("name", string().required()),
        ("category", one_of(CATEGORIES).required()),
        ("in_stock", integer().at_least(0).required()),
    ])
}

fn sku_field() -> Prop {
    string().len(1, 40).required().about("The bike's SKU, e.g. \"GRV-700\". Case doesn't matter.")
}

/// Every tool the sample server has, before the key's scopes filter them.
pub fn tools() -> Vec<Tool<BikeShop>> {
    vec![
        Spec {
            name: "bikes_list",
            title: "List bikes",
            description: "A page of the shop's bikes with their stock, in catalog order. category narrows it to one kind; \
in_stock_only leaves out bikes with none. For one bike by SKU use bike_get.",
            kind: Kind::Read,
            scopes: &[CATALOG_READ],
            input: object([
                ("category", one_of(CATEGORIES)),
                ("in_stock_only", boolean().default(false)),
                ("limit", limit_field(20, 100)),
                ("offset", offset_field()),
            ]),
            output: page_schema(bike_schema()),
        }
        .handler(bikes_list),
        Spec {
            name: "bike_get",
            title: "Read one bike",
            description: "One bike by its SKU: name, category and how many are in stock.",
            kind: Kind::Read,
            scopes: &[CATALOG_READ],
            input: object([("sku", sku_field())]),
            output: object([("bike", bike_schema().required())]),
        }
        .handler(bike_get),
        Spec {
            name: "bike_stock_adjust",
            title: "Change a bike's stock count",
            description: "Adds to or takes from one bike's stock count (change is +5 for five more, -1 for one fewer) and says \
the count before and after. reason is a short note for the record. Only when a person asked for the change: set \
confirmed to true. A count can't go below zero.",
            kind: Kind::Write,
            scopes: &[STOCK_WRITE],
            input: object([
                ("sku", sku_field()),
                ("change", integer().range(-50, 50).required().about("How many to add (positive) or take (negative); not 0.")),
                ("reason", string().len(1, 200).required()),
                ("confirmed", must_be_true().required()),
            ]),
            output: object([
                ("sku", string().required()),
                ("before", integer().at_least(0).required()),
                ("after", integer().at_least(0).required()),
                ("reason", string().required()),
            ]),
        }
        .handler(bike_stock_adjust),
        request_access_tool(),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BikesListArgs {
    category: Option<String>,
    in_stock_only: bool,
    limit: u64,
    offset: u64,
}

fn bikes_list(ctx: &Ctx<'_, BikeShop>, args: BikesListArgs) -> Result<Value, ToolError> {
    let bikes = ctx.state.bikes()?;
    let matching: Vec<&Bike> = bikes
        .iter()
        .filter(|bike| args.category.as_deref().is_none_or(|category| bike.category == category))
        .filter(|bike| !args.in_stock_only || bike.in_stock > 0)
        .collect();
    let items = matching.iter().skip(args.offset as usize).take(args.limit as usize).map(|bike| bike.to_json()).collect();
    Ok(page(items, args.offset, matching.len() as u64))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BikeGetArgs {
    sku: String,
}

fn bike_get(ctx: &Ctx<'_, BikeShop>, args: BikeGetArgs) -> Result<Value, ToolError> {
    let bikes = ctx.state.bikes()?;
    let sku = args.sku.trim();
    let bike = bikes.iter().find(|bike| bike.sku.eq_ignore_ascii_case(sku)).ok_or_else(|| no_such_bike(sku))?;
    Ok(json!({ "bike": bike.to_json() }))
}

fn no_such_bike(sku: &str) -> ToolError {
    ToolError::not_found(format!("No bike has the SKU {sku}."))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BikeStockAdjustArgs {
    sku: String,
    change: i64,
    reason: String,
    #[allow(dead_code)]
    confirmed: bool,
}

fn bike_stock_adjust(ctx: &Ctx<'_, BikeShop>, args: BikeStockAdjustArgs) -> Result<Value, ToolError> {
    if args.change == 0 {
        return Err(ToolError::argument("change", "must not be 0"));
    }
    let reason = args.reason.trim();
    if reason.is_empty() {
        return Err(ToolError::argument("reason", "must not be blank"));
    }
    let mut bikes = ctx.state.bikes()?;
    let sku = args.sku.trim();
    let bike = bikes.iter_mut().find(|bike| bike.sku.eq_ignore_ascii_case(sku)).ok_or_else(|| no_such_bike(sku))?;
    let before = bike.in_stock;
    let after = before + args.change;
    if after < 0 {
        return Err(ToolError::refused(format!(
            "{} has {before} in stock, so taking {} would leave {after}. A count can't go below zero.",
            bike.sku, -args.change
        )));
    }
    bike.in_stock = after;
    Ok(json!({ "sku": bike.sku, "before": before, "after": after, "reason": reason }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::problems;
    use crate::redact::Redactor;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn call(ready: &Ready<BikeShop>, name: &str, arguments: Value) -> Result<Value, ToolError> {
        let redactor = Redactor::new();
        let tools = tools();
        let tool = tools.iter().find(|tool| tool.name() == name).ok_or_else(|| ToolError::failed(format!("no tool {name}")))?;
        tool.call(&ready.ctx(&redactor), arguments)
    }

    #[test]
    fn every_tool_is_well_formed() {
        assert_eq!(problems(&tools()), Vec::<String>::new());
    }

    #[test]
    fn lists_page_through_every_bike() -> TestResult {
        let ready = demo_ready(KeyInfo::new("k", &[]));
        let first = call(&ready, "bikes_list", json!({ "limit": 2 }))?;
        assert_eq!((first["returned"].clone(), first["next_offset"].clone()), (json!(2), json!(2)));
        let last = call(&ready, "bikes_list", json!({ "limit": 2, "offset": 4 }))?;
        assert_eq!((last["items"][0]["sku"].clone(), last["next_offset"].clone()), (json!("TND-2"), Value::Null));
        let stocked_city = call(&ready, "bikes_list", json!({ "category": "city", "in_stock_only": true }))?;
        assert_eq!(stocked_city["total"], 1);
        Ok(())
    }

    #[test]
    fn stock_changes_stay_and_never_go_below_zero() -> TestResult {
        let ready = demo_ready(KeyInfo::new("k", &[]));
        let adjust = |sku: &str, change: i64| {
            call(&ready, "bike_stock_adjust", json!({ "sku": sku, "change": change, "reason": "count", "confirmed": true }))
        };
        assert_eq!(adjust("kid-16", -2)?["after"], 5);
        assert_eq!(call(&ready, "bike_get", json!({ "sku": "KID-16" }))?["bike"]["in_stock"], 5);
        let refused = adjust("KID-16", -6).err().map(|error| error.to_string()).unwrap_or_default();
        assert!(refused.starts_with("KID-16 has 5 in stock, so taking 6 would leave -1."), "{refused}");
        assert_eq!(adjust("KID-16", 0), Err(ToolError::argument("change", "must not be 0")));
        Ok(())
    }
}
