//! Disk-backed, barriered hydraulic water/sediment transport in horizontal stripes.
//!
//! Rain, transport, erosion/deposition and evaporation can run in globally
//! barriered ticks with O(world width) resident scalar buffers. Height slopes
//! are evaluated from the entire **pre-erosion** tick, not an in-place scan.
//! This module is not yet wired to regional graph builds.
//!
//! Temporary input/output files remain private to this state. Failed or
//! cancelled steps discard uncommitted output and preserve the last full tick.
use crate::hydraulic_exchange::WorldEdge;
use crate::hydrology::{DirectedFlux, HydraulicParameters, MassLedger, donor_fluxes};
use crate::region::MAX_REGION_RESOLUTION;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

struct FieldRow {
    height: Vec<f32>,
    water: Vec<f32>,
    sediment: Vec<f32>,
}

/// File-backed state with no full-world f32 arrays. One file per scalar field.
/// Data is little-endian f32 in row-major world coordinates, never duplicate
/// halo ownership. Files are automatically removed when this state is dropped.
pub struct DiskTransport {
    size: usize,
    height: File,
    water: File,
    sediment: File,
    last_flow: Option<File>,
    wear: Option<File>,
    deposits: Option<File>,
}

fn scratch_file() -> Result<File, String> {
    tempfile::tempfile().map_err(|err| err.to_string())
}

fn write_row(file: &mut File, samples: &[f32]) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for value in samples {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    file.write_all(&bytes).map_err(|err| err.to_string())
}

fn read_row(file: &mut File, size: usize, y: usize) -> Result<Vec<f32>, String> {
    let offset = (y as u64) * (size as u64) * 4;
    file.seek(SeekFrom::Start(offset)).map_err(|err| err.to_string())?;
    let mut bytes = vec![0u8; size * 4];
    file.read_exact(&mut bytes).map_err(|err| err.to_string())?;
    Ok(bytes.chunks_exact(4).map(|chunk| f32::from_le_bytes(
        [chunk[0], chunk[1], chunk[2], chunk[3]],
    )).collect())
}

/// Signed contribution to one cell from a face. Faces are oriented from the
/// lower world index to the higher. Opposite flows cannot both be positive.
fn face_delta(
    forward: (f32, f32), reverse: (f32, f32), target_is_lower: bool,
) -> Result<(f32, f32), String> {
    if forward.0 > 0.0 && reverse.0 > 0.0 {
        return Err("Opposing positive fluxes on a canonical face".into());
    }
    if forward.0 > 0.0 {
        return Ok(if target_is_lower {
            (-forward.0, -forward.1)
        } else {
            forward
        });
    }
    if reverse.0 > 0.0 {
        return Ok(if target_is_lower {
            reverse
        } else {
            (-reverse.0, -reverse.1)
        });
    }
    Ok((0.0, 0.0))
}

fn flux_row(
    y: usize, size: usize,
    previous: Option<&FieldRow>, center: &FieldRow, next: Option<&FieldRow>,
    boundary: WorldEdge,
) -> Vec<DirectedFlux> {
    let mut fluxes = Vec::with_capacity(size);
    for x in 0..size {
        let volume = center.water[x];
        let head = center.height[x] + volume;
        let outside = match boundary {
            WorldEdge::Closed => head,
            WorldEdge::Open => 0.0,
        };
        let left = if x > 0 {
            center.height[x - 1] + center.water[x - 1]
        } else { outside };
        let right = if x + 1 < size {
            center.height[x + 1] + center.water[x + 1]
        } else { outside };
        let up = if let Some(row) = previous {
            row.height[x] + row.water[x]
        } else { outside };
        let down = if let Some(row) = next {
            row.height[x] + row.water[x]
        } else { outside };
        fluxes.push(donor_fluxes(
            volume, center.sediment[x], head, [left, right, up, down],
        ));
    }
    debug_assert!(y < size);
    fluxes
}

