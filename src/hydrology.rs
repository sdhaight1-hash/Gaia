//! Conservative experimental hydraulic erosion with explicit water and sediment
//! ledgers. This is an independent numerical model, not QuadSpinner's solver.
//!
//! Each iteration performs (1) rainfall, (2) bounded shared-face water flux,
//! (3) upwind suspended-sediment advection, (4) erosion/deposition, and
//! (5) evaporation. Exiting water/sediment is accounted for at open edges.
use crate::terrain::HeightField;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HydraulicParameters {
    pub iterations: u32,
    pub rain: f32,
    pub evaporation: f32,
    pub capacity: f32,
    pub erosion_rate: f32,
    pub deposition_rate: f32,
}

impl Default for HydraulicParameters {
    fn default() -> Self {
        Self {
            iterations: 48,
            rain: 0.004,
            evaporation: 0.03,
            capacity: 2.0,
            erosion_rate: 0.30,
            deposition_rate: 0.18,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MassLedger {
    pub rainfall: f64,
    pub evaporated: f64,
    pub water_outflow: f64,
    pub sediment_outflow: f64,
}

pub struct HydraulicFields {
    pub height: HeightField,
    pub water: HeightField,
    pub sediment: HeightField,
    pub wear: HeightField,
    pub deposits: HeightField,
    /// Cumulative outward water flux per cell; a flow-intensity mask.
    pub flow: HeightField,
    pub ledger: MassLedger,
}

impl HydraulicFields {
    pub fn water_residual(&self) -> f64 {
        self.ledger.rainfall - self.ledger.evaporated - self.ledger.water_outflow
            - self.water.mass()
    }

    pub fn solid_residual(&self, initial: &HeightField) -> f64 {
        initial.mass() - self.ledger.sediment_outflow
            - self.height.mass() - self.sediment.mass()
    }
}

/// Positive, bounded water movement over cardinal edges. Each directed transfer
/// debits one cell, credits the next, and transports the same fraction of
/// suspended sediment. Boundary sinks are tracked rather than disappearing.
/// No voxels are used for water in this solver.
pub fn simulate(
    input: &HeightField, parameters: HydraulicParameters, cancelled: impl Fn() -> bool,
) -> Result<HydraulicFields, String> {
    let n = input.size;
    if n == 0 || input.values.len() != n * n || input.values.iter()
        .any(|v| !v.is_finite() || *v < 0.0)
    {
        return Err("Hydraulic solver requires a finite, nonnegative square field".into());
    }
    let steps = parameters.iterations.min(200);
    let rain = parameters.rain.clamp(0.0, 0.05);
    let evaporation = parameters.evaporation.clamp(0.0, 1.0);
    let capacity_coefficient = parameters.capacity.clamp(0.0, 20.0);
    let erosion_rate = parameters.erosion_rate.clamp(0.0, 1.0);
    let deposition_rate = parameters.deposition_rate.clamp(0.0, 1.0);
    let mut height = input.clone();
    let mut water = HeightField::flat(n, 0.0);
    let mut sediment = HeightField::flat(n, 0.0);
    let mut wear = HeightField::flat(n, 0.0);
    let mut deposits = HeightField::flat(n, 0.0);
    let mut flow = HeightField::flat(n, 0.0);
    let mut ledger = MassLedger::default();

    for _ in 0..steps {
        if cancelled() { return Err("Build cancelled".to_string()); }

        // Rainfall is an external source. Accumulate it before water movement.
        for v in &mut water.values { *v += rain; }
        ledger.rainfall += f64::from(rain) * (n * n) as f64;

        let old_water = water.values.clone();
        let old_sediment = sediment.values.clone();
        let mut delta_water = vec![0.0f32; n * n];
        let mut delta_sediment = vec![0.0f32; n * n];
        let mut transported = vec![0.0f32; n * n];

        for y in 0..n {
            if y % 16 == 0 && cancelled() {
                return Err("Build cancelled".to_string());
            }
            for x in 0..n {
                let i = y * n + x;
                let volume = old_water[i];
                if volume <= 0.0 { continue; }
                let head = height.values[i] + volume;
                let neighbors = [
                    (x.wrapping_sub(1), y, x > 0),
                    (x + 1, y, x + 1 < n),
                    (x, y.wrapping_sub(1), y > 0),
                    (x, y + 1, y + 1 < n),
                ];
                // Assign each outgoing face a downward hydraulic gradient.
                // Out-of-world neighbors have sea-level head = 0.
                let mut gradients = [0.0f32; 4];
                let mut sum = 0.0f32;
                for (slot, (nx, ny, interior)) in neighbors.iter().copied().enumerate() {
                    let downstream_head = if interior {
                        let j = ny * n + nx;
                        height.values[j] + old_water[j]
                    } else {
                        0.0
                    };
                    let gradient = (head - downstream_head).max(0.0);
                    gradients[slot] = gradient;
                    sum += gradient;
                }
                if sum <= 0.0 { continue; }
                let outgoing = (volume * 0.55).min(sum * 0.12);
                if outgoing <= 0.0 { continue; }
                let available_sediment = old_sediment[i].max(0.0);
                let sediment_fraction = outgoing / volume;
                let carried = available_sediment * sediment_fraction;

                delta_water[i] -= outgoing;
                delta_sediment[i] -= carried;
                transported[i] += outgoing;
                flow.values[i] += outgoing;
                for (slot, (nx, ny, interior)) in neighbors.iter().copied().enumerate() {
                    let gradient = gradients[slot];
                    if gradient <= 0.0 { continue; }
                    let ratio = gradient / sum;
                    let water_flux = outgoing * ratio;
                    let sediment_flux = carried * ratio;
                    if interior {
                        let j = ny * n + nx;
                        delta_water[j] += water_flux;
                        delta_sediment[j] += sediment_flux;
                    } else {
                        ledger.water_outflow += f64::from(water_flux);
                        ledger.sediment_outflow += f64::from(sediment_flux);
                    }
                }
            }
        }

        // Commit fluxes simultaneously to avoid order-dependent transport.
        for i in 0..n * n {
            water.values[i] = (old_water[i] + delta_water[i]).max(0.0);
            sediment.values[i] = (old_sediment[i] + delta_sediment[i]).max(0.0);
        }

        // Erode only material genuinely present; deposit only suspended mass.
        // Capacity is controlled by local flux and elevation gradient.
        for y in 0..n {
            if y % 16 == 0 && cancelled() {
                return Err("Build cancelled".to_string());
            }
            for x in 0..n {
                let i = y * n + x;
                let h = height.values[i];
                let mut slope = 0.0f32;
                if x > 0 { slope = slope.max(h - height.values[i - 1]); }
                if x + 1 < n { slope = slope.max(h - height.values[i + 1]); }
                if y > 0 { slope = slope.max(h - height.values[i - n]); }
                if y + 1 < n { slope = slope.max(h - height.values[i + n]); }
                let target = capacity_coefficient
                    * (transported[i] + water.values[i] * 0.03)
                    * (0.04 + slope.max(0.0));
                if sediment.values[i] < target {
                    let eroded = ((target - sediment.values[i]) * erosion_rate)
                        .min(height.values[i].max(0.0));
                    height.values[i] -= eroded;
                    sediment.values[i] += eroded;
                    wear.values[i] += eroded;
                } else {
                    let deposited = ((sediment.values[i] - target) * deposition_rate)
                        .min(sediment.values[i].max(0.0));
                    height.values[i] += deposited;
                    sediment.values[i] -= deposited;
                    deposits.values[i] += deposited;
                }
                let loss = water.values[i] * evaporation;
                water.values[i] -= loss;
                ledger.evaporated += f64::from(loss);
            }
        }
    }

    let fields = HydraulicFields {
        height, water, sediment, wear, deposits, flow, ledger
    };
    for field in [
        &fields.height, &fields.water, &fields.sediment,
        &fields.wear, &fields.deposits, &fields.flow,
    ] {
        if field.values.iter().any(|v| !v.is_finite() || *v < -0.000001) {
            return Err("Hydraulic simulation produced invalid negative/nonfinite values".into());
        }
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hill(n: usize) -> HeightField {
        let mut h = HeightField::flat(n, 0.0);
        for y in 0..n {
            for x in 0..n {
                let dx = x as f32 / n as f32 - 0.5;
                let dy = y as f32 / n as f32 - 0.5;
                h.values[y * n + x] = (1.0 - (dx * dx + dy * dy) * 3.0).max(0.0);
            }
        }
        h
    }

    #[test]
    fn dry_terrain_does_not_change() {
        let h = hill(32);
        let parameters = HydraulicParameters { rain: 0.0, ..Default::default() };
        let result = simulate(&h, parameters, || false).unwrap();
        assert_eq!(h, result.height);
        assert_eq!(result.water.mass(), 0.0);
        assert_eq!(result.wear.mass(), 0.0);
    }

    #[test]
    fn conserves_water_and_solids_with_open_outlets() {
        let h = hill(64);
        let result = simulate(&h, HydraulicParameters::default(), || false).unwrap();
        let water_error = result.water_residual().abs();
        let solid_error = result.solid_residual(&h).abs();
        assert!(water_error < 0.03, "water residual: {water_error}");
        assert!(solid_error < 0.03, "solid residual: {solid_error}");
        assert!(result.ledger.water_outflow > 0.0);
        assert!(result.wear.mass() > 0.0);
        assert!(result.flow.mass() > 0.0);
    }

    #[test]
    fn deterministic_and_nonnegative() {
        let h = hill(32);
        let a = simulate(&h, HydraulicParameters::default(), || false).unwrap();
        let b = simulate(&h, HydraulicParameters::default(), || false).unwrap();
        assert_eq!(a.height, b.height);
        assert_eq!(a.water, b.water);
        assert_eq!(a.sediment, b.sediment);
        assert!(a.height.values.iter().all(|v| *v >= 0.0));
    }

    #[test]
    fn obeys_cancellation() {
        let h = hill(32);
        assert_eq!(simulate(&h, HydraulicParameters::default(), || true)
            .unwrap_err(), "Build cancelled");
    }
}
