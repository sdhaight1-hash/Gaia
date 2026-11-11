mod gpu;
mod material_texture;
mod ui_theme;
mod history;

use eframe::egui;
use gaia::terrain::{export_obj, export_png16, HeightField, MaterialPalette, NodeKind, Node, Project};
use gaia::pipeline::{PortRef, PortType};
use gaia::worker::GraphWorker;
use gaia::hydrology::HydraulicParameters;
use gaia::color::{ColorField, export_rgb8, export_splat_rgba8};
use gaia::tiles::{tile_layout, tile_height, tile_color};
use serde_json::json;
use std::path::{Path, PathBuf};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult};
use history::EditHistory;

#[derive(Debug, PartialEq, Eq)]
enum DroppedDocument {
    Project,
    Heightmap,
    Unsupported,
}

fn classify_drop(path: &Path) -> DroppedDocument {
    let filename = path.file_name().and_then(|name| name.to_str())
        .unwrap_or_default();
    if filename.to_ascii_lowercase().ends_with(".gaia.json") {
        DroppedDocument::Project
    } else if path.extension().and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("png")) {
        DroppedDocument::Heightmap
    } else {
        DroppedDocument::Unsupported
    }
}

/// Use the current project's directory when opening native dialogs. A bare
/// relative filename has no parent directory to preselect.
fn project_dialog_directory(path: &Path) -> Option<&Path> {
    path.parent().filter(|parent|
        !parent.as_os_str().is_empty() && parent.is_dir())
}

/// The scroll content must encompass every node, not just the first few rows.
/// The old fixed 360px canvas stranded nodes below row 2 outside its layout.
fn graph_canvas_extent(nodes: &[Node], viewport_width: f32) -> egui::Vec2 {
    let mut width = viewport_width.max(920.0);
    let mut height = 360.0_f32;
    for node in nodes {
        let ports = node.inputs.len().max(node.kind.output_ports().len()).max(1);
        let node_height = 74.0_f32.max(47.0 + ports as f32 * 17.0);
        width = width.max(node.position[0] + 194.0 + 80.0);
        height = height.max(node.position[1] + node_height + 80.0);
    }
    egui::vec2(width, height)
}

/// Explicitly build a second, genuinely different Color input for a mixer.
/// Previously, new mixers often received the same Colorize port twice, so
/// changing the Mix slider was mathematically guaranteed to do nothing.
fn add_contrasting_colorize(project: &mut Project, mixer_id: u64) -> Result<u64, String> {
    let mixer = project.graph.get(mixer_id)
        .ok_or_else(|| format!("Unknown node #{mixer_id}"))?;
    if !matches!(mixer.kind, NodeKind::ColorMixer { .. })
        || mixer.inputs.len() < 2 || mixer.inputs[0] != mixer.inputs[1]
    {
        return Err("Only a Color Mixer using the same source twice needs a second palette".into());
    }
    let id = project.graph.add(NodeKind::Colorize {
        low: [0.28, 0.27, 0.26],
        high: [0.88, 0.84, 0.78],
        midpoint: 0.5,
        contrast: 1.0,
    });
    project.graph.connect(mixer_id, 1, PortRef::new(id, "color"))?;
    Ok(id)
}

struct GaiaApp {
    project: Project,
    field: HeightField,
    material_color: Option<ColorField>,
    material_splat: Option<ColorField>,
    worker: GraphWorker,
    texture: Option<egui::TextureHandle>,
    preview_source: PortRef,
    selected: u64,
    wire_start: Option<PortRef>,
    filename: String,
    status: String,
    yaw: f32,
    pitch: f32,
    distance: f32,
    sun_azimuth: f32,
    sun_elevation: f32,
    sun_strength: f32,
    view_2d: bool,
    graph_dirty: bool,
    gpu_dirty: bool,
    unsaved: bool,
    history: EditHistory,
    skip_history_record: bool,
    focus_graph_selection: bool,
}

