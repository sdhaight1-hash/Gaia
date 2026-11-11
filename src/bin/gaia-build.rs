//! Standalone deterministic terrain build tool.
//! No window or GPU required; intended for production pipelines and CI.
use gaia::pipeline::{GraphCache, PortRef, PortType};
use gaia::color::{export_rgb8, export_splat_rgba8};
use gaia::hydraulic_build::build_hydraulic_regions;
use gaia::hydraulic_exchange::WorldEdge;
use gaia::tiles::{tile_layout, tile_height, tile_color};
use gaia::region::{evaluate_region, evaluate_color_region, validate_region_graph,
    RegionLayout, MAX_REGION_RESOLUTION};
use gaia::terrain::{export_obj, export_png16, Project};
use serde_json::json;
use std::{env, path::{Path, PathBuf}, process, time::Instant};

const HELP: &str = r#"Gaia headless terrain builder
Usage: gaia-build --project FILE --output STEM [--resolution N] [--port NODE:PORT] [--no-obj] [--tiles COUNT]
       gaia-build --project FILE --output STEM --all-outputs NODE [--resolution N] [--no-obj] [--tiles COUNT]
       gaia-build --demo --output STEM [--resolution N] [--no-obj] [--tiles COUNT]
       gaia-build --project FILE --output STEM --port NODE:PORT --no-obj --region-tiles COUNT [--resolution N]
       gaia-build --project FILE --output STEM --hydraulic-region NODE --no-obj --region-tiles COUNT [--resolution N] [--hydraulic-edge open|closed] [--hydraulic-checkpoint]

Examples:
  gaia-build --project terrain.gaia.json --output builds/mountain
  gaia-build --project terrain.gaia.json --output builds/wear --port 2:wear --no-obj
  gaia-build --demo --output builds/sample --resolution 256
  gaia-build --project terrain.gaia.json --output builds/land --tiles 4
  gaia-build --project terrain.gaia.json --output builds/erosion --all-outputs 2
  gaia-build --demo --port 1:height --output builds/mountain-regions --no-obj --region-tiles 4 --resolution 4096
  gaia-build --project terrain.gaia.json --port 8:slope --output builds/slope-regions --no-obj --region-tiles 4
  gaia-build --project terrain.gaia.json --hydraulic-region 8 --output builds/eroded --no-obj --region-tiles 4 --resolution 512

Exports PNG16 for scalar fields, RGB8 for Color, RGBA8 for Splat weights,
and OBJ for height outputs.
Writes STEM.report.json. --tiles produces N×N PNGs in STEM.tiles/ plus
STEM.tiles.json, each tile sharing one identical border pixel with neighbors.
This subdivides an already-built field; it does not reduce graph build memory.
--all-outputs NODE exports every named output of a node using one shared
evaluation cache and writes STEM.batch.report.json. Do not combine with --port.
Regular resolution must be 32–2048; --tiles COUNT 2–8 splits a FULL graph.
--region-tiles COUNT 2–64 evaluates Height/Mask, RGB Color and RGBA Splat regions,
up to 8192 resolution, with exact shared border samples and bounded working
memory. This mode requires --no-obj, and does not output a stitched PNG/mesh.
Supports local Mountain, Noise, Adjust (autolevel OFF), Blur, Blend, Masked
Blend, Slope, Curvature, Aspect, Elevation Mask, Mask Levels, Colorize and
Color Mixer and Material Masks (sand/soil/rock/snow, albedo, splat).
Erosion, hydrology, autolevel and other global nodes are rejected
explicitly (never silently fall back to full-grid evaluation).
--hydraulic-region NODE is an opt-in 2D disk-backed hydraulic build for an
existing Hydraulic node whose upstream Height inputs support regional evaluation.
Runs the node's own iteration/physics settings and exports height/water/
sediment/wear/deposits/flow to STEM.hydraulic.tiles/ as six PNG16 tile sets
with shared world-grid border samples and a report.json. 2D simulation cores
are each <=256 pixels per axis (e.g. 4+ tiles per axis for 1024 resolution).
--hydraulic-edge defaults to open. --hydraulic-checkpoint additionally saves
the f32 restart state into that output folder. No full-world heightfield is
allocated. This does not enable Hydraulic in ordinary --region-tiles builds.
Desktop previews remain at up to 512.
"#;

