//! Deterministic, CPU-side heightfield graph. No GPU or UI dependencies.
use serde::{Deserialize, Serialize};
use crate::pipeline::PortRef;
use crate::hydrology::HydraulicParameters;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HeightField {
    pub size: usize,
    pub values: Vec<f32>,
}

impl HeightField {
    pub fn flat(size: usize, value: f32) -> Self {
        Self { size, values: vec![value; size * size] }
    }

    pub fn get(&self, x: usize, y: usize) -> f32 {
        self.values[y * self.size + x]
    }

    pub fn mass(&self) -> f64 {
        self.values.iter().map(|v| f64::from(*v)).sum()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NodeKind {
    Mountain { seed: u32, frequency: f32, height: f32 },
    ImportHeight { path: String, scale: f32 },
    Colorize { low: [f32; 3], high: [f32; 3], midpoint: f32, contrast: f32 },
    ColorMixer { mix: f32 }, 
    Range { seed: u32, height: f32, roughness: f32 },
    Crater { seed: u32, radius: f32, depth: f32, rim: f32 },
    Dunes { seed: u32, frequency: f32, amplitude: f32 },
    Canyon { seed: u32, width: f32, depth: f32 },
    Noise { seed: u32, frequency: f32, amplitude: f32 },
    Erosion { iterations: u32, talus: f32, strength: f32 },
    Hydraulic { params: HydraulicParameters }, 
    Terrace { steps: u32, softness: f32 },
    Blur { radius: u32 },
    Blend { mix: f32 },
    Slope,
    Curvature,
    ElevationMask { low: f32, high: f32, feather: f32 },
    Warp { seed: u32, frequency: f32, amount: f32 },
    MaskedBlend { mix: f32 },
}

impl NodeKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Mountain { .. } => "Mountain",
            Self::ImportHeight { .. } => "Import heightmap",
            Self::Colorize { .. } => "Colorize",
            Self::ColorMixer { .. } => "Color mixer",
            Self::Range { .. } => "Mountain range",
            Self::Crater { .. } => "Crater",
            Self::Dunes { .. } => "Dunes",
            Self::Canyon { .. } => "Canyon",
            Self::Noise { .. } => "Noise",
            Self::Erosion { .. } => "Thermal erosion",
            Self::Hydraulic { .. } => "Hydraulic erosion",
            Self::Terrace { .. } => "Terrace",
            Self::Blur { .. } => "Blur",
            Self::Blend { .. } => "Blend",
            Self::Slope => "Slope mask",
            Self::Curvature => "Curvature mask",
            Self::ElevationMask { .. } => "Elevation mask",
            Self::Warp { .. } => "Warp",
            Self::MaskedBlend { .. } => "Masked Blend",
        }
    }

    pub fn input_count(&self) -> usize {
        match self {
            Self::Mountain { .. } | Self::Noise { .. } | Self::ImportHeight { .. } |
                Self::Range { .. } | Self::Crater { .. } |
                Self::Dunes { .. } | Self::Canyon { .. } => 0,
            Self::Blend { .. } => 2,
            Self::MaskedBlend { .. } => 3,
            _ => 1,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    pub kind: NodeKind,
    pub inputs: Vec<PortRef>,
    pub position: [f32; 2],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub output: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub resolution: usize,
    pub graph: Graph,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            resolution: 128,
            graph: Graph {
                nodes: vec![
                    Node {
                        id: 1,
                        kind: NodeKind::Mountain { seed: 42, frequency: 4.0, height: 1.0 },
                        inputs: vec![],
                        position: [35.0, 90.0],
                    },
                    Node {
                        id: 2,
                        kind: NodeKind::Erosion { iterations: 24, talus: 0.008, strength: 0.28 },
                        inputs: vec![PortRef::height(1)],
                        position: [265.0, 90.0],
                    },
                    Node {
                        id: 3,
                        kind: NodeKind::Terrace { steps: 12, softness: 0.75 },
                        inputs: vec![PortRef::height(2)],
                        position: [495.0, 90.0],
                    },
                ],
                output: 3,
            },
        }
    }
}

