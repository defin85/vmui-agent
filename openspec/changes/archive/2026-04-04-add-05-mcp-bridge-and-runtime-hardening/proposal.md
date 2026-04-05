# Change: Add MCP bridge and runtime hardening

## Why

After the daemon supports live observation, actions, and 1C workflows, external agent tooling still needs a stable integration layer. The final implementation slice should add a thin MCP bridge and harden the runtime around reconnect, retention, and observability.

## What Changes

- Implement a thin `stdio`-first MCP bridge that translates tool-style requests onto explicit logical sessions backed by long-lived daemon sessions.
- Add reconnect, resync, and warning semantics needed for long-running usage, with explicit no-auto-retry safety for mutating actions.
- Add daemon-first artifact retention and runtime observability policies so operators can reason about stale state, fallback rate, and backend health.
- Preserve the contract that external agents consume stateful updates instead of full screenshot polling.

## Impact

- Depends on: `add-01-daemon-session-foundation`, `add-02-windows-ui-observer`, `add-03-semantic-actions-and-artifacts`, `add-04-onec-diagnostic-workflows`
- Affected specs: `mcp-bridge`, `daemon-runtime-hardening`
- Affected code: `crates/vmui-mcp-proxy`, `crates/vmui-agent`, `crates/vmui-core`, transport and configuration layers