/// Reduce faces for a whole row in exactly the canonical world-index order:
/// upper face, left face, right face, lower face, then any true world outlets.
/// This matches the transport-only reference regardless of stripe height.
fn commit_row(
    y: usize, size: usize, old: &FieldRow,
    upper: Option<&[DirectedFlux]>, center: &[DirectedFlux],
    lower: Option<&[DirectedFlux]>, boundary: WorldEdge,
    next_water: &mut File, next_sediment: &mut File, next_flow: &mut File,
    evaporation: f32, ledger: &mut MassLedger,
) -> Result<(), String> {
    let mut water = Vec::with_capacity(size);
    let mut sediment = Vec::with_capacity(size);
    let mut flow = Vec::with_capacity(size);
    for x in 0..size {
        let donor = center[x];
        let mut dw = 0.0f32;
        let mut ds = 0.0f32;
        // Previous row's downward face was reduced before this row's
        // horizontal faces in the single-world canonical traversal.
        if let Some(previous) = upper {
            let (w, s) = face_delta(
                (previous[x].water[3], previous[x].sediment[3]),
                (donor.water[2], donor.sediment[2]), false,
            )?;
            dw += w;
            ds += s;
        }
        if x > 0 {
            let left = center[x - 1];
            let (w, s) = face_delta(
                (left.water[1], left.sediment[1]),
                (donor.water[0], donor.sediment[0]), false,
            )?;
            dw += w;
            ds += s;
        }
        if x + 1 < size {
            let right = center[x + 1];
            let (w, s) = face_delta(
                (donor.water[1], donor.sediment[1]),
                (right.water[0], right.sediment[0]), true,
            )?;
            dw += w;
            ds += s;
        }
        if let Some(next) = lower {
            let (w, s) = face_delta(
                (donor.water[3], donor.sediment[3]),
                (next[x].water[2], next[x].sediment[2]), true,
            )?;
            dw += w;
            ds += s;
        }
        if boundary == WorldEdge::Open {
            for (on_edge, slot) in [
                (x == 0, 0), (x + 1 == size, 1),
                (y == 0, 2), (y + 1 == size, 3),
            ] {
                if on_edge {
                    let w = donor.water[slot];
                    let s = donor.sediment[slot];
                    dw -= w;
                    ds -= s;
                    ledger.water_outflow += f64::from(w);
                    ledger.sediment_outflow += f64::from(s);
                }
            }
        }
        let w = old.water[x] + dw;
        let s = old.sediment[x] + ds;
        if !w.is_finite() || !s.is_finite() || w < 0.0 || s < 0.0 {
            return Err("Hydraulic face exchange produced invalid mass".into());
        }
        let evaporated = w * evaporation;
        let retained = w - evaporated;
        if !retained.is_finite() || retained < 0.0 {
            return Err("Hydraulic evaporation produced invalid water mass".into());
        }
        ledger.evaporated += f64::from(evaporated);
        water.push(retained);
        sediment.push(s);
        flow.push(donor.total_water);
    }
    write_row(next_water, &water)?;
    write_row(next_sediment, &sediment)?;
    write_row(next_flow, &flow)?;
    Ok(())
}

/// Apply rock/suspended-sediment exchange from an immutable height snapshot.
///
/// Transport and water source accounting have already run into separate
/// temporary files. Every row reads *old* terrain plus post-transport water,
/// suspended mass and outgoing flow. The outputs are new temporary files.
/// No previous height, sediment or diagnostic file is modified here.
fn erode_rows(
    size: usize,
    old_height: &mut File,
    transport_water: &mut File, transport_sediment: &mut File,
    transport_flow: &mut File,
    mut old_wear: Option<&mut File>, mut old_deposits: Option<&mut File>,
    params: HydraulicParameters, ledger: &mut MassLedger,
    cancelled: &impl Fn() -> bool,
) -> Result<(File, File, File, File, File), String> {
    let mut next_height = scratch_file()?;
    let mut next_water = scratch_file()?;
    let mut next_sediment = scratch_file()?;
    let mut next_wear = scratch_file()?;
    let mut next_deposits = scratch_file()?;
    let mut above: Option<Vec<f32>> = None;

    for y in 0..size {
        if cancelled() { return Err("Build cancelled".into()); }
        let height = read_row(old_height, size, y)?;
        let below = if y + 1 < size {
            Some(read_row(old_height, size, y + 1)?)
        } else { None };
        let water = read_row(transport_water, size, y)?;
        let sediment = read_row(transport_sediment, size, y)?;
        let flow = read_row(transport_flow, size, y)?;
        let wear = if let Some(file) = old_wear.as_mut() {
            read_row(*file, size, y)?
        } else { vec![0.0; size] };
        let deposits = if let Some(file) = old_deposits.as_mut() {
            read_row(*file, size, y)?
        } else { vec![0.0; size] };

        let mut out_height = Vec::with_capacity(size);
        let mut out_water = Vec::with_capacity(size);
        let mut out_sediment = Vec::with_capacity(size);
        let mut out_wear = Vec::with_capacity(size);
        let mut out_deposits = Vec::with_capacity(size);

        for x in 0..size {
            let h = height[x];
            let suspended = sediment[x];
            let mut slope = 0.0f32;
            if x > 0 { slope = slope.max(h - height[x - 1]); }
            if x + 1 < size { slope = slope.max(h - height[x + 1]); }
            if let Some(row) = above.as_ref() {
                slope = slope.max(h - row[x]);
            }
            if let Some(row) = below.as_ref() {
                slope = slope.max(h - row[x]);
            }
            // Same capacity expression as the original CPU hydraulic solver.
            // Unlike its in-place sweep, every slope uses unchanged old height.
            let capacity = params.capacity
                * (flow[x] + water[x] * 0.03)
                * (0.04 + slope.max(0.0));
            if !capacity.is_finite() {
                return Err("Hydraulic erosion capacity became nonfinite".into());
            }
            let mut new_h = h;
            let mut new_s = suspended;
            let mut eroded = 0.0f32;
            let mut deposited = 0.0f32;
            if suspended < capacity {
                eroded = ((capacity - suspended) * params.erosion_rate).min(h);
                new_h -= eroded;
                new_s += eroded;
            } else {
                deposited = ((suspended - capacity) * params.deposition_rate).min(suspended);
                new_h += deposited;
                new_s -= deposited;
            }
            // Evaporation follows erosion, just as in the full-world solver.
            let loss = water[x] * params.evaporation;
            let new_water = water[x] - loss;
            let cumulative_wear = wear[x] + eroded;
            let cumulative_deposits = deposits[x] + deposited;
            if [new_h, new_s, new_water, cumulative_wear, cumulative_deposits]
                .iter().any(|v| !v.is_finite() || *v < 0.0)
            {
                return Err("Hydraulic erosion produced invalid negative/nonfinite mass".into());
            }
            ledger.evaporated += f64::from(loss);
            out_height.push(new_h);
            out_water.push(new_water);
            out_sediment.push(new_s);
            out_wear.push(cumulative_wear);
            out_deposits.push(cumulative_deposits);
        }
        write_row(&mut next_height, &out_height)?;
        write_row(&mut next_water, &out_water)?;
        write_row(&mut next_sediment, &out_sediment)?;
        write_row(&mut next_wear, &out_wear)?;
        write_row(&mut next_deposits, &out_deposits)?;
        above = Some(height);
    }
    Ok((next_height, next_water, next_sediment, next_wear, next_deposits))
}

