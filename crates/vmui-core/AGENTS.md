# vmui-core Instructions

## Scope
- This crate owns runtime defaults, session registry state, UI state cache behavior, and artifact store primitives.
- Start from `src/lib.rs`; runtime defaults live in `AgentConfig::default()`.

## Edit Routing
- Bind address, artifact dir, and default profile:
  `src/lib.rs`
- Session registry and runtime metadata:
  `src/lib.rs`
- UI snapshot/diff cache and revision handling:
  `src/lib.rs`
- Artifact store behavior:
  `src/lib.rs`

## Rules
- Keep this crate transport-agnostic and platform-neutral.
- Runtime defaults changed here must be mirrored in `README.md` and `docs/dev-runbook.md`.
- Do not move Windows-specific APIs or transport conversion logic into this crate.
- Preserve Linux-host validation paths when changing cache, artifact, or session code.

## Verification
- `cargo test -p vmui-core`
- Then run the workspace verification commands from the repo root.
