//! Static, outlet-bounded lake surfaces derived from terrain depressions.
//!
//! Priority flood gives a *potential* spill elevation in each enclosed
//! depression. Wet regions are grouped into distinct near-equal-level basins.
//! A fraction of the basin's vertical fill range determines one horizontal
//! water elevation for that basin. This is not rainfall, hydraulic pressure,
//! open-water dynamics, groundwater, or a time-dependent lake simulation.
use crate::{depression::fill_depressions, terrain::HeightField};
use std::collections::VecDeque;

pub struct LakeFields {
    /// Original bed. Lake generation does not silently modify terrain.
    pub height: HeightField,
    /// Terrain plus vertical lake water where wet; useful for 3D surfaces.
    pub surface: HeightField,
    /// Water depth in source height units, zero on dry ground.
    pub water: HeightField,
    /// Binary inundation mask in [0,1].
    pub coverage: HeightField,
    pub lake_count: usize,
    /// Sum of depth per grid sample; real-world volume requires cell area.
    pub sample_depth_sum: f64,
}

pub fn fill_lakes(
    source: &HeightField, fill_fraction: f32, minimum_area: u32,
    minimum_depth: f32, cancelled: impl Fn() -> bool,
) -> Result<LakeFields, String> {
    if !fill_fraction.is_finite() || !(0.0..=1.0).contains(&fill_fraction) {
        return Err("Lake fill fraction must be finite and within 0–1".into());
    }
    if !minimum_depth.is_finite() || !(0.0..=1.0).contains(&minimum_depth) {
        return Err("Minimum lake depth must be finite and within 0–1".into());
    }
    if minimum_area == 0 {
        return Err("Minimum lake area must be at least one cell".into());
    }

    let flood = fill_depressions(source, 0.0, &cancelled)?;
    let n = source.size;
    let length = source.values.len();
    let mut surface = source.clone();
    let mut water = HeightField::flat(n, 0.0);
    let mut coverage = HeightField::flat(n, 0.0);
    let mut visited = vec![false; length];
    let mut queue = VecDeque::<usize>::new();
    let mut component = Vec::<usize>::new();
    let mut lake_count = 0;
    let mut sample_depth_sum = 0.0_f64;

    for root in 0..length {
        if root % 4096 == 0 && cancelled() {
            return Err("Build cancelled".into());
        }
        if visited[root] || flood.fill_depth.values[root] <= 0.0 {
            continue;
        }

        let reference_level = flood.height.values[root];
        // A consistent floating-point tolerance prevents breaking a single
        // basin into disjoint pieces due to tiny roundoff in the spill level.
        let tolerance = 1.0e-5 * reference_level.abs().max(1.0);
        queue.push_back(root);
        visited[root] = true;
        component.clear();
        let mut min_bed = f32::INFINITY;
        let mut spill = reference_level;
        while let Some(index) = queue.pop_front() {
            if component.len() % 4096 == 0 && cancelled() {
                return Err("Build cancelled".into());
            }
            component.push(index);
            min_bed = min_bed.min(source.values[index]);
            spill = spill.min(flood.height.values[index]);
            let x = index % n;
            let y = index / n;
            for dy in -1_isize..=1 {
                for dx in -1_isize..=1 {
                    if dx == 0 && dy == 0 { continue; }
                    let xx = x as isize + dx;
                    let yy = y as isize + dy;
                    if xx < 0 || yy < 0 || xx >= n as isize || yy >= n as isize {
                        continue;
                    }
                    let next = yy as usize * n + xx as usize;
                    if visited[next] || flood.fill_depth.values[next] <= 0.0
                        || (flood.height.values[next] - reference_level).abs() > tolerance
                    {
                        continue;
                    }
                    visited[next] = true;
                    queue.push_back(next);
                }
            }
        }

        if component.len() < minimum_area as usize || spill - min_bed < minimum_depth {
            continue;
        }
        let level = min_bed + fill_fraction * (spill - min_bed);
        let mut wet = 0;
        for &index in &component {
            let depth = (level - source.values[index]).max(0.0);
            if depth <= 0.0 { continue; }
            surface.values[index] = level;
            water.values[index] = depth;
            coverage.values[index] = 1.0;
            sample_depth_sum += f64::from(depth);
            wet += 1;
        }
        if wet > 0 { lake_count += 1; }
    }

    Ok(LakeFields {
        height: source.clone(),
        surface, water, coverage, lake_count, sample_depth_sum
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_pits(n: usize) -> HeightField {
        let mut field = HeightField::flat(n, 0.8);
        for i in 0..n {
            field.values[i] = 0.1;
            field.values[(n-1)*n+i] = 0.1;
            field.values[i*n] = 0.1;
            field.values[i*n+n-1] = 0.1;
        }
        field.values[(n/3)*n+n/3] = 0.15;
        field.values[(n*2/3)*n+n*2/3] = 0.25;
        field
    }

    #[test]
    fn separate_basins_produce_distinct_flat_water_surfaces() {
        let n = 33;
        let source = two_pits(n);
        let full = fill_lakes(&source, 1.0, 1, 0.01, || false).unwrap();
        assert_eq!(full.lake_count, 2);
        assert_eq!(full.height, source);
        let a = (n/3)*n+n/3;
        let b = (n*2/3)*n+n*2/3;
        assert!((full.surface.values[a]-0.8).abs() < 1e-6);
        assert!((full.surface.values[b]-0.8).abs() < 1e-6);
        assert!((full.water.values[a]-0.65).abs() < 1e-5);
        assert!((full.water.values[b]-0.55).abs() < 1e-5);
        assert!(full.coverage.values.iter().filter(|&&x| x>0.0).count() == 2);
        assert!((full.sample_depth_sum-1.2).abs() < 1e-5);
        assert_eq!(full.water,
            fill_lakes(&source, 1.0, 1, 0.01, || false).unwrap().water);
    }

    #[test]
    fn partial_filling_is_level_not_fractional_depth_per_cell() {
        let n = 33;
        let mut source = two_pits(n);
        let cx = n/3;
        let cy = n/3;
        source.values[cy*n+cx+1] = 0.5;
        let half = fill_lakes(&source, 0.5, 1, 0.01, || false).unwrap();
        let full = fill_lakes(&source, 1.0, 1, 0.01, || false).unwrap();
        assert!(half.sample_depth_sum < full.sample_depth_sum);
        // The medium-height cell is dry at half fill, whereas its neighbor
        // under the same horizontal level is wet.
        assert_eq!(half.water.get(cx+1,cy), 0.0);
        assert!(half.water.get(cx,cy) > 0.0);
        assert!(full.water.get(cx+1,cy) > 0.0);
        let empty = fill_lakes(&source, 0.0, 1, 0.01, || false).unwrap();
        assert_eq!(empty.lake_count, 0);
        assert!(empty.water.values.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn flat_terrain_has_no_lakes_and_small_basins_can_be_filtered() {
        let land = HeightField::flat(32, 0.4);
        let lakes = fill_lakes(&land, 1.0, 1, 0.01, || false).unwrap();
        assert_eq!(lakes.lake_count, 0);
        assert_eq!(lakes.surface, land);
        let filtered = fill_lakes(&two_pits(33), 1.0, 2, 0.01, || false).unwrap();
        assert_eq!(filtered.lake_count, 0);
        assert!(filtered.coverage.values.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn rejects_bad_parameters_and_nonfinite_terrain_and_can_cancel() {
        let land = two_pits(64);
        assert!(fill_lakes(&land, f32::NAN, 1, 0.1, || false).is_err());
        assert!(fill_lakes(&land, 1.1, 1, 0.1, || false).is_err());
        assert!(fill_lakes(&land, 0.5, 0, 0.1, || false).is_err());
        assert!(fill_lakes(&land, 0.5, 1, f32::INFINITY, || false).is_err());
        let mut bad = land.clone();
        bad.values[0] = f32::NAN;
        assert!(fill_lakes(&bad, 0.5, 1, 0.1, || false).is_err());
        assert!(fill_lakes(&land, 0.5, 1, 0.1, || true).is_err());
    }
}
