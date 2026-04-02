## Context

The backend must produce live state without falling back to screenshot polling. It also must survive the reality that 1C and Configurator may expose partial or inconsistent accessibility metadata.

## Goals / Non-Goals

- Goals:
  - Implement one Windows backend that observes windows, element trees, properties, and events.
  - Keep UIA primary and fallback sources explicit.
  - Preserve the daemon contract established by the foundation change.
- Non-Goals:
  - Full semantic action execution.
  - 1C-specific locator profiles and post-failure workflows.

## Decisions

- Decision: Use UIA as the primary tree source and WinEvent/MSAA as event and fallback sources.
  - Alternatives considered:
    - Build on MSAA first and retrofit UIA later.
    - Depend on screenshots plus OCR from the start.
  - Rationale: UIA gives the richest semantic model, while MSAA and WinEvent improve coverage where UIA is weak.
- Decision: Treat backend events as hints, not authoritative state.
  - Alternatives considered:
    - Update cache directly from raw events without targeted refresh.
  - Rationale: backend event fidelity is inconsistent, especially on custom controls.

## Risks / Trade-offs

- COM, UIA, and hook handling introduce `unsafe` and threading complexity.
  - Mitigation: isolate the observer implementation in the Windows backend crate and keep async/runtime boundaries explicit.
- Some controls may remain opaque.
  - Mitigation: surface provenance and confidence so later layers can apply fallback strategies intentionally.

## Migration Plan

1. Add the observer thread and basic capability detection.
2. Implement UIA snapshot reads.
3. Layer WinEvent/MSAA-triggered targeted refresh on top of the snapshot path.

## Open Questions

- Whether event batching should happen fully inside the backend crate or in `vmui-core`.
