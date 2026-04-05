# ui-state-cache Specification

## Purpose
Define authoritative snapshot-plus-diff state semantics, resynchronization rules, and session-stable identifiers.
## Requirements
### Requirement: Revisioned snapshot state

The system SHALL maintain authoritative UI state as a revisioned snapshot plus monotonic diff batches.

#### Scenario: Client tracks state incrementally

- **WHEN** the daemon has already emitted an initial snapshot for a session
- **THEN** every later diff batch references the previous base revision and a newer revision
- **AND** revisions increase monotonically within the session

### Requirement: Resynchronization on revision gaps

The system SHALL support snapshot resynchronization when incremental state delivery becomes invalid.

#### Scenario: Client misses revisions

- **WHEN** the daemon detects that a client cannot safely continue from the last known revision
- **THEN** the daemon emits an explicit resynchronization event or equivalent full snapshot path
- **AND** the client can rebuild its local state without using screenshot polling

### Requirement: Session-stable identifiers

The system SHALL represent UI elements with session-stable ids plus reusable locators.

#### Scenario: Client refers to an element later in the same session

- **WHEN** the client receives an element id and locator in one snapshot or diff batch
- **THEN** the daemon can accept follow-up requests that target the same live element by that id while the element remains valid
- **AND** the locator remains available for re-resolution when the live id becomes stale

### Requirement: Generic inventory with session projections

The system SHALL maintain a generic authoritative desktop inventory and derive session-specific filtered views from it without destroying the underlying observation state.

#### Scenario: Generic and 1C sessions observe the same desktop

- **WHEN** a generic desktop session and a 1C-specific session observe the same Windows VM desktop concurrently
- **THEN** the daemon can expose non-1C windows to the generic session while hiding them from the 1C-filtered session view
- **AND** matching windows and elements keep session-stable identities and locators within each session view across targeted refresh

