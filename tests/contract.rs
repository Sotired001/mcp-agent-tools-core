//! Every registered tool has `contract/<tool>.json`, and every example in it holds. A new tool without a contract
//! fails here.

use std::path::PathBuf;

use mcp_agent_tools_core::bike_shop::{demo_ready, tools};
use mcp_agent_tools_core::contract;
use mcp_agent_tools_core::tool::KeyInfo;

#[test]
fn every_tool_has_a_contract_and_keeps_it() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contract");
    let problems = contract::check(&tools(), &dir, &|| demo_ready(KeyInfo::new("contract-test", &[])));
    assert_eq!(problems, Vec::<String>::new());
}
