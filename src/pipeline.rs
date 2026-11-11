//! Typed, multi-output terrain graph evaluation with incremental node caching.
//! Legacy JSON source node IDs are migrated transparently to named height ports.
//! This module intentionally has no UI or GPU dependencies.
use crate::hydrology::simulate;
use crate::adjust::adjust_height;
use crate::normals::derive_normals;
use crate::drainage::drainage;
use crate::depression::fill_depressions;
use crate::rivers::carve_rivers;
use crate::lakes::fill_lakes;
use crate::color::{ColorField, altitude_palette, color_blend};
use crate::import::read_heightmap;
use std::path::Path;
use crate::geology::{island, volcano, mountain_range, crater, dunes, canyon, warp, curvature, aspect_mask, elevation_mask, stratify};
use crate::terrain::{
    blur, generate_mountain, generate_noise, terrace, thermal_erosion_fields_checked, Graph, HeightField,
    NodeKind,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    Height,
    Mask,
    Color,
    Vector,
    Water,
    Points,
    Mesh,
}

impl PortType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Height => "Height",
            Self::Mask => "Mask",
            Self::Color => "Color",
            Self::Vector => "Vector",
            Self::Water => "Water",
            Self::Points => "Points",
            Self::Mesh => "Mesh",
        }
    }
}

fn default_height_port() -> String {
    "height".to_string()
}

/// A connection to one named output of a source node.
/// Serializes as an object; deserializes old numeric node IDs too.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct PortRef {
    pub node: u64,
    pub port: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PortRefWire {
    Legacy(u64),
    Named {
        node: u64,
        #[serde(default = "default_height_port")]
        port: String,
    },
}

impl<'de> Deserialize<'de> for PortRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match PortRefWire::deserialize(deserializer)? {
            PortRefWire::Legacy(node) => Ok(Self::height(node)),
            PortRefWire::Named { node, port } => Ok(Self { node, port }),
        }
    }
}

impl PortRef {
    pub fn new(node: u64, port: impl Into<String>) -> Self {
        Self { node, port: port.into() }
    }

    pub fn height(node: u64) -> Self {
        Self::new(node, "height")
    }
}

/// Typed terrain graph output. Scalar data is a luminance proxy only for
/// Color ports; consumers must use the actual RGBA field for color export.
#[derive(Debug, Clone)]
pub struct FieldValue {
    pub kind: PortType,
    pub data: Arc<HeightField>,
    pub color: Option<Arc<ColorField>>,
}

#[derive(Clone, Default)]
pub struct NodeOutputs {
    pub fields: BTreeMap<String, FieldValue>,
}

impl NodeOutputs {
    fn with(mut self, name: &str, kind: PortType, data: HeightField) -> Self {
        self.fields.insert(name.to_string(), FieldValue {
            kind, data: Arc::new(data), color: None,
        });
        self
    }

    fn with_color(mut self, name: &str, data: ColorField) -> Self {
        let preview = data.luminance();
        self.fields.insert(name.to_string(), FieldValue {
            kind: PortType::Color,
            data: Arc::new(preview),
            color: Some(Arc::new(data)),
        });
        self
    }

    fn field(&self, name: &str) -> Result<&FieldValue, String> {
        self.fields.get(name).ok_or_else(|| format!("Missing output port '{name}'"))
    }
}

#[derive(Clone)]
struct Entry {
    key: u64,
    value: Arc<NodeOutputs>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvaluationStats {
    pub computed: usize,
    pub reused: usize,
}

/// Cache is intentionally independent of the serializable project.
/// Keeping output fields in Arcs lets sibling connections share actual data.
/// Alternate compute implementation for graph blend nodes.
pub trait BlendCompute: Send + Sync {
    fn blend(
        &self, a: &HeightField, b: &HeightField,
        mask: Option<&HeightField>, mix: f32,
    ) -> Result<HeightField, String>;
}

#[derive(Default)]
pub struct GraphCache {
    entries: HashMap<u64, Entry>,
    pub stats: EvaluationStats,
    backend: Option<Arc<dyn BlendCompute>>,
}

/// Maximum full-grid build resolution. Interactive previews remain limited
/// to 512 pixels by the desktop UI; 2K fields require headless export.
pub const MAX_BUILD_RESOLUTION: usize = 2048;

impl GraphCache {
    pub fn set_backend(&mut self, backend: Option<Arc<dyn BlendCompute>>) {
        self.backend = backend;
        self.clear();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.stats = EvaluationStats::default();
    }

    pub fn cached_nodes(&self) -> usize {
        self.entries.len()
    }

    pub fn evaluate(
        &mut self, graph: &Graph, resolution: usize, source: &PortRef,
    ) -> Result<FieldValue, String> {
        self.evaluate_inner(graph, resolution, source, None)
    }

    /// Returns an error when a newer generation supersedes the current build.
    /// The signal is checked at graph nodes and throughout thermal iterations.
    pub fn evaluate_cancellable(
        &mut self, graph: &Graph, resolution: usize, source: &PortRef,
        generation: &AtomicU64, expected: u64,
    ) -> Result<FieldValue, String> {
        self.evaluate_inner(graph, resolution, source, Some((generation, expected)))
    }

