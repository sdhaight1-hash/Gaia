//! Deterministic procedural river channels, not a hydrodynamic fluid solver.
//! Routing comes from priority-flood conditioning plus D8 contributing area;
//! the raster network is widened with smooth banks and downcutting. No rain,
//! headwater control, sediment transport, or physical water surface is implied.
use crate::{
    depression::fill_depressions,
    drainage::drainage,
    terrain::HeightField,
};

pub struct RiverFields {
    /// Terrain modified by channel incision and reconditioned for drainage.
    pub height: HeightField,
    /// Soft river corridor mask (1 at channel center, 0 outside its width).
    pub channels: HeightField,
    /// Normalized net loss compared with the initial conditioned terrain.
    pub wear: HeightField,
    pub outlet_area: u64,
}

pub fn carve_rivers(
    source: &HeightField,
    catchment_fraction: f32,
    width: f32,
    depth: f32,
    grade: f32,
    cancelled: impl Fn() -> bool,
) -> Result<RiverFields, String> {
    if !width.is_finite() || !(0.5..=12.0).contains(&width) {
        return Err("Rivers width must be finite and within 0.5–12 grid cells".into());
    }
    if !depth.is_finite() || !(0.0..=0.5).contains(&depth) {
        return Err("Rivers downcut depth must be finite and within 0–0.5".into());
    }
    // The drainage analyzer validates finite heights and catchment fraction.
    let conditioned = fill_depressions(source, grade, &cancelled)?;
    let basin = drainage(&conditioned.height, catchment_fraction)?;
    let n = source.size;
    let count = n * n;
    let mut channels = HeightField::flat(n, 0.0);
    let mut cut = vec![0.0_f32; count];

    for index in 0..count {
        if index % 1024 == 0 && cancelled() {
            return Err("Build cancelled".into());
        }
        if basin.channels.values[index] == 0.0 {
            continue;
        }
        let a = basin.accumulation.values[index].clamp(0.0, 1.0);
        let radius = width * (0.35 + 0.65 * a);
        let center_x = (index % n) as isize;
        let center_y = (index / n) as isize;
        let reach = radius.ceil() as isize;
        let downcut = depth * (0.25 + 0.75 * a);
        for dy in -reach..=reach {
            let y = center_y + dy;
            if y < 0 || y >= n as isize { continue; }
            for dx in -reach..=reach {
                let x = center_x + dx;
                if x < 0 || x >= n as isize { continue; }
                let distance = ((dx * dx + dy * dy) as f32).sqrt();
                let t = (1.0 - distance / radius).clamp(0.0, 1.0);
                // Smooth banking prevents abrupt raster trench walls.
                let falloff = t * t * (3.0 - 2.0 * t);
                let dest = y as usize * n + x as usize;
                channels.values[dest] = channels.values[dest].max(falloff);
                cut[dest] = cut[dest].max(downcut * falloff);
            }
        }
    }

    let mut carved = conditioned.height.clone();
    for (height, amount) in carved.values.iter_mut().zip(cut) {
        *height -= amount;
    }
    // A locally deeper channel can create new terminal pits. Conditioning a
    // second time preserves drainage continuity on the exported carved field.
    let finished = fill_depressions(&carved, grade, &cancelled)?;
    let mut wear = HeightField::flat(n, 0.0);
    if depth > 0.0 {
        for i in 0..count {
            wear.values[i] = ((conditioned.height.values[i]
                - finished.height.values[i]) / depth).clamp(0.0, 1.0);
        }
    }
    Ok(RiverFields {
        height: finished.height,
        channels,
        wear,
        outlet_area: basin.outlet_area,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planar_slope(n: usize) -> HeightField {
        HeightField {
            size: n,
            values: (0..n).flat_map(|_| (0..n)
                .map(|x| x as f32 / (n - 1) as f32)).collect(),
        }
    }

    #[test]
    fn river_network_carves_smooth_banks_and_preserves_outlet_routing() {
        let size = 64;
        let land = planar_slope(size);
        let river = carve_rivers(&land, 0.004, 3.0, 0.06, 0.0001, || false).unwrap();
        let repeat = carve_rivers(&land, 0.004, 3.0, 0.06, 0.0001, || false).unwrap();
        assert_eq!(river.height, repeat.height);
        assert_eq!(river.channels, repeat.channels);
        assert_eq!(river.wear, repeat.wear);
        assert_eq!(river.outlet_area, (size*size) as u64);
        assert_ne!(river.height, land);
        assert!(river.wear.values.iter().any(|&v| v > 0.0));
        assert!(river.channels.values.iter().any(|&v| v > 0.0));
        assert!(river.channels.values.iter().any(|&v| v > 0.0 && v < 1.0));
        for map in [&river.height, &river.channels, &river.wear] {
            assert!(map.values.iter().all(|v| v.is_finite()));
        }
        for map in [&river.channels, &river.wear] {
            assert!(map.values.iter().all(|v| (0.0..=1.0).contains(v)));
        }
        for y in 1..size-1 {
            for x in 1..size-1 {
                let h = river.height.get(x, y);
                assert!((-1_isize..=1).any(|dy| (-1_isize..=1).any(|dx| {
                    if dx == 0 && dy == 0 { return false; }
                    let xx = (x as isize + dx) as usize;
                    let yy = (y as isize + dy) as usize;
                    river.height.get(xx, yy) < h
                })), "river created an enclosed pit at ({x},{y})");
            }
        }
        assert_eq!(drainage(&river.height, 0.004).unwrap().outlet_area,
            (size*size) as u64);
    }

    #[test]
    fn zero_depth_creates_only_network_mask_with_no_wear() {
        let land = planar_slope(32);
        let dry = carve_rivers(&land, 0.005, 3.0, 0.0, 0.0001, || false).unwrap();
        assert!(dry.channels.values.iter().any(|&v| v > 0.0));
        assert!(dry.wear.values.iter().all(|&v| v == 0.0));
        assert_eq!(dry.height, fill_depressions(&land, 0.0001, || false).unwrap().height);
    }

    #[test]
    fn validates_parameters_and_cancellation() {
        let land = planar_slope(32);
        assert!(carve_rivers(&land, 0.01, f32::NAN, 0.04, 0.0001, || false).is_err());
        assert!(carve_rivers(&land, 0.01, 0.1, 0.04, 0.0001, || false).is_err());
        assert!(carve_rivers(&land, 0.01, 3.0, f32::INFINITY, 0.0001, || false).is_err());
        assert!(carve_rivers(&land, 0.01, 3.0, 0.04, -1.0, || false).is_err());
        assert!(carve_rivers(&land, 1.1, 3.0, 0.04, 0.0001, || false).is_err());
        assert!(carve_rivers(&land, 0.01, 3.0, 0.04, 0.0001, || true).is_err());
    }
}
