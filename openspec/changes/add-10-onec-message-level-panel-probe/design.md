## Context

Some custom-looking Windows panels still wrap a standard child control or answer enough standard messages to recover structural semantics. Others remain opaque. We need a middle layer between purely behavioral inference and risky in-process instrumentation.

## Goals / Non-Goals

- Goals:
  - Detect whether a selected panel or child HWND speaks standard tree/list messages.
  - Correlate navigation input with standard control responses or WinEvent/notification output.
  - Fail explicitly when no message-level introspection is available.
- Non-Goals:
  - Injecting hooks into `1cv8.exe`.
  - Claiming custom-message semantics without evidence.

## Decisions

- Decision: standard-control compatibility comes first.
  - Candidate families:
    - tree-view
    - list-view
    - owner wrappers with child HWNDs
  - Rationale: if a standard control is present, this is the safest path to more semantics.

- Decision: keep message-level results separate from behavioral inference.
  - Rationale: they are different evidence classes and should not be blended implicitly.

- Decision: unsupported is a valid outcome.
  - Rationale: this stage is diagnostic; "opaque at message layer" is useful evidence for deciding whether level 4 is justified.

## Risks / Trade-offs

- Message probing can become Windows-version-sensitive.
  - Mitigation: keep standard-control checks narrow and version-aware.
- Some useful notifications may still be inaccessible without in-process hooks.
  - Mitigation: record the absence of usable message-level data and stop there.