    fn evaluate_inner(
        &mut self, graph: &Graph, resolution: usize, source: &PortRef,
        cancelled: Option<(&AtomicU64, u64)>,
    ) -> Result<FieldValue, String> {
        if !(32..=MAX_BUILD_RESOLUTION).contains(&resolution) {
            return Err(format!("Resolution must be between 32 and {MAX_BUILD_RESOLUTION}"));
        }
        if graph.nodes.len() > 256 {
            return Err("Project exceeds 256 nodes".to_owned());
        }
        self.stats = EvaluationStats::default();
        let mut visiting = HashSet::new();
        let (_, bundle) = self.visit(graph, resolution, source.node, &mut visiting, cancelled)?;
        Ok(bundle.field(&source.port)?.clone())
    }

    fn visit(
        &mut self,
        graph: &Graph,
        size: usize,
        id: u64,
        visiting: &mut HashSet<u64>,
        cancelled: Option<(&AtomicU64, u64)>,
    ) -> Result<(u64, Arc<NodeOutputs>), String> {
        if build_cancelled(cancelled) { return Err("Build cancelled".to_owned()); }
        if !visiting.insert(id) {
            return Err(format!("Graph cycle detected at node #{id}"));
        }
        let result = (|| {
            let node = graph.get(id).ok_or_else(|| format!("Missing node #{id}"))?;
            if node.inputs.len() != node.kind.input_count() {
                return Err(format!(
                    "{} requires {} inputs, received {}",
                    node.kind.label(), node.kind.input_count(), node.inputs.len()
                ));
            }
            let mut inputs = Vec::with_capacity(node.inputs.len());
            let mut source_keys = Vec::with_capacity(node.inputs.len());
            for (slot, source) in node.inputs.iter().enumerate() {
                let (key, bundle) = self.visit(graph, size, source.node, visiting, cancelled)?;
                let field = bundle.field(&source.port).map_err(|_| {
                    format!("Node #{} has no port '{}'", source.node, source.port)
                })?;
                let required = node.kind.input_type(slot);
                if Some(field.kind) != required {
                    return Err(format!(
                        "{} input {} expects {}, got {} from #{}:{}",
                        node.kind.label(), slot + 1,
                        required.map(|ty| ty.label()).unwrap_or("unknown"),
                        field.kind.label(), source.node, source.port
                    ));
                }
                source_keys.push(key);
                inputs.push(field.clone());
            }

            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            size.hash(&mut hasher);
            id.hash(&mut hasher);
            // NodeKind is serializable, stable within a running process; no
            // JSON is persisted as a cache key and no unstable layout is hashed.
            serde_json::to_string(&node.kind).map_err(|e| e.to_string())?.hash(&mut hasher);
            // Imported files are external dependencies: their content must
            // participate in cache identity, not only the path and parameters.
            if let NodeKind::ImportHeight { path, .. } = &node.kind {
                let bytes = std::fs::read(path)
                    .map_err(|e| format!("Cannot read imported heightmap {path}: {e}"))?;
                bytes.hash(&mut hasher);
            }
            for (source, key) in node.inputs.iter().zip(&source_keys) {
                source.hash(&mut hasher);
                key.hash(&mut hasher);
            }
            let fingerprint = hasher.finish();
            if let Some(entry) = self.entries.get(&id) {
                if entry.key == fingerprint {
                    self.stats.reused += 1;
                    return Ok((fingerprint, entry.value.clone()));
                }
            }

            let backend = if size >= 256 { self.backend.as_deref() } else { None };
            let output = evaluate_kind(
                &node.kind, size, &inputs, || build_cancelled(cancelled), backend,
            )?;
            for (name, field) in &output.fields {
                if field.data.size != size || field.data.values.iter().any(|v| !v.is_finite()) {
                    return Err(format!("Node #{id} produced invalid '{name}' data"));
                }
                if field.kind == PortType::Color && field.color.as_ref().is_none_or(|color|
                    color.size != size || color.pixels.len() != size * size
                        || color.pixels.iter().any(|rgba| rgba.iter().any(|v| !v.is_finite()))
                ) {
                    return Err(format!("Node #{id} produced invalid RGBA color data"));
                }
            }
            let output = Arc::new(output);
            self.entries.insert(id, Entry { key: fingerprint, value: output.clone() });
            self.stats.computed += 1;
            Ok((fingerprint, output))
        })();
        visiting.remove(&id);
        result
    }
}

fn build_cancelled(signal: Option<(&AtomicU64, u64)>) -> bool {
    signal.is_some_and(|(current, expected)| current.load(Ordering::Relaxed) != expected)
}

impl NodeKind {
    /// The subset of available output port types implemented by each node.
    pub fn output_ports(&self) -> &'static [(&'static str, PortType)] {
        const HEIGHT: &[(&str, PortType)] = &[("height", PortType::Height)];
        const SLOPE: &[(&str, PortType)] = &[("slope", PortType::Mask)];
        const COLOR: &[(&str, PortType)] = &[("color", PortType::Color)];
        const NORMALS: &[(&str, PortType)] = &[("normal", PortType::Color)];
        const CURVATURE: &[(&str, PortType)] = &[("curvature", PortType::Mask)];
        const ASPECT: &[(&str, PortType)] = &[("aspect", PortType::Mask)];
        const MASK_LEVELS: &[(&str, PortType)] = &[("mask", PortType::Mask)];
        const DRAINAGE: &[(&str, PortType)] = &[
            ("accumulation", PortType::Mask),
            ("channels", PortType::Mask),
        ];
        const DEPRESSION: &[(&str, PortType)] = &[
            ("height", PortType::Height),
            ("fill_depth", PortType::Water),
        ];
        const RIVERS: &[(&str, PortType)] = &[
            ("height", PortType::Height),
            ("channels", PortType::Mask),
            ("wear", PortType::Mask),
        ];
        const LAKE: &[(&str, PortType)] = &[
            ("height", PortType::Height),
            ("surface", PortType::Height),
            ("water", PortType::Water),
            ("coverage", PortType::Mask),
        ];
        const ELEVATION: &[(&str, PortType)] = &[("mask", PortType::Mask)];
        const EROSION: &[(&str, PortType)] = &[
            ("height", PortType::Height),
            ("wear", PortType::Mask),
            ("deposits", PortType::Mask),
        ];
        const HYDRAULIC: &[(&str, PortType)] = &[
            ("height", PortType::Height),
            ("wear", PortType::Mask),
            ("deposits", PortType::Mask),
            ("flow", PortType::Mask),
            ("water", PortType::Water),
            ("sediment", PortType::Mask),
        ];
        match self {
            Self::Erosion { .. } => EROSION,
            Self::Hydraulic { .. } => HYDRAULIC,
            Self::Slope => SLOPE,
            Self::Colorize { .. } | Self::ColorMixer { .. } => COLOR,
            Self::Normals { .. } => NORMALS,
            Self::Curvature => CURVATURE,
            Self::Aspect { .. } => ASPECT,
            Self::Drainage { .. } => DRAINAGE,
            Self::DepressionFill { .. } => DEPRESSION,
            Self::Rivers { .. } => RIVERS,
            Self::Lake { .. } => LAKE,
            Self::MaskLevels { .. } => MASK_LEVELS,
            Self::ElevationMask { .. } => ELEVATION,
            _ => HEIGHT,
        }
    }

    pub fn input_type(&self, slot: usize) -> Option<PortType> {
        match (self, slot) {
            (Self::Mountain { .. } | Self::Island { .. } | Self::Volcano { .. } |
             Self::Noise { .. } | Self::ImportHeight { .. } |
             Self::Range { .. } | Self::Crater { .. } |
             Self::Dunes { .. } | Self::Canyon { .. }, _) => None,
            (Self::MaskedBlend { .. }, 2) => Some(PortType::Mask),
            (Self::MaskLevels { .. }, 0) => Some(PortType::Mask),
            (Self::ColorMixer { .. }, 2) => Some(PortType::Mask),
            (Self::ColorMixer { .. }, 0 | 1) => Some(PortType::Color),
            (Self::Colorize { .. } | Self::Normals { .. }, 0) => Some(PortType::Height),
            (Self::MaskedBlend { .. }, 0 | 1) => Some(PortType::Height),
            (Self::Blend { .. }, 0 | 1) => Some(PortType::Height),
            (Self::Erosion { .. } | Self::Hydraulic { .. } | Self::Terrace { .. } | Self::Stratify { .. } | Self::Adjust { .. } | Self::Blur { .. } | Self::Slope | Self::Curvature | Self::Aspect { .. } | Self::ElevationMask { .. } | Self::Warp { .. } | Self::Drainage { .. } | Self::DepressionFill { .. } | Self::Rivers { .. } | Self::Lake { .. }, 0) => Some(PortType::Height),
            _ => None,
        }
    }
}

