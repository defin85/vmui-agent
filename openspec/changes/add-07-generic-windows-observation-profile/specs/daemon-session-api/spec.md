## MODIFIED Requirements

### Requirement: Long-lived daemon session

The system SHALL expose a long-lived bidirectional daemon session that external clients can use to negotiate an observation profile, subscribe to UI updates, submit actions, and receive results.

#### Scenario: Client subscribes to a fresh session

- **WHEN** a client opens a new daemon session and sends handshake plus subscription messages
- **THEN** the daemon returns an acknowledgement with session id, backend identity, capabilities, and negotiated observation profile
- **AND** the daemon emits an initial UI snapshot before any later diff batch for that session

## ADDED Requirements

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

## REMOVED Requirements

### Requirement: Mode-aware session negotiation

**Reason**: a single `SessionMode` enum cannot express generic desktop observation, app/window attach workflows, and domain-specific enrichment independently.

**Migration**: clients must migrate to the new session profile contract that separates observation scope, domain profile, and optional target filters.
