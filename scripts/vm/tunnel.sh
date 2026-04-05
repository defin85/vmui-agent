#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_dir/common.sh"

background=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --background)
            background=1
            shift
            ;;
        --help)
            echo "usage: ./scripts/vm/tunnel.sh [--background]"
            exit 0
            ;;
        *)
            vmui_vm_die "unknown argument: $1"
            ;;
    esac
done

vmui_vm_load_env

local_spec="${VMUI_VM_LOCAL_FORWARD_PORT}:${VMUI_VM_DAEMON_HOST}:${VMUI_VM_DAEMON_PORT}"
target="$(vmui_vm_ssh_target)"
ssh_args=()

if [[ -n "${VMUI_VM_SSH_CONFIG_FILE:-}" ]]; then
    ssh_args+=(-F "$VMUI_VM_SSH_CONFIG_FILE")
fi
if [[ -n "${VMUI_VM_SSH_IDENTITY_FILE:-}" ]]; then
    ssh_args+=(-i "$VMUI_VM_SSH_IDENTITY_FILE")
fi
if [[ -z "${VMUI_VM_SSH_TARGET:-}" ]]; then
    ssh_args+=(-p "$VMUI_VM_SSH_PORT")
fi
if (( background )); then
    ssh_args+=(-f)
fi
ssh_args+=(-N -L "$local_spec" "$target")

echo "opening SSH tunnel localhost:${VMUI_VM_LOCAL_FORWARD_PORT} -> ${VMUI_VM_DAEMON_HOST}:${VMUI_VM_DAEMON_PORT} via $target"
ssh "${ssh_args[@]}"