impl GaiaApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ui_theme::apply(&cc.egui_ctx);
        let project = Project::default();
        let field = project.graph.evaluate(project.resolution)
            .expect("default terrain project must evaluate");
        let worker = if let Some(state) = &cc.wgpu_render_state {
            GraphWorker::with_gpu(state.device.clone(), state.queue.clone())
        } else {
            GraphWorker::new()
        };
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
            wire_start: None,
            project,
            field,
            material_color: None,
            material_splat: None,
            worker,
            texture,
            preview_source,
            filename: "terrain.gaia.json".to_string(),
            status: "Ready. Drag on the 3D preview to orbit; scroll to zoom.".to_string(),
            yaw,
            pitch,
            distance,
            sun_azimuth: 55.0,
            sun_elevation: 60.0,
            sun_strength: 2.0,
            view_2d: false,
            graph_dirty: false,
            gpu_dirty: false,
            unsaved: false,
            history: EditHistory::default(),
            skip_history_record: false,
            focus_graph_selection: false,
        }
    }

    fn rebuild(&mut self, _ctx: &egui::Context) {
        match self.worker.submit(self.project.clone(), self.preview_source.clone()) {
            Ok(_) => {
                self.status = format!(
                    "Building {}² terrain  ·  preview #{}:{}",
                    self.project.resolution, self.preview_source.node, self.preview_source.port
                );
            }
            Err(error) => self.status = format!("Build failed: {error}"),
        }
        self.graph_dirty = false;
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.worker.poll() {
            match result.data {
                Ok(result) => {
                    self.field = result.height;
                    let image = if let Some(color) = &result.preview_color {
                        if result.preview_kind == PortType::Splat {
                            splat_preview_image(color)
                        } else {
                            color_image(color)
                        }
                    } else {
                        height_image(&result.preview)
                    };
                    // RGB Color nodes paint vertices, whereas typed Splat
                    // nodes select a four-layer dielectric PBR material.
                    self.material_splat = if result.preview_kind == PortType::Splat {
                        result.preview_color.clone()
                    } else { None };
                    self.material_color = if result.preview_kind == PortType::Color {
                        result.preview_color
                    } else { None };
                    if let Some(texture) = self.texture.as_mut() {
                        texture.set(image, egui::TextureOptions::LINEAR);
                    } else {
                        self.texture = Some(ctx.load_texture(
                            "Gaia selected graph output", image, egui::TextureOptions::LINEAR
                        ));
                    }
                    self.gpu_dirty = true;
                    self.status = format!(
                        "Ready · {}² · {} · {} computed / {} reused",
                        self.field.size,
                        result.preview_kind.label(),
                        result.computed,
                        result.reused,
                    );
                }
                Err(error) => {
                    self.status = format!("Graph error: {error} · showing last valid terrain");
                }
            }
        }
        if self.worker.is_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn refresh_preview(&mut self, _ctx: &egui::Context) {
        self.graph_dirty = true;
    }

    fn export_selected_map(&mut self) {
        let output = self.project.graph.evaluate_port(
            self.project.resolution, &self.preview_source);
        match output {
            Ok(value) => {
                let path = Path::new(&self.filename)
                    .with_extension(format!("{}.png", self.preview_source.port));
                self.status = match if value.kind == PortType::Splat {
                    value.color.as_ref().ok_or_else(|| "Missing splat weights".to_owned())
                        .and_then(|data| export_splat_rgba8(data, &path))
                } else if let Some(color) = &value.color {
                    export_rgb8(color, &path)
                } else {
                    export_png16(&value.data, &path)
                } {
                    Ok(()) => format!("Exported selected map to {}", path.display()),
                    Err(error) => format!("Map export failed: {error}"),
                };
            }
            Err(error) => self.status = format!("Map export failed: {error}"),
        }
    }

    fn export_selected_tiles(&mut self, count: usize) {
        let operation = (|| -> Result<String, String> {
            let output = self.project.graph.evaluate_port(
                self.project.resolution, &self.preview_source)?;
            let layout = tile_layout(self.project.resolution, count)?;
            let suffix = format!("node{}-{}", self.preview_source.node, self.preview_source.port);
            let base = Path::new(&self.filename);
            let folder = base.with_extension(format!("{suffix}.tiles"));
            let manifest = base.with_extension(format!("{suffix}.tiles.json"));
            std::fs::create_dir_all(&folder)
                .map_err(|e| format!("Tile output folder: {e}"))?;
            for y in 0..count {
                for x in 0..count {
                    let path = folder.join(format!("x{x:02}_y{y:02}.png"));
                    if matches!(output.kind, PortType::Color | PortType::Splat) {
                        let color = output.color.as_ref()
                            .ok_or("Selected color/splat port has no RGBA data")?;
                        let tile = tile_color(color, layout, x, y)?;
                        if output.kind == PortType::Splat {
                            export_splat_rgba8(&tile, &path)?;
                        } else {
                            export_rgb8(&tile, &path)?;
                        }
                    } else {
                        export_png16(&tile_height(&output.data, layout, x, y)?, &path)?;
                    }
                }
            }
            let data = json!({
                "tiles_per_axis": count,
                "tile_count": count * count,
                "pixels_per_tile": layout.pixels_per_tile,
                "stitched_pixels": layout.stitched_pixels,
                "overlap_pixels": 1,
                "source_resolution": self.project.resolution,
                "output_node": self.preview_source.node,
                "output_port": self.preview_source.port,
                "format": match output.kind {
                    PortType::Splat => "RGBA8",
                    PortType::Color => "RGB8",
                    _ => "Grayscale16",
                },
                "folder": folder.display().to_string(),
                "origin": "top-left, y increases downward",
                "sampling": "global bilinear sampling with identical adjacent edges",
                "execution": "full graph evaluated once, then split into overlapping tiles",
            });
            std::fs::write(&manifest,
                serde_json::to_vec_pretty(&data).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            Ok(format!("Exported {count}×{count} selected-output tiles into {}",
                folder.display()))
        })();
        self.status = operation.unwrap_or_else(|error|
            format!("Tiled export failed: {error}"));
    }

    fn project_file_dialog(&self, title: &str) -> FileDialog {
        let mut dialog = FileDialog::new()
            .set_title(title)
            .add_filter("Gaia project (JSON)", &["json"]);
        if let Some(directory) = project_dialog_directory(Path::new(&self.filename)) {
            dialog = dialog.set_directory(directory);
        }
        dialog
    }

    fn confirm_replace_project(&self) -> bool {
        !self.unsaved || MessageDialog::new()
            .set_title("Unsaved Gaia changes")
            .set_description("Discard unsaved changes and open a different project?")
            .set_buttons(MessageButtons::YesNo)
            .show() == MessageDialogResult::Yes
    }

    fn save_project_to(&mut self, path: &Path) {
        self.status = match self.project.save(path) {
            Ok(()) => {
                // A failed Save As must never retarget future Ctrl+S actions.
                self.filename = path.to_string_lossy().into_owned();
                self.unsaved = false;
                format!("Saved {}", path.display())
            }
            Err(error) => format!("Save failed: {error}"),
        };
    }

    fn save_project(&mut self) {
        let path = PathBuf::from(&self.filename);
        self.save_project_to(&path);
    }

    fn save_project_as_dialog(&mut self) {
        let name = Path::new(&self.filename).file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("terrain.gaia.json")
            .to_owned();
        // The initial filename includes .gaia.json. Respect any custom path
        // selected in the OS dialog instead of silently overwriting a
        // different, extension-rewritten file after the overwrite prompt.
        if let Some(path) = self.project_file_dialog("Save Gaia project as")
            .set_file_name(name).save_file()
        {
            self.save_project_to(&path);
        }
    }

    fn open_project_from(&mut self, path: &Path) {
        match Project::load(path) {
            Ok(project) => {
                // Commit all editor state only after successful loading,
                // including the filename (also used by future exports).
                self.filename = path.to_string_lossy().into_owned();
                self.selected = project.graph.output;
                self.preview_source = PortRef::height(project.graph.output);
                self.project = project;
                self.focus_graph_selection = true;
                self.history.clear();
                self.skip_history_record = true;
                self.graph_dirty = true;
                self.unsaved = false;
                self.status = format!("Opened {}", path.display());
            }
            Err(error) => self.status = format!("Open failed: {error}"),
        }
    }

    fn open_project(&mut self) {
        if self.confirm_replace_project() {
            let path = PathBuf::from(&self.filename);
            self.open_project_from(&path);
        }
    }

    fn open_project_dialog(&mut self) {
        if !self.confirm_replace_project() {
            return;
        }
        if let Some(path) = self.project_file_dialog("Open Gaia project").pick_file() {
            self.open_project_from(&path);
        }
    }

    fn import_heightmap_dialog(&mut self) {
        let mut dialog = FileDialog::new()
            .set_title("Import grayscale PNG heightmap")
            .add_filter("PNG heightmaps", &["png"]);
        if let Some(directory) = project_dialog_directory(Path::new(&self.filename)) {
            dialog = dialog.set_directory(directory);
        }
        if let Some(path) = dialog.pick_file() {
            self.add_node(NodeKind::ImportHeight {
                path: path.to_string_lossy().into_owned(), scale: 1.0,
            });
            self.status = format!("Added heightmap node: {}", path.display());
        }
    }

    fn after_history_restore(&mut self, label: &str) {
        if self.project.graph.get(self.selected).is_none() {
            self.selected = self.project.graph.output;
        }
        let preview_still_exists = self.project.graph.get(self.preview_source.node)
            .is_some_and(|node| node.kind.output_ports().iter()
                .any(|(port, _)| *port == self.preview_source.port));
        if !preview_still_exists {
            self.preview_source = PortRef::height(self.project.graph.output);
        }
        self.wire_start = None;
        self.graph_dirty = true;
        self.unsaved = true;
        self.status = label.to_owned();
    }

    fn undo_edit(&mut self) {
        if self.history.undo(&mut self.project) {
            self.after_history_restore("Undid last graph edit");
        }
    }

    fn redo_edit(&mut self) {
        if self.history.redo(&mut self.project) {
            self.after_history_restore("Redid graph edit");
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

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        for dropped in ctx.input(|input| input.raw.dropped_files.clone()) {
            let path = dropped.path().to_path_buf();
            match classify_drop(&path) {
                DroppedDocument::Project => {
                    if self.confirm_replace_project() {
                        self.open_project_from(&path);
                    }
                }
                DroppedDocument::Heightmap => {
                    self.add_node(NodeKind::ImportHeight {
                        path: path.to_string_lossy().into_owned(),
                        scale: 1.0,
                    });
                    self.status = format!("Added heightmap node: {}", path.display());
                }
                DroppedDocument::Unsupported => {
                    self.status = format!(
                        "Unsupported file: {} (use PNG or .gaia.json)",
                        path.display()
                    );
                }
            }
        }
    }

    fn add_node(&mut self, kind: NodeKind) {
        let is_mask = matches!(kind,
            NodeKind::Slope | NodeKind::Curvature | NodeKind::Aspect { .. }
            | NodeKind::Drainage { .. } | NodeKind::ElevationMask { .. }
            | NodeKind::MaskLevels { .. } | NodeKind::Colorize { .. } | NodeKind::ColorMixer { .. }
            | NodeKind::Normals { .. } | NodeKind::AmbientOcclusion { .. }
            | NodeKind::Sunlight { .. } | NodeKind::MaterialMasks { .. });
        let source = self.preview_source.clone();
        self.selected = self.project.graph.add_from(kind, &source);
        self.focus_graph_selection = true;
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
        // Windows egui's default font lacks several decorative Unicode glyphs:
        // the video showed a row of missing-glyph squares. Labels need no icons.
        let name = name.split_once("  ").map_or(name, |(_, label)| label);
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
        ui.label(egui::RichText::new(
            "Select a graph output first. New compatible nodes connect to it automatically."
        ).size(12.0).color(ui_theme::MUTED));
        ui.add_space(20.0);
        ui_theme::section(ui, "GENERATORS");
        self.palette_node(ui, "▲  Mountain", "Create a ridged mountain mass",
            NodeKind::Mountain { seed: 42, frequency: 4.0, height: 1.0 });
        self.palette_node(ui, "◉  Island", "Shape a landmass with an ocean-level coastline",
            NodeKind::Island { seed: 41, radius: 0.65, height: 1.0, roughness: 0.7 });
        self.palette_node(ui, "▲  Volcano", "Generate a volcanic cone, caldera and warped flanks",
            NodeKind::Volcano {
                seed: 59, radius: 0.36, height: 0.95, mouth: 0.28,
                bulk: 0.55, roughness: 0.45, center_x: 0.5, center_y: 0.5,
            });
        self.palette_node(ui, "◈  Plates",
            "Seeded moving plate regions, uplift, rifts and geological masks",
            NodeKind::Plates {
                seed: 73, count: 12, continental: 0.55, uplift: 0.9, warp: 0.55,
            });
        self.palette_node(ui, "◈  Noise", "Layer smooth fractal noise",
            NodeKind::Noise { seed: 7, frequency: 6.0, amplitude: 0.4 });
        self.palette_node(ui, "⇧  Import Heightmap", "Read an 8- or 16-bit grayscale PNG",
            NodeKind::ImportHeight { path: "heightmap.png".to_owned(), scale: 1.0 });
        self.palette_node(ui, "⛰  Range", "Warped mountain ridges and chains",
            NodeKind::Range { seed: 31, height: 1.0, roughness: 0.65 });
        self.palette_node(ui, "◎  Crater", "Impact bowl with elevated rim",
            NodeKind::Crater { seed: 12, radius: 0.28, depth: 0.65, rim: 0.75 });
        self.palette_node(ui, "≈  Dunes", "Wind-aligned rippled landforms",
            NodeKind::Dunes { seed: 9, frequency: 11.0, amplitude: 0.5 });
        self.palette_node(ui, "⌁  Canyon", "Meandering canyon incision",
            NodeKind::Canyon { seed: 4, width: 0.07, depth: 0.8 });
        ui.add_space(16.0);
        ui_theme::section(ui, "TERRAIN OPERATORS");
        self.palette_node(ui, "≋  Erosion", "Mass-conserving thermal talus transport",
            NodeKind::Erosion { iterations: 24, talus: 0.008, strength: 0.28 });
        self.palette_node(ui, "≈  Hydraulic", "Rain, water flow and conservative sediment transport",
            NodeKind::Hydraulic { params: HydraulicParameters::default() });
        self.palette_node(ui, "▤  Terrace", "Add stepped elevation bands",
            NodeKind::Terrace { steps: 12, softness: 0.8 });
        self.palette_node(ui, "≋  Stratify", "Add warped, tilted, locally broken sedimentary layers",
            NodeKind::Stratify {
                seed: 17, spacing: 0.08, intensity: 0.8,
                warp: 0.55, tilt: 0.15, direction: 45.0,
            });
        self.palette_node(ui, "◫  Adjust", "Multiply, bulk, autolevel, clamp or invert heights",
            NodeKind::Adjust {
                multiply: 1.0, shaper: 0.0,
                clamp_min: 0.0, clamp_max: 4.0,
                autolevel: false, invert: false,
            });
        self.palette_node(ui, "◫  Equalize",
            "Redistribute terrain elevation histogram, retaining min/max height",
            NodeKind::Equalize { factor: 0.75 });
        self.palette_node(ui, "◎  Blur", "Soften local terrain detail",
            NodeKind::Blur { radius: 2 });
        self.palette_node(ui, "◢  Slope Mask", "Derive a typed slope mask from terrain",
            NodeKind::Slope);
        self.palette_node(ui, "◌  Curvature", "Local ridges and concave bowls",
            NodeKind::Curvature);
        self.palette_node(ui, "◐  Ambient Occlusion",
            "Directional horizon sampling for a terrain occlusion mask",
            NodeKind::AmbientOcclusion { radius: 12, strength: 0.8 });
        self.palette_node(ui, "☼  Sunlight",
            "Compass-facing Lambert lighting with terrain cast shadows",
            NodeKind::Sunlight { azimuth: 45.0, elevation: 38.0, distance: 24 });
        self.palette_node(ui, "◐  Aspect Mask", "Select slopes facing a compass direction",
            NodeKind::Aspect { azimuth: 0.0, half_width: 65.0, min_slope: 0.02 });
        self.palette_node(ui, "⌁  Drainage", "Derive upstream drainage and channel masks",
            NodeKind::Drainage { min_catchment_fraction: 0.01 });
        self.palette_node(ui, "≈  Depression Fill",
            "Condition local pits into outlet-connected drainage; provide fill-depth map",
            NodeKind::DepressionFill { grade: 0.0001 });
        self.palette_node(ui, "≈  Rivers",
            "Priority-flood connected D8 river corridors with downcutting and wear masks",
            NodeKind::Rivers {
                catchment_fraction: 0.002, width: 3.0,
                depth: 0.045, grade: 0.0001,
            });
        self.palette_node(ui, "≈  Lake",
            "Fill enclosed basins to a shared water elevation and derive water/coverage maps",
            NodeKind::Lake {
                fill_fraction: 1.0, minimum_area: 4, minimum_depth: 0.01,
            });
        self.palette_node(ui, "≈  Sea",
            "Sea-level inundation, coastlines, beach mask and surface conditioning",
            NodeKind::Sea { level: 0.27, shore_width: 6, erosion: 0.3 });
        self.palette_node(ui, "*  Snow",
            "Accumulate altitude-dependent snow, melt and downhill settling",
            NodeKind::Snow {
                seed: 24, snowline: 0.32, intensity: 0.12,
                melt: 0.12, adherence: 0.55, settle: 6,
            });
        self.palette_node(ui, "◫  Elevation Mask", "Select a height range with soft edges",
            NodeKind::ElevationMask { low: 0.25, high: 0.7, feather: 0.07 });
        self.palette_node(ui, "◩  Mask Levels", "Remap mask black/white points, gamma and invert",
            NodeKind::MaskLevels { black: 0.0, white: 1.0, gamma: 1.0, invert: false });
        self.palette_node(ui, "↝  Warp", "Distort a terrain using two noise fields",
            NodeKind::Warp { seed: 55, frequency: 4.0, amount: 0.08 });
        ui.add_space(16.0);
        ui_theme::section(ui, "COMPOSITING");
        self.palette_node(ui, "◇  Blend", "Mix the heights from two input nodes",
            NodeKind::Blend { mix: 0.5 });
        self.palette_node(ui, "◈  Masked Blend", "Blend two heightfields using a mask port",
            NodeKind::MaskedBlend { mix: 1.0 });
        ui.add_space(16.0);
        ui_theme::section(ui, "SURFACE COLOR");
        self.palette_node(ui, "◫  Material Masks",
            "Normalized sand, soil, rock and snow coverage from elevation/slope",
            NodeKind::MaterialMasks {
                sea_level: 0.25, beach_width: 0.08,
                rock_slope: 0.32, rock_blend: 0.10,
                snowline: 0.80, snow_blend: 0.12,
                palette: MaterialPalette::default(),
            });
        self.palette_node(ui, "N  Normals", "Derive a game-engine RGB normal map from terrain",
            NodeKind::Normals { strength: 1.0, flip_green: false });
        self.palette_node(ui, "◉  Colorize", "Map low and high elevations to true RGBA colors",
            NodeKind::Colorize {
                low: [0.13, 0.27, 0.19],
                high: [0.85, 0.85, 0.78],
                midpoint: 0.5, contrast: 1.0,
            });
        self.palette_node(ui, "◐  Color Mixer", "Mix two RGBA fields through a mask",
            NodeKind::ColorMixer { mix: 0.65 });
        ui.add_space(26.0);
        ui.separator();
        ui.label(egui::RichText::new("TIP").size(11.0)
            .color(ui_theme::ACCENT).strong());
        ui.label(egui::RichText::new(
            "Drop a grayscale PNG onto Gaia to import it, or drag nodes together using sockets."
        ).size(12.0).color(ui_theme::MUTED));
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("GAIA").size(20.0)
                .strong().color(ui_theme::ACCENT));
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
            if ui.button("Open…").on_hover_text("Choose project · Ctrl+O").clicked() {
                self.open_project_dialog();
            }
            if ui.button("Save").on_hover_text("Save current project · Ctrl+S").clicked() {
                self.save_project();
            }
            ui.menu_button("File", |ui| {
                if ui.button("Open entered path").clicked() {
                    self.open_project();
                    ui.close();
                }
                if ui.button("Save As…  Ctrl+Shift+S").clicked() {
                    self.save_project_as_dialog();
                    ui.close();
                }
                ui.separator();
                if ui.button("Import PNG heightmap…").clicked() {
                    self.import_heightmap_dialog();
                    ui.close();
                }
            });
            if ui.add_enabled(self.history.can_undo(), egui::Button::new("Undo"))
                .on_hover_text("Undo · Ctrl+Z").clicked()
            {
                self.undo_edit();
                self.skip_history_record = true;
            }
            if ui.add_enabled(self.history.can_redo(), egui::Button::new("Redo"))
                .on_hover_text("Redo · Ctrl+Shift+Z or Ctrl+Y").clicked()
            {
                self.redo_edit();
                self.skip_history_record = true;
            }
            ui.menu_button("Export", |ui| {
                if ui.button("16-bit Heightmap  ·  PNG").clicked() {
                    self.export_heightmap();
                    ui.close();
                }
                if ui.button("3D Terrain Mesh  ·  OBJ").clicked() {
                    self.export_mesh();
                    ui.close();
                }
                if ui.button("Selected output · PNG16 / RGB8").clicked() {
                    self.export_selected_map();
                    ui.close();
                }
                ui.separator();
                if ui.button("Selected output · 2×2 tiled PNG").clicked() {
                    self.export_selected_tiles(2);
                    ui.close();
                }
                if ui.button("Selected output · 4×4 tiled PNG").clicked() {
                    self.export_selected_tiles(4);
                    ui.close();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(
                    egui::RichText::new("Rebuild").color(ui_theme::BG).strong()
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
        let mut requested_connection: Option<(u64, usize, PortRef)> = None;
        let mut requested_delete = false;
        let mut requested_secondary_color = false;
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
                                    if ui.selectable_label(reference == input, name).clicked() {
                                        requested_connection = Some((node.id, index, reference.clone()));
                                    }
                                }
                            }
                        });
                });
            }
            ui.add_space(10.0);
            match &mut node.kind {
                NodeKind::ImportHeight { path, scale } => {
                    ui.label("PNG heightmap path");
                    changed |= ui.text_edit_singleline(path).changed();
                    changed |= ui.add(egui::Slider::new(scale, 0.0..=8.0)
                        .text("Height scale")).changed();
                    ui.small("PNG8/PNG16 grayscale. Newly entered relative paths use the working directory; saved projects reference assets from the project folder.");
                }
                NodeKind::Mountain { seed, frequency, height } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(frequency, 0.2..=20.0).text("Frequency")).changed();
                    changed |= ui.add(egui::Slider::new(height, 0.0..=2.0).text("Height")).changed();
                }
                NodeKind::Island { seed, radius, height, roughness } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(radius, 0.15..=0.85).text("Island radius")).changed();
                    changed |= ui.add(egui::Slider::new(height, 0.0..=2.0).text("Relief")).changed();
                    changed |= ui.add(egui::Slider::new(roughness, 0.0..=1.0).text("Coast variation")).changed();
                    ui.small("Flat ocean-level boundary; radial coastline with procedural interior relief.");
                }
                NodeKind::Volcano { seed, radius, height, mouth, bulk, roughness, center_x, center_y } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(radius, 0.10..=0.48).text("Radius")).changed();
                    changed |= ui.add(egui::Slider::new(height, 0.0..=2.0).text("Height")).changed();
                    changed |= ui.add(egui::Slider::new(mouth, 0.08..=0.65).text("Mouth width")).changed();
                    changed |= ui.add(egui::Slider::new(bulk, 0.0..=1.0).text("Bulk")).changed();
                    changed |= ui.add(egui::Slider::new(roughness, 0.0..=1.0).text("Flank breakup")).changed();
                    changed |= ui.add(egui::Slider::new(center_x, 0.2..=0.8).text("Center X")).changed();
                    changed |= ui.add(egui::Slider::new(center_y, 0.2..=0.8).text("Center Y")).changed();
                    ui.small("Standalone volcanic cone and caldera; erosion can be applied downstream.");
                }
                NodeKind::Plates { seed, count, continental, uplift, warp } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(count, 2..=64)
                        .text("Plate count")).changed();
                    changed |= ui.add(egui::Slider::new(continental, 0.0..=1.0)
                        .text("Continental share")).changed();
                    changed |= ui.add(egui::Slider::new(uplift, 0.0..=2.0)
                        .text("Collision uplift")).changed();
                    changed |= ui.add(egui::Slider::new(warp, 0.0..=1.0)
                        .text("Boundary warp")).changed();
                    ui.small("Static kinematic regions with seeded motion; not geological time evolution.");
                    ui.small("Outputs: Height; boundary, convergence, divergence, plate ID and continental masks.");
                }
                NodeKind::Noise { seed, frequency, amplitude } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(frequency, 0.2..=40.0).text("Frequency")).changed();
                    changed |= ui.add(egui::Slider::new(amplitude, 0.0..=2.0).text("Amplitude")).changed();
                }
                NodeKind::Range { seed, height, roughness } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(height, 0.0..=2.0).text("Height")).changed();
                    changed |= ui.add(egui::Slider::new(roughness, 0.0..=1.0).text("Roughness")).changed();
                }
                NodeKind::Crater { seed, radius, depth, rim } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(radius, 0.1..=0.42).text("Radius")).changed();
                    changed |= ui.add(egui::Slider::new(depth, 0.0..=1.0).text("Depth")).changed();
                    changed |= ui.add(egui::Slider::new(rim, 0.0..=1.0).text("Rim height")).changed();
                }
                NodeKind::Dunes { seed, frequency, amplitude } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(frequency, 1.0..=40.0).text("Frequency")).changed();
                    changed |= ui.add(egui::Slider::new(amplitude, 0.0..=2.0).text("Amplitude")).changed();
                }
                NodeKind::Canyon { seed, width, depth } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(width, 0.015..=0.3).text("Width")).changed();
                    changed |= ui.add(egui::Slider::new(depth, 0.0..=1.0).text("Depth")).changed();
                }
                NodeKind::Erosion { iterations, talus, strength } => {
                    changed |= ui.add(egui::Slider::new(iterations, 0..=120).text("Iterations")).changed();
                    changed |= ui.add(egui::Slider::new(talus, 0.0..=0.1).text("Talus")).changed();
                    changed |= ui.add(egui::Slider::new(strength, 0.0..=0.5).text("Transfer")).changed();
                    ui.small("Conservative thermal talus transport; separate hydraulic erosion is available.");
                }
                NodeKind::Hydraulic { params } => {
                    changed |= ui.add(egui::Slider::new(&mut params.iterations, 0..=200)
                        .text("Steps")).changed();
                    changed |= ui.add(egui::Slider::new(&mut params.rain, 0.0..=0.05)
                        .text("Rainfall")).changed();
                    changed |= ui.add(egui::Slider::new(&mut params.evaporation, 0.0..=0.3)
                        .text("Evaporation")).changed();
                    changed |= ui.add(egui::Slider::new(&mut params.capacity, 0.0..=10.0)
                        .text("Sediment capacity")).changed();
                    changed |= ui.add(egui::Slider::new(&mut params.erosion_rate, 0.0..=1.0)
                        .text("Erosion rate")).changed();
                    changed |= ui.add(egui::Slider::new(&mut params.deposition_rate, 0.0..=1.0)
                        .text("Deposition rate")).changed();
                    ui.small("Conservative experimental hydraulic model; not Gaea's solver.");
                }
                NodeKind::Terrace { steps, softness } => {
                    changed |= ui.add(egui::Slider::new(steps, 2..=64).text("Steps")).changed();
                    changed |= ui.add(egui::Slider::new(softness, 0.0..=1.0).text("Softness")).changed();
                }
                NodeKind::Stratify { seed, spacing, intensity, warp, tilt, direction } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(spacing, 0.01..=0.4).text("Layer spacing")).changed();
                    changed |= ui.add(egui::Slider::new(intensity, 0.0..=1.0).text("Intensity")).changed();
                    changed |= ui.add(egui::Slider::new(warp, 0.0..=1.0).text("Warp")).changed();
                    changed |= ui.add(egui::Slider::new(tilt, -1.0..=1.0).text("Tilt")).changed();
                    changed |= ui.add(egui::Slider::new(direction, 0.0..=360.0).text("Direction")).changed();
                    ui.small("Adds broken layers to the selected terrain while retaining its main relief.");
                }
                NodeKind::Adjust { multiply, shaper, clamp_min, clamp_max, autolevel, invert } => {
                    changed |= ui.add(egui::Slider::new(multiply, 0.0..=4.0)
                        .text("Multiply")).changed();
                    changed |= ui.add(egui::Slider::new(shaper, -1.0..=1.0)
                        .text("Bulk / Shaper")).changed();
                    changed |= ui.add(egui::Slider::new(clamp_min, 0.0..=4.0)
                        .text("Clamp min")).changed();
                    changed |= ui.add(egui::Slider::new(clamp_max, 0.0..=4.0)
                        .text("Clamp max")).changed();
                    if *clamp_min > *clamp_max {
                        std::mem::swap(clamp_min, clamp_max);
                        changed = true;
                    }
                    changed |= ui.checkbox(autolevel, "Global Autolevel").changed();
                    changed |= ui.checkbox(invert, "Invert normalized heights").changed();
                    ui.small("Positive Shaper bulks mountains before erosion; Autolevel uses the entire input range.");
                }
                NodeKind::Equalize { factor } => {
                    changed |= ui.add(egui::Slider::new(factor, 0.0..=1.0)
                        .text("Equalize factor")).changed();
                    ui.small("Global histogram redistribution; 0 preserves the original and 1 fully equalizes, with min/max elevation unchanged.");
                }
                NodeKind::Blur { radius } => {
                    changed |= ui.add(egui::Slider::new(radius, 1..=8).text("Radius")).changed();
                }
                NodeKind::Blend { mix } | NodeKind::MaskedBlend { mix } => {
                    changed |= ui.add(egui::Slider::new(mix, 0.0..=1.0).text("Mix")).changed();
                }
                NodeKind::ColorMixer { mix } => {
                    changed |= ui.add(egui::Slider::new(mix, 0.0..=1.0).text("Mix")).changed();
                    ui.small("Requires two Color ports and a Mask port.");
                    if node.inputs.len() >= 2 && node.inputs[0] == node.inputs[1] {
                        ui.colored_label(ui_theme::AMBER,
                            "Both Color inputs are identical. Mix has no visible effect.");
                        if ui.button("Create contrasting palette").on_hover_text(
                            "Create a second Colorize node and connect it to Input 2"
                        ).clicked() {
                            requested_secondary_color = true;
                        }
                    }
                }
                NodeKind::MaterialMasks { sea_level, beach_width, rock_slope, rock_blend, snowline, snow_blend, palette } => {
                    changed |= ui.add(egui::Slider::new(sea_level, 0.0..=1.0)
                        .text("Sea / sand level")).changed();
                    changed |= ui.add(egui::Slider::new(beach_width, 0.001..=0.5)
                        .text("Beach fade")).changed();
                    changed |= ui.add(egui::Slider::new(rock_slope, 0.0..=1.0)
                        .text("Rock slope")).changed();
                    changed |= ui.add(egui::Slider::new(rock_blend, 0.001..=0.5)
                        .text("Rock fade")).changed();
                    changed |= ui.add(egui::Slider::new(snowline, 0.0..=2.0)
                        .text("Snowline")).changed();
                    changed |= ui.add(egui::Slider::new(snow_blend, 0.001..=0.5)
                        .text("Snow fade")).changed();
                    ui.small("Outputs: sand/soil/rock/snow Mask, RGBA8 splat weights and a baked RGB8 albedo map (no lighting or detail textures).");
                    ui.collapsing("Material appearance (3D PBR)", |ui| {
                        changed |= ui.add(egui::Slider::new(&mut palette.detail_scale, 1.0..=24.0)
                            .text("Texture repeats")).changed();
                        for (i, name) in ["Sand", "Soil", "Rock", "Snow"].iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(*name);
                                changed |= ui.color_edit_button_rgb(&mut palette.colors[i]).changed();
                            });
                            changed |= ui.add(egui::Slider::new(&mut palette.roughness[i], 0.05..=1.0)
                                .text(format!("{name} roughness"))).changed();
                        }
                    });
                    ui.small("Height/slope classifications stay unchanged when colors or roughness change.");
                }
                NodeKind::Normals { strength, flip_green } => {
                    changed |= ui.add(egui::Slider::new(strength, 0.0..=4.0)
                        .text("Surface strength")).changed();
                    changed |= ui.checkbox(flip_green, "Flip green (Y) channel").changed();
                    ui.small("Flat = RGB (128,128,255). Select the normal port for RGB8 export.");
                }
                NodeKind::Colorize { low, high, midpoint, contrast } => {
                    ui.horizontal(|ui| {
                        ui.label("Low elevation");
                        changed |= ui.color_edit_button_rgb(low).changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label("High elevation");
                        changed |= ui.color_edit_button_rgb(high).changed();
                    });
                    changed |= ui.add(egui::Slider::new(midpoint, 0.01..=0.99)
                        .text("Midpoint")).changed();
                    changed |= ui.add(egui::Slider::new(contrast, 0.1..=6.0)
                        .text("Contrast")).changed();
                }
                NodeKind::Slope => {
                    ui.label("Slope mask derived from surrounding elevation differences.");
                }
                NodeKind::Curvature => {
                    ui.label("Curvature mask: dark ridges, bright bowls, midgray flat terrain.");
                }
                NodeKind::AmbientOcclusion { radius, strength } => {
                    changed |= ui.add(egui::Slider::new(radius, 1..=64)
                        .text("Horizon radius")).changed();
                    changed |= ui.add(egui::Slider::new(strength, 0.0..=1.0)
                        .text("Occlusion strength")).changed();
                    ui.small("Eight horizon directions. White = exposed; dark = occluded.");
                }
                NodeKind::Sunlight { azimuth, elevation, distance } => {
                    changed |= ui.add(egui::Slider::new(azimuth, 0.0..=360.0)
                        .text("Sun azimuth")).changed();
                    changed |= ui.add(egui::Slider::new(elevation, 1.0..=89.0)
                        .text("Sun elevation")).changed();
                    changed |= ui.add(egui::Slider::new(distance, 1..=128)
                        .text("Shadow reach")).changed();
                    ui.small("0° North, 90° East. Diffuse lighting plus terrain self-shadow mask.");
                }
                NodeKind::Aspect { azimuth, half_width, min_slope } => {
                    changed |= ui.add(egui::Slider::new(azimuth, 0.0..=360.0)
                        .text("Downhill facing °")).changed();
                    changed |= ui.add(egui::Slider::new(half_width, 1.0..=180.0)
                        .text("Half-width °")).changed();
                    changed |= ui.add(egui::Slider::new(min_slope, 0.0..=2.0)
                        .text("Minimum slope")).changed();
                    ui.small("0° North, 90° East, 180° South, 270° West. Flat terrain is black.");
                }
                NodeKind::Drainage { min_catchment_fraction } => {
                    changed |= ui.add(egui::Slider::new(min_catchment_fraction, 0.001..=0.2)
                        .text("Channel fraction")).changed();
                    ui.small("D8 accumulation and channel masks. Local sinks are terminal unless upstream height is conditioned with Depression Fill.");
                }
                NodeKind::DepressionFill { grade } => {
                    changed |= ui.add(egui::Slider::new(grade, 0.0..=0.01)
                        .step_by(0.00001).text("Drainage grade")).changed();
                    ui.small("Raises enclosed pits to their lowest outlet. Positive grade makes flat cells drain; 0 preserves flat water surfaces.");
                    ui.small("Outputs: conditioned height and fill_depth (raw height units, not simulated lake water). All map edges are outlets.");
                }
                NodeKind::Rivers { catchment_fraction, width, depth, grade } => {
                    changed |= ui.add(egui::Slider::new(catchment_fraction, 0.001..=0.05)
                        .step_by(0.001).text("Catchment fraction")).changed();
                    changed |= ui.add(egui::Slider::new(width, 0.5..=12.0)
                        .text("Corridor width (pixels)")).changed();
                    changed |= ui.add(egui::Slider::new(depth, 0.0..=0.5)
                        .text("Downcutting")).changed();
                    changed |= ui.add(egui::Slider::new(grade, 0.0..=0.01)
                        .step_by(0.00001).text("Drainage grade")).changed();
                    ui.small("Global priority flood + D8 network + softened channel incision. All edges are potential outlets.");
                    ui.small("Outputs: carved Height, channels Mask, wear Mask. No water volume or sediment transport.");
                }
                NodeKind::Lake { fill_fraction, minimum_area, minimum_depth } => {
                    changed |= ui.add(egui::Slider::new(fill_fraction, 0.0..=1.0)
                        .text("Basin fill")).changed();
                    changed |= ui.add(egui::Slider::new(minimum_area, 1..=10000)
                        .text("Min basin area (pixels)")).changed();
                    changed |= ui.add(egui::Slider::new(minimum_depth, 0.0..=1.0)
                        .text("Min potential depth")).changed();
                    ui.small("Closed-basin static water levels; no water flow, rainfall, or evolving sediment.");
                    ui.small("Outputs: original terrain Height, surface Height, lake depth Water, coverage Mask.");
                }
                NodeKind::Sea { level, shore_width, erosion } => {
                    changed |= ui.add(egui::Slider::new(level, -2.0..=4.0)
                        .text("Sea level")).changed();
                    changed |= ui.add(egui::Slider::new(shore_width, 1..=32)
                        .text("Shore reach (pixels)")).changed();
                    changed |= ui.add(egui::Slider::new(erosion, 0.0..=1.0)
                        .text("Coastal smoothing")).changed();
                    ui.small("Shore is detected at actual dry/wet transitions. No simulated waves, tides or sediment mass.");
                    ui.small("Outputs: height, flat surface, water depth, wet, shore and beach masks.");
                }
                NodeKind::Snow { seed, snowline, intensity, melt, adherence, settle } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(snowline, -2.0..=4.0)
                        .text("Snow line")).changed();
                    changed |= ui.add(egui::Slider::new(intensity, 0.0..=0.5)
                        .text("Snowfall")).changed();
                    changed |= ui.add(egui::Slider::new(melt, 0.0..=1.0)
                        .text("Melt")).changed();
                    changed |= ui.add(egui::Slider::new(adherence, 0.0..=1.0)
                        .text("Slope adhesion")).changed();
                    changed |= ui.add(egui::Slider::new(settle, 0..=24)
                        .text("Settling steps")).changed();
                    ui.small("Snow mass is preserved during downhill transfer; melt explicitly removes mass.");
                    ui.small("Outputs: snow-covered Height, raw snow_depth Height, coverage Mask.");
                }
                NodeKind::MaskLevels { black, white, gamma, invert } => {
                    changed |= ui.add(egui::Slider::new(black, 0.0..=1.0)
                        .text("Black point")).changed();
                    changed |= ui.add(egui::Slider::new(white, 0.0..=1.0)
                        .text("White point")).changed();
                    changed |= ui.add(egui::Slider::new(gamma, 0.1..=4.0)
                        .text("Gamma")).changed();
                    changed |= ui.checkbox(invert, "Invert mask").changed();
                    ui.small("Black must remain below white. Input and output are typed Mask fields.");
                }
                NodeKind::ElevationMask { low, high, feather } => {
                    changed |= ui.add(egui::Slider::new(low, 0.0..=1.0).text("Min height")).changed();
                    changed |= ui.add(egui::Slider::new(high, 0.0..=2.0).text("Max height")).changed();
                    changed |= ui.add(egui::Slider::new(feather, 0.001..=0.5).text("Feather")).changed();
                }
                NodeKind::Warp { seed, frequency, amount } => {
                    changed |= ui.add(egui::Slider::new(seed, 0..=100_000).text("Seed")).changed();
                    changed |= ui.add(egui::Slider::new(frequency, 0.5..=40.0).text("Frequency")).changed();
                    changed |= ui.add(egui::Slider::new(amount, 0.0..=0.25).text("Strength")).changed();
                }
            }
            ui.add_space(12.0);
            if ui.add(egui::Button::new(
                egui::RichText::new("Delete unused node").color(ui_theme::RED)
            )).on_hover_text("Connected nodes cannot be removed until their wires are changed")
                .clicked()
            {
                requested_delete = true;
            }
        } else {
            ui.label("Click a node in the graph to edit it.");
        }
        if let Some((target, slot, source)) = requested_connection {
            match self.project.graph.connect(target, slot, source) {
                Ok(()) => changed = true,
                Err(error) => self.status = format!("Connection rejected: {error}"),
            }
        }
        if requested_secondary_color {
            match add_contrasting_colorize(&mut self.project, self.selected) {
                Ok(id) => {
                    changed = true;
                    self.status = format!("Created Colorize #{id} and connected Color Mixer Input 2");
                }
                Err(error) => self.status = format!("Color setup failed: {error}"),
            }
        }
        if set_output {
            self.project.graph.output = self.selected;
            self.preview_source = PortRef::height(self.selected);
            changed = true;
        }
        if requested_delete {
            let id = self.selected;
            match self.project.graph.remove(id) {
                Ok(()) => {
                    self.selected = self.project.graph.output;
                    self.preview_source = PortRef::height(self.selected);
                    self.wire_start = None;
                    self.status = format!("Deleted unused node #{id}");
                    changed = true;
                }
                Err(error) => self.status = error,
            }
        }
        if changed {
            self.graph_dirty = true;
            self.unsaved = true;
        }
        ui.separator();
        ui.label(egui::RichText::new("Drag from an output socket to a compatible input. Drag cards to rearrange.")
            .size(12.0).color(ui_theme::MUTED));
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Connections are validated before rebuilding.")
            .size(12.0).color(ui_theme::MUTED));
    }

    fn graph_canvas(&mut self, ui: &mut egui::Ui) {
        const WIDTH: f32 = 194.0;
        const HEIGHT: f32 = 74.0;
        const STEP: f32 = 17.0;
        let extent = graph_canvas_extent(&self.project.graph.nodes, ui.available_width());
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
                    let port_index = parent.kind.output_ports().iter()
                        .position(|(name, _)| *name == source.port).unwrap_or(0);
                    let start = rect.left_top() + egui::vec2(
                        parent.position[0] + WIDTH, parent.position[1] + 29.0 + port_index as f32 * STEP
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
        let mut requested_wire = None;
        let mut requested_preview = None;
        let released = ui.input(|i| i.pointer.any_released());
        for node in &mut self.project.graph.nodes {
            let top_left = rect.left_top() + egui::vec2(node.position[0], node.position[1]);
            let port_count = node.inputs.len().max(node.kind.output_ports().len()).max(1);
            let node_height = HEIGHT.max(47.0 + port_count as f32 * STEP);
            let node_rect = egui::Rect::from_min_size(top_left, egui::vec2(WIDTH, node_height));
            let response = ui.interact(
                node_rect, ui.scope_id().with(("node", node.id)),
                egui::Sense::click_and_drag()
            ).on_hover_text("Click to inspect · drag to move");
            if response.clicked() {
                self.selected = node.id;
                if let Some((port, _)) = node.kind.output_ports().first() {
                    requested_preview = Some(PortRef::new(node.id, *port));
                }
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
            for slot in 0..node.inputs.len() {
                let center = node_rect.left_top() + egui::vec2(0.0, 29.0 + slot as f32 * STEP);
                let socket = egui::Rect::from_center_size(center, egui::vec2(20.0, 20.0));
                let response = ui.interact(
                    socket, ui.scope_id().with(("input-socket", node.id, slot)),
                    egui::Sense::hover()
                ).on_hover_text(format!(
                    "Input {} · {}",
                    slot + 1,
                    node.kind.input_type(slot).map(|ty| ty.label()).unwrap_or("?")
                ));
                let compatible = self.wire_start.as_ref().is_some_and(|source| {
                    nodes.iter().find(|n| n.id == source.node)
                        .and_then(|n| n.kind.output_ports().iter()
                            .find(|(port, _)| *port == source.port))
                        .is_some_and(|(_, ty)| Some(*ty) == node.kind.input_type(slot))
                });
                if released && response.hovered() {
                    if let Some(source) = &self.wire_start {
                        requested_wire = Some((node.id, slot, source.clone()));
                    }
                }
                painter.circle_filled(center,
                    if compatible { 6.0 } else { 4.0 },
                    if compatible { ui_theme::ACCENT } else { ui_theme::BLUE });
            }
            for (slot, (port, kind)) in node.kind.output_ports().iter().enumerate() {
                let center = node_rect.left_top()
                    + egui::vec2(WIDTH, 29.0 + slot as f32 * STEP);
                let socket = egui::Rect::from_center_size(center, egui::vec2(20.0, 20.0));
                let response = ui.interact(
                    socket, ui.scope_id().with(("output-socket", node.id, slot)),
                    egui::Sense::click_and_drag()
                ).on_hover_text(format!("{} · {} · drag to connect", port, kind.label()));
                if response.drag_started() {
                    self.wire_start = Some(PortRef::new(node.id, *port));
                }
                painter.circle_filled(center,
                    if response.hovered() { 6.0 } else { 4.5 },
                    if response.hovered() { ui_theme::TEXT } else { color });
            }
            if node.id == self.project.graph.output {
                painter.text(node_rect.right_bottom() + egui::vec2(-10.0, -12.0),
                    egui::Align2::RIGHT_BOTTOM, "OUTPUT",
                    egui::FontId::monospace(10.0), ui_theme::ACCENT);
            }
        }
        if let Some(source) = &self.wire_start {
            if let Some(parent) = nodes.iter().find(|n| n.id == source.node) {
                if let Some(index) = parent.kind.output_ports().iter()
                    .position(|(name, _)| *name == source.port)
                {
                    let from = rect.left_top() + egui::vec2(
                        parent.position[0] + WIDTH,
                        parent.position[1] + 29.0 + index as f32 * STEP
                    );
                    if let Some(to) = ui.input(|i| i.pointer.hover_pos()) {
                        painter.line_segment(
                            [from, to], egui::Stroke::new(2.5, ui_theme::ACCENT)
                        );
                    }
                }
            }
        }
        // Selecting a graph card should inspect its actual output. Otherwise
        // the inspector changes while the preview remains on an unrelated node.
        if let Some(preview) = requested_preview {
            if self.preview_source != preview {
                self.preview_source = preview;
                self.graph_dirty = true;
            }
        }
        if self.focus_graph_selection {
            if let Some(node) = self.project.graph.get(self.selected) {
                let ports = node.inputs.len().max(node.kind.output_ports().len()).max(1);
                let node_height = 74.0_f32.max(47.0 + ports as f32 * 17.0);
                let card = egui::Rect::from_min_size(
                    rect.left_top() + egui::vec2(node.position[0], node.position[1]),
                    egui::vec2(194.0, node_height),
                );
                ui.scroll_to_rect(card, Some(egui::Align::Center));
            }
            self.focus_graph_selection = false;
        }
        if released {
            self.wire_start = None;
            if let Some((target, slot, source)) = requested_wire {
                match self.project.graph.connect(target, slot, source) {
                    Ok(()) => {
                        self.graph_dirty = true;
                        self.unsaved = true;
                        self.status = "Graph connection updated".to_owned();
                    }
                    Err(error) => self.status = format!("Connection rejected: {error}"),
                }
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
                ui.menu_button("Lighting", |ui| {
                    ui.label(egui::RichText::new("VIEWPORT LIGHT").strong());
                    ui.add(egui::Slider::new(&mut self.sun_azimuth, 0.0..=360.0)
                        .text("Azimuth °"));
                    ui.add(egui::Slider::new(&mut self.sun_elevation, 5.0..=89.0)
                        .text("Elevation °"));
                    ui.add(egui::Slider::new(&mut self.sun_strength, 0.0..=4.0)
                        .text("Sun intensity"));
                    ui.small("0° North, 90° East. Preview controls only; no change to Sunlight graph masks.");
                    if ui.button("Reset light").clicked() {
                        self.sun_azimuth = 55.0;
                        self.sun_elevation = 60.0;
                        self.sun_strength = 2.0;
                    }
                });
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
                rect, gpu::TerrainCallback {
                    camera: matrix,
                    eye: gpu::eye_position(self.yaw, self.pitch, self.distance)
                        .extend(1.0).to_array(),
                    materials: self.project.graph.get(self.preview_source.node)
                        .and_then(|node| match &node.kind {
                            NodeKind::MaterialMasks { palette, .. } => Some(palette.gpu_layers()),
                            _ => None,
                        })
                        .unwrap_or_else(|| MaterialPalette::default().gpu_layers()),
                    lighting: gpu::sun_uniform(
                        self.sun_azimuth, self.sun_elevation, self.sun_strength),
                    detail: [
                        self.project.graph.get(self.preview_source.node)
                            .and_then(|node| match &node.kind {
                                NodeKind::MaterialMasks { palette, .. } =>
                                    Some(palette.detail_scale.clamp(1.0, 24.0)),
                                _ => None,
                            })
                            .unwrap_or(7.5),
                        0.0, 0.0, 0.0,
                    ],
                    pixels: [
                        (rect.width() * ui.ctx().pixels_per_point()).ceil().max(1.0) as u32,
                        (rect.height() * ui.ctx().pixels_per_point()).ceil().max(1.0) as u32,
                    ],
                }
            ));
        }
        ui.painter().rect_stroke(rect, 9.0, egui::Stroke::new(
            1.0, ui_theme::BORDER
        ), egui::StrokeKind::Inside);
        if ui.ctx().input(|input| !input.raw.hovered_files.is_empty()) {
            ui.painter().rect_filled(rect, 9.0, egui::Color32::from_black_alpha(205));
            ui.painter().text(
                rect.center(), egui::Align2::CENTER_CENTER,
                "DROP PNG TO IMPORT\nDROP .GAIA.JSON TO OPEN",
                egui::FontId::proportional(19.0), ui_theme::ACCENT,
            );
        }
        ui.painter().text(rect.left_top() + egui::vec2(16.0, 15.0),
            egui::Align2::LEFT_TOP,
            if self.view_2d { "SELECTED GRAPH MAP  •  TOP DOWN" }
            else { "PERSPECTIVE  •  REALTIME GPU" },
            egui::FontId::monospace(11.0), ui_theme::TEXT);
        ui.painter().text(rect.left_bottom() + egui::vec2(16.0, -17.0),
            egui::Align2::LEFT_BOTTOM,
            if self.view_2d {
                let kind = self.project.graph.get(self.preview_source.node)
                    .and_then(|node| node.kind.output_ports().iter()
                        .find(|(port, _)| *port == self.preview_source.port))
                    .map(|(_, kind)| *kind);
                match kind {
                    Some(PortType::Splat) => "SPLAT • R=sand  G=soil  B=rock  A=snow  ·  RGBA8 export",
                    Some(PortType::Color) => "Color output available as RGB8 PNG in Export",
                    _ => "Scalar output available as grayscale 16-bit PNG in Export",
                }
            }
            else if self.material_splat.is_some() {
                "PBR MATERIALS  •  Sand / Soil / Rock / Snow  •  Orbit / Zoom"
            } else if self.material_color.is_some() {
                "COLOR MAP ON TERRAIN   •   Drag to orbit   •   Scroll to zoom"
            } else { "Drag to orbit   •   Scroll to zoom" },
            egui::FontId::proportional(12.0), ui_theme::TEXT);
    }
}

