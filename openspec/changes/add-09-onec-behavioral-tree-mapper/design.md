## Context

The live Configurator tree already reacts to coordinate click plus `SendInput`, and targeted captures can confirm that selection moved. This makes the panel suitable for state inference even when UIA/MSAA do not expose semantic tree items. The mapper should remain explicit that its output is inferred from behavior, not read directly from the control.

## Goals / Non-Goals

- Goals:
  - Infer which panel item is selected or expanded after controlled inputs.
  - Keep experiments reversible and low-risk.
  - Reuse exported configuration trees as optional semantic priors.
- Non-Goals:
  - Claiming permanent authoritative truth for inferred nodes.
  - Editing configuration data through behavioral probing.
  - Replacing explicit accessibility evidence.

## Decisions

- Decision: use safe reversible navigation primitives only.
  - Inputs:
    - click in panel
    - `Up`
    - `Down`
    - `Left`
    - `Right`
    - optional `Home` / `End`
  - Rationale: these are sufficient to map the tree while minimizing unintended edits.

- Decision: keep inferred ids session-local and confidence-scored.
  - Rationale: the mapper does not yet have object-level proof; it should expose confidence, ambiguity, and evidence references.

- Decision: support semantic overlay from exported repositories such as `src/cf`.
  - Rationale: the exported tree gives strong priors for expected top-level categories and descendant counts, which improves navigation planning without pretending to describe unsaved live editor state.

## Risks / Trade-offs

- Replaying navigation can disturb the operator's current selection.
  - Mitigation: use reversible sequences and record return steps.
- Semantic overlays can mislead inference when the live configuration differs from the exported tree.
  - Mitigation: keep overlays optional and record mismatch/ambiguity explicitly.
