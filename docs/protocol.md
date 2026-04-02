# Protocol

## Core Session Flow

1. Client opens a long-lived session stream.
2. Client sends `hello`.
3. Server replies with `hello_ack`.
4. Client sends `subscribe`.
5. Server always sends `initial_snapshot` as the authoritative starting point for that session.
6. Server sends `diff_batch` messages as the UI changes.
7. Client sends `action_request` messages.
8. Server replies with `action_result`; artifact bytes are fetched separately through `ReadArtifact`.

## Message Families

### Client To Server

- `hello`
- `subscribe`
- `action_request`
- `read_artifact`
- `ping`

### Server To Client

- `hello_ack`
- `initial_snapshot`
- `diff_batch`
- `action_result`
- `artifact_ready`
- `warning`
- `pong`

## State Semantics

- `initial_snapshot` is the authoritative starting point for the client cache, even if the client asked to skip it.
- `diff_batch` moves the client from `base_rev` to `new_rev`.
- If the client misses revisions, the server emits `snapshot_resync` and follows it with a refreshed `initial_snapshot`.

## Action Design Rules

- Prefer semantic patterns such as invoke, value, toggle or selection before mouse coordinates.
- Every action can emit artifacts.
- `wait_for` is server-side and runs against the live cache plus backend events.
- OCR requests are explicit and scoped to a region or window.

## Artifact Policy

- Artifacts are referenced by id in the session stream.
- Action-generated artifacts are written into the daemon artifact store before their ids are exposed.
- Large payloads are transferred through a separate artifact read request.
- Expected artifact kinds:
  - `snapshot-json`
  - `diff-json`
  - `screenshot-png`
  - `ocr-json`
  - `annotated-png`
  - `log-text`