#[derive(Debug)]
struct Options {
    project: Option<PathBuf>,
    output: PathBuf,
    resolution: Option<usize>,
    selected: Option<PortRef>,
    all_outputs: Option<u64>,
    demo: bool,
    no_obj: bool,
    tiles: Option<usize>,
    region_tiles: Option<usize>,
    hydraulic_region: Option<u64>,
    hydraulic_edge: WorldEdge,
    hydraulic_checkpoint: bool,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Options>, String> {
    let mut it = args.into_iter();
    let mut project = None;
    let mut output = None;
    let mut resolution = None;
    let mut selected = None;
    let mut all_outputs = None;
    let mut demo = false;
    let mut no_obj = false;
    let mut tiles = None;
    let mut region_tiles = None;
    let mut hydraulic_region = None;
    let mut hydraulic_edge = WorldEdge::Open;
    let mut hydraulic_checkpoint = false;
    let mut hydraulic_edge_set = false;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--demo" => demo = true,
            "--no-obj" => no_obj = true,
            "--tiles" => {
                let value = it.next().ok_or("--tiles requires COUNT")?;
                let count: usize = value.parse()
                    .map_err(|_| "--tiles COUNT must be an integer")?;
                if !(2..=8).contains(&count) {
                    return Err("--tiles COUNT must be between 2 and 8".into());
                }
                tiles = Some(count);
            },
            "--hydraulic-region" => {
                let value = it.next().ok_or("--hydraulic-region requires a node ID")?;
                let node = value.parse::<u64>()
                    .map_err(|_| "--hydraulic-region NODE must be numeric")?;
                if node == 0 || hydraulic_region.replace(node).is_some() {
                    return Err("--hydraulic-region requires one positive node ID".into());
                }
            },
            "--hydraulic-edge" => {
                if hydraulic_edge_set {
                    return Err("--hydraulic-edge may only be specified once".into());
                }
                let value = it.next().ok_or("--hydraulic-edge requires open|closed")?;
                hydraulic_edge = match value.as_str() {
                    "open" => WorldEdge::Open,
                    "closed" => WorldEdge::Closed,
                    _ => return Err("--hydraulic-edge requires open or closed".into()),
                };
                hydraulic_edge_set = true;
            },
            "--hydraulic-checkpoint" => hydraulic_checkpoint = true,
            "--region-tiles" => {
                let value = it.next().ok_or("--region-tiles requires COUNT")?;
                let count: usize = value.parse()
                    .map_err(|_| "--region-tiles COUNT must be an integer")?;
                if !(2..=64).contains(&count) {
                    return Err("--region-tiles COUNT must be between 2 and 64".into());
                }
                region_tiles = Some(count);
            },
            "--project" => {
                project = Some(PathBuf::from(it.next().ok_or("--project requires a file")?));
            }
            "--output" => {
                output = Some(PathBuf::from(it.next().ok_or("--output requires a filename stem")?));
            }
            "--resolution" => {
                let value = it.next().ok_or("--resolution requires an integer")?;
                let size: usize = value.parse().map_err(|_| "resolution must be an integer")?;
                if !(32..=MAX_REGION_RESOLUTION).contains(&size) {
                    return Err(format!("resolution must be between 32 and {MAX_REGION_RESOLUTION}"));
                }
                resolution = Some(size);
            }
            "--all-outputs" => {
                let value = it.next().ok_or("--all-outputs requires a node ID")?;
                let id = value.parse::<u64>()
                    .map_err(|_| "all-outputs node ID must be numeric")?;
                if id == 0 {
                    return Err("all-outputs node ID must be positive".into());
                }
                if all_outputs.replace(id).is_some() {
                    return Err("--all-outputs can only be provided once".into());
                }
            }
            "--port" => {
                let value = it.next().ok_or("--port requires NODE:PORT")?;
                let (node, port) = value.split_once(':').ok_or("--port must be NODE:PORT")?;
                let node = node.parse::<u64>().map_err(|_| "port node must be numeric")?;
                if port.is_empty() || node == 0 {
                    return Err("port must have a positive node ID and non-empty name".into());
                }
                selected = Some(PortRef::new(node, port));
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    if !demo && project.is_none() {
        return Err("specify --project FILE or --demo".into());
    }
    if demo && project.is_some() {
        return Err("use either --demo or --project, not both".into());
    }
    if all_outputs.is_some() && selected.is_some() {
        return Err("--port and --all-outputs are mutually exclusive".into());
    }
    if hydraulic_region.is_some() {
        if region_tiles.is_none() || !no_obj || all_outputs.is_some() ||
            selected.is_some() || tiles.is_some() || demo
        {
            return Err("--hydraulic-region requires --project, --region-tiles COUNT and --no-obj; it cannot combine with --port, --all-outputs, --tiles or --demo".into());
        }
    } else if hydraulic_edge_set || hydraulic_checkpoint {
        return Err("--hydraulic-edge and --hydraulic-checkpoint require --hydraulic-region".into());
    }
    if region_tiles.is_some() {
        if tiles.is_some() || all_outputs.is_some() {
            return Err("--region-tiles cannot combine with --tiles or --all-outputs".into());
        }
        if !no_obj {
            return Err("--region-tiles requires --no-obj (no full mesh or PNG is produced)".into());
        }
     } else if resolution.is_some_and(|size|
        size > gaia::pipeline::MAX_BUILD_RESOLUTION)
    {
        return Err(format!("full-grid resolution cannot exceed {}",
            gaia::pipeline::MAX_BUILD_RESOLUTION));
    }
    let output = output.ok_or("--output STEM is required")?;
    if output.as_os_str().is_empty() {
        return Err("--output must be non-empty".into());
    }
    Ok(Some(Options {
        project, output, resolution, selected, all_outputs, demo, no_obj, tiles,
        region_tiles, hydraulic_region, hydraulic_edge, hydraulic_checkpoint,
    }))
}

fn suffixed(path: &Path, suffix: &str) -> Result<PathBuf, String> {
    let name = path.file_name().ok_or("Output stem must include a filename")?;
    Ok(path.with_file_name(format!("{}{suffix}", name.to_string_lossy())))
}

/// Build all named fields of one node with a single shared graph cache. This
/// is crucial for erosion/hydrology: wear, deposits, water and flow all come
/// from one simulation rather than running it again for each exported port.
fn build_all_outputs(
    project: &Project, options: &Options, node_id: u64, started: Instant,
) -> Result<(), String> {
    let node = project.graph.get(node_id)
        .ok_or_else(|| format!("Cannot export outputs: missing node #{node_id}"))?;
    let ports: Vec<PortRef> = node.kind.output_ports().iter()
        .map(|(name, _)| PortRef::new(node_id, *name))
        .collect();
    if ports.is_empty() {
        return Err(format!("Node #{node_id} declares no outputs"));
    }
    if let Some(parent) = options.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
    }
    let mut cache = GraphCache::default();
    let mut reports = Vec::with_capacity(ports.len());
    let (mut computed, mut reused) = (0_usize, 0_usize);
    for port in ports {
        let field = cache.evaluate(&project.graph, project.resolution, &port)?;
        computed += cache.stats.computed;
        reused += cache.stats.reused;
        let stem = suffixed(&options.output, &format!(".node{}.{}", node_id, port.port))?;
        let png = suffixed(&stem, ".png")?;
        let format = if field.kind == PortType::Splat {
            export_splat_rgba8(field.color.as_ref().ok_or("Splat weights missing RGBA data")?, &png)?;
            "RGBA8"
        } else if field.kind == PortType::Color {
            let color = field.color.as_ref()
                .ok_or("Color field has no RGBA source data")?;
            export_rgb8(color, &png)?;
            "RGB8"
        } else {
            export_png16(&field.data, &png)?;
            "Grayscale16"
        };

        let obj = if !options.no_obj && field.kind == PortType::Height {
            let file = suffixed(&stem, ".obj")?;
            export_obj(&field.data, &file)?;
            Some(file.display().to_string())
        } else {
            None
        };
        let tile_manifest = if let Some(count) = options.tiles {
            let layout = tile_layout(project.resolution, count)?;
            let folder = suffixed(&stem, ".tiles")?;
            std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
            for ty in 0..count {
                for tx in 0..count {
                    let file = folder.join(format!("x{tx:02}_y{ty:02}.png"));
                    if field.kind == PortType::Splat {
                        let colors = field.color.as_ref().ok_or("Splat weights missing RGBA data")?;
                        export_splat_rgba8(&tile_color(colors, layout, tx, ty)?, &file)?;
                    } else if field.kind == PortType::Color {
                        let color = field.color.as_ref()
                            .ok_or("Color field has no RGBA source data")?;
                        export_rgb8(&tile_color(color, layout, tx, ty)?, &file)?;
                    } else {
                        export_png16(&tile_height(&field.data, layout, tx, ty)?, &file)?;
                    }
                }
            }
            let manifest_path = suffixed(&stem, ".tiles.json")?;
            let manifest = json!({
                "tiles_per_axis": count,
                "tile_count": count * count,
                "pixel_size": layout.pixels_per_tile,
                "overlap_pixels": layout.overlap_pixels,
                "stitched_pixels": layout.stitched_pixels,
                "source_pixels": project.resolution,
                "format": format,
                "folder": folder.display().to_string(),
                "file_pattern": "x{XX}_y{YY}.png",
                "origin": "top-left, x increases right, y increases downward",
                "sampling": "global bilinear resampling from one completed graph",
                "processing": "complete graph is evaluated first (not streamed)"
            });
            std::fs::write(&manifest_path,
                serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?
            ).map_err(|e| e.to_string())?;
            Some(manifest_path.display().to_string())
        } else { None };
        let (min, max, sum) = field.data.values.iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY, 0.0_f64), |(low, high, sum), &v| {
                (low.min(v), high.max(v), sum + f64::from(v))
            });
        reports.push(json!({
            "node": node_id,
            "port": port.port,
            "type": field.kind.label(),
            "resolution": project.resolution,
            "png_format": format,
            "png": png.display().to_string(),
            "obj": obj,
            "tile_manifest": tile_manifest,
            "statistics_channel": if field.kind == PortType::Splat {
                "RGB weighted proxy; A not included"
            } else if field.kind == PortType::Color {
                "Rec709 luminance proxy"
            } else { "raw scalar" },
            "min_raw": min,
            "max_raw": max,
            "mean_raw": sum / field.data.values.len() as f64,
        }));
        println!("Built {}² #{}:{} ({format}) → {}",
            project.resolution, node_id, port.port, png.display());
    }

    let report = json!({
        "application": "gaia",
        "version": env!("CARGO_PKG_VERSION"),
        "mode": "all_node_outputs",
        "resolution": project.resolution,
        "graph_node_count": project.graph.nodes.len(),
        "node": node_id,
        "computed_nodes": computed,
        "reused_nodes": reused,
        "output_count": reports.len(),
        "outputs": reports,
        "duration_ms": started.elapsed().as_millis(),
    });
    let report_file = suffixed(&options.output, ".batch.report.json")?;
    std::fs::write(&report_file,
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?
    ).map_err(|e| e.to_string())?;
    println!("Batch report: {}", report_file.display());
    Ok(())
}

