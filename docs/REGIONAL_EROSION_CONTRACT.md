# Conservative regional hydraulic erosion — execution contract

**Status:** design and acceptance contract, not an implemented regional hydraulic solver.
The existing `src/hydrology.rs` simulation is monolithic, uses global full-grid buffers
and currently assumes open outer-world boundaries with downstream head zero.
Do not claim regional erosion is supported until the gates below pass.

## World-cell and face ownership

- Each world cell `(x,y)` belongs to exactly one region core. Export PNGs may
  duplicate a one-sample border, but simulation must never count it twice.
- Canonical interior face ID: lower global cell index plus X or Y axis.
  Compute each interior transfer exactly once; debit one owner and credit
  its neighbor by precisely the same water/sediment flux.
- Halos are read-only snapshots of neighboring cells from the *same tick*.
  Halo cells never create rain, sediment, outflows, or region-owned mass.
- Partition seams are always **internal** faces, never open drainage outlets.
- External world edges have explicit closed (zero flux), open (ledgered outflow),
  and eventual prescribed inflow (ledgered source) policies. The current
  implementation supports the open-edge behavior only.

## Barriered global simulation tick

1. Snapshot globally committed state N and add rain/inflow to uniquely owned
   cells, recording external sources only once.
2. Read the same global snapshot and halos to compute hydraulic head, donor
   water availability and outgoing capacity on each face.
3. Calculate paired interior face fluxes and explicit world-edge fluxes.
   Donor-limited water and sediment transfer must not go negative.
4. Exchange boundary fluxes and reduce them in a deterministic global-face
   order into next-state buffers; commit only when every region finishes.
5. Apply erosion/deposition using conservative rock-to-sediment and
   sediment-to-rock transfers; apply evaporation as water-only loss.
6. Audit f64-accumulated totals, check finite/nonnegative state and either
   commit a complete tick or abort cleanly on cancellation.

A region cannot simulate all its iterations in isolation: water propagates
across region boundaries over time. Watersheds, global depression filling
and lake routing may require larger context buckets, not finite static halos.

## Conservation invariants

- Water residual = initial water + rainfall + inflow - evaporation -
  external outflow - final water.
- Solids residual = initial (bedrock + loose + suspended) + solid inflow -
  exported sediment - final (bedrock + loose + suspended).
- Every interior transfer is entered with opposite sign exactly once at
  the two cells; no interior face contributes to external outflow ledgers.
- Never conceal missing mass by unaccounted max(0) corrections; any clamp
  must be audited as an explicit numerical correction.
- Use f64 global mass sums with both absolute and relative residuals.
  Calibrate tolerances using actual 32/64/128 baseline experiments rather
  than choosing an arbitrary single absolute tolerance.

## Acceptance matrix before graph integration

| Fixture | Check |
| --- | --- |
| Flat closed world, rainfall | No exports; mass remains conserved |
| Enclosed pit or lake | No artificial outlets at tile seams |
| Sloped world, open edge | Only actual outer-edge outflow enters ledgers |
| Two and four tile water/sediment crossings | Paired face transfers exact |
| Mixed closed/open edges | Policy-specific external ledgers correct |
| Erosion and deposition | Rock + loose + suspended conserved |
| 1, 2, 3 and 7 tiles per axis | Same reference within calibrated tolerance |
| Reverse region order | Deterministic full state and mass ledgers |
| Cancel during exchange / resume | No partially committed tick |
| 200 iterations, seed/parameter sweeps | Finite nonnegative water/solids |
| 4K/8K target Windows hardware | Measured peak process RSS and wall time |

## Implemented experimental stripe checkpoint (2026-11-11)

`src/hydraulic_stream.rs` now provides a file-backed
horizontal-stripe scheduler. It accepts finite/nonnegative height/water/sediment
rows, writes each field to temporary little-endian f32 backing files, reads
same-tick neighboring rows to calculate the existing canonical donor fluxes,
and writes a complete next-step water/sediment/flow state to new temporary
files. A successful tick swaps the files atomically at the in-process state
level; errors and cancellation preserve the prior committed tick. Stripe size
changes work scheduling, not hydraulic boundary behavior.

