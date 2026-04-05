# daemon-session-api Specification

## Purpose
Define the long-lived daemon session contract, including handshake, mode negotiation, and artifact delivery boundaries.
## Requirements
### Requirement: Long-lived daemon session

The system SHALL expose a long-lived bidirectional daemon session that external clients can use to negotiate an observation profile, subscribe to UI updates, submit actions, and receive results.

#### Scenario: Client subscribes to a fresh session

- **WHEN** a client opens a new daemon session and sends handshake plus subscription messages
- **THEN** the daemon returns an acknowledgement with session id, backend identity, capabilities, and negotiated observation profile
- **AND** the daemon emits an initial UI snapshot before any later diff batch for that session

### Requirement: Action and artifact separation

The system SHALL keep the control session stream separate from large artifact payloads.

#### Scenario: Action emits diagnostic artifacts

- **WHEN** an action result includes screenshots, OCR results, or structured dumps
- **THEN** the action result references those artifacts by id and metadata only
- **AND** the binary or large structured content is retrieved through a dedicated artifact read operation

### Requirement: Profile-aware session negotiation

The system SHALL let the client request observation scope, domain profile, and optional initial target filters during session startup.

#### Scenario: Client requests a generic desktop profile

- **WHEN** a client starts a session and requests generic desktop observation with no domain-specific narrowing
- **THEN** the daemon acknowledges the negotiated generic profile
- **AND** the initial snapshot may include non-1C desktop windows such as `Notepad`

#### Scenario: Client requests an attach-filtered profile

- **WHEN** a client starts a session with an explicit attach filter such as pid, process name, title, or class name
- **THEN** the daemon acknowledges the negotiated filter or returns an explicit warning or failure if that request cannot be honored safely
- **AND** later snapshots stay scoped to that configured target view

