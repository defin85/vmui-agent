## 1. Safety and architecture

- [ ] 1.1 Define an explicit opt-in switch and operator workflow for enabling in-process instrumentation.
- [ ] 1.2 Define an isolated companion/runtime boundary so the main daemon does not depend on the experimental layer.
- [ ] 1.3 Define build/version fingerprint checks and fail-closed behavior.

## 2. Instrumentation contract

- [ ] 2.1 Define what richer node/event evidence the in-process layer is allowed to expose.
- [ ] 2.2 Define teardown, timeout, and error propagation semantics so the live session can recover safely.

## 3. Verification

- [ ] 3.1 Add acceptance criteria showing that unsupported builds fail closed without destabilizing the normal daemon.
- [ ] 3.2 Add at least one controlled VM validation path for a supported build fingerprint.
- [ ] 3.3 Run `openspec validate add-11-onec-inprocess-panel-instrumentation --strict --no-interactive`.