fn evaluate_kind(
    kind: &NodeKind, size: usize, inputs: &[FieldValue], cancelled: impl Fn() -> bool,
    backend: Option<&dyn BlendCompute>,
) -> Result<NodeOutputs, String> {
    let take = |n: usize| &*inputs[n].data;
    let computed_blend = |a: &HeightField, b: &HeightField,
        mask: Option<&HeightField>, mix: f32| -> Result<HeightField, String> {
        match backend {
            Some(compute) => compute.blend(a, b, mask, mix),
            None => Ok(blend(a, b, mask, mix)),
        }
    };
    let result = match kind {
        NodeKind::Mountain { seed, frequency, height } =>
            NodeOutputs::default().with("height", PortType::Height,
                generate_mountain(size, *seed, *frequency, *height)),
        NodeKind::Island { seed, radius, height, roughness } =>
            NodeOutputs::default().with("height", PortType::Height,
                island(size, *seed, *radius, *height, *roughness)),
        NodeKind::Volcano { seed, radius, height, mouth, bulk, roughness, center_x, center_y } =>
            NodeOutputs::default().with("height", PortType::Height,
                volcano(size, *seed, *radius, *height, *mouth, *bulk,
                    *roughness, *center_x, *center_y)),
        NodeKind::ImportHeight { path, scale } => NodeOutputs::default()
            .with("height", PortType::Height, read_heightmap(Path::new(path), size, *scale)?),
        NodeKind::Colorize { low, high, midpoint, contrast } =>
            NodeOutputs::default().with_color("color",
                altitude_palette(take(0), *low, *high, *midpoint, *contrast)),
        NodeKind::ColorMixer { mix } => {
            let source_a = inputs[0].color.as_ref()
                .ok_or("Color mixer requires first RGBA input")?;
            let source_b = inputs[1].color.as_ref()
                .ok_or("Color mixer requires second RGBA input")?;
            NodeOutputs::default().with_color("color",
                color_blend(source_a, source_b, take(2), *mix))
        },
        NodeKind::Noise { seed, frequency, amplitude } =>
            NodeOutputs::default().with("height", PortType::Height,
                generate_noise(size, *seed, *frequency, *amplitude)),
        NodeKind::Range { seed, height, roughness } => NodeOutputs::default()
            .with("height", PortType::Height, mountain_range(size, *seed, *height, *roughness)),
        NodeKind::Crater { seed, radius, depth, rim } => NodeOutputs::default()
            .with("height", PortType::Height, crater(size, *seed, *radius, *depth, *rim)),
        NodeKind::Dunes { seed, frequency, amplitude } => NodeOutputs::default()
            .with("height", PortType::Height, dunes(size, *seed, *frequency, *amplitude)),
        NodeKind::Canyon { seed, width, depth } => NodeOutputs::default()
            .with("height", PortType::Height, canyon(size, *seed, *width, *depth)),
        NodeKind::Erosion { iterations, talus, strength } => {
            let data = thermal_erosion_fields_checked(
                take(0), (*iterations).min(200), *talus, *strength, &cancelled
            ).ok_or_else(|| "Build cancelled".to_owned())?;
            NodeOutputs::default()
                .with("height", PortType::Height, data.height)
                .with("wear", PortType::Mask, data.wear)
                .with("deposits", PortType::Mask, data.deposits)
        }
        NodeKind::Hydraulic { params } => {
            let state = simulate(take(0), *params, &cancelled)?;
            NodeOutputs::default()
                .with("height", PortType::Height, state.height)
                .with("wear", PortType::Mask, state.wear)
                .with("deposits", PortType::Mask, state.deposits)
                .with("flow", PortType::Mask, state.flow)
                .with("water", PortType::Water, state.water)
                .with("sediment", PortType::Mask, state.sediment)
        }
        NodeKind::Terrace { steps, softness } => NodeOutputs::default()
            .with("height", PortType::Height, terrace(take(0), *steps, *softness)),
        NodeKind::Stratify { seed, spacing, intensity, warp, tilt, direction } =>
            NodeOutputs::default().with("height", PortType::Height,
                stratify(take(0), *seed, *spacing, *intensity, *warp, *tilt, *direction)),
        NodeKind::Adjust { multiply, shaper, clamp_min, clamp_max, autolevel, invert } =>
            NodeOutputs::default().with("height", PortType::Height,
                adjust_height(take(0), *multiply, *shaper, *clamp_min,
                    *clamp_max, *autolevel, *invert)?),
        NodeKind::Normals { strength, flip_green } =>
            NodeOutputs::default().with_color("normal",
                derive_normals(take(0), *strength, *flip_green)?),
        NodeKind::Blur { radius } => NodeOutputs::default()
            .with("height", PortType::Height, blur(take(0), *radius)),
        NodeKind::Blend { mix } => NodeOutputs::default()
            .with("height", PortType::Height, computed_blend(take(0), take(1), None, *mix)?),
        NodeKind::Slope => NodeOutputs::default()
            .with("slope", PortType::Mask, slope(take(0))),
        NodeKind::Curvature => NodeOutputs::default()
            .with("curvature", PortType::Mask, curvature(take(0))),
        NodeKind::Aspect { azimuth, half_width, min_slope } => NodeOutputs::default()
            .with("aspect", PortType::Mask,
                aspect_mask(take(0), *azimuth, *half_width, *min_slope)),
        NodeKind::Drainage { min_catchment_fraction } => {
            let result = drainage(take(0), *min_catchment_fraction)?;
            NodeOutputs::default()
                .with("accumulation", PortType::Mask, result.accumulation)
                .with("channels", PortType::Mask, result.channels)
        },
        NodeKind::DepressionFill { grade } => {
            let fields = fill_depressions(take(0), *grade, &cancelled)?;
            NodeOutputs::default()
                .with("height", PortType::Height, fields.height)
                .with("fill_depth", PortType::Water, fields.fill_depth)
        },
        NodeKind::Rivers { catchment_fraction, width, depth, grade } => {
            let fields = carve_rivers(take(0), *catchment_fraction,
                *width, *depth, *grade, &cancelled)?;
            NodeOutputs::default()
                .with("height", PortType::Height, fields.height)
                .with("channels", PortType::Mask, fields.channels)
                .with("wear", PortType::Mask, fields.wear)
        },
        NodeKind::Lake { fill_fraction, minimum_area, minimum_depth } => {
            let fields = fill_lakes(take(0), *fill_fraction, *minimum_area,
                *minimum_depth, &cancelled)?;
            NodeOutputs::default()
                .with("height", PortType::Height, fields.height)
                .with("surface", PortType::Height, fields.surface)
                .with("water", PortType::Water, fields.water)
                .with("coverage", PortType::Mask, fields.coverage)
        },
        NodeKind::MaskLevels { black, white, gamma, invert } =>
            NodeOutputs::default().with("mask", PortType::Mask,
                mask_levels(take(0), *black, *white, *gamma, *invert)?),
        NodeKind::ElevationMask { low, high, feather } => NodeOutputs::default()
            .with("mask", PortType::Mask,
                elevation_mask(take(0), *low, *high, *feather)),
        NodeKind::Warp { seed, frequency, amount } => NodeOutputs::default()
            .with("height", PortType::Height,
                warp(take(0), *seed, *frequency, *amount)),
        NodeKind::MaskedBlend { mix } => NodeOutputs::default()
            .with("height", PortType::Height, computed_blend(take(0), take(1), Some(take(2)), *mix)?),
    };
    Ok(result)
}

