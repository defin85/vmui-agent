## ADDED Requirements

### Requirement: Interactive Windows observation

The system SHALL run Windows UI observation only inside the interactive VM desktop session.

#### Scenario: Daemon starts in an unsupported session

- **WHEN** the daemon starts on a non-Windows host or in a non-interactive Windows session
- **THEN** it reports that the live Windows observer is unavailable
- **AND** it does not pretend that UI observation is active

### Requirement: UIA-first state collection

The system SHALL use UI Automation as the primary source for windows, elements, and semantic properties.

#### Scenario: Snapshot reads a normal accessible window

- **WHEN** the daemon reads state for a window that exposes usable UIA data
- **THEN** the backend builds the window and element tree from UIA
- **AND** it preserves control types, names, bounds, and relevant properties in the daemon state model

### Requirement: WinEvent and MSAA fallback

The system SHALL support WinEvent and MSAA/IAccessible as fallback inputs for observation and refresh.

#### Scenario: UIA coverage is incomplete

- **WHEN** a control or event cannot be resolved sufficiently through UIA alone
- **THEN** the backend uses WinEvent and/or MSAA to locate the affected surface or trigger a targeted refresh
- **AND** the resulting node metadata records that fallback provenance was used

### Requirement: Event-driven refresh

The system SHALL transform backend events into targeted state refresh instead of full desktop rescans on every change.

#### Scenario: Focus changes inside one window

- **WHEN** the active element changes within a tracked window
- **THEN** the backend refreshes only the relevant scope needed to produce an updated diff
- **AND** it does not rescan unrelated windows as the default behavior
