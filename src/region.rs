//! Region-aware typed Height/Mask evaluator for strictly local graph operations.
//!
//! A request contains exact integer world-grid coordinates, not a resampled
//! texture tile. Blur asks its parent for a bounded halo in those coordinates;
//! nested Blur nodes grow their halos transitively. No full-world intermediate
//! buffer is allocated. Global operations fail explicitly until they have
//! scientifically valid region/bucket algorithms.
use crate::pipeline::{PortRef, PortType};
use crate::terrain::{Graph, NodeKind, sample_mountain, sample_noise};
use crate::color::{mix_pixel, palette_pixel};
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
}

impl RegionLayout {
    pub fn new(resolution: usize, tiles_per_axis: usize) -> Result<Self, String> {
        if !(32..=MAX_REGION_RESOLUTION).contains(&resolution) ||
            !(2..=64).contains(&tiles_per_axis) || tiles_per_axis >= resolution
        {
            return Err("Region tiles require resolution 32–8192 and 2–64 tiles per axis".into());
        }
        Ok(Self { resolution, tiles_per_axis })
    }

    /// Adjacent tiles share one world sample, without upsampling/cropping.
    /// Tile widths can differ by one sample when the intervals are not divisible.
    pub fn tile(&self, x: usize, y: usize) -> Result<Region, String> {
        if x >= self.tiles_per_axis || y >= self.tiles_per_axis {
            return Err("Region tile index outside grid".into());
        }
        // Divide the (N-1) intervals evenly, not via a rounded-up
        // stride. The latter can start later tiles past the world edge
        // when tile count is large compared with the resolution.
        let intervals = self.resolution - 1;
        let left = x * intervals / self.tiles_per_axis;
        let top = y * intervals / self.tiles_per_axis;
        let right = (x + 1) * intervals / self.tiles_per_axis;
        let bottom = (y + 1) * intervals / self.tiles_per_axis;
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
    pub kind: PortType,
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

/// A region of genuine RGB color values (alpha stays available for graph
/// evaluation). Unlike a scalar proxy, these retain all RGBA channels.
#[derive(Clone, Debug)]
pub struct RegionColorField {
    pub region: Region,
    pub pixels: Vec<[f32; 4]>,
}

impl RegionColorField {
    /// Matches the ordinary full-grid Color PNG export: RGB8, no alpha.
    pub fn export_rgb8(&self, path: &Path) -> Result<(), String> {
        if self.region.area() != self.pixels.len() ||
            self.pixels.iter().flatten().any(|v| !v.is_finite())
        {
            return Err("Color region requires complete finite RGBA pixels".into());
        }
        let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file),
            self.region.width as u32, self.region.height as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(self.pixels.len() * 3);
        for pixel in &self.pixels {
            for &channel in pixel.iter().take(3) {
                bytes.push((channel.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
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

/// Accepted named outputs. All other graph features remain explicitly
/// unsupported in region mode until numerical parity is verified.
fn supported_port(kind: &NodeKind, name: &str) -> Option<PortType> {
    match (kind, name) {
        (NodeKind::Noise { .. } | NodeKind::Mountain { .. } |
         NodeKind::Adjust { autolevel: false, .. } |
         NodeKind::Blend { .. } | NodeKind::Blur { .. } |
         NodeKind::MaskedBlend { .. }, "height") => Some(PortType::Height),
        (NodeKind::Slope, "slope") |
        (NodeKind::Curvature, "curvature") |
        (NodeKind::Aspect { .. }, "aspect") |
        (NodeKind::ElevationMask { .. }, "mask") |
        (NodeKind::MaskLevels { .. }, "mask") => Some(PortType::Mask),
        (NodeKind::Colorize { .. } | NodeKind::ColorMixer { .. }, "color") =>
            Some(PortType::Color),
        _ => None,
    }
}

fn required_inputs(graph: &Graph, source: &PortRef,
    seen: &mut std::collections::HashSet<u64>,
) -> Result<(), String> {
    if !seen.insert(source.node) { return Ok(()); }
    let node = graph.get(source.node).ok_or_else(||
        format!("Missing node #{}", source.node))?;
    if matches!(node.kind, NodeKind::Adjust { autolevel: true, .. }) {
        return Err("Region mode cannot use Adjust autolevel (global min/max)".into());
    }
    if supported_port(&node.kind, &source.port).is_none() {
        return Err(format!(
            "Region mode does not support output #{}:{} of '{}' (global/untiled operation or invalid port); use full-grid builds",
            source.node, source.port, node.kind.label()));
    }
    for input in &node.inputs {
        required_inputs(graph, input, seen)?;
    }
    Ok(())
}

/// Fail before any output files are written when a graph cannot be evaluated
/// regionally; this is intentional, never an implicit memory-heavy fallback.
pub fn validate_region_graph(graph: &Graph, source: &PortRef) -> Result<(), String> {
    graph.validate_structure()?;
    required_inputs(graph, source, &mut std::collections::HashSet::new())
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
    if !matches!(supported_port(&graph.get(source.node)
        .ok_or("Region source node missing")?.kind, &source.port),
        Some(PortType::Height | PortType::Mask))
    {
        return Err("Use evaluate_color_region for Color outputs".into());
    }
    let mut stats = RegionStats::default();
    let field = evaluate(graph, resolution, source, rect, 0, &mut stats, &cancelled)?;
    Ok((field, stats))
}

/// Region-color evaluation is explicit: callers cannot accidentally turn an
/// RGBA output into a grayscale Height/Mask field. Graph validation checks all
/// requested ancestors before building a tile.
pub fn evaluate_color_region(
    graph: &Graph, resolution: usize, source: &PortRef, rect: Region,
) -> Result<(RegionColorField, RegionStats), String> {
    evaluate_color_region_cancellable(graph, resolution, source, rect, || false)
}

pub fn evaluate_color_region_cancellable(
    graph: &Graph, resolution: usize, source: &PortRef, rect: Region,
    cancelled: impl Fn() -> bool,
) -> Result<(RegionColorField, RegionStats), String> {
    rect.validate(resolution)?;
    validate_region_graph(graph, source)?;
    if supported_port(&graph.get(source.node)
        .ok_or("Region color source missing")?.kind, &source.port) != Some(PortType::Color)
    {
        return Err("Requested output is not a supported Color port".into());
    }
    let mut stats = RegionStats::default();
    let field = evaluate_color(graph, resolution, source, rect, 0, &mut stats, &cancelled)?;
    Ok((field, stats))
}

fn evaluate_color(
    graph: &Graph, size: usize, source: &PortRef, rect: Region,
    retained: usize, stats: &mut RegionStats, cancelled: &impl Fn() -> bool,
) -> Result<RegionColorField, String> {
    if cancelled() { return Err("Build cancelled".into()); }
    let node = graph.get(source.node).ok_or("Missing color node")?;
    if supported_port(&node.kind, &source.port) != Some(PortType::Color) {
        return Err(format!("Unsupported regional color output #{}:{}",
            source.node, source.port));
    }
    stats.computed_regions += 1;
    let pixels = match &node.kind {
        NodeKind::Colorize { low, high, midpoint, contrast } => {
            let height = evaluate(graph, size, &node.inputs[0], rect,
                retained, stats, cancelled)?;
            let mut pixels = Vec::with_capacity(rect.area());
            for (i, &h) in height.values.iter().enumerate() {
                if i % 4096 == 0 && cancelled() { return Err("Build cancelled".into()); }
                pixels.push(palette_pixel(h, *low, *high, *midpoint, *contrast));
            }
            stats.observe(retained + height.values.len() + pixels.len() * 4);
            pixels
        },
        NodeKind::ColorMixer { mix } => {
            let a = evaluate_color(graph, size, &node.inputs[0], rect,
                retained, stats, cancelled)?;
            let b = evaluate_color(graph, size, &node.inputs[1], rect,
                retained + a.pixels.len() * 4, stats, cancelled)?;
            let mask = evaluate(graph, size, &node.inputs[2], rect,
                retained + (a.pixels.len() + b.pixels.len()) * 4, stats, cancelled)?;
            let mut pixels = Vec::with_capacity(rect.area());
            for i in 0..rect.area() {
                if i % 4096 == 0 && cancelled() { return Err("Build cancelled".into()); }
                pixels.push(mix_pixel(a.pixels[i], b.pixels[i], mask.values[i], *mix));
            }
            stats.observe(retained + (a.pixels.len() + b.pixels.len() +
                pixels.len()) * 4 + mask.values.len());
            pixels
        },
        other => return Err(format!("Unsupported regional color node: {}", other.label())),
    };
    Ok(RegionColorField { region: rect, pixels })
}

fn halo_rect(rect: Region, size: usize, radius: usize) -> Region {
    let x = rect.x.saturating_sub(radius);
    let y = rect.y.saturating_sub(radius);
    Region {
        x, y,
        width: (rect.x + rect.width + radius).min(size) - x,
        height: (rect.y + rect.height + radius).min(size) - y,
    }
}

fn evaluate(graph: &Graph, size: usize, source: &PortRef, rect: Region,
    retained: usize, stats: &mut RegionStats,
    cancelled: &impl Fn() -> bool,
) -> Result<RegionField, String> {
    if cancelled() { return Err("Build cancelled".into()); }
    let node = graph.get(source.node)
        .ok_or_else(|| format!("Missing node #{}", source.node))?;
    let kind = supported_port(&node.kind, &source.port)
        .ok_or_else(|| format!("Unsupported region output #{}:{}",
            source.node, source.port))?;
    stats.computed_regions += 1;
    // Allocate the output only after dependent regions finish; otherwise
    // reserved but empty parent buffers inflate RSS and evade sample accounting.
    let mut values = Vec::new();
    match &node.kind {
        NodeKind::Noise { seed, frequency, amplitude } => {
            values.reserve_exact(rect.area());
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
            values.reserve_exact(rect.area());
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
            let input = evaluate(graph, size, &node.inputs[0], rect, retained, stats, cancelled)?;
            let exp = 2.0_f32.powf(-2.0 * shaper);
            values.reserve_exact(rect.area());
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
            let left = evaluate(graph, size, &node.inputs[0], rect, retained, stats, cancelled)?;
            let right = evaluate(graph, size, &node.inputs[1], rect,
                retained + left.values.len(), stats, cancelled)?;
            let influence = mix.clamp(0.0, 1.0);
            values.reserve_exact(rect.area());
            for i in 0..rect.area() {
                if i % 4096 == 0 && cancelled() { return Err("Build cancelled".into()); }
                values.push(left.values[i] * (1.0 - influence) + right.values[i] * influence);
            }
            stats.observe(retained + left.values.len() + right.values.len() + values.len());
        }
        NodeKind::MaskedBlend { mix } => {
            let left = evaluate(graph, size, &node.inputs[0], rect,
                retained, stats, cancelled)?;
            let right = evaluate(graph, size, &node.inputs[1], rect,
                retained + left.values.len(), stats, cancelled)?;
            let mask = evaluate(graph, size, &node.inputs[2], rect,
                retained + left.values.len() + right.values.len(), stats, cancelled)?;
            let mix = mix.clamp(0.0, 1.0);
            values.reserve_exact(rect.area());
            for i in 0..rect.area() {
                if i % 4096 == 0 && cancelled() { return Err("Build cancelled".into()); }
                let influence = mix * mask.values[i].clamp(0.0, 1.0);
                values.push(left.values[i] * (1.0 - influence) +
                    right.values[i] * influence);
            }
            stats.observe(retained + left.values.len() + right.values.len() +
                mask.values.len() + values.len());
        }
        NodeKind::ElevationMask { low, high, feather } => {
            let input = evaluate(graph, size, &node.inputs[0], rect,
                retained, stats, cancelled)?;
            let low = low.clamp(0.0, 1.0);
            let high = high.clamp(low, 2.0);
            let feather = feather.clamp(0.001, 0.5);
            values.reserve_exact(rect.area());
            for (i, &v) in input.values.iter().enumerate() {
                if i % 4096 == 0 && cancelled() { return Err("Build cancelled".into()); }
                let rise = ((v - low) / feather).clamp(0.0, 1.0);
                let fall = ((high - v) / feather).clamp(0.0, 1.0);
                let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
                values.push(smooth(rise) * smooth(fall));
            }
            stats.observe(retained + input.values.len() + values.len());
        }
        NodeKind::MaskLevels { black, white, gamma, invert } => {
            if !black.is_finite() || !white.is_finite() || !gamma.is_finite() ||
                *black < 0.0 || *white > 1.0 || *black >= *white ||
                !(0.1..=4.0).contains(gamma)
            {
                return Err("Mask levels require 0 <= black < white <= 1 and gamma 0.1–4".into());
            }
            let input = evaluate(graph, size, &node.inputs[0], rect,
                retained, stats, cancelled)?;
            values.reserve_exact(rect.area());
            for (i, &v) in input.values.iter().enumerate() {
                if i % 4096 == 0 && cancelled() { return Err("Build cancelled".into()); }
                if !v.is_finite() { return Err("Mask levels require finite mask input".into()); }
                let t = ((v - black) / (white - black)).clamp(0.0, 1.0).powf(*gamma);
                values.push(if *invert { 1.0 - t } else { t });
            }
            stats.observe(retained + input.values.len() + values.len());
        }
        NodeKind::Slope | NodeKind::Curvature | NodeKind::Aspect { .. } => {
            // First-order 4-neighbor differential. Nested local operations
            // request their own halos transitively, cropped at the world edge.
            let halo = halo_rect(rect, size, 1);
            let input = evaluate(graph, size, &node.inputs[0], halo,
                retained, stats, cancelled)?;
            values.reserve_exact(rect.area());
            for gy in rect.y..rect.y + rect.height {
                if (gy - rect.y) % 16 == 0 && cancelled() {
                    return Err("Build cancelled".into());
                }
                for gx in rect.x..rect.x + rect.width {
                    let l = input.get(gx.saturating_sub(1), gy);
                    let r = input.get((gx + 1).min(size - 1), gy);
                    let u = input.get(gx, gy.saturating_sub(1));
                    let d = input.get(gx, (gy + 1).min(size - 1));
                    let value = match &node.kind {
                        NodeKind::Slope => {
                            let scale = (size - 1) as f32 * 0.25;
                            let dx = r - l;
                            let dy = d - u;
                            let slope = (dx * dx + dy * dy).sqrt() * scale;
                            (slope / (1.0 + slope)).clamp(0.0, 1.0)
                        },
                        NodeKind::Curvature => {
                            let h = input.get(gx, gy);
                            let laplacian = (l + r + u + d - 4.0 * h) * 18.0;
                            0.5 + 0.5 * laplacian.clamp(-1.0, 1.0)
                        },
                        NodeKind::Aspect { azimuth, half_width, min_slope } => {
                            if !azimuth.is_finite() || !half_width.is_finite() ||
                                !min_slope.is_finite() || !(0.0..=360.0).contains(azimuth) ||
                                !(1.0..=180.0).contains(half_width) ||
                                !(0.0..=2.0).contains(min_slope)
                            {
                                0.0
                            } else {
                                let left = gx.saturating_sub(1);
                                let right = (gx + 1).min(size - 1);
                                let up = gy.saturating_sub(1);
                                let down = (gy + 1).min(size - 1);
                                let world_scale = (size - 1) as f32;
                                let dx = (r - l) * world_scale / (right - left) as f32;
                                let dy = (d - u) * world_scale / (down - up) as f32;
                                let slope = dx.hypot(dy);
                                if !slope.is_finite() || slope <= *min_slope || slope <= 1e-7 {
                                    0.0
                                } else {
                                    let facing = (-dx).atan2(dy);
                                    let target = azimuth.to_radians();
                                    let width = half_width.to_radians();
                                    let distance = (facing - target).sin()
                                        .atan2((facing - target).cos()).abs();
                                    let proximity = (1.0 - distance / width).clamp(0.0, 1.0);
                                    proximity * proximity * (3.0 - 2.0 * proximity)
                                }
                            }
                        },
                        _ => unreachable!(),
                    };
                    values.push(value);
                }
            }
            stats.observe(retained + input.values.len() + values.len());
        }
        NodeKind::Blur { radius } => {
            let radius = radius.clamp(&1, &8).to_owned() as usize;
            let halo = halo_rect(rect, size, radius);
            let input = evaluate(graph, size, &node.inputs[0], halo, retained, stats, cancelled)?;
            values.reserve_exact(rect.area());
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
    Ok(RegionField { region: rect, kind, values })
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

    fn mask_graph() -> (Graph, Vec<PortRef>) {
        let mut graph = Project::default().graph;
        let base = graph.add(NodeKind::Noise {
            seed: 37, frequency: 7.7, amplitude: 0.8,
        });
        let softened = graph.add(NodeKind::Blur { radius: 3 });
        graph.connect(softened, 0, PortRef::height(base)).unwrap();
        let raised = graph.add(NodeKind::Mountain {
            seed: 22, frequency: 5.1, height: 1.0,
        });
        let slope = graph.add(NodeKind::Slope);
        graph.connect(slope, 0, PortRef::height(softened)).unwrap();
        let curvature = graph.add(NodeKind::Curvature);
        graph.connect(curvature, 0, PortRef::height(softened)).unwrap();
        let elevation = graph.add(NodeKind::ElevationMask {
            low: 0.20, high: 0.66, feather: 0.08,
        });
        graph.connect(elevation, 0, PortRef::height(softened)).unwrap();
        let levels = graph.add(NodeKind::MaskLevels {
            black: 0.15, white: 0.9, gamma: 1.8, invert: false,
        });
        graph.connect(levels, 0, PortRef::new(slope, "slope")).unwrap();
        let blended = graph.add(NodeKind::MaskedBlend { mix: 0.73 });
        graph.connect(blended, 0, PortRef::height(softened)).unwrap();
        graph.connect(blended, 1, PortRef::height(raised)).unwrap();
        graph.connect(blended, 2, PortRef::new(levels, "mask")).unwrap();
        (graph, vec![
            PortRef::new(slope, "slope"),
            PortRef::new(curvature, "curvature"),
            PortRef::new(elevation, "mask"),
            PortRef::new(levels, "mask"),
            PortRef::height(blended),
        ])
    }

    #[test]
    fn typed_masks_and_masked_blend_match_full_grid_including_all_seams() {
        let (graph, sources) = mask_graph();
        for size in [32, 65, 128] {
            for source in &sources {
                let full = graph.evaluate_port(size, source).unwrap();
                for count in [2, 3, 7] {
                    let layout = RegionLayout::new(size, count).unwrap();
                    let mut stitched = vec![f32::NAN; size * size];
                    for ty in 0..count {
                        for tx in 0..count {
                            let rect = layout.tile(tx, ty).unwrap();
                            let (tile, _) = evaluate_region(&graph, size, source, rect).unwrap();
                            assert_eq!(tile.kind, full.kind, "{source:?}");
                            for y in 0..rect.height {
                                for x in 0..rect.width {
                                    let index = (rect.y + y) * size + rect.x + x;
                                    let v = tile.values[y * rect.width + x];
                                    if !stitched[index].is_nan() {
                                        assert_eq!(stitched[index], v,
                                            "boundary mismatch {source:?} size={size} tile=({tx},{ty})");
                                    }
                                    stitched[index] = v;
                                }
                            }
                        }
                    }
                    assert!(stitched.iter().all(|v| v.is_finite()));
                    for (i, (region_v, full_v)) in stitched.iter().zip(&full.data.values).enumerate() {
                        assert!((region_v - full_v).abs() <= 1.0e-6,
                            "{source:?} size={size} tile count={count} sample={i}: region={region_v}, full={full_v}");
                    }
                }
            }
        }
    }

    #[test]
    fn typed_region_rejects_wrong_port_and_requires_mask_for_masked_blend() {
        let (mut graph, sources) = mask_graph();
        let roi = Region { x: 10, y: 9, width: 15, height: 12 };
        assert_eq!(evaluate_region(&graph, 65, &sources[0], roi).unwrap().0.kind,
            PortType::Mask);
        assert_eq!(evaluate_region(&graph, 65, sources.last().unwrap(), roi).unwrap().0.kind,
            PortType::Height);
        assert!(evaluate_region(&graph, 65,
            &PortRef::new(sources[0].node, "height"), roi).is_err());
        let blend = sources.last().unwrap().node;
        // The type checker must fail, not misinterpret mask/height as
        // interchangeable scalar fields.
        graph.get_mut(blend).unwrap().inputs[2] = PortRef::height(1);
        assert!(evaluate_region(&graph, 65, &PortRef::height(blend), roi)
            .unwrap_err().contains("Type mismatch"));
    }

    #[test]
    fn typed_masks_have_small_halo_memory_at_four_k() {
        let (graph, sources) = mask_graph();
        let layout = RegionLayout::new(4096, 8).unwrap();
        for source in [sources[1].clone(), sources[4].clone()] {
            let (field, stats) = evaluate_region(
                &graph, 4096, &source, layout.tile(2, 5).unwrap()).unwrap();
            assert!(field.values.iter().all(|v| v.is_finite()));
            assert!(stats.peak_working_samples > field.values.len());
            assert!(stats.peak_working_samples < 4096 * 4096 / 4,
                "region path should not allocate full-grid intermediates");
        }
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
    fn high_tile_count_stays_inside_world_and_never_produces_empty_tiles() {
        for size in [32, 33, 65, 127] {
            for count in 2..=64.min(size - 1) {
                let layout = RegionLayout::new(size, count).unwrap();
                for ty in 0..count {
                    for tx in 0..count {
                        let rect = layout.tile(tx, ty).unwrap();
                        rect.validate(size).unwrap();
                        assert!(rect.width >= 2 && rect.height >= 2);
                        if tx > 0 {
                            let prev = layout.tile(tx - 1, ty).unwrap();
                            assert_eq!(prev.x + prev.width - 1, rect.x);
                        }
                        if ty > 0 {
                            let prev = layout.tile(tx, ty - 1).unwrap();
                            assert_eq!(prev.y + prev.height - 1, rect.y);
                        }
                    }
                }
                let last = layout.tile(count - 1, count - 1).unwrap();
                assert_eq!(last.x + last.width, size);
                assert_eq!(last.y + last.height, size);
            }
        }
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
