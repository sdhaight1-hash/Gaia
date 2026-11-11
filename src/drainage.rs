//! Deterministic D8 downhill drainage for scalar heightfields.
//! This is an analysis tool, not a water or lake simulator: local minima
//! are outlets and flats retain their own area until depression handling exists.
use crate::terrain::HeightField;

#[derive(Clone, Debug)]
pub struct DrainageFields {
    /// Logarithmically normalized upstream contributing area in [0,1].
    pub accumulation: HeightField,
    /// Binary catchment-area threshold derived from total grid area.
    pub channels: HeightField,
    /// Sum of accumulated contributing area at sinks / grid edges.
    /// Equal to pixel count because each cell has exactly one downhill receiver.
    pub outlet_area: u64,
}

pub fn drainage(field: &HeightField, min_catchment_fraction: f32)
    -> Result<DrainageFields, String>
{
    let n = field.size;
    let count = n.checked_mul(n).ok_or("Drainage grid dimensions overflow")?;
    if n < 2 || field.values.len() != count {
        return Err("Drainage requires a complete grid of at least 2x2".to_string());
    }
    if field.values.iter().any(|height| !height.is_finite()) {
        return Err("Drainage requires finite terrain heights".to_string());
    }
    if !min_catchment_fraction.is_finite()
        || !(0.0..=1.0).contains(&min_catchment_fraction)
    {
        return Err("Catchment fraction must be finite and within 0–1".to_string());
    }

    // Fixed neighbor order makes ties reproducible, including at grid edges.
    const NEIGHBORS: [(isize, isize, f32); 8] = [
        (-1, 0, 1.0), (0, -1, 1.0), (1, 0, 1.0), (0, 1, 1.0),
        (-1, -1, std::f32::consts::FRAC_1_SQRT_2),
        (1, -1, std::f32::consts::FRAC_1_SQRT_2),
        (-1, 1, std::f32::consts::FRAC_1_SQRT_2),
        (1, 1, std::f32::consts::FRAC_1_SQRT_2),
    ];

    let mut downstream = vec![None; count];
    for y in 0..n {
        for x in 0..n {
            let index = y * n + x;
            let current = field.values[index];
            let mut gradient = 0.0;
            for (dx, dy, inverse_length) in NEIGHBORS {
                let xx = x as isize + dx;
                let yy = y as isize + dy;
                if xx < 0 || yy < 0 || xx >= n as isize || yy >= n as isize {
                    continue;
                }
                let next = yy as usize * n + xx as usize;
                let drop = current - field.values[next];
                if drop > 0.0 {
                    let candidate = drop * inverse_length;
                    if candidate > gradient {
                        gradient = candidate;
                        downstream[index] = Some(next);
                    }
                }
            }
        }
    }

    // Strict downhill links prohibit cycles. Processing high to low
    // guarantees that every upstream contribution is finalized before
    // it is added to the receiver.
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_unstable_by(|&a, &b| {
        field.values[b].total_cmp(&field.values[a]).then_with(|| a.cmp(&b))
    });
    let mut area = vec![1_u64; count];
    for index in order {
        if let Some(receiver) = downstream[index] {
            area[receiver] += area[index];
        }
    }

    let outlet_area: u64 = (0..count)
        .filter(|&index| downstream[index].is_none())
        .map(|index| area[index]).sum();
    let mut accumulation = HeightField::flat(n, 0.0);
    let mut channels = HeightField::flat(n, 0.0);
    let normalizer = (count as f32).ln();
    let minimum_pixels = (count as f32 * min_catchment_fraction)
        .ceil().max(2.0) as u64;
    for (index, &upstream) in area.iter().enumerate() {
        accumulation.values[index] = (upstream as f32).ln() / normalizer;
        channels.values[index] = if upstream >= minimum_pixels { 1.0 } else { 0.0 };
    }
    Ok(DrainageFields { accumulation, channels, outlet_area })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planar_slope_accumulates_downhill_without_losing_area() {
        let n = 32;
        let field = HeightField {
            size: n,
            values: (0..n).flat_map(|_| (0..n)
                .map(|x| x as f32 / (n - 1) as f32)).collect(),
        };
        let fields = drainage(&field, 0.02).unwrap();
        assert_eq!(fields.outlet_area, (n * n) as u64);
        assert_eq!(fields.accumulation.get(n - 1, 16), 0.0);
        assert!(fields.accumulation.get(0, 16) > fields.accumulation.get(15, 16));
        assert_eq!(fields.channels.get(0, 16), 1.0);
        assert_eq!(fields.channels.get(n - 1, 16), 0.0);
        assert_eq!(
            fields.accumulation,
            drainage(&field, 0.02).unwrap().accumulation
        );
    }

    #[test]
    fn flat_plateau_does_not_invent_rivers_or_cycles() {
        let field = HeightField::flat(32, 0.5);
        let output = drainage(&field, 0.01).unwrap();
        assert_eq!(output.outlet_area, 1024);
        assert!(output.channels.values.iter().all(|&v| v == 0.0));
        assert!(output.accumulation.values.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn all_downhill_paths_in_a_single_sink_basin_converge() {
        let n = 33;
        let c = n / 2;
        let field = HeightField {
            size: n,
            values: (0..n).flat_map(|y| (0..n).map(move |x| {
                x.abs_diff(c) as f32 + y.abs_diff(c) as f32
            })).collect(),
        };
        let output = drainage(&field, 0.04).unwrap();
        assert_eq!(output.outlet_area, (n * n) as u64);
        assert!((output.accumulation.get(c, c) - 1.0).abs() < 1e-6);
        assert_eq!(output.channels.get(c, c), 1.0);
        assert!(output.accumulation.values.iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
    }

    #[test]
    fn invalid_fraction_and_nonfinite_heights_are_rejected() {
        let field = HeightField::flat(32, 0.5);
        assert!(drainage(&field, f32::NAN).is_err());
        assert!(drainage(&field, -0.1).is_err());
        let mut bad = field.clone();
        bad.values[4] = f32::INFINITY;
        assert!(drainage(&bad, 0.01).is_err());
    }
}
