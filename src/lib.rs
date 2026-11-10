//! A small MCP server core: JSON-RPC over stdio, tools with typed input and output schemas, API key scopes that hide
//! the tools a key can't use, an access request flow for what a key lacks, and a contract test per tool. See
//! README.md. `bike_shop` is a made-up sample domain that shows how tools are written.

pub mod access;
pub mod bike_shop;
pub mod checks;
pub mod config;
pub mod contract;
pub mod hash;
pub mod mcp;
pub mod output;
pub mod redact;
pub mod release;
pub mod schema;
pub mod tool;
