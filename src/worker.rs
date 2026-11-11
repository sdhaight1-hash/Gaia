//! Dedicated cancellable background evaluator. The GUI never waits for erosion.
use crate::pipeline::{BlendCompute, GraphCache, PortRef, PortType};
use crate::gpu_compute::GpuBlend;
use crate::color::ColorField;
use eframe::egui_wgpu::wgpu;
use crate::terrain::{HeightField, NodeKind, Project};
use std::sync::{
    Arc, mpsc::{self, Receiver, Sender, TryRecvError},
    atomic::{AtomicU64, Ordering},
};

struct Request {
    generation: u64,
    project: Project,
    preview: PortRef,
}

pub struct WorkResult {
    pub generation: u64,
    pub data: Result<BuildData, String>,
}

pub struct BuildData {
    pub height: HeightField,
    pub preview: HeightField,
    pub preview_color: Option<ColorField>,
    pub preview_kind: PortType,
    pub computed: usize,
    pub reused: usize,
}

/// Background worker with latest-request-wins scheduling. A generation token
/// cancels stale builds mid-erosion and prevents stale UI updates.
pub struct GraphWorker {
    request_tx: Sender<Request>,
    result_rx: Receiver<WorkResult>,
    generation: Arc<AtomicU64>,
    pending: Option<u64>,
}

impl Default for GraphWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphWorker {
    pub fn new() -> Self {
        Self::spawn(None)
    }

    pub fn with_gpu(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self::spawn(Some(Arc::new(GpuBlend::new(device, queue))))
    }