impl eframe::App for GaiaApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Standard shortcuts. Open never modifies the running project on failure.
        if ctx.input(|i| i.modifiers.command && i.modifiers.shift
            && i.key_pressed(egui::Key::S)) {
            self.save_project_as_dialog();
        } else if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
            self.save_project();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
            self.open_project_dialog();
        }
        // Leave keyboard undo inside focused text fields to egui's text editor.
        if !ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z)) {
                if ctx.input(|i| i.modifiers.shift) {
                    self.redo_edit();
                } else {
                    self.undo_edit();
                }
            } else if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Y)) {
                self.redo_edit();
            }
        }
        let project_before_frame = self.project.clone();
        self.handle_dropped_files(&ctx);

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
                        if ui.button("Find selected").on_hover_text(
                            "Scroll to the selected graph node"
                        ).clicked() {
                            self.focus_graph_selection = true;
                        }
                        ui.label(egui::RichText::new("Select · drag · connect ports")
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

        if !std::mem::take(&mut self.skip_history_record) {
            self.history.record_frame(
                project_before_frame, &self.project,
                ctx.input(|i| i.pointer.primary_down()),
            );
        }
        if self.graph_dirty {
            self.rebuild(&ctx);
        }
        self.poll_worker(&ctx);
        if self.gpu_dirty {
            if let Some(render_state) = frame.wgpu_render_state() {
                gpu::upload(render_state, &self.field,
                    self.material_color.as_ref(), self.material_splat.as_ref());
            }
            self.gpu_dirty = false;
        }
    }
}

