# Gaea 2.3.1 functional-parity contract (Gaia)

Target: QuadSpinner Gaea 2.3.1.0, production release of 2026-09-24.
Gaea 3 is a later target, not included in the Gaea 2 completion gate.

Parity means independently reimplementing functions and workflows, not using
proprietary code, bundled assets, or pixel-identical proprietary visuals.
Gaea's unpublished numerical algorithms cannot be exactly reproduced.

References:
- https://www.quadspinner.com/Download
- https://quadspinner.com/Gaea/Features
- https://docs.gaea.app/reference/nodes/
- https://docs.gaea.app/reference/nodes/simulate/erosion
- https://blog.quadspinner.com/gaea-2-3-now-available/

## Capability matrix (2026-11-10)

| Area | Gaea 2.3 scope | Gaia current state | Required parity |
|---|---|---|---|
| Graph | Typed ports, multi-output nodes, macros, variables, conditional routing, cache, modifiers | Heightfield-only DAG; node settings | Typed multi-output evaluation, dirty propagation, undo/redo, macros, expressions |
| Generators | Mountains, ranges, islands, canyons, dunes, volcanoes, geological primitives | Mountain and noise | Representative family with shape controls and presets |
| Surfaces/modify | Warp, transpose, rock layers, strata, fractures, filters | Blur and terrace | Production-grade geological modifiers |
| Erosion/debris | Hydraulic and thermal erosion, rock, scree, sediment, debris | Thermal talus approximation | Separate water/bedrock/sediment; art-directable validated algorithms |
| Water/snow/ice | Lakes, rivers, sea, snowfield and glaciers | None | Geometrically coherent simulations and data outputs |
| Derived data | Height, slope, curvature, wear, deposit, flow, depth, normals | None | Connected, typed and reusable mask outputs |
| Color | Surface-aware color, mixers, material/weathering, erosion colors | None | Layered graph-driven color and texture channels |
| Vegetation | Trees, shrubs, climate and topography controls | None | Terrain-aware scatter fields, masks, point export |
| Editor | Rich node graph, 2D/3D maps, draw/gizmos, recovery, analysis | Themed preview/editor prototype | Direct socket wiring, pan/zoom, undo/redo, live previews, persistence |
| Build/export | High-resolution tiled/region builds, caches/baking, adaptive mesh, CLI, point clouds | 512 max; PNG16, OBJ | Efficient chunk builds, seam tests, multiple formats, batch automation |
| Compute | Hybrid CPU/GPU and SIMD algorithms | CPU processing; wgpu rendering only | GPU compute where beneficial; CPU reference and numerical validation |

## Non-negotiable engine architecture

1. Typed multi-output graph. Port types include Height, Mask, Color,
   Vector, Water, Points, Mesh. A node may publish multiple named outputs.
   Erosion publishes at least Height, Wear, Deposits and Flow.
2. Pure evaluation core. Node parameters and result caches stay separate.
   Cache identity captures inputs, parameters, algorithm revision,
   region/resolution and seed.
3. Incremental evaluation. Editing one node invalidates only that node
   and downstream dependencies; preview and final build share cache.
4. Compute uses Rust + wgpu. Keep a reference CPU path for validation
   and fallback, then add GPU kernels. Floating-point output is compared
   with explicit tolerances across GPUs.
5. Simulation state is explicit. Bedrock, sediment, water, snow/ice and
   point populations are distinct fields with conservation diagnostics.
6. Tile boundaries are validated; algorithms declare halo requirements.
7. The desktop UI is a client of the same engine used by headless builds.

## Immediate implementation sequence

A. Port and output contract (first PR): PortType, PortRef, typed outputs,
   JSON migration from legacy graph, graph validation. Add real
   Height/Slope/Wear/Deposits output nodes and a consuming masked Blend.

B. Caching/evaluation: per-node cache, dependency invalidation, preview
   of any output, asynchronous cancellable builds.

C. GPU compute: wgpu pointwise blend/masks first; ping-pong iterative
   simulation second. Establish CPU-vs-GPU tests and timing on RTX 2060.

D. Physically grounded hydraulics: conservative rain/flow, sediment
   erosion and deposition, wear/deposits/flow outputs, outlet accounting.
   Follow with detailed geology, hydrology, surface masks and color.

## First milestone acceptance tests

- Legacy projects load and six existing tests still pass.
- Height/Wear/Deposits are separate independently selectable outputs.
- Typed Mask-to-Blend connection works; invalid links and cycles are errors.
- Editing one branch does not evaluate unrelated branches.
- GPU compute executes and matches CPU reference within stated tolerance.
- Long runs are cancellable without blocking the UI.
- Windows wgpu viewport and graph UX are visually verified at 1080p.

Do not describe Gaia as Gaea-equivalent until the capability matrix is
substantially complete and validated, including production exports.