    fn spawn(backend: Option<Arc<dyn BlendCompute>>) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (result_tx, result_rx) = mpsc::channel::<WorkResult>();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = generation.clone();
        std::thread::Builder::new()
            .name("gaia-graph-worker".to_string())
            .spawn(move || {
                let mut cache = GraphCache::default();
                cache.set_backend(backend);
                while let Ok(mut request) = request_rx.recv() {
                    // Drop requests superseded before this worker started.
                    while let Ok(newest) = request_rx.try_recv() {
                        request = newest;
                    }
                    let current = request.generation;
                    if worker_generation.load(Ordering::Relaxed) != current {
                        continue;
                    }
                    let data = build(&mut cache, &request, &worker_generation);
                    // A slider edit can arrive during the build. Do not
                    // publish stale heightfields even if evaluation succeeded.
                    if worker_generation.load(Ordering::Relaxed) == current
                        && result_tx.send(WorkResult { generation: current, data }).is_err()
                    {
                        break;
                    }
                }
            })
            .expect("could not start Gaia graph worker");
        Self { request_tx, result_rx, generation, pending: None }
    }

    pub fn submit(&mut self, project: Project, preview: PortRef) -> Result<u64, String> {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.pending = Some(generation);
        self.request_tx.send(Request { generation, project, preview })
            .map_err(|_| "Terrain build worker has stopped".to_string())?;
        Ok(generation)
    }

    pub fn poll(&mut self) -> Option<WorkResult> {
        let mut latest = None;
        loop {
            match self.result_rx.try_recv() {
                Ok(result) => {
                    if Some(result.generation) == self.pending {
                        self.pending = None;
                        latest = Some(result);
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        latest
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

fn build(
    cache: &mut GraphCache, request: &Request, generation: &AtomicU64,
) -> Result<BuildData, String> {
    let size = request.project.resolution;
    // For selected Height ports, the selected field is the actual 3D mesh.
    // Do not also evaluate the unrelated global output (often an expensive
    // erosion chain) merely to show a Mountain or Lake:surface preview.
    let selected_is_height = request.project.graph.get(request.preview.node)
        .is_some_and(|node| node.kind.output_ports().iter().any(|(name, ty)|
            *name == request.preview.port && *ty == PortType::Height));
    let (render_height, preview, computed, reused) = if selected_is_height {
        let preview = cache.evaluate_cancellable(
            &request.project.graph, size, &request.preview,
            generation, request.generation,
        )?;
        (preview.data.clone(), preview, cache.stats.computed, cache.stats.reused)
    } else {
        // A MaterialMasks node is classified from *its own input height*.
        // Preview that same upstream terrain for the splat and mask ports:
        // using the unrelated project output misaligns rock/snow coverage
        // when the graph continues with erosion or other height modifiers.
        let render_source = request.project.graph.get(request.preview.node)
            .filter(|node| matches!(node.kind, NodeKind::MaterialMasks { .. }))
            .and_then(|node| node.inputs.first())
            .cloned()
            .unwrap_or_else(|| PortRef::height(request.project.graph.output));
        let height = cache.evaluate_cancellable(
            &request.project.graph, size, &render_source,
            generation, request.generation,
        )?;
        let computed = cache.stats.computed;
        let reused = cache.stats.reused;
        let preview = cache.evaluate_cancellable(
            &request.project.graph, size, &request.preview,
            generation, request.generation,
        )?;
        (height.data.clone(), preview,
            computed + cache.stats.computed, reused + cache.stats.reused)
    };
    Ok(BuildData {
        height: (*render_height).clone(),
        preview: (*preview.data).clone(),
        preview_color: preview.color.as_ref().map(|rgba| (**rgba).clone()),
        preview_kind: preview.kind,
        computed,
        reused,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn worker_produces_terrain_and_selected_mask() {
        let mut worker = GraphWorker::new();
        let project = Project::default();
        let generation = worker.submit(project, PortRef::new(2, "wear")).unwrap();
        let limit = std::time::Instant::now() + Duration::from_secs(5);
        let result = loop {
            if let Some(result) = worker.poll() { break result; }
            assert!(std::time::Instant::now() < limit, "worker did not respond");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(result.generation, generation);
        let result = result.data.unwrap();
        assert_eq!(result.height.size, 128);
        assert_eq!(result.preview_kind, PortType::Mask);
        assert!(result.preview.mass() > 0.0);
    }

    #[test]
    fn selecting_earlier_height_changes_3d_geometry_without_building_final_erosion() {
        let mut project = Project::default();
        project.resolution = 32;
        let source = project.graph.evaluate_port(32, &PortRef::height(1)).unwrap();
        let latest = project.graph.evaluate(32).unwrap();
        assert_ne!(*source.data, latest);
        let mut cache = GraphCache::default();
        let generation = AtomicU64::new(1);
        let output = build(&mut cache, &Request {
            project, generation: 1, preview: PortRef::height(1)
        }, &generation).unwrap();
        assert_eq!(output.height, *source.data);
        assert_eq!(output.preview_kind, PortType::Height);
        assert_eq!(output.computed, 1,
            "only the selected upstream height node should be built");
    }

    #[test]
    fn selected_lake_surface_becomes_preview_mesh_and_mask_keeps_terrain_mesh() {
        use crate::terrain::NodeKind;
        let mut project = Project::default();
        project.resolution = 32;
        let lake = project.graph.add_from(NodeKind::Lake {
            fill_fraction: 0.8, minimum_area: 1, minimum_depth: 0.0,
        }, &PortRef::height(1));
        let surface_port = PortRef::new(lake, "surface");
        let surface = project.graph.evaluate_port(32, &surface_port).unwrap();
        let generation = AtomicU64::new(1);
        let mut cache = GraphCache::default();
        let selected = build(&mut cache, &Request {
            project: project.clone(), generation: 1, preview: surface_port
        }, &generation).unwrap();
        assert_eq!(selected.height, *surface.data);
        assert_eq!(selected.preview_kind, PortType::Height);
        let masked = build(&mut cache, &Request {
            project: project.clone(), generation: 1,
            preview: PortRef::new(lake, "coverage")
        }, &generation).unwrap();
        assert_eq!(masked.preview_kind, PortType::Mask);
        assert_eq!(masked.height, project.graph.evaluate(32).unwrap());
    }

    #[test]
    fn material_mask_preview_uses_its_input_height_not_unrelated_output() {
        let mut project = Project::default();
        project.resolution = 32;
        let source_height = project.graph.evaluate_port(32, &PortRef::height(1))
            .unwrap().data;
        let final_height = project.graph.evaluate(32).unwrap();
        assert_ne!(*source_height, final_height);
        let node = project.graph.add_from(NodeKind::MaterialMasks {
            sea_level: 0.25, beach_width: 0.08,
            rock_slope: 0.32, rock_blend: 0.1,
            snowline: 0.8, snow_blend: 0.12,
        }, &PortRef::height(1));
        assert_eq!(project.graph.output, 3);
        let generation = AtomicU64::new(1);
        let mut cache = GraphCache::default();
        for name in ["splat", "rock"] {
            let out = build(&mut cache, &Request {
                generation: 1, project: project.clone(),
                preview: PortRef::new(node, name),
            }, &generation).unwrap();
            assert_eq!(out.height, *source_height);
            assert_ne!(out.height, final_height);
            if name == "splat" {
                assert_eq!(out.preview_kind, PortType::Splat);
                assert_eq!(out.preview_color.as_ref().unwrap().pixels.len(), 1024);
            } else {
                assert_eq!(out.preview_kind, PortType::Mask);
            }
        }
    }

    #[test]
    fn newest_request_supersedes_old_build() {
        let mut worker = GraphWorker::new();
        let mut project = Project::default();
        project.resolution = 256;
        let old = worker.submit(project.clone(), PortRef::height(1)).unwrap();
        project.resolution = 64;
        let latest = worker.submit(project, PortRef::new(2, "deposits")).unwrap();
        assert!(latest > old);
        let limit = std::time::Instant::now() + Duration::from_secs(5);
        let result = loop {
            if let Some(result) = worker.poll() { break result; }
            assert!(std::time::Instant::now() < limit, "worker did not respond");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(result.generation, latest);
        assert_eq!(result.data.unwrap().height.size, 64);
    }
}