/// Independent world-coordinate Height/Mask/Color/Splat regions; each tile is
/// exported and released before the next one, without full-grid evaluation.
fn build_region_tiles(project: &Project, options: &Options,
    source: &PortRef, started: Instant,
) -> Result<(), String> {
    let count = options.region_tiles.ok_or("Missing region tile count")?;
    let layout = RegionLayout::new(project.resolution, count)?;
    // Preflight the entire connected dependency path before creating output
    // files. Unsupported global operators never trigger a full-grid fallback.
    validate_region_graph(&project.graph, source)?;
    let kind = project.graph.get(source.node)
        .and_then(|node| node.kind.output_ports().iter()
            .find(|(port, _)| *port == source.port).map(|(_, kind)| *kind))
        .ok_or("Cannot resolve region output type")?;
    if !matches!(kind, PortType::Height | PortType::Mask | PortType::Color | PortType::Splat) {
        return Err("Only Height/Mask/Color/Splat regions are available".into());
    }
    let format = match kind {
        PortType::Color => "RGB8",
        PortType::Splat => "RGBA8",
        _ => "Grayscale16",
    };
    let directory = options.output.with_extension("region.tiles");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let mut tiles = Vec::with_capacity(count * count);
    let mut raw_min = f32::INFINITY;
    let mut raw_max = f32::NEG_INFINITY;
    let mut raw_sum = 0.0_f64;
    let mut unique_samples = 0usize;
    let mut peak_samples = 0usize;
    let mut computed_regions = 0usize;
    for ty in 0..count {
        for tx in 0..count {
            let rect = layout.tile(tx, ty)?;
            let (scalar, rgb, stats) = if matches!(kind, PortType::Color | PortType::Splat) {
                let (field, stats) = evaluate_color_region(
                    &project.graph, project.resolution, source, rect)?;
                (None, Some(field), stats)
            } else {
                let (field, stats) = evaluate_region(
                    &project.graph, project.resolution, source, rect)?;
                debug_assert_eq!(field.kind, kind);
                (Some(field), None, stats)
            };
            peak_samples = peak_samples.max(stats.peak_working_samples);
            computed_regions += stats.computed_regions;
            // Adjacent tiles repeat exactly one border sample. Count it once
            // in the global summary. Color uses the full-grid Rec709 RGB proxy.
            for y in (if ty == 0 { 0 } else { 1 })..rect.height {
                for x in (if tx == 0 { 0 } else { 1 })..rect.width {
                    let i = y * rect.width + x;
                    let value = if let Some(color) = &rgb {
                        let pixel = color.pixels[i];
                        pixel[0] * 0.2126 + pixel[1] * 0.7152 + pixel[2] * 0.0722
                    } else {
                        scalar.as_ref().expect("scalar region missing").values[i]
                    };
                    raw_min = raw_min.min(value);
                    raw_max = raw_max.max(value);
                    raw_sum += f64::from(value);
                    unique_samples += 1;
                }
            }
            let filename = format!("x{tx:02}_y{ty:02}.png");
            let filepath = directory.join(&filename);
            if let Some(color) = rgb {
                if kind == PortType::Splat {
                    color.export_splat_rgba8(&filepath)?;
                } else {
                    color.export_rgb8(&filepath)?;
                }
            } else {
                scalar.expect("scalar region missing").export_png16(&filepath)?;
            }
            tiles.push(json!({
                "file": filename,
                "x": rect.x, "y": rect.y,
                "width": rect.width, "height": rect.height,
            }));
        }
    }
    debug_assert_eq!(unique_samples, project.resolution * project.resolution);
    let manifest = options.output.with_extension("region.tiles.json");
    let manifest_json = json!({
        "execution": "region-evaluated, bounded-memory, no full-grid buffers",
        "resolution": project.resolution,
        "tiles_per_axis": count,
        "tile_count": tiles.len(),
        "overlap_pixels": 1,
        "origin": "top-left, integer world samples",
        "node": source.node, "port": source.port,
        "type": kind.label(),
        "format": format,
        "folder": directory.display().to_string(),
        "tiles": tiles,
    });
    std::fs::write(&manifest,
        serde_json::to_vec_pretty(&manifest_json).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let report = options.output.with_extension("region.report.json");
    std::fs::write(&report, serde_json::to_vec_pretty(&json!({
        "resolution": project.resolution,
        "node": source.node, "port": source.port,
        "type": kind.label(), "format": format,
        "statistics_channel": if kind == PortType::Splat {
            "RGB weighted proxy; A not included"
        } else if kind == PortType::Color {
            "Rec709 luminance proxy"
        } else { "raw scalar" },
        "min_raw": raw_min, "max_raw": raw_max,
        "mean_raw": raw_sum / unique_samples as f64,
        "unique_samples": unique_samples,
        "peak_intermediate_samples": peak_samples,
        "peak_intermediate_bytes": peak_samples * std::mem::size_of::<f32>(),
        "computed_node_regions": computed_regions,
        "tile_manifest": manifest.display().to_string(),
        "stitched_full_png": null,
        "obj": null,
        "duration_ms": started.elapsed().as_millis(),
    })).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("Region-built {}² {} #{}:{} into {} tiles → {}",
        project.resolution, kind.label(), source.node, source.port,
        count * count, directory.display());
    println!("Report: {}", report.display());
    Ok(())
}

fn build(options: Options) -> Result<(), String> {
    let started = Instant::now();
    let mut project = if options.demo {
        Project::default()
    } else {
        Project::load(options.project.as_ref().expect("parser requires path"))?
    };
    if let Some(size) = options.resolution {
        project.resolution = size;
    }
    if let Some(node) = options.all_outputs {
        return build_all_outputs(&project, &options, node, started);
    }
    if let Some(node) = options.hydraulic_region {
        let folder = build_hydraulic_regions(
            &project, node,
            options.region_tiles.ok_or("Missing region tile count")?,
            &options.output, options.hydraulic_edge, options.hydraulic_checkpoint,
        )?;
        println!("Hydraulic region build → {}", folder.display());
        println!("Report: {}", folder.join("report.json").display());
        return Ok(());
    }
    let output = options.selected.clone()
        .unwrap_or_else(|| PortRef::height(project.graph.output));
    if options.region_tiles.is_some() {
        return build_region_tiles(&project, &options, &output, started);
    }
    let field = project.graph.evaluate_port(project.resolution, &output)?;
    if let Some(parent) = options.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let png = options.output.with_extension("png");
    let png_format = if field.kind == PortType::Splat {
        export_splat_rgba8(field.color.as_ref().ok_or("Splat weights missing RGBA data")?, &png)?;
        "RGBA8"
    } else if field.kind == PortType::Color {
        let color = field.color.as_ref().ok_or("Color output has no RGBA field")?;
        export_rgb8(color, &png)?;
        "RGB8"
    } else {
        export_png16(&field.data, &png)?;
        "Grayscale16"
    };
    let mut obj = None;
    if !options.no_obj && field.kind == PortType::Height {
        let file = options.output.with_extension("obj");
        export_obj(&field.data, &file)?;
        obj = Some(file.display().to_string());
    }
    // A single world-consistent graph build is subdivided *after* evaluation,
    // preventing per-tile erosion, normalization or color discontinuities.
    let tile_metadata = if let Some(count) = options.tiles {
        let layout = tile_layout(project.resolution, count)?;
        let directory = options.output.with_extension("tiles");
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        for ty in 0..count {
            for tx in 0..count {
                let file = directory.join(format!("x{tx:02}_y{ty:02}.png"));
                if field.kind == PortType::Splat {
                    let colors = field.color.as_ref().ok_or("Splat weights missing RGBA data")?;
                    export_splat_rgba8(&tile_color(colors, layout, tx, ty)?, &file)?;
                } else if field.kind == PortType::Color {
                    let color = field.color.as_ref()
                        .ok_or("Color output has no RGBA data for tiling")?;
                    export_rgb8(&tile_color(color, layout, tx, ty)?, &file)?;
                } else {
                    export_png16(&tile_height(&field.data, layout, tx, ty)?, &file)?;
                }
            }
        }
        let manifest_path = options.output.with_extension("tiles.json");
        let manifest = json!({
            "tiles_per_axis": count,
            "tile_count": count * count,
            "pixel_size": layout.pixels_per_tile,
            "overlap_pixels": layout.overlap_pixels,
            "stitched_pixels": layout.stitched_pixels,
            "source_pixels": project.resolution,
            "format": png_format,
            "folder": directory.display().to_string(),
            "file_pattern": "x{XX}_y{YY}.png",
            "origin": "top-left, x increases right, y increases downward",
            "sampling": "full-grid world-space bilinear, uniform resampling",
            "processing": "complete graph is evaluated first (not streaming/tiled evaluation)",
        });
        std::fs::write(&manifest_path,
            serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?
        ).map_err(|e| e.to_string())?;
        Some(manifest_path.display().to_string())
    } else {
        None
    };
    let (min, max, sum) = field.data.values.iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY, 0.0_f64), |(min, max, sum), &v| {
            (min.min(v), max.max(v), sum + f64::from(v))
        });
    let report = json!({
        "application": "gaia",
        "version": env!("CARGO_PKG_VERSION"),
        "resolution": project.resolution,
        "graph_node_count": project.graph.nodes.len(),
        "node": output.node,
        "port": output.port.clone(),
        "type": field.kind.label(),
        "min_raw": min,
        "max_raw": max,
        "mean_raw": sum / field.data.values.len() as f64,
        "png": png.display().to_string(),
        "png_format": png_format,
        "png16": if png_format == "Grayscale16" { Some(png.display().to_string()) } else { None },
        "rgb8": if png_format == "RGB8" { Some(png.display().to_string()) } else { None },
        "rgba8": if png_format == "RGBA8" { Some(png.display().to_string()) } else { None },
        "statistics_channel": match png_format {
            "RGB8" => "Rec709 luminance proxy",
            "RGBA8" => "RGB weight proxy, alpha excluded",
            _ => "raw scalar",
        },
        "obj": obj,
        "tile_manifest": tile_metadata,
        "png_normalization": if png_format == "RGBA8" {
            "normalize weights then largest-remainder quantize to RGBA8 summing exactly 255"
        } else if png_format == "RGB8" {
            "clamp RGB channels to [0,1] and multiply by 255; discard alpha"
        } else {
            "clamp raw values to [0,1] and multiply by 65535"
        },
        "duration_ms": started.elapsed().as_millis(),
    });
    let report_path = options.output.with_extension("report.json");
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report)
        .map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("Built {}² {} #{}:{} → {}",
        project.resolution, field.kind.label(), output.node, output.port,
        png.display());
    println!("Report: {}", report_path.display());
    Ok(())
}

