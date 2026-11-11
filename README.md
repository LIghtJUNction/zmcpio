# zmcpio

Bounded MCP client/server primitives for applications that own authorization and
scheduling. The protocol core is `no_std`, allocation-free and forbids unsafe
Rust. `zcall` is its only direct third-party dependency; `zconv` is transitive.
The optional `std` feature adds a real blocking stdio server-process connection.

The crate is named `zmcpio` on crates.io and the Rust API is imported as
`zmcpio`. The former project name, `zmcp`, belongs to a different crates.io
publisher and **must not** be used as a dependency for this implementation.

## Install

```toml
[dependencies]
zmcpio = { version = "0.2", default-features = false }
```

Enable `features = ["std"]` for `stdio::StdioTransport`. The protocol core and
`zcall` bridge do not require an allocator. Process startup may allocate in std.

## Boundary

`zmcpio` parses complete JSON-RPC messages and returns borrowed actions.
`Action::CallTool` does not execute anything. An application checks policy,
runs its tool, then calls `Server::tool_result`. No agent loop, HTTP stack,
database, model runtime or automatic network requests are included. Shared model
types and provider-format conversion come from `zcall`.

`zharness` and `zmcpio` remain independent components, sharing the `zcall` contract:

```text
application
  +-- zmcpio           MCP client/server, stdio, zcall tool bridge
  |     +-- zcall      shared model/tool types (re-exported as zmcpio::zcall)
  +-- zharness         tool registry, execution boundary, model bridge
        +-- zcall      model lifecycle
              +-- zconv
```

`Client` encodes requests and correlates replies using bounded caller-owned
pending slots. `bridge` converts discovered MCP tools into `zcall::v1::ToolDefinition`,
accepts complete `Content::ToolCall` or streaming `ReadyTool`, and appends results
through `Request::append_tool_result`. MCP IDs and model call IDs remain distinct.

## Use from the source checkout

```sh
cargo test --locked --all-targets
cargo run --locked --example basic
cargo run --locked --example mcp_stdio
cargo run --locked --example zcall_tools
cargo build --locked --features std --example mcp_stdio --example mcp_client
target/debug/examples/mcp_client target/debug/examples/mcp_stdio echo '{"text":"hello"}'
```

The stdio example implements only an explicitly allowlisted `echo` tool. It uses
bounded LF framing and keeps diagnostics off stdout. See [MCP](docs/MCP.md),
[architecture](docs/ARCHITECTURE.md), and [security](docs/SECURITY.md).
See the [client and zcall guide](docs/CLIENT.md) for lifetimes, cancellation,
pagination, error policies and custom transports. On Windows use `.exe` paths.

## Implemented profile

JSON-RPC request/notification/success/error classification; MCP 2026-07-28
per-request version and client capabilities, discovery, tools/list and tools/call;
explicit 2025-11-25 initialization/initialized/ping compatibility; pagination
cursors; text/image/audio/resource content; structured results; execution-error
flags; cancellation/progress decoding; bounded in-flight ID and progress tracking.
Client support includes modern discovery and explicit legacy initialization for
2025-11-25 / 2025-06-18, paginated tool discovery, calls, correlated responses,
progress, cancellation, late-response rejection, and optional stdio process I/O.

This is a **tools-oriented protocol core**, not a complete MCP SDK. MRTR,
subscriptions, resources and prompts routing, sampling, elicitation, authorization,
and HTTP/TLS/SSE implementations are not implemented here. Modern `input_required`
is surfaced distinctly for explicit caller handling, never automatically continued.
HTTP/custom transports supply complete messages through the transport boundary. The generic
RPC types preserve messages for application-owned extensions. Do not describe
this tools profile as full MCP conformance.

## Memory contract

Input, token tape, output, framing and request slots are caller-owned. Parsing
borrows input and token storage. Retain both while an action or tracker ID uses
them. No hidden growth occurs. `Error` reports a category, byte offset and required
capacity when known. On serialization failure discard partial output and retry
serialization only; never repeat a side-effecting tool just to grow output.

JSON accepts valid UTF-8 and paired Unicode escapes, rejects duplicate decoded
object keys, enforces byte/member/string/token/depth bounds, and preserves number
lexemes. Request IDs use signed 64-bit integers or strings; out-of-range integer
IDs are rejected rather than rounded. Nesting has a hard ceiling of 64 containers.
Full JSON Schema validation is an application responsibility.

License: MIT OR Apache-2.0.
