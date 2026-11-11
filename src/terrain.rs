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
    Island { seed: u32, radius: f32, height: f32, roughness: f32 },
    Plates { seed: u32, count: u32, continental: f32, uplift: f32, warp: f32 },
    Volcano { seed: u32, radius: f32, height: f32, mouth: f32, bulk: f32,
        roughness: f32, center_x: f32, center_y: f32 },
    ImportHeight { path: String, scale: f32 },
    Colorize { low: [f32; 3], high: [f32; 3], midpoint: f32, contrast: f32 },
    ColorMixer { mix: f32 },
    Drainage { min_catchment_fraction: f32 },
    DepressionFill { grade: f32 },
    Rivers { catchment_fraction: f32, width: f32, depth: f32, grade: f32 },
    Lake { fill_fraction: f32, minimum_area: u32, minimum_depth: f32 },
    Sea { level: f32, shore_width: u32, erosion: f32 },
    AmbientOcclusion { radius: u32, strength: f32 },
    Sunlight { azimuth: f32, elevation: f32, distance: u32 },
    Snow { seed: u32, snowline: f32, intensity: f32, melt: f32, adherence: f32, settle: u32 },
    MaskLevels { black: f32, white: f32, gamma: f32, invert: bool },
    Range { seed: u32, height: f32, roughness: f32 },
    Crater { seed: u32, radius: f32, depth: f32, rim: f32 },
    Dunes { seed: u32, frequency: f32, amplitude: f32 },
    Canyon { seed: u32, width: f32, depth: f32 },
    Noise { seed: u32, frequency: f32, amplitude: f32 },
    Erosion { iterations: u32, talus: f32, strength: f32 },
    Hydraulic { params: HydraulicParameters }, 
    Terrace { steps: u32, softness: f32 },
    Stratify { seed: u32, spacing: f32, intensity: f32, warp: f32, tilt: f32, direction: f32 },
    Adjust { multiply: f32, shaper: f32, clamp_min: f32, clamp_max: f32,
        autolevel: bool, invert: bool },
    Equalize { factor: f32 },
    Normals { strength: f32, flip_green: bool },
    Blur { radius: u32 },
    Blend { mix: f32 },
    Slope,
    Aspect { azimuth: f32, half_width: f32, min_slope: f32 },
    Curvature,
    ElevationMask { low: f32, high: f32, feather: f32 },
    Warp { seed: u32, frequency: f32, amount: f32 },
    MaskedBlend { mix: f32 },
}

