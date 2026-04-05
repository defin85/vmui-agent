<!-- OPENSPEC:START -->
# OpenSpec Instructions

These instructions are for AI assistants working in this project.

Always open `@/openspec/AGENTS.md` when the request:
- Mentions planning or proposals (words like proposal, spec, change, plan)
- Introduces new capabilities, breaking changes, architecture shifts, or big performance/security work
- Sounds ambiguous and you need the authoritative spec before coding

Use `@/openspec/AGENTS.md` to learn:
- How to create and apply change proposals
- Spec format and conventions
- Project structure and guidelines

Keep this managed block so 'openspec update' can refresh the instructions.

<!-- OPENSPEC:END -->

# Repo Instructions

## Purpose
- This repository hosts a Rust workspace for a Windows UI agent that runs inside an interactive Windows 10 VM session and automates 1C UI without touching the host desktop.
- The primary product is a long-lived stateful agent, not a screenshot bot.

## Architecture Boundaries
- Keep transport-agnostic UI/domain models in `crates/vmui-protocol`.
- Keep state/cache/session orchestration in `crates/vmui-core`.
- Keep platform traits in `crates/vmui-platform`.
- Keep Windows-specific UI Automation, WinEvent, MSAA, input and capture code in `crates/vmui-platform-windows`.
- Keep protobuf generation and gRPC/domain conversion logic in `crates/vmui-transport-grpc`.
- Keep the in-VM daemon entrypoint in `crates/vmui-agent`.
- Keep MCP translation separate in `crates/vmui-mcp-proxy`.

## Source Of Truth
- The canonical wire contract lives in `proto/vmui/v1/agent.proto`.
- When changing state, diff or action semantics, update both `proto/vmui/v1/agent.proto` and the matching Rust models in `crates/vmui-protocol`.
- Architecture decisions belong in `docs/architecture.md`, protocol semantics in `docs/protocol.md`, and delivery slices in `docs/roadmap.md`.

## Start Here
- Read `docs/index.md` first for the repository map, task routing, and related docs.
- Read `docs/dev-runbook.md` before changing startup flow, verification commands, or Codex-specific setup.
- Read `docs/windows-vm-access.md` before remote deploy/test or Windows VM bootstrap work.
- Read `docs/codex-workflow.md` for the canonical search, verification, OpenSpec, and Beads workflow.
- Read `docs/codex-setup.md` and run `./scripts/doctor.sh` on a new machine before assuming local tool breakage.
- For review requests, also follow `code_review.md`.
- Expect more specific local guidance in `crates/vmui-agent/AGENTS.md`, `crates/vmui-core/AGENTS.md`, `crates/vmui-platform-windows/AGENTS.md`, `crates/vmui-transport-grpc/AGENTS.md`, and `proto/AGENTS.md`.

## Remote Windows VM
- Current remote test VM recorded on 2026-04-05:
  `192.168.32.142`
- For remote deploy/test work, treat `docs/windows-vm-access.md` as the control-plane source of truth.
- Current VM bootstrap state:
  SSH key access, PowerShell 7, `C:\vmui-agent`, `vmui-agent-session`, and `vmui-smoke` are already configured.
- Keep UI automation and `vmui-agent` inside the interactive Windows session; do not assume an SSH or service session can touch the desktop.
- Prefer a loopback-bound daemon plus SSH tunnel and local `vmui-mcp-proxy`; do not expose the daemon on the LAN by default.

## Agent Development Rules
- Prefer event-driven state and semantic actions over screenshot polling.
- Do not add screenshot-first flows when UIA/MSAA or cached state can answer the question.
- Keep Linux `cargo check --workspace` working even if a feature only runs on Windows.
- Windows-only dependencies must remain isolated behind `cfg(windows)` or the Windows backend crate.
- If you change crate boundaries, update the workspace map in `README.md`.

## Codex Workflow
- Use `docs/codex-workflow.md` for the canonical search order, verification commands, OpenSpec discovery, and Beads tracking flow.
- Use the canonical repo root `/home/egor/code/vmui-agent/` for semantic indexing tools.
- Confirm important implementation facts in at least two sources: code + tests/spec/docs.
- Do not treat plans, TODO lists, or status files as proof that behavior is implemented.

## Verification
- Run `./scripts/check-agent-docs.sh`.
- Run `cargo fmt --all --check`.
- Run `cargo check --workspace`.
- Run `cargo test --workspace`.
- Run `openspec validate --all --strict --no-interactive` if you changed files under `openspec/`, or `openspec validate <change-id> --strict --no-interactive` when validating one change in isolation.
- If you touch CI or repo scaffolding, also inspect `.github/workflows/ci.yml`.
