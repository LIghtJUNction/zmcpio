# Changelog

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
