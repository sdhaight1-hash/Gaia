//! Deterministic, CPU-side heightfield graph. No GPU or UI dependencies.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    Noise { seed: u32, frequency: f32, amplitude: f32 },
    Erosion { iterations: u32, talus: f32, strength: f32 },
    Terrace { steps: u32, softness: f32 },
    Blur { radius: u32 },
    Blend { mix: f32 },
}

impl NodeKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Mountain { .. } => "Mountain",
            Self::Noise { .. } => "Noise",
            Self::Erosion { .. } => "Thermal erosion",
            Self::Terrace { .. } => "Terrace",
            Self::Blur { .. } => "Blur",
            Self::Blend { .. } => "Blend",
        }
    }

    pub fn input_count(&self) -> usize {
        match self {
            Self::Mountain { .. } | Self::Noise { .. } => 0,
            Self::Blend { .. } => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    pub kind: NodeKind,
    pub inputs: Vec<u64>,
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
                        inputs: vec![1],
                        position: [265.0, 90.0],
                    },
                    Node {
                        id: 3,
                        kind: NodeKind::Terrace { steps: 12, softness: 0.75 },
                        inputs: vec![2],
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

    pub fn add(&mut self, kind: NodeKind) -> u64 {
        let id = self.nodes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
        let inputs = match kind.input_count() {
            0 => vec![],
            1 => vec![self.output],
            _ => vec![self.output, 1],
        };
        let position = [65.0 + (self.nodes.len() % 4) as f32 * 200.0,
                        205.0 + (self.nodes.len() / 4) as f32 * 115.0];
        self.nodes.push(Node { id, kind, inputs, position });
        self.output = id;
        id
    }

    pub fn evaluate(&self, resolution: usize) -> Result<HeightField, String> {
        if !(32..=512).contains(&resolution) {
            return Err("Resolution must be between 32 and 512".to_string());
        }
        if self.nodes.len() > 256 {
            return Err("Project exceeds 256 nodes".to_string());
        }
        let mut cache = HashMap::new();
        let mut visiting = HashSet::new();
        self.eval_node(self.output, resolution, &mut cache, &mut visiting)
    }

    fn eval_node(
        &self,
        id: u64,
        size: usize,
        cache: &mut HashMap<u64, HeightField>,
        visiting: &mut HashSet<u64>,
    ) -> Result<HeightField, String> {
        if let Some(field) = cache.get(&id) {
            return Ok(field.clone());
        }
        if !visiting.insert(id) {
            return Err(format!("Graph cycle detected at node {id}"));
        }
        let result = (|| {
            let node = self.get(id).ok_or_else(|| format!("Missing node {id}"))?;
            if node.inputs.len() != node.kind.input_count() {
                return Err(format!("{} requires {} inputs", node.kind.label(), node.kind.input_count()));
            }
            let sources = node.inputs.iter().map(|src| {
                self.eval_node(*src, size, cache, visiting)
            }).collect::<Result<Vec<_>, _>>()?;
            let output = match &node.kind {
                NodeKind::Mountain { seed, frequency, height } => {
                    generate_mountain(size, *seed, *frequency, *height)
                }
                NodeKind::Noise { seed, frequency, amplitude } => {
                    generate_noise(size, *seed, *frequency, *amplitude)
                }
                NodeKind::Erosion { iterations, talus, strength } => {
                    thermal_erosion(&sources[0], (*iterations).min(200), *talus, *strength)
                }
                NodeKind::Terrace { steps, softness } => {
                    terrace(&sources[0], *steps, *softness)
                }
                NodeKind::Blur { radius } => blur(&sources[0], *radius),
                NodeKind::Blend { mix } => {
                    let mut h = sources[0].clone();
                    for (dest, src) in h.values.iter_mut().zip(&sources[1].values) {
                        *dest = *dest * (1.0 - mix.clamp(0.0, 1.0)) + src * mix.clamp(0.0, 1.0);
                    }
                    h
                }
            };
            if output.values.iter().any(|v| !v.is_finite()) {
                return Err(format!("Node {id} produced non-finite terrain"));
            }
            Ok(output)
        })();
        visiting.remove(&id);
        let output = result?;
        cache.insert(id, output.clone());
        Ok(output)
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

fn fractal(x: f32, y: f32, seed: u32, frequency: f32) -> f32 {
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

fn generate_noise(size: usize, seed: u32, frequency: f32, amplitude: f32) -> HeightField {
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

fn generate_mountain(size: usize, seed: u32, frequency: f32, height: f32) -> HeightField {
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
pub fn thermal_erosion(input: &HeightField, iterations: u32, talus: f32, strength: f32) -> HeightField {
    let mut h = input.clone();
    let size = h.size;
    let talus = talus.clamp(0.0, 1.0);
    let strength = strength.clamp(0.0, 0.5);
    for _ in 0..iterations {
        let mut delta = vec![0.0_f32; h.values.len()];
        for y in 0..size {
            for x in 0..size {
                let index = y * size + x;
                let current = h.values[index];
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
                        let ni = ny * size + nx;
                        let excess = (current - h.values[ni] - talus).max(0.0);
                        flows[i] = (ni, excess);
                        total_excess += excess;
                    }
                }
                if total_excess > 0.0 {
                    let amount = (strength * total_excess).min(current.max(0.0) * 0.45);
                    delta[index] -= amount;
                    for (ni, excess) in flows {
                        if excess > 0.0 {
                            delta[ni] += amount * (excess / total_excess);
                        }
                    }
                }
            }
        }
        for (value, change) in h.values.iter_mut().zip(delta) {
            *value += change;
        }
    }
    h
}

fn terrace(input: &HeightField, steps: u32, softness: f32) -> HeightField {
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

fn blur(input: &HeightField, radius: u32) -> HeightField {
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
        p.graph.nodes[1].inputs = vec![999];
        assert!(p.graph.evaluate(64).unwrap_err().contains("Missing node"));
    }

    #[test]
    fn graph_cycle_is_rejected() {
        let mut p = Project::default();
        p.graph.nodes[1].inputs = vec![3];
        assert!(p.graph.evaluate(64).unwrap_err().contains("cycle"));
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
