#!/usr/bin/env bash

set -euo pipefail

vmui_vm_repo_root() {
    cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd
}

vmui_vm_die() {
    echo "error: $*" >&2
    exit 1
}

vmui_vm_load_env() {
    local repo_root env_file
    local host_override="${VMUI_VM_HOST-__unset__}"
    local ssh_port_override="${VMUI_VM_SSH_PORT-__unset__}"
    local ssh_user_override="${VMUI_VM_SSH_USER-__unset__}"
    local ssh_target_override="${VMUI_VM_SSH_TARGET-__unset__}"
    local ssh_config_override="${VMUI_VM_SSH_CONFIG_FILE-__unset__}"
    local ssh_identity_override="${VMUI_VM_SSH_IDENTITY_FILE-__unset__}"
    local workdir_override="${VMUI_VM_WORKDIR-__unset__}"
    local powershell_override="${VMUI_VM_POWERSHELL_EXE-__unset__}"
    local daemon_host_override="${VMUI_VM_DAEMON_HOST-__unset__}"
    local daemon_port_override="${VMUI_VM_DAEMON_PORT-__unset__}"
    local local_forward_override="${VMUI_VM_LOCAL_FORWARD_PORT-__unset__}"
    local session_task_override="${VMUI_VM_SESSION_TASK-__unset__}"
    local smoke_task_override="${VMUI_VM_SMOKE_TASK-__unset__}"
    local artifact_dir_override="${VMUI_VM_ARTIFACT_DIR-__unset__}"
    local log_dir_override="${VMUI_VM_LOG_DIR-__unset__}"
    repo_root="$(vmui_vm_repo_root)"
    env_file="${VMUI_VM_ENV_FILE:-$repo_root/.vmui-vm.env}"

    if [[ -f "$env_file" ]]; then
        # shellcheck disable=SC1090
        source "$env_file"
    fi

    [[ "$host_override" != "__unset__" ]] && VMUI_VM_HOST="$host_override"
    [[ "$ssh_port_override" != "__unset__" ]] && VMUI_VM_SSH_PORT="$ssh_port_override"
    [[ "$ssh_user_override" != "__unset__" ]] && VMUI_VM_SSH_USER="$ssh_user_override"
    [[ "$ssh_target_override" != "__unset__" ]] && VMUI_VM_SSH_TARGET="$ssh_target_override"
    [[ "$ssh_config_override" != "__unset__" ]] && VMUI_VM_SSH_CONFIG_FILE="$ssh_config_override"
    [[ "$ssh_identity_override" != "__unset__" ]] && VMUI_VM_SSH_IDENTITY_FILE="$ssh_identity_override"
    [[ "$workdir_override" != "__unset__" ]] && VMUI_VM_WORKDIR="$workdir_override"
    [[ "$powershell_override" != "__unset__" ]] && VMUI_VM_POWERSHELL_EXE="$powershell_override"
    [[ "$daemon_host_override" != "__unset__" ]] && VMUI_VM_DAEMON_HOST="$daemon_host_override"
    [[ "$daemon_port_override" != "__unset__" ]] && VMUI_VM_DAEMON_PORT="$daemon_port_override"
    [[ "$local_forward_override" != "__unset__" ]] && VMUI_VM_LOCAL_FORWARD_PORT="$local_forward_override"
    [[ "$session_task_override" != "__unset__" ]] && VMUI_VM_SESSION_TASK="$session_task_override"
    [[ "$smoke_task_override" != "__unset__" ]] && VMUI_VM_SMOKE_TASK="$smoke_task_override"
    [[ "$artifact_dir_override" != "__unset__" ]] && VMUI_VM_ARTIFACT_DIR="$artifact_dir_override"
    [[ "$log_dir_override" != "__unset__" ]] && VMUI_VM_LOG_DIR="$log_dir_override"

    export VMUI_VM_HOST="${VMUI_VM_HOST:-192.168.32.142}"
    export VMUI_VM_SSH_PORT="${VMUI_VM_SSH_PORT:-22}"
    export VMUI_VM_WORKDIR="${VMUI_VM_WORKDIR:-C:/vmui-agent}"
    export VMUI_VM_POWERSHELL_EXE="${VMUI_VM_POWERSHELL_EXE:-powershell.exe}"
    export VMUI_VM_DAEMON_HOST="${VMUI_VM_DAEMON_HOST:-127.0.0.1}"
    export VMUI_VM_DAEMON_PORT="${VMUI_VM_DAEMON_PORT:-50051}"
    export VMUI_VM_LOCAL_FORWARD_PORT="${VMUI_VM_LOCAL_FORWARD_PORT:-50051}"
    export VMUI_VM_SESSION_TASK="${VMUI_VM_SESSION_TASK:-vmui-agent-session}"
    export VMUI_VM_SMOKE_TASK="${VMUI_VM_SMOKE_TASK:-vmui-smoke}"
    export VMUI_VM_ARTIFACT_DIR="${VMUI_VM_ARTIFACT_DIR:-$VMUI_VM_WORKDIR/var/artifacts}"
    export VMUI_VM_LOG_DIR="${VMUI_VM_LOG_DIR:-$VMUI_VM_WORKDIR/var/log}"
}

