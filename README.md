# vmui-agent

Rust workspace for a stateful Windows UI agent that runs inside a dedicated Windows 10 VM and supports 1C UI diagnostics, Configurator navigation, and post-failure investigation around standard 1C automated testing.

## Current Status

This repository is intentionally scaffolded for agent-driven development:

- workspace and crate boundaries are in place;
- the protocol/state model is defined up front;
- the daemon session foundation and gRPC transport are implemented;
- subscription always starts from an authoritative `initial_snapshot`;
- resync falls back to `snapshot_resync` plus refreshed snapshot instead of screenshot polling;
- action artifacts are persisted into the in-VM artifact store before clients read them back;
- Windows-specific automation is isolated behind a dedicated backend crate;
- the Windows backend now performs UIA-first window/tree reads and event-driven targeted refresh;
- WinEvent and MSAA are wired as refresh hints/fallback sources with explicit provenance in state;
- window and element identity now uses session-stable rebinding plus semantic locators instead of raw ordinal-only paths;
- MCP is planned as a thin proxy, not as the core transport;
- semantic actions and 1C-specific diagnostic workflows are still pending.

## Workspace Layout

- `crates/vmui-protocol`: shared domain and transport models.
- `crates/vmui-core`: config, session registry and UI state cache skeleton.
- `crates/vmui-platform`: backend trait used by the daemon.
- `crates/vmui-platform-windows`: Windows UI backend with interactive-session gating, UIA snapshot reads and WinEvent/MSAA refresh integration.
- `crates/vmui-transport-grpc`: generated protobuf/tonic types and conversion layer.
- `crates/vmui-agent`: in-VM daemon entrypoint.
- `crates/vmui-mcp-proxy`: external MCP adapter scaffold.
- `proto/vmui/v1/agent.proto`: canonical wire contract draft.
- `docs/architecture.md`: recommended runtime architecture.
- `docs/protocol.md`: API semantics and message flow.
- `docs/roadmap.md`: MVP to production rollout.

## Development Commands

- `cargo fmt --all`
- `cargo check --workspace`
- `cargo test --workspace`
- `just ci`

## Design Constraints

- The Windows automation process must run in the interactive VM session, not in Session 0.
- UIA is the primary backend, WinEvent/MSAA is secondary, OCR is fallback-only.
- The external agent should consume `initial_snapshot + diff stream + wait_for`, not full-screen polling.

## OpenSpec And Beads

- `openspec/` is the spec-first planning layer for capabilities, architecture changes and approved implementation plans.
- `.beads/` is the operational issue tracker for execution sequencing, discovered follow-ups and dependency tracking.
- Use OpenSpec when the request introduces a new capability, cross-cutting architecture shift, or other change that should be reviewed before coding.
- Use Beads for task-level execution after the scope is approved or when tracking follow-up work found during implementation.

Useful commands:

- `openspec list`
- `openspec list --specs`
- `openspec validate --strict --no-interactive`
- `bd ready --json`
- `bd status`

## Next Implementation Slice

1. Add semantic action execution beyond the current unsupported backend action path.
2. Add 1C-specific locator profiles and diagnostic workflows.
3. Add MCP bridge hardening and runtime policies.
