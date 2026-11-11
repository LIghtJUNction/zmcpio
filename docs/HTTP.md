# HTTP and SSE integration

The default crate handles protocol bytes without sockets, allocation or a runtime.
Enable `std` for bounded blocking HTTP/1.1 framing over a caller-supplied
`BufRead`/`Write` stream. A verified TLS stream can use the same interface; TLS,
DNS, OAuth, connection pooling and async scheduling remain application choices.

## Choose the negotiated transport, not merely “SSE”

| Profile | Requests and replies | Session and stream lifetime |
|---|---|---|
| 2026-07-28 Streamable HTTP | POST; request returns JSON or SSE, accepted notification returns empty 202 | Stateless binding; no legacy GET/DELETE, session header or Last-Event-ID behavior. Closing the response stream cancels that request. |
| 2025-03-26, 2025-06-18, 2025-11-25 Streamable HTTP | POST; request returns JSON or SSE, accepted notification/response returns empty 202; optional GET SSE and DELETE | Optional server-issued sessions. GET with Last-Event-ID supports host-managed resumption. Disconnection alone does not cancel a request. |
| 2024-11-05 HTTP+SSE | A long-lived GET receives an endpoint event and JSON-RPC messages; separate POSTs send messages and return 202 | Two channels share one legacy MCP lifecycle. A POST acknowledgment is not the JSON-RPC result. |

Legacy negotiation here is single-message compatibility. JSON-RPC batch arrays
are rejected, including the batch feature of the 2025-03-26 specification.
The transport does not invent a session or silently switch protocol versions.
For 2025-03-26, a missing version header is accepted only by an explicitly
configured matching server; later legacy versions validate it after initialization.

The version selection is deliberate: modern Tasks use get/update/cancel and
per-request extension capabilities, while legacy Tasks use their own negotiated
methods and result shape. Treating all revisions as one protocol loses semantics.

## Runnable client and server

Run these commands in separate terminals from the source checkout:

```sh
cargo run --locked --features std --example mcp_http -- server 8080 sse
cargo run --locked --features std --example mcp_http -- client 127.0.0.1:8080
```

Change `sse` to `json` for a single JSON response. The example binds only to
127.0.0.1, has socket deadlines and exposes only a fixed echo tool. It is not an
authenticated production daemon. It supports the Expect: 100-continue exchange;
unknown expectations are rejected without waiting for their body. Its client
skips a bounded number of informational responses and rejects protocol upgrades.

`tests/legacy_sse_network.rs` demonstrates the older dual-connection exchange
using actual TCP: persistent GET, endpoint negotiation, separate keepalive POSTs,
initialization and a tool call. Responses arrive only through GET. A duplicate
tool reply is rejected without another POST or another tool execution.

## Client integration

1. Choose `http::Version` and create `ClientSession` with caller-owned session
   storage. Use a matching `client::Client` era and explicit legacy negotiation.
2. Encode the MCP request into a bounded buffer. Use `ClientSession::post_headers`
   to generate standard headers. Modern routing names are the tool/prompt name,
   resource URI or task ID, depending on the method.
3. For tools with `x-mcp-header` annotations, call `http::parameters` on the
   selected input schema and arguments, then `write_parameter_headers`. Append
   these headers using your HTTP stack. Add authentication separately.
4. Send exactly once. With `http_io`, use `write_request` or
   `write_request_with(..., Connection::KeepAlive)`; set deadlines on the stream
   before I/O. Use `read_response_head` for a bounded informational-head loop.
5. Interpret the final status and Content-Type with `response_mode`. Read JSON
   into a bounded message buffer, or feed decoded HTTP body chunks to
   `sse::Decoder`. Pass complete JSON-RPC payloads to `Client::receive`.
6. For a legacy initialized session, retain a valid server-issued ID with
   `set_session_id`. Validate its ownership server-side. `observe_status(404)`
   clears local session state; it does not repeat the failed tool call.

A transport write failure is ambiguous: the peer might already have executed the
tool. Neither reconnecting nor obtaining a new session proves a retry is safe.
Applications must decide from operation semantics, idempotency controls and
their own execution records. No helper automatically repeats a tool request.

For legacy recovery, generate GET headers with `get_headers(last_event_id)`
and let the host select and replay previously stored events. Event storage,
retention, unique event IDs and ownership checks are not provided by this crate.
Use `delete_headers` only where session termination is supported.

## Server integration

Parse and bound the HTTP head/body before dispatch. Copy header views into a
caller-owned slot array if needed; values remain borrowed. Construct
`Incoming { method, headers, message }`, then call
`ServerConfig::validate` with an application-defined `SecurityPolicy`.