impl Graph {
    pub fn get(&self, id: u64) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Connect a named source output to a typed target input.
    /// Reject invalid ports, bad slots, self-links and cyclic dependencies
    /// before modifying the graph.
    pub fn connect(
        &mut self,
        target: u64,
        slot: usize,
        source: PortRef,
    ) -> Result<(), String> {
        let source_node = self.get(source.node)
            .ok_or_else(|| format!("Missing source node #{}", source.node))?;
        let source_type = source_node.kind.output_ports().iter()
            .find(|(name, _)| *name == source.port)
            .map(|(_, ty)| *ty)
            .ok_or_else(|| format!(
                "Node #{} has no output '{}'", source.node, source.port
            ))?;
        let target_node = self.get(target)
            .ok_or_else(|| format!("Missing target node #{target}"))?;
        let target_type = target_node.kind.input_type(slot)
            .ok_or_else(|| format!("Node #{target} has no input {slot}"))?;
        if source_type != target_type {
            return Err(format!(
                "Cannot connect {} to {} input", source_type.label(),
                target_type.label()
            ));
        }
        if target == source.node {
            return Err("Cannot connect a node to itself".to_owned());
        }
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![source.node];
        while let Some(node_id) = stack.pop() {
            if node_id == target {
                return Err("Connection would create a graph cycle".to_owned());
            }
            if !visited.insert(node_id) {
                continue;
            }
            let node = self.get(node_id)
                .ok_or_else(|| format!("Missing dependency node #{node_id}"))?;
            stack.extend(node.inputs.iter().map(|input| input.node));
        }
        let target_node = self.nodes.iter_mut()
            .find(|node| node.id == target)
            .expect("validated target must exist");
        target_node.inputs[slot] = source;
        Ok(())
    }

    pub fn add(&mut self, kind: NodeKind) -> u64 {
        let id = self.nodes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
        let inputs = match kind.input_count() {
            0 => vec![],
            1 => vec![PortRef::height(self.output)],
            2 => vec![PortRef::height(self.output), PortRef::height(1)],
            3 => {
                let mask = self.nodes.iter().rev().find_map(|node| {
                    node.kind.output_ports().iter()
                        .find(|(_, ty)| *ty == crate::pipeline::PortType::Mask)
                        .map(|(name, _)| PortRef::new(node.id, *name))
                }).unwrap_or_else(|| PortRef::new(self.output, "wear"));
                vec![PortRef::height(self.output), PortRef::height(1), mask]
            }
            _ => unreachable!("all node kinds have up to three inputs"),
        };
        let position = [
            65.0 + (self.nodes.len() % 4) as f32 * 220.0,
            205.0 + (self.nodes.len() / 4) as f32 * 115.0,
        ];
        let has_height = kind.output_ports().iter()
            .any(|(name, _)| *name == "height");
        self.nodes.push(Node { id, kind, inputs, position });
        if has_height {
            self.output = id;
        }
        id
    }

    /// Backwards-compatible heightfield-only entry point.
    pub fn evaluate(&self, resolution: usize) -> Result<HeightField, String> {
        let output = self.evaluate_port(resolution, &PortRef::height(self.output))?;
        if output.kind != crate::pipeline::PortType::Height {
            return Err("Project output is not a heightfield".to_owned());
        }
        Ok((*output.data).clone())
    }
}

impl Project {
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let data = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, data).map_err(|e| e.to_string())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let project: Self = serde_json::from_str(&data).map_err(|e| e.to_string())?;
        project.graph.evaluate(project.resolution)?;
        Ok(project)
    }
}

// Smooth, deterministic value noise with a stable integer hash.
fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9e37_79b9)
        ^ (y as u32).wrapping_mul(0x85eb_ca6b) ^ seed;
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    h as f32 / u32::MAX as f32
}

fn noise(x: f32, y: f32, seed: u32) -> f32 {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let tx = (x - ix as f32).clamp(0.0, 1.0);
    let ty = (y - iy as f32).clamp(0.0, 1.0);
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sy = ty * ty * (3.0 - 2.0 * ty);
    let a = hash(ix, iy, seed);
    let b = hash(ix + 1, iy, seed);
    let c = hash(ix, iy + 1, seed);
    let d = hash(ix + 1, iy + 1, seed);
    (a + (b - a) * sx) * (1.0 - sy) + (c + (d - c) * sx) * sy
}

