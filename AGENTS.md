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
- For review requests, also follow `code_review.md`.
- Expect more specific local guidance in `crates/vmui-agent/AGENTS.md`, `crates/vmui-platform-windows/AGENTS.md`, and `proto/AGENTS.md`.

## Agent Development Rules
- Prefer event-driven state and semantic actions over screenshot polling.
- Do not add screenshot-first flows when UIA/MSAA or cached state can answer the question.
- Keep Linux `cargo check --workspace` working even if a feature only runs on Windows.
- Windows-only dependencies must remain isolated behind `cfg(windows)` or the Windows backend crate.
- If you change crate boundaries, update the workspace map in `README.md`.

## Search Playbook
- Search order: `mcp__claude-context__search_code` -> `rg` -> `rg --files` -> targeted file reads.
- Use the canonical repo root `/home/egor/code/vmui-agent/` for semantic indexing tools.
- Formulate the first query as `component + action + context` and keep the first pass narrow (`limit: 6-10`).
- Set `extensionFilter` early when using semantic search:
  - Rust/workspace code: `.rs`
  - Wire contract and transport schema: `.proto`
  - Workspace/tooling config: `.toml`
  - Specs and architecture docs: `.md`
- Prioritize `crates/`, `proto/`, `docs/`, `openspec/`, `README.md`, and `AGENTS.md` when narrowing search scope.
- Confirm important implementation facts in at least two sources: code + tests/spec/docs.
- Do not treat plans, TODO lists, or status files as proof that behavior is implemented.
- For OpenSpec work, use `openspec` CLI discovery first and treat `rg` as full-text fallback per `openspec/AGENTS.md`.

## Verification
- Run `cargo fmt --all`.
- Run `cargo check --workspace`.
- Run `cargo test --workspace`.
- Run `openspec validate --strict --no-interactive` if you changed files under `openspec/`.
- If you touch CI or repo scaffolding, also inspect `.github/workflows/ci.yml`.

<!-- BEGIN BEADS INTEGRATION -->
## Issue Tracking with bd (beads)

This project uses **bd (beads)** for operational issue tracking and dependency management.

### Scope split

- Use **OpenSpec** for capability proposals, spec deltas, and approved implementation checklists.
- Use **bd** for executable work items, discovered follow-up tasks, and dependency tracking across sessions.
- OpenSpec markdown checklists inside `openspec/changes/*/tasks.md` are allowed and expected.

### Minimal workflow

```bash
bd ready --json
bd create "Issue title" --type task --priority 2 --json
bd update <issue-id> --status in_progress --json
bd close <issue-id> --reason "Done" --json
```

### Rules

- Prefer `bd ready --json` when looking for unblocked work.
- Use `--json` for programmatic or agent-driven invocations.
- Link discovered work with dependencies such as `discovered-from:<parent-id>` when applicable.
- Do not maintain a second operational task tracker outside `bd`.
- Do not treat this block as permission to push, sync, or modify remotes without an explicit user request.

For full local workflow details, run `bd prime`.

<!-- END BEADS INTEGRATION -->