fn node_accent(kind: &NodeKind) -> egui::Color32 {
    match kind {
        NodeKind::Mountain { .. } | NodeKind::Island { .. } | NodeKind::Volcano { .. } | NodeKind::Plates { .. } | NodeKind::Noise { .. } | NodeKind::ImportHeight { .. } |
        NodeKind::Range { .. } | NodeKind::Crater { .. } |
        NodeKind::Dunes { .. } | NodeKind::Canyon { .. } => ui_theme::ACCENT,
        NodeKind::Erosion { .. } | NodeKind::Hydraulic { .. } | NodeKind::Terrace { .. } | NodeKind::Stratify { .. } | NodeKind::Adjust { .. } | NodeKind::Equalize { .. } | NodeKind::DepressionFill { .. } | NodeKind::Rivers { .. } | NodeKind::Lake { .. } | NodeKind::Sea { .. } | NodeKind::Snow { .. } | NodeKind::Blur { .. } => ui_theme::BLUE,
        NodeKind::Blend { .. } | NodeKind::MaskedBlend { .. } |
        NodeKind::Colorize { .. } | NodeKind::ColorMixer { .. } | NodeKind::Normals { .. } => ui_theme::AMBER,
        NodeKind::Slope | NodeKind::Curvature | NodeKind::MaterialMasks { .. } | NodeKind::AmbientOcclusion { .. } | NodeKind::Sunlight { .. } | NodeKind::Aspect { .. } | NodeKind::Drainage { .. } | NodeKind::MaskLevels { .. } | NodeKind::ElevationMask { .. } | NodeKind::Warp { .. } => ui_theme::BLUE,
    }
}

