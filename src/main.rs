mod gpu;

use eframe::egui;
use gaia::terrain::{export_obj, export_png16, HeightField, NodeKind, Project};
use std::path::Path;

struct GaiaApp {
    project: Project,
    field: HeightField,
    texture: Option<egui::TextureHandle>,
    selected: u64,
    filename: String,
    status: String,
    yaw: f32,
    pitch: f32,
    distance: f32,
    view_2d: bool,
    graph_dirty: bool,
    gpu_dirty: bool,
}

impl GaiaApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let project = Project::default();
        let field = project.graph.evaluate(project.resolution)
            .expect("default terrain project must evaluate");
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
            texture,
            filename: "terrain.gaia.json".to_string(),
            status: "Ready. Drag on the 3D preview to orbit; scroll to zoom.".to_string(),
            yaw,
            pitch,
            distance,
            view_2d: false,
            graph_dirty: false,
            gpu_dirty: false,
        }
    }

    fn rebuild(&mut self, ctx: &egui::Context) {
        match self.project.graph.evaluate(self.project.resolution) {
            Ok(field) => {
                self.field = field;
                let image = height_image(&self.field);
                if let Some(texture) = self.texture.as_mut() {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    self.texture = Some(ctx.load_texture(
                        "Gaia heightfield", image, egui::TextureOptions::LINEAR,
                    ));
                }
                self.gpu_dirty = true;
                self.status = format!(
                    "Built {} × {} terrain from node #{}",
                    self.field.size, self.field.size, self.project.graph.output,
                );
            }
            Err(err) => {
                self.status = format!("Graph error: {err}. Showing last valid preview.");
            }
        }
        self.graph_dirty = false;
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("GAIA");
            ui.separator();
            ui.label("Project file");
            ui.add(egui::TextEdit::singleline(&mut self.filename).desired_width(185.0));
            if ui.button("Save").clicked() {
                self.status = match self.project.save(Path::new(&self.filename)) {
                    Ok(()) => format!("Saved {}", self.filename),
                    Err(e) => format!("Save failed: {e}"),
                };
            }
            if ui.button("Open").clicked() {
                match Project::load(Path::new(&self.filename)) {
                    Ok(project) => {
                        self.selected = project.graph.output;
                        self.project = project;
                        self.graph_dirty = true;
                    }
                    Err(e) => self.status = format!("Open failed: {e}"),
                }
            }
            if ui.button("Export 16-bit PNG").clicked() {
                let path = Path::new(&self.filename).with_extension("png");
                self.status = match export_png16(&self.field, &path) {
                    Ok(()) => format!("Exported {}", path.display()),
                    Err(e) => format!("PNG export failed: {e}"),
                };
            }
            if ui.button("Export OBJ").clicked() {
                let path = Path::new(&self.filename).with_extension("obj");
                self.status = match export_obj(&self.field, &path) {
                    Ok(()) => format!("Exported {}", path.display()),
                    Err(e) => format!("OBJ export failed: {e}"),
                };
            }
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Terrain");
        ui.horizontal(|ui| {
            ui.label("Resolution");
            let before = self.project.resolution;
            egui::ComboBox::from_id_salt("resolution")
                .selected_text(format!("{}²", self.project.resolution))
                .show_ui(ui, |ui| {
                    for size in [64, 128, 256, 512] {
                        ui.selectable_value(&mut self.project.resolution, size, format!("{size}²"));
                    }
                });
            if before != self.project.resolution {
                self.graph_dirty = true;
            }
        });
        ui.separator();
        ui.heading("Add node");
        ui.horizontal_wrapped(|ui| {
            for (name, kind) in [
                ("Mountain", NodeKind::Mountain { seed: 42, frequency: 4.0, height: 1.0 }),
                ("Noise", NodeKind::Noise { seed: 7, frequency: 6.0, amplitude: 0.4 }),
                ("Erosion", NodeKind::Erosion { iterations: 24, talus: 0.008, strength: 0.28 }),
                ("Terrace", NodeKind::Terrace { steps: 12, softness: 0.8 }),
                ("Blur", NodeKind::Blur { radius: 2 }),
                ("Blend", NodeKind::Blend { mix: 0.5 }),
            ] {
                if ui.button(name).clicked() {
                    self.selected = self.project.graph.add(kind);
                    self.graph_dirty = true;
                }
            }
        });
        ui.separator();
        ui.heading("Node inspector");
        let options: Vec<(u64, String)> = self.project.graph.nodes.iter().map(|n| {
            (n.id, format!("#{} {}", n.id, n.kind.label()))
        }).collect();
        let mut changed = false;
        let mut set_output = false;
        if let Some(node) = self.project.graph.nodes.iter_mut().find(|n| n.id == self.selected) {
            ui.strong(format!("#{} {}", node.id, node.kind.label()));
            if ui.button("Use as graph output").clicked() {
                set_output = true;
            }
            for (index, input) in node.inputs.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("Input {}", index + 1));
                    let current_name = options.iter()
                        .find(|(id, _)| *id == *input)
                        .map(|(_, name)| name.as_str())
                        .unwrap_or("Missing");
                    egui::ComboBox::from_id_salt(("input", node.id, index))
                        .selected_text(current_name)
                        .show_ui(ui, |ui| {
                            for (id, name) in &options {
                                if *id != node.id {
                                    changed |= ui.selectable_value(input, *id, name).changed();
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
                NodeKind::Blend { mix } => {
                    changed |= ui.add(egui::Slider::new(mix, 0.0..=1.0).text("Mix")).changed();
                }
            }
        } else {
            ui.label("Click a node in the graph to edit it.");
        }
        if set_output {
            self.project.graph.output = self.selected;
            changed = true;
        }
        if changed {
            self.graph_dirty = true;
        }
        ui.separator();
        ui.small("Drag nodes to arrange the graph. Connect them using the input selectors above.");
        ui.small("Graph cycles and missing connections are checked before every rebuild.");
    }

    fn graph_canvas(&mut self, ui: &mut egui::Ui) {
        let extent = egui::vec2(ui.available_width().max(900.0), 370.0);
        let (rect, _) = ui.allocate_exact_size(extent, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, egui::Color32::from_rgb(19, 22, 30));
        let nodes = self.project.graph.nodes.clone();
        for node in &nodes {
            for (i, source) in node.inputs.iter().enumerate() {
                if let Some(parent) = nodes.iter().find(|n| n.id == *source) {
                    let start = rect.left_top() + egui::vec2(parent.position[0] + 182.0, parent.position[1] + 28.0);
                    let end = rect.left_top() + egui::vec2(node.position[0], node.position[1] + 22.0 + i as f32 * 13.0);
                    let mut points = Vec::with_capacity(21);
                    for k in 0..=20 {
                        let t = k as f32 / 20.0;
                        let a = start;
                        let b = start + egui::vec2(65.0, 0.0);
                        let c = end - egui::vec2(65.0, 0.0);
                        let d = end;
                        let s = 1.0 - t;
                        points.push(egui::pos2(
                            a.x*s*s*s + 3.0*b.x*s*s*t + 3.0*c.x*s*t*t + d.x*t*t*t,
                            a.y*s*s*s + 3.0*b.y*s*s*t + 3.0*c.y*s*t*t + d.y*t*t*t,
                        ));
                    }
                    painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, egui::Color32::from_rgb(118, 169, 167))));
                }
            }
        }
        for node in &mut self.project.graph.nodes {
            let top_left = rect.left_top() + egui::vec2(node.position[0], node.position[1]);
            let node_rect = egui::Rect::from_min_size(top_left, egui::vec2(182.0, 58.0));
            let response = ui.interact(node_rect, ui.scope_id().with(("node", node.id)), egui::Sense::click_and_drag());
            if response.clicked() {
                self.selected = node.id;
            }
            if response.dragged() {
                let delta = response.drag_delta();
                node.position[0] = (node.position[0] + delta.x).max(0.0);
                node.position[1] = (node.position[1] + delta.y).max(0.0);
                self.selected = node.id;
            }
            let selected = self.selected == node.id;
            painter.rect_filled(node_rect, 7.0, if selected {
                egui::Color32::from_rgb(49, 75, 78)
            } else {
                egui::Color32::from_rgb(37, 43, 52)
            });
            painter.rect_stroke(node_rect, 7.0,
                egui::Stroke::new(if selected { 2.0 } else { 1.0 },
                    if selected { egui::Color32::LIGHT_GREEN } else { egui::Color32::from_gray(85) }),
                egui::StrokeKind::Outside,
            );
            let label = format!("#{} {}", node.id, node.kind.label());
            painter.text(node_rect.left_top() + egui::vec2(12.0, 9.0),
                egui::Align2::LEFT_TOP, label, egui::FontId::proportional(15.0), egui::Color32::WHITE);
            painter.text(node_rect.left_bottom() + egui::vec2(12.0, -20.0),
                egui::Align2::LEFT_TOP,
                if node.id == self.project.graph.output { "OUTPUT" } else { "Terrain → Terrain" },
                egui::FontId::monospace(11.0), egui::Color32::from_rgb(143, 187, 179));
        }
    }

    fn viewport(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view_2d, false, "3D terrain");
            ui.selectable_value(&mut self.view_2d, true, "Heightmap");
            ui.separator();
            ui.label(format!("{} × {} samples", self.field.size, self.field.size));
            if ui.button("Reset view").clicked() {
                self.yaw = 0.85;
                self.pitch = 0.72;
                self.distance = 3.7;
                self.gpu_dirty = true;
            }
        });
        let size = ui.available_size().max(egui::vec2(100.0, 100.0));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::drag());
        ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(45, 57, 70));
        if self.view_2d {
            if let Some(texture) = &self.texture {
                ui.painter().image(texture.id(), rect,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                    egui::Color32::WHITE);
            }
        } else {
            if response.dragged() {
                let motion = response.drag_delta();
                if motion.length_sq() > 0.0 {
                    self.yaw += motion.x * 0.008;
                    self.pitch = (self.pitch - motion.y * 0.008).clamp(0.2, 1.5);
                    self.gpu_dirty = true;
                }
            }
            if response.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll.abs() > 0.1 {
                    self.distance = (self.distance * (1.0 - scroll * 0.001)).clamp(1.5, 12.0);
                    self.gpu_dirty = true;
                }
            }
            let matrix = gpu::view_matrix(
                self.yaw, self.pitch, self.distance, rect.width() / rect.height().max(1.0)
            );
            ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(
                rect, gpu::TerrainCallback { camera: matrix },
            ));
        }
    }
}

impl eframe::App for GaiaApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("gaia_toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.small(&self.status);
                ui.separator();
                ui.small("Gaia 0.1 | Rust + wgpu");
            });
        });
        egui::Panel::left("inspector").default_size(275.0).resizable(true).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.inspector(ui));
        });
        egui::Panel::bottom("graph")
            .resizable(true).default_size(270.0).min_size(120.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("NODE GRAPH");
                    ui.label("Click to select • drag to move");
                });
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| self.graph_canvas(ui));
            });
        egui::CentralPanel::default().show(ui, |ui| self.viewport(ui));
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
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("Gaia • Terrain Editor", options, Box::new(|cc| Ok(Box::new(GaiaApp::new(cc)))))
}
