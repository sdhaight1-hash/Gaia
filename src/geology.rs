//! Independent procedural landform generators and structural masks.
//! All functions operate in normalized world coordinates and are deterministic.
use crate::terrain::{fractal, HeightField};
use std::f32::consts::{PI, TAU};

fn uv(index: usize, size: usize) -> f32 {
    index as f32 / (size - 1).max(1) as f32
}

/// Land surrounded by a guaranteed ocean-level boundary.
/// Coastline noise modulates the island radius; a radial continental-shelf
/// falloff spans the full island radius, avoiding wall-like coastal cliffs.
/// This is a procedural shape, not a model of ocean erosion or sea levels.
pub fn island(size: usize, seed: u32, radius: f32, height: f32, roughness: f32) -> HeightField {
    let mut out = HeightField::flat(size, 0.0);
    if size < 2 { return out; }
    let radius = radius.clamp(0.15, 0.85);
    let height = height.clamp(0.0, 3.0);
    let roughness = roughness.clamp(0.0, 1.0);
    for y in 0..size {
        for x in 0..size {
            let u = uv(x, size);
            let v = uv(y, size);
            let dx = u * 2.0 - 1.0;
            let dy = v * 2.0 - 1.0;
            let distance = (dx * dx + dy * dy).sqrt();
            let coastal_noise = fractal(u, v, seed, 5.0);
            let shore = (radius * (1.0 + roughness * 0.35 * (coastal_noise - 0.5)))
                .min(0.93);
            // Previously the entire height fell to sea level over only 0.07
            // radial units. At the default radius 0.65 this made an abrupt
            // vertical cylinder (reproduced in the Windows QA recording).
            // Distribute the falloff across the island instead: zero slope
            // at both the coastline and the central peak.
            let inland = ((shore - distance) / shore).clamp(0.0, 1.0);
            let mask = inland * inland * (3.0 - 2.0 * inland);
            let relief = fractal(u, v, seed.wrapping_add(97), 3.8);
            out.values[y * size + x] = height * mask * (0.3 + 0.7 * relief);
        }
    }
    out
}

pub fn mountain_range(size: usize, seed: u32, height: f32, roughness: f32) -> HeightField {
    let mut output = HeightField::flat(size, 0.0);
    let height = height.clamp(0.0, 3.0);
    let roughness = roughness.clamp(0.0, 1.0);
    for y in 0..size {
        for x in 0..size {
            let u = uv(x, size);
            let v = uv(y, size);
            let trace = 0.5
                + (fractal(u, 0.6, seed, 2.5) - 0.5) * 0.55
                + 0.07 * (TAU * (u * 1.65 + 0.15)).sin();
            let distance = (v - trace).abs();
            let core = (1.0 - distance * 3.0).max(0.0).powf(1.8);
            let ridge = (1.0 - (fractal(u, v, seed.wrapping_add(15), 8.0) * 2.0 - 1.0).abs())
                .powf(2.3);
            let variation = (1.0 - roughness) + roughness * (0.3 + ridge * 0.7);
            output.values[y * size + x] = height * core * variation;
        }
    }
    output
}

pub fn crater(size: usize, seed: u32, radius: f32, depth: f32, rim: f32) -> HeightField {
    let mut output = HeightField::flat(size, 0.0);
    let radius = radius.clamp(0.1, 0.42);
    let depth = depth.clamp(0.0, 1.0);
    let rim = rim.clamp(0.0, 1.0);
    for y in 0..size {
        for x in 0..size {
            let u = uv(x, size) - 0.5;
            let v = uv(y, size) - 0.5;
            let distortion = (fractal(u + 0.5, v + 0.5, seed, 5.0) - 0.5) * 0.08;
            let r = (u * u + v * v).sqrt() * (1.0 + distortion);
            let dist_to_rim = (r - radius) / (radius * 0.13);
            let ring = (-dist_to_rim * dist_to_rim).exp() * rim * 0.36;
            let bowl = (1.0 - (r / radius).powf(2.3)).max(0.0) * depth * 0.52;
            let plateau = 0.55 + (fractal(u + 0.5, v + 0.5, seed.wrapping_add(1), 7.0) - 0.5) * 0.13;
            output.values[y * size + x] = (plateau + ring - bowl).max(0.0);
        }
    }
    output
}

