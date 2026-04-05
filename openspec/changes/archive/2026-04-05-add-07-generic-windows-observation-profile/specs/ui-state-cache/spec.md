## ADDED Requirements

### Requirement: Generic inventory with session projections

The system SHALL maintain a generic authoritative desktop inventory and derive session-specific filtered views from it without destroying the underlying observation state.

#### Scenario: Generic and 1C sessions observe the same desktop

- **WHEN** a generic desktop session and a 1C-specific session observe the same Windows VM desktop concurrently
- **THEN** the daemon can expose non-1C windows to the generic session while hiding them from the 1C-filtered session view
- **AND** matching windows and elements keep session-stable identities and locators within each session view across targeted refresh