pub(crate) fn fractal(x: f32, y: f32, seed: u32, frequency: f32) -> f32 {
    let mut sum = 0.0;
    let mut norm = 0.0;
    let mut weight = 1.0;
    let mut freq = frequency.clamp(0.1, 80.0);
    for octave in 0..5 {
        sum += noise(x * freq, y * freq, seed.wrapping_add(octave)) * weight;
        norm += weight;
        weight *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

pub(crate) fn generate_noise(size: usize, seed: u32, frequency: f32, amplitude: f32) -> HeightField {
    let mut out = HeightField::flat(size, 0.0);
    for y in 0..size {
        for x in 0..size {
            let u = x as f32 / (size - 1) as f32;
            let v = y as f32 / (size - 1) as f32;
            out.values[y * size + x] = fractal(u, v, seed, frequency) * amplitude.clamp(0.0, 4.0);
        }
    }
    out
}

pub(crate) fn generate_mountain(size: usize, seed: u32, frequency: f32, height: f32) -> HeightField {
    let mut out = HeightField::flat(size, 0.0);
    for y in 0..size {
        for x in 0..size {
            let u = x as f32 / (size - 1) as f32;
            let v = y as f32 / (size - 1) as f32;
            let warp = fractal(u, v, seed.wrapping_add(19), 2.2) - 0.5;
            let dx = u - 0.5 + warp * 0.16;
            let dy = v - 0.5 - warp * 0.12;
            let distance = (dx * dx + dy * dy).sqrt();
            let footprint = (1.0 - distance * 1.85).max(0.0).powf(1.4);
            let n = fractal(u, v, seed, frequency);
            // Ridged multi-scale detail over a mountainous radial envelope.
            let ridges = (1.0 - (2.0 * n - 1.0).abs()).powf(2.2);
            out.values[y * size + x] = footprint * (0.27 + 0.73 * ridges) * height.clamp(0.0, 4.0);
        }
    }
    out
}

/// A finite-volume talus relaxation: each cell only transfers its own material
/// to downhill cardinal neighbors. Every transfer is added and subtracted once.
/// Conservative thermal erosion and its true cumulative transport masks.
pub struct ErosionFields {
    pub height: HeightField,
    pub wear: HeightField,
    pub deposits: HeightField,
}

pub fn thermal_erosion(input: &HeightField, iterations: u32, talus: f32, strength: f32) -> HeightField {
    thermal_erosion_fields(input, iterations, talus, strength).height
}

/// Cumulative wear/deposits record all material leaving/entering each cell
/// at each step. Wear can exceed net height loss where material is repeatedly
/// mobilized. This is thermal talus transport, NOT rainfall-driven erosion.
pub fn thermal_erosion_fields(
    input: &HeightField, iterations: u32, talus: f32, strength: f32,
) -> ErosionFields {
    thermal_erosion_fields_checked(input, iterations, talus, strength, || false)
        .expect("uninterrupted erosion must finish")
}

pub(crate) fn thermal_erosion_fields_checked<F: Fn() -> bool>(
    input: &HeightField, iterations: u32, talus: f32, strength: f32, cancelled: F,
) -> Option<ErosionFields> {
    let mut height = input.clone();
    let size = height.size;
    let mut wear = HeightField::flat(size, 0.0);
    let mut deposits = HeightField::flat(size, 0.0);
    let talus = talus.clamp(0.0, 1.0);
    let strength = strength.clamp(0.0, 0.5);
    for _ in 0..iterations {
        if cancelled() { return None; }
        let mut delta = vec![0.0_f32; height.values.len()];
        for y in 0..size {
            if y % 16 == 0 && cancelled() { return None; }
            for x in 0..size {
                let index = y * size + x;
                let current = height.values[index];
                let neighbors = [
                    (x.wrapping_sub(1), y, x > 0),
                    (x + 1, y, x + 1 < size),
                    (x, y.wrapping_sub(1), y > 0),
                    (x, y + 1, y + 1 < size),
                ];
                let mut flows = [(0_usize, 0.0_f32); 4];
                let mut total_excess = 0.0;
                for (i, &(nx, ny, valid)) in neighbors.iter().enumerate() {
                    if valid {
                        let neighbor = ny * size + nx;
                        let excess = (current - height.values[neighbor] - talus).max(0.0);
                        flows[i] = (neighbor, excess);
                        total_excess += excess;
                    }
                }
                if total_excess > 0.0 {
                    let amount = (strength * total_excess).min(current.max(0.0) * 0.45);
                    delta[index] -= amount;
                    wear.values[index] += amount;
                    for (neighbor, excess) in flows {
                        if excess > 0.0 {
                            let transferred = amount * (excess / total_excess);
                            delta[neighbor] += transferred;
                            deposits.values[neighbor] += transferred;
                        }
                    }
                }
            }
        }
        for (value, change) in height.values.iter_mut().zip(delta) {
            *value += change;
        }
    }
    Some(ErosionFields { height, wear, deposits })
}

pub(crate) fn terrace(input: &HeightField, steps: u32, softness: f32) -> HeightField {
    let steps = steps.clamp(2, 128) as f32;
    let softness = softness.clamp(0.0, 1.0);
    let mut out = input.clone();
    for v in &mut out.values {
        let t = (*v).max(0.0) * steps;
        let fraction = t.fract();
        let eased = fraction * fraction * (3.0 - 2.0 * fraction);
        *v = (t.floor() + (1.0 - softness) * fraction + softness * eased) / steps;
    }
    out
}

pub(crate) fn blur(input: &HeightField, radius: u32) -> HeightField {
    let radius = radius.clamp(1, 8) as isize;
    let size = input.size as isize;
    let mut out = input.clone();
    for y in 0..size {
        for x in 0..size {
            let mut sum = 0.0;
            let mut count = 0.0;
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx >= 0 && nx < size && ny >= 0 && ny < size {
                        sum += input.get(nx as usize, ny as usize);
                        count += 1.0;
                    }
                }
            }
            out.values[y as usize * input.size + x as usize] = sum / count;
        }
    }
    out
}

