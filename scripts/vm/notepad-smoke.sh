#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_dir/common.sh"

text=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --text)
            [[ $# -ge 2 ]] || vmui_vm_die "--text requires a value"
            text="$2"
            shift 2
            ;;
        --help)
            echo "usage: ./scripts/vm/notepad-smoke.sh [--text '<text>']"
            exit 0
            ;;
        *)
            vmui_vm_die "unknown argument: $1"
            ;;
    esac
done

vmui_vm_load_env

repo_root="$(vmui_vm_repo_root)"
token="$(date +%Y%m%d-%H%M%S)-$RANDOM"
text="${text:-codex-smoke-$token}"
start_task="vmui-notepad-start-$token"
clipboard_task="vmui-notepad-clipboard-$token"
remote_log_dir="$VMUI_VM_WORKDIR/var/log"
pid_file="$remote_log_dir/notepad-smoke-$token.pid"
clipboard_file="$remote_log_dir/notepad-smoke-$token.clipboard.txt"
clipboard_error_file="$remote_log_dir/notepad-smoke-$token.clipboard.err.txt"
start_script="$remote_log_dir/notepad-start-$token.ps1"
clipboard_script="$remote_log_dir/notepad-clipboard-$token.ps1"

cleanup_remote() {
    set +e
    local start_task_ps clipboard_task_ps start_script_ps clipboard_script_ps pid_file_ps clipboard_file_ps clipboard_error_file_ps
    start_task_ps="$(vmui_vm_quote_ps "$start_task")"
    clipboard_task_ps="$(vmui_vm_quote_ps "$clipboard_task")"
    start_script_ps="$(vmui_vm_quote_ps "$start_script")"
    clipboard_script_ps="$(vmui_vm_quote_ps "$clipboard_script")"
    pid_file_ps="$(vmui_vm_quote_ps "$pid_file")"
    clipboard_file_ps="$(vmui_vm_quote_ps "$clipboard_file")"
    clipboard_error_file_ps="$(vmui_vm_quote_ps "$clipboard_error_file")"
    vmui_vm_run_powershell "\$ErrorActionPreference = 'SilentlyContinue'
Unregister-ScheduledTask -TaskName $start_task_ps -Confirm:\$false | Out-Null
Unregister-ScheduledTask -TaskName $clipboard_task_ps -Confirm:\$false | Out-Null
Remove-Item -LiteralPath $start_script_ps,$clipboard_script_ps,$pid_file_ps,$clipboard_file_ps,$clipboard_error_file_ps -Force | Out-Null
" >/dev/null 2>&1 || true
}

trap cleanup_remote EXIT

echo "stopping any existing interactive vmui-agent task"
"$script_dir/powershell.sh" "schtasks.exe /end /tn '$VMUI_VM_SESSION_TASK'; Get-Process cargo,vmui-agent,notepad -ErrorAction SilentlyContinue | Stop-Process -Force" >/dev/null 2>&1 || true

echo "restarting interactive vmui-agent on VM"
"$script_dir/restart-agent.sh"

if command -v ss >/dev/null 2>&1; then
    while read -r pid; do
        [[ -n "$pid" ]] || continue
        kill "$pid" >/dev/null 2>&1 || true
    done < <(
        ss -ltnp "sport = :${VMUI_VM_LOCAL_FORWARD_PORT}" 2>/dev/null |
            sed -n 's/.*pid=\([0-9][0-9]*\).*/\1/p' |
            sort -u
    )
fi

echo "opening fresh local SSH tunnel to VM daemon"
"$script_dir/tunnel.sh" --background

echo "waiting for vmui-agent to listen on ${VMUI_VM_DAEMON_HOST}:${VMUI_VM_DAEMON_PORT}"
cat <<EOF | "$script_dir/powershell.sh" >/dev/null
\$ErrorActionPreference = 'Stop'
for (\$i = 0; \$i -lt 60; \$i++) {
    \$listener = Get-NetTCPConnection -State Listen -LocalAddress ${VMUI_VM_DAEMON_HOST} -LocalPort ${VMUI_VM_DAEMON_PORT} -ErrorAction SilentlyContinue
    if (\$listener) {
        exit 0
    }
    Start-Sleep -Seconds 1
}
throw 'vmui-agent did not start listening on ${VMUI_VM_DAEMON_HOST}:${VMUI_VM_DAEMON_PORT} in time'
EOF

start_task_ps="$(vmui_vm_quote_ps "$start_task")"
clipboard_task_ps="$(vmui_vm_quote_ps "$clipboard_task")"
remote_log_dir_ps="$(vmui_vm_quote_ps "$remote_log_dir")"
pid_file_ps="$(vmui_vm_quote_ps "$pid_file")"
clipboard_file_ps="$(vmui_vm_quote_ps "$clipboard_file")"
clipboard_error_file_ps="$(vmui_vm_quote_ps "$clipboard_error_file")"
start_script_ps="$(vmui_vm_quote_ps "$start_script")"
clipboard_script_ps="$(vmui_vm_quote_ps "$clipboard_script")"
automation_user="${VMUI_VM_SSH_USER:-}"
if [[ -z "$automation_user" && "${VMUI_VM_SSH_TARGET:-}" == *"@"* ]]; then
    automation_user="${VMUI_VM_SSH_TARGET%@*}"
