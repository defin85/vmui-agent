# Development Runbook

## Toolchain

- Rust toolchain:
  `stable` with `rustfmt` and `clippy`
- Workspace minimum Rust version:
  `1.82`
- Canonical commands are `cargo ...`; `just ci` is only a local convenience wrapper.
- Run `./scripts/doctor.sh` on a new machine before assuming the repository setup is broken.
- If `just` is installed locally, `just doctor` runs the same readiness check.

## Verification

- Agent-facing docs freshness:
  `./scripts/check-agent-docs.sh`
- Format check:
  `cargo fmt --all --check`
- Build check:
  `cargo check --workspace`
- Test suite:
  `cargo test --workspace`
- OpenSpec validation when `openspec/` changes:
  `openspec validate --all --strict --no-interactive`
  or `openspec validate <change-id> --strict --no-interactive`

## Running The Daemon

- Start the daemon:
  `RUST_LOG=info cargo run -p vmui-agent`
- The current binary has no CLI flags; it uses `AgentConfig::default()`.
- Default bind address:
  `127.0.0.1:50051`
- Default artifact directory:
  `var/artifacts`
- Stop the daemon with `Ctrl-C`.

## Running The MCP Proxy Scaffold

- Start the proxy scaffold:
  `RUST_LOG=info cargo run -p vmui-mcp-proxy`
- Current behavior:
  logs startup, waits for `Ctrl-C`, and does not yet bridge MCP calls to the daemon transport.

## Host Expectations

- Linux host:
  `cargo check --workspace` and `cargo test --workspace` are expected to pass.
- Linux host runtime:
  the workspace can start and validate non-Windows paths, but live Windows observation and semantic desktop actions are unavailable.
- Windows VM runtime:
  run `vmui-agent` inside the interactive desktop session, not in Session 0.

## Changing Runtime Defaults

- Bind address, artifact dir, and default mode live in `crates/vmui-core/src/lib.rs`.
- If you change those defaults, update:
  `README.md`
  `docs/dev-runbook.md`
  any relevant layer-specific `AGENTS.md`

## Codex Setup

- For the end-to-end Codex workflow, read `docs/codex-workflow.md` and `docs/codex-setup.md`.
- `.codex/config.toml` is an optional local optimization layer, not a required part of the repository runtime.
- The checked-in config assumes a machine with local `claude-context`, Ollama, and Milvus services.
- If that stack is unavailable, either disable the MCP entry or replace it with a setup available on the current machine.
