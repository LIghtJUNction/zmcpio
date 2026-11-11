# Reproducing transport validation

Validation distinguishes byte-level primitives, protocol behavior, actual I/O
and host-owned application behavior. Passing these tests is evidence for the
listed contracts, not a blanket claim of official MCP certification.

## Build and interoperability

Run from a source checkout with the Cargo.lock file unchanged:

```sh
cargo test --locked --all-targets --all-features
cargo test --locked --all-targets --release
cargo test --locked --no-default-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo run --locked --example mcp_extensions
python scripts/stdio_smoke.py
python scripts/client_smoke.py
python scripts/http_smoke.py
python scripts/audit.py
python scripts/test_release.py
cargo check --locked --no-default-features --target wasm32-unknown-unknown
cargo package --locked
```

Install the WASM target with rustup first. CI runs stable and the pinned
rust-version on Linux, Windows and macOS. Transport regression runs tests before
formatting to expose compiler/runtime failures separately; the full CI still
requires formatting and warnings-as-errors. No validation job publishes a crate.

| Contract | Evidence |
|---|---|
| Modern request/response headers, status, versions and security hooks | tests/http.rs |
| JSON and chunked SSE over real sockets; no dispatch before policy checks | tests/http_network.rs |
| Independent Python JSON/SSE peer; Rust executable client; invalid Origin/RPC/methods; 100-continue | scripts/http_smoke.py |
| Old dual-endpoint HTTP+SSE, initialization retained across POSTs, duplicate reply without tool replay | tests/legacy_sse_network.rs |
| Every split, BOM/UTF-8, CRLF, heartbeats, EOF, backpressure and capacity recovery | tests/sse.rs |
| Legacy session creation, truncated stream, explicit GET replay without tool re-execution, DELETE/404 cleanup | tests/http_session_network.rs |
| Exact-2025-03-26 batches, per-item errors, atomic arrays, shared HTTP/session gates | tests/batch.rs and tests/http_batch.rs |
| Same-origin legacy endpoint negotiation and atomic URI failures | tests/legacy_sse.rs |
| Framing ambiguity, short reads/writes, limits, keepalive, interruption, informational heads and poisoned state | tests/http_io.rs, tests/http_io_regressions.rs, tests/http_informational.rs |
| MRTR, Tasks, callback opt-in, subscription ordering/filter/correlation | tests/extensions.rs and tests/client.rs |
| Original protocol, JSON, tool bridge and real stdio process behavior | existing core/mcp/bridge/client/stdio suites |

The two ignored stdio fixture tests are subprocess entry points explicitly
launched by the surrounding process-adapter tests; they are not skipped feature
coverage. TCP tests use loopback with deadlines and do not contact remote MCP
servers or run privileged tools.

## CPU and allocation measurement

```sh
cargo bench --locked --bench transport
```

Unset ZMCPIO_BENCH_SMOKE for the normal 501-observation, 512-operation batch
measurement. A set value enables an 11-observation smoke mode for CI execution
checks. Transport regression runs the normal release measurement explicitly;
cross-platform matrix runs use the faster smoke mode.

The CSV reports amortized mean nanoseconds, instrumented p50/p95/p99, operations
per second, measured allocation count and explicit workspace bytes. Cases cover
SSE encoding/decoding at 64, 256, 1024, 4096 and 65536 payload bytes, 17-byte
fragments, a bytewise decoder, modern ASCII/Unicode header validation, and custom
parameter header extraction/encoding.

The global allocator counter asserts zero allocations in the measured operations.
Setup, warmup, sample storage and CSV printing are excluded. The safe no-std
library does not contain this benchmark-only allocator instrumentation.

These are local warm-buffer CPU measurements, not network latency or complete
MCP roundtrip timings. SSE cases exclude JSON parsing; header and schema cases
use pre-parsed fixtures. workspace_bytes includes the explicit retained buffers
and state described by each case, not immutable fixtures, temporary stack frames,
executable pages or process RSS. Percentiles include per-observation Instant
overhead. A shared CI runner is not a dedicated benchmarking machine.

Do not claim “fastest,” a latency SLA, a memory-RSS figure or a speedup without an
equivalent controlled baseline. Preserve runner/toolchain, commit, build profile,
case size and sample count when quoting a measurement.

## Deliberate boundaries

The default core remains no_std with no allocator or unsafe code and one direct
dependency, zcall; zconv is transitive. Optional std adapters can allocate during
process/socket/buffer setup, which is distinct from the measured codec hot paths.

TLS verification, OAuth acquisition, app authorization, complete JSON Schema
validation, durable replay/task state, task scheduling, callback execution,
async runtimes and automatic retries are host responsibilities. The built-in
HTTP adapter is HTTP/1.x, not an HTTP/2/3 implementation. JSON-RPC batches are
bounded and supported only for the exact 2025-03-26 revision, never initialization.

The release workflow retains its explicit bump/version mechanism and
CARGO_REGISTRY_TOKEN publication gate. A transport change does not implicitly
authorize publishing a new crate version; the source checkout may be ahead of
the most recent published release.
