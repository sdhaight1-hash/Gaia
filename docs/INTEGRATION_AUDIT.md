# Gaia baseline integration review — November 11, 2026

## Audit and decision

- Pre-integration \`main\`: \`f76b39b5\`, initial README only.
- Reviewed development tip: \`7980c7de\`, 273 commits ahead and zero behind.
- Inventory: 59 branch refs and 56 open draft PRs, accumulated through the
  November 10–11 node editor, hydrology, terrain generation, material and PBR work.
- 55 of 56 PR branch tips are already reachable from the development tip.
- There are three branch tips off the linear history:
  - \`feat/color-surfaces\`: earlier prototype of Colorize, Color Mixer and RGB export,
    replaced by typed color integration already in the current pipeline. No
    independent feature cherry-picked; retains a branch ref for archival traceability.
  - \`feat/wgpu-compute\`: two unique changes (wgpu 30 adapter defaults and
    optional adapter-based GPU numerical CI); both are present in the current
    \`src/gpu_compute.rs\` and \`.github/workflows/rust.yml\`.
  - \`feat/surface-preview-materials\`: surface-specific Snow/Sea/Lake tinting
    was not included in the later stack. Reimplemented and regression-tested in
    this integration commit, with selected-height geometry and 2D data preserved.

## Reviewed subsystems and regression gates

- **Graph and project**: typed ports, deterministic incremental cache, legacy
  project JSON compatibility, node validation, undo/redo and portable PNG imports.
- **Terrain algorithms**: generators, erosion, hydraulic water/sediment, lakes,
  rivers, snow, coastal masks, AO and sunlight; coverage and conservation tests.
- **Material pipeline**: four normalized coverage masks, packed RGBA8 splat
  weights with conserved 255-per-pixel quantization, baked RGB8 albedo, palette
  persistence, and seam-matched tiled export tests.
- **GPU and UI**: wgpu depth-tested mesh, typed color/height/splat preview,
  GGX dielectric material shading, procedural triplanar atlas, camera and
  lighting uniforms, native project dialogs and library search.
- **Build**: Ubuntu/Windows all-target check and test, CPU WGSL validation,
  optional GPU comparison (skips where no adapter), Windows release binary
  packaging and artifact upload.

## Known non-blocking preview limitations

- No full QuadSpinner Gaea feature parity, proprietary algorithm reproduction,
  authored high-resolution PBR assets, full water/atmosphere simulation, or true
  streaming, region-evaluated terrain; current tiled exports split completed maps.
- CI validates shader source and compilation, not a real GPU runtime screenshot.
  Verify Vulkan/DX12 startup, terrain draw, interactive node graph and panels,
  native dialogs and practical GPU frame rate on the intended Windows hardware.
- Physical UV/terrain scale conventions remain approximate; triplanar material
  textures are procedural placeholders.
- Native dialogs are synchronous OS dialogs.
- Existing stacked PRs document feature history; after consolidation, close
  superseded PRs without deleting or rewriting unmerged branch contents.

## Merge acceptance

Before merging the integration PR into \`main\`, require successful Ubuntu and
Windows check/tests for the integration SHA, a successful Windows release
artifact, and a comparison showing \`main\` is the ancestor of integration.
Do not claim hands-on Windows GPU visual validation without an actual recording.