This prototype's resident scalar scratch space is O(world width), rather than
O(world area). The `step_weather` method supports pre-transport rainfall and
post-transport evaporation with f64 mass ledgers. The `step_eroding` method
adds a conservative per-cell rock/suspended sediment exchange based on the
existing global hydraulic capacity expression. Each one-tick call:
1. Adds rainfall to owned cells using scratch row snapshots.
2. Transfers water and suspended sediment on canonical interior faces.
3. Recomputes erosion/deposition using the **unchanged pre-tick height**
   (one-row vertical halos) and post-transport water/sediment.
4. Evaporates water, writes cumulative wear/deposits diagnostics and validates
   nonnegative, finite output before committing all replacement files.

Because all slope neighbors read the same old height snapshot, results are
deterministic across stripe sizes. The original full-world `simulate` performs
in-place height updates during erosion; exact pixelwise equivalence with that
legacy sweep is not claimed. The new frozen-slope behavior is an explicit
numerical difference, backed by an independent dense reference fixture.

**Still not full regional hydraulic erosion:** no loose surface sediment,
river/watershed coupling, arbitrary two-dimensional region ownership,
persistent/restartable disk caches or CLI/GUI graph execution. The normal
regional CLI must continue rejecting Hydraulic. Also, 4K/8K disk/CPU/RSS
benchmarks have not been run on the target hardware.
The normal region CLI must keep rejecting global hydraulic operations.

## Restartable disk-state checkpoint checkpoint (experimental)

`src/hydraulic_checkpoint.rs` adds `DiskTransport::save_checkpoint`
and `DiskTransport::open_checkpoint`. A checkpoint is an immutable,
versioned directory containing up to six little-endian f32 full-world field
files and a small `manifest.json`. Height, water and suspended sediment
are mandatory; outgoing-flow is optional before a first tick; cumulative
wear and deposits must both be present or both absent. Saving and loading
stream one world row at a time and retain **O(world width)** memory cost.

Saving requires a **new, non-existent destination** inside an existing
parent directory. Files are staged in a sibling temporary directory,
validated for finite/nonnegative samples, flushed, and published with a
same-filesystem directory rename. Cancellation/error discards the staged
directory. Loading validates the manifest version, 32–8192 resolution,
exact field sizes, nonnegative/finite samples, and a deterministic FNV-1a
checksum for each present file before constructing a separate scratch
state. FNV-1a detects accidental corruption, **not malicious tampering**.

This is a simple manual checkpoint API, not continuous durable storage:
there is no multi-generation journal or undo history, guaranteed
power-loss recovery, compression, unattended autosave, disk-space
reservation, or UI/CLI project-format integration. On-disk field sizes
are large; six full 8192² float fields require about 1.5 GiB per
checkpoint, plus transient staging space. Tests exercise save/load
equivalence, restart/continuation, optional field presence,
corruption/truncation, cancellation cleanup, existing destination
preservation and version rejection.

## Narrow implementation sequence

1. Extract a pure canonical interior-face flux operator from the existing
   hydraulic model, retain its CPU monolithic reference, and test exact
   debit/credit conservation without new engine abstractions.
2. Introduce owned-core regions and a two-region water-only face exchange
   fixture. Compare against the same single-region tick semantics.
3. Add suspended sediment exchange, outer-world policy ledgers, deterministic
   reduction, and cancellation without partial commits.
4. Bring erosion/deposition and bucket/global watershed behavior under those
   invariants, then expose regional hydraulic nodes to the CLI/GUI.
5. Profile actual process RSS with `scripts/benchmark-regions.ps1` and add
   restart/disk-cache tests before claiming large-world acceptance.

Until then `gaia-build --region-tiles` must explicitly reject Erosion,
Hydraulic and Drainage rather than silently falling back to a full-grid build.

Related: Issues #3 and #5.
