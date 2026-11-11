//! Bounded-memory, disk-backed 2D hydraulic tile-core execution.
//!
//! Each world sample belongs to exactly one rectangular core. A tile reads
//! a two-sample halo from the *previous committed tick*, enough to calculate
//! the four neighboring donor fluxes needed to update a core cell. Neighbor
//! transfers never become external outlets at an internal tile seam.
//! All output fields are scattered into private scratch files, then swapped
//! together only when every core succeeds. No dense world field is allocated.
//!
//! This remains an experimental CPU numerical path: no region graph/CLI
//! hydraulic support, restartable disk cache, channels or loose-sediment layer.
use crate::hydraulic_exchange::WorldEdge;
use crate::hydraulic_stream::{DiskTransport, face_delta, scratch_file};
use crate::hydrology::{DirectedFlux, HydraulicParameters, MassLedger, donor_fluxes};
use crate::region::Region;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

/// Explicit core area budget; an N×N project therefore requires at least
/// ceil(N/256) cores per axis for a regular square layout. The two-sample
/// input halo increases scratch use only by a fixed amount per tile.
pub const MAX_HYDRAULIC_TILE_EDGE: usize = 256;
const MAX_CORE_AREA: usize = MAX_HYDRAULIC_TILE_EDGE * MAX_HYDRAULIC_TILE_EDGE;

fn row_offset(size: usize, y: usize, x: usize) -> u64 {
    (y as u64 * size as u64 + x as u64) * 4
}

/// Read only the span requested by one tile, not a full world row.
fn read_span(
    file: &mut File, world_size: usize, x: usize, y: usize, width: usize,
) -> Result<Vec<f32>, String> {
    file.seek(SeekFrom::Start(row_offset(world_size, y, x)))
        .map_err(|err| err.to_string())?;
    let mut bytes = vec![0u8; width * 4];
    file.read_exact(&mut bytes).map_err(|err| err.to_string())?;
    Ok(bytes.chunks_exact(4).map(|four| f32::from_le_bytes([
        four[0], four[1], four[2], four[3],
    ])).collect())
}

