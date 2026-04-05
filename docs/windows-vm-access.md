# Remote Windows VM Access Design

## Status

- Operator-context document plus the current checked-in control-plane contract.
- Current target Windows VM recorded on 2026-04-05:
  `192.168.32.142`
- Current VM bootstrap state on 2026-04-05:
  SSH key access, PowerShell 7, `C:\vmui-agent`, `vmui-agent-session`, and `vmui-smoke` are configured.
- Use this document before remote deploy, smoke, or VM bootstrap work.

## Goal

Provide a repeatable control plane from the Linux/WSL host to the dedicated Windows VM so Codex can deploy, restart, inspect, and test changes without violating the requirement that UI automation runs only inside the interactive Windows desktop session.

## Recommended Runtime Shape

```text
Codex on Linux / WSL
        |
        +--> ssh / scp / PowerShell-over-SSH --> Windows VM service plane
        |                                         - OpenSSH Server
        |                                         - PowerShell 7
        |                                         - Rust/MSVC toolchain
        |                                         - repo worktree
        |
        +--> ssh -L 50051:127.0.0.1:50051 ------> vmui-agent gRPC loopback
        |                                         - daemon stays bound to VM loopback
        |
        +--> local vmui-mcp-proxy --------------> tunneled daemon session
        |
        +--> schtasks /run ---------------------> Windows interactive plane
                                                  - logged-on automation user
                                                  - vmui-agent-session task
                                                  - vmui-smoke task(s)
```

## Control Planes

### Linux control plane

- Keeps the authoritative git workspace.
- Owns SSH keys, host aliases, and any future `scripts/vm/*` helpers.
- Runs `vmui-mcp-proxy` locally against an SSH-forwarded daemon port.
- Pulls logs and artifacts back from the VM after runs.

### Windows service plane

- Exposes administrative access through OpenSSH.
- Runs non-GUI work such as file sync, `git fetch`, build, log inspection, and scheduled-task control.
- Must not be treated as a desktop-capable session for UI Automation work.

### Windows interactive plane

- Runs under a dedicated logged-on Windows user.
- Hosts `vmui-agent` and any GUI smoke scripts that need the real desktop.
- Is triggered through Task Scheduler or an equivalent interactive-only runner.

## Recommended Architecture Decisions

### 1. SSH first, not WinRM first

- Use OpenSSH Server on Windows as the default remote administration path.
- Prefer key-based authentication from Linux/WSL.
- Treat PowerShell over SSH as the structured command channel when shell commands alone are too brittle.

Rationale:

- The current development host is Linux/WSL.
- Microsoft documents that WSMan remoting is not supported on non-Windows platforms in the general case, while PowerShell remoting over SSH is supported cross-platform.

### 2. Keep the daemon loopback-bound on the VM

- Keep `vmui-agent` listening on `127.0.0.1:50051` inside the VM by default.
- Reach it from Linux through `ssh -L`, then run `vmui-mcp-proxy` locally.
- Do not expose the daemon directly on the LAN unless there is an explicit follow-up change for that risk.

Rationale:

- This keeps the gRPC surface off the network.
- The local proxy model already fits the repository architecture.

### 3. Split remote admin from desktop execution

- SSH is for build, sync, and orchestration.
- GUI-capable daemon start/restart and smoke jobs must run through a logged-on interactive user session.
- The preferred bridge is Windows Task Scheduler with interactive-only tasks.

Rationale:

- The repository requires the daemon to run in the interactive Windows session, not Session 0.
- An SSH service session is not a substitute for the interactive desktop.

### 4. Standardize the remote contract in repo-visible docs

- New sessions should not rediscover the VM address, the tunnel pattern, or the interactive-session constraint.
- The control-plane contract should live in checked-in docs and root-level agent instructions.

## Standardized Variables

The repository now includes `scripts/vm/vm.env.example`. Copy it to `.vmui-vm.env` and fill the machine-specific values.

```bash
VMUI_VM_HOST=192.168.32.142
VMUI_VM_SSH_PORT=22
VMUI_VM_SSH_USER=<fill-me>
VMUI_VM_WORKDIR='C:/vmui-agent'
VMUI_VM_DAEMON_ADDR=127.0.0.1:50051
VMUI_VM_SESSION_TASK=vmui-agent-session
VMUI_VM_SMOKE_TASK=vmui-smoke
```

## Repo-Tracked Helper Surface

Current scripts:

- `scripts/vm/ssh.sh`: open an SSH session or run one remote command
- `scripts/vm/powershell.sh`: run a PowerShell command or script over SSH
- `scripts/vm/tunnel.sh`: create an SSH local forward to the loopback daemon endpoint
- `scripts/vm/sync.sh`: deploy `HEAD` or the current worktree, including untracked non-ignored files, to the VM workdir
- `scripts/vm/build.sh`: run `cargo` in the VM workdir
- `scripts/vm/restart-agent.sh`: trigger the interactive daemon task
- `scripts/vm/run-smoke.sh`: trigger the interactive smoke task
- `scripts/vm/notepad-smoke.sh`: run the repo-tracked `Notepad -> focus_window -> send_keys -> clipboard verify` smoke against the interactive VM session
- `scripts/vm/pull-artifacts.sh`: download a zip of logs and artifacts from the VM
- `scripts/vm/windows/start-vmui-agent.ps1`: Windows-side task entrypoint for the daemon
- `scripts/vm/windows/run-vmui-smoke.ps1`: Windows-side task entrypoint for smoke logging

