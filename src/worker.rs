//! Dedicated cancellable background evaluator. The GUI never waits for erosion.
use crate::pipeline::{GraphCache, PortRef, PortType};
use crate::terrain::{HeightField, Project};
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
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (result_tx, result_rx) = mpsc::channel::<WorkResult>();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = generation.clone();
        std::thread::Builder::new()
            .name("gaia-graph-worker".to_string())
            .spawn(move || {
                let mut cache = GraphCache::default();
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
    let height = cache.evaluate_cancellable(
        &request.project.graph, size,
        &PortRef::height(request.project.graph.output),
        generation, request.generation,
    )?;
    let computed = cache.stats.computed;
    let reused = cache.stats.reused;
    let preview = cache.evaluate_cancellable(
        &request.project.graph, size, &request.preview,
        generation, request.generation,
    )?;
    let computed = computed + cache.stats.computed;
    let reused = reused + cache.stats.reused;
    Ok(BuildData {
        height: (*height.data).clone(),
        preview: (*preview.data).clone(),
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
