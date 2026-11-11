//! Small, deterministic Height→Height adjustment stage for graph workflows.
//! Its parameters are independent of any proprietary modifier implementation.
use crate::terrain::HeightField;

/// Multiply, shape, optionally normalize globally, invert and clamp a
/// heightfield. All numerical transforms are world-global before tile export.
pub fn adjust_height(
    source: &HeightField, multiply: f32, shaper: f32,
    clamp_min: f32, clamp_max: f32, autolevel: bool, invert: bool,
) -> Result<HeightField, String> {
    if !multiply.is_finite() || !(0.0..=4.0).contains(&multiply)
        || !shaper.is_finite() || !(-1.0..=1.0).contains(&shaper)
        || !clamp_min.is_finite() || !clamp_max.is_finite()
        || !(0.0..=4.0).contains(&clamp_min)
        || !(0.0..=4.0).contains(&clamp_max)
        || clamp_min > clamp_max
    {
        return Err("Adjust requires finite multiply 0–4, shape -1–1 and 0 <= min <= max <= 4".into());
    }
    if source.size == 0 || source.values.len() != source.size * source.size ||
        source.values.iter().any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err("Adjust requires a finite nonnegative square heightfield".into());
    }
    let (mut low, mut high) = (f32::INFINITY, f32::NEG_INFINITY);
    if autolevel {
        for &value in &source.values {
            low = low.min(value);
            high = high.max(value);
        }
    }
    let normalize = autolevel && (high-low) > 1.0e-8;
    // Positive Shaper bulks up low elevations (preserving zero), negative
    // Shaper thins slopes and emphasizes the tallest peaks.
    let exponent = 2.0_f32.powf(-2.0 * shaper);
    let mut output = source.clone();
    for value in &mut output.values {
        let mut h = if normalize { (*value-low) / (high-low) } else { *value };
        h = (h * multiply).max(0.0).powf(exponent);
        if invert {
            h = 1.0 - h.clamp(0.0, 1.0);
        }
        if !h.is_finite() {
            return Err("Adjust result overflowed".into());
        }
        *value = h.clamp(clamp_min, clamp_max);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_shaper_controls_work() {
        let source = HeightField {
            size: 32,
            values: (0..1024).map(|i| i as f32 / 1023.0).collect(),
        };
        let identity = adjust_height(&source, 1.0, 0.0, 0.0, 4.0, false, false).unwrap();
        assert_eq!(identity, source);
        let bulked = adjust_height(&source, 1.0, 0.8, 0.0, 4.0, false, false).unwrap();
        let thin = adjust_height(&source, 1.0, -0.8, 0.0, 4.0, false, false).unwrap();
        assert!(bulked.values[512] > source.values[512]);
        assert!(thin.values[512] < source.values[512]);
        assert_eq!(bulked.values[0], 0.0);
    }

    #[test]
    fn autolevel_invert_and_clamping_are_consistent() {
        let source = HeightField {
            size: 32,
            values: (0..1024).map(|i| 0.2 + i as f32 / 1023.0 * 0.3).collect(),
        };
        let normalized = adjust_height(&source, 1.0, 0.0, 0.0, 4.0, true, false).unwrap();
        assert!((normalized.values[0] - 0.0).abs() < 1e-6);
        assert!((normalized.values[1023] - 1.0).abs() < 1e-6);
        let inverted = adjust_height(&source, 1.0, 0.0, 0.0, 4.0, true, true).unwrap();
        assert!((inverted.values[0] - 1.0).abs() < 1e-6);
        assert!((inverted.values[1023] - 0.0).abs() < 1e-6);
        let clipped = adjust_height(&source, 4.0, 0.0, 0.1, 0.5, false, false).unwrap();
        assert!(clipped.values.iter().all(|&value| (0.1..=0.5).contains(&value)));
        let flat = HeightField::flat(32, 0.3);
        assert_eq!(adjust_height(&flat, 1.0, 0.0, 0.0, 4.0, true, false).unwrap(),flat);
    }

    #[test]
    fn rejects_invalid_settings_and_nonfinite_inputs() {
        let source = HeightField::flat(32, 0.3);
        assert!(adjust_height(&source, f32::NAN, 0.0, 0.0, 4.0, false, false).is_err());
        assert!(adjust_height(&source, 1.0, 0.0, 0.8, 0.2, false, false).is_err());
        assert!(adjust_height(&source, 1.0, 2.0, 0.0, 4.0, false, false).is_err());
        let mut bad = source.clone();
        bad.values[12] = f32::INFINITY;
        assert!(adjust_height(&bad, 1.0, 0.0, 0.0, 4.0, false, false).is_err());
    }
}
