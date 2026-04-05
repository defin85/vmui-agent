## ADDED Requirements

### Requirement: Standard-control message interrogation

The system SHALL test whether a selected 1C Configurator surface exposes standard control-message semantics before declaring the panel opaque at this layer.

#### Scenario: Wrapped standard tree control exists

- **WHEN** the selected panel or one of its child HWNDs supports standard tree-view style interrogation
- **THEN** the probe records the relevant structural results from that message path
- **AND** it ties those results to the selected surface and child HWND identity

### Requirement: Input-to-message correlation

The system SHALL correlate safe navigation input with message-level or event-level observations for the same selected surface.

#### Scenario: Directional navigation changes panel state

- **WHEN** the probe sends a safe navigation action such as `Down` or `Right`
- **THEN** it records any corresponding standard-control response or event evidence that follows
- **AND** it preserves timing and target provenance for later analysis

### Requirement: Explicit unsupported message layer

The system SHALL return an explicit unsupported result when a selected surface does not expose usable message-level semantics.

#### Scenario: Panel remains opaque at this layer

- **WHEN** the selected surface does not support standard interrogation and yields no useful message-level evidence
- **THEN** the probe reports that the surface is opaque at the message layer
- **AND** it does not fabricate tree structure from missing data
