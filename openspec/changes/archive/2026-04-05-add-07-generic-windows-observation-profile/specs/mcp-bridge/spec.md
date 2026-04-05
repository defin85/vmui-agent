## MODIFIED Requirements

### Requirement: Explicit logical MCP sessions

The system SHALL expose logical MCP sessions that are distinct from individual tool invocations and that retain negotiated observation profile, target filters, and daemon continuity state.

#### Scenario: MCP client opens a generic desktop session

- **WHEN** an MCP client opens a bridge session for generic desktop observation
- **THEN** the bridge returns a logical session identifier backed by a reusable daemon session with the negotiated generic profile
- **AND** later related tool calls can reference that identifier to reuse cached state and locators across those calls

## ADDED Requirements

### Requirement: Explicit attach filters for MCP sessions

The system SHALL let MCP clients open bridge sessions scoped by explicit attach filters such as pid, process name, title, or class name.

#### Scenario: MCP client targets a non-1C desktop app

- **WHEN** an MCP client opens a bridge session with an attach filter for a desktop app such as `Notepad`
- **THEN** the bridge forwards that filter to the daemon session configuration
- **AND** `list_windows`, `get_tree`, and action tools operate on that filtered session view without relying on 1C-specific hints
