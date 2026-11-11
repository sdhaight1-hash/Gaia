//! Deterministic, tileable neutral detail atlas for four terrain materials.
//! The PBR shader supplies editable albedo; this 2D array supplies only detail
//! modulation, so the same textures can be reused with any material palette.
pub const SIZE: u32 = 64;
pub const LAYERS: u32 = 4;
const TAU: f32 = std::f32::consts::TAU;

fn hash(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9e37_79b9)
        ^ y.wrapping_mul(0x85eb_ca6b)
        ^ seed.wrapping_mul(0x7feb_352d);
    h ^= h >> 16;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 15;
    h as f32 / u32::MAX as f32
}

/// Periodic lattice noise. Sampling u=0 and u=1 yields identical values.
fn noise(u: f32, v: f32, cells: u32, seed: u32) -> f32 {
    let domain = cells as f32;
    let x = (u * domain).rem_euclid(domain);
    let y = (v * domain).rem_euclid(domain);
    let ix = x.floor() as u32;
    let iy = y.floor() as u32;
    let sx = (x - ix as f32).powi(2) * (3.0 - 2.0 * (x - ix as f32));
    let sy = (y - iy as f32).powi(2) * (3.0 - 2.0 * (y - iy as f32));
    let a = hash(ix, iy, seed);
    let b = hash((ix + 1) % cells, iy, seed);
    let c = hash(ix, (iy + 1) % cells, seed);
    let d = hash((ix + 1) % cells, (iy + 1) % cells, seed);
    (a + (b - a) * sx) * (1.0 - sy)
        + (c + (d - c) * sx) * sy
}

/// Array layers: sand ripples, soil grain, rock striations, snow patches.
/// RGB stores neutral brightness; A stores a small roughness variation.
pub fn build_atlas() -> Vec<u8> {
    let mut pixels = Vec::with_capacity((SIZE * SIZE * LAYERS * 4) as usize);
    for layer in 0..LAYERS {
        for y in 0..SIZE {
            for x in 0..SIZE {
                let (u, v) = (x as f32 / SIZE as f32, y as f32 / SIZE as f32);
                let broad = noise(u, v, 5, 100 + layer);
                let fine = noise(u, v, 17, 200 + layer);
                let ripple = (TAU * (u * 9.0 + v * 3.0)).sin();
                let strata = (TAU * (u * 7.0 - v * 2.0)).sin().abs();
                let (value, roughness) = match layer {
                    0 => (0.66 + 0.09 * ripple + 0.12 * fine, 0.60 + 0.20 * fine),
                    1 => (0.49 + 0.28 * broad + 0.15 * fine, 0.58 + 0.25 * fine),
                    2 => (0.45 + 0.20 * strata + 0.17 * broad, 0.38 + 0.32 * fine),
                    _ => (0.72 + 0.13 * broad + 0.04 * fine, 0.48 + 0.20 * broad),
                };
                let value = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                pixels.extend_from_slice(&[value, value, value,
                    (roughness.clamp(0.0, 1.0) * 255.0).round() as u8]);
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_is_deterministic_complete_and_layers_are_distinct() {
        let atlas = build_atlas();
        assert_eq!(atlas.len(), (SIZE * SIZE * LAYERS * 4) as usize);
        assert_eq!(atlas, build_atlas());
        let tile_bytes = (SIZE * SIZE * 4) as usize;
        assert_ne!(&atlas[..tile_bytes], &atlas[tile_bytes..tile_bytes * 2]);
        assert_ne!(&atlas[tile_bytes * 2..tile_bytes * 3], &atlas[tile_bytes * 3..]);
        assert!(atlas.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2]));
    }

    #[test]
    fn procedural_noise_wraps_at_all_tile_edges() {
        for freq in [5, 17] {
            for index in 0..24 {
                let t = index as f32 / 23.0;
                assert!((noise(0.0, t, freq, 71) - noise(1.0, t, freq, 71)).abs() < 1e-6);
                assert!((noise(t, 0.0, freq, 71) - noise(t, 1.0, freq, 71)).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn atlas_rows_are_copy_buffer_aligned() {
        assert_eq!((SIZE * 4) % 256, 0);
    }
}
