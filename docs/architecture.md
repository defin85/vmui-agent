# Architecture

## Goal

Build a long-lived Windows UI agent that runs inside an interactive Windows 10 VM session and exposes a stateful debugger-like model of the UI for 1C application windows and Configurator.

## Recommended Runtime Shape

```text
external agent / CI / operator
        |
    MCP proxy (optional)
        |
   gRPC bidirectional stream
        |
      vmui-agent
        |
  +-----+-------------------+-------------------+
  |                         |                   |
session registry      UI state store       artifact store
  |                         |
  +-------------------------+
            |
  Windows backend facade
            |
  +---------+------------+-----------------+
  |                      |                 |
UIA observer thread  WinEvent hook    MSAA fallback
  |                      |                 |
  +----------> diff normalizer <-----------+
                     |
              action executor
                     |
              capture/OCR fallback
```

## Key Decisions

### One in-VM daemon

- One daemon owns the UI session, state cache, artifact store and action executor.
- The daemon must run in the interactive desktop session inside the VM.
- A Windows Service may supervise startup, but it must not own the UI automation runtime.

### Transport

- Core transport: gRPC streaming for typed bidirectional control and event delivery.
- MCP stays a thin proxy process so that the daemon keeps a stable internal contract and can also serve non-MCP clients.

### State Model

- Snapshot-first, then diff stream.
- Session-stable element ids plus reusable locators.
- Event notifications are hints; targeted refresh remains the source of truth.

### Fallback Strategy

- Primary: UI Automation.
- Secondary: WinEvent + MSAA/IAccessible.
- Tertiary: region capture and OCR for opaque or owner-drawn surfaces only.

## Crate Map

- `vmui-protocol`: serializable messages, snapshots, diffs, locators, action models.
- `vmui-core`: in-memory state cache, revision handling, config and session metadata.
- `vmui-platform`: trait for pluggable UI backends.
- `vmui-platform-windows`: Windows implementation and host capability detection.
- `vmui-agent`: daemon bootstrap and lifecycle.
- `vmui-mcp-proxy`: MCP facade over the daemon transport.

## Non-Goals For The Scaffold

- No screenshot polling loop.
- No embedded OCR dependency in the hot path.
- No attempt to keep `RuntimeId` as a permanent identifier across restarts.
