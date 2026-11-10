# mcp-agent-tools-core

A small Model Context Protocol (MCP) server core in Rust, written by hand on `serde_json`. It speaks JSON-RPC 2.0
over stdio and gives you:

- tools declared once, with typed input and output schemas that are both shown to the model and enforced,
- API key scopes that decide which tools a key can see and call,
- an access request flow, so an agent that lacks a tool or scope asks for it once instead of guessing,
- a contract file per tool, checked by `cargo test`, so schemas, handlers and examples can't drift apart,
- errors that say what went wrong and what to do next, with keys and tokens redacted.

It ships with a made-up sample domain, a tiny in-memory bike shop, and a binary (`bike-shop-mcp`) that serves it.
The bike shop exists only to show how tools are defined, scoped and tested.

Dependencies: `serde`, `serde_json`, `sha2`. No MCP SDK, no async runtime.

## Layout

```
src/
  mcp.rs        JSON-RPC over stdio: initialize, ping, tools/list, tools/call (+ empty resources and prompts)
  tool.rs       Spec, Tool, Kind (annotations), ToolError, Ctx, Scope, KeyInfo
  schema.rs     schema builder: the JSON Schema the model sees, the argument checks, the defaults
  access.rs     the access request flow: the AccessRequests trait, an in-memory store, the request_access tool
  contract.rs   runs contract/<tool>.json against each tool
  checks.rs     what every tool must get right (names, scopes, schemas, argument structs, tool names in texts)
  output.rs     paging helpers and their output schema
  redact.rs     keys and tokens out of every failure
  release.rs    exit when a release symlink moves, so the client restarts the server and reads new tools
  hash.rs       SHA-256 hex (access request keys)
  config.rs     the demo binary's settings from the environment
  bike_shop/    the sample tools
  main.rs       the demo binary: a read-line, write-line loop around Server::handle
contract/       one contract file per tool
tests/          the contract test
```

## Design

### Scoped keys hide tools

Every tool lists the scopes it needs (`scopes: &[CATALOG_READ]`, plus `.or_scopes(&[...])` when any one of
several will do). The server is told the key's name and scopes once, at startup, as a `KeyInfo`. Then:

- `tools/list` shows only the tools the key holds every scope for. A key that can't use a tool never sees it, so the
  model isn't tempted to call it.
- `tools/call` checks again. A call to a tool the key can't use answers with an error result naming the missing
  scope and the key, and, if the key can see `request_access`, a line telling the agent to ask for it.

Scopes are plain strings chosen by the host (`catalog:read`, `stock:write`). A key scope that no tool uses grants
nothing; the demo binary says so on stderr.

### The access request flow

`request_access` is a built-in tool that every key can see (`open_to_every_key()`), so a key missing a scope can
still reach it. The agent says what it needs, whether something is broken or missing, what it was trying to do, and
the error it got. The tool:

- redacts the pasted error (it may hold a key or token),
- files the request under a key made from the normalised need, so the same need asked twice, however it is spaced
  or capitalised, is one request,
- answers `new: false` with a note when an open request for that need already exists, and files a fresh one when
  the earlier request was closed.

Where requests go is up to the host: implement the one-method `AccessRequests` trait (a ticket queue, a chat
message, a database row). `MemoryAccessRequests` keeps them in memory for tests and the demo; the demo binary also
prints each new request on stderr. Errors tell the agent when not to ask: a `ToolError::Refused` (a rule said no)
and a `ToolError::NotFound` (a wrong id) each carry a hint saying it isn't a missing tool.

### A contract test per tool

Each tool has `contract/<tool>.json` with valid and invalid example calls:

```json
{
  "tool": "bike_get",
  "valid": [
    { "input": { "sku": "TRL-29" },
      "output": { "bike": { "sku": "TRL-29", "name": "Trail Badger 29", "category": "mountain", "in_stock": 2 } } }
  ],
  "invalid": [
    { "input": { "sku": "" }, "error": "Bad argument `sku`: must be 1 to 40 characters" }
  ]
}
```

`tests/contract.rs` runs `contract::check` over every registered tool, each example on a fresh state. It fails when:

- a tool has no contract file, or a contract file names no tool,
- a valid example's input breaks the input schema, its `output` breaks the output schema, or the tool's real answer
  differs from the documented `output`,
- an invalid example doesn't fail, or fails with different text than the documented `error`,
- a contract has no valid example.

So adding a tool without a contract fails `cargo test`. A second test (`checks::problems`) fails on malformed tools:
duplicate or badly formed names, missing scopes, an input or output that isn't an object schema, builder mistakes
(a default that doesn't pass its own field's checks, a field declared twice), an argument struct whose fields don't
match the input schema, and a description that names a tool that doesn't exist.

Outputs are checked at run time too: a handler that answers something its output schema doesn't allow fails loudly
instead of handing the model a surprise.

### Errors

A failed call is an MCP tool result with `isError: true` and one text block. The text is the `ToolError`:

- `Argument`: ``Bad argument `field`: why``, from the schema checks or the handler,
- `NotFound`: the message plus a hint to check the id and list or search,
- `Refused`: the message plus a hint that this is a rule working, not a missing tool,
- `Failed`: anything else, as written.

Every failure passes through `Redactor` before it leaves the server: `Bearer ...`, `api_key=...`, tokens with the
host's own prefixes, and configured secrets become `[REDACTED]`, and the text is cut to 2000 characters. Protocol
errors (bad JSON, unknown method, unknown tool) are JSON-RPC errors with the standard codes.

