## ADDED Requirements

### Requirement: Reversible behavioral mapping

The system SHALL support reversible navigation experiments for a selected live 1C Configurator tree surface.

#### Scenario: Selection moves by one step

- **WHEN** the mapper focuses the selected tree surface and performs a safe navigation sequence such as `click + Down`
- **THEN** it records before and after evidence for the same surface
- **AND** it can optionally apply a reverse step such as `Up` to restore the prior state

### Requirement: Session-local inferred tree state

The system SHALL expose session-local inferred tree state with explicit confidence and ambiguity markers.

#### Scenario: Mapper cannot read a semantic `TreeItem`

- **WHEN** the custom panel changes visually after navigation but accessibility layers still do not expose a semantic item
- **THEN** the mapper reports an inferred selected-node state
- **AND** it marks that state as inferred rather than directly observed

### Requirement: Exported repository overlay

The system SHALL support optional semantic overlay from an exported configuration repository tree.

#### Scenario: Exported `src/cf` tree is available

- **WHEN** the mapper receives a path to an exported repository tree
- **THEN** it can use that tree to narrow likely logical categories or descendants for the current inferred selection
- **AND** it keeps the overlay separate from directly observed live evidence
