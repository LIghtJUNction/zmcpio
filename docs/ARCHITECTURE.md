# Architecture

Each module owns one boundary. `json` validates once into caller-sized offset
tokens; field iteration subsequently follows token indexes without reparsing.
`rpc` distinguishes message categories. `mcp` checks version/lifecycle and returns
actions. `tools` encodes definitions and results. `tracker` accounts for in-flight
IDs and cancellation. `transport` frames stdio or defines complete-message I/O.

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
