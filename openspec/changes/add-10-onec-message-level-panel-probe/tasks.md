## 1. Message-level probe contract

- [ ] 1.1 Define target selection and output schema for message-level panel probing.
- [ ] 1.2 Define explicit unsupported semantics for opaque custom panels.

## 2. Standard-control interrogation

- [ ] 2.1 Detect whether the selected surface or a descendant HWND exposes standard tree/list control behavior.
- [ ] 2.2 Query standard messages when supported and record structural results.
- [ ] 2.3 Correlate safe navigation inputs with standard control responses or observable WinEvent output.

## 3. Diagnostics integration

- [ ] 3.1 Expose the message-level probe without changing the normal session hot path.
- [ ] 3.2 Add probe artifacts and logs that can be compared with behavioral and out-of-process evidence.

## 4. Verification

- [ ] 4.1 Run the probe against the live Configurator tree and record whether standard control interrogation is supported.
- [ ] 4.2 Validate explicit unsupported reporting for opaque cases.
- [ ] 4.3 Run `openspec validate add-10-onec-message-level-panel-probe --strict --no-interactive`.