/// Adjust a typed mask without converting it to terrain height.
pub fn mask_levels(
    input: &HeightField, black: f32, white: f32, gamma: f32, invert: bool
) -> Result<HeightField, String> {
    if !black.is_finite() || !white.is_finite() || !gamma.is_finite()
        || black < 0.0 || white > 1.0 || black >= white
        || !(0.1..=4.0).contains(&gamma)
    {
        return Err("Mask levels require 0 <= black < white <= 1 and gamma 0.1–4".into());
    }
    let mut output = HeightField::flat(input.size, 0.0);
    for (dst, &value) in output.values.iter_mut().zip(&input.values) {
        if !value.is_finite() {
            return Err("Mask levels require finite mask input".into());
        }
        let t = ((value - black) / (white - black)).clamp(0.0, 1.0)
            .powf(gamma);
        *dst = if invert { 1.0 - t } else { t };
    }
    Ok(output)
}

pub fn slope(field: &HeightField) -> HeightField {
    let n = field.size;
    let mut out = HeightField::flat(n, 0.0);
    let scale = (n - 1) as f32 * 0.25;
    for y in 0..n {
        for x in 0..n {
            let dx = field.get((x + 1).min(n - 1), y) -
                field.get(x.saturating_sub(1), y);
            let dy = field.get(x, (y + 1).min(n - 1)) -
                field.get(x, y.saturating_sub(1));
            let slope = (dx * dx + dy * dy).sqrt() * scale;
            out.values[y * n + x] = (slope / (1.0 + slope)).clamp(0.0, 1.0);
        }
    }
    out
}