pub fn export_png16(field: &HeightField, path: &Path) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, field.size as u32, field.size as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Sixteen);
    let mut png = encoder.write_header().map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(field.values.len() * 2);
    for &value in &field.values {
        bytes.extend_from_slice(&((value.clamp(0.0, 1.0) * 65535.0).round() as u16).to_be_bytes());
    }
    png.write_image_data(&bytes).map_err(|e| e.to_string())
}

pub fn export_obj(field: &HeightField, path: &Path) -> Result<(), String> {
    use std::io::Write;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    writeln!(out, "# Gaia terrain mesh").map_err(|e| e.to_string())?;
    let n = field.size;
    for y in 0..n {
        for x in 0..n {
            let fx = x as f32 / (n - 1) as f32 * 2.0 - 1.0;
            let fy = y as f32 / (n - 1) as f32 * 2.0 - 1.0;
            writeln!(out, "v {fx:.6} {:.6} {fy:.6}", field.get(x, y)).map_err(|e| e.to_string())?;
        }
    }
    for y in 0..(n - 1) {
        for x in 0..(n - 1) {
            let a = y * n + x + 1;
            let b = a + 1;
            let c = a + n;
            let d = c + 1;
            writeln!(out, "f {a} {c} {b}\nf {b} {c} {d}").map_err(|e| e.to_string())?;
        }
    }
    out.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_project_is_deterministic() {
        let p = Project::default();
        let a = p.graph.evaluate(64).unwrap();
        let b = p.graph.evaluate(64).unwrap();
        assert_eq!(a, b);
        assert!(a.values.iter().any(|&v| v > 0.1));
    }

    #[test]
    fn thermal_erosion_conserves_sediment() {
        let input = generate_mountain(64, 11, 3.0, 1.0);
        let output = thermal_erosion(&input, 40, 0.005, 0.25);
        assert!((input.mass() - output.mass()).abs() < 0.002);
        assert!(output.values.iter().all(|&v| v >= 0.0));
    }

    #[test]
    fn missing_input_is_rejected() {
        let mut p = Project::default();
        p.graph.nodes[1].inputs = vec![PortRef::height(999)];
        assert!(p.graph.evaluate(64).unwrap_err().contains("Missing node"));
    }

    #[test]
    fn graph_cycle_is_rejected() {
        let mut p = Project::default();
        p.graph.nodes[1].inputs = vec![PortRef::height(3)];
        assert!(p.graph.evaluate(64).unwrap_err().contains("cycle"));
    }

    #[test]
    fn graph_connect_enforces_types_and_cycle_safety() {
        let mut p = Project::default();
        assert!(p.graph.connect(2, 0, PortRef::height(1)).is_ok());
        assert!(p.graph.connect(2, 0, PortRef::new(3, "height"))
            .unwrap_err().contains("cycle"));
        assert!(p.graph.connect(2, 0, PortRef::new(2, "height"))
            .unwrap_err().contains("itself"));
        let slope = p.graph.add(NodeKind::Slope);
        p.graph.connect(slope, 0, PortRef::height(1)).unwrap();
        assert!(p.graph.connect(2, 0, PortRef::new(slope, "slope"))
            .unwrap_err().contains("Cannot connect"));
        assert_eq!(p.graph.get(2).unwrap().inputs[0], PortRef::height(1));
    }

    #[test]
    fn project_serialization_roundtrip() {
        let p = Project::default();
        let json = serde_json::to_string(&p).unwrap();
        let restored: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.graph.evaluate(32).unwrap(), p.graph.evaluate(32).unwrap());
    }

    #[test]
    fn output_node_selection_changes_result() {
        let mut p = Project::default();
        let final_height = p.graph.evaluate(32).unwrap();
        p.graph.output = 1;
        assert_ne!(p.graph.evaluate(32).unwrap(), final_height);
    }
}
