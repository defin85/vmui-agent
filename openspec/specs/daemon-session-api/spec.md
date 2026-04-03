# daemon-session-api Specification

## Purpose
TBD - created by archiving change add-01-daemon-session-foundation. Update Purpose after archive.
## Requirements
### Requirement: Long-lived daemon session

The system SHALL expose a long-lived bidirectional daemon session that external clients can use to negotiate mode, subscribe to UI updates, submit actions, and receive results.

#### Scenario: Client subscribes to a fresh session

- **WHEN** a client opens a new daemon session and sends handshake plus subscription messages
- **THEN** the daemon returns an acknowledgement with session id, backend identity, and capabilities
- **AND** the daemon emits an initial UI snapshot before any later diff batch for that session

### Requirement: Action and artifact separation

The system SHALL keep the control session stream separate from large artifact payloads.

#### Scenario: Action emits diagnostic artifacts

- **WHEN** an action result includes screenshots, OCR results, or structured dumps
- **THEN** the action result references those artifacts by id and metadata only
- **AND** the binary or large structured content is retrieved through a dedicated artifact read operation

### Requirement: Mode-aware session negotiation

The system SHALL let the client request the intended operating mode during session startup.

#### Scenario: Client requests Configurator mode

- **WHEN** a client starts a session and requests Configurator mode
- **THEN** the daemon acknowledges the requested mode or returns an explicit warning if that mode cannot be honored

