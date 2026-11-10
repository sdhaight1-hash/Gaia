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

Development branch: `feat/geological-generators` (stacked on preceding PRs).

To compile from source:

```sh
cargo run --release
```

Requires Rust 1.95+ and a current Vulkan/DX12-capable graphics driver.

## Current workflow

- Left **Node Library**: Mountain, Noise, Mountain Range, Crater, Dunes,
  Canyon, thermal erosion, hydraulic erosion, Terrace, Blur, Slope Mask,
  Curvature, Elevation Mask, Warp, Blend and Masked Blend.
- Bottom graph: click/select and drag nodes; connect compatible named
  outputs to typed inputs in the right **Inspector**.
- Right inspector: change procedural parameters and graph resolution
  (64, 128, 256 or 512); select a height-producing output node.
- Center **3D** preview: left drag orbit, scroll zoom, reset camera.
- Center **2D map**: choose any existing node output port (height,
  wear, deposits, flow, water, sediment, slope, curvature, elevation mask)
  and inspect the result.
- Top toolbar: Save, Open, Export, Rebuild. `Ctrl+S` and `Ctrl+O`
  shortcuts. Save/Open use an editable project path, not file dialogs.
- Export normalised 16-bit grayscale PNG heightmaps, selected scalar
  maps, or OBJ terrain meshes. All project parameters and connection
  references are serialized to JSON.

## Execution and algorithms

- Typed node ports and named multi-output bundles. Old JSON projects
  whose connections are numeric IDs deserialize as height-port links.
- Incremental, signature-based CPU cache; changes to a node cause its
  affected graph path to rebuild, while clean ancestors are reused.
- Dedicated cancellable worker thread. Rapid changes supersede stale
  jobs without blocking UI interaction.
- Conservative thermal talus relaxation with cumulative wear/deposit masks.
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
cargo run --release --bin gaia-build -- --project terrain.gaia.json --output builds/final
cargo run --release --bin gaia-build -- --project terrain.gaia.json --output builds/erosion-wear --port 2:wear --no-obj
```

Every build writes normalized 16-bit PNG plus a JSON report of dimensions,
raw min/max/mean, selected graph port and export metadata. Height outputs
also write OBJ unless `--no-obj` is set. The current supported resolutions
are 32–512, and the build is single-grid (not tiled).

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

- Production-grade depth-buffered preview, on-canvas socket wiring,
  undo/redo, native file dialogs, color materials, terrain painting,
  vegetation, snow/glaciers, lakes, rivers, tiles, large resolutions,
  high-end geology and production exports are not finished.
- Graph cores are currently height/mask/water scalar fields. Color,
  vector, points and mesh port types are reserved but not populated.
- The hydraulic model is an experimental reference implementation,
  **not** numerically or visually identical to Gaea's erosion solvers.
- GPU blending requires a working GPU and needs performance profiling.
  There is no claim that GPU compute is faster for every graph operation.
- The current 3D preview uses sorted triangle cells, not a proper
  depth-buffered render target.
- Windows screenshot/video sign-off is still required for the 1080p UI.

## Source layout

`src/terrain.rs` — core heightfield primitives, project persistence, exports.

`src/pipeline.rs` — typed multi-output graph evaluation and cache.

`src/worker.rs` — cancellable asynchronous evaluation.

`src/geology.rs` — terrain generators, warp and structural masks.

`src/hydrology.rs` — conservative hydraulic erosion and mass ledgers.

`src/gpu_compute.rs`, `src/blend.wgsl` — WGSL compute masked blending.

`src/gpu.rs`, `src/terrain.wgsl` — native wgpu terrain preview.

`src/ui_theme.rs`, `src/main.rs` — modern desktop workspace and controls.

See the [technical roadmap](docs/ROADMAP.md) and
[feature parity matrix](docs/FEATURE_PARITY.md) for the next gates.
