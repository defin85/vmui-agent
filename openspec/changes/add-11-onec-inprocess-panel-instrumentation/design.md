## Context

In-process instrumentation is the most powerful and the riskiest layer in this plan. It should exist only as a last-resort capability after lower-risk stages fail to expose enough structure. The design must fail closed on unknown 1C builds and must keep the main daemon functional even when the experimental layer is unavailable.

## Goals / Non-Goals

- Goals:
  - Provide a gated path for richer panel introspection inside `1cv8.exe`.
  - Keep this layer isolated from the default daemon runtime.
  - Require version/build validation before attachment.
- Non-Goals:
  - Making injection the default observation path.
  - Hiding instrumentation failures behind fallback language.

## Decisions

- Decision: explicit opt-in gate.
  - Rationale: this layer changes the risk profile and must never activate implicitly.

- Decision: isolate instrumentation into a separate companion boundary.
  - Rationale: a crash or incompatibility in the experimental layer must not bring down the default observer path.

- Decision: build/version fingerprint checks are mandatory.
  - Rationale: 1C internal structures are not a stable public contract.

- Decision: fail closed on mismatch.
  - Rationale: unknown builds should produce "unsupported for this build" rather than best-effort undefined behavior.

## Risks / Trade-offs

- High maintenance cost across 1C versions and patches.
- Greater security and stability risk than all other levels.
- Potential interference with the live interactive session if implemented carelessly.

Mitigation: keep this stage explicitly experimental, isolated, and optional.
