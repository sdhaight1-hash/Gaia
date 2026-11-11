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
| Graph | Typed Height/Mask/Water output ports, multi-output nodes, direct socket wiring, safe unused-node deletion, input type validation, legacy JSON migration, cached DAG, async cancellable builds | Graph undo/redo, macros, expressions, variables, user subgraphs and deletion/rewiring of connected nodes |
| Terrain generators | Mountain, Noise, Range, Crater, Dunes, Canyon | Islands, volcanoes, uplift/tectonics, rock layers, geological diversity, art-directed generator fidelity |
| Terrain modifiers | Terrace, Blur, Warp, Blend, Masked Blend | Fracture, strata, displacement operators, custom remaps, more blending modes |
| Erosion | Mass-conserving thermal talus + separate wear/deposits; experimental rain/flow/sediment model with open-boundary water/solids ledger | Production-quality large-scale hydraulic erosion, river carving, rock/scree/debris, numerical/visual Gaea-level fidelity |
| Hydrology | Water depth, suspended sediment, cumulative flow map | Rivers/lakes/sea geometry, watershed hierarchy, coastlines, stable high-resolution water systems |
| Masks | Slope, Curvature, Elevation, Wear, Deposits, Flow | Aspect, terrain feature classifiers, rock/soil/weathering, mask painting and advanced compositing |
| Color/materials | No true Color fields | Colorized surfaces, gradients, texture/multi-channel materials, splat/normal maps |
| Snow, ice, vegetation | Not implemented | Glacier/snowpack simulation, climate-dependent vegetation/scatter, point exports |
| Viewport/editor | Themed docked workspace, 2D maps, depth-tested wgpu 3D mesh preview, draggable socket wires, property inspector and export menu | Per-node thumbnails, analysis overlays, painting tools, native dialogs, persisted workspaces, visual QA |
| Output/build | Transparent JSON projects, normalized 16-bit PNG and OBJ, export selected scalar map | Large tiled/region builds, adaptive meshes, heightfield import, texture/material exports, CLI/job queue, DCC/engine integrations |
| Performance | Cancellable background builds, incremental cache, wgpu preview, first WGSL Blend compute kernel (>=256 samples) | CPU/GPU performance profiling, broader compute kernels, GPU batched graph execution, tiling, memory budgets, GPU numerical sign-off |

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

1. Finish depth-tested wgpu 3D render target, drag-to-connect node ports,
   undo/redo and native file handling — before expanding the UI further.
2. Add terrain import and output units/scales, batch builds, diagnostic
   overlays and a reproducible benchmark suite.
3. Expand hydrology (flow accumulation, drains, channel extraction, lakes),
   test water/solid ledgers over long runs and validate tile seams.
4. Add multi-channel Color/Materials as actual typed buffers, then
   environment-aware surface and ecosystem models.
5. Tiled evaluation and GPU batched kernels with CPU reference checks,
   caches, cancellation, persistent build artifacts and headless CLI.

## Release gate

Do not call Gaia a completed Gaea clone until major documented node families,
multi-output workflows, large builds, material/output pipelines and verified
interactive experiences are delivered. Exact numerical identity is neither
possible nor intended without access to QuadSpinner's proprietary implementation.
