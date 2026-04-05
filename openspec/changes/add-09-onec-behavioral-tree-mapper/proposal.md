# Change: Add behavioral 1C tree mapper

## Why

Out-of-process probing can tell us where the custom Configurator tree lives and what evidence each accessibility layer exposes, but it does not tell us which logical node is currently selected. Since the panel already accepts click and keyboard navigation, the next safe step is to infer tree state from controlled input sequences and before/after observations instead of pretending UIA already exposes `TreeItem` semantics.

## What Changes

- Add a behavioral tree mapper that drives the selected Configurator panel with safe, reversible navigation inputs such as click, `Up`, `Down`, `Left`, and `Right`.
- Infer session-local selection and expansion state from before/after captures plus observer evidence.
- Support optional semantic overlay from an exported `src/cf` repository tree so the mapper can narrow likely logical categories and object paths.
- Keep all inferred state explicitly marked as inferred, confidence-scored, and session-local.

## Impact

- Affected specs: `onec-behavioral-tree-mapper`
- Affected code: `crates/vmui-platform-windows`, `crates/vmui-agent`, `crates/vmui-mcp-proxy`, optional new inference module/crate
- Affected docs: `docs/architecture.md`, `docs/dev-runbook.md`
- External impact: additive diagnostics and navigation capability; no public breaking change