pub fn dunes(size: usize, seed: u32, frequency: f32, amplitude: f32) -> HeightField {
    let mut output = HeightField::flat(size, 0.0);
    let frequency = frequency.clamp(1.0, 40.0);
    let amplitude = amplitude.clamp(0.0, 3.0);
    for y in 0..size {
        for x in 0..size {
            let u = uv(x, size);
            let v = uv(y, size);
            let warp = fractal(u, v, seed, 3.0) - 0.5;
            let phase = TAU * ((u * 0.90 + v * 0.28) * frequency + warp * 0.27);
            let wave = (0.5 + 0.5 * phase.sin()).powf(3.5);
            let irregularity = 0.5 + 0.5 * fractal(u, v, seed.wrapping_add(123), 3.3);
            output.values[y * size + x] = amplitude * wave * irregularity;
        }
    }
    output
}

pub fn canyon(size: usize, seed: u32, width: f32, depth: f32) -> HeightField {
    let mut output = HeightField::flat(size, 0.0);
    let width = width.clamp(0.015, 0.3);
    let depth = depth.clamp(0.0, 1.0);
    for y in 0..size {
        for x in 0..size {
            let u = uv(x, size);
            let v = uv(y, size);
            let meander = 0.5
                + (fractal(u, 0.42, seed, 2.0) - 0.5) * 0.5
                + 0.055 * (u * 5.0 * PI).sin();
            let distance = ((v - meander) / width).abs();
            let incision = (-distance.powf(2.6)).exp();
            let detail = (fractal(u, v, seed.wrapping_add(119), 7.0) - 0.5) * 0.11;
            output.values[y * size + x] = (0.7 + detail - incision * depth * 0.62).max(0.0);
        }
    }
    output
}

fn bilinear(input: &HeightField, u: f32, v: f32) -> f32 {
    let max = (input.size - 1) as f32;
    let x = u.clamp(0.0, 1.0) * max;
    let y = v.clamp(0.0, 1.0) * max;
    let ix = x.floor() as usize;
    let iy = y.floor() as usize;
    let fx = x - ix as f32;
    let fy = y - iy as f32;
    let ax = (ix + 1).min(input.size - 1);
    let ay = (iy + 1).min(input.size - 1);
    let a = input.get(ix, iy);
    let b = input.get(ax, iy);
    let c = input.get(ix, ay);
    let d = input.get(ax, ay);
    (a * (1.0 - fx) + b * fx) * (1.0 - fy) +
        (c * (1.0 - fx) + d * fx) * fy
}

pub fn warp(input: &HeightField, seed: u32, frequency: f32, amount: f32) -> HeightField {
    let size = input.size;
    let mut output = HeightField::flat(size, 0.0);
    let amount = amount.clamp(0.0, 0.25);
    if amount == 0.0 { return input.clone(); }
    let frequency = frequency.clamp(0.5, 40.0);
    for y in 0..size {
        for x in 0..size {
            let u = uv(x, size);
            let v = uv(y, size);
            let offset_x = (fractal(u, v, seed, frequency) - 0.5) * amount;
            let offset_y = (fractal(u, v, seed.wrapping_add(127), frequency) - 0.5) * amount;
            output.values[y * size + x] = bilinear(input, u + offset_x, v + offset_y);
        }
    }
    output
}

/// Signed heightfield curvature remapped from [-1,1] into [0,1].
/// 0.5 is locally flat; below 0.5 tends to convex ridges, above to bowls.
pub fn curvature(input: &HeightField) -> HeightField {
    let n = input.size;
    let mut output = HeightField::flat(n, 0.5);
    for y in 0..n {
        for x in 0..n {
            let h = input.get(x, y);
            let l = input.get(x.saturating_sub(1), y);
            let r = input.get((x + 1).min(n - 1), y);
            let u = input.get(x, y.saturating_sub(1));
            let d = input.get(x, (y + 1).min(n - 1));
            let laplacian = (l + r + u + d - 4.0 * h) * 18.0;
            output.values[y * n + x] = 0.5 + 0.5 * laplacian.clamp(-1.0, 1.0);
        }
    }
    output
}

