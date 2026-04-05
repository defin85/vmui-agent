# Change: Add message-level 1C panel probe

## Why

After out-of-process probing and behavioral mapping, the next question is whether the custom Configurator panel still exposes useful semantics through standard control messages or notification patterns. If it does, we can lift tree semantics without jumping straight to in-process instrumentation. If it does not, we need explicit evidence that the panel is opaque at the message layer as well.

## What Changes

- Add a message-level probe for selected 1C Configurator surfaces that first tests for standard control/message compatibility.
- Query standard tree/list style messages where a real child HWND supports them.
- Correlate safe navigation input with observed Windows events or message-level responses for the same surface.
- Return explicit unsupported results when the panel does not expose usable message-level introspection.

## Impact

- Affected specs: `onec-message-level-panel-probe`
- Affected code: `crates/vmui-platform-windows`, `crates/vmui-agent`, optional new probe module
- Affected docs: `docs/architecture.md`, `docs/dev-runbook.md`
- External impact: additive diagnostics-only capability; no public breaking change
