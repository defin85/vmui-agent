#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_dir/common.sh"

if [[ "${1:-}" == "--help" ]]; then
    echo "usage: ./scripts/vm/powershell.sh '<PowerShell code>'"
    echo "   or: printf '%s\n' '<PowerShell code>' | ./scripts/vm/powershell.sh"
    exit 0
fi

vmui_vm_load_env

if [[ $# -gt 0 ]]; then
    vmui_vm_run_powershell "$*"
    exit 0
fi

if [[ -t 0 ]]; then
    vmui_vm_die "provide a PowerShell command argument or pipe a script on stdin"
fi

vmui_vm_run_powershell "$(cat)"
