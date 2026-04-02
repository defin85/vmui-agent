## Context

The daemon is intended to serve external agents, but those agents often speak tool protocols rather than native gRPC. The project also targets long-lived sessions, so reconnect and observability become part of the product rather than optional polish.

## Goals / Non-Goals

- Goals:
  - Add an MCP adapter without turning MCP into the internal source of truth.
  - Make long-running session behavior observable and recoverable.
  - Preserve the no-screenshot-polling contract for ordinary agent usage.
- Non-Goals:
  - Replacing the primary daemon transport with MCP.
  - Hiding backend degradation or resync events from clients.

## Decisions

- Decision: Keep MCP as a thin proxy over daemon sessions.
  - Alternatives considered:
    - Make the daemon itself speak MCP as the primary API.
  - Rationale: the repository architecture already treats MCP as an adapter, and the daemon needs a richer typed contract internally.
- Decision: Add explicit health and warning surfaces.
  - Alternatives considered:
    - Keep reconnect and fallback behavior implicit in logs only.
  - Rationale: higher-level agents need to know when state quality degraded or a resync occurred.

## Risks / Trade-offs

- MCP translation can duplicate transport model logic.
  - Mitigation: map MCP tools to the same action and snapshot semantics already defined by the daemon contract.
- Observability adds configuration and storage overhead.
  - Mitigation: keep the first metrics set small and focused on runtime quality signals.

## Migration Plan

1. Implement MCP session reuse on top of the daemon.
2. Add reconnect/resync handling.
3. Add retention and observability policies.

## Open Questions

- Whether the first observability surface should be metrics-only, structured logs only, or both.