/// Cardinal-facing slope mask. Azimuth is clockwise from grid north
/// (negative image Y): 0° north, 90° east, 180° south, 270° west.
/// A flat surface has no aspect and always yields zero.
/// 'half_width_degrees' is the angular distance at which the mask reaches zero.
pub fn aspect_mask(
    input: &HeightField, azimuth_degrees: f32,
    half_width_degrees: f32, min_slope: f32,
) -> HeightField {
    let n = input.size;
    let mut out = HeightField::flat(n, 0.0);
    if n < 2 || !azimuth_degrees.is_finite()
        || !half_width_degrees.is_finite() || !min_slope.is_finite()
        || !(0.0..=360.0).contains(&azimuth_degrees)
        || !(1.0..=180.0).contains(&half_width_degrees)
        || !(0.0..=2.0).contains(&min_slope)
    {
        return out;
    }
    let target = azimuth_degrees.to_radians();
    let width = half_width_degrees.to_radians();
    let world_scale = (n - 1) as f32;
    for y in 0..n {
        let up = y.saturating_sub(1);
        let down = (y + 1).min(n - 1);
        for x in 0..n {
            let left = x.saturating_sub(1);
            let right = (x + 1).min(n - 1);
            let dx = (input.get(right, y) - input.get(left, y))
                * world_scale / (right - left) as f32;
            let dy = (input.get(x, down) - input.get(x, up))
                * world_scale / (down - up) as f32;
            let slope = dx.hypot(dy);
            if !slope.is_finite() || slope <= min_slope || slope <= 1e-7 {
                continue;
            }
            // Downhill direction (-dx,-dy), measured clockwise from north.
            let facing = (-dx).atan2(dy);
            let distance = (facing - target).sin().atan2((facing - target).cos()).abs();
            let proximity = (1.0 - distance / width).clamp(0.0, 1.0);
            out.values[y * n + x] = proximity * proximity * (3.0 - 2.0 * proximity);
        }
    }
    out
}

