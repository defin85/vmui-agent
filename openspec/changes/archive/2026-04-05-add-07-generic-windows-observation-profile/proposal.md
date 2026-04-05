# Change: Add generic Windows observation profile

## Why

The current Windows backend already supports generic desktop actions such as focusing a window and sending keys to `Notepad`, but the authoritative read path still suppresses non-1C windows from daemon snapshots. This makes the product narrower than the runtime actually is, blocks generic desktop smoke workflows, and couples the public session contract too tightly to one application family.

The next change should make generic Windows observation first-class, keep 1C-specific semantics as an opt-in domain profile, and align daemon plus MCP session startup around a more expressive profile/scope/filter model.

## What Changes

- **BREAKING** replace the public `SessionMode`-only contract with an explicit session profile model that can represent generic desktop observation, app/window attach flows, and 1C-specific domain behavior independently.
- Make generic Windows window inventory the authoritative observation source instead of hard-filtering snapshots to 1C windows by default.
- Keep 1C classification, annotations, and diagnostics as opt-in enrichment layered on top of generic Windows observation.
- Align daemon and MCP session semantics around negotiated observation scope, domain profile, and optional target filters such as pid, process name, title, or class name.
- Add validation coverage for generic desktop smoke workflows such as `Notepad -> focus_window -> send_keys -> clipboard verify` while preserving existing 1C-oriented flows.

## Impact

- Affected specs: `windows-ui-observer`, `daemon-session-api`, `mcp-bridge`, `ui-state-cache`
- Affected code: `proto/vmui/v1/agent.proto`, `crates/vmui-protocol`, `crates/vmui-transport-grpc`, `crates/vmui-agent`, `crates/vmui-mcp-proxy`, `crates/vmui-platform-windows`
- Affected docs: `docs/architecture.md`, `docs/protocol.md`, `docs/dev-runbook.md`, `README.md`
- External impact: daemon and MCP clients must migrate from `enterprise_ui` / `configurator` mode negotiation to the new profile/scope/filter contract
