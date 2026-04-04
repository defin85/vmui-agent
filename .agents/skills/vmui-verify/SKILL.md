---
name: vmui-verify
description: Run the canonical verification flow for vmui-agent after documentation, protocol, runtime, or platform changes. Use when the task is done and you need the repo-standard proof.
---

# vmui Verify

## Use When

- You changed code, docs with runnable implications, AGENTS files, or OpenSpec artifacts.
- You need the repository-standard verification sequence.

## Workflow

1. Run `./scripts/check-agent-docs.sh`.
2. Run `cargo fmt --all --check`.
3. Run `cargo check --workspace`.
4. Run `cargo test --workspace`.
5. If `openspec/` changed, run `openspec validate --all --strict --no-interactive`, or `openspec validate <change-id> --strict --no-interactive` when validating one change in isolation.
6. Report exact command results and any skipped steps.
