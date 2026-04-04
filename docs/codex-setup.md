# Codex Setup

## First Run

- Run `./scripts/doctor.sh` from the repository root.
- If `just` is installed locally, `just doctor` runs the same readiness check.
- Treat a failing doctor run as an environment problem first, not as evidence that the repository is broken.

## Required Local Tools

- Required:
  `cargo`
  `openspec`
  `bd`
  `rg`
- Optional:
  `just`

## Optional Codex Config

- `.codex/config.toml` is an optional local optimization layer, not a required part of the repository runtime.
- The checked-in config assumes local `claude-context`, Ollama, and Milvus services.
- The current `claude-context` entry also assumes a local Node 20 or Node 22 runtime.
- If that stack is unavailable, disable or replace the MCP entry instead of treating the repository as broken.

## Expected Ready State

- `./scripts/doctor.sh` exits successfully.
- The repo-standard verification flow from `docs/codex-workflow.md` is runnable on the current machine.
- Linux hosts are expected to pass `cargo check --workspace` and `cargo test --workspace`.
