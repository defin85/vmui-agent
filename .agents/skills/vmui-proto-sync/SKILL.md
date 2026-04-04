---
name: vmui-proto-sync
description: Keep the canonical wire contract, Rust domain model, transport conversion layer, and protocol docs aligned. Use when changing proto messages, action semantics, state delivery, or artifact contracts.
---

# vmui Proto Sync

## Use When

- `proto/vmui/v1/agent.proto` changes.
- Session, action, diff, resync, or artifact semantics change.

## Workflow

1. Update `proto/vmui/v1/agent.proto`.
2. Update matching types in `crates/vmui-protocol/src/lib.rs`.
3. Update conversion and transport expectations in `crates/vmui-transport-grpc/src/lib.rs`.
4. Update `docs/protocol.md` if behavior or semantics changed.
5. Run at least:
   - `cargo check --workspace`
   - `cargo test -p vmui-protocol`
   - `cargo test -p vmui-transport-grpc`
