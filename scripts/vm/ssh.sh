#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_dir/common.sh"

if [[ "${1:-}" == "--help" ]]; then
    echo "usage: ./scripts/vm/ssh.sh [remote command ...]"
    exit 0
fi

vmui_vm_load_env

if [[ $# -eq 0 ]]; then
    vmui_vm_ssh
else
    vmui_vm_ssh "$@"
fi
