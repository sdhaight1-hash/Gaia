# Gaia

**Gaia** is an independently implemented procedural terrain editor written
in **Rust** with **wgpu** graphics and GPU compute. The long-term objective
is production-workflow parity with QuadSpinner Gaea 2.3. It is not affiliated
with QuadSpinner and uses no proprietary Gaea source code or assets.

> **Development preview, not a completed Gaea replacement.** See
> [feature parity](docs/FEATURE_PARITY.md) for exactly what is missing.

## Get the Windows preview

The newest GitHub Actions build attached to the development pull request
provides a `gaia-windows-preview` artifact containing `gaia.exe`.
Download and extract the artifact, then run the executable on a Windows PC
with Vulkan/DX12-capable GPU drivers. No Rust toolchain is necessary for
an artifact download. These builds have not been physically tested with
a target GPU by the maintainers.

Development branch: `feat/lake-basins` (stacked on preceding PRs).

To compile from source:

```sh
cargo run --release
```

Requires Rust 1.95+ and a current Vulkan/DX12-capable graphics driver.

## Current workflow

- Left **Node Library**: Mountain, Island, Volcano, Noise, Mountain Range, Crater, Dunes,
  Canyon, thermal erosion, hydraulic erosion, Terrace, Stratify, Adjust, Blur, Slope Mask,
  Curvature, Aspect Mask, Drainage, Depression Fill, Rivers, Lake, Elevation Mask, Mask Levels, Warp,
  Blend, Masked Blend, Normals, Colorize and Color Mixer.
- Bottom graph: click/select and drag nodes; connect compatible named
  output and input sockets directly. When adding a node, Gaia connects its
  first compatible input to the selected output port and places it beside the
  source (avoiding occupied cards). Mask-aware and Color-aware compositors
  automatically create missing Slope or initial Colorize source nodes so new
  graph nodes do not start with invalid placeholder input ports.
  Unused nodes can be deleted in the right **Inspector**; nodes with dependents are protected.
- Right inspector: change procedural parameters and graph resolution
  (64, 128, 256 or 512); select a height-producing output node.
- Center **3D** preview: left drag orbit, scroll zoom, reset camera.
  Default terrain surface colors blend smoothly across elevation and slope,
  avoiding unnatural horizontal rings around crater or mountain thresholds.
  Selecting a Color graph port and switching to 3D paints that RGBA map's
  RGB channels over the terrain geometry (per-vertex lighting preserved).
- Center **2D map**: choose any existing node output port (height,
  wear, deposits, flow, water, sediment, slope, curvature, aspect,
  drainage accumulation, channels, elevation mask, color)
  and inspect the result.
- Top toolbar: Save, Open, Undo, Redo, Export, Rebuild. `Ctrl+S`,
  `Ctrl+O`, `Ctrl+Z`, `Ctrl+Shift+Z` and `Ctrl+Y` shortcuts. Pointer
  drags coalesce into one undo step; history holds the last 64 edits.
  Save/Open use an editable project path, not file dialogs.
- Export normalized 16-bit grayscale PNG scalar maps, RGB8 PNG color and
  derived normal maps (flat tangent normal RGB ≈128,128,255; green-axis flip),
  or 2×2 / 4×4 overlapping PNG tile grids for the currently selected output
  directly from the desktop Export menu (also supported by the headless CLI),
  or OBJ terrain meshes with planar UVs and normalized Y-up vertex normals.
  Color is stored as typed RGBA data in the graph,
  not faked by an 8-bit heightfield. All graph parameters and connections
  are serialized to JSON.
- Import grayscale 8-/16-bit PNG as a height-producing graph node, including
  by dragging a PNG into the desktop editor. Drop a `.gaia.json` file to open
  a project. When saved,
  file paths become relative to the project file (including sibling folders),
  making scenes portable when assets are moved with them. Absolute paths on
  different Windows drives remain absolute.

## Execution and algorithms

- Typed node ports and named multi-output bundles. Old JSON projects
  whose connections are numeric IDs deserialize as height-port links.
- Incremental, signature-based CPU cache; changes to a node cause its
  affected graph path to rebuild, while clean ancestors are reused.
- Dedicated cancellable worker thread. Rapid changes supersede stale
  jobs without blocking UI interaction.
- Conservative thermal talus relaxation with cumulative wear/deposit masks.
- Mask Levels transforms typed masks through black/white points, gamma and inversion;
  output can drive masked terrain and color blending.
- Adjust Height operator combining multiplication, bulking/shaping, optional global autolevel, range clamp and normalized inversion.
- Volcano generator with adjustable cone radius, summit crater, bulk, surface breakup and center placement.
- Stratify terrain operator for seeded, warped and tilted sedimentary-looking rock layers; preserves the input terrain silhouette rather than replacing it.
- Island generator with seed-controlled shoreline roughness, interior relief,
  smooth coast gradient and ocean-level outer boundary.
- Deterministic D8 downslope drainage analysis with contributing-area ledger,
  logarithmic accumulation mask and channel cutoff. Unconditioned flats/local
  minima remain terminal outlets.
- Priority-flood Depression Fill makes a Height field outlet-connected by raising
  enclosed pits to a spill level. An optional small positive grade guarantees
  strictly downhill D8 neighbors across flats. Outputs: conditioned Height and
  fill_depth (Water-type scalar, in raw height units). All map edges act as
  outlets; no physically simulated lakes, rainfall or sediment balance is implied.
