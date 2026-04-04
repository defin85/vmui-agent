set shell := ["bash", "-cu"]

doctor:
    ./scripts/doctor.sh

check-agent-docs:
    ./scripts/check-agent-docs.sh

fmt:
    cargo fmt --all

check:
    cargo check --workspace

test:
    cargo test --workspace

ci:
    ./scripts/check-agent-docs.sh
    cargo fmt --all --check
    cargo check --workspace
    cargo test --workspace
