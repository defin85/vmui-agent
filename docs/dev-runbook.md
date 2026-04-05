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
- Default artifact retention:
  24h max age, 256 MiB max bytes, 512 artifacts, 5 minute periodic cleanup
- Stop the daemon with `Ctrl-C`.

## Running The MCP Proxy

- Start the proxy against the default daemon:
  `RUST_LOG=info cargo run -p vmui-mcp-proxy`
- Override the daemon endpoint when needed:
  `VMUI_DAEMON_ADDR=http://127.0.0.1:50051 RUST_LOG=info cargo run -p vmui-mcp-proxy`
- Current behavior:
  starts a `stdio` MCP server with explicit logical sessions (`session_open`, `session_status`, `session_close`), daemon session reuse, read-only reconnect, and no silent retry for mutating tools.

## Host Expectations

- Linux host:
  `cargo check --workspace` and `cargo test --workspace` are expected to pass.
- Linux host runtime:
  the workspace can start and validate non-Windows paths, but live Windows observation and semantic desktop actions are unavailable.
- Windows VM runtime:
  run `vmui-agent` inside the interactive desktop session, not in Session 0.

## Remote Windows VM

- Current remote test VM recorded on 2026-04-05:
  `192.168.32.142`
- Read `docs/windows-vm-access.md` before remote deploy/test or Windows VM bootstrap work.
- Use `docs/windows-vm-bootstrap.md` for concrete Windows-side bootstrap commands.
- Treat SSH administration and desktop-capable execution as different planes.
- Prefer an SSH local forward to the VM loopback daemon endpoint and run `vmui-mcp-proxy` locally on the Linux host.
- For the canonical generic desktop smoke on the VM, use:
  `./scripts/vm/notepad-smoke.sh`
- For a live out-of-process probe of the selected Configurator surface, run:
  `VMUI_REMOTE_SCOPE=attached_windows VMUI_REMOTE_DOMAIN_PROFILE=onec_configurator VMUI_REMOTE_PROCESS_NAME=1cv8.exe VMUI_REMOTE_PANEL_PROBE=1 VMUI_REMOTE_PANEL_PROBE_PATH=var/tmp/panel-probe.json cargo run -p vmui-mcp-proxy --example remote_session_smoke`

## Changing Runtime Defaults

- Bind address, artifact dir, and default profile live in `crates/vmui-core/src/lib.rs`.
- If you change those defaults, update:
  `README.md`
  `docs/dev-runbook.md`
  any relevant layer-specific `AGENTS.md`

## Codex Setup

- For the end-to-end Codex workflow, read `docs/codex-workflow.md` and `docs/codex-setup.md`.
- `.codex/config.toml` is an optional local optimization layer, not a required part of the repository runtime.
- The checked-in config assumes a machine with local `claude-context`, Ollama, and Milvus services.
- If that stack is unavailable, either disable the MCP entry or replace it with a setup available on the current machine.
