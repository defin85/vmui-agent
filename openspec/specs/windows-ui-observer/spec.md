# windows-ui-observer Specification

## Purpose
TBD - created by archiving change add-02-windows-ui-observer. Update Purpose after archive.
## Requirements
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

#### Scenario: Fallback hint triggers a successful UIA refresh

- **WHEN** WinEvent or MSAA only acts as the trigger for a refresh
- **AND** the affected window can still be rebuilt from UIA
- **THEN** the refreshed window keeps UIA as its backend provenance
- **AND** the backend does not replace the whole window only because the refresh was triggered by a fallback hint

### Requirement: Event-driven refresh

The system SHALL transform backend events into targeted state refresh instead of full desktop rescans on every change.

#### Scenario: Focus changes inside one window

- **WHEN** the active element changes within a tracked window
- **THEN** the backend refreshes only the relevant scope needed to produce an updated diff
- **AND** it does not rescan unrelated windows as the default behavior

### Requirement: Session-stable identity and locators

The system SHALL expose session-stable window and element identifiers together with reusable semantic locators.

#### Scenario: Tree order changes but the same control remains present

- **WHEN** a subtree is rebuilt or sibling order changes within the same tracked window
- **THEN** the backend keeps the existing session id for the matched window and element
- **AND** the locator uses semantic fields such as control type, class name, automation id, and name
- **AND** sibling ordinal is used only as a duplicate tie-breaker