fi
[[ -n "$automation_user" ]] || vmui_vm_die "set VMUI_VM_SSH_USER or a user-qualified VMUI_VM_SSH_TARGET before running notepad-smoke.sh"
automation_user_ps="$(vmui_vm_quote_ps "$automation_user")"

echo "launching Notepad in the interactive Windows session"
vmui_vm_run_powershell "\$ErrorActionPreference = 'Stop'
\$pwsh = (Get-Command pwsh -ErrorAction Stop).Source
New-Item -Force -ItemType Directory -Path $remote_log_dir_ps | Out-Null
@'
\$p = Start-Process notepad.exe -PassThru
Set-Content -LiteralPath '$pid_file' -Value \$p.Id
'@ | Set-Content -LiteralPath $start_script_ps
\$action = New-ScheduledTaskAction -Execute \$pwsh -Argument \"-NoLogo -NoProfile -ExecutionPolicy Bypass -File $start_script\"
\$principal = New-ScheduledTaskPrincipal -UserId $automation_user_ps -LogonType Interactive -RunLevel Highest
\$trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(10)
Register-ScheduledTask -Force -TaskName $start_task_ps -Action \$action -Trigger \$trigger -Principal \$principal | Out-Null
Start-ScheduledTask -TaskName $start_task_ps
for (\$i = 0; \$i -lt 50; \$i++) {
    if (Test-Path -LiteralPath $pid_file_ps) { break }
    Start-Sleep -Milliseconds 200
}
if (-not (Test-Path -LiteralPath $pid_file_ps)) {
    throw 'interactive Notepad launch did not produce a pid file'
}
" >/dev/null

sleep 2

echo "typing smoke text through daemon session"
VMUI_DAEMON_ADDR="http://127.0.0.1:${VMUI_VM_LOCAL_FORWARD_PORT}" \
    cargo run -q -p vmui-mcp-proxy --example remote_notepad_smoke -- "$text"

echo "copying text from Notepad clipboard in the interactive session"
vmui_vm_run_powershell "\$ErrorActionPreference = 'Stop'
\$pwsh = (Get-Command pwsh -ErrorAction Stop).Source
@'
try {
    Remove-Item -LiteralPath '$clipboard_error_file' -Force -ErrorAction SilentlyContinue
    \$notepadPid = [int](Get-Content -LiteralPath '$pid_file' | Select-Object -First 1)
    \$shell = New-Object -ComObject WScript.Shell
    if (-not \$shell.AppActivate(\$notepadPid)) {
        throw \"failed to activate notepad pid \$notepadPid\"
    }
    Start-Sleep -Milliseconds 600
    \$shell.SendKeys('^a')
    Start-Sleep -Milliseconds 300
    \$shell.SendKeys('^c')
    Start-Sleep -Milliseconds 800
    (Get-Clipboard) | Set-Content -LiteralPath '$clipboard_file'
    Stop-Process -Id \$notepadPid
} catch {
    (\$_ | Out-String) | Set-Content -LiteralPath '$clipboard_error_file'
    throw
}
'@ | Set-Content -LiteralPath $clipboard_script_ps
\$action = New-ScheduledTaskAction -Execute \$pwsh -Argument \"-NoLogo -NoProfile -ExecutionPolicy Bypass -File $clipboard_script\"
\$principal = New-ScheduledTaskPrincipal -UserId $automation_user_ps -LogonType Interactive -RunLevel Highest
\$trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(10)
Register-ScheduledTask -Force -TaskName $clipboard_task_ps -Action \$action -Trigger \$trigger -Principal \$principal | Out-Null
Start-ScheduledTask -TaskName $clipboard_task_ps
for (\$i = 0; \$i -lt 100; \$i++) {
    if (Test-Path -LiteralPath $clipboard_file_ps) { break }
    Start-Sleep -Milliseconds 200
}
if (-not (Test-Path -LiteralPath $clipboard_file_ps)) {
    if (Test-Path -LiteralPath $clipboard_error_file_ps) {
        \$details = Get-Content -LiteralPath $clipboard_error_file_ps -Raw
        throw \"interactive clipboard verification failed: \$details\"
    }
    throw 'interactive clipboard verification did not produce an output file'
}
" >/dev/null

clipboard_text="$("$script_dir/powershell.sh" "Get-Content -LiteralPath $clipboard_file_ps")"
clipboard_text="${clipboard_text//$'\r'/}"

if [[ "$clipboard_text" != "$text" ]]; then
    echo "expected: $text" >&2
    echo "actual:   $clipboard_text" >&2
    vmui_vm_die "remote Notepad smoke verification failed"
fi

echo "remote Notepad smoke passed"
echo "typed_text=$text"
