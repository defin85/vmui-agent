# Roadmap

## Phase 1: Repo And Contract Baseline

- Establish workspace boundaries.
- Freeze the first protocol draft.
- Keep Linux-based development and CI green.

## Phase 2: MVP In-VM Daemon

- Implement daemon config and session bootstrap.
- Add a Windows backend observer thread.
- Support `list_windows`, `get_tree`, `focus_window`, `invoke`, `send_keys`, `capture_region`.
- Add snapshot, diff and artifact persistence.

## Phase 3: 1C Diagnostic Workflows

- Add 1C process/window filters for application UI and Configurator.
- Add locator profiles for common Configurator surfaces.
- Add post-failure diagnostic bundle collection.

## Phase 4: Production Hardening

- Add transport resilience and session resync.
- Add metrics, tracing, and artifact retention policies.
- Add OCR plugin and opaque-surface strategies.
- Measure fallback rate and stale-tree rate on the target VM.

## Release Gates

- The daemon survives long-running sessions without leaking state.
- Diff stream stays monotonic and resync logic is tested.
- Standard forms work mostly through UIA/MSAA.
- Problematic surfaces are explicitly marked as fallback-only.
