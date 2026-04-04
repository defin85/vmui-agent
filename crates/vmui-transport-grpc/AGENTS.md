# vmui-transport-grpc Instructions

## Scope
- This crate owns protobuf generation, tonic-facing wire types, and conversion between the canonical proto contract and Rust domain models.
- Start from `build.rs` for code generation setup and `src/lib.rs` for transport/domain conversion logic.

## Edit Routing
- Protobuf generation and include setup:
  `build.rs`
- Domain <-> protobuf conversion:
  `src/lib.rs`
- Transport roundtrip tests:
  `src/lib.rs`

## Rules
- `proto/vmui/v1/agent.proto` remains the canonical wire contract.
- Any semantic wire change must stay aligned with `crates/vmui-protocol/src/lib.rs`, `proto/AGENTS.md`, and `docs/protocol.md`.
- Keep transport mapping explicit; do not hide wire-format changes behind ad hoc defaults.
- Preserve Linux-host `cargo check --workspace` and crate-local tests when updating generation or conversion logic.

## Verification
- `cargo test -p vmui-transport-grpc`
- `cargo test -p vmui-protocol`
- Then run the workspace verification commands from the repo root.
