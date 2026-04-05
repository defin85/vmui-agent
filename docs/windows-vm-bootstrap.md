# Windows VM Bootstrap Commands

## Status

- Operator-run bootstrap commands for the dedicated Windows VM.
- These commands are concrete, but they are not automatically executed by the repository.
- Fill placeholders before use:
  - `<AUTOMATION_USER>`
  - `<AUTOMATION_PASSWORD>`
  - `<LINUX_PUBLIC_KEY_LINE>`

## Assumptions

- Run the PowerShell snippets below in an elevated PowerShell session on the Windows VM.
- Target VM recorded on 2026-04-05:
  `192.168.32.142`
- Recommended repo workdir:
  `C:\vmui-agent`

## 1. Enable OpenSSH Server

```powershell
Get-WindowsCapability -Online | Where-Object Name -like 'OpenSSH*'
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Start-Service sshd
Set-Service -Name sshd -StartupType Automatic
if (!(Get-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -DisplayName 'OpenSSH Server (sshd)' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22
}
```

## 2. Configure PowerShell 7 As The SSH Subsystem

This assumes `pwsh.exe` is installed at `C:\Program Files\PowerShell\7\pwsh.exe`.

```powershell
$sshdConfig = "$env:ProgramData\ssh\sshd_config"
$subsystem = 'Subsystem powershell C:/progra~1/powershell/7/pwsh.exe -sshs -NoLogo'
$current = Get-Content -LiteralPath $sshdConfig -ErrorAction Stop
$filtered = $current | Where-Object { $_ -notmatch '^\s*Subsystem\s+powershell\b' }
Set-Content -LiteralPath $sshdConfig -Value ($filtered + $subsystem)
Restart-Service sshd
```

## 3. Install SSH Public Key For The Administrative Account

Use the single-line public key from the Linux host, not the private key.

```powershell
$authorizedKeys = "$env:ProgramData\ssh\administrators_authorized_keys"
New-Item -Force -ItemType File -Path $authorizedKeys | Out-Null
Add-Content -LiteralPath $authorizedKeys -Value '<LINUX_PUBLIC_KEY_LINE>'
icacls.exe "$authorizedKeys" /inheritance:r /grant "Administrators:F" /grant "SYSTEM:F"
Restart-Service sshd
```

## 4. Create The Repo Workdir And Runtime Directories

```powershell
New-Item -Force -ItemType Directory -Path 'C:\vmui-agent' | Out-Null
New-Item -Force -ItemType Directory -Path 'C:\vmui-agent\var\artifacts' | Out-Null
New-Item -Force -ItemType Directory -Path 'C:\vmui-agent\var\log' | Out-Null
```

## 5. Install Base Tooling

The exact installer source is an operator choice. After installation, the following commands must succeed in a fresh PowerShell session:

```powershell
pwsh.exe -Version
git --version
cargo --version
rustup show
cl.exe
link.exe
```

## 6. Create The Interactive Daemon Task

This task is intentionally interactive-only so the daemon stays in the logged-on desktop session.

```powershell
$pwsh = (Get-Command pwsh -ErrorAction Stop).Source
$action = New-ScheduledTaskAction -Execute $pwsh -Argument "-NoLogo -NoProfile -ExecutionPolicy Bypass -File C:\vmui-agent\scripts\vm\windows\start-vmui-agent.ps1"
$trigger = New-ScheduledTaskTrigger -AtLogOn -User "<AUTOMATION_USER>"
$principal = New-ScheduledTaskPrincipal -UserId "<AUTOMATION_USER>" -LogonType InteractiveToken -RunLevel Highest
Register-ScheduledTask -Force -TaskName "vmui-agent-session" -Action $action -Trigger $trigger -Principal $principal
```

## 7. Create The Interactive Smoke Task

This is a placeholder smoke task until a repo-tracked Windows smoke script is added.

```powershell
$pwsh = (Get-Command pwsh -ErrorAction Stop).Source
$action = New-ScheduledTaskAction -Execute $pwsh -Argument "-NoLogo -NoProfile -ExecutionPolicy Bypass -File C:\vmui-agent\scripts\vm\windows\run-vmui-smoke.ps1"
$principal = New-ScheduledTaskPrincipal -UserId "<AUTOMATION_USER>" -LogonType InteractiveToken -RunLevel Highest
$trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).Date.AddYears(10)
Register-ScheduledTask -Force -TaskName "vmui-smoke" -Action $action -Trigger $trigger -Principal $principal
```

## 8. Verify From The Linux Host

After the VM bootstrap is done, the intended host-side flow is:

```bash
cp scripts/vm/vm.env.example .vmui-vm.env
./scripts/vm/ssh.sh
./scripts/vm/sync.sh --worktree
./scripts/vm/build.sh check --workspace
./scripts/vm/restart-agent.sh
./scripts/vm/tunnel.sh --background
VMUI_DAEMON_ADDR=http://127.0.0.1:50051 cargo run -p vmui-mcp-proxy
```

## References

- `docs/windows-vm-access.md`
- `scripts/vm/vm.env.example`
- `scripts/vm/ssh.sh`
- `scripts/vm/sync.sh`
- `scripts/vm/build.sh`
- `scripts/vm/restart-agent.sh`
- `scripts/vm/tunnel.sh`
