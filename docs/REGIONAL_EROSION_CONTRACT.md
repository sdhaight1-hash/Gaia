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