fn main() {
    match parse_args(env::args().skip(1)) {
        Ok(None) => print!("{HELP}"),
        Ok(Some(options)) => {
            if let Err(error) = build(options) {
                eprintln!("Gaia build error: {error}");
                process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("{error}\n\n{HELP}");
            process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_cli_requires_no_mesh_and_disallows_full_grid_fallbacks() {
        let args = |extra: &[&str]| {
            let mut a = vec!["--demo".to_string(), "--output".to_string(),
                "terrain".to_string()];
            a.extend(extra.iter().map(|x| x.to_string()));
            a
        };
        assert!(parse_args(args(&["--resolution", "4096"])).is_err());
        assert_eq!(parse_args(args(&["--no-obj", "--region-tiles", "8",
            "--port", "1:height", "--resolution", "4096"]))
            .unwrap().unwrap().resolution, Some(4096));
        assert!(parse_args(args(&["--region-tiles", "4", "--no-obj",
            "--tiles", "4"])).is_err());
        assert!(parse_args(args(&["--region-tiles", "4"])).is_err());
        assert!(parse_args(args(&["--region-tiles", "65", "--no-obj"])).is_err());
        assert!(parse_args(args(&["--region-tiles", "2", "--no-obj",
            "--all-outputs", "1"])).is_err());
        // Named masks are parsed, and their graph semantics are checked
        // during preflight so Mask ports may legitimately be exported.
        assert!(parse_args(args(&["--region-tiles", "2", "--no-obj",
            "--port", "2:wear"])).is_ok());
    }

    #[test]
    fn region_cli_exports_real_png_tiles_with_exact_global_edge_coordinates() {
        let temp = tempfile::tempdir().unwrap();
        let stem = temp.path().join("mountain");
        let opts = parse_args(vec![
            "--demo".into(), "--output".into(),
            stem.to_string_lossy().into_owned(), "--resolution".into(),
            "65".into(), "--port".into(), "1:height".into(),
            "--region-tiles".into(), "3".into(), "--no-obj".into(),
        ]).unwrap().unwrap();
        build(opts).unwrap();
        assert!(!stem.with_extension("png").exists());
        assert!(!stem.with_extension("obj").exists());
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("region.tiles.json")).unwrap()).unwrap();
        assert_eq!(manifest["tile_count"], 9);
        assert_eq!(manifest["resolution"], 65);
        assert_eq!(manifest["tiles"][0]["x"], 0);
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("region.report.json")).unwrap()).unwrap();
        assert_eq!(report["unique_samples"], 65*65);
        assert!(report["peak_intermediate_samples"].as_u64().unwrap() < 65*65);
        let folder = stem.with_extension("region.tiles");
        let read = |name: &str| {
            let mut reader = png::Decoder::new(std::io::BufReader::new(
                std::fs::File::open(folder.join(name)).unwrap()))
                .read_info().unwrap();
            let mut data = vec![0_u8; reader.output_buffer_size()];
            let info = reader.next_frame(&mut data).unwrap();
            (info.width as usize, info.height as usize, data)
        };
        let (aw, ah, a) = read("x00_y00.png");
        let (bw, bh, b) = read("x01_y00.png");
        assert_eq!(ah, bh);
        for row in 0..ah {
            assert_eq!(&a[(row*aw+aw-1)*2..(row*aw+aw)*2],
                &b[(row*bw)*2..(row*bw+1)*2]);
        }
    }

    #[test]
    fn region_cli_exports_a_named_mask_and_reports_its_real_type() {
        use gaia::terrain::NodeKind;
        let temp = tempfile::tempdir().unwrap();
        let scene = temp.path().join("mask-project.gaia.json");
        let stem = temp.path().join("mask-output");
        let mut project = Project::default();
        let noise = project.graph.add(NodeKind::Noise {
            seed: 31, frequency: 7.8, amplitude: 0.75,
        });
        let slope = project.graph.add(NodeKind::Slope);
        project.graph.connect(slope, 0, PortRef::height(noise)).unwrap();
        project.resolution = 65;
        project.save(&scene).unwrap();
        let source = PortRef::new(slope, "slope");
        let opts = parse_args(vec![
            "--project".into(), scene.to_string_lossy().into_owned(),
            "--output".into(), stem.to_string_lossy().into_owned(),
            "--port".into(), format!("{slope}:slope"),
            "--region-tiles".into(), "3".into(), "--no-obj".into(),
        ]).unwrap().unwrap();
        build(opts).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("region.tiles.json")).unwrap()).unwrap();
        assert_eq!(manifest["type"], "Mask");
        assert_eq!(manifest["port"], "slope");
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("region.report.json")).unwrap()).unwrap();
        assert_eq!(report["type"], "Mask");
        assert_eq!(report["unique_samples"], 65 * 65);
        let expected = project.graph.evaluate_port(65, &source).unwrap();
        let folder = stem.with_extension("region.tiles");
        let read = |x: usize, y: usize| {
            let path = folder.join(format!("x{x:02}_y{y:02}.png"));
            let mut png = png::Decoder::new(std::io::BufReader::new(
                std::fs::File::open(path).unwrap())).read_info().unwrap();
            let mut bytes = vec![0_u8; png.output_buffer_size()];
            let info = png.next_frame(&mut bytes).unwrap();
            assert_eq!(info.color_type, png::ColorType::Grayscale);
            assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
            (info.width as usize, info.height as usize, bytes)
        };
        let (aw, ah, a) = read(0, 0);
        let (bw, bh, b) = read(1, 0);
        assert_eq!(ah, bh);
        for row in 0..ah {
            assert_eq!(&a[(row * aw + aw-1)*2..(row * aw + aw)*2],
                &b[(row * bw)*2..(row * bw + 1)*2]);
        }
        let rect = gaia::region::RegionLayout::new(65, 3).unwrap().tile(0, 0).unwrap();
        for y in 0..ah {
            for x in 0..aw {
                let value = expected.data.get(rect.x + x, rect.y + y);
                let encoded = (value.clamp(0.0, 1.0) * 65535.0).round() as u16;
                let at = 2 * (y * aw + x);
                assert_eq!(u16::from_be_bytes([a[at], a[at + 1]]), encoded);
            }
        }
    }

    #[test]
    fn region_cli_exports_rgb_color_tiles_without_global_evaluation() {
        use gaia::terrain::NodeKind;
        let temp = tempfile::tempdir().unwrap();
        let scene = temp.path().join("color.gaia.json");
        let stem = temp.path().join("color-output");
        let mut project = Project::default();
        project.resolution = 65;
        let height = project.graph.add(NodeKind::Noise {
            seed: 42, frequency: 5.0, amplitude: 0.8,
        });
        let mask = project.graph.add(NodeKind::Aspect {
            azimuth: 55.0, half_width: 80.0, min_slope: 0.1,
        });
        project.graph.connect(mask, 0, PortRef::height(height)).unwrap();
        let a = project.graph.add(NodeKind::Colorize {
            low: [0.1,0.2,0.9], high: [0.8,0.6,0.2],
            midpoint: 0.4, contrast: 1.5,
        });
        let b = project.graph.add(NodeKind::Colorize {
            low: [0.7,0.1,0.2], high: [0.3,0.9,0.5],
            midpoint: 0.6, contrast: 0.8,
        });
        project.graph.connect(a, 0, PortRef::height(height)).unwrap();
        project.graph.connect(b, 0, PortRef::height(height)).unwrap();
        let mixed = project.graph.add(NodeKind::ColorMixer { mix: 0.7 });
        project.graph.connect(mixed, 0, PortRef::new(a, "color")).unwrap();
        project.graph.connect(mixed, 1, PortRef::new(b, "color")).unwrap();
        project.graph.connect(mixed, 2, PortRef::new(mask, "aspect")).unwrap();
        project.save(&scene).unwrap();
        build(parse_args(vec![
            "--project".into(), scene.to_string_lossy().into_owned(),
            "--output".into(), stem.to_string_lossy().into_owned(),
            "--port".into(), format!("{mixed}:color"),
            "--no-obj".into(), "--region-tiles".into(), "3".into(),
        ]).unwrap().unwrap()).unwrap();
        assert!(!stem.with_extension("png").exists());
        assert!(!stem.with_extension("obj").exists());
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("region.tiles.json")).unwrap()).unwrap();
        assert_eq!(manifest["format"], "RGB8");
        assert_eq!(manifest["type"], "Color");
        assert_eq!(manifest["tile_count"], 9);
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("region.report.json")).unwrap()).unwrap();
        assert_eq!(report["type"], "Color");
        assert_eq!(report["unique_samples"], 65 * 65);
        assert_eq!(report["statistics_channel"], "Rec709 luminance proxy");
        let expected = project.graph.evaluate_port(65,
            &PortRef::new(mixed, "color")).unwrap().color.unwrap();
        let layout = RegionLayout::new(65, 3).unwrap();
        let mut edges = Vec::new();
        for ty in 0..3 {
            for tx in 0..3 {
                let rect = layout.tile(tx, ty).unwrap();
                let path = stem.with_extension("region.tiles")
                    .join(format!("x{tx:02}_y{ty:02}.png"));
                let mut reader = png::Decoder::new(std::io::BufReader::new(
                    std::fs::File::open(path).unwrap())).read_info().unwrap();
                let mut bytes = vec![0; reader.output_buffer_size()];
                let info = reader.next_frame(&mut bytes).unwrap();
                assert_eq!(info.color_type, png::ColorType::Rgb);
                assert_eq!(info.bit_depth, png::BitDepth::Eight);
                assert_eq!(info.width as usize, rect.width);
                assert_eq!(info.height as usize, rect.height);
                for y in 0..rect.height {
                    for x in 0..rect.width {
                        let expected_pixel = expected.pixels[
                            (rect.y + y)*65 + rect.x + x];
                        for c in 0..3 {
                            assert_eq!(bytes[(y*rect.width+x)*3+c],
                                (expected_pixel[c].clamp(0.0,1.0)*255.0).round() as u8);
                        }
                    }
                }
                edges.push((rect, bytes));
            }
        }
        let (left, a) = &edges[0];
        let (right, b) = &edges[1];
        for row in 0..left.height {
            assert_eq!(&a[(row*left.width+left.width-1)*3..(row*left.width+left.width)*3],
                &b[(row*right.width)*3..(row*right.width+1)*3]);
        }
    }

    #[test]
    fn region_cli_exports_material_splat_rgba8_with_exact_255_weight_sum() {
        use gaia::terrain::NodeKind;
        let temp = tempfile::tempdir().unwrap();
        let stem = temp.path().join("material-splat");
        let scene = temp.path().join("materials.gaia.json");
        let mut project = Project::default();
        project.resolution = 65;
        let material = project.graph.add_from(NodeKind::MaterialMasks {
            sea_level: 0.25, beach_width: 0.08,
            rock_slope: 0.32, rock_blend: 0.10,
            snowline: 0.8, snow_blend: 0.12,
            palette: gaia::terrain::MaterialPalette::default(),
        }, &PortRef::height(1));
        project.save(&scene).unwrap();
        let source = PortRef::new(material, "splat");
        build(parse_args(vec![
            "--project".into(), scene.to_string_lossy().into_owned(),
            "--output".into(), stem.to_string_lossy().into_owned(),
            "--port".into(), format!("{material}:splat"),
            "--no-obj".into(), "--region-tiles".into(), "3".into(),
        ]).unwrap().unwrap()).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("region.tiles.json")).unwrap()).unwrap();
        assert_eq!(manifest["type"], PortType::Splat.label());
        assert_eq!(manifest["format"], "RGBA8");
        assert_eq!(manifest["tile_count"], 9);
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("region.report.json")).unwrap()).unwrap();
        assert_eq!(report["unique_samples"], 65*65);
        assert_eq!(report["statistics_channel"], "RGB weighted proxy; A not included");
        let expected = project.graph.evaluate_port(65, &source).unwrap().color.unwrap();
        let layout = RegionLayout::new(65, 3).unwrap();
        for ty in 0..3 {
            for tx in 0..3 {
                let rect = layout.tile(tx, ty).unwrap();
                let path = stem.with_extension("region.tiles")
                    .join(format!("x{tx:02}_y{ty:02}.png"));
                let mut reader = png::Decoder::new(std::io::BufReader::new(
                    std::fs::File::open(path).unwrap())).read_info().unwrap();
                let mut bytes = vec![0_u8; reader.output_buffer_size()];
                let info = reader.next_frame(&mut bytes).unwrap();
                assert_eq!(info.color_type, png::ColorType::Rgba);
                assert_eq!(info.bit_depth, png::BitDepth::Eight);
                assert_eq!(info.width as usize, rect.width);
                assert_eq!(info.height as usize, rect.height);
                for y in 0..rect.height {
                    for x in 0..rect.width {
                        let i = y*rect.width+x;
                        let rgba = &bytes[i*4..i*4+4];
                        let exp = expected.pixels[(rect.y+y)*65+rect.x+x];
                        for c in 0..4 {
                            assert!((rgba[c] as f32 / 255.0 - exp[c]).abs() <= 1.0 / 255.0 + 1e-6);
                        }
                        assert_eq!(rgba.iter().map(|&v| v as u16).sum::<u16>(), 255);
                    }
                }
            }
        }
    }

    #[test]
    fn region_cli_rejects_global_erosion_before_creating_output_folder() {
        let temp = tempfile::tempdir().unwrap();
        let stem = temp.path().join("blocked");
        let opts = parse_args(vec![
            "--demo".into(), "--output".into(),
            stem.to_string_lossy().into_owned(), "--no-obj".into(),
            "--region-tiles".into(), "4".into(),
        ]).unwrap().unwrap();
        assert!(build(opts).unwrap_err().contains("does not support"));
        assert!(!stem.with_extension("region.tiles").exists());
    }

    #[test]
    fn requires_valid_arguments() {
        assert!(parse_args(Vec::<String>::new()).is_err());
        assert!(parse_args(vec!["--demo".into()]).is_err());
        assert!(parse_args(vec![
            "--demo".into(), "--output".into(), "x".into(), "--resolution".into(),
            "4096".into()
        ]).is_err());
        assert_eq!(parse_args(vec![
            "--demo".into(), "--output".into(), "x".into(),
            "--resolution".into(), "2048".into()
        ]).unwrap().unwrap().resolution, Some(2048));
        assert!(parse_args(vec![
            "--demo".into(), "--output".into(), "x".into(),
            "--resolution".into(), "2049".into()
        ]).is_err());
        assert!(parse_args(vec!["--help".into()]).unwrap().is_none());
    }

    #[test]
    fn validates_tile_count_argument() {
        let args = |count: &str| vec![
            "--demo".into(), "--output".into(), "x".into(),
            "--tiles".into(), count.into(),
        ];
        assert_eq!(parse_args(args("2")).unwrap().unwrap().tiles, Some(2));
        assert_eq!(parse_args(args("8")).unwrap().unwrap().tiles, Some(8));
        assert!(parse_args(args("1")).is_err());
        assert!(parse_args(args("9")).is_err());
        assert!(parse_args(args("abc")).is_err());
    }

    #[test]
    fn batch_flag_validates_node_and_conflicting_selected_port() {
        let args = |tail: &[&str]| {
            let mut base = vec![
                "--demo".to_string(), "--output".to_string(), "x".to_string()
            ];
            base.extend(tail.iter().map(|s| s.to_string()));
            base
        };
        assert_eq!(parse_args(args(&["--all-outputs", "2"])).unwrap()
            .unwrap().all_outputs, Some(2));
        assert!(parse_args(args(&["--all-outputs", "0"])).is_err());
        assert!(parse_args(args(&["--all-outputs", "bogus"])).is_err());
        assert!(parse_args(args(&["--all-outputs", "2", "--port", "2:wear"])).is_err());
        assert!(parse_args(args(&["--all-outputs", "2", "--all-outputs", "3"])).is_err());
    }

    #[test]
    fn batch_builds_every_erosion_port_once_and_exports_compatible_tiles() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir()
            .join(format!("gaia-batch-{}-{unique}", process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let stem = root.join("erosion");
        build(Options {
            project: None,
            output: stem.clone(), resolution: Some(32),
            selected: None, all_outputs: Some(2),
            demo: true, no_obj: false, tiles: Some(2), region_tiles: None, hydraulic_region: None, hydraulic_edge: WorldEdge::Open, hydraulic_checkpoint: false,
        }).unwrap();
        let report_path = suffixed(&stem, ".batch.report.json").unwrap();
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(report_path).unwrap()).unwrap();
        assert_eq!(report["output_count"], 3);
        assert_eq!(report["computed_nodes"], 2);
        assert!(report["reused_nodes"].as_u64().unwrap() >= 4);
        assert_eq!(report["outputs"].as_array().unwrap().len(), 3);
        for port in ["height", "wear", "deposits"] {
            let base = suffixed(&stem, &format!(".node2.{port}")).unwrap();
            let image = suffixed(&base, ".png").unwrap();
            assert!(image.is_file());
            let input = std::fs::File::open(&image).unwrap();
            let reader = png::Decoder::new(std::io::BufReader::new(input));
            let image = reader.read_info().unwrap();
            assert_eq!(image.info().color_type, png::ColorType::Grayscale);
            assert_eq!(image.info().bit_depth, png::BitDepth::Sixteen);
            assert!(suffixed(&base, ".tiles.json").unwrap().exists());
            assert!(suffixed(&base, ".tiles").unwrap()
                .join("x01_y01.png").is_file());
        }
        assert!(suffixed(&suffixed(&stem, ".node2.height").unwrap(), ".obj")
            .unwrap().exists());
        assert!(!suffixed(&suffixed(&stem, ".node2.wear").unwrap(), ".obj")
            .unwrap().exists());
        let standalone = root.join("solo");
        build(Options {
            project: None,
            output: standalone.clone(), resolution: Some(32),
            selected: Some(PortRef::new(2, "wear")), all_outputs: None,
            demo: true, no_obj: true, tiles: None, region_tiles: None, hydraulic_region: None, hydraulic_edge: WorldEdge::Open, hydraulic_checkpoint: false,
        }).unwrap();
        assert_eq!(std::fs::read(
            suffixed(&suffixed(&stem, ".node2.wear").unwrap(), ".png").unwrap()
        ).unwrap(), std::fs::read(standalone.with_extension("png")).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn batch_color_port_uses_real_rgb8_export() {
        use gaia::terrain::NodeKind;
        let mut project = Project::default();
        project.resolution = 32;
        let node = project.graph.add(NodeKind::Colorize {
            low: [1.0,0.0,0.0], high: [0.0,0.0,1.0],
            midpoint: 0.5, contrast: 1.0,
        });
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir()
            .join(format!("gaia-batch-color-{}-{unique}", process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let scene = root.join("colors.gaia.json");
        project.save(&scene).unwrap();
        let stem = root.join("batch");
        build(Options {
            project: Some(scene),
            output: stem.clone(), resolution: None,
            selected: None, all_outputs: Some(node),
            demo: false, no_obj: false, tiles: None, region_tiles: None, hydraulic_region: None, hydraulic_edge: WorldEdge::Open, hydraulic_checkpoint: false,
        }).unwrap();
        let png = suffixed(&stem, &format!(".node{node}.color.png")).unwrap();
        let img = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(png).unwrap()
        )).read_info().unwrap();
        assert_eq!(img.info().color_type, png::ColorType::Rgb);
        assert_eq!(img.info().bit_depth, png::BitDepth::Eight);
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(suffixed(&stem, ".batch.report.json").unwrap()).unwrap()
        ).unwrap();
        assert_eq!(report["outputs"][0]["type"], "Color");
        assert_eq!(report["outputs"][0]["png_format"], "RGB8");
        assert!(report["outputs"][0]["obj"].is_null());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hydraulic_cli_requires_explicit_mode_and_rejects_conflicting_args() {
        let args = [
            "--project", "world.gaia.json", "--output", "build/eroded",
            "--hydraulic-region", "7", "--region-tiles", "4", "--no-obj",
            "--hydraulic-edge", "closed", "--hydraulic-checkpoint",
        ];
        let opts = parse_args(args.into_iter().map(str::to_owned)).unwrap().unwrap();
        assert_eq!(opts.hydraulic_region, Some(7));
        assert_eq!(opts.region_tiles, Some(4));
        assert_eq!(opts.hydraulic_edge, WorldEdge::Closed);
        assert!(opts.hydraulic_checkpoint);
        for invalid in [
            vec!["--project", "x", "--output", "x", "--hydraulic-region", "3",
                 "--region-tiles", "2"],
            vec!["--project", "x", "--output", "x", "--hydraulic-region", "3",
                 "--no-obj"],
            vec!["--project", "x", "--output", "x", "--hydraulic-region", "3",
                 "--region-tiles", "2", "--no-obj", "--port", "1:height"],
            vec!["--demo", "--output", "x", "--region-tiles", "2",
                 "--no-obj", "--hydraulic-region", "3"],
            vec!["--project", "x", "--output", "x", "--hydraulic-checkpoint"],
        ] {
            assert!(parse_args(invalid.into_iter().map(str::to_owned)).is_err());
        }
    }

    #[test]
    fn hydraulic_cli_builds_from_saved_project_and_exports_six_fields() {
        use gaia::hydrology::HydraulicParameters;
        use gaia::terrain::{Node, NodeKind};
        let dir = tempfile::tempdir().unwrap();
        let scene = dir.path().join("project.gaia.json");
        let stem = dir.path().join("eroded");
        let mut project = Project::default();
        project.resolution = 32;
        project.graph.nodes.push(Node {
            id: 7,
            kind: NodeKind::Hydraulic {
                params: HydraulicParameters {
                    iterations: 2, ..Default::default()
                },
            },
            inputs: vec![PortRef::height(1)],
            position: [600.0, 100.0],
        });
        project.graph.output = 7;
        project.save(&scene).unwrap();
        let options = parse_args(vec![
            "--project".into(), scene.display().to_string(),
            "--output".into(), stem.display().to_string(),
            "--hydraulic-region".into(), "7".into(),
            "--region-tiles".into(), "2".into(), "--no-obj".into(),
        ]).unwrap().unwrap();
        build(options).unwrap();
        let folder = stem.with_extension("hydraulic.tiles");
        for channel in ["height", "water", "sediment", "wear", "deposits", "flow"] {
            assert!(folder.join(channel).join("x00_y00.png").exists());
        }
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(folder.join("report.json")).unwrap(),
        ).unwrap();
        assert_eq!(report["hydraulic_node"], 7);
        assert_eq!(report["iterations"], 2);
        assert_eq!(report["tile_count"], 4);
        assert!(report["solid_ledger"]["residual"].as_f64().unwrap().abs() < 0.01);
    }

    #[test]
    fn accepts_mask_output_without_mesh() {
        let options = parse_args(vec![
            "--demo".into(), "--output".into(), "out".into(),
            "--port".into(), "2:wear".into(), "--no-obj".into(),
        ]).unwrap().unwrap();
        assert_eq!(options.selected.unwrap(), PortRef::new(2, "wear"));
        assert!(options.no_obj);
    }

    #[test]
    fn headless_color_port_exports_real_rgb8_not_luminance_png16() {
        use gaia::terrain::NodeKind;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!(
            "gaia-cli-rgb-{}-{unique}", process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut project = Project::default();
        project.resolution = 32;
        let color_id = project.graph.add(NodeKind::Colorize {
            low: [1.0, 0.0, 0.0],
            high: [1.0, 0.0, 0.0],
            midpoint: 0.5, contrast: 1.0,
        });
        let stem = root.join("terrain_color");
        let scene = root.join("color.gaia.json");
        project.save(&scene).unwrap();
        build(Options {
            project: Some(scene), output: stem.clone(),
            resolution: None, selected: Some(PortRef::new(color_id, "color")),
            all_outputs: None, demo: false, no_obj: false, tiles: None, region_tiles: None, hydraulic_region: None, hydraulic_edge: WorldEdge::Open, hydraulic_checkpoint: false,
        }).unwrap();
        let file = std::fs::File::open(stem.with_extension("png")).unwrap();
        let reader = png::Decoder::new(std::io::BufReader::new(file));
        let mut png = reader.read_info().unwrap();
        let mut buffer = vec![0; png.output_buffer_size()];
        let info = png.next_frame(&mut buffer).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        assert_eq!(&buffer[0..3], &[255, 0, 0]);
        assert!(!stem.with_extension("obj").exists());
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("report.json")).unwrap()
        ).unwrap();
        assert_eq!(report["png_format"], "RGB8");
        assert_eq!(report["type"], "Color");
        assert!(report["png16"].is_null());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn builds_scene_with_project_relative_heightmap_from_other_directory() {
        use gaia::terrain::{HeightField, NodeKind};
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!(
            "gaia-cli-import-{}-{unique}", process::id()
        ));
        let assets = root.join("assets");
        let scenes = root.join("scenes");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::create_dir_all(&scenes).unwrap();
        let image = assets.join("terrain.png");
        export_png16(&HeightField::flat(32, 0.375), &image).unwrap();
        let mut project = Project::default();
        project.resolution = 32;
        project.graph.add(NodeKind::ImportHeight {
            path: image.to_string_lossy().into_owned(),
            scale: 1.0,
        });
        let scene = scenes.join("example.gaia.json");
        project.save(&scene).unwrap();

        let stem = root.join("output").join("final");
        build(Options {
            project: Some(scene),
            output: stem.clone(),
            resolution: None,
            selected: None,
            all_outputs: None,
            demo: false,
            no_obj: true,
            tiles: None, region_tiles: None, hydraulic_region: None, hydraulic_edge: WorldEdge::Open, hydraulic_checkpoint: false,
        }).unwrap();
        assert!(stem.with_extension("png").exists());
        let restored = gaia::import::read_heightmap(
            &stem.with_extension("png"), 32, 1.0
        ).unwrap();
        assert!(restored.values.iter().all(|&v| (v - 0.375).abs() < 2e-5));
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("report.json")).unwrap()
        ).unwrap();
        assert_eq!(report["type"], "Height");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn headless_tiled_export_has_matching_png16_borders_and_manifest() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(
            format!("gaia-cli-tiles-{}-{unique}", process::id()));
        let stem = root.join("terrain");
        build(Options {
            project: None, output: stem.clone(), resolution: Some(32),
            selected: None, all_outputs: None, demo: true, no_obj: true, tiles: Some(2), region_tiles: None, hydraulic_region: None, hydraulic_edge: WorldEdge::Open, hydraulic_checkpoint: false,
        }).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("tiles.json")).unwrap()
        ).unwrap();
        assert_eq!(manifest["tiles_per_axis"], 2);
        assert_eq!(manifest["tile_count"], 4);
        assert_eq!(manifest["overlap_pixels"], 1);
        assert_eq!(manifest["stitched_pixels"], 33);
        let dir = stem.with_extension("tiles");
        let read = |name: &str| {
            let file = std::fs::File::open(dir.join(name)).unwrap();
            let mut reader = png::Decoder::new(std::io::BufReader::new(file))
                .read_info().unwrap();
            let mut pixels = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut pixels).unwrap();
            assert_eq!(info.color_type, png::ColorType::Grayscale);
            assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
            assert_eq!(info.width, 17);
            pixels
        };
        let a = read("x00_y00.png");
        let b = read("x01_y00.png");
        let c = read("x00_y01.png");
        let pixels = 17;
        let stride = pixels * 2;
        for row in 0..pixels {
            assert_eq!(
                &a[row*stride+(pixels-1)*2..row*stride+pixels*2],
                &b[row*stride..row*stride+2]
            );
            assert_eq!(
                &a[(pixels-1)*stride+row*2..(pixels-1)*stride+row*2+2],
                &c[row*2..row*2+2]
            );
        }
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("report.json")).unwrap()
        ).unwrap();
        assert!(report["tile_manifest"].as_str().unwrap().ends_with("terrain.tiles.json"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn builds_without_gpu_or_window() {
        let path = std::env::temp_dir().join(format!("gaia-cli-test-{}", process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let stem = path.join("terrain");
        build(Options {
            project: None,
            output: stem.clone(),
            resolution: Some(32),
            selected: None,
            all_outputs: None,
            demo: true,
            no_obj: false,
            tiles: None, region_tiles: None, hydraulic_region: None, hydraulic_edge: WorldEdge::Open, hydraulic_checkpoint: false,
        }).unwrap();
        assert!(stem.with_extension("png").exists());
        assert!(stem.with_extension("obj").exists());
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("report.json")).unwrap()
        ).unwrap();
        assert_eq!(report["resolution"], 32);
        std::fs::remove_dir_all(&path).unwrap();
    }
}
