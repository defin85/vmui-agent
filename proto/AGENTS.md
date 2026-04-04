# proto Instructions

## Scope
- `proto/vmui/v1/agent.proto` is the canonical wire contract.

## Rules
- Any semantic change here must be mirrored in:
  `crates/vmui-protocol/src/lib.rs`
  `crates/vmui-transport-grpc/src/lib.rs`
- Re-read `docs/protocol.md` when changing message flow, action semantics, or artifact handling.
- Do not leave proto-only changes without transport or domain alignment.

## Verification
- `cargo check --workspace`
- `cargo test -p vmui-transport-grpc`
- `cargo test -p vmui-protocol`
