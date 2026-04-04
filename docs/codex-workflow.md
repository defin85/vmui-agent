# Codex Workflow

## Start Here

- Read `docs/index.md` for the repository map and task routing.
- Read `docs/dev-runbook.md` for runtime behavior, verification commands, and host expectations.
- Read `docs/codex-setup.md` and run `./scripts/doctor.sh` on a new machine.
- For review requests, use `code_review.md`.

## Search Playbook

- Search order: `mcp__claude-context__search_code` -> `rg` -> `rg --files` -> targeted file reads.
- Use the canonical repo root `/home/egor/code/vmui-agent/` for semantic indexing tools.
- Formulate the first semantic search query as `component + action + context` and keep the first pass narrow (`limit: 6-10`).
- Set `extensionFilter` early when using semantic search:
  - Rust/workspace code: `.rs`
  - Wire contract and transport schema: `.proto`
  - Workspace/tooling config: `.toml`
  - Specs and architecture docs: `.md`
- Prioritize `crates/`, `proto/`, `docs/`, `openspec/`, `README.md`, and `AGENTS.md` when narrowing search scope.
- For OpenSpec work, use `openspec` CLI discovery first and treat `rg` as full-text fallback per `openspec/AGENTS.md`.
- Confirm important implementation facts in at least two sources: code + tests/spec/docs.
- Do not treat plans, TODO lists, or status files as proof that behavior is implemented.

## Verification

- Run the smallest relevant check first, then the repo-standard flow:
  `./scripts/check-agent-docs.sh`
  `cargo fmt --all --check`
  `cargo check --workspace`
  `cargo test --workspace`
- If you changed `openspec/`, run:
  `openspec validate --all --strict --no-interactive`
  or `openspec validate <change-id> --strict --no-interactive` when validating one change in isolation.
- If you touched CI or repo scaffolding, inspect `.github/workflows/ci.yml`.

## Planning And Tracking

- Approved capability changes live in `openspec/changes/`.
- Enumerate capabilities with `openspec list --specs`.
- Enumerate active approved changes with `openspec list`.
- Use `bd ready --json` for the current executable queue.
- Link discovered follow-up work in `bd`; do not maintain a second operational task tracker outside `bd`.
- OpenSpec markdown checklists inside `openspec/changes/*/tasks.md` are allowed, but they do not replace `bd` for operational tracking.

## Codex-Specific Files

- Root instructions:
  `AGENTS.md`
- Layer-specific instructions:
  `crates/vmui-agent/AGENTS.md`
  `crates/vmui-core/AGENTS.md`
  `crates/vmui-platform-windows/AGENTS.md`
  `crates/vmui-transport-grpc/AGENTS.md`
  `proto/AGENTS.md`
- Review checklist:
  `code_review.md`
- Shared repo skills:
  `.agents/skills/`
- Optional local Codex config:
  `.codex/config.toml`
- Local readiness helper:
  `scripts/doctor.sh`
- Agent-facing docs guard:
  `scripts/check-agent-docs.sh`
