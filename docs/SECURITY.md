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

The library has no unsafe code and does not link alloc. The std examples and
benchmark use standard-library I/O outside the protocol core. Benchmark allocator
instrumentation, when enabled, is isolated from the published library.
