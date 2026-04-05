## 1. Behavioral experiment model

- [ ] 1.1 Define a safe navigation action set and reversible experiment sequences for custom 1C trees.
- [ ] 1.2 Define before/after observation records that tie input, capture, and inferred state together.

## 2. Inference engine

- [ ] 2.1 Infer session-local selection state from click plus directional navigation.
- [ ] 2.2 Infer expansion/collapse state where the panel behavior provides sufficient evidence.
- [ ] 2.3 Emit confidence and ambiguity markers instead of over-claiming exact object identity.

## 3. Exported repository overlay

- [ ] 3.1 Add an optional adapter for exported `src/cf` repository trees.
- [ ] 3.2 Use that overlay to narrow likely logical categories and object paths during mapping.
- [ ] 3.3 Mark overlay-derived conclusions as inferred rather than directly observed.

## 4. Verification

- [ ] 4.1 Validate that the mapper can reproduce a real selection shift in the Configurator left tree on the VM.
- [ ] 4.2 Validate that the mapper can align top-level tree movement with an exported repository shape.
- [ ] 4.3 Run `openspec validate add-09-onec-behavioral-tree-mapper --strict --no-interactive`.
