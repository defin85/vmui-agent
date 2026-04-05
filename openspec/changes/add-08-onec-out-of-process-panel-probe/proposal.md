# Change: Add out-of-process 1C panel probe

## Why

The left configuration tree in 1C Configurator is already navigable through live window actions, but the current observer does not expose semantic `TreeItem` nodes for that custom surface. We need a first reverse-engineering layer that collects evidence from the live panel without injecting code into `1cv8.exe`, restarting Configurator, or pretending that inferred structure is already authoritative.

## What Changes

- Add an out-of-process probe workflow for custom 1C Configurator panels, starting with the left configuration tree.
- Collect aligned HWND hierarchy, UIA tree views, MSAA/IAccessible traversal, coordinate hit-test data, and targeted region captures for one selected surface.
- Define a probe artifact bundle shape that external agents and later reverse-engineering stages can reuse.
- Add VM-backed acceptance coverage for attaching the probe to the live Configurator tree without disrupting the interactive session.

## Impact

- Affected specs: `onec-panel-probe`
- Affected code: `crates/vmui-platform-windows`, `crates/vmui-agent`, `crates/vmui-mcp-proxy`, `scripts/vm`
- Affected docs: `docs/architecture.md`, `docs/windows-vm-access.md`, `docs/dev-runbook.md`
- External impact: no public breaking change; additive diagnostics-only capability
