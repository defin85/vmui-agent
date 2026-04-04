# Docs Index

## Start Here

- `README.md`: short product summary and high-level repository status.
- `docs/dev-runbook.md`: canonical run, verification, and Codex setup notes.
- `docs/architecture.md`: runtime shape, crate boundaries, and fallback model.
- `docs/protocol.md`: session flow, action semantics, and artifact policy.
- `docs/roadmap.md`: delivery phases and release gates.
- `openspec/project.md`: project conventions and domain constraints for spec work.

## Task Routing

- Protocol or wire-contract changes:
  `proto/vmui/v1/agent.proto` -> `crates/vmui-protocol/src/lib.rs` -> `crates/vmui-transport-grpc/src/lib.rs`
- Daemon session flow, server-side actions, artifact persistence:
  `crates/vmui-agent/src/lib.rs`
- Daemon integration tests and session/action regression coverage:
  `crates/vmui-agent/src/tests.rs`
- Runtime defaults such as bind address, artifact dir, and default mode:
  `crates/vmui-core/src/lib.rs`
- Platform abstraction:
  `crates/vmui-platform/src/lib.rs`
- Windows UIA, WinEvent, MSAA, SendInput, and capture behavior:
  `crates/vmui-platform-windows/src/lib.rs` -> `crates/vmui-platform-windows/src/windows_impl.rs`
- Windows backend tests and refresh/stabilization checks:
  `crates/vmui-platform-windows/src/tests.rs`
- External MCP bridge scaffold:
  `crates/vmui-mcp-proxy/src/main.rs`

## Verification

- Fast path:
  `cargo fmt --all --check`
  `cargo check --workspace`
  `cargo test --workspace`
- Optional shortcut:
  `just ci`
- If you changed `openspec/`:
  `openspec validate --strict --no-interactive`

## Planning And Tracking

- Approved capability changes live in `openspec/changes/`.
- Current capability list:
  `openspec list --specs`
- Active approved changes:
  `openspec list`
- Operational execution queue:
  `bd ready --json`

## Codex-Specific Files

- Root instructions:
  `AGENTS.md`
- Layer-specific instructions:
  `crates/vmui-agent/AGENTS.md`
  `crates/vmui-platform-windows/AGENTS.md`
  `proto/AGENTS.md`
- Review checklist:
  `code_review.md`
- Shared repo skills:
  `.agents/skills/`
- Optional project-local Codex config:
  `.codex/config.toml`
