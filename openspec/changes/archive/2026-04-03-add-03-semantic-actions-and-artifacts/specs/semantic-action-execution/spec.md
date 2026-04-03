## ADDED Requirements

### Requirement: Semantic-first action execution

The system SHALL execute supported actions through semantic patterns and cached UI state before coordinate fallback.

#### Scenario: Invoking a normal control

- **WHEN** a client requests `invoke` or `click_element` for a control that exposes a semantic pattern
- **THEN** the daemon uses the semantic pattern first
- **AND** it only reports coordinate fallback when a semantic path is unavailable or fails

### Requirement: Server-side wait conditions

The system SHALL evaluate wait conditions on the daemon side against live UI state and backend events.

#### Scenario: Waiting for an element to appear

- **WHEN** a client requests `wait_for` on a locator or element target
- **THEN** the daemon resolves the condition using cached state plus backend updates
- **AND** the client does not need to poll screenshots or full snapshots to detect completion

### Requirement: Action result status

The system SHALL return explicit action outcomes for success, failure, timeout, and unsupported behavior.

#### Scenario: Unsupported action path

- **WHEN** the client requests an action that the current backend cannot execute safely
- **THEN** the daemon returns an explicit unsupported or failed action result
- **AND** the result includes enough detail for later diagnostics
