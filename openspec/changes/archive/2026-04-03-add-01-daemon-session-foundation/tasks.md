## 1. Transport and session bootstrap

- [x] 1.1 Add a typed gRPC transport layer that serves the `UiAgent` contract from `proto/vmui/v1/agent.proto`.
- [x] 1.2 Implement daemon startup, handshake, subscription, and session lifecycle wiring in `vmui-agent`.
- [x] 1.3 Expose artifact read streaming by artifact id without embedding large binary payloads into the main session stream.

## 2. Revisioned state model

- [x] 2.1 Implement the first authoritative UI snapshot cache with monotonic revisions and resync handling.
- [x] 2.2 Keep `vmui-protocol` and generated transport types aligned through tests or conversion checks.
- [x] 2.3 Persist session metadata and artifact metadata in `vmui-core`.

## 3. Validation

- [x] 3.1 Run `cargo fmt --all`.
- [x] 3.2 Run `cargo check --workspace`.
- [x] 3.3 Run `cargo test --workspace`.
