//! Typed, multi-output terrain graph evaluation with incremental node caching.
//! Legacy JSON source node IDs are migrated transparently to named height ports.
//! This module intentionally has no UI or GPU dependencies.
use crate::hydrology::simulate;
use crate::color::{ColorField, altitude_palette, color_blend};
use crate::import::read_heightmap;
use std::path::Path;
use crate::geology::{mountain_range, crater, dunes, canyon, warp, curvature, elevation_mask};
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

/// Field-based output. This first milestone supports Height/Mask values.
/// Remaining port types are reserved and cannot be wired until implemented.
#[derive(Debug, Clone)]
pub struct FieldValue {
    pub kind: PortType,
    /// Scalar field, or luminance proxy for a genuine RGBA output.
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
        if !(32..=512).contains(&resolution) {
            return Err("Resolution must be between 32 and 512".to_owned());
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
        const CURVATURE: &[(&str, PortType)] = &[("curvature", PortType::Mask)];
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
            Self::Curvature => CURVATURE,
            Self::ElevationMask { .. } => ELEVATION,
            _ => HEIGHT,
        }
    }

    pub fn input_type(&self, slot: usize) -> Option<PortType> {
        match (self, slot) {
            (Self::Mountain { .. } | Self::Noise { .. } | Self::ImportHeight { .. } |
             Self::Range { .. } | Self::Crater { .. } |
             Self::Dunes { .. } | Self::Canyon { .. }, _) => None,
            (Self::MaskedBlend { .. }, 2) => Some(PortType::Mask),
            (Self::ColorMixer { .. }, 2) => Some(PortType::Mask),
            (Self::ColorMixer { .. }, 0 | 1) => Some(PortType::Color),
            (Self::MaskedBlend { .. }, 0 | 1) => Some(PortType::Height),
            (Self::Colorize { .. }, 0) => Some(PortType::Height),
            (Self::Blend { .. }, 0 | 1) => Some(PortType::Height),
            (Self::Erosion { .. } | Self::Hydraulic { .. } | Self::Terrace { .. } | Self::Blur { .. } | Self::Slope | Self::Curvature | Self::ElevationMask { .. } | Self::Warp { .. }, 0) => Some(PortType::Height),
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
        NodeKind::Blur { radius } => NodeOutputs::default()
            .with("height", PortType::Height, blur(take(0), *radius)),
        NodeKind::Blend { mix } => NodeOutputs::default()
            .with("height", PortType::Height, computed_blend(take(0), take(1), None, *mix)?),
        NodeKind::Slope => NodeOutputs::default()
            .with("slope", PortType::Mask, slope(take(0))),
        NodeKind::Curvature => NodeOutputs::default()
            .with("curvature", PortType::Mask, curvature(take(0))),
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
    fn color_node_yields_rgba_and_rejects_scalar_connections() {
        let mut p = Project::default();
        let id = p.graph.add(NodeKind::Colorize {
            low: [0.1, 0.2, 0.3], high: [0.8, 0.9, 1.0],
            midpoint: 0.5, contrast: 1.0,
        });
        let color = p.graph.evaluate_port(64, &PortRef::new(id, "color")).unwrap();
        assert_eq!(color.kind, PortType::Color);
        assert_eq!(color.color.as_ref().unwrap().pixels.len(), 64 * 64);
        assert!(p.graph.connect(2, 0, PortRef::new(id, "color")).is_err());
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
