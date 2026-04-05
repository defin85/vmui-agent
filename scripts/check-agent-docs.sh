#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

required_files=(
    "AGENTS.md"
    "README.md"
    "code_review.md"
    "docs/index.md"
    "docs/dev-runbook.md"
    "docs/windows-vm-access.md"
    "docs/windows-vm-bootstrap.md"
    "docs/codex-workflow.md"
    "docs/codex-setup.md"
    "crates/vmui-agent/AGENTS.md"
    "crates/vmui-core/AGENTS.md"
    "crates/vmui-platform-windows/AGENTS.md"
    "crates/vmui-transport-grpc/AGENTS.md"
    "proto/AGENTS.md"
    ".agents/skills/vmui-verify/SKILL.md"
    "scripts/doctor.sh"
    "scripts/check-agent-docs.sh"
    "justfile"
    ".github/workflows/ci.yml"
)

for file in "${required_files[@]}"; do
    if [[ ! -f "$file" ]]; then
        echo "error: required agent-facing file is missing: $file" >&2
        exit 1
    fi
done

stale_matches="$(
    grep -nH "openspec validate --strict --no-interactive" \
        AGENTS.md \
        README.md \
        code_review.md \
        docs/index.md \
        docs/dev-runbook.md \
        docs/codex-workflow.md \
        .agents/skills/vmui-verify/SKILL.md \
        2>/dev/null || true
)"

if [[ -n "$stale_matches" ]]; then
    echo "error: found stale unscoped OpenSpec validation command:" >&2
    echo "$stale_matches" >&2
    exit 1
fi

grep -q '^doctor:$' justfile || {
    echo "error: justfile is missing the 'doctor' recipe" >&2
    exit 1
}

grep -q '^check-agent-docs:$' justfile || {
    echo "error: justfile is missing the 'check-agent-docs' recipe" >&2
    exit 1
}

ci_block="$(awk '
    /^ci:$/ { in_ci = 1; next }
    in_ci && /^[[:alnum:]_-]+:$/ { in_ci = 0 }
    in_ci { print }
' justfile)"

if ! grep -Fqx '    ./scripts/check-agent-docs.sh' <<<"$ci_block"; then
    echo "error: just ci must run ./scripts/check-agent-docs.sh" >&2
    exit 1
fi

if ! grep -q './scripts/check-agent-docs.sh' .github/workflows/ci.yml; then
    echo "error: CI workflow must run ./scripts/check-agent-docs.sh" >&2
    exit 1
fi

echo "agent-facing docs check passed"
