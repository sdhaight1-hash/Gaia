# Gaia — terrain design roadmap

Gaia is an independent, open-source terrain creation application, implemented in
Rust with GPU presentation through wgpu. It aims for the **workflow class** of
node-based tools such as Gaea, not code/asset/interface duplication.

## Implemented in first vertical slice

- Validated, serializable directed graph; selectable output and editable input links.
- Mountain and fractal noise generators (deterministic seeds).
- Thermal talus erosion (mass-conserving transfer; **not hydraulic erosion**).
- Blur, terracing and two-input blend.
- Interactive desktop editor (drag node layout, inspect/edit parameters).
- 2D heightfield view and native wgpu/WGSL 3D mesh preview.
- Project save/open in transparent JSON; 16-bit grayscale PNG and OBJ export.
- Unit tests for determinism, graph failures, serialization, conservation.

## Gaps and phased development

### Phase 2 — viewport and graph quality
- Dedicated depth-tested wgpu terrain pass (current egui callback uses cell
  sorting to approximate visibility; it is not suitable for production scenes).
- Undo/redo; native save/open dialogs; delete, multi-select, explicit socket linking.
- Per-node cached previews; dirty propagation only to downstream nodes.
- Async graph evaluation with cancellation and profiling.

### Phase 3 — physically grounded landforms
- Hydraulic erosion with explicit water and sediment conservation, mass ledgers,
  water boundary handling, and long-run stability tests.
- Flow directions/accumulation, rivers, lakes, coastlines, sediment and soil maps.
- Geological primitives: ridges, canyons, dunes, strata, tectonic forms.
- Separate water, sediment and geometry fields rather than mixing them into
  one scalar height buffer.

### Phase 4 — realistic surfaces and ecosystem
- Slope, curvature, aspect, flow, wear and depositional masks.
- Surface materials and multilayer procedural colorization.
- Climate, snow/glacier, debris, vegetation distribution models.

### Phase 5 — large-world production
- Tiled streaming and chunk halo handling; LOD, adaptive meshes.
- Chunk-local compute kernels and wgpu compute where profiling justifies it.
- Heightfield import and export of tiled maps, masks, mesh, project recipes,
  with deterministic headless builds and benchmark artifacts.

## Quality gates

1. Graph evaluation rejects cycles and missing inputs, is deterministic for
   identical seeds and is independent of UI state.
2. Thermal sediment transfer conserves total field mass within floating-point
   tolerance; hydraulic work must account for water **and** sediment mass.
3. Saving and reopening preserves graph topology, node parameters and output.
4. Exports have documented dimensions and units; PNG stores normalized heights.
5. Windows application launch and native GPU preview need hands-on validation
   beyond CI compilation; no claim of parity with QuadSpinner Gaea is made.

Hardware target for initial preview: 1080p and an NVIDIA RTX 2060-class GPU.
Limit default terrain resolution to 128 to support responsive development; larger
resolutions are available for testing.
