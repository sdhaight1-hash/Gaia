//! Two-dimensional, same-tick hydraulic face exchange prototype.
//!
//! This isolates canonical face ownership and region-core bookkeeping from
//! erosion/deposition. Inputs are full-world immutable snapshots: **this is a
//! numerical fixture, not a bounded-memory regional erosion implementation**.
//! No graph or regional CLI support is enabled by this module.
use crate::hydrology::{DirectedFlux, MassLedger, donor_fluxes};
use crate::region::{Region, MAX_REGION_RESOLUTION};
use crate::terrain::HeightField;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldEdge {
    Closed,
    Open,
}

#[derive(Debug)]
pub struct TransportStep {
    pub water: HeightField,
    pub sediment: HeightField,
    /// Outward water flux per uniquely owned cell, including world outlets.
    pub flow: HeightField,
    /// Only true external world-edge exports are recorded here.
    pub ledger: MassLedger,
}

/// Apply one canonical interior face exactly once. Adjacent cells receive
/// opposite deltas computed from the same water/sediment values.
fn apply_face(
    a: usize, b: usize, forward: (f32, f32), reverse: (f32, f32),
    delta_water: &mut [f32], delta_sediment: &mut [f32],
) -> Result<(), String> {
    if forward.0 > 0.0 && reverse.0 > 0.0 {
        return Err("Opposing positive fluxes on a canonical face".into());
    }
    let (donor, receiver, water, sediment) = if forward.0 > 0.0 {
        (a, b, forward.0, forward.1)
    } else {
        (b, a, reverse.0, reverse.1)
    };
    if water > 0.0 {
        delta_water[donor] -= water;
        delta_water[receiver] += water;
        delta_sediment[donor] -= sediment;
        delta_sediment[receiver] += sediment;
    }
    Ok(())
}