If the host can't start (a missing setting, a key it can't resolve), it passes `Err(why)` to `Server::new`. The
server then answers the first request with that reason and stops, so the MCP client shows the reason instead of a
dead process.

## Running it over stdio

Build it (Rust 1.88 or newer: edition 2024 and `let` chains):

```
cargo build --release
```

The demo binary reads its settings from its environment:

| Variable | Meaning |
|---|---|
| `AGENT_TOOLS_KEY_NAME` | the key's name, used in messages and access requests |
| `AGENT_TOOLS_KEY_SCOPES` | comma-separated scopes; must be set, empty means none. The sample uses `catalog:read` and `stock:write`. |
| `AGENT_TOOLS_RELEASE_LINK` | optional; see "Restarting on a new release" |

Most MCP clients take a command and an environment. For a client that reads an `mcpServers` JSON file:

```json
{
  "mcpServers": {
    "bike-shop": {
      "command": "/path/to/target/release/bike-shop-mcp",
      "env": { "AGENT_TOOLS_KEY_NAME": "helper-bot", "AGENT_TOOLS_KEY_SCOPES": "catalog:read" }
    }
  }
}
```

With only `catalog:read`, the model sees `bikes_list`, `bike_get` and `request_access`. Calling `bike_stock_adjust`
anyway answers with the missing scope and the hint to ask; asking prints the request on the server's stderr. Add
`stock:write` and the adjust tool appears.

To try it without a client, pipe JSON-RPC lines in:

```
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"bike_get","arguments":{"sku":"TRL-29"}}}' \
| AGENT_TOOLS_KEY_NAME=helper-bot AGENT_TOOLS_KEY_SCOPES=catalog:read ./target/release/bike-shop-mcp
```

The bike shop lives in memory: stock changes and access requests last until the process exits.

### Restarting on a new release

If the binary runs from a release directory reached through a symlink (`/opt/my-app/current`), set
`AGENT_TOOLS_RELEASE_LINK` to that link. The server checks it after each answer and every 30 seconds while idle. When
the link points somewhere else it exits cleanly; a client that restarts its servers picks up the new tool list.
After `notifications/initialized` the server also sends `notifications/tools/list_changed`, so a client that cached
the old list reads it again.

## Adding a tool

A tool is a `Spec` (what the model sees, the scopes it needs, its input and output schemas) plus a handler. The
handler gets its arguments already checked against `input`, defaults filled in, and read into its own struct.

```rust
use serde::Deserialize;
use serde_json::{Value, json};

use crate::schema::{integer, object, string};
use crate::tool::{Ctx, Kind, Scope, Spec, Tool, ToolError};

const CATALOG_READ: Scope = Scope("catalog:read");

fn bike_count_tool() -> Tool<BikeShop> {
    Spec {
        name: "bike_count",
        title: "Count bikes in a category",
        description: "How many bikes of one category are in stock.",
        kind: Kind::Read,
        scopes: &[CATALOG_READ],
        input: object([("category", string().len(1, 20).required())]),
        output: object([("category", string().required()), ("in_stock", integer().at_least(0).required())]),
    }
    .handler(bike_count)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BikeCountArgs {
    category: String,
}

fn bike_count(ctx: &Ctx<'_, BikeShop>, args: BikeCountArgs) -> Result<Value, ToolError> {
    let in_stock = ctx.state.count(&args.category)?; // your own state
    Ok(json!({ "category": args.category, "in_stock": in_stock }))
}
```

Then:

1. Register it in the list your server passes to `Server::new` (for the sample, `bike_shop::tools()`).
2. Write `contract/bike_count.json` with at least one valid example and the failures worth pinning down.
3. Run `cargo test`. It fails until the contract holds, and tells you what the tool really answered.

The schema builder (`schema.rs`) has `string`, `uuid`, `one_of`, `integer`, `number`, `boolean`, `must_be_true`
(a switch the agent sets only when a person asked for a change), `array`, `object`, `free_object` and `any`, with
`.required()`, `.nullable()`, `.default(..)`, `.len(..)`, `.range(..)`, `.at_least(..)` and `.about(..)`. Optional
fields may be omitted or sent as `null`. Unknown fields are refused by name. String lengths count UTF-16 units, as
JavaScript does.

`Ctx<S>` carries your state (`ctx.state`), the key (`ctx.key`), the access request store and the redactor. `S` is
whatever your tools need: a database pool, an HTTP client for your own API, or, in the sample, a `Mutex` around a
list.

## What is not included

- Key issuing, scoping, expiry and revocation. These live in the host application. This crate is told a key's name
  and scopes and trusts them; the demo reads them from environment variables, which is fine for a demo and not an
  authentication scheme. A real host resolves the key itself, for example by asking its own API who the key belongs
  to, and passes the result in.
- Granting access. `request_access` files a request; deciding on it and changing a key's scopes is up to the host.
- Any transport but stdio. There is no Streamable HTTP or SSE transport.
- Resources, prompts, sampling, completions, logging, progress and cancellation. `resources/list`,
  `resources/templates/list` and `prompts/list` answer empty lists; other methods answer "Method not found".
- Concurrency. Requests are handled one at a time, in order.
- Persistence. The sample shop and `MemoryAccessRequests` are in memory only.

Protocol versions answered: 2026-07-28, 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05. A client asking for
another gets the newest.

## Checks

```
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`unsafe` is forbidden, and clippy denies `unwrap`, `expect` and `panic`, in tests too: tests return `Result` and use
`?`. The release watcher's tests use Unix symlinks and run only on Unix.

## License

MIT. See [LICENSE](LICENSE).
