## 1. Probe contract

- [x] 1.1 Define a target model for selecting a specific panel or region inside a live Configurator window.
- [x] 1.2 Define a probe artifact bundle schema that keeps HWND, UIA, MSAA, hit-test, and capture evidence aligned by target surface.
- [x] 1.3 Document probe provenance semantics so later layers can distinguish real observations from absence of data.

## 2. Windows implementation

- [x] 2.1 Add HWND hierarchy enumeration scoped to the selected panel surface.
- [x] 2.2 Add UIA raw/control/content dumps for the selected surface.
- [x] 2.3 Add MSAA/IAccessible traversal and hit-test collection for the same surface.
- [x] 2.4 Add targeted region capture bound to the same probe result.

## 3. Agent integration

- [x] 3.1 Expose the probe through daemon/MCP-accessible tooling without polluting the normal hot read path.
- [x] 3.2 Add VM runbook steps for attaching the probe to the left Configurator tree.

## 4. Verification

- [x] 4.1 Add Linux-host checks for new contracts and serialization.
- [x] 4.2 Run a Windows VM probe against the live Configurator left tree and confirm that all expected artifact channels are emitted.
- [x] 4.3 Run `openspec validate add-08-onec-out-of-process-panel-probe --strict --no-interactive`.