- Rivers: priority-flood-conditioned D8 contributing-area network, softly widened
  channel corridors, configurable downcutting and second-pass conditioning to
  prevent new terminal pits. Outputs carved Height, channels Mask and normalized
  wear Mask. This first version does not model rain discharge, water surface,
  sediment mass conservation or art-directable headwater masks.
- Lake: static outlet-bounded basin filling with flat water surfaces, selectable
  fill fraction, minimum basin area and potential depth thresholds. Exports
  original terrain Height, inundated surface Height, depth Water and coverage Mask.
  It is not rainfall/evaporation driven or time-evolving, and volume units require
  a real-world cell area that Gaia does not yet know.
- Experimental hydraulic erosion: rain, water advection, sediment
  transport, erosion/deposition, evaporation, boundary outflow ledgers.
  Water/solids are tested for mass consistency.
- Native WGSL pointwise blending via wgpu compute on resolutions >=256,
  with CPU fallback when no GPU device was supplied. GPU numerical
  verification on a target machine is still required.

## Headless builds

Gaia also includes a windowless CPU builder, useful for automation and
reproducible terrain exports without a working GPU or graphics session:

```sh
cargo run --release --bin gaia-build -- --demo --output builds/sample --resolution 256
cargo run --release --bin gaia-build -- --demo --output builds/sample-2k --resolution 2048 --no-obj
cargo run --release --bin gaia-build -- --project terrain.gaia.json --output builds/final
cargo run --release --bin gaia-build -- --project terrain.gaia.json --output builds/erosion-wear --port 2:wear --no-obj
cargo run --release --bin gaia-build -- --project terrain.gaia.json --output builds/all-erosion --all-outputs 2 --tiles 2
cargo run --release --bin gaia-build -- --project terrain.gaia.json --output builds/tiles --tiles 4 --no-obj
```

Scalar builds write normalized 16-bit grayscale PNG; Color builds write
RGB8 PNG (alpha intentionally omitted). Every single-port build writes a JSON report
of dimensions, scalar/luminance min/max/mean, selected port and export format.
The `--all-outputs NODE` command exports all named ports of a graph node
from one shared CPU graph cache, writing independent typed PNG16/RGB8 maps,
optional OBJ height meshes, optional tiled grids and a `STEM.batch.report.json`
manifest with per-output metadata and computed/reused node counts.
Do not combine `--all-outputs` with `--port`.
Height outputs also write OBJ unless `--no-obj` is set. The current supported resolutions
are 32–2048 for the headless builder (desktop 3D preview stays at 64–512).
A 2K graph build still evaluates every terrain node at full-grid resolution,
so memory use scales with resolution squared and cached graph fields.
Optional `--tiles 2..8` exports an overlapping grid of PNG16
scalar or RGB8 Color tiles into `STEM.tiles/` and writes `STEM.tiles.json`.
Each adjacent pair shares one identical pixel border; non-divisible source
resolutions are uniformly resampled onto a common global pixel lattice.
**This is an export-only split of a full graph build**, not independent tile
simulation, streamed evaluation, or a reduction in build memory.

## Verification

```sh
cargo check --all-targets
cargo test --lib
# Explicit hardware-dependent GPU-vs-CPU numerical comparison:
cargo test --lib gpu_matches_cpu_at_multiple_resolutions -- --ignored --nocapture
```

In CI, the GPU comparison explicitly skips its numerical check if no
adapter is available; that is **not** evidence of GPU correctness.

## Known limitations

- Native file dialogs, realistic multi-layer PBR materials, terrain painting,
  vegetation, snow/glaciers, lakes, rivers, tiles, large resolutions,
  high-end geology and production exports are not finished.
- Graph ports support Height, Mask, Water and true RGBA Color fields.
  Vector, Points and Mesh port types are reserved but not populated.
- The hydraulic model is an experimental reference implementation,
  **not** numerically or visually identical to Gaea's erosion solvers.
- GPU blending requires a working GPU and needs performance profiling.
  There is no claim that GPU compute is faster for every graph operation.
- The 3D preview uses depth-tested wgpu rendering and selected Color-map
  vertex colors. It does not yet implement PBR materials or texture layers.
- Windows screenshot/video sign-off is still required for the 1080p UI.

## Source layout

`src/terrain.rs` — core heightfield primitives, project persistence, exports.

`src/pipeline.rs` — typed multi-output graph evaluation and cache.

`src/worker.rs` — cancellable asynchronous evaluation.

`src/geology.rs` — terrain generators, warp and structural masks.

`src/hydrology.rs` — conservative hydraulic erosion and mass ledgers.

`src/drainage.rs` — D8 catchment-area accumulation and channel mask analysis.

`src/color.rs` — typed RGBA color fields, palettes, mask mixing and RGB8 export.

`src/gpu_compute.rs`, `src/blend.wgsl` — WGSL compute masked blending.

`src/gpu.rs`, `src/terrain.wgsl` — native wgpu terrain preview.

`src/ui_theme.rs`, `src/main.rs` — modern desktop workspace and controls.

See the [technical roadmap](docs/ROADMAP.md) and
[feature parity matrix](docs/FEATURE_PARITY.md) for the next gates.