impl NodeKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Mountain { .. } => "Mountain",
            Self::Island { .. } => "Island",
            Self::Volcano { .. } => "Volcano",
            Self::Plates { .. } => "Plates",
            Self::ImportHeight { .. } => "Import heightmap",
            Self::Colorize { .. } => "Colorize",
            Self::ColorMixer { .. } => "Color mixer",
            Self::Drainage { .. } => "Drainage",
            Self::DepressionFill { .. } => "Depression fill",
            Self::Rivers { .. } => "Rivers",
            Self::Lake { .. } => "Lake",
            Self::Sea { .. } => "Sea",
            Self::AmbientOcclusion { .. } => "Ambient Occlusion",
            Self::Sunlight { .. } => "Sunlight",
            Self::Snow { .. } => "Snow",
            Self::MaskLevels { .. } => "Mask levels",
            Self::Range { .. } => "Mountain range",
            Self::Crater { .. } => "Crater",
            Self::Dunes { .. } => "Dunes",
            Self::Canyon { .. } => "Canyon",
            Self::Noise { .. } => "Noise",
            Self::Erosion { .. } => "Thermal erosion",
            Self::Hydraulic { .. } => "Hydraulic erosion",
            Self::Terrace { .. } => "Terrace",
            Self::Stratify { .. } => "Stratify",
            Self::Adjust { .. } => "Adjust",
            Self::Equalize { .. } => "Equalize",
            Self::Normals { .. } => "Normals",
            Self::Blur { .. } => "Blur",
            Self::Blend { .. } => "Blend",
            Self::Slope => "Slope mask",
            Self::Aspect { .. } => "Aspect mask",
            Self::Curvature => "Curvature mask",
            Self::ElevationMask { .. } => "Elevation mask",
            Self::Warp { .. } => "Warp",
            Self::MaskedBlend { .. } => "Masked Blend",
        }
    }

    pub fn input_count(&self) -> usize {
        match self {
            Self::Mountain { .. } | Self::Island { .. } | Self::Volcano { .. } | Self::Plates { .. } |
                Self::Noise { .. } | Self::ImportHeight { .. } |
                Self::Range { .. } | Self::Crater { .. } |
                Self::Dunes { .. } | Self::Canyon { .. } => 0,
            Self::Blend { .. } => 2,
            Self::MaskedBlend { .. } | Self::ColorMixer { .. } => 3,
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

    /// Remove only an unused node. Reject connections and the last height
    /// source rather than leaving dangling port references in the graph.
    pub fn remove(&mut self, id: u64) -> Result<(), String> {
        let index = self.nodes.iter().position(|node| node.id == id)
            .ok_or_else(|| format!("Missing node #{id}"))?;
        if let Some(dependent) = self.nodes.iter()
            .find(|node| node.id != id && node.inputs.iter().any(|input| input.node == id))
        {
            return Err(format!(
                "Cannot delete node #{id}: it is used by node #{}",
                dependent.id
            ));
        }
        let replacement = if self.output == id {
            Some(self.nodes.iter().rev()
                .find(|node| node.id != id && node.kind.output_ports().iter()
                    .any(|(port, ty)| *port == "height"
                        && *ty == crate::pipeline::PortType::Height))
                .map(|node| node.id)
                .ok_or_else(|| "Cannot delete the last height output node".to_owned())?)
        } else {
            None
        };
        self.nodes.remove(index);
        if let Some(output) = replacement {
            self.output = output;
        }
        Ok(())
    }

    pub fn add(&mut self, kind: NodeKind) -> u64 {
        // An empty project graph can lack both masks and color fields. Invalid
        // placeholder ports like #output:wear poisoned evaluation immediately
        // after inserting a MaskLevels, MaskedBlend or ColorMixer.
        //
        // Create small, visible, correctly typed prerequisite nodes instead.
        let needs_mask = matches!(kind,
            NodeKind::MaskLevels { .. } | NodeKind::MaskedBlend { .. }
                | NodeKind::ColorMixer { .. });
        if needs_mask && !self.nodes.iter().any(|node|
            node.kind.output_ports().iter().any(|(_, ty)|
                *ty == crate::pipeline::PortType::Mask))
        {
            self.add(NodeKind::Slope);
        }
        if matches!(kind, NodeKind::ColorMixer { .. }) && !self.nodes.iter().any(|node|
            node.kind.output_ports().iter().any(|(_, ty)|
                *ty == crate::pipeline::PortType::Color))
        {
            self.add(NodeKind::Colorize {
                low: [0.13, 0.27, 0.19], high: [0.85, 0.85, 0.78],
                midpoint: 0.5, contrast: 1.0,
            });
            self.add(NodeKind::Colorize {
                low: [0.28, 0.27, 0.26], high: [0.88, 0.84, 0.78],
                midpoint: 0.5, contrast: 1.0,
            });
        }
        let id = self.nodes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
        let inputs = if matches!(kind, NodeKind::MaskLevels { .. }) {
            let mask = self.nodes.iter().rev().find_map(|node| {
                node.kind.output_ports().iter()
                    .find(|(_, ty)| *ty == crate::pipeline::PortType::Mask)
                    .map(|(name, _)| PortRef::new(node.id, *name))
            }).unwrap_or_else(|| PortRef::new(self.output, "wear"));
            vec![mask]
        } else if matches!(kind, NodeKind::ColorMixer { .. }) {
            let color_sources: Vec<PortRef> = self.nodes.iter().rev()
                .filter_map(|node| node.kind.output_ports().iter()
                    .find(|(_, ty)| *ty == crate::pipeline::PortType::Color)
                    .map(|(port, _)| PortRef::new(node.id, *port)))
                .collect();
            let first = color_sources.first().cloned()
                .unwrap_or_else(|| PortRef::new(self.output, "color"));
            let second = color_sources.get(1).cloned().unwrap_or_else(|| first.clone());
            let mask = self.nodes.iter().rev().find_map(|node| {
                node.kind.output_ports().iter()
                    .find(|(_, ty)| *ty == crate::pipeline::PortType::Mask)
                    .map(|(name, _)| PortRef::new(node.id, *name))
            }).unwrap_or_else(|| PortRef::new(self.output, "wear"));
            vec![first, second, mask]
        } else {
            match kind.input_count() {
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
            }
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

    /// Add a node after the user's selected graph port when the types match.
    /// Incompatible selections use the existing safe default wiring, so
    /// choosing a Mask while creating a height modifier never corrupts the graph.
    pub fn add_from(&mut self, kind: NodeKind, selected: &PortRef) -> u64 {
        let compatible = self.get(selected.node).is_some_and(|node| {
            node.kind.output_ports().iter()
                .any(|(name, ty)| *name == selected.port
                    && Some(*ty) == kind.input_type(0))
        });
        let source_position = if compatible {
            self.get(selected.node).map(|node| node.position)
        } else {
            None
        };

        let id = self.add(kind);
        if compatible {
            // The selected source existed before insertion: this cannot cycle.
            self.connect(id, 0, selected.clone())
                .expect("matching preexisting source must be connectable");
        }
        if let Some([x, y]) = source_position {
            let mut target = [x + 235.0, y];
            // Avoid placing new cards directly on top of prior cards.
            while self.nodes.iter().any(|node| node.id != id &&
                (node.position[0] - target[0]).abs() < 205.0 &&
                (node.position[1] - target[1]).abs() < 104.0
            ) {
                target[1] += 112.0;
            }
            if let Some(node) = self.nodes.iter_mut().find(|node| node.id == id) {
                node.position = target;
            }
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

/// Build a path relative to the project directory, including sibling assets.
/// Different Windows drives cannot form a relative path; retain the absolute path.
fn relative_asset_path(base: &Path, asset: &Path) -> Option<std::path::PathBuf> {
    use std::path::Component;
    let from: Vec<_> = base.components().collect();
    let to: Vec<_> = asset.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if common == 0
        || from[common..].iter().any(|c| !matches!(c, Component::Normal(_)))
        || to[common..].iter().any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let mut relative = std::path::PathBuf::new();
    for _ in &from[common..] {
        relative.push("..");
    }
    for part in &to[common..] {
        relative.push(part.as_os_str());
    }
    Some(relative)
}

impl Project {
    pub fn save(&self, path: &Path) -> Result<(), String> {
        // Keep runtime paths intact. The on-disk JSON uses paths relative to
        // the project file so moving a project folder with its assets works.
        let directory = path.parent().filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .canonicalize().map_err(|e| format!("Project directory: {e}"))?;
        let working_directory = std::env::current_dir().map_err(|e| e.to_string())?;
        let mut stored = self.clone();
        for node in &mut stored.graph.nodes {
            if let NodeKind::ImportHeight { path: asset, .. } = &mut node.kind {
                let supplied = Path::new(asset);
                let absolute = if supplied.is_absolute() {
                    supplied.to_path_buf()
                } else {
                    working_directory.join(supplied)
                };
                let absolute = absolute.canonicalize().unwrap_or(absolute);
                let on_disk = relative_asset_path(&directory, &absolute)
                    .unwrap_or(absolute);
                *asset = on_disk.to_string_lossy().into_owned();
            }
        }
        let data = serde_json::to_string_pretty(&stored).map_err(|e| e.to_string())?;
        std::fs::write(path, data).map_err(|e| e.to_string())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut project: Self = serde_json::from_str(&data).map_err(|e| e.to_string())?;
        let directory = path.parent().filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .canonicalize().map_err(|e| format!("Project directory: {e}"))?;
        for node in &mut project.graph.nodes {
            if let NodeKind::ImportHeight { path: asset, .. } = &mut node.kind {
                let supplied = Path::new(asset);
                let absolute = if supplied.is_absolute() {
                    supplied.to_path_buf()
                } else {
                    directory.join(supplied)
                };
                *asset = absolute.canonicalize().unwrap_or(absolute)
                    .to_string_lossy().into_owned();
            }
        }
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
    let n = field.size;
    if n < 2 || field.values.len() != n * n ||
        field.values.iter().any(|v| !v.is_finite())
    {
        return Err("OBJ export requires a complete finite heightfield of at least 2x2".into());
    }
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    writeln!(out, "# Gaia terrain mesh with UVs and per-vertex normals")
        .map_err(|e| e.to_string())?;
    let step = 2.0 / (n - 1) as f32;
    for y in 0..n {
        for x in 0..n {
            let fx = x as f32 * step - 1.0;
            let fz = y as f32 * step - 1.0;
            writeln!(out, "v {fx:.6} {:.6} {fz:.6}", field.get(x, y))
                .map_err(|e| e.to_string())?;
        }
    }
    // Align the texture with image-space PNG output (origin top-left).
    for y in 0..n {
        for x in 0..n {
            let u = x as f32 / (n - 1) as f32;
            let v = 1.0 - y as f32 / (n - 1) as f32;
            writeln!(out, "vt {u:.6} {v:.6}").map_err(|e| e.to_string())?;
        }
    }
    for y in 0..n {
        for x in 0..n {
            let left = field.get(x.saturating_sub(1), y);
            let right = field.get((x + 1).min(n - 1), y);
            let up = field.get(x, y.saturating_sub(1));
            let down = field.get(x, (y + 1).min(n - 1));
            // Same Y-up vertex normal convention as Gaia's wgpu preview.
            let nx = left - right;
            let ny = step * 2.0;
            let nz = up - down;
            let inv_length = (nx * nx + ny * ny + nz * nz).sqrt().recip();
            writeln!(out, "vn {:.6} {:.6} {:.6}",
                nx * inv_length, ny * inv_length, nz * inv_length)
                .map_err(|e| e.to_string())?;
        }
    }
    for y in 0..(n - 1) {
        for x in 0..(n - 1) {
            let a = y * n + x + 1;
            let b = a + 1;
            let c = a + n;
            let d = c + 1;
            writeln!(out,
                "f {a}/{a}/{a} {c}/{c}/{c} {b}/{b}/{b}\nf {b}/{b}/{b} {c}/{c}/{c} {d}/{d}/{d}")
                .map_err(|e| e.to_string())?;
        }
    }
    out.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obj_export_includes_uvs_normals_and_indexed_faces() {
        let n = 32;
        let field = HeightField::flat(n, 0.45);
        let file = std::env::temp_dir().join(format!(
            "gaia-obj-uv-{}-{}.obj", std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        export_obj(&field, &file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.iter().filter(|l| l.starts_with("v ")).count(), n*n);
        assert_eq!(lines.iter().filter(|l| l.starts_with("vt ")).count(), n*n);
        assert_eq!(lines.iter().filter(|l| l.starts_with("vn ")).count(), n*n);
        assert_eq!(lines.iter().filter(|l| l.starts_with("f ")).count(), (n-1)*(n-1)*2);
        assert!(lines.iter().any(|l| *l == "vt 0.000000 1.000000"));
        assert!(lines.iter().any(|l| *l == "vn 0.000000 1.000000 0.000000"));
        assert!(lines.iter().any(|l| *l == "f 1/1/1 33/33/33 2/2/2"));
        std::fs::remove_file(&file).unwrap();
        let mut invalid = field;
        invalid.values[0] = f32::NAN;
        assert!(export_obj(&invalid, &file).is_err());
    }

    #[test]
    fn context_creation_connects_selected_height_not_global_output() {
        let mut project = Project::default();
        let earlier = PortRef::height(1);
        let id = project.graph.add_from(NodeKind::Blur { radius: 2 }, &earlier);
        assert_eq!(project.graph.get(id).unwrap().inputs[0], earlier);
        assert_eq!(project.graph.output, id);
        assert!(project.graph.evaluate(32).is_ok());

        let pos = project.graph.get(id).unwrap().position;
        assert!(pos[0] > project.graph.get(1).unwrap().position[0]);
        assert!(project.graph.nodes.iter().filter(|n| n.id != id).all(|other|
            (other.position[0] - pos[0]).abs() >= 205.0 ||
            (other.position[1] - pos[1]).abs() >= 104.0
        ));
    }

    #[test]
    fn context_creation_preserves_typed_mask_connections_and_fallbacks() {
        let mut project = Project::default();
        let slope = project.graph.add(NodeKind::Slope);
        let slope_port = PortRef::new(slope, "slope");
        let mask = project.graph.add_from(NodeKind::MaskLevels {
            black: 0.0, white: 1.0, gamma: 1.0, invert: false
        }, &slope_port);
        assert_eq!(project.graph.get(mask).unwrap().inputs[0], slope_port);
        assert!(project.graph.evaluate_port(32, &PortRef::new(mask, "mask")).is_ok());

        let before_height = project.graph.output;
        let height = project.graph.add_from(NodeKind::Terrace {
            steps: 8, softness: 0.5
        }, &slope_port);
        assert_eq!(project.graph.get(height).unwrap().inputs[0],
            PortRef::height(before_height));
        assert!(project.graph.evaluate(32).is_ok());

        let color = project.graph.add(NodeKind::Colorize {
            low: [0.1, 0.2, 0.3], high: [0.8, 0.9, 1.0],
            midpoint: 0.5, contrast: 1.0
        });
        let color_port = PortRef::new(color, "color");
        let mixer = project.graph.add_from(NodeKind::ColorMixer { mix: 0.3 }, &color_port);
        assert_eq!(project.graph.get(mixer).unwrap().inputs[0], color_port);
    }

    #[test]
    fn default_mask_and_color_compositing_nodes_have_valid_autowired_inputs() {
        let mut project = Project::default();
        let levels = project.graph.add(NodeKind::MaskLevels {
            black: 0.0, white: 1.0, gamma: 1.0, invert: false,
        });
        let input = &project.graph.get(levels).unwrap().inputs[0];
        assert!(project.graph.get(input.node).is_some_and(|node|
            node.kind.output_ports().iter().any(|(name, ty)|
                *name == input.port && *ty == crate::pipeline::PortType::Mask)));
        assert!(project.graph.evaluate_port(32, &PortRef::new(levels, "mask")).is_ok());

        let mixer = project.graph.add(NodeKind::ColorMixer { mix: 0.5 });
        let inputs = &project.graph.get(mixer).unwrap().inputs;
        assert_ne!(inputs[0], inputs[1], "two default palettes should be distinct");
        assert!(project.graph.evaluate_port(32,
            &PortRef::new(mixer, "color")).is_ok());
        assert!(project.graph.evaluate(32).is_ok());

        let blend = project.graph.add(NodeKind::MaskedBlend { mix: 0.7 });
        assert!(project.graph.evaluate_port(32,
            &PortRef::height(blend)).is_ok());
    }

    #[test]
    fn adding_color_mixer_to_fresh_project_builds_all_missing_sources() {
        let mut project = Project::default();
        let mixer = project.graph.add(NodeKind::ColorMixer { mix: 0.65 });
        let inputs = &project.graph.get(mixer).unwrap().inputs;
        assert_eq!(inputs.len(), 3);
        let sources: Vec<_> = inputs.iter().map(|reference|
            project.graph.get(reference.node).unwrap().kind
                .output_ports().iter()
                .find(|(name, _)| *name == reference.port).unwrap().1
        ).collect();
        assert_eq!(sources.as_slice(), &[
            crate::pipeline::PortType::Color,
            crate::pipeline::PortType::Color,
            crate::pipeline::PortType::Mask,
        ]);
        assert_ne!(inputs[0], inputs[1]);
        assert!(project.graph.evaluate_port(32,
            &PortRef::new(mixer, "color")).is_ok());
    }

    #[test]
    fn masked_blend_reuses_existing_wear_or_generates_missing_slope() {
        // A default scene has Thermal Erosion's valid wear mask.
        let mut existing = Project::default();
        let blend = existing.graph.add(NodeKind::MaskedBlend { mix: 0.8 });
        assert_eq!(existing.graph.get(blend).unwrap().inputs[2],
            PortRef::new(2, "wear"));
        assert!(existing.graph.evaluate(32).is_ok());

        // A graph containing only a height source requires a new mask.
        let mut project = Project::default();
        project.graph.nodes.retain(|node| node.id == 1);
        project.graph.output = 1;
        let blend = project.graph.add(NodeKind::MaskedBlend { mix: 0.8 });
        let input = &project.graph.get(blend).unwrap().inputs[2];
        assert_eq!(input.port, "slope");
        assert!(project.graph.evaluate(32).is_ok());
    }

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
    fn deleting_unused_output_selects_previous_height_source() {
        let mut project = Project::default();
        let id = project.graph.add(NodeKind::Noise {
            seed: 81, frequency: 4.0, amplitude: 0.5,
        });
        assert_eq!(project.graph.output, id);
        project.graph.remove(id).unwrap();
        assert_eq!(project.graph.output, 3);
        assert!(project.graph.get(id).is_none());
        assert!(project.graph.evaluate(32).is_ok());
    }

    #[test]
    fn deleting_connected_or_last_height_node_is_rejected_without_mutation() {
        let mut project = Project::default();
        let before = serde_json::to_string(&project.graph).unwrap();
        assert!(project.graph.remove(1).unwrap_err().contains("used by node #2"));
        assert_eq!(serde_json::to_string(&project.graph).unwrap(), before);
        assert!(project.graph.remove(999).unwrap_err().contains("Missing node"));
        assert_eq!(serde_json::to_string(&project.graph).unwrap(), before);

        let only_height = project.graph.nodes[0].clone();
        let mut single = Graph { nodes: vec![only_height], output: 1 };
        assert!(single.remove(1).unwrap_err().contains("last height"));
        assert_eq!(single.nodes.len(), 1);
        assert_eq!(single.output, 1);
    }

    #[test]
    fn project_serialization_roundtrip() {
        let p = Project::default();
        let json = serde_json::to_string(&p).unwrap();
        let restored: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.graph.evaluate(32).unwrap(), p.graph.evaluate(32).unwrap());
    }

    #[test]
    fn imported_heightmap_remains_valid_when_project_folder_moves() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!(
            "gaia-relative-assets-{}-{unique}", std::process::id()
        ));
        let original = root.join("original");
        let moved = root.join("moved");
        let assets = original.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let heightmap = assets.join("mountain.png");
        let field = HeightField {
            size: 32,
            values: (0..1024).map(|i| i as f32 / 1023.0).collect(),
        };
        export_png16(&field, &heightmap).unwrap();

        let mut project = Project::default();
        project.resolution = 32;
        let imported_id = project.graph.add(NodeKind::ImportHeight {
            path: heightmap.to_string_lossy().into_owned(),
            scale: 1.0,
        });
        let scene = original.join("terrain.gaia.json");
        project.save(&scene).unwrap();
        // The runtime graph is not changed by serialization.
        assert_eq!(project.graph.get(imported_id).unwrap().kind.label(), "Import heightmap");
        let serialized: Project = serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
        let NodeKind::ImportHeight { path: saved_path, .. } =
            &serialized.graph.get(imported_id).unwrap().kind else { panic!("wrong node"); };
        assert_eq!(Path::new(saved_path), Path::new("assets").join("mountain.png"));
        let expected = Project::load(&scene).unwrap().graph.evaluate(32).unwrap();

        std::fs::rename(&original, &moved).unwrap();
        let relocated = Project::load(&moved.join("terrain.gaia.json")).unwrap();
        assert_eq!(relocated.graph.evaluate(32).unwrap(), expected);
        let copy = moved.join("copy.gaia.json");
        relocated.save(&copy).unwrap();
        assert_eq!(Project::load(&copy).unwrap().graph.evaluate(32).unwrap(), expected);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn output_node_selection_changes_result() {
        let mut p = Project::default();
        let final_height = p.graph.evaluate(32).unwrap();
        p.graph.output = 1;
        assert_ne!(p.graph.evaluate(32).unwrap(), final_height);
    }
}
