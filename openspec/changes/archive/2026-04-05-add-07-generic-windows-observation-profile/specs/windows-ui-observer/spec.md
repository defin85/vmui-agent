## ADDED Requirements

### Requirement: Generic desktop inventory

The system SHALL treat the visible Windows desktop inventory as the authoritative observation source instead of pre-filtering snapshots to one application family.

#### Scenario: Generic profile observes a non-1C window

- **WHEN** a visible accessible desktop window such as `Notepad` is present
- **AND** the negotiated session profile requests generic desktop observation
- **THEN** the backend includes that window in the session snapshot
- **AND** it does not drop the window solely because it lacks 1C-specific process names, titles, classes, or annotations

### Requirement: Domain-specific enrichment is opt-in

The system SHALL apply 1C-specific classification, narrowing, and `onec_*` annotations only when the negotiated session profile requests a 1C domain profile.

#### Scenario: Generic profile skips 1C enrichment as an inclusion gate

- **WHEN** the negotiated session profile uses the generic desktop domain
- **THEN** the backend does not require `onec_*` metadata for a window to appear in session state
- **AND** generic desktop apps remain observable through the same authoritative state pipeline

#### Scenario: 1C profile enriches matching windows

- **WHEN** the negotiated session profile requests an 1C enterprise or configurator domain
- **AND** an observed window matches that domain
- **THEN** the backend annotates matching windows and nodes with relevant `onec_*` metadata
- **AND** non-matching windows may be excluded from that profile-specific session view
