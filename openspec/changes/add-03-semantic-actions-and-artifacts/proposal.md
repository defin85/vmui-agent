# Change: Add semantic actions and artifact capture

## Why

Once live observation exists, the daemon still cannot act on the UI or emit useful diagnostics. The next implementation slice must add semantic action execution and explicit artifact capture semantics without falling back to screenshot-first automation.

## What Changes

- Implement the first semantic action executor for window focus, tree reads, invoke, value setting, key input, and wait conditions.
- Define server-side `wait_for` behavior that operates on the live cache plus backend events.
- Add artifact capture primitives for structured dumps, screenshots, and OCR fallback regions.
- Standardize action result semantics so later 1C-specific diagnostic workflows can compose on top of them.

## Impact

- Depends on: `add-01-daemon-session-foundation`, `add-02-windows-ui-observer`
- Affected specs: `semantic-action-execution`, `artifact-capture`
- Affected code: `crates/vmui-agent`, `crates/vmui-platform`, `crates/vmui-platform-windows`, `crates/vmui-core`, `crates/vmui-protocol`
