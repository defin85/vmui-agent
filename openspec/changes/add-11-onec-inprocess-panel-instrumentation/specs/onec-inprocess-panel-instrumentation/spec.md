## ADDED Requirements

### Requirement: Explicit opt-in instrumentation

The system SHALL require an explicit opt-in before enabling any in-process instrumentation inside `1cv8.exe`.

#### Scenario: Default runtime starts normally

- **WHEN** the daemon starts without the experimental instrumentation flag
- **THEN** it does not attach any in-process companion to `1cv8.exe`
- **AND** the normal UIA/MSAA-based runtime remains the active default path

### Requirement: Build-gated attachment

The system SHALL validate the target 1C build fingerprint before allowing in-process attachment.

#### Scenario: Unsupported build is detected

- **WHEN** the instrumentation layer encounters an unknown or unsupported 1C build fingerprint
- **THEN** it refuses to attach
- **AND** it returns an explicit unsupported result instead of attempting best-effort introspection

### Requirement: Fail-closed isolation

The system SHALL isolate experimental in-process failures from the default daemon runtime.

#### Scenario: Companion initialization fails

- **WHEN** the in-process companion or hook fails to initialize or terminates unexpectedly
- **THEN** the main daemon remains available for its normal observation and action paths
- **AND** the experimental layer reports failure without silently downgrading to undefined behavior
