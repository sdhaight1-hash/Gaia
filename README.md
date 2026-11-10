# Gaia

**Gaia** is an independent, open-source **Rust + wgpu terrain editor** targeting
the workflow of node-based terrain design applications. It is not affiliated with
QuadSpinner, does not contain its code/assets, and does not yet reproduce the
full feature set of Gaea.

## Run

Install the [Rust toolchain](https://rustup.rs/) (Rust 1.95 or newer), clone
this repository, and from its root run:

```sh
cargo run --release
```

The application uses a native desktop window and a `wgpu` GPU backend.
Start with the default 128×128 mountain → thermal erosion → terrace graph.

## Current editor features

- Procedural Mountain and Noise generators, plus thermal Erosion, Terrace,
  Blur, and two-input Blend nodes.
- Drag nodes in the graph, click to select, and edit input connections and
  parameters in the left inspector. Set any node as the graph output.
- Orbit the 3D viewport with left mouse drag; scroll to zoom; switch to a
  grayscale 2D heightmap with the Heightmap tab.
- Save / Open a JSON project using the filename field.
- Export normalized **16-bit grayscale PNG** or heightfield **OBJ** mesh using
  the project filename as the output base.
- Select 64, 128, 256 or 512 heightfield resolution (higher resolutions are
  CPU-bound for now).

**Limitations:** erosion is thermal only, not hydraulic. The first wgpu preview
uses sorted surface cells instead of a depth-buffered render target. The UI
rebuilds the graph synchronously, and has not been visually validated on
Windows. Advanced Gaea features are out of scope for this first slice.

## Tests

```sh
cargo test --lib
cargo check --all-targets
```

See [ROADMAP](docs/ROADMAP.md) for phased development, acceptance criteria,
and large-world geological/hydrological plans.

## Architecture

`src/terrain.rs` — serializable procedural graph, heightfields, algorithms and exports.

`src/main.rs` — node editor, inspector, project I/O and preview interaction.

`src/gpu.rs` and `src/terrain.wgsl` — native wgpu terrain drawing.

Code is original and the application is independently designed.
