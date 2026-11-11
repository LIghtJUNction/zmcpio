# MCP tools profile

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

HTTP headers must be validated by the transport adapter, which owns status codes,
authentication, Origin checks, request size limits, SSE, reconnect and disconnect
semantics. HttpHeaders checks the tools-profile routing fields; it is not a
complete HTTP implementation. WebSocket/TCP adapters are custom transports, not a
claim that those transports are official MCP bindings.

Unsupported profiles: MRTR, subscriptions, prompts/resources routing, sampling,
elicitation, roots and authorization. Capabilities for unimplemented methods are
not advertised. An official discovery fixture is checked; no claim is made of
passing a complete official conformance suite.
