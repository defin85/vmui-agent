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
- Runtime degradation and recovery stay explicit through `warning` messages such as `session_resync_required`, `session_state_recovered`, `session_resync_apply_failed`, `session_resync_refresh_failed`, and `artifact_retention_cleanup`.
- Session mode scopes 1C observation to the intended surface family instead of broad desktop-wide automation.
- Windows and elements carry backend provenance plus confidence so fallback-triggered refreshes stay explicit.
- 1C-oriented snapshots can annotate windows and nodes with profile metadata such as `onec_window_profile`, `onec_profile`, and `onec_fallback_reason`.
- WinEvent and MSAA are treated as refresh hints; the emitted snapshot/diff remains the source of truth.
- Window and element ids are expected to stay stable across targeted refresh when the same semantic control is matched again.
- Locator segments should contain semantic fields first, with sibling ordinal used only as a duplicate tie-breaker.

## Action Design Rules

- Prefer semantic patterns such as invoke, value, toggle or selection before mouse coordinates.
- `list_windows`, `get_tree`, `get_runtime_status`, and `write_artifact` return structured JSON through artifact references rather than inline payloads.
- `get_tree.raw=true` returns the raw target object; `raw=false` wraps the target with contextual fields such as `window_id`.
- Every action can emit artifacts.
- `wait_for` is server-side and runs against the live cache plus backend events.
- `collect_diagnostic_bundle` is server-side and complements standard 1C automated testing by preserving the original external test verdict together with daemon-side state, recent diffs, baseline comparison, and target-scoped artifacts.
- `timeout_ms` applies both to daemon-side waits and backend action execution.
- OCR requests are explicit and scoped to a region or window.
- `capture_region` is explicit and scoped; `window_id + bounds` is interpreted as a window-relative region.

## Artifact Policy

- Artifacts are referenced by id in the session stream.
- Action-generated artifacts are written into the daemon artifact store before their ids are exposed.
- Large payloads are transferred through a separate artifact read request.
- Expected artifact kinds:
  - `snapshot-json`
  - `diff-json`
  - `diagnostic-json`
  - `baseline-comparison-json`
  - `runtime-status-json`
  - `screenshot-png`
  - `screenshot-jpeg`
  - `ocr-json`
  - `annotated-png`
  - `log-text`
