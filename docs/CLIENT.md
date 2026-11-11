# MCP client and zcall integration

`zmcpio` can act as the client of an MCP tool server. The default core encodes
requests and validates replies without I/O or allocation. The optional `std`
adapter starts a real stdio server process. `zcall` is the only direct dependency;
the exact same types are available through `zmcpio::zcall::v1`.

## Run a real server connection

```toml
[dependencies]
zmcpio = { version = "0.2", features = ["std"] }
```

From this source checkout:

```sh
cargo build --locked --features std --example mcp_stdio --example mcp_client
target/debug/examples/mcp_client target/debug/examples/mcp_stdio echo '{"text":"hello"}'
```

Use `.exe` executable paths on Windows. The client example starts the selected
server, performs the legacy handshake, follows tools/list pages, checks that the
explicitly selected tool exists, calls it, prints its result and closes the child.
It never chooses a tool on its own. Server command arguments follow the JSON
argument. In an application, pass a configured `std::process::Command` directly
to `stdio::StdioTransport::spawn`; no shell is inserted by the library.

The executable is an integration example, not a hardened long-running agent.
Its 32-message reply budget and 128-page discovery budget bound loops, not elapsed
time. Blocking stdio reads do not have a response deadline. For cancellation or
timeouts during a blocked read, supply application-owned asynchronous/deadline
I/O through `MessageReader`, `MessageWriter` or your own loop.

## Protocol client

Create a `Client` with a borrowed identity, an explicit `Era`, and caller-sized
`Pending` storage. A new client identity starts its numeric request IDs at one;
do not reset it on an existing legacy connection. The client retains no request
or response payload and allocates no dynamically growing request table.

Legacy mode starts with `initialize`. A supported initialize response moves to
`AwaitInitialized`; encode and successfully send `initialized` before requesting
tools. Client negotiation accepts the tested tools profiles `2025-11-25` and
`2025-06-18`. Other versions are rejected; this is not a claim of supporting old
batch-based MCP revisions. `protocol_version()` exposes the selected version.

Modern mode uses `2026-07-28` metadata on every request and is immediately ready.
`discover` is optional and exposes supported versions, capabilities and optional
server identity. There is no automatic legacy fallback or retry of tools/call.
The client advertises no sampling/elicitation callbacks it does not implement.

`list_tools` returns one borrowed `ToolPage` at a time. Its cursor is opaque:
decode it into retained caller storage before reusing the response input/tokens.
Applications own cross-page duplicate handling, tool namespaces, page limits,
authorization, JSON Schema validation and caching. No schema/resource URL is fetched.

`call_tool` accepts a validated JSON object and returns `Outgoing { id, len }`.
Send only `output[..len]`. A too-small output or full pending table reserves no
ID and changes no state. A successful encode does reserve a slot before network
I/O, because a partial send is ambiguous; explicitly close or abandon rather than
silently issuing the side effect twice.

`receive` takes exactly one complete JSON-RPC message and returns a typed `Event`.
Only a matching, well-formed response completes a pending request. Numeric and
string IDs are distinct; duplicates and late/unmatched responses are rejected.
Input/token-capacity errors leave pending state intact for re-parsing with larger
storage. Notifications and uncorrelated errors do not complete requests.

## Bridge to model calls

The bridge uses zcall's actual public types rather than parallel look-alikes:

| Input | Adapter | Output |
|---|---|---|
| Discovered MCP `ToolView` | `bridge::tool_definition` | `zcall::v1::ToolDefinition` for `RequestBuilder::tool` / `Request::append_tool` |
| Complete `Content::ToolCall` | `bridge::prepare_call` | Validated `PreparedCall` with caller-buffered escaped arguments |
| Streaming `ReadyTool` | `bridge::prepare_ready` | The same `PreparedCall`; partial deltas are not accepted |
| Streaming `ReadyTool` with later Session mutation | `bridge::prepare_ready_detached` | Copies only ID/name into caller storage so the retained Invocation does not borrow Session |
| Authorized `PreparedCall` | `PreparedCall::start` | MCP request bytes plus a retained `Invocation` identity binding |
| Completed MCP `CallResult` | `Invocation::content` | `zcall::v1::Content::ToolResult` with the original provider call ID |
| Completed MCP `CallResult` | `Invocation::append_result` | Checked update to a zcall `Request` history |