vmui_vm_require_value() {
    local name value
    name="$1"
    value="${!name:-}"
    [[ -n "$value" ]] || vmui_vm_die "missing required setting '$name'; fill .vmui-vm.env from scripts/vm/vm.env.example"
}

vmui_vm_ssh_target() {
    if [[ -n "${VMUI_VM_SSH_TARGET:-}" ]]; then
        printf '%s' "$VMUI_VM_SSH_TARGET"
        return
    fi

    if [[ -n "${VMUI_VM_SSH_USER:-}" ]]; then
        printf '%s@%s' "$VMUI_VM_SSH_USER" "$VMUI_VM_HOST"
        return
    fi

    printf '%s' "$VMUI_VM_HOST"
}

vmui_vm_collect_ssh_args() {
    local -n out="$1"
    out=()

    if [[ -n "${VMUI_VM_SSH_CONFIG_FILE:-}" ]]; then
        out+=(-F "$VMUI_VM_SSH_CONFIG_FILE")
    fi
    if [[ -n "${VMUI_VM_SSH_IDENTITY_FILE:-}" ]]; then
        out+=(-i "$VMUI_VM_SSH_IDENTITY_FILE")
    fi
    if [[ -z "${VMUI_VM_SSH_TARGET:-}" ]]; then
        out+=(-p "$VMUI_VM_SSH_PORT")
    fi

    out+=("$(vmui_vm_ssh_target)")
}

vmui_vm_collect_scp_args() {
    local -n out="$1"
    out=()

    if [[ -n "${VMUI_VM_SSH_CONFIG_FILE:-}" ]]; then
        out+=(-F "$VMUI_VM_SSH_CONFIG_FILE")
    fi
    if [[ -n "${VMUI_VM_SSH_IDENTITY_FILE:-}" ]]; then
        out+=(-i "$VMUI_VM_SSH_IDENTITY_FILE")
    fi
    if [[ -z "${VMUI_VM_SSH_TARGET:-}" ]]; then
        out+=(-P "$VMUI_VM_SSH_PORT")
    fi
}

vmui_vm_ssh() {
    local -a ssh_args
    vmui_vm_collect_ssh_args ssh_args
    ssh "${ssh_args[@]}" "$@"
}

vmui_vm_scp() {
    local -a scp_args
    vmui_vm_collect_scp_args scp_args
    scp -O "${scp_args[@]}" "$@"
}

vmui_vm_run_powershell() {
    local script="$1"
    local -a ssh_args
    vmui_vm_collect_ssh_args ssh_args
    ssh "${ssh_args[@]}" "$VMUI_VM_POWERSHELL_EXE" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command - <<<"$script"
}

vmui_vm_quote_ps() {
    local value="$1"
    value="${value//\'/\'\'}"
    printf "'%s'" "$value"
}

vmui_vm_join_ps_args() {
    local joined="" part
    for part in "$@"; do
        if [[ -n "$joined" ]]; then
            joined+=" "
        fi
        joined+="$(vmui_vm_quote_ps "$part")"
    done
    printf '%s' "$joined"
}

vmui_vm_temp_dir() {
    mktemp -d "${TMPDIR:-/tmp}/vmui-agent-vm.XXXXXX"
}
