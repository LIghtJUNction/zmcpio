# zmcp

Bounded MCP protocol primitives for applications that own their transport,
authorization and scheduling. The library is `no_std`, forbids unsafe Rust,
and has no normal, development, build, optional or target-specific dependencies.

**Unreleased.** The crates.io name `zmcp` already has an unrelated-or-unverified
0.0.1 registration. Do not install that version expecting this implementation.
Publication is blocked until package ownership or a new package name is verified.

## Boundary

`zmcp` parses complete JSON-RPC messages and returns borrowed actions.
`Action::CallTool` does not execute anything. An application checks policy,
runs its tool, then calls `Server::tool_result`. No agent loop, provider adapter,
HTTP stack, database, runtime, or model dependency is included.

`zharness` and `zmcp` are sibling components, not dependencies of each other:

```text
application
  +-- zmcp             MCP wire messages and transport boundaries
  +-- zharness         tool registry, execution boundary, model bridge
        +-- zcall      model lifecycle
              +-- zconv
```

This preserves zharness's sole direct dependency on zcall. A standalone MCP
server does not bring model-conversion code into its dependency graph.

## Use from the source checkout

```sh
cargo test --locked --all-targets
cargo run --locked --example basic
cargo run --locked --example mcp_stdio
```

The stdio example implements only an explicitly allowlisted `echo` tool. It uses
bounded LF framing and keeps diagnostics off stdout. See [MCP](docs/MCP.md),
[architecture](docs/ARCHITECTURE.md), and [security](docs/SECURITY.md).

## Implemented profile

JSON-RPC request/notification/success/error classification; MCP 2026-07-28
per-request version and client capabilities, discovery, tools/list and tools/call;
explicit 2025-11-25 initialization/initialized/ping compatibility; pagination
cursors; text/image/audio/resource content; structured results; execution-error
flags; cancellation/progress decoding; bounded in-flight ID and progress tracking.

This is a **tools-oriented protocol core**, not a complete MCP SDK. MRTR,
subscriptions, resources and prompts routing, sampling, elicitation, authorization,
and HTTP/SSE transport implementations are not implemented here. The generic
RPC types preserve messages for application-owned extensions. Do not describe
this release candidate as full MCP conformance.

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
