## ADDED Requirements

### Requirement: Explicit artifact capture

The system SHALL expose structured artifact capture as explicit operations and action side effects.

#### Scenario: Capturing a failing region

- **WHEN** a client requests a region capture or an action fails with capture enabled
- **THEN** the daemon stores the resulting artifact and returns only artifact references on the session stream

### Requirement: OCR as fallback

The system SHALL treat OCR as a fallback mechanism, not the default state model.

#### Scenario: Reading text from an opaque surface

- **WHEN** the backend cannot obtain the required semantic data through UIA or MSAA
- **THEN** the client can request OCR for the specific region or window
- **AND** the daemon records that OCR fallback was used

### Requirement: No screenshot polling requirement

The system SHALL not require screenshot polling for ordinary client interaction.

#### Scenario: Client tracks UI changes over time

- **WHEN** the client needs to observe ongoing UI changes
- **THEN** the daemon provides state updates through snapshot and diff delivery
- **AND** screenshots remain optional artifacts rather than the primary transport
