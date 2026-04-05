#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_dir/common.sh"

if [[ "${1:-}" == "--help" ]]; then
    echo "usage: ./scripts/vm/restart-agent.sh"
    exit 0
fi

vmui_vm_load_env

task_ps="$(vmui_vm_quote_ps "$VMUI_VM_SESSION_TASK")"
vmui_vm_run_powershell "\$ErrorActionPreference = 'Stop'
& schtasks.exe /run /tn $task_ps
& schtasks.exe /query /tn $task_ps /fo list /v
"