pub fn blend(a: &HeightField, b: &HeightField, mask: Option<&HeightField>, mix: f32) -> HeightField {
    let mut result = a.clone();
    let mix = mix.clamp(0.0, 1.0);
    for i in 0..result.values.len() {
        let influence = mix * mask.map_or(1.0, |m| m.values[i].clamp(0.0, 1.0));
        result.values[i] = a.values[i] * (1.0 - influence) + b.values[i] * influence;
    }
    result
}

impl Graph {
    pub fn evaluate_port(&self, resolution: usize, source: &PortRef) -> Result<FieldValue, String> {
        GraphCache::default().evaluate(self, resolution, source)
    }

    pub fn evaluate_cached(
        &self,
        resolution: usize,
        source: &PortRef,
        cache: &mut GraphCache,
    ) -> Result<FieldValue, String> {
        cache.evaluate(self, resolution, source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::{Node, Project};

    #[test]
    fn normals_from_height_are_rgba_and_roundtrip_as_color_port() {
        let mut p = Project::default();
        let id = p.graph.add_from(NodeKind::Normals {
            strength: 1.3, flip_green: true,
        }, &PortRef::height(1));
        assert_eq!(p.graph.get(id).unwrap().inputs[0], PortRef::height(1));
        let output = p.graph.evaluate_port(32, &PortRef::new(id, "normal")).unwrap();
        assert_eq!(output.kind, PortType::Color);
        assert_eq!(output.color.as_ref().unwrap().pixels.len(), 1024);
        assert!(output.color.as_ref().unwrap().pixels.iter()
            .all(|px| px.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))));
        let restored: Project = serde_json::from_str(
            &serde_json::to_string(&p).unwrap()
        ).unwrap();
        assert_eq!(output.color, restored.graph.evaluate_port(32,
            &PortRef::new(id, "normal")).unwrap().color);
        assert!(p.graph.connect(2, 0, PortRef::new(id, "normal")).is_err());
    }

    #[test]
    fn adjust_connects_to_selected_mountain_and_changes_elevation() {
        let mut scene = Project::default();
        let node = scene.graph.add_from(NodeKind::Adjust {
            multiply: 1.0, shaper: 0.65,
            clamp_min: 0.0, clamp_max: 4.0,
            autolevel: false, invert: false,
        }, &PortRef::height(1));
        assert_eq!(scene.graph.get(node).unwrap().inputs[0], PortRef::height(1));
        let source = scene.graph.evaluate_port(32, &PortRef::height(1)).unwrap();
        let output = scene.graph.evaluate_port(32, &PortRef::height(node)).unwrap();
        assert_eq!(output.kind, PortType::Height);
        assert_ne!(source.data, output.data);
        let restored: Project = serde_json::from_str(
            &serde_json::to_string(&scene).unwrap()).unwrap();
        assert_eq!(output.data, restored.graph.evaluate_port(32,
            &PortRef::height(node)).unwrap().data);
    }

    #[test]
    fn volcano_routes_into_stratify_and_hydrology_and_roundtrips() {
        let mut p = Project::default();
        let source = p.graph.add(NodeKind::Volcano {
            seed: 42, radius: 0.36, height: 0.8, mouth: 0.27,
            bulk: 0.5, roughness: 0.4, center_x: 0.5, center_y: 0.5,
        });
        let layer = p.graph.add_from(NodeKind::Stratify {
            seed: 17, spacing: 0.07, intensity: 0.6, warp: 0.4,
            tilt: 0.2, direction: 20.0,
        }, &PortRef::height(source));
        let output = p.graph.evaluate_port(32, &PortRef::height(layer)).unwrap();
        assert_eq!(output.kind, PortType::Height);
        let scene: Project = serde_json::from_str(
            &serde_json::to_string(&p).unwrap()
        ).unwrap();
        assert_eq!(output.data, scene.graph.evaluate_port(
            32, &PortRef::height(layer)
        ).unwrap().data);
        let drainage = p.graph.add_from(NodeKind::Drainage {
            min_catchment_fraction: 0.02,
        }, &PortRef::height(layer));
        assert!(p.graph.evaluate_port(32,
            &PortRef::new(drainage, "channels")).is_ok());
    }

