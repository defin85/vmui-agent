## Context

The daemon is intended to serve external agents, but those agents often speak tool protocols rather than native gRPC. The project also targets long-lived sessions, so reconnect and observability become part of the product rather than optional polish.

## Goals / Non-Goals

- Goals:
  - Add an MCP adapter without turning MCP into the internal source of truth.
  - Keep daemon sessions and cached state reusable across related MCP tool calls.
  - Make long-running session behavior observable and recoverable.
  - Preserve the no-screenshot-polling contract for ordinary agent usage.
- Non-Goals:
  - Replacing the primary daemon transport with MCP.
  - Hiding backend degradation or resync events from clients.
  - Re-implementing daemon-side UI logic, locator semantics, or artifact retention inside the MCP adapter.

## Decisions

- Decision: Keep MCP as a thin proxy over daemon sessions and ship `stdio` first.
  - Alternatives considered:
    - Make the daemon itself speak MCP as the primary API.
    - Start with Streamable HTTP or another remote MCP transport.
  - Rationale: the repository architecture already treats MCP as an adapter, the daemon needs a richer typed contract internally, and `stdio` is the lowest-risk first transport for local agent workflows.
- Decision: Introduce explicit logical MCP sessions backed by reusable daemon session workers.
  - Alternatives considered:
    - Treat every MCP tool call as an independent daemon session.
    - Keep implicit reuse only when exactly one session exists, without explicit session ids.
  - Rationale: the daemon contract is stream-based and stateful. Per-call sessions would repeatedly rebuild snapshots and lose locators, which breaks the reuse requirement and adds observer overhead. The bridge needs its own logical session id, while each logical session is owned by a single worker that manages one daemon stream at a time.
- Decision: Keep runtime health, resync, and retention as daemon-first concerns.
  - Alternatives considered:
    - Expose runtime quality only through MCP-local counters or logs.
    - Implement artifact cleanup only in the MCP proxy.
  - Rationale: MCP is optional. Runtime hardening must remain available to non-MCP clients and must not fork the source of truth for health, artifacts, or warning semantics.
- Decision: Reconnect must be explicit and safe, especially for mutating actions.
  - Alternatives considered:
    - Automatically retry interrupted actions after reconnect.
    - Hide daemon restarts behind transparent proxy retries.
  - Rationale: write actions such as click, invoke, set value, and send keys are not generally safe to replay. The bridge may rebuild read state after reconnect, but it must never silently repeat a mutating action.
- Decision: Add explicit health and warning surfaces.
  - Alternatives considered:
    - Keep reconnect and fallback behavior implicit in logs only.
  - Rationale: higher-level agents need to know when state quality degraded or a resync occurred.

## Proposed Runtime Shape

```text
MCP client (stdio)
        |
   vmui-mcp-proxy
        |
  +-----+---------------------------+
  |                                 |
tool router                  session registry
  |                                 |
  |                         logical session id
  |                                 |
  +----------> DaemonSessionWorker <+
                     |
             gRPC session stream
                     |
                 vmui-agent
                     |
      session registry / state cache / artifact store
```

## Component Design

### `vmui-mcp-proxy`

- Use `rmcp` server mode with `stdio` transport in the first rollout.
- Add explicit session lifecycle tools, at minimum:
  - `session_open`
  - `session_status`
  - `session_close`
- Add read tools that resolve through cached daemon state rather than screenshot polling, for example:
  - `list_windows`
  - `get_tree`
  - `wait_for`
  - diagnostic and artifact read helpers as needed
- Keep write-capable tools separate from read-only tools so MCP annotations and future policy gates can distinguish them clearly.

### `DaemonSessionWorker`

- Each logical MCP session owns one `DaemonSessionWorker`.
- The worker owns:
  - daemon gRPC client connection
  - active daemon `session_id`
  - latest snapshot revision and cached snapshot
  - warning/resync state
  - pending action correlation
- The worker serializes writes over the single daemon stream and updates its local cache from `initial_snapshot` plus `diff_batch`.
- The worker may support single-session implicit resolution only when exactly one compatible logical session exists. Otherwise tools must require explicit `session_id`.

### `vmui-agent`

- Extend daemon runtime surfaces rather than teaching the MCP proxy to infer health indirectly.
- Add a read-only runtime status/metrics action or equivalent daemon contract surface that reports:
  - resync count and last resync reason
  - warning counts by class
  - fallback-heavy observation rate
  - action outcome distribution
  - artifact store pressure and cleanup activity
- Keep `warning` and `snapshot_resync` as real-time stream signals; the new runtime status surface complements them with aggregated state.

### `vmui-core`

- Extend `AgentConfig` with artifact retention settings, for example:
  - max age
  - max bytes and/or max count
  - cleanup interval
- Extend `ArtifactStore` with:
  - startup sweep
  - periodic cleanup
  - deletion and metadata consistency guarantees
- Add runtime counters/state structures that the daemon can expose without scraping logs.

## Reconnect And Recovery Semantics

- Loss of daemon connectivity invalidates the current daemon stream owned by the worker.
- For read-only operations:
  - the worker may reconnect,
  - rebuild daemon session state from a fresh `initial_snapshot`,
  - and return explicit continuity metadata when previous locators or cached state had to be invalidated.
- For mutating operations:
  - the worker MUST NOT silently retry after reconnect,
  - the result must surface an explicit retry-needed or interrupted status,
  - and higher-level agents decide whether to re-issue the action.
- If reconnect changes daemon `session_id`, the logical MCP session may stay alive, but it must report that state continuity was broken if locators or cached element ids can no longer be trusted.

## Tooling And Contract Boundaries

- MCP tools should translate to existing daemon actions and snapshot reads wherever possible.
- The bridge must not create a second artifact store or duplicate daemon-side retention logic.
- Artifact bytes continue to come from the daemon artifact store; the bridge may stream them or wrap them for MCP, but the backing lifecycle remains daemon-owned.
- The bridge should prefer explicit typed mappings in Rust over stringly-typed JSON dispatch tables spread across multiple modules.

## Risks / Trade-offs

- MCP translation can duplicate transport model logic.
  - Mitigation: map MCP tools to the same action and snapshot semantics already defined by the daemon contract.
- Session duplication can cause redundant observer work and inconsistent caches.
  - Mitigation: keep a bounded registry of logical sessions and reuse workers by explicit `session_id`.
- Reconnect can accidentally replay writes.
  - Mitigation: make retry behavior asymmetric: reconnect for reads is allowed, silent replay for mutating actions is forbidden.
- Observability adds configuration and storage overhead.
  - Mitigation: keep the first metrics set small and focused on runtime quality signals.
- Metrics can explode in cardinality if keyed by element or window ids.
  - Mitigation: aggregate by reason/category, not by high-cardinality UI identifiers.

## Migration Plan

1. Add daemon-side runtime status and artifact retention primitives.
2. Implement `vmui-mcp-proxy` session registry and `DaemonSessionWorker`.
3. Add MCP session lifecycle tools and state-reading tools.
4. Add write-capable tools with explicit no-auto-retry safety.
5. Add stdio integration tests and daemon-side hardening tests.

## Quality Gates

- Repeated MCP calls against the same logical session reuse one daemon stream and preserve cached state.
- Read-only reconnect paths are explicit and tested.
- Mutating actions are never silently retried after reconnect.
- Artifact cleanup updates both files on disk and in-memory metadata consistently.
- Runtime quality can be queried through a structured surface, not only logs.

## Open Questions

- Whether the first runtime status surface should be represented as daemon actions, extra RPC methods, or both.
- Whether the first MCP slice should expose write tools immediately or start as read-only plus diagnostics.
