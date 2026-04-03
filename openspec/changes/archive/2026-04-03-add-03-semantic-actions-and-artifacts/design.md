## Context

The daemon must help operators and higher-level agents interact with the UI after it has observed it. Those actions need deterministic contracts and must preserve the stateful model instead of degrading into coordinate scripts by default.

## Goals / Non-Goals

- Goals:
  - Add semantic-first action execution.
  - Keep waits and artifacts server-side.
  - Make fallback use visible in results.
- Non-Goals:
  - 1C-specific diagnostic orchestration.
  - MCP tool translation.

## Decisions

- Decision: Prefer semantic patterns and cached locators before coordinate fallback.
  - Alternatives considered:
    - Always click by bounds center.
  - Rationale: the project explicitly aims for debugger-like state, not coordinate-only automation.
- Decision: Keep `wait_for` on the daemon side.
  - Alternatives considered:
    - Let clients poll snapshots or screenshots externally.
  - Rationale: server-side wait conditions reduce chatter and avoid screenshot polling behavior.

## Risks / Trade-offs

- Some controls will still require coordinate or OCR fallback.
  - Mitigation: report fallback provenance explicitly and keep fallback scoped to the requested target.
- Action postconditions can become flaky if tied only to timing.
  - Mitigation: couple actions with state-based waits and backend events.

## Migration Plan

1. Add read-only actions and wait semantics.
2. Add semantic write actions.
3. Layer explicit artifact capture and OCR fallback on top.

## Open Questions

- Whether OCR should start as an in-process plugin or an external helper process.
