#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_dir/common.sh"

if [[ "${1:-}" == "--help" ]]; then
    echo "usage: ./scripts/vm/build.sh [cargo args ...]"
    echo "default: ./scripts/vm/build.sh build --workspace"
    exit 0
fi

vmui_vm_load_env

if [[ $# -eq 0 ]]; then
    set -- build --workspace
fi

workdir_ps="$(vmui_vm_quote_ps "$VMUI_VM_WORKDIR")"
cargo_args_ps="$(vmui_vm_join_ps_args "$@")"

vmui_vm_run_powershell "\$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $workdir_ps
& cargo $cargo_args_ps
"
