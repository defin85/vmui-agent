# vmui-platform-windows Instructions

## Scope
- This crate owns Windows-specific UI observation, refresh hints, semantic desktop actions, input, and capture.
- Start from `src/lib.rs` for backend orchestration, `src/windows_impl.rs` for Windows API work, and `src/tests.rs` for backend tests.

## Rules
- Keep Windows APIs inside this crate or behind `cfg(windows)`.
- UIA is primary; WinEvent and MSAA are hint or fallback paths.
- Do not introduce screenshot-first flows for state reads.
- Daemon-side actions such as `list_windows`, `get_tree`, `wait_for`, and `write_artifact` do not belong here.
- Non-Windows hosts must still compile, check, and test the workspace.
- Keep low-level Windows API handling in `src/windows_impl.rs`; avoid growing `src/lib.rs` back into a monolith.

## Verification
- `cargo test -p vmui-platform-windows`
- Then run the workspace verification commands from the repo root.