/// A false-color material preview; the actual export contains RGBA weights.
/// Sand=tan, soil=green, rock=slate, snow=white.
fn splat_preview_image(weights: &ColorField) -> egui::ColorImage {
    const PALETTE: [[f32; 3]; 4] = [
        [0.76, 0.63, 0.41], [0.20, 0.39, 0.21],
        [0.48, 0.49, 0.52], [0.92, 0.95, 0.99],
    ];
    let mut rgba = Vec::with_capacity(weights.pixels.len() * 4);
    for weight in &weights.pixels {
        for c in 0..3 {
            let value: f32 = (0..4).map(|layer| weight[layer] * PALETTE[layer][c]).sum();
            rgba.push((value.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        rgba.push(255);
    }
    egui::ColorImage::from_rgba_unmultiplied([weights.size, weights.size], &rgba)
}

fn color_image(field: &ColorField) -> egui::ColorImage {
    let mut rgba = Vec::with_capacity(field.pixels.len() * 4);
    for pixel in &field.pixels {
        for &channel in pixel {
            rgba.push((channel.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([field.size, field.size], &rgba)
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

#[cfg(test)]
mod file_drop_tests {
    use super::*;

    #[test]
    fn project_dialog_directory_uses_existing_parent_only() {
        assert!(project_dialog_directory(Path::new("terrain.gaia.json")).is_none());
        assert!(project_dialog_directory(Path::new("nonexistent-folder/terrain.gaia.json")).is_none());
        let temp = std::env::temp_dir();
        let path = temp.join("terrain.gaia.json");
        assert_eq!(project_dialog_directory(&path), Some(temp.as_path()));
    }

    #[test]
    fn only_png_heightmaps_and_gaia_projects_are_accepted() {
        assert_eq!(classify_drop(Path::new("height.PNG")), DroppedDocument::Heightmap);
        assert_eq!(classify_drop(Path::new("world.gaia.json")), DroppedDocument::Project);
        assert_eq!(classify_drop(Path::new("WORLD.GAIA.JSON")), DroppedDocument::Project);
        assert_eq!(classify_drop(Path::new("world.json")), DroppedDocument::Unsupported);
        assert_eq!(classify_drop(Path::new("photo.jpeg")), DroppedDocument::Unsupported);
    }

    #[test]
    fn duplicate_color_mixer_can_create_distinct_secondary_source() {
        let mut project = Project::default();
        let first = project.graph.add(NodeKind::Colorize {
            low: [0.1, 0.2, 0.3], high: [0.8, 0.8, 0.7],
            midpoint: 0.5, contrast: 1.0,
        });
        let mixer = project.graph.add(NodeKind::ColorMixer { mix: 0.7 });
        assert_eq!(project.graph.get(mixer).unwrap().inputs[0],
            PortRef::new(first, "color"));
        assert_eq!(project.graph.get(mixer).unwrap().inputs[0],
            project.graph.get(mixer).unwrap().inputs[1]);

        let second = add_contrasting_colorize(&mut project, mixer).unwrap();
        let inputs = &project.graph.get(mixer).unwrap().inputs;
        assert_eq!(inputs[1], PortRef::new(second, "color"));
        assert_ne!(inputs[0], inputs[1]);
        assert!(add_contrasting_colorize(&mut project, mixer).is_err());

        let value = project.graph.evaluate_port(32, &PortRef::new(mixer, "color")).unwrap();
        assert_eq!(value.kind, PortType::Color);
        assert_eq!(value.color.as_ref().unwrap().pixels.len(), 32 * 32);
    }

    #[test]
    fn graph_canvas_extends_to_last_node_and_dragged_positions() {
        let mut project = Project::default();
        for index in 0..30 {
            project.graph.add(NodeKind::Noise {
                seed: index, frequency: 3.0, amplitude: 0.2
            });
        }
        let last = project.graph.nodes.last().unwrap();
        let extent = graph_canvas_extent(&project.graph.nodes, 800.0);
        assert!(extent.y >= last.position[1] + 74.0 + 80.0);
        assert!(extent.y > 1000.0, "large graph must scroll vertically");
        project.graph.nodes[0].position = [2500.0, 1800.0];
        let extent = graph_canvas_extent(&project.graph.nodes, 800.0);
        assert!(extent.x >= 2500.0 + 194.0 + 80.0);
        assert!(extent.y >= 1800.0 + 74.0 + 80.0);
    }
}
