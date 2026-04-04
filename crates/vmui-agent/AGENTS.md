# vmui-agent Instructions

## Scope
- This crate owns daemon startup, session lifecycle, daemon-side action execution, and artifact persistence.
- Start from `src/main.rs` for process entry, `src/lib.rs` for runtime behavior, and `src/tests.rs` for crate-local integration tests.

## Edit Routing
- Session handshake, subscribe flow, and gRPC service wiring:
  `src/lib.rs`
- Daemon-side actions such as `list_windows`, `get_tree`, `wait_for`, and `write_artifact`:
  `src/lib.rs`
- Daemon integration and regression tests:
  `src/tests.rs`
- Runtime defaults live in `vmui-core`, not here.

## Rules
- Keep daemon-resident actions in this crate; do not move them into platform backends.
- Prefer live cache plus backend refresh over screenshot polling.
- If you change runtime behavior, review and update matching coverage in `src/tests.rs`.
- If you change user-visible runtime defaults or startup expectations, update `README.md` and `docs/dev-runbook.md`.

## Verification
- `cargo test -p vmui-agent`
- Then run the workspace verification commands from the repo root.
