## 1. MCP bridge

- [ ] 1.1 Implement the first MCP proxy that maps stateless tool invocations onto one or more long-lived daemon sessions.
- [ ] 1.2 Ensure MCP-facing tools reuse daemon state and avoid mandatory screenshot polling between steps.
- [ ] 1.3 Add tests or contract checks for MCP request-to-action translation and session reuse.

## 2. Runtime hardening

- [ ] 2.1 Add reconnect and resync handling for long-running sessions and backend restarts.
- [ ] 2.2 Add artifact retention policy, warning surfaces, and backend health reporting.
- [ ] 2.3 Add observability for key signals such as fallback rate, stale state resync, and action outcome distribution.

## 3. Validation

- [ ] 3.1 Run `cargo fmt --all`.
- [ ] 3.2 Run `cargo check --workspace`.
- [ ] 3.3 Run `cargo test --workspace`.