    #[test]
    fn stratify_renders_in_graph_and_roundtrips_json() {
        let mut p = Project::default();
        let source = PortRef::height(1);
        let id = p.graph.add_from(NodeKind::Stratify {
            seed: 89, spacing: 0.075, intensity: 0.75,
            warp: 0.4, tilt: 0.35, direction: 30.0,
        }, &source);
        assert_eq!(p.graph.get(id).unwrap().inputs[0], source);
        let output = p.graph.evaluate_port(32, &PortRef::height(id)).unwrap();
        assert_eq!(output.kind, PortType::Height);
        assert!(output.data.values.iter().all(|v| v.is_finite() && *v >= 0.0));
        let json = serde_json::to_string(&p).unwrap();
        let again: Project = serde_json::from_str(&json).unwrap();
        let roundtrip = again.graph.evaluate_port(32, &PortRef::height(id)).unwrap();
        assert_eq!(output.data, roundtrip.data);
    }

    #[test]
    fn color_nodes_use_actual_rgba_and_are_type_safe() {
        let mut p = Project::default();
        let sea = p.graph.add(NodeKind::Colorize {
            low: [0.02, 0.15, 0.45], high: [0.9, 0.8, 0.6],
            midpoint: 0.45, contrast: 1.0,
        });
        let land = p.graph.add(NodeKind::Colorize {
            low: [0.1, 0.3, 0.1], high: [0.8, 0.85, 0.8],
            midpoint: 0.5, contrast: 1.0,
        });
        let aspect = p.graph.add(NodeKind::Aspect {
            azimuth: 90.0, half_width: 60.0, min_slope: 0.02,
        });
        let mixer = p.graph.add(NodeKind::ColorMixer { mix: 0.8 });
        p.graph.connect(mixer, 0, PortRef::new(sea, "color")).unwrap();
        p.graph.connect(mixer, 1, PortRef::new(land, "color")).unwrap();
        p.graph.connect(mixer, 2, PortRef::new(aspect, "aspect")).unwrap();
        let color = p.graph.evaluate_port(32, &PortRef::new(mixer, "color")).unwrap();
        assert_eq!(color.kind, PortType::Color);
        assert_eq!(color.color.as_ref().unwrap().pixels.len(), 1024);
        assert!(color.color.unwrap().pixels.iter()
            .all(|rgba| rgba.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))));
        assert!(p.graph.connect(2, 0, PortRef::new(mixer, "color")).is_err());
        assert!(p.graph.connect(mixer, 0, PortRef::height(1)).is_err());
        let json = serde_json::to_string(&p).unwrap();
        let restored: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(
            p.graph.evaluate_port(32, &PortRef::new(mixer, "color")).unwrap().color,
            restored.graph.evaluate_port(32, &PortRef::new(mixer, "color")).unwrap().color,
        );
    }

    #[test]
    fn mask_levels_transform_is_bounded_invertible_and_type_safe() {
        let source = HeightField { size: 32,
            values: (0..1024).map(|i| (i % 32) as f32 / 31.0).collect(),
        };
        assert_eq!(mask_levels(&source, 0.0, 1.0, 1.0, false).unwrap(), source);
        let inverted = mask_levels(&source, 0.0, 1.0, 1.0, true).unwrap();
        for (a, b) in source.values.iter().zip(&inverted.values) {
            assert!((a + b - 1.0).abs() < 1e-6);
        }
        assert!(mask_levels(&source, 0.7, 0.3, 1.0, false).is_err());
        assert!(mask_levels(&source, 0.0, 1.0, f32::NAN, false).is_err());

        let mut project = Project::default();
        let source_id = project.graph.add(NodeKind::Slope);
        let levels = project.graph.add(NodeKind::MaskLevels {
            black: 0.15, white: 0.85, gamma: 1.5, invert: false
        });
        project.graph.connect(levels, 0, PortRef::new(source_id, "slope")).unwrap();
        let output = project.graph.evaluate_port(32, &PortRef::new(levels, "mask")).unwrap();
        assert_eq!(output.kind, PortType::Mask);
        assert!(output.data.values.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(project.graph.connect(levels, 0, PortRef::height(1)).is_err());
    }

    #[test]
    fn island_generates_height_that_is_compatible_with_color_and_drainage() {
        let mut project = Project::default();
        let land = project.graph.add(NodeKind::Island {
            seed: 1337, radius: 0.65, height: 1.2, roughness: 0.6,
        });
        let height = project.graph.evaluate_port(32, &PortRef::height(land)).unwrap();
        assert_eq!(height.kind, PortType::Height);
        assert_eq!(height.data.get(0, 0), 0.0);
        let drain = project.graph.add(NodeKind::Drainage {
            min_catchment_fraction: 0.01,
        });
        project.graph.connect(drain, 0, PortRef::height(land)).unwrap();
        let surface = project.graph.add(NodeKind::Colorize {
            low: [0.1, 0.2, 0.3], high: [0.8, 0.8, 0.9],
            midpoint: 0.5, contrast: 1.0,
        });
        project.graph.connect(surface, 0, PortRef::height(land)).unwrap();
        assert!(project.graph.evaluate_port(32, &PortRef::new(drain, "channels")).is_ok());
        assert!(project.graph.evaluate_port(32, &PortRef::new(surface, "color")).is_ok());
    }

    #[test]
    fn drainage_outputs_are_typed_and_can_drive_masked_blends() {
        let mut project = Project::default();
        let drainage = project.graph.add(NodeKind::Drainage {
            min_catchment_fraction: 0.015,
        });
        let acc = project.graph.evaluate_port(32, &PortRef::new(drainage, "accumulation")).unwrap();
        assert_eq!(acc.kind, PortType::Mask);
        assert!(acc.data.values.iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        let channels = project.graph.evaluate_port(32, &PortRef::new(drainage, "channels")).unwrap();
        assert_eq!(channels.kind, PortType::Mask);
        let composite = project.graph.add(NodeKind::MaskedBlend { mix: 0.7 });
        project.graph.connect(composite, 2, PortRef::new(drainage, "channels")).unwrap();
        assert!(project.graph.evaluate_port(32, &PortRef::height(composite)).is_ok());
        assert!(project.graph.connect(composite, 0,
            PortRef::new(drainage, "accumulation")).is_err());
    }

    #[test]
    fn aspect_port_is_mask_and_connects_to_masked_blend() {
        let mut project = Project::default();
        let aspect_id = project.graph.add(NodeKind::Aspect {
            azimuth: 270.0, half_width: 50.0, min_slope: 0.05,
        });
        let blend_id = project.graph.add(NodeKind::MaskedBlend { mix: 0.6 });
        project.graph.connect(blend_id, 2, PortRef::new(aspect_id, "aspect")).unwrap();
        let selected = project.graph.evaluate_port(32, &PortRef::new(aspect_id, "aspect")).unwrap();
        assert_eq!(selected.kind, PortType::Mask);
        assert!(selected.data.values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        let output = project.graph.evaluate_port(32, &PortRef::height(blend_id)).unwrap();
        assert_eq!(output.kind, PortType::Height);
        assert!(output.data.values.iter().all(|v| v.is_finite()));
        assert!(project.graph.connect(blend_id, 0, PortRef::new(aspect_id, "aspect"))
            .unwrap_err().contains("Cannot connect"));
    }

    #[test]
    fn imported_asset_content_invalidates_graph_cache() {
        use crate::terrain::export_png16;
        let path = std::env::temp_dir().join(format!(
            "gaia-import-cache-{}.png", std::process::id()
        ));
        let p = Project::default();
        let mut graph = p.graph.clone();
        let id = graph.add(NodeKind::ImportHeight {
            path: path.display().to_string(),
            scale: 1.0,
        });
        let mut cache = GraphCache::default();
        let first = HeightField::flat(32, 0.1);
        export_png16(&first, &path).unwrap();
        let a = graph.evaluate_cached(32, &PortRef::height(id), &mut cache).unwrap();
        assert_eq!(cache.stats.computed, 1);
        let b = graph.evaluate_cached(32, &PortRef::height(id), &mut cache).unwrap();
        assert_eq!(cache.stats.computed, 0);
        assert_eq!(a.data, b.data);
        export_png16(&HeightField::flat(32, 0.7), &path).unwrap();
        let c = graph.evaluate_cached(32, &PortRef::height(id), &mut cache).unwrap();
        assert_eq!(cache.stats.computed, 1);
        assert_ne!(a.data.values, c.data.values);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn accepts_legacy_json_references() {
        let old: PortRef = serde_json::from_str("23").unwrap();
        assert_eq!(old, PortRef::height(23));
        let json = serde_json::to_string(&old).unwrap();
        assert_eq!(serde_json::from_str::<PortRef>(&json).unwrap(), old);
        let project = Project::default();
        let old_json = serde_json::to_string(&project).unwrap()
            .replace(r#"{"node":1,"port":"height"}"#, "1");
        let migrated: Project = serde_json::from_str(&old_json).unwrap();
        assert_eq!(project.graph.evaluate(64).unwrap(), migrated.graph.evaluate(64).unwrap());
    }

    #[test]
    fn erosion_exposes_real_wear_and_deposits() {
        let p = Project::default();
        let mut cache = GraphCache::default();
        let height = cache.evaluate(&p.graph, 64, &PortRef::height(2)).unwrap();
        let wear = cache.evaluate(&p.graph, 64, &PortRef::new(2, "wear")).unwrap();
        let deposits = cache.evaluate(&p.graph, 64, &PortRef::new(2, "deposits")).unwrap();
        assert_eq!(height.kind, PortType::Height);
        assert_eq!(wear.kind, PortType::Mask);
        assert_eq!(deposits.kind, PortType::Mask);
        assert!(wear.data.values.iter().all(|v| *v >= 0.0));
        assert!(deposits.data.values.iter().all(|v| *v >= 0.0));
        assert!(wear.data.mass() > 0.0);
        assert!((wear.data.mass() - deposits.data.mass()).abs() < 0.003);
    }

    #[test]
    fn masked_blend_routes_erosion_mask() {
        let mut p = Project::default();
        let id = p.graph.add(NodeKind::MaskedBlend { mix: 1.0 });
        let node = p.graph.nodes.iter_mut().find(|node| node.id == id).unwrap();
        node.inputs = vec![PortRef::height(1), PortRef::height(2),
            PortRef::new(2, "wear")];
        let mask = p.graph.evaluate_port(64, &PortRef::new(2, "wear")).unwrap();
        assert_eq!(mask.kind, PortType::Mask);
        let result = p.graph.evaluate(64).unwrap();
        let original = p.graph.evaluate_port(64, &PortRef::height(1)).unwrap();
        assert_ne!(result, *original.data);
    }

    #[test]
    fn rejects_incompatible_ports() {
        let mut p = Project::default();
        p.graph.nodes[1].inputs = vec![PortRef::new(1, "not_a_port")];
        assert!(p.graph.evaluate(64).unwrap_err().contains("no port"));
        p.graph.nodes[1].inputs = vec![PortRef::new(1, "height")];
        let id = p.graph.add(NodeKind::Slope);
        p.graph.nodes.iter_mut().find(|node| node.id == id).unwrap().inputs =
            vec![PortRef::height(1)];
        let mask = PortRef::new(id, "slope");
        p.graph.nodes[1].inputs = vec![mask];
        assert!(p.graph.evaluate_port(64, &PortRef::height(2))
            .unwrap_err().contains("expects Height"));
    }

    #[test]
    fn reuses_unmodified_ancestors() {
        let mut p = Project::default();
        let mut cache = GraphCache::default();
        cache.evaluate(&p.graph, 64, &PortRef::height(3)).unwrap();
        assert_eq!(cache.stats.computed, 3);
        cache.evaluate(&p.graph, 64, &PortRef::height(3)).unwrap();
        assert_eq!(cache.stats.computed, 0);
        assert_eq!(cache.stats.reused, 3);
        if let NodeKind::Terrace { steps, .. } = &mut p.graph.nodes[2].kind {
            *steps += 1;
        }
        cache.evaluate(&p.graph, 64, &PortRef::height(3)).unwrap();
        assert_eq!(cache.stats.computed, 1);
        assert_eq!(cache.stats.reused, 2);
    }

    #[test]
    fn hydraulic_node_exposes_conserved_independent_fields() {
        use crate::hydrology::HydraulicParameters;
        let mut p = Project::default();
        let id = p.graph.add(NodeKind::Hydraulic {
            params: HydraulicParameters::default(),
        });
        // Hydraulics consumes the original generator for this test.
        p.graph.nodes.iter_mut().find(|node| node.id == id).unwrap().inputs =
            vec![PortRef::height(1)];
        let height = p.graph.evaluate_port(64, &PortRef::height(id)).unwrap();
        let flow = p.graph.evaluate_port(64, &PortRef::new(id, "flow")).unwrap();
        let water = p.graph.evaluate_port(64, &PortRef::new(id, "water")).unwrap();
        assert_eq!(height.kind, PortType::Height);
        assert_eq!(flow.kind, PortType::Mask);
        assert_eq!(water.kind, PortType::Water);
        assert!(flow.data.values.iter().any(|&v| v > 0.0));
        assert!(water.data.values.iter().all(|v| *v >= 0.0));
        assert!(height.data.values.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn cancelled_generation_does_not_finish() {
        let p = Project::default();
        let signal = AtomicU64::new(3);
        let mut cache = GraphCache::default();
        let error = cache.evaluate_cancellable(
            &p.graph, 64, &PortRef::height(p.graph.output), &signal, 2
        ).unwrap_err();
        assert!(error.contains("cancelled"));
        assert_eq!(cache.stats.computed, 0);
    }

    #[test]
    fn headless_2k_generation_is_finite_and_matches_normalized_corners() {
        let mut graph = Project::default().graph;
        let noise = graph.add(NodeKind::Noise {
            seed: 19, frequency: 7.0, amplitude: 0.8
        });
        let mut cache = GraphCache::default();
        let high = cache.evaluate(&graph, 2048, &PortRef::height(noise))
            .unwrap();
        let preview = cache.evaluate(&graph, 32, &PortRef::height(noise))
            .unwrap();
        assert_eq!(high.data.size, 2048);
        assert_eq!(high.data.values.len(), 2048 * 2048);
        for &(x,y) in &[(0,0),(1,0),(0,1),(1,1)] {
            let hx = x * 2047;
            let hy = y * 2047;
            assert_eq!(high.data.get(hx,hy), preview.data.get(x*31,y*31));
        }
        assert!(high.data.values.iter().all(|v| v.is_finite()));
        assert!(cache.evaluate(&graph, 2049, &PortRef::height(noise)).is_err());
    }

    #[test]
    fn sibling_uses_shared_upstream_fields() {
        let mut p = Project::default();
        p.graph.nodes.push(Node {
            id: 4,
            kind: NodeKind::Slope,
            inputs: vec![PortRef::height(2)],
            position: [0.0, 0.0],
        });
        let mut cache = GraphCache::default();
        cache.evaluate(&p.graph, 64, &PortRef::new(4, "slope")).unwrap();
        cache.evaluate(&p.graph, 64, &PortRef::height(3)).unwrap();
        assert_eq!(cache.stats.computed, 1);
        assert_eq!(cache.stats.reused, 2);
    }
}
