//! Disk-backed, barriered hydraulic water/sediment transport in horizontal stripes.
//!
//! This intentionally does **not** perform rain, erosion, deposition or
//! evaporation and is not wired to regional graph builds. It proves that one
//! transport tick can run with O(world width) resident scalar buffers, using
//! same-tick neighbor snapshots even across stripe boundaries.
//!
//! Temporary input/output files remain private to this state. Failed or
//! cancelled steps discard uncommitted output and preserve the last full tick.
use crate::hydraulic_exchange::WorldEdge;
use crate::hydrology::{DirectedFlux, MassLedger, donor_fluxes};
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
    ledger: &mut MassLedger,
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
        water.push(w);
        sediment.push(s);
        flow.push(donor.total_water);
    }
    write_row(next_water, &water)?;
    write_row(next_sediment, &sediment)?;
    write_row(next_flow, &flow)?;
    Ok(())
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
            if [h.as_slice(), w.as_slice(), s.as_slice()]
                .iter().flatten().any(|v| !v.is_finite() || *v < 0.0)
            {
                return Err("Disk transport rows must contain finite nonnegative values".into());
            }
            write_row(&mut height, &h)?;
            write_row(&mut water, &w)?;
            write_row(&mut sediment, &s)?;
        }
        Ok(Self { size, height, water, sediment, last_flow: None })
    }

    pub fn size(&self) -> usize { self.size }

    fn load_row(&mut self, y: usize) -> Result<FieldRow, String> {
        Ok(FieldRow {
            height: read_row(&mut self.height, self.size, y)?,
            water: read_row(&mut self.water, self.size, y)?,
            sediment: read_row(&mut self.sediment, self.size, y)?,
        })
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
        if stripe_rows == 0 || stripe_rows > self.size {
            return Err("Stripe height must be within 1..=world resolution".into());
        }
        if cancelled() { return Err("Build cancelled".into()); }
        let mut next_water = scratch_file()?;
        let mut next_sediment = scratch_file()?;
        let mut next_flow = scratch_file()?;
        let mut ledger = MassLedger::default();
        let mut previous: Option<FieldRow> = None;
        let mut current = Some(self.load_row(0)?);
        let mut prev_flux: Option<Vec<DirectedFlux>> = None;
        let mut prev_prev_flux: Option<Vec<DirectedFlux>> = None;

        for stripe_start in (0..self.size).step_by(stripe_rows) {
            let stripe_end = stripe_start.saturating_add(stripe_rows).min(self.size);
            for y in stripe_start..stripe_end {
                if cancelled() { return Err("Build cancelled".into()); }
                let next = if y + 1 < self.size {
                    Some(self.load_row(y + 1)?)
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
                        &mut next_water, &mut next_sediment, &mut next_flow, &mut ledger,
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
            &mut next_flow, &mut ledger,
        )?;
        if cancelled() { return Err("Build cancelled".into()); }
        // This is the only commit point. No caller-visible partial iteration.
        self.water = next_water;
        self.sediment = next_sediment;
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
            assert_eq!(w, expected.water.values[y * n..(y + 1) * n]);
            assert_eq!(s, expected.sediment.values[y * n..(y + 1) * n]);
            assert_eq!(flow.unwrap(), expected.flow.values[y * n..(y + 1) * n]);
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
        assert!(state.transport_row(32).is_err());
    }
}
