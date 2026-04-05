# Change: Add experimental in-process 1C panel instrumentation

## Why

If the custom Configurator tree remains opaque after out-of-process probing, behavioral mapping, and message-level interrogation, the only remaining path to richer semantics may be in-process instrumentation. This is high-risk, version-sensitive work and must not leak into the default product path. It needs an explicit experimental change with strong safety gates.

## What Changes

- Add an experimental, opt-in in-process instrumentation path for selected 1C Configurator panels.
- Require build/version fingerprint checks before any instrumentation attaches to `1cv8.exe`.
- Keep the in-process layer isolated from the normal daemon runtime and disabled by default.
- Return richer node/event evidence only when the companion/instrumentation layer is explicitly enabled and validated.

## Impact

- Affected specs: `onec-inprocess-panel-instrumentation`
- Affected code: likely new isolated crate/module plus minimal integration points in `crates/vmui-platform-windows` and `crates/vmui-agent`
- Affected docs: `docs/architecture.md`, `docs/dev-runbook.md`, `docs/windows-vm-access.md`
- External impact: additive experimental capability; must remain off by default
