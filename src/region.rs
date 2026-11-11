//! Region-aware Height evaluator for strictly local graph operations.
//!
//! A request contains exact integer world-grid coordinates, not a resampled
//! texture tile. Blur asks its parent for a bounded halo in those coordinates;
//! nested Blur nodes grow their halos transitively. No full-world intermediate
//! buffer is allocated. Global operations fail explicitly until they have
//! scientifically valid region/bucket algorithms.
use crate::pipeline::{PortRef, PortType};
use crate::terrain::{Graph, NodeKind, sample_mountain, sample_noise};
use std::path::Path;

pub const MAX_REGION_RESOLUTION: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Region {
    fn area(self) -> usize { self.width * self.height }

    pub fn validate(self, size: usize) -> Result<(), String> {
        if size < 32 || size > MAX_REGION_RESOLUTION ||
            self.width == 0 || self.height == 0 ||
            self.x.checked_add(self.width).is_none_or(|end| end > size) ||
            self.y.checked_add(self.height).is_none_or(|end| end > size)
        {
            return Err("Region must be nonempty and contained in a 32–8192 pixel world".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionLayout {
    pub resolution: usize,
    pub tiles_per_axis: usize,
    stride: usize,
}

impl RegionLayout {
    pub fn new(resolution: usize, tiles_per_axis: usize) -> Result<Self, String> {
        if !(32..=MAX_REGION_RESOLUTION).contains(&resolution) ||
            !(2..=64).contains(&tiles_per_axis) || tiles_per_axis >= resolution
        {
            return Err("Region tiles require resolution 32–8192 and 2–64 tiles per axis".into());
        }
        Ok(Self {
            resolution, tiles_per_axis,
            stride: (resolution - 1).div_ceil(tiles_per_axis),
        })
    }

    /// Adjacent tiles share one world sample, without upsampling/cropping.
    /// The right/bottom tiles can be narrower when resolution is not divisible.
    pub fn tile(&self, x: usize, y: usize) -> Result<Region, String> {
        if x >= self.tiles_per_axis || y >= self.tiles_per_axis {
            return Err("Region tile index outside grid".into());
        }
        let left = x * self.stride;
        let top = y * self.stride;
        let right = ((x + 1) * self.stride).min(self.resolution - 1);
        let bottom = ((y + 1) * self.stride).min(self.resolution - 1);
        let rect = Region {
            x: left, y: top,
            width: right - left + 1, height: bottom - top + 1,
        };
        rect.validate(self.resolution)?;
        Ok(rect)
    }
}

#[derive(Clone, Debug)]
pub struct RegionField {
    pub region: Region,
    pub values: Vec<f32>,
}

impl RegionField {
    fn get(&self, x: usize, y: usize) -> f32 {
        self.values[(y - self.region.y) * self.region.width + x - self.region.x]
    }

    pub fn export_png16(&self, path: &Path) -> Result<(), String> {
        if self.region.area() != self.values.len() {
            return Err("Region field has incorrect sample count".into());
        }
        let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file),
            self.region.width as u32, self.region.height as u32);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Sixteen);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(self.values.len() * 2);
        for &value in &self.values {
            if !value.is_finite() {
                return Err("Region contains a nonfinite height".into());
            }
            bytes.extend_from_slice(&((value.clamp(0.0, 1.0) * 65535.0)
                .round() as u16).to_be_bytes());
        }
        writer.write_image_data(&bytes).map_err(|e| e.to_string())
    }
}

/// Upper bound on simultaneously resident intermediate float samples for
/// each region request. Does not include PNG encoder buffers or OS overhead.
#[derive(Clone, Copy, Debug, Default)]
pub struct RegionStats {
    pub peak_working_samples: usize,
    pub computed_regions: usize,
}

impl RegionStats {
    fn observe(&mut self, live: usize) {
        self.peak_working_samples = self.peak_working_samples.max(live);
    }
}

fn required_inputs(graph: &Graph, id: u64, seen: &mut std::collections::HashSet<u64>)
    -> Result<(), String>
{
    if !seen.insert(id) { return Ok(()); }
    let node = graph.get(id).ok_or_else(|| format!("Missing node #{id}"))?;
    match &node.kind {
        NodeKind::Noise { .. } | NodeKind::Mountain { .. } |
        NodeKind::Blend { .. } | NodeKind::Blur { .. } => {}
        NodeKind::Adjust { autolevel: false, .. } => {}
        NodeKind::Adjust { autolevel: true, .. } =>
            return Err("Region mode cannot use Adjust autolevel (global min/max)".into()),
        other => return Err(format!(
            "Region mode does not support '{}' (global/untiled operation); use full-grid builds",
            other.label())),
    }
    for input in &node.inputs {
        if input.port != "height" {
            return Err(format!("Region mode currently requires Height inputs: #{}:{}",
                input.node, input.port));
        }
        required_inputs(graph, input.node, seen)?;
    }
    Ok(())
}

/// Fail before any output files are written when a graph cannot be evaluated
/// regionally; this is intentional, never an implicit memory-heavy fallback.
pub fn validate_region_graph(graph: &Graph, source: &PortRef) -> Result<(), String> {
    graph.validate_structure()?;
    if source.port != "height" ||
        !graph.get(source.node).is_some_and(|n| n.kind.output_ports().iter()
            .any(|(name, ty)| *name == "height" && *ty == PortType::Height))
    {
        return Err("Region mode currently supports only named Height outputs".into());
    }
    required_inputs(graph, source.node, &mut std::collections::HashSet::new())
}

pub fn evaluate_region(graph: &Graph, resolution: usize, source: &PortRef,
    rect: Region) -> Result<(RegionField, RegionStats), String>
{
    evaluate_region_cancellable(graph, resolution, source, rect, || false)
}

pub fn evaluate_region_cancellable(graph: &Graph, resolution: usize,
    source: &PortRef, rect: Region, cancelled: impl Fn() -> bool,
) -> Result<(RegionField, RegionStats), String> {
    rect.validate(resolution)?;
    validate_region_graph(graph, source)?;
    let mut stats = RegionStats::default();
    let field = evaluate(graph, resolution, source.node, rect, 0, &mut stats, &cancelled)?;
    Ok((field, stats))
}

fn evaluate(graph: &Graph, size: usize, id: u64, rect: Region,
    retained: usize, stats: &mut RegionStats,
    cancelled: &impl Fn() -> bool,
) -> Result<RegionField, String> {
    if cancelled() { return Err("Build cancelled".into()); }
    let node = graph.get(id).ok_or_else(|| format!("Missing node #{id}"))?;
    stats.computed_regions += 1;
    let mut values = Vec::with_capacity(rect.area());
    match &node.kind {
        NodeKind::Noise { seed, frequency, amplitude } => {
            for y in rect.y..rect.y + rect.height {
                if (y - rect.y) % 16 == 0 && cancelled() {
                    return Err("Build cancelled".into());
                }
                for x in rect.x..rect.x + rect.width {
                    values.push(sample_noise(size, x, y, *seed, *frequency, *amplitude));
                }
            }
            stats.observe(retained + values.len());
        }
        NodeKind::Mountain { seed, frequency, height } => {
            for y in rect.y..rect.y + rect.height {
                if (y - rect.y) % 16 == 0 && cancelled() {
                    return Err("Build cancelled".into());
                }
                for x in rect.x..rect.x + rect.width {
                    values.push(sample_mountain(size, x, y, *seed, *frequency, *height));
                }
            }
            stats.observe(retained + values.len());
        }
        NodeKind::Adjust {
            multiply, shaper, clamp_min, clamp_max, autolevel: false, invert,
        } => {
            if !multiply.is_finite() || !(0.0..=4.0).contains(multiply) ||
                !shaper.is_finite() || !(-1.0..=1.0).contains(shaper) ||
                !clamp_min.is_finite() || !clamp_max.is_finite() ||
                !(0.0..=4.0).contains(clamp_min) ||
                !(0.0..=4.0).contains(clamp_max) || clamp_min > clamp_max
            {
                return Err("Invalid local Adjust parameters".into());
            }
            let input = evaluate(graph, size, node.inputs[0].node, rect, retained, stats, cancelled)?;
            let exp = 2.0_f32.powf(-2.0 * shaper);
            for (i, &value) in input.values.iter().enumerate() {
                if i % 4096 == 0 && cancelled() { return Err("Build cancelled".into()); }
                if !value.is_finite() || value < 0.0 {
                    return Err("Adjust requires finite nonnegative inputs".into());
                }
                let mut h = (value * multiply).max(0.0).powf(exp);
                if *invert { h = 1.0 - h.clamp(0.0, 1.0); }
                if !h.is_finite() { return Err("Adjust result overflowed".into()); }
                values.push(h.clamp(*clamp_min, *clamp_max));
            }
            stats.observe(retained + input.values.len() + values.len());
        }
        NodeKind::Blend { mix } => {
            let left = evaluate(graph, size, node.inputs[0].node, rect, retained, stats, cancelled)?;
            let right = evaluate(graph, size, node.inputs[1].node, rect,
                retained + left.values.len(), stats, cancelled)?;
            let influence = mix.clamp(0.0, 1.0);
            for i in 0..rect.area() {
                if i % 4096 == 0 && cancelled() { return Err("Build cancelled".into()); }
                values.push(left.values[i] * (1.0 - influence) + right.values[i] * influence);
            }
            stats.observe(retained + left.values.len() + right.values.len() + values.len());
        }
        NodeKind::Blur { radius } => {
            let radius = radius.clamp(&1, &8).to_owned() as usize;
            let x = rect.x.saturating_sub(radius);
            let y = rect.y.saturating_sub(radius);
            let halo = Region {
                x, y,
                width: (rect.x + rect.width + radius).min(size) - x,
                height: (rect.y + rect.height + radius).min(size) - y,
            };
            let input = evaluate(graph, size, node.inputs[0].node, halo, retained, stats, cancelled)?;
            for gy in rect.y..rect.y + rect.height {
                if (gy - rect.y) % 16 == 0 && cancelled() {
                    return Err("Build cancelled".into());
                }
                for gx in rect.x..rect.x + rect.width {
                    let mut sum = 0.0_f32;
                    let mut count = 0.0_f32;
                    for dy in -(radius as isize)..=radius as isize {
                        for dx in -(radius as isize)..=radius as isize {
                            let px = gx as isize + dx;
                            let py = gy as isize + dy;
                            if px >= 0 && px < size as isize &&
                                py >= 0 && py < size as isize
                            {
                                sum += input.get(px as usize, py as usize);
                                count += 1.0;
                            }
                        }
                    }
                    values.push(sum / count);
                }
            }
            stats.observe(retained + input.values.len() + values.len());
        }
        _ => return Err(format!("Unsupported regional node: {}", node.kind.label())),
    }
    Ok(RegionField { region: rect, values })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::{Node, Project};

    fn sample_graph() -> (Graph, u64) {
        let mut g = Project::default().graph;
        let noise = g.add(NodeKind::Noise {
            seed: 11, frequency: 6.3, amplitude: 0.7,
        });
        let mountain = g.add(NodeKind::Mountain {
            seed: 92, frequency: 3.8, height: 1.1,
        });
        let adjusted = g.add(NodeKind::Adjust {
            multiply: 0.85, shaper: -0.3, clamp_min: 0.0,
            clamp_max: 3.0, autolevel: false, invert: false,
        });
        g.connect(adjusted, 0, PortRef::height(noise)).unwrap();
        let blur_a = g.add(NodeKind::Blur { radius: 2 });
        g.connect(blur_a, 0, PortRef::height(adjusted)).unwrap();
        let blur_b = g.add(NodeKind::Blur { radius: 4 });
        g.connect(blur_b, 0, PortRef::height(blur_a)).unwrap();
        let blend = g.add(NodeKind::Blend { mix: 0.43 });
        g.connect(blend, 0, PortRef::height(blur_b)).unwrap();
        g.connect(blend, 1, PortRef::height(mountain)).unwrap();
        (g, blend)
    }

    #[test]
    fn stitched_regions_match_whole_grid_for_nested_halos_and_blends() {
        let (graph, node) = sample_graph();
        for size in [32, 65, 128] {
            let full = graph.evaluate_port(size, &PortRef::height(node)).unwrap();
            for ntiles in [2, 3, 4] {
                let layout = RegionLayout::new(size, ntiles).unwrap();
                let mut stitched = vec![f32::NAN; size * size];
                for ty in 0..ntiles {
                    for tx in 0..ntiles {
                        let rect = layout.tile(tx, ty).unwrap();
                        let (field, stats) =
                            evaluate_region(&graph, size, &PortRef::height(node), rect).unwrap();
                        assert!(stats.peak_working_samples > 0);
                        for y in 0..rect.height {
                            for x in 0..rect.width {
                                let pos = (rect.y + y) * size + rect.x + x;
                                let value = field.values[y * rect.width + x];
                                if !stitched[pos].is_nan() {
                                    assert_eq!(stitched[pos], value, "seam at {tx},{ty} ({x},{y})");
                                }
                                stitched[pos] = value;
                            }
                        }
                    }
                }
                assert!(stitched.iter().all(|v| v.is_finite()));
                for (x, (a, b)) in stitched.iter().zip(&full.data.values).enumerate() {
                    assert!((a-b).abs() <= 1.0e-6,
                        "size {size} tiles {ntiles} pixel {x}: {a} vs {b}");
                }
            }
        }
    }

    #[test]
    fn regional_execution_uses_bounded_samples_not_full_grid() {
        let (graph, node) = sample_graph();
        let size = 4096;
        let layout = RegionLayout::new(size, 8).unwrap();
        let (field, stats) = evaluate_region(&graph, size, &PortRef::height(node),
            layout.tile(1, 1).unwrap()).unwrap();
        assert!(stats.peak_working_samples < size * size / 4);
        assert!(stats.peak_working_samples >= field.values.len());
    }

    #[test]
    fn rejects_unsupported_global_nodes_invalid_ports_and_autolevel() {
        let mut graph = Project::default().graph;
        assert!(evaluate_region(&graph, 64, &PortRef::height(3),
            Region { x: 0, y: 0, width: 16, height: 16 })
            .unwrap_err().contains("does not support"));
        let noise = graph.add(NodeKind::Noise {
            seed: 1, frequency: 3.0, amplitude: 0.5,
        });
        let adjust = graph.add(NodeKind::Adjust {
            multiply: 1.0, shaper: 0.0, clamp_min: 0.0, clamp_max: 4.0,
            autolevel: true, invert: false,
        });
        graph.connect(adjust, 0, PortRef::height(noise)).unwrap();
        let roi = Region { x: 0, y: 0, width: 16, height: 16 };
        assert!(evaluate_region(&graph, 64, &PortRef::height(adjust), roi)
            .unwrap_err().contains("autolevel"));
        assert!(evaluate_region(&graph, 64, &PortRef::new(noise, "wear"), roi)
            .is_err());
        assert!(evaluate_region(&graph, 64, &PortRef::height(noise),
            Region { x: 60, y: 0, width: 6, height: 8 }).is_err());
    }

    #[test]
    fn validates_edges_and_cancellation() {
        assert!(RegionLayout::new(32, 32).is_err());
        assert!(RegionLayout::new(32, 1).is_err());
        let layout = RegionLayout::new(73, 4).unwrap();
        assert_eq!(layout.tile(0, 0).unwrap().x, 0);
        assert_eq!(layout.tile(3, 3).unwrap().x +
            layout.tile(3, 3).unwrap().width, 73);
        assert_eq!(layout.tile(0, 0).unwrap().x +
            layout.tile(0, 0).unwrap().width - 1, layout.tile(1, 0).unwrap().x);
        let (graph, node) = sample_graph();
        assert_eq!(evaluate_region_cancellable(&graph, 73, &PortRef::height(node),
            layout.tile(0, 0).unwrap(), || true).unwrap_err(), "Build cancelled");
    }

    #[test]
    fn png16_can_export_rectangular_last_tile() {
        let dir = tempfile::tempdir().unwrap();
        let layout = RegionLayout::new(32, 3).unwrap();
        let (graph, id) = sample_graph();
        let (field, _) = evaluate_region(&graph, 32, &PortRef::height(id),
            layout.tile(2, 1).unwrap()).unwrap();
        let path = dir.path().join("edge.png");
        field.export_png16(&path).unwrap();
        let info = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(path).unwrap())).read_info().unwrap();
        assert_eq!(info.info().width as usize, field.region.width);
        assert_eq!(info.info().height as usize, field.region.height);
    }
}
