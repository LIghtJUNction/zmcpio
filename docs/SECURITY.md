# Security

Treat model output, client metadata, tool descriptions and tool results as
untrusted. Tool-name matching is not authorization. Applications must explicitly
approve a call before execution, enforce their JSON Schema/business policy and
keep secrets out of results and diagnostic messages.

Set Limits and caller storage based on the endpoint's resource budget. Duplicate
keys are rejected after decoding escapes. Invalid UTF-8, unpaired surrogates,
integer conversion overflow, malformed numbers, trailing input and incomplete
frames are rejected. No JSON Schema references are fetched from the network.

A complete valid JSON object only proves syntactic validity. It does not prove
that a tool is safe, affordable, authorized, idempotent or semantically valid.

On output capacity failure, retain the execution result and retry encoding; do
not re-execute a side effect. On input framing capacity failure, retain accepted
bytes and submit only the unconsumed suffix. A cancelled request is an application
signal, not a guarantee that an already-started external operation was undone.

The protocol core and zcall bridge have no unsafe code or allocator use. The
optional stdio adapter and std examples use standard-library process/I/O APIs.
Benchmark allocator instrumentation is isolated from the published library.

A discovered tool is not authorized merely because a server advertises it or a
model selects it. Apply allowlists and schema/business validation before sending
the encoded tools/call. Treat returned resources, URLs and instructions as data;
the library never fetches or executes them. MCP IDs and model call IDs must be
correlated through the bridge Invocation, not assumed equal.

The convenience result bridge preserves the whole MCP result, including `_meta`
and nested resource metadata. It is not a privacy filter. Applications using
client-only/private metadata must construct a filtered model tool result instead
of forwarding that envelope unchanged; preserve call correlation and error state.

After a partial transport write the operation may already be running remotely.
The stdio writer becomes unusable rather than automatically resending. Cancelling
or abandoning a request does not roll back work. The process adapter executes the
explicit Command without inserting a shell; choose its executable, environment
and arguments deliberately. It manages the direct child, not a process tree.

## HTTP, SSE and callbacks

Use an explicit `http::SecurityPolicy` for Origin, authorization and legacy
session ownership. A session ID is not a credential. Reject duplicate routing
or credential headers, header/body mismatches and malformed custom parameter
annotations before any handler runs. An absent Origin does not authenticate a
request; enforce authorization independently. Bind local services to loopback.

The optional HTTP/1.1 layer rejects ambiguous Content-Length/Transfer-Encoding,
obsolete folded fields, header injection and oversized headers/bodies. Configure
read/write deadlines on supplied sockets; the generic stream layer cannot invent
them. Use buffered I/O and bounded concurrency. Close after framing or partial
write errors; a poisoned chunk writer must not be reused.

Use a verified TLS implementation for remote credentials. This crate deliberately
does not invent a TLS verifier, OAuth token store, browser consent flow or
certificate bypass. Never follow HTTP redirects or legacy endpoint events to
another origin with credentials automatically. Legacy endpoint resolution is
conservative: ambiguous encodings, userinfo, backslashes and cross-origin URLs
fail explicitly.

SSE capacity retries retain accepted data; only feed the unconsumed suffix.
Commit a legacy resume ID after accepting the associated complete event; do not
checkpoint an incomplete block. A resume/replay store must be identity-bound and
stream-scoped. Modern response-stream closure means cancellation; legacy closure
does not. Cancellation cannot undo external effects already completed.

Sampling, elicitation, resource access and task continuation are requests for
application decisions, not authorization. Require explicit capabilities and
user/business approval where appropriate; preserve opaque continuation state
without evaluating it. Apply independent per-request, wall-clock, concurrency,
pagination, subscription and task budgets.

Legacy batches are opt-in only for 2025-03-26. Preflight the full array with the
batch parser and validate the HTTP envelope before effects. Then authorize each
operation separately; a batch is not a permission to execute every sibling.
Initialization and modern metadata cannot be smuggled into this legacy boundary.
Never answer malformed response batches or re-execute completed tools to grow a
response array buffer.
