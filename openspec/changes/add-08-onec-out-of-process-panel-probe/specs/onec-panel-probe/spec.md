## ADDED Requirements

### Requirement: Out-of-process panel probe

The system SHALL provide a non-invasive probe for a selected live 1C Configurator surface without injecting code into `1cv8.exe`.

#### Scenario: Probe attaches to the left configuration tree

- **WHEN** an operator selects the left Configurator tree as the probe target
- **THEN** the system attaches from outside the process boundary
- **AND** it does not require restarting Configurator or enabling in-process instrumentation

### Requirement: Multi-source probe bundle

The system SHALL emit a probe bundle that preserves evidence from each observation layer separately.

#### Scenario: Probe returns aligned artifacts

- **WHEN** a probe completes successfully for one selected surface
- **THEN** it returns HWND hierarchy data, UIA data, MSAA data, hit-test output, and a targeted capture for that same surface
- **AND** each artifact records its own provenance rather than being collapsed into one synthetic tree

### Requirement: Explicit opaque-surface reporting

The system SHALL explicitly report when a selected surface exposes weak or missing semantics at this layer.

#### Scenario: UIA and MSAA cannot describe the panel structure

- **WHEN** the target panel only exposes shallow or opaque accessibility data
- **THEN** the probe marks that layer as insufficient
- **AND** it does not claim that the panel has no children merely because the layer is weak
