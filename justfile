set shell := ["bash", "-cu"]

fmt:
    cargo fmt --all

check:
    cargo check --workspace

test:
    cargo test --workspace

ci:
    cargo fmt --all --check
    cargo check --workspace
    cargo test --workspace
