## ADDED Requirements

### Requirement: MCP as an adapter

The system SHALL provide MCP access through a thin adapter over the daemon session model.

#### Scenario: MCP client requests a window list

- **WHEN** an MCP client invokes a tool that requires current UI state
- **THEN** the MCP bridge resolves that request through an existing or newly established daemon session
- **AND** it returns data derived from daemon state rather than requiring direct screenshot polling

### Requirement: Session reuse for MCP workflows

The system SHALL let MCP-driven workflows reuse daemon sessions when multiple related tool calls operate on the same target context.

#### Scenario: MCP client performs multiple related steps

- **WHEN** an MCP client issues several related calls against the same target context
- **THEN** the bridge reuses the relevant daemon session whenever possible
- **AND** it preserves access to previously observed state and locators across those calls
