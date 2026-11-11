# Performance

Client/bridge measurements use `cargo bench --locked --bench client`.
`client_call_encode_receive` includes a fresh bounded client, request encoding and
full response parsing/correlation. `client_server_roundtrip` uses a persistent
client and validates/encodes both sides. `zcall_bridge_encode_receive` also prepares an
escaped model call and converts the entire correlated MCP result through zcall's
checked JSON API. Caller storage is allocated outside timing; each case asserts
zero measured allocator calls. These exclude process startup, IPC, HTTP, model
inference and application tool business logic. They are not network latency SLAs.

Run `cargo bench --locked --bench core`. Capture the git SHA, `rustc -Vv`, CPU,
OS and power policy. Initializers construct a fresh legacy Server for every
operation; discovery and tool calls include modern per-request metadata checks.
Tool round trips include complete parsing, exact-name dispatch, content encoding
and output bounds checks. Tool lists include definition uniqueness checking.

JSON sizes: 64, 256, 1024, 4096 and 65536 bytes. Tool counts: 1, 4, 16, 64, 256 and
1024. Modern MCP metadata itself can exceed 64 bytes; the 64-byte JSON case is not
misrepresented as a full modern MCP call.

mean_ns is an amortized batch mean. Individual p50/p95/p99 samples include clock
instrumentation overhead. Throughput is derived from mean_ns. A benchmark-only
System allocator wrapper counts allocation/reallocation requests and asserts zero
during measured operations. Fixture construction, buffers and printing are outside
timing. workspace_bytes reports configured caller storage and retained state, not
RSS, allocator metadata, fixture strings or maximum recursive stack consumption.

Current duplicate-key and tools-list uniqueness checks are quadratic within
caller-bounded collections. Large-list timing must not be extrapolated from small
message timing. No unmeasured comparison with a mature MCP SDK or blanket
microsecond guarantee is claimed. Network, model and arbitrary user-tool latency
are outside this core's contract.

CI smoke checks are correctness checks, not stable speed comparisons. A fair
external baseline and independent hardware replication remain separate work.
