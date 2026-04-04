## Context

The repository is justified by 1C-specific workflows. Generic Windows automation features alone do not solve the target problem of Configurator navigation and fast post-failure diagnostics after standard 1C tests.

## Goals / Non-Goals

- Goals:
  - Add explicit 1C operating modes.
  - Make post-failure diagnostics a first-class workflow.
  - Keep cooperation with standard 1C testing explicit.
- Non-Goals:
  - Replace standard 1C testing infrastructure.
  - Promise full semantic coverage for every Configurator surface.

## Decisions

- Decision: Separate `enterprise_ui` and `configurator` modes at the daemon level.
  - Alternatives considered:
    - Infer mode implicitly from whatever window is currently active.
  - Rationale: the user wants explicit handling for different 1C contexts, and later tooling needs predictable targeting.
- Decision: Treat post-failure diagnostics as a guided workflow, not a single screenshot dump.
  - Alternatives considered:
    - Capture only a screenshot on failure.
  - Rationale: the project is intended to provide state, element trees, and diffs near a debugger model.

## Risks / Trade-offs

- Configurator surfaces may expose poor accessibility metadata.
  - Mitigation: keep locator profiles and fallback annotations explicit, and do not overstate semantic coverage.
- Baselines can drift as the UI changes.
  - Mitigation: keep baseline comparison at the diagnostic layer and tie it to explicit workflow contexts.

## Migration Plan

1. Introduce 1C-specific daemon modes.
2. Add locator profiles and fallback annotations.
3. Implement the post-failure diagnostic bundle and reporting flow.

## Open Questions

- Which Configurator surfaces should be covered by built-in profiles first after the initial rollout.
