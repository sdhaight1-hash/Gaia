//! RGB tangent-style normal maps from a uniformly sampled heightfield.
//! Flat = (0.5, 0.5, 1.0). World X goes right and image Y goes downward.
//! Optional green-channel flip accommodates applications that invert Y.
use crate::{color::ColorField, terrain::HeightField};

pub fn derive_normals(
    source: &HeightField, strength: f32, flip_green: bool,
) -> Result<ColorField, String> {
    let size = source.size;
    if size < 2 || source.values.len() != size * size ||
        source.values.iter().any(|value| !value.is_finite())
    {
        return Err("Normals require a complete, finite heightfield of at least 2x2".into());
    }
    if !strength.is_finite() || !(0.0..=4.0).contains(&strength) {
        return Err("Normal strength must be finite and in the range 0–4".into());
    }
    let mut output = ColorField::flat(size, [0.5, 0.5, 1.0, 1.0]);
    for y in 0..size {
        for x in 0..size {
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(size - 1);
            let y0 = y.saturating_sub(1);
            let y1 = (y + 1).min(size - 1);
            // One-sided differences at edges, central differences elsewhere.
            // Scale by grid density to obtain consistent normals at 32/512.
            let dx = (source.get(x1, y) - source.get(x0, y))
                * (size - 1) as f32 / (x1 - x0) as f32;
            let dy = (source.get(x, y1) - source.get(x, y0))
                * (size - 1) as f32 / (y1 - y0) as f32;
            let nx = -strength * dx;
            let ny = -strength * dy;
            let length = (nx * nx + ny * ny + 1.0).sqrt();
            let green = if flip_green { -ny } else { ny };
            output.pixels[y * size + x] = [
                (0.5 + 0.5 * nx / length).clamp(0.0, 1.0),
                (0.5 + 0.5 * green / length).clamp(0.0, 1.0),
                (0.5 + 0.5 / length).clamp(0.0, 1.0),
                1.0,
            ];
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_terrain_has_standard_flat_normal() {
        for size in [32, 64, 128] {
            let field = HeightField::flat(size, 0.4);
            let normal = derive_normals(&field, 1.0, false).unwrap();
            assert!(normal.pixels.iter()
                .all(|&pixel| pixel == [0.5, 0.5, 1.0, 1.0]));
        }
    }

    #[test]
    fn ramp_normals_point_opposite_gradient_and_flip_green() {
        let n = 32;
        let source = HeightField {
            size: n,
            values: (0..n).flat_map(|y| (0..n)
                .map(move |x| (x as f32 + y as f32) / (2 * (n - 1)) as f32))
                .collect(),
        };
        let a = derive_normals(&source, 1.0, false).unwrap();
        let b = derive_normals(&source, 1.0, true).unwrap();
        for (&px, &flipped) in a.pixels.iter().zip(&b.pixels) {
            assert!(px[0] < 0.5);
            assert!(px[1] < 0.5);
            assert!((flipped[0] - px[0]).abs() < 1e-6);
            assert!((flipped[1] + px[1] - 1.0).abs() < 1e-6);
            assert!(px[2] > 0.5);
            assert!(px.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        }
        let neutral = derive_normals(&source, 0.0, false).unwrap();
        assert!(neutral.pixels.iter().all(|&p| p == [0.5, 0.5, 1.0, 1.0]));
    }

    #[test]
    fn rejects_bad_inputs_and_strength() {
        let mut source = HeightField::flat(32, 0.1);
        assert!(derive_normals(&source, f32::NAN, false).is_err());
        assert!(derive_normals(&source, 10.0, false).is_err());
        source.values[0] = f32::NAN;
        assert!(derive_normals(&source, 1.0, false).is_err());
    }
}
