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

After a partial transport write the operation may already be running remotely.
The stdio writer becomes unusable rather than automatically resending. Cancelling
or abandoning a request does not roll back work. The process adapter executes the
explicit Command without inserting a shell; choose its executable, environment
and arguments deliberately. It manages the direct child, not a process tree.
