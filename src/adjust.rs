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

/// Exact, tie-aware histogram equalization of a heightfield.
/// The first and last distinct elevation stay fixed, so the original units
/// and relief range are preserved. Intermediate values are remapped by their
/// empirical quantile; factor blends from untouched input (0) to equalized (1).
///
/// Sorting makes this O(n log n), with O(n) scratch space. The operation
/// evaluates the whole heightfield before export, not one tile at a time.
pub fn equalize_height(
    source: &HeightField, factor: f32, cancelled: impl Fn() -> bool,
) -> Result<HeightField, String> {
    if !factor.is_finite() || !(0.0..=1.0).contains(&factor) {
        return Err("Equalize factor must be finite and between 0 and 1".into());
    }
    if source.size == 0
        || source.size.checked_mul(source.size) != Some(source.values.len())
        || source.values.iter().any(|value| !value.is_finite())
    {
        return Err("Equalize requires a finite square heightfield".into());
    }
    if cancelled() {
        return Err("Build cancelled".into());
    }
    if factor == 0.0 {
        return Ok(source.clone());
    }

    let mut order: Vec<usize> = (0..source.values.len()).collect();
    order.sort_unstable_by(|&a, &b|
        source.values[a].total_cmp(&source.values[b]));
    if cancelled() {
        return Err("Build cancelled".into());
    }

    let len = order.len();
    let low = source.values[order[0]];
    let high = source.values[order[len - 1]];
    if low == high {
        return Ok(source.clone());
    }

    // Midranks give equal elevations the same result regardless of spatial
    // location or unstable sort order. Anchor both extreme groups exactly.
    let first_end = order.partition_point(|&i| source.values[i] == low);
    let last_start = order.partition_point(|&i| source.values[i] < high);
    let first_rank = (first_end as f64 - 1.0) * 0.5;
    let last_rank = (last_start as f64 + len as f64 - 1.0) * 0.5;
    let span = last_rank - first_rank;
    let mut output = source.clone();
    let mut start = 0;
    let mut groups = 0usize;
    while start < len {
        if groups % 4096 == 0 && cancelled() {
            return Err("Build cancelled".into());
        }
        groups += 1;
        let current = source.values[order[start]];
        let mut end = start + 1;
        while end < len && source.values[order[end]] == current {
            if (end - start) % 16384 == 0 && cancelled() {
                return Err("Build cancelled".into());
            }
            end += 1;
        }
        let midrank = (start as f64 + end as f64 - 1.0) * 0.5;
        let quantile = ((midrank - first_rank) / span).clamp(0.0, 1.0);
        let mapped = low as f64 + (high as f64 - low as f64) * quantile;
        let value = (current as f64 + factor as f64 * (mapped - current as f64)) as f32;
        for &index in &order[start..end] {
            output.values[index] = value;
        }
        start = end;
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

#[cfg(test)]
mod equalize_tests {
    use super::*;

    #[test]
    fn factor_zero_is_identity_and_flat_fields_remain_flat() {
        let source = HeightField {
            size: 32,
            values: (0..1024).map(|i| i as f32 / 1023.0).collect(),
        };
        assert_eq!(equalize_height(&source, 0.0, || false).unwrap(), source);
        let flat = HeightField::flat(32, 0.375);
        assert_eq!(equalize_height(&flat, 1.0, || false).unwrap(), flat);
    }

    #[test]
    fn skewed_distribution_reveals_midrange_without_changing_extrema() {
        let source = HeightField {
            size: 32,
            values: (0..1024).map(|i| {
                if i < 256 { 0.0 }
                else if i < 768 { 0.2 + (i - 256) as f32 * 0.2 / 511.0 }
                else { 1.0 }
            }).collect(),
        };
        let result = equalize_height(&source, 1.0, || false).unwrap();
        assert_eq!(result.values[0], 0.0);
        assert_eq!(result.values[1023], 1.0);
        assert!(result.values[512] > source.values[512] + 0.1);
        assert!(result.values.windows(2).all(|v| v[0] <= v[1]));
        let half = equalize_height(&source, 0.5, || false).unwrap();
        assert!((half.values[512] - 0.5 * (source.values[512] + result.values[512])).abs() < 1e-6);
        assert_eq!(equalize_height(&source, 1.0, || false).unwrap(), result);
    }

    #[test]
    fn equal_values_share_ranks_and_height_units_are_preserved() {
        let source = HeightField {
            size: 32,
            values: (0..1024).map(|i| {
                if i % 4 == 0 { -8.0 }
                else if i % 4 == 1 { 1.25 }
                else if i % 4 == 2 { 1.25 }
                else { 12.0 }
            }).collect(),
        };
        let output = equalize_height(&source, 1.0, || false).unwrap();
        assert_eq!(output.values[0], -8.0);
        assert_eq!(output.values[3], 12.0);
        assert_eq!(output.values[1], output.values[2]);
        assert!(output.values.iter().all(|v| (-8.0..=12.0).contains(v)));
    }

    #[test]
    fn invalid_inputs_and_cancellation_are_rejected() {
        let source = HeightField::flat(32, 0.25);
        for factor in [f32::NAN, -0.01, 1.01, f32::INFINITY] {
            assert!(equalize_height(&source, factor, || false).is_err());
        }
        let mut bad = source.clone();
        bad.values[0] = f32::NAN;
        assert!(equalize_height(&bad, 0.0, || false).is_err());
        assert_eq!(equalize_height(&source, 0.5, || true).unwrap_err(), "Build cancelled");
    }
}