Example host-side flow:

```bash
cp scripts/vm/vm.env.example .vmui-vm.env
./scripts/vm/sync.sh --worktree
./scripts/vm/build.sh check --workspace
./scripts/vm/restart-agent.sh
./scripts/vm/tunnel.sh --background
VMUI_DAEMON_ADDR=http://127.0.0.1:50051 cargo run -p vmui-mcp-proxy
./scripts/vm/notepad-smoke.sh
./scripts/vm/pull-artifacts.sh --extract
```

### Live Configurator panel probe

Use the repo-tracked helper when Configurator is already open in the interactive VM session and you want aligned `HWND/UIA/MSAA/hit-test/capture` evidence for one selected surface.

Example:

```bash
./scripts/vm/tunnel.sh --background
VMUI_DAEMON_ADDR=http://127.0.0.1:50051 \
VMUI_REMOTE_SCOPE=attached_windows \
VMUI_REMOTE_DOMAIN_PROFILE=onec_configurator \
VMUI_REMOTE_PROCESS_NAME=1cv8.exe \
VMUI_REMOTE_PANEL_PROBE=1 \
VMUI_REMOTE_PANEL_PROBE_UIA_MAX_DEPTH=6 \
VMUI_REMOTE_PANEL_PROBE_MSAA_MAX_DEPTH=3 \
VMUI_REMOTE_PANEL_PROBE_PATH=var/tmp/panel-probe.json \
cargo run -p vmui-mcp-proxy --example remote_session_smoke
```

Notes:

- The helper returns `panel-probe-json`, which references the persisted per-layer artifacts.
- For the left Configurator tree, prefer an `attached_windows` session filter that resolves only the active `1cv8.exe` Configurator window before probing a narrower element or region target.

## Recommended Bootstrap On The Windows VM

1. Install and enable OpenSSH Server.
2. Install PowerShell 7 and configure the SSH PowerShell subsystem.
3. Configure key-based authentication for the chosen Windows account.
4. Install the Rust MSVC toolchain and any Windows build dependencies used by this repo.
5. Create a dedicated working directory such as `C:\vmui-agent`.
6. Create a dedicated automation user and keep that user logged into the VM desktop when GUI work is needed.
7. Create scheduled tasks:
   - `vmui-agent-session`: on-logon, interactive-only, starts or supervises the daemon
   - `vmui-smoke`: on-demand, interactive-only, runs GUI smoke scripts in the same user session
8. Store logs and artifacts under the repo workdir, for example `C:\vmui-agent\var\`.

Concrete commands live in:

- `docs/windows-vm-bootstrap.md`

## Recommended Host Workflow

1. Open or verify SSH access to `192.168.32.142`.
2. Sync the current repo snapshot to the VM workdir.
   - Preferred long-term shape:
     - `git fetch` / `git checkout <sha>` for committed revisions
     - archive or diff-based sync for uncommitted local changes
3. Build Windows-specific artifacts on the VM through SSH.
4. Trigger the interactive task that starts or restarts `vmui-agent`.
5. Open an SSH local forward from the host to `127.0.0.1:50051` on the VM.
6. Run `vmui-mcp-proxy` locally against the forwarded daemon port.
7. Execute smoke or exploratory checks.
   - Canonical repo-tracked smoke:
     `./scripts/vm/notepad-smoke.sh`
8. Pull logs and artifacts back to the host if a failure occurs.

## Invariants

- Never assume an SSH session can drive the Windows desktop.
- Never move the primary daemon runtime into Session 0 or a Windows service.
- Never expose the daemon on a LAN-reachable socket by default when an SSH tunnel is sufficient.
- Keep Linux-host verification green even when Windows-only deploy/test helpers are added.

## Explicit Non-Goals

- This document does not claim that remote deploy/test automation is implemented today.
- This document does not define credentials, private keys, or secrets.
- This document does not replace OpenSpec for the follow-up implementation change.

## External References

- Microsoft Learn, Get started with OpenSSH for Windows:
  https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse
- Microsoft Learn, Key-based authentication in OpenSSH for Windows:
  https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_keymanagement
- Microsoft Learn, PowerShell remoting over SSH:
  https://learn.microsoft.com/en-us/powershell/scripting/security/remoting/ssh-remoting-in-powershell?view=powershell-7.5
- Microsoft Learn, Using WS-Management (WSMan) remoting in PowerShell:
  https://learn.microsoft.com/en-us/powershell/scripting/security/remoting/wsman-remoting-in-powershell?view=powershell-5.1
- Microsoft Learn, `schtasks /create`:
  https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/schtasks-create
- Microsoft Learn, `schtasks /run`:
  https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/schtasks-run
