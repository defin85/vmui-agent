## 1. Operating modes and profiles

- [ ] 1.1 Add explicit daemon modes for `enterprise_ui` and `configurator`.
- [ ] 1.2 Implement process, window, and locator profile filtering for common 1C application and Configurator surfaces.
- [ ] 1.3 Mark low-confidence and opaque surfaces so later actions and reports can communicate fallback expectations clearly.

## 2. Post-failure diagnostics

- [ ] 2.1 Implement a diagnostic bundle flow that captures state, diffs, and targeted artifacts after a failed standard 1C automated test step.
- [ ] 2.2 Add baseline comparison support for expected versus actual UI state at the diagnostic layer.
- [ ] 2.3 Document how the daemon integrates alongside standard 1C automated testing instead of replacing it.

## 3. Validation

- [ ] 3.1 Run `cargo fmt --all`.
- [ ] 3.2 Run `cargo check --workspace`.
- [ ] 3.3 Run `cargo test --workspace`.
