# zmcpio

Bounded MCP client/server primitives for applications that own authorization and
scheduling. The protocol core is `no_std`, allocation-free and forbids unsafe
Rust. `zcall` is its only direct third-party dependency; `zconv` is transitive.
The optional `std` feature adds blocking stdio process I/O and bounded HTTP/1.1
I/O over caller-supplied TCP or verified TLS streams.

The crate is named `zmcpio` on crates.io and the Rust API is imported as
`zmcpio`. The former project name, `zmcp`, belongs to a different crates.io
publisher and **must not** be used as a dependency for this implementation.

## Install

```toml
[dependencies]
zmcpio = { version = "0.3", default-features = false }
```

To use the stdio and HTTP/1.1 I/O adapters, enable the optional `std` feature:

```toml
[dependencies]
zmcpio = { version = "0.3", default-features = false, features = ["std"] }
```

The protocol core and `zcall` bridge do not require an allocator. Process/socket
setup may allocate. HTTPS requires a caller-supplied verified TLS stream.

## Boundary

`zmcpio` parses complete JSON-RPC messages and returns borrowed actions.
`Action::CallTool` does not execute anything. An application checks policy,
runs its tool, then calls `Server::tool_result`. No agent loop, TLS engine,
OAuth login flow, database, model runtime or automatic network requests are included. Shared model
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

## Implemented boundaries

| Boundary | Provided primitives |
|---|---|
| Tool client/server | Discovery, list/call, pagination, multimodal results, progress, cancellation, ID correlation |
| Streamable HTTP | Modern POST-only and legacy POST/GET/DELETE envelopes; JSON/SSE; sessions and resume metadata for legacy only |
| SSE | Incremental caller-buffered decoder, encoder, comments, BOM/UTF-8 handling, backpressure and exact capacity errors |
| Legacy batches | Caller-bounded 2025-03-26 request/response arrays, per-item errors and no response for notification-only batches |
| Legacy HTTP+SSE | Same-origin endpoint negotiation and separate message stream; no implicit credential forwarding or tool replay |
| Other MCP methods | Explicitly enabled resource/prompt/completion/logging routes, callback and MRTR envelopes, subscription validation, version-specific task messages |
| Optional I/O | Real stdio process adapter and bounded HTTP/1.1 framing over supplied streams |

Streamable HTTP, legacy SSE and the opt-in extension APIs ship in 0.3.0.
See [HTTP](docs/HTTP.md) and [extensions](docs/EXTENSIONS.md) for their
contracts and executable examples.
See [validation](docs/VALIDATION.md) for reproducible tests and benchmark limits.
A local modern HTTP server and client can be run in separate terminals:

```sh
cargo run --locked --features std --example mcp_http -- server 8080 sse
cargo run --locked --features std --example mcp_http -- client 127.0.0.1:8080
```

Use `json` instead of `sse` to exercise a single-JSON HTTP response. The example
binds only to loopback and exposes only echo. For remote HTTPS, inject a verified
TLS stream or connect the no-std protocol adapter to an existing HTTP stack.

Legacy single-message negotiation covers 2024-11-05, 2025-03-26, 2025-06-18 and
2025-11-25. Modern requests use 2026-07-28. This is a set of protocol and transport
primitives, not an automatic agent, OAuth client, task worker or storage service.
It is not a certification that every optional extension or third-party server
conforms. Bounded JSON-RPC batches are supported only for 2025-03-26, through an
explicit batch API; other revisions reject arrays. Application-owned callbacks, task
execution, policy, replay storage and timers remain explicit.

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
