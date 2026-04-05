# vmui-agent

Rust workspace for a stateful Windows UI agent that runs inside a dedicated Windows 10 VM and supports 1C UI diagnostics, Configurator navigation, and post-failure investigation around standard 1C automated testing.

Start with:

- `docs/index.md` for the repository map and edit routing.
- `docs/dev-runbook.md` for run, verification, and Codex-specific setup notes.
- `docs/codex-workflow.md` for the canonical search, verification, OpenSpec, and tracking workflow.
- `docs/codex-setup.md` for local tool readiness and optional Codex config details.

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
- semantic action execution is implemented in the daemon and Windows backend;
- generic Windows desktop observation is now the canonical read path, with explicit session profiles for generic desktop, attach-filtered sessions, 1C Enterprise UI, and 1C Configurator;
- post-failure 1C diagnostic bundles now capture current state, recent diffs, baseline comparison, and targeted artifacts while preserving the original external test verdict;
- daemon runtime status now exposes structured health, resync, warning, fallback, action-outcome, and artifact-retention summaries;
- artifact retention is now explicit, with startup orphan sweep plus periodic cleanup;
- `vmui-mcp-proxy` now provides a `stdio` MCP bridge with logical sessions, daemon session reuse, read-only reconnect, and no silent retry for mutating actions.

## Workspace Layout

- `crates/vmui-protocol`: shared domain and transport models.
- `crates/vmui-core`: config, session registry and UI state cache skeleton.
- `crates/vmui-platform`: backend trait used by the daemon.
- `crates/vmui-platform-windows`: Windows UI backend with interactive-session gating, UIA snapshot reads and WinEvent/MSAA refresh integration.
- `crates/vmui-transport-grpc`: generated protobuf/tonic types and conversion layer.
- `crates/vmui-agent`: in-VM daemon entrypoint.
- `crates/vmui-mcp-proxy`: external MCP adapter over daemon sessions.
- `proto/vmui/v1/agent.proto`: canonical wire contract.
- `docs/architecture.md`: recommended runtime architecture.
- `docs/protocol.md`: API semantics and message flow.
- `docs/roadmap.md`: MVP to production rollout.
- `docs/index.md`: onboarding index for agents and contributors.
- `docs/dev-runbook.md`: run and verification workflow.

## Development Commands

- `./scripts/doctor.sh`
- `./scripts/check-agent-docs.sh`
- `cargo fmt --all --check`
- `cargo check --workspace`
- `cargo test --workspace`
- `just ci` if `just` is installed locally
- `just doctor` if `just` is installed locally

## Design Constraints

- The Windows automation process must run in the interactive VM session, not in Session 0.
- UIA is the primary backend, WinEvent/MSAA is secondary, OCR is fallback-only.
- The external agent should consume `initial_snapshot + diff stream + wait_for`, not full-screen polling.
- Session startup must negotiate a structured observation profile, not a legacy mode enum.

## OpenSpec And Beads

- `openspec/` is the spec-first planning layer for capabilities, architecture changes and approved implementation plans.
- `.beads/` is the operational issue tracker for execution sequencing, discovered follow-ups and dependency tracking.
- Use OpenSpec when the request introduces a new capability, cross-cutting architecture shift, or other change that should be reviewed before coding.
- Use Beads for task-level execution after the scope is approved or when tracking follow-up work found during implementation.

Useful commands:

- `openspec list`
- `openspec list --specs`
- `openspec validate --all --strict --no-interactive`
- `openspec validate <change-id> --strict --no-interactive`
- `bd ready --json`
- `bd status`

## Current Planned Work

- Active capability work lives under `openspec/changes/`.
- Use `openspec list` for the current approved change queue.
- Use `bd ready --json` for the current executable task queue.