Run `cargo run --locked --example zcall_tools` for the complete executable
discovery -> model tool definition -> MCP tool call -> next model request example.
This example uses in-memory transport and a supplied completed model call; it does
not make a real model API call. The same byte/state APIs are used with stdio.

An `Invocation` binds the model's call ID to the independent MCP request ID.
Retain it until the response arrives, and pass the actual response ID to the
result adapter. The adapter rejects mismatches. `Request::append_tool_result`
then rejects unknown, ambiguous or duplicate model call results. For streaming
sessions, explicitly retain the result before calling the model session's
`submit_tool_result`; the bridge does not prematurely advance a model lifecycle.
Use `prepare_ready_detached` when that Invocation must outlive a borrow of the
session: it copies identity bytes into caller storage but still borrows arguments
until they have been encoded. This permits a later mutable Session transition
without allocating Strings or bypassing Rust's borrowing rules.

### Results and provider differences

The default `ResultEncoding::Portable` retains the **entire MCP result object**
as `zcall::Data::Json`, including content, structuredContent, isError and metadata.
It omits the separate provider-native error flag because several zcall protocol
adapters cannot encode that field. Actual tool failures remain visible inside the
JSON envelope. The model receives that JSON representation, not provider-native
image/audio tool-result blocks. Nothing is automatically flattened or fetched.

This full-envelope adapter is **not a privacy filter**: `_meta` and nested
metadata also reach the model. If a server/application contract marks any data
as client-only or confidential, do not pass that result through the convenience
adapter unchanged. Construct an application-filtered `zcall::Content::ToolResult`
instead, retaining the original model call ID and the tool-failure indication.

`NativeErrorFlag` is an explicit opt-in through `content_with_encoding` or
`append_result_with_encoding` for a format supporting it, such as Anthropic.
An unsupported destination still fails in zcall rather than silently dropping
the flag. MCP tool annotations/output schemas remain on `ToolView`; they are not
input schemas or permission grants. MCP schemas do not imply a provider's strict
schema mode, so discovered definitions explicitly set `strict=false`.

JSON-RPC failures and modern `input_required` are not completed tool results.
Handle them separately. The client validates the basic input-required envelope
and exposes it without implementing MRTR continuation or executing callbacks.
The bridge cannot consume it as a successful `CallResult`.

## Ownership, transports and limits

Retain discovery/result bytes and their MCP token tapes while borrowed views or
zcall requests reference them. Escaped model IDs/names use caller identity
storage; escaped JSON-string arguments use separate argument storage. After
encoding, argument storage can be reused while identity storage remains retained.
Conversion into zcall validated JSON uses its public checked constructor and
caller token scratch; it does not introduce an unchecked lifetime shortcut.

The core works with complete messages supplied by any transport. There is **no
built-in HTTP/TLS/SSE, OAuth or remote-URL client**. An HTTP integration owns its
request headers, session IDs, status codes, authentication, SSE framing and
connection lifecycle, then supplies decoded JSON messages to `Client::receive`.
For modern HTTP cancellation close the applicable response stream; the provided
`cancel` encoder emits the notification used by stdio, not an HTTP abort.

The client keeps a request pending after encoding cancellation because the peer
may already have completed it. `abandon` is an explicit local timeout/cleanup
decision and never reuses that ID. Neither operation proves remote effects were
undone. Child-process cleanup manages only the direct child, not a process tree.

## Evidence and boundaries

`tests/client.rs` checks state transitions, capacity retries, IDs, pagination,
notifications, malformed results and explicit error paths. `tests/bridge.rs`
checks real zcall request encoding across five tool-capable protocols, full result
preservation and separate model/MCP identities. `tests/stdio.rs` checks framing
and actual child lifecycle; `scripts/client_smoke.py` uses both the Rust example
server and an independent Python JSON-RPC peer. These are tools-profile tests,
not a full official MCP conformance certification or a network latency guarantee.