impl DiskTransport {
    /// Populate one uniquely owned world row at a time; no dense HeightField
    /// is required. All three slices must be filled with finite nonnegative
    /// values. The caller can later supply rows from regional graph evaluation.
    pub fn from_rows(
        size: usize,
        mut fill: impl FnMut(usize, &mut [f32], &mut [f32], &mut [f32])
            -> Result<(), String>,
    ) -> Result<Self, String> {
        if !(32..=MAX_REGION_RESOLUTION).contains(&size) {
            return Err("Disk transport requires world resolution 32–8192".into());
        }
        let mut height = scratch_file()?;
        let mut water = scratch_file()?;
        let mut sediment = scratch_file()?;
        let mut h = vec![f32::NAN; size];
        let mut w = vec![f32::NAN; size];
        let mut s = vec![f32::NAN; size];
        for y in 0..size {
            h.fill(f32::NAN);
            w.fill(f32::NAN);
            s.fill(f32::NAN);
            fill(y, &mut h, &mut w, &mut s)?;
            if h.iter().chain(&w).chain(&s)
                .any(|v| !v.is_finite() || *v < 0.0)
            {
                return Err("Disk transport rows must contain finite nonnegative values".into());
            }
            write_row(&mut height, &h)?;
            write_row(&mut water, &w)?;
            write_row(&mut sediment, &s)?;
        }
        Ok(Self {
            size, height, water, sediment, last_flow: None,
            wear: None, deposits: None,
        })
    }

    pub fn size(&self) -> usize { self.size }

    fn load_row(&mut self, y: usize) -> Result<FieldRow, String> {
        Ok(FieldRow {
            height: read_row(&mut self.height, self.size, y)?,
            water: read_row(&mut self.water, self.size, y)?,
            sediment: read_row(&mut self.sediment, self.size, y)?,
        })
    }

    /// Read one immutable pre-tick row with rainfall added in scratch memory.
    /// The original backing file is untouched until the entire tick commits.
    fn load_rained_row(&mut self, y: usize, rain: f32) -> Result<FieldRow, String> {
        let mut row = self.load_row(y)?;
        for water in &mut row.water {
            *water += rain;
            if !water.is_finite() {
                return Err("Rainfall overflowed water storage".into());
            }
        }
        Ok(row)
    }

    /// Read a single committed row, optionally including the most recent
    /// tick's outgoing-flow diagnostic. No whole-world materialization.
    pub fn transport_row(
        &mut self, y: usize,
    ) -> Result<(Vec<f32>, Vec<f32>, Option<Vec<f32>>), String> {
        if y >= self.size { return Err("Row outside world".into()); }
        let row = self.load_row(y)?;
        let flow = self.last_flow.as_mut()
            .map(|file| read_row(file, self.size, y)).transpose()?;
        Ok((row.water, row.sediment, flow))
    }

    /// Read committed terrain elevation and cumulative solid-transfer masks.
    /// Prior to the first eroding step both diagnostics are all zeros.
    pub fn solid_row(
        &mut self, y: usize,
    ) -> Result<(Vec<f32>, Vec<f32>, Vec<f32>), String> {
        if y >= self.size { return Err("Row outside world".into()); }
        let height = read_row(&mut self.height, self.size, y)?;
        let wear = if let Some(file) = self.wear.as_mut() {
            read_row(file, self.size, y)?
        } else { vec![0.0; self.size] };
        let deposits = if let Some(file) = self.deposits.as_mut() {
            read_row(file, self.size, y)?
        } else { vec![0.0; self.size] };
        Ok((height, wear, deposits))
    }

