# Architecture

Each module owns one boundary. `json` validates once into caller-sized offset
tokens; field iteration subsequently follows token indexes without reparsing.
`rpc` distinguishes message categories. `mcp` checks version/lifecycle and returns
actions. `tools` encodes definitions and results. `tracker` accounts for in-flight
IDs and cancellation. `transport` frames stdio or defines complete-message I/O.

`client` owns only protocol state and caller-sized pending slots. Request IDs are
monotonic integers and are not reused during a client lifetime. Successful
encoding reserves a slot; successful response validation releases it. Malformed
or unrelated replies cannot complete a different operation. Notifications never
complete requests. Client construction is for a new protocol connection/identity,
not a way to reset ID counters on an existing legacy connection.

`bridge` uses the public zcall v1 contract rather than inventing parallel model
types. A retained Invocation binds the independent MCP and provider call IDs.
Validated MCP values are revalidated through zcall's public JSON constructor at
the type boundary; this avoids unsafe constructors or a second direct dependency.
Default bridging retains the whole result envelope as JSON for portability.

The optional `stdio` module owns a spawned child and its pipes. Framing remains
bounded and caller-buffered; std process creation is not allocation-free. It is
blocking, with bounded graceful shutdown but no response-read deadline. Async
I/O, HTTP/TLS, credentials and scheduling remain application responsibilities.

Offsets use u32 to keep each token compact. Inputs larger than u32::MAX are
rejected. Recursive descent is bounded to at most 64 container levels even when a
caller sets a larger limit. Duplicate-key detection is quadratic in the number
of fields of one object and is bounded by max_members. This tradeoff avoids an
unbounded hash map; adversarial wide-object performance remains a benchmark case.

Server identity is borrowed. Modern capability metadata is borrowed per request
and never cached across requests. Legacy lifecycle changes occur only after a
successful response encoding, so growing a too-small output does not consume the
initialization transition. Initialization starts a protocol connection, not an
agent or model session.

The application must track concurrent requests, retain borrowed storage until
completion, authorize execution, schedule or cancel work, and deliver exactly one
response. The server exposes these pieces; it does not secretly do them. Calls
may finish in a different order because response IDs are retained explicitly.
