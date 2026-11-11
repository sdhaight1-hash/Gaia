//! Static four-way terrain material classification, not a PBR shader.
use crate::{pipeline::slope, terrain::HeightField};

pub struct MaterialFields {
    pub sand: HeightField,
    pub soil: HeightField,
    pub rock: HeightField,
    pub snow: HeightField,
}

fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rock owns steep faces; gentle low terrain becomes sand, high terrain snow,
/// and the remaining coverage is soil. Every pixel's four weights sum to 1.
pub fn classify_materials(
    height: &HeightField, sea_level: f32, beach_width: f32,
    rock_slope: f32, rock_blend: f32, snowline: f32, snow_blend: f32,
    cancelled: impl Fn() -> bool,
) -> Result<MaterialFields, String> {
    if !sea_level.is_finite() || !(0.0..=1.0).contains(&sea_level)
        || !beach_width.is_finite() || !(0.001..=0.5).contains(&beach_width)
        || !rock_slope.is_finite() || !(0.0..=1.0).contains(&rock_slope)
        || !rock_blend.is_finite() || !(0.001..=0.5).contains(&rock_blend)
        || !snowline.is_finite() || !(0.0..=2.0).contains(&snowline)
        || !snow_blend.is_finite() || !(0.001..=0.5).contains(&snow_blend)
    {
        return Err("Material masks require finite thresholds and positive blend widths".into());
    }
    let n = height.size;
    if n < 2 || n.checked_mul(n) != Some(height.values.len())
        || height.values.iter().any(|v| !v.is_finite())
    {
        return Err("Material masks require a finite square heightfield".into());
    }
    if cancelled() { return Err("Build cancelled".into()); }
    // Reuse the same normalized slope that appears in the editor's slope node.
    let steepness = slope(height);
    let mut fields = MaterialFields {
        sand: HeightField::flat(n, 0.0),
        soil: HeightField::flat(n, 0.0),
        rock: HeightField::flat(n, 0.0),
        snow: HeightField::flat(n, 0.0),
    };
    for y in 0..n {
        if cancelled() { return Err("Build cancelled".into()); }
        for x in 0..n {
            let i = y * n + x;
            let h = height.values[i];
            let rock = smoothstep(rock_slope - rock_blend * 0.5,
                rock_slope + rock_blend * 0.5, steepness.values[i]);
            let beach = 1.0 - smoothstep(sea_level, sea_level + beach_width, h);
            let snowy = smoothstep(snowline - snow_blend * 0.5,
                snowline + snow_blend * 0.5, h);
            let gentle = 1.0 - rock;
            let inland = 1.0 - beach;
            fields.rock.values[i] = rock;
            fields.sand.values[i] = gentle * beach;
            fields.snow.values[i] = gentle * inland * snowy;
            fields.soil.values[i] = gentle * inland * (1.0 - snowy);
        }
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(field: &HeightField) -> MaterialFields {
        classify_materials(field, 0.25, 0.08, 0.32, 0.10,
            0.8, 0.12, || false).unwrap()
    }

    #[test]
    fn flat_low_mid_high_terrain_selects_expected_material() {
        assert_eq!(run(&HeightField::flat(32, 0.15)).sand.values[200], 1.0);
        assert_eq!(run(&HeightField::flat(32, 0.5)).soil.values[200], 1.0);
        assert_eq!(run(&HeightField::flat(32, 1.0)).snow.values[200], 1.0);
    }

    #[test]
    fn a_cliff_is_rock_but_a_nearby_plateau_is_soil() {
        let mut height = HeightField::flat(32, 0.5);
        for y in 0..32 {
            for x in 16..32 { height.values[y * 32 + x] = 0.7; }
        }
        let fields = run(&height);
        assert!(fields.rock.values[16 * 32 + 16] > 0.9);
        assert!(fields.soil.values[16 * 32 + 20] > 0.9);
    }

    #[test]
    fn four_masks_are_normalized_bounded_and_deterministic() {
        let height = HeightField {
            size: 32,
            values: (0..1024).map(|i| (i as f32 * 0.067).sin() * 0.4 + 0.55).collect(),
        };
        let first = run(&height);
        assert_eq!(first.rock, run(&height).rock);
        for i in 0..1024 {
            let values = [first.sand.values[i], first.soil.values[i],
                first.rock.values[i], first.snow.values[i]];
            assert!(values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
            assert!((values.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn rejects_nonfinite_inputs_bad_controls_and_cancellation() {
        let h = HeightField::flat(32, 0.5);
        assert!(classify_materials(&h, 0.25, 0.0, 0.3, 0.1, 0.8, 0.1, || false).is_err());
        assert!(classify_materials(&h, f32::NAN, 0.08, 0.3, 0.1, 0.8, 0.1, || false).is_err());
        assert_eq!(classify_materials(&h, 0.25, 0.08, 0.3, 0.1, 0.8, 0.1, || true)
            .err().unwrap(), "Build cancelled");
        let mut bad = h.clone();
        bad.values[0] = f32::INFINITY;
        assert!(classify_materials(&bad, 0.25, 0.08, 0.3, 0.1, 0.8, 0.1, || false).is_err());
    }
}
