# Code Review Checklist

Use this checklist for `/review`, manual review, or acceptance checks in this repository.

## Scope And Architecture

- The change matches the user request or approved spec without silent scope creep.
- Crate boundaries still match `README.md`, `docs/architecture.md`, and root `AGENTS.md`.
- MCP-specific logic stays out of core daemon internals unless the approved change requires it.

## Contract And State Semantics

- Changes to `proto/vmui/v1/agent.proto` are mirrored in `crates/vmui-protocol` and `crates/vmui-transport-grpc`.
- Snapshot, diff, resync, and artifact semantics still match `docs/protocol.md` and OpenSpec specs.
- Session-stable ids and locators are preserved; no screenshot-first regressions were introduced.

## Platform Boundaries

- Windows-only APIs remain isolated behind `cfg(windows)` or in `crates/vmui-platform-windows`.
- Linux `cargo check --workspace` and `cargo test --workspace` remain valid after the change.
- UIA stays primary; WinEvent/MSAA stay hint or fallback paths; capture/OCR remains explicit.

## Tests And Verification

- The smallest relevant tests were added or updated when behavior changed.
- `cargo fmt --all --check`
- `cargo check --workspace`
- `cargo test --workspace`
- `openspec validate --strict --no-interactive` if `openspec/` changed

## Documentation Freshness

- `README.md`, `docs/`, and `openspec/specs/` reflect the new reality.
- Archived specs do not retain stale placeholders such as `Purpose: TBD`.
- New operational follow-up work is tracked in `bd`, not left as ad hoc TODO notes.
