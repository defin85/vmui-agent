## 1. Probe contract

- [ ] 1.1 Define a target model for selecting a specific panel or region inside a live Configurator window.
- [ ] 1.2 Define a probe artifact bundle schema that keeps HWND, UIA, MSAA, hit-test, and capture evidence aligned by target surface.
- [ ] 1.3 Document probe provenance semantics so later layers can distinguish real observations from absence of data.

## 2. Windows implementation

- [ ] 2.1 Add HWND hierarchy enumeration scoped to the selected panel surface.
- [ ] 2.2 Add UIA raw/control/content dumps for the selected surface.
- [ ] 2.3 Add MSAA/IAccessible traversal and hit-test collection for the same surface.
- [ ] 2.4 Add targeted region capture bound to the same probe result.

## 3. Agent integration

- [ ] 3.1 Expose the probe through daemon/MCP-accessible tooling without polluting the normal hot read path.
- [ ] 3.2 Add VM runbook steps for attaching the probe to the left Configurator tree.

## 4. Verification

- [ ] 4.1 Add Linux-host checks for new contracts and serialization.
- [ ] 4.2 Run a Windows VM probe against the live Configurator left tree and confirm that all expected artifact channels are emitted.
- [ ] 4.3 Run `openspec validate add-08-onec-out-of-process-panel-probe --strict --no-interactive`.
