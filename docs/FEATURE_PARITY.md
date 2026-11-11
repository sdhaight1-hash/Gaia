# Gaia ↔ Gaea 2.3 functional parity ledger

**Updated: 2026-11-10.** Target is the documented workflow of QuadSpinner
Gaea 2.3, implemented independently in Rust and wgpu. Gaea's proprietary
code/assets/algorithms are not part of Gaia.

References: https://quadspinner.com/Gaea |
https://docs.gaea.app/reference/nodes/ |
https://docs.gaea.app/reference/nodes/simulate/erosion

## Verified implementation versus outstanding work

| Capability | Implemented on latest development branch | Still missing for Gaea-class workflow |
|---|---|---|
| Graph | Typed Height/Mask/Water/Color output ports, multi-output nodes, direct socket wiring, 64-edit project undo/redo, safe unused-node deletion, input type validation, legacy JSON migration, cached DAG, async cancellable builds | Macros, expressions, variables, user subgraphs and deletion/rewiring of connected nodes |
| Terrain generators | Mountain, Island, Volcano, seeded Voronoi Plates with collision uplift/rift and continental/boundary masks, Noise, Range, Crater, Dunes, Canyon | Dynamically evolving plate tectonics, folding, more realistic structural geology and art-directed Gaea-level fidelity |
| Terrain modifiers | Independently implemented Adjust (multiply, shaper, clamp, autolevel, invert), Equalize (tie-aware global quantiles, adjustable factor), Terrace, Stratify (tilted/warped rock layering), Blur, Warp, Blend and Masked Blend | advanced fractures, exposed rock structures, displacement, paintable curves and more blend modes |
| Erosion | Mass-conserving thermal talus + wear/deposits; experimental rain/flow/sediment model; separate priority-flood/D8 river corridor carving with mask outputs | Production-quality large-scale hydraulic erosion, mass-conserving river sediment transport, scree/debris, numerical/visual Gaea-level fidelity |
| Hydrology | Priority-flood pit conditioning, D8 drainage, river downcutting, static Lake basins, static Sea level/coast surface with raster shores/beaches, inundation/depth maps and experimental water/sediment transport | Dynamic lake filling, tides/ocean waves, conserved coastal erosion, full watershed hierarchy, headwater masks and stable large systems |
| Masks | Slope, Curvature, Aspect, four normalized Material Masks (sand/soil/rock/snow), horizon AO, Sunlight shadows, plate masks, sea wet/shore/beach, lake and snow coverage, drainage Accumulation/Channels, Elevation, Wear, Deposits, Flow, Mask Levels/invert | Feature classifiers, rock/soil/weathering, mask painting and advanced compositing |
| Color/materials | RGBA Color fields, editable altitude palettes, four-layer material coverage masks, typed RGBA8 splat map (sand/soil/rock/snow) with 4-layer GGX dielectric wgpu shading (editable serialized albedo/roughness/detail scale, triplanar texture atlas and adjustable viewport sun), typed Color Mixer, AO/terrain-cast Sunlight masks, RGB normals with strength and green-axis flip, 2D/3D vertex-color preview, RGB8 export | External authored PBR texture assets, detail/LOD normal maps and tangent-space mesh integration |
| Snow, ice, vegetation | Independent Snow node: snowline/seed/slope adhesion, snowfall/melt and mass-conserving downhill settling; height/depth/coverage outputs | Particle snow, cascading snowfield, glaciers and ice dynamics, climate-dependent vegetation/scatter, point exports |
| Viewport/editor | Themed docked workspace, 2D maps, depth-tested wgpu 3D mesh preview that follows selected Height ports with continuous slope/elevation palette and optional four-layer GGX splat shading, draggable socket wires, property inspector, native Open/Save As/Import PNG dialogs and export menu | Per-node thumbnails, analysis overlays, painting tools, persisted workspaces, visual QA |
| Output/build | Portable JSON projects, PNG8/PNG16 heightmap import, scalar PNG16/RGB8 Color/RGBA8 Splat PNG and OBJ with UVs and vertex normals, selected-port or all-node-ports cached headless CLI at up to 2048², seam-verified overlapping tile PNG export with manifest | Truly tiled/region graph execution, adaptive meshes, layered materials, multi-node build selection, build profiles/job queue, PBR material textures and DCC/engine integrations |
| Performance | Cancellable background builds, incremental cache, wgpu preview, 2K full-grid headless builds, first WGSL Blend compute kernel (>=256 samples) | True streamed tile/bucket graph evaluation beyond 2K, reduced cache memory footprint, GPU batched kernels and device numerical sign-off |

## Acceptance levels

- **A — Compiled:** Rust code type-checks for both supported CI platforms.
- **B — Numerically validated:** Determinism, graph safety and mass ledgers
  pass automated regression tests.
- **C — GPU verified:** Actual device execution, WGSL validation, CPU/GPU
  tolerances and performance measured at 64/128/256/512 samples.
- **D — Interactive verified:** Windows 1080p application screenshot/video
  and mouse/keyboard controls checked on target hardware.
- **E — Workflow-complete:** Production work can be built/exported end to end
  without manual fixes, at relevant scale, with stability benchmarks.

CI A/B passing does not imply C/D/E. On GitHub's Ubuntu runner the optional
GPU numerical comparison can *skip* because no graphics adapter exists.
Real GPU numerical results remain required.

## Immediate implementation priorities

1. Validate the depth-tested viewport, socket wiring, undo/redo and file drag/drop
   on a real Windows GPU; Windows native dialogs still require physical interaction sign-off.
2. Add terrain import and output units/scales, batch builds, diagnostic
   overlays and a reproducible benchmark suite.
3. Extend priority-flood-conditioned D8 drainage through connected river tracing, basin labels and physically modeled lakes;
   test water/solid ledgers over long runs and validate tile seams.
4. Evolve the colorized 3D preview into proper layered PBR materials, then
   environment-aware surface and ecosystem models.
5. Tiled evaluation and GPU batched kernels with CPU reference checks,
   caches, cancellation, persistent build artifacts and headless CLI.

## Release gate

Do not call Gaia a completed Gaea clone until major documented node families,
multi-output workflows, large builds, material/output pipelines and verified
interactive experiences are delivered. Exact numerical identity is neither
possible nor intended without access to QuadSpinner's proprietary implementation.
