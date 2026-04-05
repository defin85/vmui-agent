## 1. MCP bridge

- [x] 1.1 Implement a `stdio`-first MCP proxy in `crates/vmui-mcp-proxy` using an explicit logical session lifecycle (`session_open`, `session_status`, `session_close`).
- [x] 1.2 Add a per-session `DaemonSessionWorker` that owns one daemon stream, caches snapshot state, and reuses daemon sessions across related MCP calls.
- [x] 1.3 Map MCP read/write tools onto daemon actions and cached state without mandatory screenshot polling or duplicated UI logic in the proxy.
- [x] 1.4 Ensure reconnect behavior is explicit and that mutating operations are never silently retried after session loss.
- [x] 1.5 Add integration tests or contract checks for MCP request translation, session reuse, reconnect/error handling, and single-session ambiguity rules.

## 2. Runtime hardening

- [x] 2.1 Extend daemon runtime state/config with artifact retention settings and implement startup plus periodic cleanup with consistent metadata updates.
- [x] 2.2 Add daemon-first runtime status and warning surfaces for resyncs, backend degradation, artifact store pressure, and health reporting.
- [x] 2.3 Add aggregated observability for fallback-heavy behavior, stale-state resync frequency, and action outcome distribution without high-cardinality identifiers.
- [x] 2.4 Define and test reconnect/resync semantics for long-running sessions and backend restarts, including state invalidation boundaries.

## 3. Validation

- [x] 3.1 Run `cargo fmt --all`.
- [x] 3.2 Run `cargo check --workspace`.
- [x] 3.3 Run `cargo test --workspace`.
- [x] 3.4 Run `openspec validate add-05-mcp-bridge-and-runtime-hardening --strict --no-interactive`.
