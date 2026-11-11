# Measured local performance — 2026-11-11

Source revision: `71db6844e2f1d9f38ff94150e90c54db444d021a`.
Full benchmark and candidate-package verification:
https://github.com/LIghtJUNction/zmcp/actions/runs/42959252983
Six-way cross-platform CI:
https://github.com/LIghtJUNction/zmcp/actions/runs/42959252840
Both runs completed successfully. No crate was published.

Environment: GitHub-hosted Azure x86_64 VM, 4 available virtual CPUs, host CPU
reported as AMD EPYC 9V74 80-Core Processor, Linux 7.0.0-1019-azure,
rustc 1.99.1 (58cf8f9c3 2026-10-13), LLVM 23.1.1. Shared virtualized hardware;
power policy and neighboring workloads were not controlled.

Reproduce with `cargo bench --locked --bench core` at the recorded revision,
without ZMCP_BENCH_SMOKE. Mean latency is amortized over a batch; p50/p95/p99 are
individually timed calls including clock overhead. Throughput derives from mean.

## Protocol operations

| Case | Input bytes | Mean ns | p50 ns | p95 ns | p99 ns | Operations/s | Workspace bytes |
|---|---:|---:|---:|---:|---:|---:|---:|
| Legacy initialize | 146 | 1018.90 | 1021 | 1062 | 1131 | 981,452.09 | 4096 |
| Legacy ping | 40 | 337.98 | 360 | 390 | 401 | 2,958,716.66 | 4096 |
| Modern discovery | 175 | 1065.36 | 1051 | 1072 | 1112 | 938,653.84 | 4096 |
| Local tool round trip | 206 | 1332.51 | 1322 | 1372 | 1383 | 750,462.44 | 4136 |

The local round trip includes JSON and RPC validation, modern request metadata,
name dispatch and text-result encoding. It does not include a network request,
model inference, arbitrary tool business logic or a zharness composition layer.
Initialize uses a fresh server each time rather than reusing initialized state.

## JSON objects

| Bytes | Mean ns | p50 ns | p95 ns | p99 ns |
|---:|---:|---:|---:|---:|
| 64 | 172.68 | 191 | 201 | 201 |
| 256 | 686.43 | 671 | 691 | 701 |
| 1024 | 2562.56 | 2554 | 2574 | 4847 |
| 4096 | 10345.46 | 10095 | 10155 | 13490 |
| 65536 | 162393.72 | 161120 | 168541 | 177114 |

Modern MCP metadata alone can exceed 64 bytes; the 64-byte JSON object is not
presented as a complete modern MCP request.

## Tool list serialization, including validation

| Tools | Mean ns | p50 ns | p95 ns | p99 ns | Workspace bytes |
|---:|---:|---:|---:|---:|---:|
| 1 | 1129.61 | 1101 | 1141 | 1152 | 3440 |
| 4 | 1209.73 | 1172 | 1222 | 1242 | 4544 |
| 16 | 1733.73 | 1693 | 1743 | 1753 | 8960 |
| 64 | 7283.28 | 7181 | 7241 | 7271 | 26624 |
| 256 | 59092.37 | 58196 | 64716 | 67330 | 97280 |
| 1024 | 1265047.93 | 1301337 | 1317861 | 1361367 | 379904 |

The 1024-tool list is approximately 1.265 milliseconds, not low microseconds.
Tool uniqueness currently uses a bounded quadratic check per list response. A
future optimization should validate/precompute a caller-owned tool index once
without removing duplicate validation or altering serialized content.

All measured operations asserted zero allocator calls. Fixture allocation and
printing occur outside timing. Workspace bytes count configured caller buffers
and retained state, not RSS, allocator metadata or worst-case recursion stack.
The raw CSV, environment details and source archive are retained in the workflow
artifact. No mature SDK speedup claim, complete protocol-conformance claim or
latency guarantee for all message sizes is made.
