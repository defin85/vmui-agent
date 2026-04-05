# daemon-runtime-hardening Specification

## Purpose

Define the runtime hardening rules for long-lived daemon operation, including explicit degradation signals, recovery boundaries, artifact retention, and structured observability.

## Requirements
### Requirement: Explicit runtime degradation signals

The system SHALL surface runtime degradation and recovery events explicitly.

#### Scenario: Backend requires a resync

- **WHEN** the daemon detects stale incremental state, backend restart, or another condition that invalidates incremental tracking
- **THEN** it emits an explicit warning or resynchronization signal
- **AND** clients can distinguish that condition from a normal steady-state diff stream

### Requirement: Explicit reconnect and recovery boundaries

The system SHALL expose whether long-running session state remains trustworthy after recovery.

#### Scenario: Backend restart invalidates session continuity

- **WHEN** the daemon loses backend continuity and re-establishes observation for a session
- **THEN** it records an explicit recovery reason and whether cached locators or state continuity were invalidated
- **AND** clients can distinguish recovered read-state availability from uninterrupted session continuity

### Requirement: Artifact retention policy

The system SHALL manage diagnostic artifacts under an explicit retention policy.

#### Scenario: Artifact storage reaches retention thresholds

- **WHEN** stored artifacts exceed the configured retention policy
- **THEN** the daemon expires or rotates artifacts according to configuration
- **AND** metadata remains consistent with the stored state

#### Scenario: Daemon starts with expired artifacts on disk

- **WHEN** the daemon starts and finds artifacts that already violate retention policy
- **THEN** it cleans them up before exposing stale descriptors to clients
- **AND** the in-memory artifact metadata reflects the cleaned state

### Requirement: Runtime quality observability

The system SHALL expose runtime quality signals for long-lived operation.

#### Scenario: Operator inspects daemon health

- **WHEN** an operator or higher-level system inspects daemon runtime quality
- **THEN** it can determine whether resyncs, fallback-heavy behavior, or action failures are increasing
- **AND** it can do so without relying only on manual log inspection

#### Scenario: Client reads structured runtime status

- **WHEN** a client requests daemon runtime status or metrics
- **THEN** the daemon returns structured counters or summaries for resyncs, warnings, fallback-heavy observation, action outcome distribution, and artifact store pressure
- **AND** that information is available without inferring health only from log text
