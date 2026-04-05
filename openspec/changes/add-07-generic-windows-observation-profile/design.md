## Context

The repository started as a 1C-focused Windows UI agent, and that product direction remains valid. However, the current implementation now has an asymmetry: the action executor can already target arbitrary visible desktop windows, but the authoritative snapshot path is still filtered through 1C-specific heuristics before state reaches daemon clients. As a result, external clients cannot reliably discover or inspect non-1C windows even though the runtime can act on them.

The system needs a cleaner split between:

- what part of the Windows desktop is observed;
- what domain-specific interpretation is applied to that observation;
- what subset of windows the client wants to attach to initially.

## Goals / Non-Goals

- Goals:
  - Make generic Windows desktop observation a first-class capability.
  - Keep 1C-specific semantics as a strong, opt-in domain profile rather than a hard-coded global filter.
  - Replace the overloaded `SessionMode` concept with a more expressive public session configuration model.
  - Keep daemon state, MCP sessions, and backend actions aligned around the same targeting model.
  - Preserve the current strengths around event-driven state, stable locators, and explicit fallback provenance.
- Non-Goals:
  - Turning the product into a generic cross-platform automation framework.
  - Removing 1C-specific diagnostics or reducing them to a secondary afterthought.
  - Replacing the daemon protocol with WebDriver, Appium, or WinAppDriver semantics.
  - Expanding screenshot/OCR into the default read path.

## Decisions

- Decision: Make generic desktop inventory the authoritative source of observed windows.
  - Alternatives considered:
    - Keep the current 1C-only filter in the authoritative snapshot.
    - Add one more `SessionMode` enum variant such as `generic_desktop`.
  - Rationale: the authoritative cache should reflect what the Windows backend can truly observe. A single enum variant is not expressive enough because it conflates desktop scope, domain semantics, and attach strategy.

- Decision: Replace `SessionMode` with an explicit session profile model.
  - Proposed conceptual shape:
    - `observation_scope`
      - `desktop`
      - `attached_windows`
    - `domain_profile`
      - `generic`
      - `onec_enterprise_ui`
      - `onec_configurator`
    - `target_filter`
      - optional `pid`
      - optional `process_name`
      - optional `title`
      - optional `class_name`
  - Alternatives considered:
    - Keep `SessionMode` and bolt on extra flags ad hoc.
    - Store attach filters only in MCP and keep the daemon unaware of them.
  - Rationale: scope, domain, and attach filter are independent concerns. The daemon should understand all three so that its cache, snapshots, and actions stay coherent.

- Decision: Make 1C detection and annotation a profile-specific enrichment layer.
  - Alternatives considered:
    - Keep 1C heuristics inside the core observation pipeline.
    - Duplicate the observer into separate generic and 1C backends.
  - Rationale: 1C-specific metadata such as `onec_window_profile`, `onec_profile`, and `onec_fallback_reason` is still valuable, but it should be applied after generic observation identifies windows and trees.

- Decision: Keep read and action targeting aligned.
  - Alternatives considered:
    - Allow actions to stay generic while reads remain 1C-filtered.
    - Add a second generic read path only for smoke testing.
  - Rationale: a session should not be able to mutate a target that it cannot observe and address through the same negotiated session view. The read path and action path need the same profile/filter model.

- Decision: Treat this as an explicit breaking change.
  - Alternatives considered:
    - Keep legacy `SessionMode` fields indefinitely.
    - Ship two parallel public contracts.
  - Rationale: the current public model is structurally too small. Carrying both contracts for too long would complicate transport, MCP, docs, and test coverage. A short-lived internal compatibility shim is acceptable during rollout, but the target public contract should be the new profile model.

## Proposed Runtime Shape

```text
client / MCP caller
        |
        +--> session profile
              - observation_scope
              - domain_profile
              - target_filter
        |
      vmui-agent session
        |
        +--> generic desktop inventory
        |     - visible windows
        |     - UIA-first trees
        |     - fallback provenance
        |
        +--> profile projection
              - generic view
              - 1C enterprise view
              - 1C configurator view
              - attach-filtered subset
```

## Component Design

### Wire and domain model

- Replace the public `SessionMode` enum with a structured session profile payload in proto, domain types, and transport conversion.
- Return the negotiated session profile in `hello_ack` and snapshot metadata.
- Keep the configuration explicit enough that MCP and non-MCP clients can express the same target context.

### Windows observer

- Collect visible desktop windows generically through UIA first, with MSAA fallback and provenance unchanged.
- Stop discarding windows solely because they do not match 1C process names, titles, or classes.
- Apply 1C-specific narrowing and metadata only after generic observation, based on the negotiated domain profile.

### Daemon cache

- Store a generic authoritative inventory and derive per-session filtered views from it.
- Preserve revision semantics and session-stable ids inside each session view.
- Avoid duplicating the full physical desktop scan per logical session when only the projection differs.

### MCP bridge

- Replace mode-only `session_open` semantics with profile/scope/filter semantics.
- Support explicit attach workflows for generic desktop apps by pid, process name, title, or class name.
- Preserve reconnect and no-auto-retry guarantees for mutating tools.

## Breaking Migration

The public contract changes from:

```text
requested_mode = enterprise_ui | configurator
negotiated_mode = enterprise_ui | configurator
snapshot.mode = enterprise_ui | configurator
```

to a structured profile model:

```text
requested_profile = { observation_scope, domain_profile, target_filter? }
negotiated_profile = { observation_scope, domain_profile, target_filter? }
snapshot.profile = { observation_scope, domain_profile, target_filter? }
```

Migration expectations:

1. Existing daemon and MCP clients must stop assuming every valid session is 1C-scoped.
2. Generic desktop consumers should request `domain_profile=generic`.
3. 1C-specific consumers should request the corresponding 1C profile explicitly.
4. Attach-style clients should pass a target filter instead of relying on post-hoc window guessing.

## Risks / Trade-offs

- A generic desktop inventory will surface more windows and may increase cache churn.
  - Mitigation: keep shallow/default projections, reuse one authoritative scan, and rely on event-driven targeted refresh.
- UIA cost can increase when non-1C windows are no longer dropped early.
  - Mitigation: introduce cache-aware UIA reads and scoped tree depth limits as part of the implementation.
- External clients will need to migrate.
  - Mitigation: update protocol docs, MCP docs, tests, and smoke workflows in the same rollout.
- 1C-focused workflows could regress if generic observation weakens domain annotations.
  - Mitigation: keep dedicated 1C regression coverage and make enrichment/profile application an explicit acceptance gate.

## Migration Plan

1. Define the new session profile model in OpenSpec and transport docs.
2. Refactor proto/domain/transport types away from public `SessionMode`.
3. Change the Windows observer to build a generic authoritative inventory.
4. Reintroduce 1C narrowing as a profile projection over the generic inventory.
5. Update daemon session startup and MCP `session_open` to negotiate the new profile model.
6. Add generic desktop smoke coverage on the remote VM and rerun existing 1C regressions.

## Quality Gates

- A generic desktop session can observe and target a non-1C app such as `Notepad`.
- A 1C-specific session still emits 1C metadata and preserves current diagnostic workflows.
- MCP sessions can be opened with explicit attach filters without relying on 1C hints.
- The daemon cache remains revisioned, event-driven, and screenshot-free in the hot read path.

## Open Questions

- Whether the initial public filter model should support exact title match only or also substring/regex semantics.
- Whether per-session projections should be computed eagerly on every refresh or lazily on first read.
- Whether a short-lived compatibility alias for legacy `SessionMode` should exist for one release or the cut should be immediate.