    /// Audit rock plus suspended sediment without a full-world allocation.
    /// Any true sediment export belongs in the external mass ledger, not in
    /// cumulative wear/deposit diagnostics (which are transfers, not stocks).
    pub fn solid_mass(&mut self) -> Result<f64, String> {
        let mut total = 0.0f64;
        for y in 0..self.size {
            for value in read_row(&mut self.height, self.size, y)? {
                total += f64::from(value);
            }
            for value in read_row(&mut self.sediment, self.size, y)? {
                total += f64::from(value);
            }
        }
        Ok(total)
    }

    /// f64 audit over persisted snapshots. Does not load whole fields.
    pub fn mass(&mut self) -> Result<(f64, f64), String> {
        let mut water = 0.0f64;
        let mut sediment = 0.0f64;
        for y in 0..self.size {
            let w = read_row(&mut self.water, self.size, y)?;
            let s = read_row(&mut self.sediment, self.size, y)?;
            water += w.into_iter().map(f64::from).sum::<f64>();
            sediment += s.into_iter().map(f64::from).sum::<f64>();
        }
        Ok((water, sediment))
    }

    /// Run exactly one globally barriered water/sediment transport tick.
    /// The stripe size changes scheduling only; neighbor heads always come
    /// from the pre-tick snapshot. The peak scratch floats scale with world
    /// width, not area, and work files hold the next complete state on disk.
    /// Cancellation or any numerical/IO error preserves all committed files.
    pub fn step(
        &mut self, stripe_rows: usize, boundary: WorldEdge,
        cancelled: impl Fn() -> bool,
    ) -> Result<MassLedger, String> {
        self.step_weather(stripe_rows, boundary, 0.0, 0.0, cancelled)
    }

    /// Like step, but rain is a positive external water source applied to
    /// every uniquely owned world cell before flux evaluation, and evaporation
    /// is a water-only loss applied after transport. Both enter the f64 ledger.
    /// Rates follow the current global hydraulic solver's accepted ranges.
    pub fn step_weather(
        &mut self, stripe_rows: usize, boundary: WorldEdge,
        rain: f32, evaporation: f32, cancelled: impl Fn() -> bool,
    ) -> Result<MassLedger, String> {
        self.step_inner(stripe_rows, boundary, rain, evaporation, None, cancelled)
    }

    /// Perform exactly one full rain/transport/rock-exchange/evaporation tick.
    /// Requires iterations == 1: callers must explicitly repeat ticks and
    /// audit mass at the desired cadence. Material exchange uses the existing
    /// hydraulic capacity/rate model with a same-tick frozen slope snapshot.
    ///
    /// This is a file-backed prototype, *not* a supported regional CLI node.
    pub fn step_eroding(
        &mut self, stripe_rows: usize, boundary: WorldEdge,
        params: HydraulicParameters, cancelled: impl Fn() -> bool,
    ) -> Result<MassLedger, String> {
        if params.iterations != 1 {
            return Err("Disk hydraulic erosion executes exactly one tick (iterations must be 1)".into());
        }
        if !params.capacity.is_finite() || !(0.0..=20.0).contains(&params.capacity) ||
            !params.erosion_rate.is_finite() || !(0.0..=1.0).contains(&params.erosion_rate) ||
            !params.deposition_rate.is_finite() ||
            !(0.0..=1.0).contains(&params.deposition_rate)
        {
            return Err("Invalid hydraulic erosion/deposition parameters".into());
        }
        self.step_inner(
            stripe_rows, boundary, params.rain, params.evaporation,
            Some(params), cancelled,
        )
    }

