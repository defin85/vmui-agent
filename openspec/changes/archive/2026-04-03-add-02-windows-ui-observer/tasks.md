## 1. Interactive Windows observer

- [x] 1.1 Add a Windows observer runtime that only activates in the interactive desktop session and rejects unsupported host conditions.
- [x] 1.2 Implement UIA tree reads for windows and elements needed by the daemon snapshot model.
- [x] 1.3 Add WinEvent and MSAA/IAccessible fallback hooks that can trigger targeted refreshes.

## 2. Diff normalization

- [x] 2.1 Normalize backend observations into revisioned snapshot and diff updates for `vmui-core`.
- [x] 2.2 Track backend provenance and confidence on observed nodes and windows.
- [x] 2.3 Add tests for event coalescing, refresh behavior, and non-Windows compile isolation where practical.
- [x] 2.4 Implement session-stable window and element identity with semantic locator segments.
- [x] 2.5 Keep fallback hint provenance separate from the backend that actually produced the refreshed tree.
- [x] 2.6 Add tests for identity stability and fallback-triggered UIA refresh semantics.

## 3. Validation

- [x] 3.1 Run `cargo fmt --all`.
- [x] 3.2 Run `cargo check --workspace`.
- [x] 3.3 Run `cargo test --workspace`.
