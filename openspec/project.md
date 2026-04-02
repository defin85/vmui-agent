# Project Context

## Purpose
Build a Rust-based Windows UI agent that runs inside a dedicated Windows 10 VM and provides a stateful, debugger-like automation and diagnostics layer for 1C UI and Configurator workflows.

The project is not meant to replace standard 1C automated testing. It complements it by providing:

- live Windows UI state inspection;
- navigation and semi-automation inside 1C Configurator;
- post-failure diagnostics after test crashes;
- targeted screenshot/OCR fallback only when UIA/MSAA are insufficient.

## Tech Stack
- Rust 2021 workspace
- Tokio async runtime
- gRPC/protobuf transport around a long-lived bidirectional session
- Windows UI Automation, WinEvent, MSAA/IAccessible on the target Windows VM
- Optional MCP proxy for external agent integration
- Beads for issue/dependency tracking
- OpenSpec for spec-first planning and change control

## Project Conventions

### Code Style
- Keep the default code style compatible with `cargo fmt`.
- Prefer small, explicit crate boundaries over large mixed modules.
- Keep Windows-only code behind `cfg(windows)` or in the dedicated Windows backend crate.
- Use ASCII unless an existing file already requires Unicode.
- Favor transport-agnostic domain models in `vmui-protocol` and keep platform side effects isolated.

### Architecture Patterns
- One long-lived agent process runs inside the interactive Windows VM session.
- The system is event-driven: `initial_snapshot` plus `diff_batch`, not screenshot polling.
- UIA is primary, WinEvent/MSAA is secondary, OCR/screenshot is fallback-only.
- MCP is an adapter over the core daemon transport, not the internal source of truth.
- Stable identifiers are session-stable ids plus reusable locators, not permanent `RuntimeId` assumptions.

### Testing Strategy
- Keep `cargo check --workspace` and `cargo test --workspace` green on the Linux development host.
- Add unit tests for protocol, state, diff and locator logic as they are introduced.
- Keep Windows runtime specifics isolated enough that non-Windows hosts can still validate most of the workspace.
- Validate OpenSpec artifacts with `openspec validate --strict --no-interactive`.

### Git Workflow
- The repository uses git with a `main` default branch.
- Keep changes focused and reviewable.
- Do not assume remote push rights from inside the agent workflow.
- Use Beads for operational issue tracking instead of ad hoc TODO tracking.

## Domain Context
- The target environment is a dedicated Windows 10 VM used for UI automation only.
- The host desktop must never be automated or affected.
- The main product scenarios are 1C application UI inspection, Configurator navigation, and diagnostics after standard 1C test failures.
- Ordinary forms and some Configurator surfaces may expose weak accessibility metadata and require explicit fallback handling.

## Important Constraints
- The automation runtime must run in the interactive VM desktop session, not in Session 0.
- The design should minimize external dependencies, but not at the expense of runtime stability.
- External agents should not need full screenshot polling on every step.
- Windows security boundaries such as UIPI and integrity levels are real constraints.
- Some 1C controls may be custom or owner-drawn and may not expose full semantic trees.

## External Dependencies
- Windows UI Automation and Win32 accessibility APIs
- 1C:Enterprise application UI and Configurator UI inside the target VM
- Optional OCR engine for fallback capture flows
- Optional MCP clients that connect through a thin proxy