/// Evaluate a transport-only snapshot using nonoverlapping, collectively
/// exhaustive region cores. Core borders do not duplicate ownership: any
/// duplicated pixel borders in exported tiles are deliberately excluded.
///
/// Fluxes read same-tick world data and are reduced in canonical world-face
/// order, independent of region scheduling. This reference allocates world-size
/// buffers and intentionally does not yet integrate the erosion solver.
pub fn transport_snapshot(
    height: &HeightField,
    water: &HeightField,
    sediment: &HeightField,
    owned_cores: &[Region],
    outer_edge: WorldEdge,
    cancelled: impl Fn() -> bool,
) -> Result<TransportStep, String> {
    let n = height.size;
    if !(32..=MAX_REGION_RESOLUTION).contains(&n) {
        return Err("Exchange fixture requires world resolution 32–8192".into());
    }
    let cells = n.checked_mul(n).ok_or("World dimensions overflow")?;
    for field in [height, water, sediment] {
        if field.size != n || field.values.len() != cells ||
            field.values.iter().any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err("Exchange requires matching finite, nonnegative square fields".into());
        }
    }
    if cancelled() { return Err("Build cancelled".into()); }

    let mut claimed = vec![false; cells];
    let mut donors = vec![DirectedFlux::default(); cells];
    for core in owned_cores {
        core.validate(n)?;
        for y in core.y..core.y + core.height {
            if cancelled() { return Err("Build cancelled".into()); }
            for x in core.x..core.x + core.width {
                let i = y * n + x;
                if claimed[i] {
                    return Err("Simulation region cores overlap".into());
                }
                claimed[i] = true;
                let head = height.values[i] + water.values[i];
                let edge_head = match outer_edge {
                    WorldEdge::Closed => head,
                    WorldEdge::Open => 0.0,
                };
                let neighbors = [
                    if x > 0 { height.values[i - 1] + water.values[i - 1] } else { edge_head },
                    if x + 1 < n { height.values[i + 1] + water.values[i + 1] } else { edge_head },
                    if y > 0 { height.values[i - n] + water.values[i - n] } else { edge_head },
                    if y + 1 < n { height.values[i + n] + water.values[i + n] } else { edge_head },
                ];
                donors[i] = donor_fluxes(water.values[i], sediment.values[i], head, neighbors);
            }
        }
    }
    if claimed.iter().any(|&owned| !owned) {
        return Err("Simulation region cores do not cover the world".into());
    }

    let mut delta_water = vec![0.0f32; cells];
    let mut delta_sediment = vec![0.0f32; cells];
    let mut flow = HeightField::flat(n, 0.0);
    let mut ledger = MassLedger::default();
    // All exchanges are committed together after this loop. No input field
    // is mutated, so cancellation cannot expose a partly completed tick.
    for y in 0..n {
        if cancelled() { return Err("Build cancelled".into()); }
        for x in 0..n {
            let i = y * n + x;
            let donor = donors[i];
            flow.values[i] = donor.total_water;
            // Canonical interior faces: lower world index, then X/Y axis.
            if x + 1 < n {
                let j = i + 1;
                apply_face(i, j,
                    (donor.water[1], donor.sediment[1]),
                    (donors[j].water[0], donors[j].sediment[0]),
                    &mut delta_water, &mut delta_sediment)?;
            }
            if y + 1 < n {
                let j = i + n;
                apply_face(i, j,
                    (donor.water[3], donor.sediment[3]),
                    (donors[j].water[2], donors[j].sediment[2]),
                    &mut delta_water, &mut delta_sediment)?;
            }
            if outer_edge == WorldEdge::Open {
                // Only genuine exterior boundaries can export mass.
                for (on_edge, slot) in [
                    (x == 0, 0), (x + 1 == n, 1),
                    (y == 0, 2), (y + 1 == n, 3),
                ] {
                    if !on_edge { continue; }
                    let w = donor.water[slot];
                    let s = donor.sediment[slot];
                    delta_water[i] -= w;
                    delta_sediment[i] -= s;
                    ledger.water_outflow += f64::from(w);
                    ledger.sediment_outflow += f64::from(s);
                }
            }
        }
    }

    let mut next_water = HeightField::flat(n, 0.0);
    let mut next_sediment = HeightField::flat(n, 0.0);
    for i in 0..cells {
        if i % (n * 16) == 0 && cancelled() {
            return Err("Build cancelled".into());
        }
        let w = water.values[i] + delta_water[i];
        let s = sediment.values[i] + delta_sediment[i];
        // No silent clipping: numerical debt would be a conservation bug.
        if !w.is_finite() || !s.is_finite() || w < 0.0 || s < 0.0 {
            return Err("Hydraulic face exchange produced invalid mass".into());
        }
        next_water.values[i] = w;
        next_sediment.values[i] = s;
    }
    Ok(TransportStep {
        water: next_water, sediment: next_sediment, flow, ledger,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(n: usize) -> Region {
        Region { x: 0, y: 0, width: n, height: n }
    }

    fn partition(n: usize, tiles: usize) -> Vec<Region> {
        let mut result = Vec::new();
        for row in 0..tiles {
            for col in 0..tiles {
                let left = col * n / tiles;
                let right = (col + 1) * n / tiles;
                let top = row * n / tiles;
                let bottom = (row + 1) * n / tiles;
                result.push(Region {
                    x: left, y: top,
                    width: right - left, height: bottom - top,
                });
            }
        }
        result
    }

    fn fixture(n: usize) -> (HeightField, HeightField, HeightField) {
        let mut height = HeightField::flat(n, 0.0);
        let mut water = HeightField::flat(n, 0.0);
        let mut sediment = HeightField::flat(n, 0.0);
        for y in 0..n {
            for x in 0..n {
                let i = y * n + x;
                height.values[i] = ((n - x) as f32 / n as f32) * 0.6;
                water.values[i] = if (x + y) % 3 == 0 { 0.3 } else { 0.02 };
                sediment.values[i] = if (2 * x + y) % 5 == 0 { 0.12 } else { 0.015 };
            }
        }
        (height, water, sediment)
    }

    fn assert_conserved(
        initial_water: &HeightField, initial_sediment: &HeightField, output: &TransportStep,
    ) {
        let water_error = initial_water.mass() - output.water.mass() - output.ledger.water_outflow;
        let sediment_error = initial_sediment.mass() - output.sediment.mass()
            - output.ledger.sediment_outflow;
        assert!(water_error.abs() < 0.0005, "water residual {water_error}");
        assert!(sediment_error.abs() < 0.0005, "sediment residual {sediment_error}");
    }

    #[test]
    fn tile_schedules_match_single_region_and_reverse_order() {
        let n = 32;
        let (height, water, sediment) = fixture(n);
        for edge in [WorldEdge::Closed, WorldEdge::Open] {
            let expected = transport_snapshot(
                &height, &water, &sediment, &[whole(n)], edge, || false,
            ).unwrap();
            assert_conserved(&water, &sediment, &expected);
            for tiles in [2, 3, 7] {
                let cores = partition(n, tiles);
                let mut reversed = cores.clone();
                reversed.reverse();
                for layout in [&cores, &reversed] {
                    let got = transport_snapshot(
                        &height, &water, &sediment, layout, edge, || false,
                    ).unwrap();
                    assert_eq!(got.water, expected.water);
                    assert_eq!(got.sediment, expected.sediment);
                    assert_eq!(got.flow, expected.flow);
                    assert_eq!(got.ledger.water_outflow, expected.ledger.water_outflow);
                    assert_eq!(got.ledger.sediment_outflow, expected.ledger.sediment_outflow);
                }
            }
        }
    }

    #[test]
    fn transfers_sediment_across_seam_without_external_outflow() {
        let n = 32;
        let height = HeightField::flat(n, 0.0);
        let mut water = HeightField::flat(n, 0.0);
        let mut sediment = HeightField::flat(n, 0.0);
        water.values[11 * n + 15] = 1.0;
        sediment.values[11 * n + 15] = 0.4;
        let cores = [
            Region { x: 0, y: 0, width: 16, height: n },
            Region { x: 16, y: 0, width: 16, height: n },
        ];
        let result = transport_snapshot(
            &height, &water, &sediment, &cores, WorldEdge::Open, || false,
        ).unwrap();
        assert!(result.water.get(16, 11) > 0.0);
        assert!(result.sediment.get(16, 11) > 0.0);
        assert_eq!(result.ledger.water_outflow, 0.0);
        assert_eq!(result.ledger.sediment_outflow, 0.0);
        assert_conserved(&water, &sediment, &result);
    }

    #[test]
    fn closed_outer_edges_do_not_export_but_open_edges_do() {
        let n = 32;
        let height = HeightField::flat(n, 0.0);
        let mut water = HeightField::flat(n, 0.0);
        let mut sediment = HeightField::flat(n, 0.0);
        water.values[0] = 1.0;
        sediment.values[0] = 0.6;
        let closed = transport_snapshot(
            &height, &water, &sediment, &[whole(n)], WorldEdge::Closed, || false,
        ).unwrap();
        let open = transport_snapshot(
            &height, &water, &sediment, &[whole(n)], WorldEdge::Open, || false,
        ).unwrap();
        assert_eq!(closed.ledger.water_outflow, 0.0);
        assert_eq!(closed.ledger.sediment_outflow, 0.0);
        assert!(open.ledger.water_outflow > 0.0);
        assert!(open.ledger.sediment_outflow > 0.0);
        assert_conserved(&water, &sediment, &closed);
        assert_conserved(&water, &sediment, &open);
    }

    #[test]
    fn level_water_on_closed_flat_world_does_not_move() {
        let n = 32;
        let height = HeightField::flat(n, 0.0);
        let water = HeightField::flat(n, 0.7);
        let sediment = HeightField::flat(n, 0.2);
        let output = transport_snapshot(
            &height, &water, &sediment, &partition(n, 7), WorldEdge::Closed, || false,
        ).unwrap();
        assert_eq!(output.water, water);
        assert_eq!(output.sediment, sediment);
        assert!(output.flow.values.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn rejects_overlapping_missing_and_invalid_region_cores() {
        let n = 32;
        let (h, w, s) = fixture(n);
        let left = Region { x: 0, y: 0, width: 16, height: n };
        let right = Region { x: 16, y: 0, width: 16, height: n };
        assert!(transport_snapshot(
            &h, &w, &s, &[left], WorldEdge::Closed, || false,
        ).unwrap_err().contains("cover"));
        assert!(transport_snapshot(
            &h, &w, &s, &[whole(n), right], WorldEdge::Closed, || false,
        ).unwrap_err().contains("overlap"));
        assert!(transport_snapshot(
            &h, &w, &s, &[Region { width: 0, ..left }, right],
            WorldEdge::Closed, || false,
        ).is_err());
        let bad = HeightField { size: n, values: vec![] };
        assert!(transport_snapshot(
            &h, &bad, &s, &[whole(n)], WorldEdge::Closed, || false,
        ).is_err());
    }

    #[test]
    fn cancellation_leaves_input_snapshots_unmodified() {
        let n = 32;
        let (h, w, s) = fixture(n);
        let w_before = w.clone();
        let s_before = s.clone();
        assert_eq!(
            transport_snapshot(&h, &w, &s, &partition(n, 3), WorldEdge::Open, || true)
                .unwrap_err(),
            "Build cancelled"
        );
        assert_eq!(w, w_before);
        assert_eq!(s, s_before);
    }
}
