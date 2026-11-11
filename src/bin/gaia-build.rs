//! Standalone deterministic terrain build tool.
//! No window or GPU required; intended for production pipelines and CI.
use gaia::pipeline::{GraphCache, PortRef, PortType};
use gaia::color::{export_rgb8, export_splat_rgba8};
use gaia::tiles::{tile_layout, tile_height, tile_color};
use gaia::region::{evaluate_region, validate_region_graph, RegionLayout, MAX_REGION_RESOLUTION};
use gaia::terrain::{export_obj, export_png16, Project};
use serde_json::json;
use std::{env, path::{Path, PathBuf}, process, time::Instant};

const HELP: &str = r#"Gaia headless terrain builder
Usage: gaia-build --project FILE --output STEM [--resolution N] [--port NODE:PORT] [--no-obj] [--tiles COUNT]
       gaia-build --project FILE --output STEM --all-outputs NODE [--resolution N] [--no-obj] [--tiles COUNT]
       gaia-build --demo --output STEM [--resolution N] [--no-obj] [--tiles COUNT]
       gaia-build --project FILE --output STEM --port NODE:height --no-obj --region-tiles COUNT [--resolution N]

Examples:
  gaia-build --project terrain.gaia.json --output builds/mountain
  gaia-build --project terrain.gaia.json --output builds/wear --port 2:wear --no-obj
  gaia-build --demo --output builds/sample --resolution 256
  gaia-build --project terrain.gaia.json --output builds/land --tiles 4
  gaia-build --project terrain.gaia.json --output builds/erosion --all-outputs 2
  gaia-build --demo --port 1:height --output builds/mountain-regions --no-obj --region-tiles 4 --resolution 4096

Exports PNG16 for scalar fields, RGB8 for Color, RGBA8 for Splat weights,
and OBJ for height outputs.
Writes STEM.report.json. --tiles produces N×N PNGs in STEM.tiles/ plus
STEM.tiles.json, each tile sharing one identical border pixel with neighbors.
This subdivides an already-built field; it does not reduce graph build memory.
--all-outputs NODE exports every named output of a node using one shared
evaluation cache and writes STEM.batch.report.json. Do not combine with --port.
Regular resolution must be 32–2048; --tiles COUNT 2–8 splits a FULL graph.
--region-tiles COUNT 2–64 instead evaluates Height output regions directly,
up to 8192 resolution, with exact shared border samples and bounded working
memory. This mode requires --no-obj, and does not output a stitched PNG/mesh.
Supports only local Mountain, Noise, Adjust (autolevel OFF), Blur and Blend
ancestors. Erosion, hydrology, autolevel and other global nodes are rejected
explicitly (never silently fall back to full-grid evaluation). Desktop
previews remain at up to 512.
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
    if region_tiles.is_some() {
        if tiles.is_some() || all_outputs.is_some() {
            return Err("--region-tiles cannot combine with --tiles or --all-outputs".into());
        }
        if !no_obj {
            return Err("--region-tiles requires --no-obj (no full mesh or PNG is produced)".into());
        }
        if selected.as_ref().is_some_and(|port| port.port != "height") {
            return Err("--region-tiles currently supports only NODE:height".into());
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
        region_tiles,
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

/// Independent, world-coordinate Height regions. Each tile finishes and is
/// dropped before the next one is requested; no full-resolution heightfield
/// and no full-grid evaluation cache are constructed.
fn build_region_tiles(project: &Project, options: &Options,
    source: &PortRef, started: Instant,
) -> Result<(), String> {
    let count = options.region_tiles.ok_or("Missing region tile count")?;
    let layout = RegionLayout::new(project.resolution, count)?;
    // Preflight prior to creating directories/files: never silently allocate
    // a complete full-field fallback for a globally coupled operator.
    validate_region_graph(&project.graph, source)?;
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
            let (tile, stats) = evaluate_region(
                &project.graph, project.resolution, source, rect)?;
            peak_samples = peak_samples.max(stats.peak_working_samples);
            computed_regions += stats.computed_regions;
            // Neighbor tiles repeat exactly one border sample. Count it
            // only once when reporting world-wide min/max/mean.
            for y in (if ty == 0 { 0 } else { 1 })..rect.height {
                for x in (if tx == 0 { 0 } else { 1 })..rect.width {
                    let value = tile.values[y * rect.width + x];
                    raw_min = raw_min.min(value);
                    raw_max = raw_max.max(value);
                    raw_sum += f64::from(value);
                    unique_samples += 1;
                }
            }
            let filename = format!("x{tx:02}_y{ty:02}.png");
            tile.export_png16(&directory.join(&filename))?;
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
        "format": "Grayscale16",
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
        "type": "Height", "format": "Grayscale16",
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
    println!("Region-built {}² Height #{}:{} into {} tiles → {}",
        project.resolution, source.node, source.port, count * count, directory.display());
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
    fn region_cli_requires_explicit_local_height_and_disallows_full_grid_fallbacks() {
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
        assert!(parse_args(args(&["--region-tiles", "2", "--no-obj",
            "--port", "2:wear"])).is_err());
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
            demo: true, no_obj: false, tiles: Some(2), region_tiles: None,
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
            demo: true, no_obj: true, tiles: None, region_tiles: None,
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
            demo: false, no_obj: false, tiles: None, region_tiles: None,
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
            all_outputs: None, demo: false, no_obj: false, tiles: None, region_tiles: None,
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
            tiles: None, region_tiles: None,
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
            selected: None, all_outputs: None, demo: true, no_obj: true, tiles: Some(2), region_tiles: None,
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
            tiles: None, region_tiles: None,
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
