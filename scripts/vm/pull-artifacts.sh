#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_dir/common.sh"

extract=0
dest_dir=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --extract)
            extract=1
            shift
            ;;
        --dest)
            [[ $# -ge 2 ]] || vmui_vm_die "--dest requires a directory argument"
            dest_dir="$2"
            shift 2
            ;;
        --help)
            echo "usage: ./scripts/vm/pull-artifacts.sh [--dest DIR] [--extract]"
            exit 0
            ;;
        *)
            vmui_vm_die "unknown argument: $1"
            ;;
    esac
done

vmui_vm_load_env

repo_root="$(vmui_vm_repo_root)"
timestamp="$(date +%Y%m%d-%H%M%S)"
dest_dir="${dest_dir:-$repo_root/var/vm-downloads/$timestamp}"
mkdir -p "$dest_dir"

remote_zip="vmui-agent-artifacts-$timestamp.zip"
remote_zip_ps="$(vmui_vm_quote_ps "$remote_zip")"
artifact_dir_ps="$(vmui_vm_quote_ps "$VMUI_VM_ARTIFACT_DIR")"
log_dir_ps="$(vmui_vm_quote_ps "$VMUI_VM_LOG_DIR")"

vmui_vm_run_powershell "\$ErrorActionPreference = 'Stop'
\$artifactDir = $artifact_dir_ps
\$logDir = $log_dir_ps
\$zipPath = Join-Path \$HOME $remote_zip_ps
\$staging = Join-Path \$env:TEMP ('vmui-agent-artifacts-' + [guid]::NewGuid().ToString('N'))
\$paths = @()
if (Test-Path -LiteralPath \$artifactDir) { \$paths += \$artifactDir }
if (Test-Path -LiteralPath \$logDir) { \$paths += \$logDir }
if (\$paths.Count -eq 0) {
    throw 'no artifact or log directories exist on the VM'
}
New-Item -Force -ItemType Directory -Path \$staging | Out-Null
foreach (\$source in \$paths) {
    \$leaf = Split-Path -Leaf \$source
    \$targetRoot = Join-Path \$staging \$leaf
    New-Item -Force -ItemType Directory -Path \$targetRoot | Out-Null
    Get-ChildItem -LiteralPath \$source -Recurse -Force -ErrorAction SilentlyContinue | ForEach-Object {
        if (\$_.PSIsContainer) {
            return
        }
        \$relative = \$_.FullName.Substring(\$source.Length).TrimStart('\')
        \$destination = Join-Path \$targetRoot \$relative
        New-Item -Force -ItemType Directory -Path (Split-Path -Parent \$destination) | Out-Null
        try {
            Copy-Item -LiteralPath \$_.FullName -Destination \$destination -Force -ErrorAction Stop
        } catch {
            Write-Warning ('skipping locked or unreadable file: ' + \$_.FullName)
        }
    }
}
if (Test-Path -LiteralPath \$zipPath) {
    Remove-Item -LiteralPath \$zipPath -Force
}
Compress-Archive -Path (Join-Path \$staging '*') -DestinationPath \$zipPath -Force
Remove-Item -LiteralPath \$staging -Recurse -Force
"

local_zip="$dest_dir/$remote_zip"
vmui_vm_scp "$(vmui_vm_ssh_target):$remote_zip" "$local_zip"
vmui_vm_run_powershell "Remove-Item -LiteralPath (Join-Path \$HOME $remote_zip_ps) -Force"

if (( extract )); then
    if command -v unzip >/dev/null 2>&1; then
        unzip -q "$local_zip" -d "$dest_dir"
    else
        python_bin="${PYTHON:-$(command -v python3 || command -v python || true)}"
        [[ -n "$python_bin" ]] || vmui_vm_die "unzip or python3/python is required for --extract"
        "$python_bin" - "$local_zip" "$dest_dir" <<'PY'
import pathlib
import sys
import zipfile

archive_path = pathlib.Path(sys.argv[1])
destination = pathlib.Path(sys.argv[2])
with zipfile.ZipFile(archive_path) as archive:
    archive.extractall(destination)
PY
    fi
    rm -f "$local_zip"
    echo "downloaded and extracted artifacts into $dest_dir"
else
    echo "downloaded artifacts archive to $local_zip"
fi
