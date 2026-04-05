# mcp-bridge Specification

## Purpose

Define the MCP bridge that exposes vmui daemon capabilities through a thin, session-aware adapter instead of reimplementing UI automation logic in the MCP layer.

## Requirements
### Requirement: MCP as an adapter

The system SHALL provide MCP access through a thin adapter over the daemon session model.

#### Scenario: MCP client requests a window list

- **WHEN** an MCP client invokes a tool that requires current UI state
- **THEN** the MCP bridge resolves that request through an existing or newly established daemon session
- **AND** it returns data derived from daemon state rather than requiring direct screenshot polling

### Requirement: Explicit logical MCP sessions

The system SHALL expose logical MCP sessions that are distinct from individual tool invocations.

#### Scenario: MCP client opens a logical session

- **WHEN** an MCP client opens a bridge session for a specific mode or target context
- **THEN** the bridge returns a logical session identifier backed by a reusable daemon session
- **AND** later related tool calls can reference that identifier to reuse cached state and locators

### Requirement: Session reuse for MCP workflows

The system SHALL let MCP-driven workflows reuse daemon sessions when multiple related tool calls operate on the same target context.

#### Scenario: MCP client performs multiple related steps

- **WHEN** an MCP client issues several related calls against the same logical session
- **THEN** the bridge reuses the relevant daemon session whenever possible
- **AND** it preserves access to previously observed state and locators across those calls

#### Scenario: MCP client omits session id while several sessions exist

- **WHEN** an MCP tool call does not identify a logical session
- **AND** multiple compatible bridge sessions exist
- **THEN** the bridge returns an explicit ambiguity error instead of guessing
- **AND** it does not silently bind the call to an arbitrary daemon session

### Requirement: Safe reconnect semantics for MCP-driven actions

The system SHALL preserve safety when MCP-driven workflows lose daemon connectivity.

#### Scenario: Read-only operation resumes after reconnect

- **WHEN** a read-only MCP tool call encounters daemon session loss
- **THEN** the bridge may reconnect and rebuild state from a fresh daemon snapshot
- **AND** the result indicates whether prior state continuity was preserved or invalidated

#### Scenario: Mutating operation is interrupted by reconnect

- **WHEN** a mutating MCP tool call loses daemon connectivity before completion
- **THEN** the bridge MUST NOT silently retry that action
- **AND** it returns an explicit failure or retry-needed result so the caller can decide what to do next
