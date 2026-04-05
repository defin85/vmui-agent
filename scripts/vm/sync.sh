#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_dir/common.sh"

include_worktree=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --worktree)
            include_worktree=1
            shift
            ;;
        --help)
            echo "usage: ./scripts/vm/sync.sh [--worktree]"
            echo "  default: deploy HEAD archive only"
            echo "  --worktree: deploy the current worktree, including untracked non-ignored files"
            exit 0
            ;;
        *)
            vmui_vm_die "unknown argument: $1"
            ;;
    esac
done

vmui_vm_load_env

repo_root="$(vmui_vm_repo_root)"
tmp_dir="$(vmui_vm_temp_dir)"
trap 'rm -rf "$tmp_dir"' EXIT

archive_name="vmui-agent-sync-head.zip"
archive_path="$tmp_dir/$archive_name"

if (( include_worktree )); then
    python_bin="${PYTHON:-$(command -v python3 || command -v python || true)}"
    [[ -n "$python_bin" ]] || vmui_vm_die "python3 or python is required for --worktree mode"
    archive_name="vmui-agent-sync-worktree.zip"
    archive_path="$tmp_dir/$archive_name"
    file_list="$tmp_dir/worktree-files.list"
    git -C "$repo_root" ls-files --cached --modified --others --exclude-standard -z >"$file_list"
    sort -zu "$file_list" -o "$file_list"
    [[ -s "$file_list" ]] || vmui_vm_die "worktree archive would be empty"
    "$python_bin" - "$repo_root" "$file_list" "$archive_path" <<'PY'
import pathlib
import sys
import zipfile

repo_root = pathlib.Path(sys.argv[1])
file_list = pathlib.Path(sys.argv[2])
archive_path = pathlib.Path(sys.argv[3])
entries = [item.decode("utf-8") for item in file_list.read_bytes().split(b"\0") if item]

with zipfile.ZipFile(archive_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
    for relative_path in entries:
        source_path = repo_root / relative_path
        if not source_path.exists():
            continue
        archive.write(source_path, relative_path)
PY
else
    git -C "$repo_root" archive --format=zip --output="$archive_path" HEAD
fi

vmui_vm_scp "$archive_path" "$(vmui_vm_ssh_target):$archive_name"

workdir_ps="$(vmui_vm_quote_ps "$VMUI_VM_WORKDIR")"
archive_name_ps="$(vmui_vm_quote_ps "$archive_name")"

vmui_vm_run_powershell "\$ErrorActionPreference = 'Stop'
\$workdir = $workdir_ps
\$archive = Join-Path \$HOME $archive_name_ps
New-Item -Force -ItemType Directory -Path \$workdir | Out-Null
Get-ChildItem -LiteralPath \$workdir -Force -ErrorAction SilentlyContinue |
    Where-Object { \$_.Name -ne 'var' } |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Expand-Archive -LiteralPath \$archive -DestinationPath \$workdir -Force
Remove-Item -LiteralPath \$archive -Force
"

echo "synced repository content to $VMUI_VM_WORKDIR"
if (( include_worktree )); then
    echo "current worktree content, including untracked files, was deployed"
fi
