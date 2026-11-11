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

The verified development history was consolidated into `main`. Get the
latest Windows preview artifact from a successful `main` GitHub Actions run.
Development features continue on short-lived pull request branches.

To compile from source:

```sh
cargo run --release
```

Requires Rust 1.95+ and a current Vulkan/DX12-capable graphics driver.

## Project persistence (reliability)

Opening a project now performs bounded **structural** validation (node count,
output type, input count/types, missing sources, cycles, graph coordinates and
missing imported heightmap files) without running costly generators or erosion
on the UI thread. The existing cancellable graph worker builds the preview
after the project is installed. A project with a structurally invalid graph
remains rejected without replacing the active project.

Saving writes and syncs a same-directory temporary file before atomically
replacing the destination with \`tempfile::NamedTempFile::persist\`. Failed
staging does not truncate the previous project. As with most normal filesystem
operations, this is *not* a guaranteed power-loss durable transaction: the
containing directory is not independently synced. An imported PNG's content
may still fail decoding during the asynchronous first build (only its
existence is checked on open).

See \`src/terrain.rs\` persistence tests.

## Current workflow

- Left **Node Library**: search by node name or description; add Mountain,
  Island, Volcano, Plates, Noise, Mountain Range, Crater, Dunes,
  Canyon, thermal erosion, hydraulic erosion, Terrace, Stratify, Adjust, Blur, Slope Mask,
  Curvature, AO, Sunlight, Aspect Mask, Drainage, Depression Fill, Rivers, Lake, Sea, Snow, Elevation Mask, Mask Levels, Warp,
  Blend, Masked Blend, Normals, Colorize and Color Mixer.
- Bottom graph: use **+ Add node** in the graph header, the left searchable
  library or right-click empty canvas to create a node. New compatible nodes
  connect to the selected output port automatically; cards avoid existing
  nodes, including tall multi-output cards. Mask/Color compositors still
  create valid missing prerequisites where necessary.
  **Drag a node's header** to move it. Click or drag from a labeled colored
  output socket onto an input of the same type to connect; Escape cancels
  a pending cable. Incompatible types and cycles are rejected.
  **Delete…**, the Inspector, node-header right-click or the Delete key can
  delete a selected node. A single-input connected operator is safely bypassed
  when its input matches all downstream output types; otherwise Gaia asks
  before deleting its entire dependent branch. Undo restores deleted nodes.
  Navigate with **wheel/pinch zoom**, **middle-mouse drag** or **drag empty
  canvas to pan**. Use **Fit all** (F) to see the whole graph and
  **Find** (Home) to center the selected node at a readable zoom;
  the +/− buttons zoom around the viewport center. Graph navigation does
  not alter serialized node positions.
  **Ctrl/Shift-click** adds or removes cards from a multi-selection;
  **Ctrl-drag the empty canvas** selects cards inside a marquee, and Ctrl+A
  selects every node while graph has focus. Drag a selected card header to
  move the group in one undoable gesture. **Delete…** asks for confirmation
  before removing all selected nodes and any downstream consumers; it refuses
  to remove the last Height source. Right-click an input socket to select
  a compatible upstream output directly, or click the input then click an
  output to replace its cable. Inputs currently require a source: arbitrary
  disconnected sockets are not supported.
- Right inspector: change procedural parameters and graph resolution
  (64, 128, 256 or 512); select a height-producing output node.
- Center **3D** preview: left drag orbit, scroll zoom, reset camera.
  Default terrain surface colors blend smoothly across elevation and slope,
  avoiding unnatural horizontal rings around crater or mountain thresholds.
  Selecting any Height output (including Lake:surface, or an earlier generator)
  also selects the actual 3D mesh and evaluates only the required subgraph,
  avoiding unrelated downstream erosion builds.
  Selecting a Color graph port and switching to 3D paints that RGBA map's
  RGB channels over the terrain geometry (per-vertex lighting preserved).
  Selected Snow height, Sea surface and Lake surface also use their own
  coverage masks for an illustrative 3D-only snow/water tint; 2D height
  previews and exported heights remain unchanged.
- Center **2D map**: choose any existing node output port (height,
  wear, deposits, flow, water, sediment, slope, curvature, aspect,
  drainage accumulation, channels, elevation mask, color)
  and inspect the result.
- Top toolbar: Save, Open, Undo, Redo, Export, Rebuild. `Ctrl+S`,
  `Ctrl+O`, `Ctrl+Shift+S` (Save As), `Ctrl+Z`, `Ctrl+Shift+Z` and `Ctrl+Y` shortcuts. Pointer
  drags coalesce into one undo step; history holds the last 64 edits.
  Open uses a native project-file chooser, Save writes the current project path,
  and File → Save As opens a native save chooser; the editable path remains
  available for explicit paths and headless-style workflows. Unsaved edits are
  confirmed before replacing a project. Failed opens preserve the running
  project and filename.
- Export normalized 16-bit grayscale PNG scalar maps, RGB8 PNG color,
  RGBA8 packed material splat weights and
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
- Equalize modifier: deterministic, tie-aware global histogram redistribution with adjustable 0–1 blend strength; preserves original minimum/maximum elevation and raw height units.
- Plates generator with seeded plate regions, independent plate-motion directions,
  collision uplift and divergent rifts. Exports regional height, boundary,
  convergence, divergence, plate ID and continental Mask fields. This is a
  deterministic kinematic terrain primitive, not a million-year tectonic
  reconstruction or a physically coupled lithosphere/mantle solver.
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
- Depth-tested GPU GGX dielectric shading of selected RGBA splat layers in the
  3D viewport: weighted sand, soil, rock and snow albedo/roughness with
  view-dependent highlights. Four color pickers and roughness sliders per
  Material Masks node let authors control the GPU material look, preserved
  in project JSON with legacy defaults. Existing RGB Color nodes retain
  painted colors. A deterministic, tileable four-layer 64² detail texture array
  is sampled with triplanar projection in the fragment shader, avoiding blank
  steep cliffs. It currently supplies neutral albedo/roughness variation;
  authored PBR texture assets, normal maps and IBL remain unimplemented.
  The node's Texture repeats slider controls detail density in world space;
  the viewport Lighting menu controls sun azimuth, elevation and intensity
  independently of the graph's exported Sunlight mask.
- Surface Material Masks node gives four mutually normalized coverage fields
  (sand, soil, rock, snow) from elevation and slope. Outputs can drive typed
  Color Mixers or independent PNG16 material-layer exports. A companion
  albedo Color port bakes the four edited material colors to RGB8 without
  scene lighting, triplanar detail or roughness. The same node
  also exposes a typed RGBA8 splat output (R=sand, G=soil, B=rock, A=snow)
  with a false-color preview and tiled/headless export.
- Derived AO and Sunlight maps: eight-direction horizon occlusion and
  directional diffuse light with terrain self-shadow, both typed Mask outputs
  for color blending or PNG16 export. This is deterministic CPU raster
  lighting, not ray-traced PBR lighting or an editable material graph.
- Sea: static sea-level inundation with actual dry/wet coastline detection,
  flat water surface, raw water depth, wet/shore/beach masks and adjustable
  local coastline smoothing. Shoreline distance uses raster adjacency, not
  a blanket elevation band; no waves, tides, dynamic coastal sediment or
  ocean circulation are implemented.
- Lake: static outlet-bounded basin filling with flat water surfaces, selectable
  fill fraction, minimum basin area and potential depth thresholds. Exports
  original terrain Height, inundated surface Height, depth Water and coverage Mask.
  It is not rainfall/evaporation driven or time-evolving, and volume units require
  a real-world cell area that Gaia does not yet know.
- Snow: seeded altitude-dependent deposition, melt, slope adhesion and conservative
  downslope settling. Outputs snow-covered Height, raw snow_depth Height and
  normalized coverage Mask. Snowfall and melt are separate from settling mass
  conservation; this is not a particle snowfall, temperature or glacier solver.
- Experimental hydraulic erosion: rain, water advection, sediment
  transport, erosion/deposition, evaporation, boundary outflow ledgers.
  Water/solids are tested for mass consistency.
- Native WGSL pointwise blending via wgpu compute on resolutions >=256,
  with CPU fallback when no GPU device was supplied. GPU numerical
  verification on a target machine is still required.

## Bounded-memory regional terrain builds (phase 1)

For selected **local Height graphs** Gaia can now evaluate exact world-grid
regions directly, rather than calculating and retaining an entire heightfield
and then subdividing it:

```powershell
cargo run --release --bin gaia-build -- --demo --port 1:height --output builds/mountain-regions --resolution 4096 --region-tiles 8 --no-obj
```

This example builds the demo's **Mountain node #1**, not its Erosion output
node #3. The command writes rectangular (where needed) overlapping PNG16
images in `STEM.region.tiles/`, a `STEM.region.tiles.json` manifest
with each tile's global sample bounds, and a `STEM.region.report.json`
containing unique-pixel statistics and intermediate sample-memory counts.
Adjacent regions share the same global boundary samples exactly, without
resampling. World resolution is 32–8192; region count is 2–64 per axis. Tile
widths can differ by one sample when the world intervals do not divide evenly.

**Supported ancestor nodes:** Mountain, Noise, Adjust (autolevel off), Blur
(with exact clipped input halos, including nested blurs), and Blend. A
disconnected unrelated node need not be region-compatible. Other nodes,
including hydraulic/thermal erosion, autolevel, materials and global drainage,
fail with a clear error **before** writing output; they are not evaluated
with a hidden full-world fallback. There is deliberately no stitched whole
PNG, OBJ mesh, color port, batch export or GUI region job in this phase.
Use ordinary `--tiles` when you need the legacy export-only
subdivision of an already built global graph. Peak memory in the region
report covers live intermediate float samples; it excludes PNG encoding and
Rust/OS overhead. No claim of fully streamed Gaea-class erosion is made.

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
RGB8 PNG (alpha intentionally omitted). Splat outputs write RGBA8 PNG,
using largest-remainder quantization so each pixel's four weights sum to 255. Every single-port build writes a JSON report
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

- Realistic multi-layer PBR materials, terrain painting,
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