    fn step_inner(
        &mut self, stripe_rows: usize, boundary: WorldEdge,
        rain: f32, evaporation: f32, erosion: Option<HydraulicParameters>,
        cancelled: impl Fn() -> bool,
    ) -> Result<MassLedger, String> {
        if stripe_rows == 0 || stripe_rows > self.size {
            return Err("Stripe height must be within 1..=world resolution".into());
        }
        if !rain.is_finite() || !(0.0..=0.05).contains(&rain) ||
            !evaporation.is_finite() || !(0.0..=1.0).contains(&evaporation)
        {
            return Err("Rain must be 0..=0.05 and evaporation 0..=1".into());
        }
        if cancelled() { return Err("Build cancelled".into()); }
        let mut next_water = scratch_file()?;
        let mut next_sediment = scratch_file()?;
        let mut next_flow = scratch_file()?;
        // Eroding ticks apply evaporation after rock/sediment exchange.
        let transport_evaporation = if erosion.is_some() { 0.0 } else { evaporation };
        let mut ledger = MassLedger {
            rainfall: f64::from(rain) * (self.size * self.size) as f64,
            ..MassLedger::default()
        };
        let mut previous: Option<FieldRow> = None;
        let mut current = Some(self.load_rained_row(0, rain)?);
        let mut prev_flux: Option<Vec<DirectedFlux>> = None;
        let mut prev_prev_flux: Option<Vec<DirectedFlux>> = None;

        for stripe_start in (0..self.size).step_by(stripe_rows) {
            let stripe_end = stripe_start.saturating_add(stripe_rows).min(self.size);
            for y in stripe_start..stripe_end {
                if cancelled() { return Err("Build cancelled".into()); }
                let next = if y + 1 < self.size {
                    Some(self.load_rained_row(y + 1, rain)?)
                } else { None };
                let donors = flux_row(
                    y, self.size, previous.as_ref(), current.as_ref().unwrap(),
                    next.as_ref(), boundary,
                );
                if let (Some(old), Some(center_flux)) =
                    (previous.as_ref(), prev_flux.as_deref())
                {
                    commit_row(
                        y - 1, self.size, old, prev_prev_flux.as_deref(),
                        center_flux, Some(&donors), boundary,
                        &mut next_water, &mut next_sediment, &mut next_flow,
                        transport_evaporation, &mut ledger,
                    )?;
                }
                previous = current.take();
                current = next;
                prev_prev_flux = prev_flux.take();
                prev_flux = Some(donors);
            }
        }
        commit_row(
            self.size - 1, self.size, previous.as_ref().unwrap(),
            prev_prev_flux.as_deref(), prev_flux.as_deref().unwrap(),
            None, boundary, &mut next_water, &mut next_sediment,
            &mut next_flow, transport_evaporation, &mut ledger,
        )?;
        if cancelled() { return Err("Build cancelled".into()); }
        // Optional second barrier: transform rock + suspended solids from the
        // pre-erosion height snapshot. All files are scratch until success.
        if let Some(params) = erosion {
            let (height, water, sediment, wear, deposits) = erode_rows(
                self.size, &mut self.height,
                &mut next_water, &mut next_sediment, &mut next_flow,
                self.wear.as_mut(), self.deposits.as_mut(),
                params, &mut ledger, &cancelled,
            )?;
            if cancelled() { return Err("Build cancelled".into()); }
            // Infallible in-process swap: no field can expose a partial tick.
            self.height = height;
            self.water = water;
            self.sediment = sediment;
            self.wear = Some(wear);
            self.deposits = Some(deposits);
        } else {
            self.water = next_water;
            self.sediment = next_sediment;
        }
        self.last_flow = Some(next_flow);
        Ok(ledger)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydraulic_exchange::transport_snapshot;
    use crate::region::Region;
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

    fn create(
        h: &HeightField, w: &HeightField, s: &HeightField,
    ) -> DiskTransport {
        let n = h.size;
        DiskTransport::from_rows(n, |y, hh, ww, ss| {
            hh.copy_from_slice(&h.values[y * n..(y + 1) * n]);
            ww.copy_from_slice(&w.values[y * n..(y + 1) * n]);
            ss.copy_from_slice(&s.values[y * n..(y + 1) * n]);
            Ok(())
        }).unwrap()
    }

    fn compare(
        state: &mut DiskTransport, expected: &crate::hydraulic_exchange::TransportStep,
    ) {
        let n = state.size();
        for y in 0..n {
            let (w, s, flow) = state.transport_row(y).unwrap();
            assert_eq!(w.as_slice(), &expected.water.values[y * n..(y + 1) * n]);
            assert_eq!(s.as_slice(), &expected.sediment.values[y * n..(y + 1) * n]);
            assert_eq!(flow.unwrap().as_slice(), &expected.flow.values[y * n..(y + 1) * n]);
        }
    }

    #[test]
    fn stripe_sizes_match_canonical_reference_exactly() {
        for n in [32, 65] {
            let (h, w, s) = fixture(n);
            for boundary in [WorldEdge::Open, WorldEdge::Closed] {
                let expected = transport_snapshot(
                    &h, &w, &s,
                    &[Region { x: 0, y: 0, width: n, height: n }],
                    boundary, || false,
                ).unwrap();
                for stripe in [1, 3, 7, 16, n] {
                    let mut state = create(&h, &w, &s);
                    let ledger = state.step(stripe, boundary, || false).unwrap();
                    compare(&mut state, &expected);
                    assert_eq!(ledger.water_outflow, expected.ledger.water_outflow);
                    assert_eq!(ledger.sediment_outflow, expected.ledger.sediment_outflow);
                }
            }
        }
    }

    #[test]
    fn two_hundred_closed_ticks_conserve_without_seam_artifacts() {
        let n = 32;
        let (h, w, s) = fixture(n);
        let mut state = create(&h, &w, &s);
        let initial = state.mass().unwrap();
        let mut reference_water = w;
        let mut reference_sediment = s;
        for tick in 0..200 {
            let reference = transport_snapshot(
                &h, &reference_water, &reference_sediment,
                &[Region { x: 0, y: 0, width: n, height: n }],
                WorldEdge::Closed, || false,
            ).unwrap();
            let ledger = state.step(if tick % 2 == 0 { 3 } else { 7 },
                WorldEdge::Closed, || false).unwrap();
            assert_eq!(ledger.water_outflow, 0.0);
            assert_eq!(ledger.sediment_outflow, 0.0);
            compare(&mut state, &reference);
            reference_water = reference.water;
            reference_sediment = reference.sediment;
        }
        let final_mass = state.mass().unwrap();
        assert!((initial.0 - final_mass.0).abs() < initial.0 * 0.00001);
        assert!((initial.1 - final_mass.1).abs() < initial.1 * 0.00001);
    }

    #[test]
    fn open_world_exports_only_real_world_boundaries_over_multiple_ticks() {
        let n = 32;
        let (h, w, s) = fixture(n);
        let mut state = create(&h, &w, &s);
        let initial = state.mass().unwrap();
        let mut water_out = 0.0f64;
        let mut sediment_out = 0.0f64;
        for _ in 0..30 {
            let ledger = state.step(5, WorldEdge::Open, || false).unwrap();
            water_out += ledger.water_outflow;
            sediment_out += ledger.sediment_outflow;
        }
        let final_mass = state.mass().unwrap();
        assert!(water_out > 0.0);
        assert!(sediment_out > 0.0);
        assert!((initial.0 - final_mass.0 - water_out).abs() < initial.0 * 0.00001);
        assert!((initial.1 - final_mass.1 - sediment_out).abs() < initial.1 * 0.00001);
    }

    #[test]
    fn cancelled_tick_preserves_last_committed_snapshot() {
        let (h, w, s) = fixture(32);
        let mut state = create(&h, &w, &s);
        state.step(4, WorldEdge::Open, || false).unwrap();
        let before = state.mass().unwrap();
        let first_row = state.transport_row(0).unwrap();
        let calls = Cell::new(0usize);
        let failure = state.step(4, WorldEdge::Open, || {
            calls.set(calls.get() + 1);
            calls.get() >= 16
        }).unwrap_err();
        assert_eq!(failure, "Build cancelled");
        assert_eq!(before, state.mass().unwrap());
        assert_eq!(first_row, state.transport_row(0).unwrap());
    }

    #[test]
    fn weather_tick_matches_reference_and_ledger() {
        let n = 32;
        let (h, w, s) = fixture(n);
        let rain = 0.004f32;
        let evaporation = 0.03f32;
        let mut rained = w.clone();
        for value in &mut rained.values { *value += rain; }
        let transported = transport_snapshot(
            &h, &rained, &s,
            &[Region { x: 0, y: 0, width: n, height: n }],
            WorldEdge::Open, || false,
        ).unwrap();
        let mut state = create(&h, &w, &s);
        let ledger = state.step_weather(
            7, WorldEdge::Open, rain, evaporation, || false,
        ).unwrap();
        assert_eq!(ledger.rainfall, f64::from(rain) * (n * n) as f64);
        assert_eq!(ledger.water_outflow, transported.ledger.water_outflow);
        assert_eq!(ledger.sediment_outflow, transported.ledger.sediment_outflow);
        let mut evaporated = 0.0f64;
        for y in 0..n {
            let (actual_water, actual_sediment, actual_flow) =
                state.transport_row(y).unwrap();
            for x in 0..n {
                let idx = y * n + x;
                let before_evap = transported.water.values[idx];
                let loss = before_evap * evaporation;
                evaporated += f64::from(loss);
                assert_eq!(actual_water[x], before_evap - loss);
                assert_eq!(actual_sediment[x], transported.sediment.values[idx]);
                assert_eq!(actual_flow.as_ref().unwrap()[x], transported.flow.values[idx]);
            }
        }
        assert_eq!(ledger.evaporated, evaporated);
        let (final_water, final_sediment) = state.mass().unwrap();
        let water_error = w.mass() + ledger.rainfall - ledger.water_outflow -
            ledger.evaporated - final_water;
        let solid_error = s.mass() - ledger.sediment_outflow - final_sediment;
        assert!(water_error.abs() < 0.0005, "water residual: {water_error}");
        assert!(solid_error.abs() < 0.0005, "solid residual: {solid_error}");
    }

    #[test]
    fn weather_sources_sinks_remain_conservative_across_two_hundred_ticks() {
        let (h, w, s) = fixture(32);
        let mut state = create(&h, &w, &s);
        let (initial_water, initial_sediment) = state.mass().unwrap();
        let mut accumulated_rain = 0.0;
        let mut accumulated_evaporation = 0.0;
        for tick in 0..200 {
            let stripe = if tick % 2 == 0 { 3 } else { 7 };
            let ledger = state.step_weather(
                stripe, WorldEdge::Closed, 0.004, 0.03, || false,
            ).unwrap();
            assert_eq!(ledger.water_outflow, 0.0);
            assert_eq!(ledger.sediment_outflow, 0.0);
            accumulated_rain += ledger.rainfall;
            accumulated_evaporation += ledger.evaporated;
        }
        let (final_water, final_sediment) = state.mass().unwrap();
        let water_error = initial_water + accumulated_rain -
            accumulated_evaporation - final_water;
        assert!(water_error.abs() < 0.004, "water residual: {water_error}");
        assert!((initial_sediment - final_sediment).abs() < 0.001);
    }

    /// Dense independent oracle: same-tick face transport, then rock/suspended
    /// exchange from the frozen initial height and post-transport water.
    fn erosion_reference(
        h: &HeightField, w: &HeightField, s: &HeightField,
        params: HydraulicParameters, boundary: WorldEdge,
    ) -> (HeightField, HeightField, HeightField, HeightField, HeightField, MassLedger) {
        let n = h.size;
        let mut rained = w.clone();
        for value in &mut rained.values { *value += params.rain; }
        let transported = transport_snapshot(
            h, &rained, s,
            &[Region { x: 0, y: 0, width: n, height: n }],
            boundary, || false,
        ).unwrap();
        let mut next_h = h.clone();
        let mut next_w = transported.water.clone();
        let mut next_s = transported.sediment.clone();
        let mut wear = HeightField::flat(n, 0.0);
        let mut deposits = HeightField::flat(n, 0.0);
        let mut ledger = transported.ledger;
        ledger.rainfall = f64::from(params.rain) * (n * n) as f64;
        for y in 0..n {
            for x in 0..n {
                let i = y * n + x;
                let original = h.values[i];
                let mut slope = 0.0f32;
                if x > 0 { slope = slope.max(original - h.values[i - 1]); }
                if x + 1 < n { slope = slope.max(original - h.values[i + 1]); }
                if y > 0 { slope = slope.max(original - h.values[i - n]); }
                if y + 1 < n { slope = slope.max(original - h.values[i + n]); }
                let target = params.capacity
                    * (transported.flow.values[i] + transported.water.values[i] * 0.03)
                    * (0.04 + slope.max(0.0));
                if next_s.values[i] < target {
                    let e = ((target - next_s.values[i]) * params.erosion_rate)
                        .min(original);
                    next_h.values[i] -= e;
                    next_s.values[i] += e;
                    wear.values[i] = e;
                } else {
                    let d = ((next_s.values[i] - target) * params.deposition_rate)
                        .min(next_s.values[i]);
                    next_h.values[i] += d;
                    next_s.values[i] -= d;
                    deposits.values[i] = d;
                }
                let loss = transported.water.values[i] * params.evaporation;
                next_w.values[i] -= loss;
                ledger.evaporated += f64::from(loss);
            }
        }
        (next_h, next_w, next_s, wear, deposits, ledger)
    }

    #[test]
    fn eroded_tick_matches_frozen_slope_oracle_on_all_stripe_layouts() {
        for n in [32, 65] {
            let (h, w, s) = fixture(n);
            let params = HydraulicParameters {
                iterations: 1, ..HydraulicParameters::default()
            };
            for boundary in [WorldEdge::Closed, WorldEdge::Open] {
                let (expected_h, expected_w, expected_s, wear, deposits, reference_ledger) =
                    erosion_reference(&h, &w, &s, params, boundary);
                for stripe in [1, 3, 7, n] {
                    let mut state = create(&h, &w, &s);
                    let ledger = state.step_eroding(stripe, boundary, params, || false).unwrap();
                    assert_eq!(ledger.rainfall, reference_ledger.rainfall);
                    assert_eq!(ledger.evaporated, reference_ledger.evaporated);
                    assert_eq!(ledger.water_outflow, reference_ledger.water_outflow);
                    assert_eq!(ledger.sediment_outflow, reference_ledger.sediment_outflow);
                    for y in 0..n {
                        let range = y * n..(y + 1) * n;
                        let (rock, wrow, drow) = state.solid_row(y).unwrap();
                        let (water, sediment, flow) = state.transport_row(y).unwrap();
                        assert_eq!(rock.as_slice(), &expected_h.values[range.clone()]);
                        assert_eq!(water.as_slice(), &expected_w.values[range.clone()]);
                        assert_eq!(sediment.as_slice(), &expected_s.values[range.clone()]);
                        assert_eq!(wrow.as_slice(), &wear.values[range.clone()]);
                        assert_eq!(drow.as_slice(), &deposits.values[range]);
                        assert!(flow.unwrap().iter().all(|v| *v >= 0.0));
                    }
                    let solid_error = h.mass() + s.mass() - ledger.sediment_outflow
                        - state.solid_mass().unwrap();
                    assert!(solid_error.abs() < 0.0008, "solids residual: {solid_error}");
                }
            }
        }
    }

    #[test]
    fn two_hundred_eroding_ticks_conserve_rock_and_suspended_sediment() {
        let (h, w, s) = fixture(32);
        let mut state = create(&h, &w, &s);
        let initial_solid = state.solid_mass().unwrap();
        let (initial_water, _) = state.mass().unwrap();
        let params = HydraulicParameters {
            iterations: 1, ..HydraulicParameters::default()
        };
        let mut rain = 0.0f64;
        let mut evaporated = 0.0f64;
        for tick in 0..200 {
            let ledger = state.step_eroding(
                if tick % 2 == 0 { 3 } else { 7 },
                WorldEdge::Closed, params, || false,
            ).unwrap();
            assert_eq!(ledger.water_outflow, 0.0);
            assert_eq!(ledger.sediment_outflow, 0.0);
            rain += ledger.rainfall;
            evaporated += ledger.evaporated;
        }
        let (final_water, _) = state.mass().unwrap();
        let final_solid = state.solid_mass().unwrap();
        assert!((initial_water + rain - evaporated - final_water).abs() < 0.005);
        assert!((initial_solid - final_solid).abs() < initial_solid * 0.00003);
        let mut total_wear = 0.0f64;
        let mut total_deposits = 0.0f64;
        for y in 0..32 {
            let (height, wear, deposits) = state.solid_row(y).unwrap();
            assert!(height.iter().all(|v| *v >= 0.0 && v.is_finite()));
            total_wear += wear.into_iter().map(f64::from).sum::<f64>();
            total_deposits += deposits.into_iter().map(f64::from).sum::<f64>();
        }
        assert!(total_wear > 0.0);
        assert!(total_deposits > 0.0);
    }

    #[test]
    fn eroded_results_are_independent_of_stripe_height_across_ticks() {
        let (h, w, s) = fixture(32);
        let mut narrow = create(&h, &w, &s);
        let mut wide = create(&h, &w, &s);
        let params = HydraulicParameters { iterations: 1, ..Default::default() };
        for _ in 0..24 {
            let a = narrow.step_eroding(1, WorldEdge::Open, params, || false).unwrap();
            let b = wide.step_eroding(11, WorldEdge::Open, params, || false).unwrap();
            assert_eq!(a.water_outflow, b.water_outflow);
            assert_eq!(a.sediment_outflow, b.sediment_outflow);
            assert_eq!(a.evaporated, b.evaporated);
        }
        for y in 0..32 {
            assert_eq!(narrow.transport_row(y).unwrap(), wide.transport_row(y).unwrap());
            assert_eq!(narrow.solid_row(y).unwrap(), wide.solid_row(y).unwrap());
        }
    }

    #[test]
    fn cancellation_during_erosion_preserves_all_committed_fields() {
        let (h, w, s) = fixture(32);
        let mut state = create(&h, &w, &s);
        let params = HydraulicParameters { iterations: 1, ..Default::default() };
        state.step_eroding(4, WorldEdge::Open, params, || false).unwrap();
        let before_mass = state.mass().unwrap();
        let before_solid_mass = state.solid_mass().unwrap();
        let before_solid_row = state.solid_row(9).unwrap();
        let before_transport = state.transport_row(9).unwrap();
        let count = Cell::new(0usize);
        let err = state.step_eroding(4, WorldEdge::Closed, params, || {
            count.set(count.get() + 1);
            count.get() >= 40
        }).unwrap_err();
        assert_eq!(err, "Build cancelled");
        assert!(count.get() >= 40);
        assert_eq!(state.mass().unwrap(), before_mass);
        assert_eq!(state.solid_mass().unwrap(), before_solid_mass);
        assert_eq!(state.solid_row(9).unwrap(), before_solid_row);
        assert_eq!(state.transport_row(9).unwrap(), before_transport);
    }

    #[test]
    fn rejects_invalid_erosion_parameters_without_mutating_state() {
        let (h, w, s) = fixture(32);
        let mut state = create(&h, &w, &s);
        let prior = state.solid_mass().unwrap();
        for bad in [
            HydraulicParameters::default(),
            HydraulicParameters { iterations: 1, capacity: f32::NAN, ..Default::default() },
            HydraulicParameters { iterations: 1, erosion_rate: -0.1, ..Default::default() },
            HydraulicParameters { iterations: 1, deposition_rate: 1.1, ..Default::default() },
            HydraulicParameters { iterations: 1, rain: f32::INFINITY, ..Default::default() },
        ] {
            assert!(state.step_eroding(4, WorldEdge::Closed, bad, || false).is_err());
        }
        assert_eq!(state.solid_mass().unwrap(), prior);
        assert_eq!(state.solid_row(0).unwrap().0, h.values[..32]);
    }

    #[test]
    fn validation_rejects_incomplete_or_invalid_rows() {
        assert!(DiskTransport::from_rows(31, |_, _, _, _| Ok(())).is_err());
        assert!(DiskTransport::from_rows(32, |_, h, w, s| {
            h.fill(0.0);
            w.fill(0.0);
            if !s.is_empty() { s[0] = f32::NAN; }
            Ok(())
        }).is_err());
        let (h, w, s) = fixture(32);
        let mut state = create(&h, &w, &s);
        assert!(state.step(0, WorldEdge::Closed, || false).is_err());
        assert!(state.step(33, WorldEdge::Closed, || false).is_err());
        assert!(state.step_weather(3, WorldEdge::Closed, f32::NAN, 0.1,
            || false).is_err());
        assert!(state.step_weather(3, WorldEdge::Closed, 0.01, 1.1,
            || false).is_err());
        assert!(state.transport_row(32).is_err());
    }
}
