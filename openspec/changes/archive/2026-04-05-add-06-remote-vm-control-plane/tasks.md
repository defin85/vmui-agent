## 1. Linux host control plane

- [x] 1.1 Add repo-tracked helpers for SSH access, tunnel management, sync, restart, smoke execution, and artifact pull.
- [x] 1.2 Standardize the environment variable contract and optional SSH host alias used by those helpers.
- [x] 1.3 Keep host-side MCP usage local by default, with `vmui-mcp-proxy` pointed at the tunneled daemon endpoint.

## 2. Windows VM bootstrap

- [x] 2.1 Install and configure OpenSSH Server, PowerShell 7 over SSH, key-based authentication, and the Windows build toolchain.
- [x] 2.2 Provision a dedicated interactive Windows user and create the `vmui-agent-session` interactive task that starts or supervises the daemon after logon.
- [x] 2.3 Add one or more interactive smoke tasks and define stable log/artifact output paths under the repo workdir.

## 3. End-to-end validation

- [x] 3.1 Verify Linux-to-Windows SSH access, PowerShell-over-SSH, and `schtasks /run` control from the host.
- [x] 3.2 Verify daemon start/restart in the interactive session and host access through an SSH local forward to `127.0.0.1:50051`.
- [x] 3.3 Verify deploy -> local MCP proxy -> smoke execution -> artifact pull on the target VM.
- [x] 3.4 Run `./scripts/check-agent-docs.sh`.
- [x] 3.5 Run `openspec validate add-06-remote-vm-control-plane --strict --no-interactive`.
