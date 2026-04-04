# onec-diagnostic-workflows Specification

## Purpose
Define explicit 1C diagnostic modes, post-failure diagnostic bundles, and fallback-aware reporting that complements standard 1C automated testing.
## Requirements
### Requirement: Explicit 1C operating modes

The system SHALL expose explicit operating modes for 1C application UI and 1C Configurator workflows.

#### Scenario: Client selects application UI mode

- **WHEN** a client starts a session for ordinary 1C application diagnostics
- **THEN** the daemon tracks only the configured application processes and windows for that mode
- **AND** it does not default to broad desktop-wide automation scope

#### Scenario: Client selects Configurator mode

- **WHEN** a client starts a session for Configurator work
- **THEN** the daemon applies Configurator-specific filtering and profiles
- **AND** it exposes that mode in session metadata and diagnostics

### Requirement: Cooperation with standard 1C testing

The system SHALL complement standard 1C automated testing rather than replace it.

#### Scenario: Test runner reports a failed step

- **WHEN** a standard 1C automated test fails and external orchestration requests diagnostics
- **THEN** the daemon collects the relevant current UI state and targeted artifacts for the failure context
- **AND** it preserves the distinction between the original test verdict and daemon-side diagnostic data

### Requirement: Post-failure diagnostic bundle

The system SHALL support a post-failure diagnostic bundle for 1C scenarios.

#### Scenario: Investigating a failed UI step

- **WHEN** a client requests diagnostics for a failed 1C step
- **THEN** the daemon can return the active windows, element tree snapshot, relevant diff information, and targeted artifacts for the affected context
- **AND** the diagnostic output highlights whether semantic data or fallback methods were used

### Requirement: Fallback-aware 1C profiles

The system SHALL record when 1C surfaces require fallback handling because semantic accessibility data is incomplete.

#### Scenario: Ordinary form or Configurator editor is opaque

- **WHEN** the daemon encounters a 1C surface with weak or missing semantic metadata
- **THEN** it marks that surface as low-confidence or fallback-driven in diagnostic output
- **AND** later actions and reports can use that information to explain degraded behavior
