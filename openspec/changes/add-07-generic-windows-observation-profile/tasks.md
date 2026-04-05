## 1. Public contract redesign

- [x] 1.1 Replace the public `SessionMode` contract in `proto/vmui/v1/agent.proto`, `crates/vmui-protocol`, and `crates/vmui-transport-grpc` with a structured session profile model.
- [x] 1.2 Define negotiation semantics for generic desktop observation, attach-filtered observation, and 1C-specific domain profiles.
- [x] 1.3 Update protocol and architecture docs to describe the breaking migration and new session profile semantics.

## 2. Generic Windows observation

- [x] 2.1 Remove 1C-only filtering from the authoritative Windows snapshot pipeline so generic visible desktop windows can enter daemon state.
- [x] 2.2 Move 1C classification, narrowing, and `onec_*` annotations into an opt-in enrichment/projection layer.
- [x] 2.3 Keep UIA-first, fallback provenance, and event-driven refresh behavior intact after the generic observation shift.

## 3. Session views and MCP integration

- [x] 3.1 Teach daemon session startup to accept observation scope, domain profile, and optional target filters instead of mode-only inputs.
- [x] 3.2 Update MCP `session_open` and related session semantics to use the new profile/filter model.
- [x] 3.3 Add explicit attach workflows by pid, process name, title, or class name for generic desktop apps.
- [x] 3.4 Preserve reconnect safety and no-silent-retry semantics for mutating actions under the new session model.

## 4. Cache and identity behavior

- [x] 4.1 Ensure the daemon keeps a generic authoritative inventory and derives per-session filtered views from it.
- [x] 4.2 Preserve session-stable ids, locators, and revision semantics inside each session view.
- [x] 4.3 Add explicit resync or continuity invalidation behavior anywhere profile/filter changes would make incremental state unsafe.

## 5. Verification

- [x] 5.1 Add protocol and transport tests for the new session profile negotiation.
- [x] 5.2 Add Windows backend tests for generic desktop capture plus 1C-specific enrichment/projection.
- [x] 5.3 Add remote-VM smoke coverage for `Notepad -> focus_window -> send_keys -> clipboard verify`.
- [x] 5.4 Re-run existing 1C observer, daemon, and MCP regression coverage.
- [x] 5.5 Run `cargo fmt --all --check`.
- [x] 5.6 Run `cargo check --workspace`.
- [x] 5.7 Run `cargo test --workspace`.
- [x] 5.8 Run `openspec validate add-07-generic-windows-observation-profile --strict --no-interactive`.
