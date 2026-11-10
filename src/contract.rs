//! A contract per tool: `contract/<tool>.json` holds example calls, and `check` runs them, so the declared schemas,
//! the handler and the documented examples can't drift apart. `cargo test` fails when a tool has no contract file,
//! when a file names no tool, or when any example doesn't hold.
//!
//! ```json
//! {
//!   "tool": "bike_get",
//!   "valid": [
//!     { "input": { "sku": "GRV-700" }, "output": { "bike": { "sku": "GRV-700", "...": "..." } } }
//!   ],
//!   "invalid": [
//!     { "input": { "sku": "" }, "error": "Bad argument `sku`: must be 1 to 40 characters" }
//!   ]
//! }
//! ```
//!
//! Each example runs on a fresh state from the `fresh` function, so examples don't depend on each other. A valid
//! example's input must pass the input schema, its `output` must pass the output schema, and the tool must answer
//! exactly that `output`. An invalid example must fail with exactly that `error`, as the agent would read it.
//! Every tool needs at least one valid example.

use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

use crate::mcp::Ready;
use crate::redact::Redactor;
use crate::tool::Tool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractFile {
    tool: String,
    valid: Vec<Valid>,
    #[serde(default)]
    invalid: Vec<Invalid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Valid {
    input: Value,
    output: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invalid {
    input: Value,
    error: String,
}

/// Every way `tools` break their contracts in `dir`, each as `<tool>: <what's wrong>`. Empty when all hold.
pub fn check<S>(tools: &[Tool<S>], dir: &Path, fresh: &dyn Fn() -> Ready<S>) -> Vec<String> {
    let mut found = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if let Some(stem) = file.strip_suffix(".json")
                    && !tools.iter().any(|tool| tool.name() == stem)
                {
                    found.push(format!("{file}: no tool is named {stem}; delete the file or register the tool"));
                }
            }
        }
        Err(error) => return vec![format!("can't read the contract folder {}: {error}", dir.display())],
    }
    for tool in tools {
        found.extend(check_tool(tool, dir, fresh).into_iter().map(|problem| format!("{}: {problem}", tool.name())));
    }
    found
}

fn check_tool<S>(tool: &Tool<S>, dir: &Path, fresh: &dyn Fn() -> Ready<S>) -> Vec<String> {
    let path = dir.join(format!("{}.json", tool.name()));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return vec![format!("it has no contract; write {} with at least one valid example", path.display())],
    };
    let contract: ContractFile = match serde_json::from_str(&text) {
        Ok(contract) => contract,
        Err(error) => return vec![format!("{} isn't a contract file: {error}", path.display())],
    };
    let mut found = Vec::new();
    if contract.tool != tool.name() {
        found.push(format!("its contract file says it is for {}", contract.tool));
    }
    if contract.valid.is_empty() {
        found.push("its contract has no valid example".to_string());
    }
    let redactor = Redactor::new();
    for (at, example) in contract.valid.iter().enumerate() {
        if let Err(error) = tool.spec.input.check(example.input.clone(), "") {
            found.push(format!("valid[{at}]: the input breaks the input schema: {error}"));
            continue;
        }
        if let Err(error) = tool.spec.output.check(example.output.clone(), "") {
            found.push(format!("valid[{at}]: the documented output breaks the output schema: {error}"));
        }
        let ready = fresh();
        match tool.call(&ready.ctx(&redactor), example.input.clone()) {
            Ok(output) if output == example.output => {}
            Ok(output) => found.push(format!("valid[{at}]: the tool answered {output}, not the documented output")),
            Err(error) => found.push(format!("valid[{at}]: the tool failed: {error}")),
        }
    }
    for (at, example) in contract.invalid.iter().enumerate() {
        let ready = fresh();
        match tool.call(&ready.ctx(&redactor), example.input.clone()) {
            Ok(output) => found.push(format!("invalid[{at}]: the tool answered {output} instead of failing")),
            Err(error) if error.to_string() == example.error => {}
            Err(error) => found.push(format!("invalid[{at}]: the tool failed with {:?}, not {:?}", error.to_string(), example.error)),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bike_shop::{demo_ready, tools};
    use crate::tool::KeyInfo;
    use serde_json::json;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Result<TempDir, String> {
            let dir = std::env::temp_dir().join(format!("mcp-core-contract-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            Ok(TempDir(dir))
        }

        fn write(&self, name: &str, content: &Value) -> Result<(), String> {
            std::fs::write(self.0.join(name), content.to_string()).map_err(|error| error.to_string())
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fresh() -> Ready<crate::bike_shop::BikeShop> {
        demo_ready(KeyInfo::new("contract", &[]))
    }

    #[test]
    fn a_missing_contract_and_a_stray_file_are_both_caught() -> Result<(), String> {
        let dir = TempDir::new("missing")?;
        dir.write("bike_rename.json", &json!({ "tool": "bike_rename", "valid": [] }))?;
        let tools: Vec<_> = tools().into_iter().filter(|tool| tool.name() == "bike_get").collect();
        let found = check(&tools, &dir.0, &fresh);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("bike_rename.json: no tool is named bike_rename"));
        assert!(found[1].starts_with("bike_get: it has no contract"));
        Ok(())
    }

    #[test]
    fn examples_that_dont_hold_are_caught() -> Result<(), String> {
        let dir = TempDir::new("drift")?;
        dir.write(
            "bike_get.json",
            &json!({
                "tool": "bike_get",
                "valid": [
                    { "input": { "sku": "GRV-700" }, "output": { "bike": { "sku": "GRV-700", "name": "Old name", "category": "gravel", "in_stock": 4 } } },
                    { "input": { "sku": "GRV-700", "extra": 1 }, "output": {} }
                ],
                "invalid": [{ "input": { "sku": "GRV-700" }, "error": "never" }]
            }),
        )?;
        let tools: Vec<_> = tools().into_iter().filter(|tool| tool.name() == "bike_get").collect();
        let found = check(&tools, &dir.0, &fresh);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].starts_with("bike_get: valid[0]: the tool answered"));
        assert!(found[1].starts_with("bike_get: valid[1]: the input breaks the input schema"));
        assert!(found[2].starts_with("bike_get: invalid[0]: the tool answered"));
        Ok(())
    }
}
