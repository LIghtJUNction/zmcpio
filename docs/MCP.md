# MCP protocol boundaries

Client behavior is described in [CLIENT.md](CLIENT.md). Explicit legacy
single-message negotiation covers 2024-11-05, 2025-03-26, 2025-06-18 and
2025-11-25. The server selects a supported requested revision or offers the
latest supported legacy revision. Bounded JSON-RPC batches are supported only for
2025-03-26 through the explicit `batch` module; initialization is never batched.
Other revisions reject arrays. Modern discovery/list responses include private, zero-TTL cache
hints; the client does not cache or grant permission from those hints.

Normative references checked 2026-11-11:
- https://modelcontextprotocol.io/specification/2026-07-28
- https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/schema/2026-07-28/schema.ts
- https://modelcontextprotocol.io/specification/2025-11-25
- https://www.jsonrpc.org/specification

Modern requests include params._meta keys
`io.modelcontextprotocol/protocolVersion` and
`io.modelcontextprotocol/clientCapabilities`. ClientInfo is optional. Discovery
is available without an initialization exchange. Results contain resultType and
serverInfo metadata. Unsupported version errors use -32022 and list supported
versions. The caller's HTTP adapter maps the corresponding failure to HTTP 400.

Legacy requests have an explicit initialize -> notifications/initialized
transition, with 2025-11-25 as the supported fallback version. Ping is legacy
only. Callers should use one Server per legacy connection. Modern requests are
not allowed to inherit client capabilities from legacy or prior modern calls.

The application implements deterministic tool ordering and pagination. Omitted
arguments become {}. A present arguments field must be an object. Unknown tools
and policy/schema rejection become -32602 or an application-defined permission
error; actual tool failures return a tool result with isError=true.

Modern structuredContent can be any JSON value. Legacy structuredContent must be
an object. Input schemas must be object schemas; this API validates their wire
shape, not schema correctness or tool arguments against the schema. Output-schema
matching is also the application's responsibility.

Notifications never receive responses. Cancellation may race completion; ignore
unknown/completed IDs. Tracker permits modern ID reuse after completion or legacy
connection-wide retention. Retention exhausts a caller-sized slot table rather
than allocating. Progress monotonicity is enforced by Tracker when used.

HTTP headers must be validated before side effects. Use
`http::ServerConfig::validate` for standard headers and security policy, then
`http::parameters`/`validate_parameter_headers` for the selected tool schema's
mirrored parameters. The older `mcp::HttpHeaders` helper is only the original
tools-profile check and is not a complete modern HTTP validator.
See [HTTP](HTTP.md) for version-specific status/cancellation/session rules.

Resources, prompts, completion, legacy logging and callbacks, modern MRTR and
subscriptions, and task messages have explicit opt-in wire boundaries; see
[EXTENSIONS.md](EXTENSIONS.md). The host implements the work and only advertises
capabilities it actually provides. OAuth flows, general JSON Schema validation,
persistent task/replay storage and automatic execution remain outside this crate.
An official discovery fixture and adversarial/independent-peer tests are checked;
no claim is made of passing a complete official conformance suite.
