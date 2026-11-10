mod gpu;
mod ui_theme;

use eframe::egui;
use gaia::terrain::{export_obj, export_png16, HeightField, NodeKind, Project};
use gaia::pipeline::{GraphCache, PortRef, PortType};
use std::path::Path;

struct GaiaApp {
    project: Project,
    field: HeightField,
    cache: GraphCache,
    texture: Option<egui::TextureHandle>,
    preview_source: PortRef,
    selected: u64,
    filename: String,
    status: String,
    yaw: f32,
    pitch: f32,
    distance: f32,
    view_2d: bool,
    graph_dirty: bool,
    gpu_dirty: bool,
    unsaved: bool,
}

impl GaiaApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ui_theme::apply(&cc.egui_ctx);
        let project = Project::default();
        let mut cache = GraphCache::default();
        let field = (*project.graph.evaluate_cached(project.resolution,
            &PortRef::height(project.graph.output), &mut cache)
            .expect("default terrain project must evaluate").data).clone();
        let preview_source = PortRef::height(project.graph.output);
        let yaw = 0.85;
        let pitch = 0.72;
        let distance = 3.7;
        if let Some(render_state) = &cc.wgpu_render_state {
            gpu::install(render_state, &field, gpu::eye_position(yaw, pitch, distance));
        }
        let texture = Some(cc.egui_ctx.load_texture(
            "Gaia heightfield", height_image(&field), egui::TextureOptions::LINEAR,
        ));
        Self {
            selected: project.graph.output,
            project,
            field,
            cache,
            texture,
            preview_source,
            filename: "terrain.gaia.json".to_string(),
            status: "Ready. Drag on the 3D preview to orbit; scroll to zoom.".to_string(),
            yaw,
            pitch,
            distance,
            view_2d: false,
            graph_dirty: false,
            gpu_dirty: false,
            unsaved: false,
        }
    }

    fn rebuild(&mut self, ctx: &egui::Context) {
        match self.project.graph.evaluate_cached(self.project.resolution,
            &PortRef::height(self.project.graph.output), &mut self.cache) {
            Ok(field) => {
                self.field = (*field.data).clone();
                let computed = self.cache.stats.computed;
                let reused = self.cache.stats.reused;
                self.gpu_dirty = true;
                self.status = format!(
                    "Built {} × {} terrain from node #{} · {} computed / {} reused",
                    self.field.size, self.field.size, self.project.graph.output,
                    computed, reused,
                );
                self.refresh_preview(ctx);
            }
            Err(err) => {
                self.status = format!("Graph error: {err}. Showing last valid preview.");
            }
        }
        self.graph_dirty = false;
    }

    fn refresh_preview(&mut self, ctx: &egui::Context) {
        match self.project.graph.evaluate_cached(self.project.resolution,
            &self.preview_source, &mut self.cache)
        {
            Ok(value) => {
                let image = height_image(&value.data);
                if let Some(texture) = self.texture.as_mut() {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    self.texture = Some(ctx.load_texture(
                        "Gaia selected graph output", image, egui::TextureOptions::LINEAR));
                }
            }
            Err(error) => self.status = format!(
                "Preview #{}:{}: {error}", self.preview_source.node, self.preview_source.port
            ),
        }
    }

    fn export_selected_map(&mut self) {
        let output = self.project.graph.evaluate_cached(self.project.resolution,
            &self.preview_source, &mut self.cache);
        match output {
            Ok(value) => {
                let path = Path::new(&self.filename)
                    .with_extension(format!("{}.png", self.preview_source.port));
                self.status = match export_png16(&value.data, &path) {
                    Ok(()) => format!("Exported selected map to {}", path.display()),
                    Err(error) => format!("Map export failed: {error}"),
                };
            }
            Err(error) => self.status = format!("Map export failed: {error}"),
        }
    }

    fn save_project(&mut self) {
        self.status = match self.project.save(Path::new(&self.filename)) {
            Ok(()) => {
                self.unsaved = false;
                format!("Saved {}", self.filename)
            }
            Err(error) => format!("Save failed: {error}"),
        };
    }

    fn open_project(&mut self) {
        match Project::load(Path::new(&self.filename)) {
            Ok(project) => {
                self.selected = project.graph.output;
                self.preview_source = PortRef::height(project.graph.output);
                self.project = project;
                self.cache.clear();
                self.graph_dirty = true;
                self.unsaved = false;
                self.status = format!("Opened {}", self.filename);
            }
            Err(error) => self.status = format!("Open failed: {error}"),
        }
    }

    fn export_heightmap(&mut self) {
        let path = Path::new(&self.filename).with_extension("png");
        self.status = match export_png16(&self.field, &path) {
            Ok(()) => format!("Exported heightmap to {}", path.display()),
            Err(error) => format!("PNG export failed: {error}"),
        };
    }

    fn export_mesh(&mut self) {
        let path = Path::new(&self.filename).with_extension("obj");
        self.status = match export_obj(&self.field, &path) {
            Ok(()) => format!("Exported mesh to {}", path.display()),
            Err(error) => format!("OBJ export failed: {error}"),
        };
    }

    fn add_node(&mut self, kind: NodeKind) {
        let is_mask = matches!(kind, NodeKind::Slope);
        self.selected = self.project.graph.add(kind);
        if let Some(node) = self.project.graph.get(self.selected) {
            if let Some((name, _)) = node.kind.output_ports().first() {
                self.preview_source = PortRef::new(self.selected, *name);
            }
        }
        if is_mask {
            self.view_2d = true;
        }
        self.graph_dirty = true;
        self.unsaved = true;
    }

    fn palette_node(&mut self, ui: &mut egui::Ui, name: &str, detail: &str, kind: NodeKind) {
        let button = egui::Button::new(
            egui::RichText::new(name).size(13.0).color(ui_theme::TEXT)
        ).fill(ui_theme::PANEL_RAISED);
        let response = ui.add_sized([ui.available_width(), 34.0], button)
            .on_hover_text(detail);
        if response.clicked() {
            self.add_node(kind);
        }
    }

    fn library(&mut self, ui: &mut egui::Ui) {
        ui_theme::section(ui, "NODE LIBRARY");
        ui.label(egui::RichText::new("Create").size(21.0).strong().color(ui_theme::TEXT));
        ui.label(egui::RichText::new("Build your terrain with procedural blocks.")
            .size(12.0).color(ui_theme::MUTED));
        ui.add_space(20.0);
        ui_theme::section(ui, "GENERATORS");
        self.palette_node(ui, "▲  Mountain", "Create a ridged mountain mass",
            NodeKind::Mountain { seed: 42, frequency: 4.0, height: 1.0 });
        self.palette_node(ui, "◈  Noise", "Layer smooth fractal noise",
            NodeKind::Noise { seed: 7, frequency: 6.0, amplitude: 0.4 });
        ui.add_space(16.0);
        ui_theme::section(ui, "TERRAIN OPERATORS");
        self.palette_node(ui, "≋  Erosion", "Mass-conserving thermal talus transport",
            NodeKind::Erosion { iterations: 24, talus: 0.008, strength: 0.28 });
        self.palette_node(ui, "▤  Terrace", "Add stepped elevation bands",
            NodeKind::Terrace { steps: 12, softness: 0.8 });
        self.palette_node(ui, "◎  Blur", "Soften local terrain detail",
            NodeKind::Blur { radius: 2 });
        self.palette_node(ui, "◢  Slope Mask", "Derive a typed slope mask from terrain",
            NodeKind::Slope);
        ui.add_space(16.0);
        ui_theme::section(ui, "COMPOSITING");
        self.palette_node(ui, "◇  Blend", "Mix the heights from two input nodes",
            NodeKind::Blend { mix: 0.5 });
        self.palette_node(ui, "◈  Masked Blend", "Blend two heightfields using a mask port",
            NodeKind::MaskedBlend { mix: 1.0 });
        ui.add_space(26.0);
        ui.separator();
        ui.label(egui::RichText::new("TIP").size(11.0)
            .color(ui_theme::ACCENT).strong());
        ui.label(egui::RichText::new(
            "Add a node, select it in the graph, then edit its connections in Properties."
        ).size(12.0).color(ui_theme::MUTED));
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("◈").size(22.0).color(ui_theme::ACCENT));
            ui.label(egui::RichText::new("GAIA").size(20.0)
                .strong().color(ui_theme::TEXT));
            ui.label(egui::RichText::new("TERRAIN STUDIO").size(10.0)
                .color(ui_theme::MUTED));
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.filename)
                .desired_width(185.0).hint_text("project.gaia.json"))
                .on_hover_text("Project location (relative to the current folder)");
            if self.unsaved {
                ui.label(egui::RichText::new("●").color(ui_theme::AMBER))
                    .on_hover_text("Unsaved project changes");
            }
            if ui.button("Open").on_hover_text("Open project · Ctrl+O").clicked() {
                self.open_project();
            }
            if ui.button("Save").on_hover_text("Save project · Ctrl+S").clicked() {
                self.save_project();
            }
            ui.menu_button("Export ▾", |ui| {
                if ui.button("16-bit Heightmap  ·  PNG").clicked() {
                    self.export_heightmap();
                    ui.close();
                }
                if ui.button("3D Terrain Mesh  ·  OBJ").clicked() {
                    self.export_mesh();
                    ui.close();
                }
                if ui.button("Selected graph output  ·  PNG16").clicked() {
                    self.export_selected_map();
                    ui.close();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(
                    egui::RichText::new("↻  Rebuild").color(ui_theme::BG).strong()
                ).fill(ui_theme::ACCENT)).on_hover_text("Regenerate the terrain").clicked() {
                    self.graph_dirty = true;
                }
            });
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        ui_theme::section(ui, "PROPERTIES");
        ui.label(egui::RichText::new("Inspector").size(21.0)
            .strong().color(ui_theme::TEXT));
        ui.label(egui::RichText::new("Edit the selected terrain node.")
            .size(12.0).color(ui_theme::MUTED));
        ui.add_space(15.0);
        ui.separator();
        ui_theme::section(ui, "PROJECT");
        ui.horizontal(|ui| {
            ui.label("Resolution");
            let before = self.project.resolution;
            egui::ComboBox::from_id_salt("resolution")
                .selected_text(format!("{} × {}", self.project.resolution, self.project.resolution))
                .show_ui(ui, |ui| {
                    for size in [64, 128, 256, 512] {
                        ui.selectable_value(&mut self.project.resolution, size, format!("{size} × {size}"));
                    }
                });
            if before != self.project.resolution {
                self.graph_dirty = true;
                self.unsaved = true;
            }
        });
        ui.add_space(14.0);
        ui.separator();
        ui_theme::section(ui, "SELECTED NODE");
        let options: Vec<(PortRef, PortType, String)> = self.project.graph.nodes.iter()
            .flat_map(|node| node.kind.output_ports().iter().map(|(port, kind)| {
                (PortRef::new(node.id, *port), *kind,
                    format!("#{} {} · {}", node.id, node.kind.label(), port))
            })).collect();
        let mut changed = false;
        let mut set_output = false;
        if let Some(node) = self.project.graph.nodes.iter_mut().find(|n| n.id == self.selected) {
            ui.label(egui::RichText::new(node.kind.label()).size(18.0)
                .strong().color(ui_theme::ACCENT));
            ui.label(egui::RichText::new(format!("NODE  ·  #{:03}", node.id))
                .monospace().size(11.0).color(ui_theme::MUTED));
            ui.add_space(9.0);
            let can_output = node.kind.output_ports().iter()
                .any(|(name, ty)| *name == "height" && *ty == PortType::Height);
            if ui.add_enabled_ui(can_output, |ui| {
                ui.add_sized([ui.available_width(), 32.0],
                    egui::Button::new("Set as output").fill(ui_theme::PANEL_RAISED))
            }).inner.on_hover_text("Use this node as the terrain height output").clicked() {
                set_output = true;
            }
            for (index, input) in node.inputs.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("Input {}", index + 1));
                    let required = node.kind.input_type(index);
                    let current_name = options.iter()
                        .find(|(reference, _, _)| *reference == *input)
                        .map(|(_, _, name)| name.as_str()).unwrap_or("Invalid connection");
                    egui::ComboBox::from_id_salt(("input", node.id, index))
                        .selected_text(current_name)
                        .show_ui(ui, |ui| {
                            for (reference, kind, name) in &options {
                                if reference.node != node.id && Some(*kind) == required {
                                    changed |= ui.selectable_value(input, reference.clone(), name).changed();
                                }
                            }
                        });
                });
            }
            ui.add_space(10.0);
            match &mut node.kind {
                NodeKind::Mountain { seed, frequency, height } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(frequency, 0.2..=20.0).text("Frequency")).changed();
                    changed |= ui.add(egui::Slider::new(height, 0.0..=2.0).text("Height")).changed();
                }
                NodeKind::Noise { seed, frequency, amplitude } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(frequency, 0.2..=40.0).text("Frequency")).changed();
                    changed |= ui.add(egui::Slider::new(amplitude, 0.0..=2.0).text("Amplitude")).changed();
                }
                NodeKind::Erosion { iterations, talus, strength } => {
                    changed |= ui.add(egui::Slider::new(iterations, 0..=120).text("Iterations")).changed();
                    changed |= ui.add(egui::Slider::new(talus, 0.0..=0.1).text("Talus")).changed();
                    changed |= ui.add(egui::Slider::new(strength, 0.0..=0.5).text("Transfer")).changed();
                    ui.small("Finite-volume thermal erosion; hydraulic erosion is not yet implemented.");
                }
                NodeKind::Terrace { steps, softness } => {
                    changed |= ui.add(egui::Slider::new(steps, 2..=64).text("Steps")).changed();
                    changed |= ui.add(egui::Slider::new(softness, 0.0..=1.0).text("Softness")).changed();
                }
                NodeKind::Blur { radius } => {
                    changed |= ui.add(egui::Slider::new(radius, 1..=8).text("Radius")).changed();
                }
                NodeKind::Blend { mix } | NodeKind::MaskedBlend { mix } => {
                    changed |= ui.add(egui::Slider::new(mix, 0.0..=1.0).text("Mix")).changed();
                }
                NodeKind::Slope => {
                    ui.label("Slope mask derived from surrounding elevation differences.");
                }
            }
        } else {
            ui.label("Click a node in the graph to edit it.");
        }
        if set_output {
            self.project.graph.output = self.selected;
            self.preview_source = PortRef::height(self.selected);
            changed = true;
        }
        if changed {
            self.graph_dirty = true;
            self.unsaved = true;
        }
        ui.separator();
        ui.label(egui::RichText::new("Drag nodes to arrange your graph. Change connections with the input selectors.")
            .size(12.0).color(ui_theme::MUTED));
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Connections are validated before rebuilding.")
            .size(12.0).color(ui_theme::MUTED));
    }

    fn graph_canvas(&mut self, ui: &mut egui::Ui) {
        const WIDTH: f32 = 194.0;
        const HEIGHT: f32 = 74.0;
        let extent = egui::vec2(ui.available_width().max(920.0), 360.0);
        let (rect, _) = ui.allocate_exact_size(extent, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, ui_theme::CANVAS);

        // Deliberately subtle grid: nodes and dataflow remain the focal point.
        let grid = egui::Stroke::new(0.65, egui::Color32::from_rgb(29, 37, 43));
        for x in 0..=(extent.x / 28.0) as usize {
            let at = rect.left() + x as f32 * 28.0;
            painter.line_segment(
                [egui::pos2(at, rect.top()), egui::pos2(at, rect.bottom())], grid
            );
        }
        for y in 0..=(extent.y / 28.0) as usize {
            let at = rect.top() + y as f32 * 28.0;
            painter.line_segment(
                [egui::pos2(rect.left(), at), egui::pos2(rect.right(), at)], grid
            );
        }

        let nodes = self.project.graph.nodes.clone();
        for node in &nodes {
            for (i, source) in node.inputs.iter().enumerate() {
                if let Some(parent) = nodes.iter().find(|n| n.id == source.node) {
                    let start = rect.left_top() + egui::vec2(
                        parent.position[0] + WIDTH, parent.position[1] + 28.0
                    );
                    let end = rect.left_top() + egui::vec2(
                        node.position[0], node.position[1] + 29.0 + i as f32 * 17.0
                    );
                    let mut points = Vec::with_capacity(21);
                    for k in 0..=20 {
                        let t = k as f32 / 20.0;
                        let a = start;
                        let b = start + egui::vec2(60.0, 0.0);
                        let c = end - egui::vec2(60.0, 0.0);
                        let d = end;
                        let inv = 1.0 - t;
                        points.push(egui::pos2(
                            a.x * inv * inv * inv + 3.0 * b.x * inv * inv * t
                                + 3.0 * c.x * inv * t * t + d.x * t * t * t,
                            a.y * inv * inv * inv + 3.0 * b.y * inv * inv * t
                                + 3.0 * c.y * inv * t * t + d.y * t * t * t,
                        ));
                    }
                    painter.add(egui::Shape::line(
                        points, egui::Stroke::new(2.0, node_accent(&parent.kind))
                    ));
                }
            }
        }
        for node in &mut self.project.graph.nodes {
            let top_left = rect.left_top() + egui::vec2(node.position[0], node.position[1]);
            let node_rect = egui::Rect::from_min_size(top_left, egui::vec2(WIDTH, HEIGHT));
            let response = ui.interact(
                node_rect, ui.scope_id().with(("node", node.id)),
                egui::Sense::click_and_drag()
            ).on_hover_text("Click to inspect · drag to move");
            if response.clicked() {
                self.selected = node.id;
            }
            if response.dragged() {
                // drag_motion is the per-frame delta; drag_delta would compound.
                let motion = response.drag_motion();
                node.position[0] = (node.position[0] + motion.x).max(0.0);
                node.position[1] = (node.position[1] + motion.y).max(0.0);
                self.selected = node.id;
                self.unsaved = true;
            }
            let color = node_accent(&node.kind);
            let selected = self.selected == node.id;
            let background = if selected {
                egui::Color32::from_rgb(36, 56, 59)
            } else if response.hovered() {
                egui::Color32::from_rgb(41, 50, 59)
            } else {
                ui_theme::PANEL_RAISED
            };
            painter.rect_filled(node_rect, 9.0, background);
            painter.rect_stroke(
                node_rect, 9.0,
                egui::Stroke::new(if selected { 1.8 } else { 1.0 },
                    if selected { ui_theme::ACCENT } else { ui_theme::BORDER }),
                egui::StrokeKind::Outside
            );
            painter.rect_filled(egui::Rect::from_min_max(
                node_rect.left_top() + egui::vec2(0.0, 10.0),
                node_rect.left_bottom() + egui::vec2(3.0, -10.0)
            ), 2.0, color);
            painter.text(node_rect.left_top() + egui::vec2(14.0, 11.0),
                egui::Align2::LEFT_TOP, node.kind.label(),
                egui::FontId::proportional(15.0), ui_theme::TEXT);
            painter.text(node_rect.left_top() + egui::vec2(14.0, 41.0),
                egui::Align2::LEFT_TOP, format!("NODE #{:03}", node.id),
                egui::FontId::monospace(10.0), ui_theme::MUTED);
            for i in 0..node.inputs.len() {
                painter.circle_filled(
                    node_rect.left_top() + egui::vec2(0.0, 29.0 + i as f32 * 17.0),
                    4.0, ui_theme::BLUE
                );
            }
            painter.circle_filled(
                node_rect.left_top() + egui::vec2(WIDTH, 28.0), 4.0, color
            );
            if node.id == self.project.graph.output {
                painter.text(node_rect.right_bottom() + egui::vec2(-10.0, -12.0),
                    egui::Align2::RIGHT_BOTTOM, "OUTPUT",
                    egui::FontId::monospace(10.0), ui_theme::ACCENT);
            }
        }
    }

    fn viewport(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Preview").size(19.0)
                .strong().color(ui_theme::TEXT));
            ui.separator();
            ui.selectable_value(&mut self.view_2d, false, "3D");
            ui.selectable_value(&mut self.view_2d, true, "2D map");
            let before = self.preview_source.clone();
            egui::ComboBox::from_id_salt("preview_port")
                .selected_text(format!(
                    "#{}:{}",
                    self.preview_source.node, self.preview_source.port
                ))
                .show_ui(ui, |ui| {
                    for node in &self.project.graph.nodes {
                        for (port, kind) in node.kind.output_ports() {
                            let source = PortRef::new(node.id, *port);
                            let label = format!("#{} {} · {} ({})",
                                node.id, node.kind.label(), port, kind.label());
                            ui.selectable_value(&mut self.preview_source, source, label);
                        }
                    }
                });
            if before != self.preview_source {
                self.view_2d = true;
                let ctx = ui.ctx().clone();
                self.refresh_preview(&ctx);
            }
            ui.label(egui::RichText::new(format!(
                "{}²", self.field.size
            )).monospace().size(11.0).color(ui_theme::MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Reset camera").clicked() {
                    self.yaw = 0.85;
                    self.pitch = 0.72;
                    self.distance = 3.7;
                    self.gpu_dirty = true;
                }
            });
        });
        ui.add_space(9.0);
        let size = ui.available_size().max(egui::vec2(100.0, 100.0));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::drag());
        ui.painter().rect_filled(rect, 9.0, ui_theme::CANVAS);
        if self.view_2d {
            if let Some(texture) = &self.texture {
                let side = rect.width().min(rect.height());
                let image_rect = egui::Rect::from_center_size(rect.center(),
                    egui::vec2(side, side));
                ui.painter().image(texture.id(), image_rect,
                    egui::Rect::from_min_max(egui::Pos2::ZERO,
                        egui::Pos2::new(1.0, 1.0)),
                    egui::Color32::WHITE);
            }
        } else {
            if response.dragged() {
                let motion = response.drag_motion();
                if motion.length_sq() > 0.0 {
                    self.yaw += motion.x * 0.008;
                    self.pitch = (self.pitch - motion.y * 0.008).clamp(0.2, 1.5);
                    self.gpu_dirty = true;
                }
            }
            if response.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll.abs() > 0.1 {
                    self.distance = (self.distance * (1.0 - scroll * 0.001))
                        .clamp(1.5, 12.0);
                    self.gpu_dirty = true;
                }
            }
            let matrix = gpu::view_matrix(
                self.yaw, self.pitch, self.distance,
                rect.width() / rect.height().max(1.0)
            );
            ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(
                rect, gpu::TerrainCallback { camera: matrix }
            ));
        }
        ui.painter().rect_stroke(rect, 9.0, egui::Stroke::new(
            1.0, ui_theme::BORDER
        ), egui::StrokeKind::Inside);
        ui.painter().text(rect.left_top() + egui::vec2(16.0, 15.0),
            egui::Align2::LEFT_TOP,
            if self.view_2d { "SELECTED GRAPH MAP  •  TOP DOWN" }
            else { "PERSPECTIVE  •  REALTIME GPU" },
            egui::FontId::monospace(11.0), ui_theme::TEXT);
        ui.painter().text(rect.left_bottom() + egui::vec2(16.0, -17.0),
            egui::Align2::LEFT_BOTTOM,
            if self.view_2d { "Selected map available as 16-bit PNG in Export" }
            else { "Drag to orbit   •   Scroll to zoom" },
            egui::FontId::proportional(12.0), ui_theme::TEXT);
    }
}

