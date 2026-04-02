## 1. Interactive Windows observer

- [ ] 1.1 Add a Windows observer runtime that only activates in the interactive desktop session and rejects unsupported host conditions.
- [ ] 1.2 Implement UIA tree reads for windows and elements needed by the daemon snapshot model.
- [ ] 1.3 Add WinEvent and MSAA/IAccessible fallback hooks that can trigger targeted refreshes.

## 2. Diff normalization

- [ ] 2.1 Normalize backend observations into revisioned snapshot and diff updates for `vmui-core`.
- [ ] 2.2 Track backend provenance and confidence on observed nodes and windows.
- [ ] 2.3 Add tests for event coalescing, refresh behavior, and non-Windows compile isolation where practical.

## 3. Validation

- [ ] 3.1 Run `cargo fmt --all`.
- [ ] 3.2 Run `cargo check --workspace`.
- [ ] 3.3 Run `cargo test --workspace`.
