# zcall 0.4 integration evidence

Date: 2026-11-20. This source candidate aligns zmcpio with the current zcall contract and preserves the existing MCP bridge and transport behavior.

## Source and dependency contract

The tested zmcpio source is [ec43faa5347962517807bdf3a2ff4d667e0a8fe2](https://github.com/LIghtJUNction/zmcpio/commit/ec43faa5347962517807bdf3a2ff4d667e0a8fe2), based on the published 0.4.0 source [bc89d7fa3b0249b3a9037748e2113b31615e8c89](https://github.com/LIghtJUNction/zmcpio/commit/bc89d7fa3b0249b3a9037748e2113b31615e8c89). The candidate changes Cargo.toml, Cargo.lock, README.md and CHANGELOG.md. Its dependency is the available, non-yanked registry release zcall 0.4.0, whose source is [45df84f93848a9f7dc66eb7da953d18d14bb08f7](https://github.com/LIghtJUNction/zcall/commit/45df84f93848a9f7dc66eb7da953d18d14bb08f7).

The complete default dependency graph is `zmcpio -> zcall 0.4.0 -> zconv 0.4.0`. Both dependency edges keep default features disabled. The optional zcall server stays outside this graph. The existing model and tool APIs compile unchanged; the dependency update requires no changes to zmcpio Rust source.

The source checkout retains the current package version until the release workflow selects a new version. CHANGELOG.md records this work under Unreleased. A release should use the next minor version, 0.5.0, because the public `zmcpio::zcall` re-export and bridge parameters now use the zcall 0.4 crate identity. Applications separately depending on zcall must align that dependency, and consumers pinning zmcpio exactly must update their constraints during release propagation.

## Library validation

The following checks passed on Rust 1.100.0, `x86_64-unknown-linux-gnu`. WASM is a compile check for `wasm32-unknown-unknown`.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `python3 scripts/audit.py` | Three-package dependency closure passed |
| `cargo test --locked --all-targets --all-features` | 209 passed; two subprocess fixture entry points marked ignored and invoked by their parent tests |
| `cargo test --locked --no-default-features` | 170 passed |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | Passed |
| `cargo check --locked --no-default-features --target wasm32-unknown-unknown` | Passed |
| `python3 scripts/stdio_smoke.py` | Three requests and one notification completed |
| `python3 scripts/client_smoke.py` | Rust/Python stdio, legacy negotiation, pagination, Unicode, rejection paths and zcall bridge passed |
| `python3 scripts/http_smoke.py` | Rust/Python JSON and SSE, Origin rejection, malformed RPC, method checks and 100-continue passed |
| `cargo package --locked --offline` | Source package and extracted-package compilation passed |

`ZMCPIO_BENCH_SMOKE=1` selected the existing benchmark execution smoke mode during all-target testing. These runs establish regression behavior; they supply no new performance claim. The package check used the locally cached, previously verified registry dependencies.

## Direct consumer validation

The direct Cargo consumer is zuse-computer 0.2.2. A clean checkout at [94c837551a7a2fd8ec9f577a3e2f57309bd5ce5a](https://github.com/LIghtJUNction/zuse/commit/94c837551a7a2fd8ec9f577a3e2f57309bd5ce5a) was tested with a temporary Cargo patch selecting the exact zmcpio candidate source. Cargo resolved this graph:

| Package | Selected source/version |
| --- | --- |
| zuse-computer | 0.2.2 at 94c837551a7a2fd8ec9f577a3e2f57309bd5ce5a |
| zmcpio | Candidate ec43faa5347962517807bdf3a2ff4d667e0a8fe2 |
| zcall | Registry 0.4.0 |
| zconv | Registry 0.4.0 |
| z64 | Registry 0.1.2 |
| zclap | Registry 0.1.4 |
| zsys | Registry 0.1.1 |

The zuse library suite passed 110 tests. The binary built successfully, and `tests/transports.py --binary <built-zuse>` passed all 12 checks. These checks covered real stdio, loopback HTTP and Unix socket servers; legacy and modern messages; malformed requests; Host, Origin and token binding; read-only policy; bounded frames; human handoff integrity; and an actual screenshot from a private Xvfb display. The original consumer lockfile was restored after verification, leaving its isolated checkout clean.

At observation time, zuse main was [75201caf6b73bc3a53671d1458db1f95f5e95f17](https://github.com/LIghtJUNction/zuse/commit/75201caf6b73bc3a53671d1458db1f95f5e95f17). The difference from the tested source contains delivery/evidence documentation and a release browser-selection test. Cargo manifests and runtime source are unchanged. The acceptance result remains bound to the tested source above.

To reproduce the direct consumer check, create sibling source checkouts named `zmcpio` and `zuse` at the exact commits above. Run the following from their parent directory:

```sh
zmcpio_source="$(git -C zmcpio rev-parse --show-toplevel)"
cargo_patch="patch.crates-io.zmcpio.path=\"$zmcpio_source\""
cd zuse
export CARGO_TARGET_DIR="$PWD/target"
cargo update --package zmcpio --config "$cargo_patch"
cargo tree --locked --config "$cargo_patch"
cargo test --locked --lib --config "$cargo_patch"
cargo build --locked --bin zuse --config "$cargo_patch"
python3 tests/transports.py --binary "$CARGO_TARGET_DIR/debug/zuse"
```

Use an isolated checkout because the temporary patch changes Cargo.lock while it is active.

## Propagation and release ownership

Project Z snapshot [76dd114c11685d430dd55bb0d78b064a7349af73](https://github.com/LIghtJUNction/project-z/commit/76dd114c11685d430dd55bb0d78b064a7349af73) reports the following affected repositories through `python3 scripts/project_z.py impact --repositories zmcpio`:

| Consumer | Current evidence and next step |
| --- | --- |
| zuse | Direct source compatibility verified as above; update the exact zmcpio dependency and release lockfile after zmcpio 0.5.0 becomes available |
| ssmcp | Existing SSMCP owner and Project Z coordinator must adopt the new zuse/library combination with the active release candidate |
| zrc | Project Z coordinator must validate the runtime dependency chain with its existing owner |
| ztype | Existing input-method owner must validate the new shared-library combination in the accepted application candidate |
| oh-my-ztty | Existing suite owner must update source pins and verify the resulting installed artifacts |

The Project Z coordinator owns acceptance of the next compatible aggregate snapshot. The stable snapshot currently pins zcall 0.3.0 and remains a separate combination. The verified candidate and direct-consumer results provide the reusable source input for the zcall 0.4 cohort; publication, exact consumer version changes, application acceptance and aggregate adoption remain explicit release steps.
