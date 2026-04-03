# Change: Add daemon session foundation

## Why

The repository currently contains only a compileable scaffold. It does not yet define an executable in-VM daemon contract for long-lived sessions, revisioned UI state, or artifact retrieval.

## What Changes

- Add the first executable daemon transport slice around a long-lived bidirectional session.
- Introduce authoritative session lifecycle semantics: handshake, subscription, initial snapshot, diff stream, action dispatch, and artifact retrieval.
- Formalize revisioned UI state and session-stable identifiers so later Windows backend and 1C-specific work build on one contract.
- Add a dedicated gRPC transport implementation layer that stays aligned with `vmui-protocol`.

## Impact

- Affected specs: `daemon-session-api`, `ui-state-cache`
- Affected code: `crates/vmui-protocol`, `crates/vmui-core`, `crates/vmui-agent`, new transport crate(s), `proto/vmui/v1/agent.proto`