pub fn elevation_mask(input: &HeightField, low: f32, high: f32, feather: f32) -> HeightField {
    let low = low.clamp(0.0, 1.0);
    let high = high.clamp(low, 2.0);
    let feather = feather.clamp(0.001, 0.5);
    let mut out = HeightField::flat(input.size, 0.0);
    for (dest, value) in out.values.iter_mut().zip(&input.values) {
        let rise = ((*value - low) / feather).clamp(0.0, 1.0);
        let fall = ((high - *value) / feather).clamp(0.0, 1.0);
        let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
        *dest = smooth(rise) * smooth(fall);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn island_shoreline_has_no_abrupt_cylindrical_wall() {
        // Default settings from the 128² Windows recording formerly formed
        // a near-vertical 0.07-wide drop. Adjacent cell steps should remain
        // small relative to a one-unit elevation at the same grid scale.
        let n = 128;
        for roughness in [0.0, 0.7] {
            let surface = island(n, 41, 0.65, 1.0, roughness);
            let max_step = (0..n).flat_map(|y| (0..n).flat_map(move |x| {
                let v = surface.get(x, y);
                let horizontal = (x + 1 < n).then(|| (v - surface.get(x + 1, y)).abs());
                let vertical = (y + 1 < n).then(|| (v - surface.get(x, y + 1)).abs());
                horizontal.into_iter().chain(vertical)
            })).fold(0.0_f32, f32::max);
            assert!(max_step < 0.10,
                "Unexpected cliff with roughness {roughness}: max cell step {max_step}");
            let center = surface.get(n / 2, n / 2);
            let quarter = surface.get((n as f32 * 0.40) as usize, n / 2);
            let beach = surface.get((n as f32 * 0.82) as usize, n / 2);
            assert!(center > quarter && quarter > beach,
                "Elevation should rise gradually from beach to interior: {center}, {quarter}, {beach}");
        }
    }

    #[test]
    fn island_has_ocean_boundary_deterministic_coastline_and_interior() {
        let n = 64;
        let a = island(n, 7, 0.65, 1.3, 0.75);
        let b = island(n, 7, 0.65, 1.3, 0.75);
        let c = island(n, 8, 0.65, 1.3, 0.75);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.get(n / 2, n / 2) > 0.1);
        for i in 0..n {
            assert_eq!(a.get(0, i), 0.0);
            assert_eq!(a.get(n - 1, i), 0.0);
            assert_eq!(a.get(i, 0), 0.0);
            assert_eq!(a.get(i, n - 1), 0.0);
        }
        assert!(a.values.iter().all(|value| value.is_finite() && *value >= 0.0));
        assert!(island(n, 7, 0.65, 0.0, 0.75)
            .values.iter().all(|&value| value == 0.0));
    }

    #[test]
    fn geological_generators_are_deterministic_and_finite() {
        for f in [
            mountain_range(64, 11, 1.0, 0.75),
            crater(64, 11, 0.28, 0.7, 0.6),
            dunes(64, 11, 9.0, 0.45),
            canyon(64, 11, 0.07, 0.85),
        ] {
            assert_eq!(f.values.len(), 64 * 64);
            assert!(f.values.iter().all(|v| v.is_finite() && *v >= 0.0));
            assert!(f.values.iter().any(|v| *v > 0.1));
        }
        assert_eq!(mountain_range(64, 11, 1.0, 0.75),
            mountain_range(64, 11, 1.0, 0.75));
    }

    #[test]
    fn zero_warp_returns_original_grid() {
        let f = dunes(64, 1, 9.0, 1.0);
        assert_eq!(warp(&f, 8, 3.0, 0.0), f);
    }

    #[test]
    fn aspect_mask_follows_downhill_cardinal_direction() {
        let n = 32;
        let plane = HeightField {
            size: n,
            // Rising east means the downhill slope faces west.
            values: (0..n).flat_map(|_| (0..n).map(|x| x as f32 / (n - 1) as f32)).collect(),
        };
        let west = aspect_mask(&plane, 270.0, 60.0, 0.01);
        let east = aspect_mask(&plane, 90.0, 60.0, 0.01);
        assert!(west.values.iter().all(|&v| (v - 1.0).abs() < 1e-6));
        assert!(east.values.iter().all(|&v| v == 0.0));

        let north = HeightField {
            size: n,
            // Rising toward bottom of image means downhill north.
            values: (0..n).flat_map(|y| (0..n).map(move |_| y as f32 / (n - 1) as f32)).collect(),
        };
        assert!(aspect_mask(&north, 0.0, 45.0, 0.01)
            .values.iter().all(|&v| (v - 1.0).abs() < 1e-6));
        assert!(aspect_mask(&north, 180.0, 45.0, 0.01)
            .values.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn aspect_mask_is_bounded_flat_safe_and_wraps_at_north() {
        let flat = HeightField::flat(32, 0.4);
        assert!(aspect_mask(&flat, 35.0, 45.0, 0.0)
            .values.iter().all(|&v| v == 0.0));
        let n = 32;
        let plane = HeightField {
            size: n,
            values: (0..n).flat_map(|y| (0..n).map(move |x| {
                (y as f32 - x as f32 * 0.01) / (n - 1) as f32
            })).collect(),
        };
        let mask = aspect_mask(&plane, 359.0, 40.0, 0.0);
        assert!(mask.values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        assert!(mask.get(16, 16) > 0.9);
        assert_eq!(aspect_mask(&plane, 359.0, 40.0, 2.0).mass(), 0.0);
    }

    #[test]
    fn masks_are_in_unit_interval() {
        let f = crater(64, 4, 0.25, 0.6, 0.8);
        let a = curvature(&f);
        let b = elevation_mask(&f, 0.2, 0.65, 0.1);
        for mask in [a, b] {
            assert!(mask.values.iter().all(|v| (0.0..=1.0).contains(v)));
        }
    }
}
