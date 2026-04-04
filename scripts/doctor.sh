#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

required_tools=(cargo openspec bd rg)
optional_tools=(just)
missing=0

for tool in "${required_tools[@]}"; do
    if command -v "$tool" >/dev/null 2>&1; then
        echo "ok: found required tool '$tool'"
    else
        echo "error: missing required tool '$tool'" >&2
        missing=1
    fi
done

for tool in "${optional_tools[@]}"; do
    if command -v "$tool" >/dev/null 2>&1; then
        echo "ok: found optional tool '$tool'"
    else
        echo "warn: optional tool '$tool' not found"
    fi
done

if (( missing != 0 )); then
    echo "doctor failed: install the missing required tools and rerun ./scripts/doctor.sh" >&2
    exit 1
fi

cargo metadata --no-deps --format-version 1 >/dev/null
echo "ok: cargo metadata resolved for this workspace"

openspec list >/dev/null
echo "ok: openspec CLI can read repository state"

bd ready --json >/dev/null
echo "ok: beads CLI can read repository state"

if [[ -f ".codex/config.toml" ]]; then
    echo "info: .codex/config.toml is optional and repository runtime does not depend on it"
    if grep -q '^\[mcp_servers\.claude-context\]' ".codex/config.toml"; then
        echo "info: current Codex config expects local claude-context, Ollama, Milvus, and Node 20 or 22"
    fi
fi

echo "doctor complete: environment is ready for the repo-standard workflow"