/// Write a tile's owned core span at its global offset.
fn write_span(
    file: &mut File, world_size: usize, x: usize, y: usize, data: &[f32],
) -> Result<(), String> {
    file.seek(SeekFrom::Start(row_offset(world_size, y, x)))
        .map_err(|err| err.to_string())?;
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for sample in data {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    file.write_all(&bytes).map_err(|err| err.to_string())
}

/// Verify unique, exhaustive ownership without an O(world area) bitmap.
/// At every distinct vertical boundary, the active horizontal core spans
/// must tile [0, N) contiguously and without overlap.
fn validated_cores(size: usize, requested: &[Region]) -> Result<Vec<Region>, String> {
    if requested.is_empty() {
        return Err("Hydraulic tile cores must cover the world".into());
    }
    let mut cores = requested.to_vec();
    let mut cuts = vec![0usize, size];
    for core in &cores {
        core.validate(size)?;
        if core.width > MAX_HYDRAULIC_TILE_EDGE ||
            core.height > MAX_HYDRAULIC_TILE_EDGE ||
            core.width.checked_mul(core.height).is_none_or(|area| area > MAX_CORE_AREA)
        {
            return Err("Hydraulic tile core exceeds the 256x256 memory budget".into());
        }
        cuts.push(core.y);
        cuts.push(core.y + core.height);
    }
    cores.sort_unstable_by_key(|core| (core.y, core.x));
    cuts.sort_unstable();
    cuts.dedup();
    for band in cuts.windows(2) {
        if band[0] == band[1] { continue; }
        let mut active = cores.iter()
            .filter(|core| core.y <= band[0] && core.y + core.height >= band[1])
            .collect::<Vec<_>>();
        active.sort_unstable_by_key(|core| core.x);
        let mut next_x = 0usize;
        for core in active {
            if core.x < next_x { return Err("Hydraulic tile cores overlap".into()); }
            if core.x > next_x { return Err("Hydraulic tile cores leave a gap".into()); }
            next_x = core.x + core.width;
        }
        if next_x != size {
            return Err("Hydraulic tile cores do not cover the world".into());
        }
    }
    Ok(cores)
}

/// Input samples and a clipped two-pixel halo. All are immutable snapshots
/// except for rainfall added to the local water copy before transport.
struct TileHalo {
    left: usize,
    top: usize,
    stride: usize,
    height: Vec<f32>,
    water: Vec<f32>,
    sediment: Vec<f32>,
}

impl TileHalo {
    fn load(
        size: usize, core: Region,
        rock: &mut File, water: &mut File, sediment: &mut File,
        rain: f32,
    ) -> Result<Self, String> {
        let left = core.x.saturating_sub(2);
        let top = core.y.saturating_sub(2);
        let right = (core.x + core.width + 2).min(size);
        let bottom = (core.y + core.height + 2).min(size);
        let stride = right - left;
        let count = stride * (bottom - top);
        let mut out = Self {
            left, top, stride,
            height: Vec::with_capacity(count),
            water: Vec::with_capacity(count),
            sediment: Vec::with_capacity(count),
        };
        for y in top..bottom {
            out.height.extend(read_span(rock, size, left, y, stride)?);
            let mut row = read_span(water, size, left, y, stride)?;
            for v in &mut row {
                *v += rain;
                if !v.is_finite() { return Err("Rainfall overflowed tile water storage".into()); }
            }
            out.water.extend(row);
            out.sediment.extend(read_span(sediment, size, left, y, stride)?);
        }
        Ok(out)
    }

    fn index(&self, x: usize, y: usize) -> usize {
        (y - self.top) * self.stride + x - self.left
    }

    fn head(&self, x: usize, y: usize) -> f32 {
        let i = self.index(x, y);
        self.height[i] + self.water[i]
    }

    /// Donors for the core and its one-pixel neighborhood each have a
    /// complete neighbor-head stencil inside the two-pixel input halo.
    fn donor(&self, x: usize, y: usize, size: usize, edge: WorldEdge) -> DirectedFlux {
        let i = self.index(x, y);
        let head = self.height[i] + self.water[i];
        let external = if edge == WorldEdge::Closed { head } else { 0.0 };
        let heads = [
            if x > 0 { self.head(x - 1, y) } else { external },
            if x + 1 < size { self.head(x + 1, y) } else { external },
            if y > 0 { self.head(x, y - 1) } else { external },
            if y + 1 < size { self.head(x, y + 1) } else { external },
        ];
        donor_fluxes(self.water[i], self.sediment[i], head, heads)
    }

    fn slope(&self, x: usize, y: usize, size: usize) -> f32 {
        let h = self.height[self.index(x, y)];
        let mut slope = 0.0f32;
        if x > 0 { slope = slope.max(h - self.height[self.index(x - 1, y)]); }
        if x + 1 < size { slope = slope.max(h - self.height[self.index(x + 1, y)]); }
        if y > 0 { slope = slope.max(h - self.height[self.index(x, y - 1)]); }
        if y + 1 < size { slope = slope.max(h - self.height[self.index(x, y + 1)]); }
        slope
    }
}

/// Precompute a tile's one-cell donor halo once. The neighboring donor
/// requires the second input halo pixel, but each donor flux is evaluated only
/// once instead of being recomputed for five adjacent cells.
struct FluxHalo {
    left: usize,
    top: usize,
    stride: usize,
    values: Vec<DirectedFlux>,
}

impl FluxHalo {
    fn new(tile: &TileHalo, core: Region, size: usize, edge: WorldEdge) -> Self {
        let left = core.x.saturating_sub(1);
        let top = core.y.saturating_sub(1);
        let right = (core.x + core.width + 1).min(size);
        let bottom = (core.y + core.height + 1).min(size);
        let stride = right - left;
        let mut values = Vec::with_capacity(stride * (bottom - top));
        for y in top..bottom {
            for x in left..right {
                values.push(tile.donor(x, y, size, edge));
            }
        }
        Self { left, top, stride, values }
    }

    fn at(&self, x: usize, y: usize) -> DirectedFlux {
        self.values[(y - self.top) * self.stride + x - self.left]
    }
}

fn validate_params(params: HydraulicParameters) -> Result<(), String> {
    if params.iterations != 1 {
        return Err("Disk hydraulic erosion executes exactly one tick (iterations must be 1)".into());
    }
    if !params.capacity.is_finite() || !(0.0..=20.0).contains(&params.capacity) ||
        !params.erosion_rate.is_finite() || !(0.0..=1.0).contains(&params.erosion_rate) ||
        !params.deposition_rate.is_finite() || !(0.0..=1.0).contains(&params.deposition_rate) ||
        !params.rain.is_finite() || !(0.0..=0.05).contains(&params.rain) ||
        !params.evaporation.is_finite() || !(0.0..=1.0).contains(&params.evaporation)
    {
        return Err("Invalid hydraulic erosion/deposition or weather parameters".into());
    }
    Ok(())
}

impl DiskTransport {
    /// One complete, globally barriered 2D-tile erosion tick. Core list order
    /// is irrelevant: all reads use the old committed snapshots, and cores are
    /// processed in a stable (y,x) order. Each core is owned exactly once.
    ///
    /// RAM is bounded by a tile plus a two-pixel halo and its row scratch,
    /// independent of world area. World-sized *files* are still required.
    pub fn step_eroding_tiles(
        &mut self, requested: &[Region], edge: WorldEdge,
        params: HydraulicParameters, cancelled: impl Fn() -> bool,
    ) -> Result<MassLedger, String> {
        validate_params(params)?;
        let cores = validated_cores(self.size, requested)?;
        if cancelled() { return Err("Build cancelled".into()); }

        let mut next_height = scratch_file()?;
        let mut next_water = scratch_file()?;
        let mut next_sediment = scratch_file()?;
        let mut next_wear = scratch_file()?;
        let mut next_deposits = scratch_file()?;
        let mut next_flow = scratch_file()?;
        let n = self.size;
        let mut ledger = MassLedger {
            rainfall: f64::from(params.rain) * (n * n) as f64,
            ..MassLedger::default()
        };

        for core in cores {
            if cancelled() { return Err("Build cancelled".into()); }
            let halo = TileHalo::load(
                n, core, &mut self.height, &mut self.water, &mut self.sediment,
                params.rain,
            )?;
            let fluxes = FluxHalo::new(&halo, core, n, edge);
            for y in core.y..core.y + core.height {
                if cancelled() { return Err("Build cancelled".into()); }
                let old_wear = if let Some(file) = self.wear.as_mut() {
                    read_span(file, n, core.x, y, core.width)?
                } else { vec![0.0; core.width] };
                let old_deposits = if let Some(file) = self.deposits.as_mut() {
                    read_span(file, n, core.x, y, core.width)?
                } else { vec![0.0; core.width] };
                let mut heights = Vec::with_capacity(core.width);
                let mut waters = Vec::with_capacity(core.width);
                let mut sediments = Vec::with_capacity(core.width);
                let mut wears = Vec::with_capacity(core.width);
                let mut deposits = Vec::with_capacity(core.width);
                let mut flows = Vec::with_capacity(core.width);

                for x in core.x..core.x + core.width {
                    let i = halo.index(x, y);
                    let center = fluxes.at(x, y);
                    let mut dw = 0.0f32;
                    let mut ds = 0.0f32;
                    // Same contribution order as the full-width stripe executor.
                    if y > 0 {
                        let upper = fluxes.at(x, y - 1);
                        let (w, s) = face_delta(
                            (upper.water[3], upper.sediment[3]),
                            (center.water[2], center.sediment[2]), false,
                        )?;
                        dw += w; ds += s;
                    }
                    if x > 0 {
                        let left = fluxes.at(x - 1, y);
                        let (w, s) = face_delta(
                            (left.water[1], left.sediment[1]),
                            (center.water[0], center.sediment[0]), false,
                        )?;
                        dw += w; ds += s;
                    }
                    if x + 1 < n {
                        let right = fluxes.at(x + 1, y);
                        let (w, s) = face_delta(
                            (center.water[1], center.sediment[1]),
                            (right.water[0], right.sediment[0]), true,
                        )?;
                        dw += w; ds += s;
                    }
                    if y + 1 < n {
                        let lower = fluxes.at(x, y + 1);
                        let (w, s) = face_delta(
                            (center.water[3], center.sediment[3]),
                            (lower.water[2], lower.sediment[2]), true,
                        )?;
                        dw += w; ds += s;
                    }
                    if edge == WorldEdge::Open {
                        for (on_edge, slot) in [
                            (x == 0, 0), (x + 1 == n, 1),
                            (y == 0, 2), (y + 1 == n, 3),
                        ] {
                            if !on_edge { continue; }
                            let w = center.water[slot];
                            let s = center.sediment[slot];
                            dw -= w; ds -= s;
                            ledger.water_outflow += f64::from(w);
                            ledger.sediment_outflow += f64::from(s);
                        }
                    }
                    let transported_w = halo.water[i] + dw;
                    let transported_s = halo.sediment[i] + ds;
                    if !transported_w.is_finite() || !transported_s.is_finite() ||
                        transported_w < 0.0 || transported_s < 0.0
                    {
                        return Err("Hydraulic tile transport produced invalid mass".into());
                    }

                    let old_h = halo.height[i];
                    let target = params.capacity
                        * (center.total_water + transported_w * 0.03)
                        * (0.04 + halo.slope(x, y, n));
                    if !target.is_finite() {
                        return Err("Hydraulic tile erosion capacity became nonfinite".into());
                    }
                    let mut new_h = old_h;
                    let mut new_s = transported_s;
                    let mut eroded = 0.0f32;
                    let mut deposited = 0.0f32;
                    if transported_s < target {
                        eroded = ((target - transported_s) * params.erosion_rate).min(old_h);
                        new_h -= eroded;
                        new_s += eroded;
                    } else {
                        deposited = ((transported_s - target) * params.deposition_rate)
                            .min(transported_s);
                        new_h += deposited;
                        new_s -= deposited;
                    }
                    let loss = transported_w * params.evaporation;
                    let new_w = transported_w - loss;
                    let idx = x - core.x;
                    let new_wear = old_wear[idx] + eroded;
                    let new_deposits = old_deposits[idx] + deposited;
                    if [new_h, new_w, new_s, new_wear, new_deposits]
                        .iter().any(|v| !v.is_finite() || *v < 0.0)
                    {
                        return Err("Hydraulic tile erosion produced invalid mass".into());
                    }
                    ledger.evaporated += f64::from(loss);
                    heights.push(new_h);
                    waters.push(new_w);
                    sediments.push(new_s);
                    wears.push(new_wear);
                    deposits.push(new_deposits);
                    flows.push(center.total_water);
                }
                write_span(&mut next_height, n, core.x, y, &heights)?;
                write_span(&mut next_water, n, core.x, y, &waters)?;
                write_span(&mut next_sediment, n, core.x, y, &sediments)?;
                write_span(&mut next_wear, n, core.x, y, &wears)?;
                write_span(&mut next_deposits, n, core.x, y, &deposits)?;
                write_span(&mut next_flow, n, core.x, y, &flows)?;
            }
        }
        if cancelled() { return Err("Build cancelled".into()); }
        // All six files are now complete; swap only after the final barrier.
        self.height = next_height;
        self.water = next_water;
        self.sediment = next_sediment;
        self.wear = Some(next_wear);
        self.deposits = Some(next_deposits);
        self.last_flow = Some(next_flow);
        Ok(ledger)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::HeightField;
    use std::cell::Cell;

    fn fixture(n: usize) -> (HeightField, HeightField, HeightField) {
        let mut h = HeightField::flat(n, 0.0);
        let mut w = HeightField::flat(n, 0.0);
        let mut s = HeightField::flat(n, 0.0);
        for y in 0..n {
            for x in 0..n {
                let i = y * n + x;
                h.values[i] = (n - x) as f32 / n as f32 * 0.7;
                w.values[i] = if (x * 7 + y * 3) % 11 < 3 { 0.38 } else { 0.013 };
                s.values[i] = if (x + y * 2) % 9 < 2 { 0.08 } else { 0.007 };
            }
        }
        (h, w, s)
    }

    fn disk_state(
        h: &HeightField, w: &HeightField, s: &HeightField,
    ) -> DiskTransport {
        let n = h.size;
        DiskTransport::from_rows(n, |y, heights, waters, sediments| {
            heights.copy_from_slice(&h.values[y * n..(y + 1) * n]);
            waters.copy_from_slice(&w.values[y * n..(y + 1) * n]);
            sediments.copy_from_slice(&s.values[y * n..(y + 1) * n]);
            Ok(())
        }).unwrap()
    }

    fn grid(n: usize, across: usize, down: usize) -> Vec<Region> {
        let mut cores = Vec::new();
        for row in 0..down {
            for col in 0..across {
                let x = col * n / across;
                let y = row * n / down;
                let right = (col + 1) * n / across;
                let bottom = (row + 1) * n / down;
                cores.push(Region {
                    x, y, width: right - x, height: bottom - y,
                });
            }
        }
        cores
    }

    fn compare_states(left: &mut DiskTransport, right: &mut DiskTransport) {
        assert_eq!(left.size(), right.size());
        for y in 0..left.size() {
            assert_eq!(left.solid_row(y).unwrap(), right.solid_row(y).unwrap());
            assert_eq!(left.transport_row(y).unwrap(), right.transport_row(y).unwrap());
        }
    }

    #[test]
    fn two_dimensional_tiles_match_stripes_across_seams_and_edges() {
        for n in [32, 65] {
            let (h, w, s) = fixture(n);
            let params = HydraulicParameters { iterations: 1, ..Default::default() };
            for boundary in [WorldEdge::Closed, WorldEdge::Open] {
                let mut reference = disk_state(&h, &w, &s);
                let reference_ledger = reference.step_eroding(
                    7, boundary, params, || false,
                ).unwrap();
                for (across, down) in [(1, 1), (2, 3), (3, 2), (7, 5)] {
                    let base = grid(n, across, down);
                    for reverse in [false, true] {
                        let mut cores = base.clone();
                        if reverse { cores.reverse(); }
                        let mut tiled = disk_state(&h, &w, &s);
                        let ledger = tiled.step_eroding_tiles(
                            &cores, boundary, params, || false,
                        ).unwrap();
                        compare_states(&mut tiled, &mut reference);
                        assert_eq!(ledger.rainfall, reference_ledger.rainfall);
                        assert!((ledger.evaporated - reference_ledger.evaporated).abs() < 1e-8);
                        assert!((ledger.water_outflow - reference_ledger.water_outflow).abs() < 1e-8);
                        assert!((ledger.sediment_outflow - reference_ledger.sediment_outflow).abs() < 1e-8);
                        let solids_error = h.mass() + s.mass() -
                            ledger.sediment_outflow - tiled.solid_mass().unwrap();
                        assert!(solids_error.abs() < 0.001, "solid residual: {solids_error}");
                    }
                }
            }
        }
    }

    #[test]
    fn multiple_tile_layouts_match_stripes_for_twenty_four_ticks() {
        let (h, w, s) = fixture(32);
        let params = HydraulicParameters { iterations: 1, ..Default::default() };
        let mut stripe = disk_state(&h, &w, &s);
        let mut tiled = disk_state(&h, &w, &s);
        let mut reversed = grid(32, 5, 3);
        reversed.reverse();
        let mut out = MassLedger::default();
        for tick in 0..24 {
            let edge = if tick % 2 == 0 { WorldEdge::Open } else { WorldEdge::Closed };
            let baseline = stripe.step_eroding(7, edge, params, || false).unwrap();
            let tiled_ledger = tiled.step_eroding_tiles(
                if tick % 2 == 0 { &reversed } else { &grid(32, 3, 7) },
                edge, params, || false,
            ).unwrap();
            out.water_outflow += tiled_ledger.water_outflow;
            assert!((baseline.water_outflow - tiled_ledger.water_outflow).abs() < 1e-8);
            assert!((baseline.sediment_outflow - tiled_ledger.sediment_outflow).abs() < 1e-8);
            compare_states(&mut stripe, &mut tiled);
        }
        assert!(out.water_outflow > 0.0);
    }

    #[test]
    fn tile_core_boundaries_are_not_world_outlets() {
        let n = 32;
        let h = HeightField::flat(n, 0.0);
        let mut w = HeightField::flat(n, 0.0);
        let mut s = HeightField::flat(n, 0.0);
        w.values[15 * n + 15] = 1.0;
        s.values[15 * n + 15] = 0.4;
        let mut state = disk_state(&h, &w, &s);
        let params = HydraulicParameters {
            iterations: 1, rain: 0.0, evaporation: 0.0,
            capacity: 0.0, erosion_rate: 0.0, deposition_rate: 0.0,
        };
        let ledger = state.step_eroding_tiles(
            &grid(n, 2, 2), WorldEdge::Open, params, || false,
        ).unwrap();
        assert_eq!(ledger.water_outflow, 0.0);
        assert_eq!(ledger.sediment_outflow, 0.0);
        let (water, sediment, _) = state.transport_row(15).unwrap();
        assert!(water[16] > 0.0);
        assert!(sediment[16] > 0.0);
        let (water, sediment, _) = state.transport_row(16).unwrap();
        assert!(water[15] > 0.0);
        assert!(sediment[15] > 0.0);
    }

    #[test]
    fn nonuniform_t_junction_cores_match_the_full_width_reference() {
        let n = 65;
        let (h, w, s) = fixture(n);
        let params = HydraulicParameters { iterations: 1, ..Default::default() };
        // Right-hand neighbors have different vertical boundaries than the
        // left core. A horizontal stripe layout cannot express this partition.
        let mut cores = vec![
            Region { x: 0, y: 0, width: 17, height: n },
            Region { x: 17, y: 0, width: n - 17, height: 19 },
            Region { x: 17, y: 19, width: n - 17, height: n - 19 },
        ];
        let mut baseline = disk_state(&h, &w, &s);
        baseline.step_eroding(7, WorldEdge::Open, params, || false).unwrap();
        let mut tiled = disk_state(&h, &w, &s);
        cores.reverse();
        tiled.step_eroding_tiles(&cores, WorldEdge::Open, params, || false).unwrap();
        compare_states(&mut tiled, &mut baseline);
    }

    #[test]
    fn tile_partition_validation_rejects_gaps_overlaps_and_oversized_cores() {
        let (h, w, s) = fixture(32);
        let params = HydraulicParameters { iterations: 1, ..Default::default() };
        let mut state = disk_state(&h, &w, &s);
        let original = state.solid_mass().unwrap();
        assert!(state.step_eroding_tiles(
            &[], WorldEdge::Closed, params, || false,
        ).unwrap_err().contains("cover"));
        let gap = state.step_eroding_tiles(
            &[Region { x: 0, y: 0, width: 16, height: 32 }],
            WorldEdge::Closed, params, || false,
        ).unwrap_err();
        assert!(gap.contains("cover") || gap.contains("gap"));
        let mut overlaps = grid(32, 2, 2);
        overlaps.push(overlaps[0]);
        assert!(state.step_eroding_tiles(
            &overlaps, WorldEdge::Closed, params, || false,
        ).unwrap_err().contains("overlap"));
        assert_eq!(state.solid_mass().unwrap(), original);
        // A long, skinny 512x8 core must also be rejected: a pixel-area
        // cap alone would not keep the two-sample halo bounded by 260x260.
        assert!(validated_cores(
            512,
            &[Region { x: 0, y: 0, width: 512, height: 8 }],
        ).unwrap_err().contains("budget"));
        let mut large = DiskTransport::from_rows(512, |_, h, w, s| {
            h.fill(0.5);
            w.fill(0.1);
            s.fill(0.01);
            Ok(())
        }).unwrap();
        assert!(large.step_eroding_tiles(
            &[Region { x: 0, y: 0, width: 512, height: 512 }],
            WorldEdge::Closed, params, || false,
        ).unwrap_err().contains("budget"));
    }

    #[test]
    fn cancellation_after_tile_work_keeps_every_committed_field() {
        let (h, w, s) = fixture(32);
        let mut state = disk_state(&h, &w, &s);
        let params = HydraulicParameters { iterations: 1, ..Default::default() };
        state.step_eroding_tiles(&grid(32, 2, 2), WorldEdge::Open, params, || false).unwrap();
        let first = state.solid_row(16).unwrap();
        let water = state.transport_row(16).unwrap();
        let solid_mass = state.solid_mass().unwrap();
        let count = Cell::new(0usize);
        let err = state.step_eroding_tiles(&grid(32, 2, 2), WorldEdge::Open,
            params, || {
                count.set(count.get() + 1);
                count.get() >= 25
            }).unwrap_err();
        assert_eq!(err, "Build cancelled");
        assert!(count.get() >= 25);
        assert_eq!(state.solid_row(16).unwrap(), first);
        assert_eq!(state.transport_row(16).unwrap(), water);
        assert_eq!(state.solid_mass().unwrap(), solid_mass);
    }
}
