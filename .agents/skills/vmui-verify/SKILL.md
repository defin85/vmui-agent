---
name: vmui-verify
description: Run the canonical verification flow for vmui-agent after documentation, protocol, runtime, or platform changes. Use when the task is done and you need the repo-standard proof.
---

# vmui Verify

## Use When

- You changed code, docs with runnable implications, AGENTS files, or OpenSpec artifacts.
- You need the repository-standard verification sequence.

## Workflow

1. Run `cargo fmt --all --check`.
2. Run `cargo check --workspace`.
3. Run `cargo test --workspace`.
4. If `openspec/` changed, run `openspec validate --strict --no-interactive`.
5. Report exact command results and any skipped steps.
