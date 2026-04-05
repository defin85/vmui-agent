## Context

We already proved that the left Configurator panel can be focused, clicked, and driven by keyboard navigation. What we still lack is a trustworthy semantic description of that surface. Before considering injection or risky hooks, the system needs a non-invasive probe layer that answers a narrower question: what can we learn about this panel from outside the process boundary?

## Goals / Non-Goals

- Goals:
  - Attach to a live Configurator panel from the outside.
  - Collect multiple observation views for the same physical surface.
  - Keep outputs stable enough for later behavioral and message-level analysis.
- Non-Goals:
  - Inferring the logical tree structure.
  - Editing configuration data.
  - Injecting code into `1cv8.exe`.

## Decisions

- Decision: start with a dedicated out-of-process probe bundle rather than stuffing this into the normal session snapshot.
  - Rationale: the normal session snapshot remains the product path; the probe is research tooling with different output density and provenance.

- Decision: preserve source provenance per observation layer.
  - Layers:
    - HWND hierarchy
    - UIA raw/control/content view
    - MSAA/IAccessible object graph
    - point hit-test and bounds
    - targeted region capture
  - Rationale: reverse-engineering needs evidence, not one collapsed synthetic tree.

- Decision: target one selected surface at a time.
  - Rationale: the first useful unit is "the left Configurator panel", not "the whole desktop again".

## Proposed Runtime Shape

```text
live configurator window
        |
   probe target selection
        |
  +-----+-----+-----+-----+------+
  |           |     |     |      |
 HWND      UIA    MSAA  hit-test capture
  |           |     |     |      |
  +-----------+-----+-----+------+
              |
        probe artifact bundle
```

## Risks / Trade-offs

- Some custom 1C controls may expose almost nothing via UIA or MSAA.
  - Mitigation: return explicit "opaque at this layer" artifacts instead of pretending the surface is empty.
- Probe output can become noisy.
  - Mitigation: scope the probe to one target surface and keep source channels separate.
