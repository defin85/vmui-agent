## Context

The daemon is the contract root for all later work. If session, revision, and artifact semantics stay vague, Windows backend work and MCP integration will drift or duplicate logic.

## Goals / Non-Goals

- Goals:
  - Freeze one canonical session model for snapshot, diff, actions, and artifacts.
  - Keep the contract transport-safe and independent from the Windows backend implementation details.
  - Preserve Linux-host compilation and testing for non-Windows parts of the workspace.
- Non-Goals:
  - Implement UIA/WinEvent/MSAA observation in this change.
  - Implement 1C-specific workflows in this change.

## Decisions

- Decision: Introduce the first real gRPC transport now, not later.
  - Alternatives considered:
    - Keep using only in-memory protocol models until the Windows backend exists.
    - Use WebSocket JSON for the first runtime slice.
  - Rationale: the project already treats gRPC as the primary transport, and later changes depend on stable streaming semantics.
- Decision: Keep large artifacts out of the main session stream.
  - Alternatives considered:
    - Inline screenshots and OCR payloads in action results.
  - Rationale: artifact payloads would complicate diff and action flow control and encourage screenshot-heavy clients.

## Risks / Trade-offs

- More upfront transport code before Windows automation exists.
  - Mitigation: keep backend traits and daemon wiring thin and testable on Linux.
- Generated types can drift from hand-written protocol models.
  - Mitigation: add explicit conversion coverage and keep `vmui-protocol` as the domain source of truth.

## Migration Plan

1. Add transport crate(s) and generated protobuf code.
2. Implement session bootstrap and artifact read path.
3. Make `vmui-agent` serve the contract against a placeholder backend.

## Open Questions

- Whether the generated protobuf types should live in a standalone `vmui-transport-grpc` crate or inside `vmui-agent`.
