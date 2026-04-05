# Change: Add remote Windows VM control plane

## Why

The product depends on a live interactive Windows VM session, but the repository currently has no explicit control-plane contract for how a Linux/WSL host should reach that VM, deploy current changes, restart the daemon in the correct session, and run smoke checks. As a result, each new Codex session must rediscover the target VM, the access pattern, and the boundary between non-interactive administration and interactive desktop execution.

## What Changes

- Define a repo-visible remote control plane for the dedicated Windows VM at `192.168.32.142`.
- Standardize SSH-first administration, loopback-only daemon exposure, and local MCP access over an SSH tunnel.
- Define the required interactive-session bridge for daemon restart and GUI smoke execution.
- Add checked-in operator docs so future Codex sessions can immediately recover the VM context and the intended deploy/test workflow.
- Plan follow-up helper scripts and Windows-side bootstrap steps needed to implement the design safely.

## Impact

- Affected specs: `remote-vm-control-plane`
- Affected docs: `AGENTS.md`, `docs/index.md`, `docs/dev-runbook.md`, `docs/windows-vm-access.md`
- Affected future code/ops: `scripts/` helper surface, Windows scheduled tasks, VM bootstrap steps, optional SSH tunnel supervision