impl eframe::App for GaiaApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Standard shortcuts. Open never modifies the running project on failure.
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
            self.save_project();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
            self.open_project();
        }

        egui::Panel::top("gaia_toolbar")
            .frame(ui_theme::toolbar())
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status")
            .exact_size(34.0)
            .frame(egui::Frame::new()
                .fill(ui_theme::PANEL)
                .inner_margin(5)
                .stroke(egui::Stroke::new(1.0, ui_theme::BORDER)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let status_color = if self.status.contains("failed")
                        || self.status.contains("error") {
                        ui_theme::RED
                    } else {
                        ui_theme::ACCENT
                    };
                    ui.colored_label(status_color, "●");
                    ui.label(egui::RichText::new(&self.status)
                        .size(12.0).color(ui_theme::MUTED));
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new("GAIA 0.1  ·  WGPU")
                                .monospace().size(11.0).color(ui_theme::MUTED));
                        }
                    );
                });
            });
        egui::Panel::left("library")
            .default_size(200.0).min_size(170.0).max_size(270.0)
            .resizable(true).frame(ui_theme::panel())
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.library(ui));
            });
        egui::Panel::right("inspector")
            .default_size(295.0).min_size(255.0).max_size(420.0)
            .resizable(true).frame(ui_theme::panel())
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.inspector(ui));
            });
        egui::Panel::bottom("graph")
            .resizable(true).default_size(270.0).min_size(135.0)
            .frame(ui_theme::panel())
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("GRAPH").size(12.0)
                        .strong().color(ui_theme::TEXT));
                    ui.label(egui::RichText::new(
                        format!("{} NODES  ·  OUTPUT #{:03}",
                            self.project.graph.nodes.len(), self.project.graph.output)
                    ).monospace().size(10.0).color(ui_theme::MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("Select · drag · edit inputs in Properties")
                            .size(11.0).color(ui_theme::MUTED));
                    });
                });
                ui.add_space(8.0);
                egui::ScrollArea::both().auto_shrink([false, false])
                    .show(ui, |ui| self.graph_canvas(ui));
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui_theme::BG).inner_margin(12))
            .show(ui, |ui| self.viewport(ui));

        if self.graph_dirty {
            self.rebuild(&ctx);
        }
        if self.gpu_dirty {
            if let Some(render_state) = frame.wgpu_render_state() {
                gpu::upload(render_state, &self.field,
                    gpu::eye_position(self.yaw, self.pitch, self.distance));
            }
            self.gpu_dirty = false;
        }
    }
}

fn node_accent(kind: &NodeKind) -> egui::Color32 {
    match kind {
        NodeKind::Mountain { .. } | NodeKind::Noise { .. } => ui_theme::ACCENT,
        NodeKind::Erosion { .. } | NodeKind::Terrace { .. } | NodeKind::Blur { .. } => ui_theme::BLUE,
        NodeKind::Blend { .. } | NodeKind::MaskedBlend { .. } => ui_theme::AMBER,
        NodeKind::Slope => ui_theme::BLUE,
    }
}

fn height_image(field: &HeightField) -> egui::ColorImage {
    let mut data = Vec::with_capacity(field.values.len() * 4);
    for &height in &field.values {
        let value = (height.clamp(0.0, 1.0) * 255.0) as u8;
        data.extend_from_slice(&[value, value, value, 255]);
    }
    egui::ColorImage::from_rgba_unmultiplied([field.size, field.size], &data)
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([1024.0, 680.0]),
        ..Default::default()
    };
    eframe::run_native("Gaia • Terrain Editor", options, Box::new(|cc| Ok(Box::new(GaiaApp::new(cc)))))
}
