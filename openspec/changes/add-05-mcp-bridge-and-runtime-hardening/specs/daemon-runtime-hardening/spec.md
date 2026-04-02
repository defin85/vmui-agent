## ADDED Requirements

### Requirement: Explicit runtime degradation signals

The system SHALL surface runtime degradation and recovery events explicitly.

#### Scenario: Backend requires a resync

- **WHEN** the daemon detects stale incremental state, backend restart, or another condition that invalidates incremental tracking
- **THEN** it emits an explicit warning or resynchronization signal
- **AND** clients can distinguish that condition from a normal steady-state diff stream

### Requirement: Artifact retention policy

The system SHALL manage diagnostic artifacts under an explicit retention policy.

#### Scenario: Artifact storage reaches retention thresholds

- **WHEN** stored artifacts exceed the configured retention policy
- **THEN** the daemon expires or rotates artifacts according to configuration
- **AND** metadata remains consistent with the stored state

### Requirement: Runtime quality observability

The system SHALL expose runtime quality signals for long-lived operation.

#### Scenario: Operator inspects daemon health

- **WHEN** an operator or higher-level system inspects daemon runtime quality
- **THEN** it can determine whether resyncs, fallback-heavy behavior, or action failures are increasing
- **AND** it can do so without relying only on manual log inspection