That policy has three separate checks: allowed Origin, authorization and session
ownership. A session identifier is not a credential. The core validates media
types, version-specific methods, standard singleton headers and modern
header/body mirroring before returning a message/listen/terminate action.
Header/body mismatch takes precedence over an unsupported modern version.

For a selected tool schema, additionally call `parameters` and
`validate_parameter_headers` before performing side effects. Standard validation
cannot discover tool-specific annotations without that schema. Validate schemas
before publishing tools over HTTP; do not advertise an invalid header annotation.

Only then call `mcp::Server::handle`, check application permissions, perform the
requested work and encode a result. An `Action::CallTool` or
`Action::Extension` is a description of work, not execution. For legacy HTTP,
retain the same Server across the logical session, not one Server per POST.

Map `HttpError` through `status`/`write_rpc`; use `rpc_error_status` when
mapping a protocol error reply. Send an Allow header with a 405 response.
Accepted notifications and accepted legacy response messages receive empty 202.
Keep JSON-RPC parse/invalid-request errors distinct from tool execution errors.

## Incremental SSE and bounded memory

`Decoder::new` receives separate buffers for a line, joined data, event name and
event ID, plus a maximum raw event-block size. Reuse them across events.
`feed` reports exactly how many input bytes were consumed; keep the remainder.
On Ready, handle `event()` before `consume()`. A pending event applies
backpressure rather than overwriting data.

On NeedCapacity, use the reported buffer and size with `replace_buffer`, then
retry the unconsumed bytes. No complete event is delivered until a blank line.
`finish` discards an incomplete trailing event; it does not turn a disconnected
tool request into success. Comment heartbeats, CR/LF/CRLF, a leading BOM,
multiline data and UTF-8 replacement are handled across arbitrary chunk splits.

Event IDs are per block, not automatically inherited. Commit a legacy resume
checkpoint only after accepting the complete block according to application
policy; retain the checkpoint outside the decoder. Metadata-only blocks may
carry IDs or retry hints without a JSON-RPC payload. `retry_hint()` can expose a
complete retry field before the final delimiter. Modern HTTP ignores these
legacy resumption mechanisms.

`write_event` and `write_comment` preflight output capacity. Event data must be
UTF-8 and cannot contain CR; LF is encoded as multiple data lines. The JSON
encoder already escapes embedded string controls, avoiding field injection.

## Optional blocking I/O

`http_io` supports Content-Length, chunked and close-delimited response bodies,
bounded trailers, short-read/write handling, Interrupted retries and sequential
keepalive reuse. Supply a buffered reader/writer to avoid one syscall per field.
Consume the entire framed body before reusing a connection.

It rejects ambiguous CL+TE, duplicate security/framing headers, folded headers,
bare LF, HTTP/1.0 transfer coding, unsupported compression and malformed chunks.
A framing/capacity failure poisons BodyReader; a partial write/flush failure
poisons ChunkWriter. Close the connection after either error. ChunkWriter flushes
each event and emits the final zero chunk only on explicit successful finish.

This is an MCP-oriented HTTP/1.1 adapter, not a general browser stack. It does not
implement HTTP/2 or HTTP/3 framing, CONNECT, upgrades, automatic redirects,
compression, certificate verification or credential acquisition. Use the
no-std protocol layer with an existing verified stack when those are required.
The stream's timer, executor and connection policy belong to its owner.

## Older endpoint resolution

`legacy_sse::Session` accepts the endpoint event before message events and
resolves same-origin relative/absolute endpoints into caller storage. It rejects
cross-origin endpoints, userinfo, fragments, control characters, backslashes,
encoded dot segments and ambiguous repeated slashes. It does not forward a token
or open the destination itself. This conservative URI resolver is not a browser
URL implementation; rejected unusual URLs require an explicit host decision.

## References and tests

Normative sources checked on 2026-11-11:

- https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http
- https://modelcontextprotocol.io/specification/2025-11-25/basic/transports
- https://modelcontextprotocol.io/specification/2025-03-26/basic/transports
- https://modelcontextprotocol.io/specification/2024-11-05/basic/transports
- https://html.spec.whatwg.org/multipage/server-sent-events.html
- https://www.rfc-editor.org/rfc/rfc9112

See `tests/http.rs`, `tests/sse.rs`, `tests/http_io*.rs`,
`tests/http_network.rs`, `tests/legacy_sse*.rs` and
`scripts/http_smoke.py`. The Python peer independently checks the executable
Rust server and also runs the Rust client; it is not an official conformance
suite. Reproduction commands and measurement scope are in [VALIDATION.md](VALIDATION.md).
