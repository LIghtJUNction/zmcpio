# Changelog

## Unreleased — HTTP transports and opt-in protocol extensions

- Add allocation-free SSE encoding/decoding and era-aware Streamable HTTP rules.
- Add bounded optional HTTP/1.1 I/O with JSON/chunked SSE, strict framing, explicit
  keepalive policy, interrupt recovery and terminal partial-write handling.
- Add same-origin legacy HTTP+SSE endpoint negotiation and explicit older handshake
  baselines without batch handling or automatic tool retries.
- Add modern routing/custom-parameter header validation, security policy hooks,
  caller-owned legacy sessions and resume metadata.
- Add opt-in resource/prompt/completion/logging, callback/MRTR, subscription and
  version-specific task wire boundaries; preserve application-owned execution.
- Add real TCP and independent Python interoperability checks, adversarial tests,
  transport benchmarks and documented scope/ownership requirements.
- Preserve default no_std, the unsafe-code ban, exact dependency closure, and the
  protected bump/CARGO_REGISTRY_TOKEN publication workflow.

## 0.2.0 — MCP client and zcall integration

- Add a caller-buffered MCP client with bounded pending slots, monotonic numeric
  request IDs, modern discovery and legacy 2025-11-25 / 2025-06-18 negotiation.
- Add paginated tools discovery, calls, error/notification separation, progress,
  cancellation and explicit abandonment without automatic side-effect retries.
- Depend directly on zcall 0.1.2, re-export its public contract, and bridge tool
  definitions, complete/streaming model calls and checked model history results.
- Add optional stdio subprocess transport and executable client/model examples.
- Preserve the entire MCP tool-result envelope; portable/native error-flag
  policies make provider-specific limitations explicit.
- Add adversarial client/bridge tests, real-process smoke tests and dependency
  closure auditing for zmcpio -> zcall -> zconv.
- Include safe modern discovery/list cache hints (ttlMs=0, private scope).

## 0.1.0 — initial release

- Initial independent no_std MCP tools protocol core, with zero dependencies.
- Strict bounded JSON/RPC parsing, modern discovery and legacy initialization.
- Borrowed tool actions, content/result encoders, cancellation/progress tracking.
- Caller-buffered stdio framing, examples, adversarial and protocol tests.
- Use the new `zmcpio` package name to avoid the unrelated `zmcp` registration.
