## Context

The repository already distinguishes between Linux-host development and Windows-VM runtime, but that split is still implicit. The product cannot be validated end-to-end without reaching a dedicated Windows desktop session, yet the current repo instructions stop at local commands. The control plane must work from Linux/WSL, preserve the interactive-session constraint, and be explicit enough that a new Codex session does not have to infer the environment from memory.

## Goals / Non-Goals

- Goals:
  - Give the Linux/WSL host a stable, low-friction path to deploy and test against the Windows VM.
  - Keep the daemon runtime inside the interactive Windows desktop session.
  - Keep the daemon off the LAN by default.
  - Make the current VM target and workflow visible in repo-tracked docs and agent instructions.
  - Leave room for automation scripts without blocking on credential details in the first pass.
- Non-Goals:
  - Turning this planning change into a full infrastructure implementation in one step.
  - Moving the daemon into a Windows service or Session 0.
  - Replacing SSH with a Windows-only remoting protocol as the primary host-to-VM path.
  - Checking secrets, private keys, or machine-specific credentials into the repository.

## Decisions

- Decision: Use OpenSSH as the default host-to-VM administration path.
  - Alternatives considered:
    - WinRM / WSMan as the main remoting layer.
    - RDP-only manual operation.
  - Rationale: the working host is Linux/WSL, Microsoft documents cross-platform PowerShell remoting over SSH, and WSMan remoting is not the right default for non-Windows clients. SSH also gives one transport for shell, file copy, and port forwarding.

- Decision: Keep `vmui-agent` bound to VM loopback and access it through an SSH local forward.
  - Alternatives considered:
    - Bind the daemon to a LAN address on the VM.
    - Run the MCP proxy on the Windows VM and expose MCP remotely instead.
  - Rationale: loopback plus tunnel keeps the daemon off the network, aligns with the current `vmui-mcp-proxy` architecture, and lets Codex keep the MCP client side local to the Linux workspace.

- Decision: Separate the service plane from the interactive desktop plane.
  - Alternatives considered:
    - Start GUI-capable jobs directly from the SSH session.
    - Run the daemon under a Windows service and hope UI access remains available.
  - Rationale: the repository already states that UI automation must run inside the interactive Windows session. SSH is good for build/orchestration, but not the desktop boundary.

- Decision: Use Task Scheduler as the first interactive bridge.
  - Alternatives considered:
    - Sysinternals PsExec-style interactive injection.
    - A custom always-on Windows agent from day one.
  - Rationale: Task Scheduler is built in, scriptable over SSH, supports on-logon and interactive-only execution, and is enough to bootstrap the first deploy/test loop without inventing another control daemon first.

- Decision: Make the repo carry explicit operator context.
  - Alternatives considered:
    - Keep the VM address and workflow only in chat history or local shell aliases.
  - Rationale: a new Codex session already starts from `AGENTS.md` and `docs/index.md`. The remote VM context should be recoverable there immediately.

## Proposed Runtime Shape

```text
Linux / WSL host
  |
  +--> ssh / scp / pwsh over ssh --------------------------+
  |                                                        |
  |                                                Windows VM service plane
  |                                                - OpenSSH Server
  |                                                - PowerShell 7 SSH subsystem
  |                                                - repo workdir
  |                                                - build/log/artifact commands
  |
  +--> ssh -L 50051:127.0.0.1:50051 ----------------------> vmui-agent loopback endpoint
  |
  +--> local vmui-mcp-proxy ------------------------------> tunneled daemon session
  |
  +--> schtasks /run /tn vmui-agent-session --------------> interactive Windows desktop session
                                                           - logged-on automation user
                                                           - daemon / smoke jobs
```

## Component Design

### Linux host

- Remains the source of truth for the repo checkout.
- Owns future helper scripts under `scripts/vm/`.
- Uses SSH keys and a stable host alias or environment variables for the target VM.
- Runs `vmui-mcp-proxy` locally, not on the VM, unless a later change proves a remote proxy is necessary.

### Windows service plane

- Hosts OpenSSH Server and PowerShell 7.
- Accepts build, sync, inspection, and scheduled-task control commands.
- Does not own the GUI automation runtime itself.

### Windows interactive plane

- Uses a dedicated logged-on Windows user.
- Runs `vmui-agent` and GUI smoke flows through Task Scheduler or a compatible interactive runner.
- Writes artifacts and logs into a stable repo-local path so the host can pull them back over SSH.

## Deployment Model

- Short term:
  - support `git fetch` / `git checkout <sha>` on the VM for committed work;
  - support archive- or diff-based sync for local uncommitted changes when needed.
- Long term:
  - hide those transport details behind repo-tracked helper scripts.

This split avoids forcing every iteration through a remote push while still allowing a reproducible deployment of exact commits.

## Validation Model

- Linux remains responsible for `cargo check --workspace` and `cargo test --workspace`.
- Windows VM validation covers:
  - Windows build sanity;
  - daemon start in the interactive desktop session;
  - host-side SSH tunnel to the loopback daemon;
  - local MCP access through the tunnel;
  - GUI smoke execution and artifact retrieval.

## Risks / Trade-offs

- The VM may be reachable over SSH while no interactive user session is logged in.
  - Mitigation: make interactive-session presence an explicit preflight check for GUI work.
- SSH key setup can drift from machine to machine.
  - Mitigation: document one env-var contract and one expected SSH host alias layout.
- Task Scheduler is enough for bootstrap, but it is not a rich job orchestration system.
  - Mitigation: start with scheduled tasks and only add a custom runner if the task interface becomes too coarse.
- Using loopback plus tunnel adds one extra process hop.
  - Mitigation: the security boundary and local-MCP ergonomics are worth the small operational overhead.
- Two deployment modes (`git checkout` and local diff sync) can diverge.
  - Mitigation: define a single wrapper command surface in repo-tracked scripts when implementation begins.

## Migration Plan

1. Record the current VM target and control-plane design in repo docs.
2. Add a new OpenSpec capability for the remote VM control plane.
3. Implement the Linux helper surface for SSH, tunnel, sync, restart, and artifact pull.
4. Bootstrap the Windows VM with OpenSSH, PowerShell 7 SSH remoting, toolchain, and interactive scheduled tasks.
5. Add end-to-end validation for deploy -> restart -> tunnel -> local MCP -> smoke -> artifact pull.

## Quality Gates

- A fresh Codex session can find the current VM target and remote workflow from checked-in docs alone.
- The default remote path from Linux to Windows uses SSH and does not require LAN exposure of the daemon.
- GUI-capable jobs are explicitly routed into a logged-on interactive Windows session.
- The deploy/test loop is repeatable enough to be scripted in-repo.

## Open Questions

- Which Windows user account should own the long-lived interactive session.
- Whether the first sync helper should prefer `git archive`, `scp`, or a remote `git fetch` workflow for uncommitted local changes.
- Whether SSH tunnel persistence should be managed with `autossh`, `systemd --user`, or a simple foreground helper script on WSL.
