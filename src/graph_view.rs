//! Pure 2D graph viewport transforms: never mutate serialized node positions
//! when the user navigates. Mouse-centered zoom preserves the point under
//! the pointer; Fit all accounts for actual port counts and card dimensions.
use eframe::egui::{self, Pos2, Rect, Vec2};
use gaia::terrain::Node;

pub const CARD_WIDTH: f32 = 194.0;
pub const PORT_START: f32 = 56.0;
pub const PORT_STEP: f32 = 22.0;
// Allow a meaningful entire-graph overview even for 50–100 node layouts.
pub const MIN_ZOOM: f32 = 0.06;
pub const MAX_ZOOM: f32 = 2.25;

pub fn card_height(node: &Node) -> f32 {
    let ports = node.inputs.len().max(node.kind.output_ports().len()).max(1);
    65.0 + ports as f32 * PORT_STEP
}

#[derive(Clone, Copy, Debug)]
pub struct GraphView {
    pub zoom: f32,
    pub pan: Vec2,
}

impl Default for GraphView {
    fn default() -> Self {
        Self { zoom: 1.0, pan: egui::Vec2::ZERO }
    }
}

impl GraphView {
    pub fn to_screen(&self, viewport: Rect, world: Pos2) -> Pos2 {
        viewport.min + self.pan + world.to_vec2() * self.zoom
    }

    pub fn to_world(&self, viewport: Rect, screen: Pos2) -> Pos2 {
        let v = (screen - viewport.min - self.pan) / self.zoom;
        Pos2::new(v.x, v.y)
    }

    pub fn node_rect(&self, viewport: Rect, node: &Node) -> Rect {
        Rect::from_min_size(self.to_screen(viewport,
            Pos2::new(node.position[0], node.position[1])),
            egui::vec2(CARD_WIDTH, card_height(node)) * self.zoom)
    }

    pub fn zoom_at(&mut self, viewport: Rect, anchor: Pos2, factor: f32) {
        let world = self.to_world(viewport, anchor);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = anchor - viewport.min - world.to_vec2() * self.zoom;
    }

    pub fn center_on(&mut self, viewport: Rect, node: &Node) {
        let world_center = Pos2::new(
            node.position[0] + CARD_WIDTH * 0.5,
            node.position[1] + card_height(node) * 0.5);
        self.pan = viewport.size() * 0.5 - world_center.to_vec2() * self.zoom;
    }

    pub fn fit_all(&mut self, viewport: Rect, nodes: &[Node]) {
        if nodes.is_empty() {
            *self = Self::default();
            return;
        }
        let min_x = nodes.iter().map(|n| n.position[0])
            .fold(f32::INFINITY, f32::min);
        let min_y = nodes.iter().map(|n| n.position[1])
            .fold(f32::INFINITY, f32::min);
        let max_x = nodes.iter().map(|n| n.position[0] + CARD_WIDTH)
            .fold(f32::NEG_INFINITY, f32::max);
        let max_y = nodes.iter().map(|n| n.position[1] + card_height(n))
            .fold(f32::NEG_INFINITY, f32::max);
        let margin = 44.0;
        let available = viewport.size() - Vec2::splat(margin * 2.0);
        let width = (max_x - min_x).max(1.0);
        let height = (max_y - min_y).max(1.0);
        self.zoom = (available.x.max(1.0) / width)
            .min(available.y.max(1.0) / height).clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = viewport.size() * 0.5
            - egui::vec2((min_x + max_x) * 0.5,
                (min_y + max_y) * 0.5) * self.zoom;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gaia::terrain::Project;

    fn viewport() -> Rect {
        Rect::from_min_size(Pos2::new(18.0, 35.0), egui::vec2(780.0, 360.0))
    }

    #[test]
    fn zoom_keeps_cursor_world_point_stationary_and_clamps() {
        let viewport = viewport();
        let anchor = Pos2::new(310.0, 182.0);
        let mut view = GraphView::default();
        let world = view.to_world(viewport, anchor);
        view.zoom_at(viewport, anchor, 1.6);
        assert!((view.to_screen(viewport, world) - anchor).length() < 1e-4);
        view.zoom_at(viewport, anchor, 100.0);
        assert_eq!(view.zoom, MAX_ZOOM);
        view.zoom_at(viewport, anchor, 0.00001);
        assert_eq!(view.zoom, MIN_ZOOM);
    }

    #[test]
    fn fit_and_focus_are_view_only_and_keep_cards_on_screen() {
        let graph = Project::default().graph;
        let nodes = &graph.nodes;
        let before = nodes.iter().map(|n| n.position).collect::<Vec<_>>();
        let viewport = viewport();
        let mut view = GraphView::default();
        view.fit_all(viewport, nodes);
        for node in nodes {
            assert!(view.node_rect(viewport, node).intersects(viewport));
        }
        view.center_on(viewport, &nodes[0]);
        assert!((view.node_rect(viewport, &nodes[0]).center()
            - viewport.center()).length() < 1e-4);
        assert_eq!(nodes.iter().map(|n| n.position).collect::<Vec<_>>(), before);
    }

    #[test]
    fn fit_frames_large_graph_without_touching_node_coordinates() {
        let mut graph = Project::default().graph;
        for i in 0..80 {
            graph.add(gaia::terrain::NodeKind::Noise {
                seed: i, frequency: 4.0, amplitude: 0.2,
            });
        }
        let positions: Vec<_> = graph.nodes.iter().map(|node| node.position).collect();
        let viewport = Rect::from_min_size(
            Pos2::new(10.0, 20.0), egui::vec2(700.0, 230.0));
        let mut view = GraphView::default();
        view.fit_all(viewport, &graph.nodes);
        assert!(view.zoom < 0.35);
        for node in &graph.nodes {
            let card = view.node_rect(viewport, node);
            assert!(viewport.contains(card.center()), "not fitted node #{}: {:?}", node.id, card);
        }
        assert_eq!(positions, graph.nodes.iter().map(|node| node.position).collect::<Vec<_>>());
    }

    #[test]
    fn port_count_controls_card_height() {
        let mut graph = Project::default().graph;
        let short = card_height(graph.get(1).unwrap());
        let id = graph.add(gaia::terrain::NodeKind::MaterialMasks {
            sea_level: 0.25, beach_width: 0.08, rock_slope: 0.32,
            rock_blend: 0.1, snowline: 0.8, snow_blend: 0.12,
            palette: Default::default(),
        });
        assert!(card_height(graph.get(id).unwrap()) > short);
    }
}
