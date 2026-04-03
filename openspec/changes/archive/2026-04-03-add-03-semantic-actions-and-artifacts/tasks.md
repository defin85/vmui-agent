## 1. Semantic action executor

- [x] 1.1 Implement `list_windows` and `get_tree` against the live cache and backend reads.
- [x] 1.2 Implement `focus_window`, `invoke`, `click_element`, `set_value`, and `send_keys` with semantic-first behavior and explicit fallback reporting.
- [x] 1.3 Implement server-side `wait_for` over live state and backend events.

## 2. Artifact capture

- [x] 2.1 Add artifact storage for structured dumps, screenshots, and OCR output references.
- [x] 2.2 Implement `capture_region` and `ocr_region` as explicit, scoped operations rather than default polling behavior.
- [x] 2.3 Add action-result tests that verify status, artifact references, and fallback reporting.

## 3. Validation

- [x] 3.1 Run `cargo fmt --all`.
- [x] 3.2 Run `cargo check --workspace`.
- [x] 3.3 Run `cargo test --workspace`.
