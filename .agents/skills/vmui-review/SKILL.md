---
name: vmui-review
description: Review vmui-agent changes against repository-specific architectural boundaries, protocol rules, and verification expectations. Use when asked for a repo-aware code review or acceptance check.
---

# vmui Review

## Use When

- The user asks for a review, acceptance check, or regression scan in this repository.

## Core Checks

- Crate boundaries still follow `README.md`, `docs/architecture.md`, and `AGENTS.md`.
- No screenshot-first regression was introduced where cache, UIA, or fallback hints should suffice.
- Proto, domain, and transport layers stay aligned.
- Windows-only code stays isolated from Linux-host validation paths.
- README, docs, and OpenSpec specs were updated when behavior changed.

## Verification

- Use `code_review.md` as the repository checklist.
- Confirm the relevant verification commands were run, or say explicitly when they were not.
