//! Headless, bounded-memory hydraulic builds from saved Gaia node graphs.
//!
//! Local upstream Height graphs are evaluated one world-row at a time, then
//! passed to the disk-backed two-dimensional erosion scheduler. Six output
//! channels are exported as overlapping PNG16 tiles; the whole terrain never
//! becomes a resident HeightField. This is opt-in and preserves the existing
//! global Hydraulic node evaluator and regional preflight behavior.
use crate::hydraulic_exchange::WorldEdge;
use crate::hydraulic_stream::DiskTransport;
use crate::hydraulic_tiles::MAX_HYDRAULIC_TILE_EDGE;
use crate::hydrology::{HydraulicParameters, MassLedger};
use crate::pipeline::PortType;
use crate::region::{
    Region, RegionField, RegionLayout, MAX_REGION_RESOLUTION,
    evaluate_region, validate_region_graph,
};
use crate::terrain::{NodeKind, Project};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

const CHANNELS: [&str; 6] = [
    "height", "water", "sediment", "wear", "deposits", "flow",
];

fn source_and_parameters(
    project: &Project, hydraulic_node: u64,
) -> Result<(crate::pipeline::PortRef, HydraulicParameters), String> {
    project.graph.validate_structure()?;
    let node = project.graph.get(hydraulic_node)
        .ok_or_else(|| format!("Hydraulic node #{hydraulic_node} does not exist"))?;
    let params = match node.kind {
        NodeKind::Hydraulic { params } => params,
        _ => return Err(format!("Node #{hydraulic_node} is not Hydraulic")),
    };
    let source = node.inputs.first()
        .ok_or("Hydraulic node has no terrain input")?.clone();
    let input_node = project.graph.get(source.node)
        .ok_or("Hydraulic input references a missing node")?;
    if input_node.kind.output_ports().iter()
        .find(|(port, _)| *port == source.port)
        .is_none_or(|(_, kind)| *kind != PortType::Height)
    {
        return Err("Hydraulic input must be a typed Height output".into());
    }
    validate_region_graph(&project.graph, &source)?;
    if params.iterations == 0 || params.iterations > 200 {
        return Err("Regional hydraulic build requires 1..=200 iterations".into());
    }
    // The tile engine validates every physical coefficient per tick; preflight
    // them here as well before any input is loaded or output is staged.
    if !params.rain.is_finite() || !(0.0..=0.05).contains(&params.rain) ||
        !params.evaporation.is_finite() || !(0.0..=1.0).contains(&params.evaporation) ||
        !params.capacity.is_finite() || !(0.0..=20.0).contains(&params.capacity) ||
        !params.erosion_rate.is_finite() || !(0.0..=1.0).contains(&params.erosion_rate) ||
        !params.deposition_rate.is_finite() || !(0.0..=1.0).contains(&params.deposition_rate)
    {
        return Err("Regional Hydraulic parameters must be finite and within solver bounds".into());
    }
    Ok((source, params))
}

fn disjoint_tile_cores(size: usize, count: usize) -> Vec<Region> {
    let mut cores = Vec::with_capacity(count * count);
    for ty in 0..count {
        for tx in 0..count {
            let x = tx * size / count;
            let y = ty * size / count;
            cores.push(Region {
                x, y,
                width: (tx + 1) * size / count - x,
                height: (ty + 1) * size / count - y,
            });
        }
    }
    cores
}

fn export_tiled_fields(
    state: &mut DiskTransport, stage: &Path, layout: RegionLayout,
) -> Result<Vec<serde_json::Value>, String> {
    for channel in CHANNELS {
        fs::create_dir(stage.join(channel)).map_err(|err| err.to_string())?;
    }
    let count = layout.tiles_per_axis;
    let mut manifest = Vec::with_capacity(count * count);
    for ty in 0..count {
        for tx in 0..count {
            let rect = layout.tile(tx, ty)?;
            let mut samples: [Vec<f32>; 6] = std::array::from_fn(|_| {
                Vec::with_capacity(rect.width * rect.height)
            });
            for y in rect.y..rect.y + rect.height {
                let (height, wear, deposits) = state.solid_row(y)?;
                let (water, sediment, flow) = state.transport_row(y)?;
                let flow = flow.ok_or("Hydraulic flow diagnostic missing after build")?;
                let fields = [height, water, sediment, wear, deposits, flow];
                let range = rect.x..rect.x + rect.width;
                for (output, row) in samples.iter_mut().zip(fields.iter()) {
                    output.extend_from_slice(&row[range.clone()]);
                }
            }
            let filename = format!("x{tx:02}_y{ty:02}.png");
            for (channel, values) in CHANNELS.iter().zip(samples) {
                RegionField {
                    region: rect,
                    kind: if *channel == "height" { PortType::Height } else { PortType::Mask },
                    values,
                }.export_png16(&stage.join(channel).join(&filename))?;
            }
            manifest.push(json!({
                "tile_x": tx, "tile_y": ty, "file": filename,
                "x": rect.x, "y": rect.y,
                "width": rect.width, "height": rect.height
            }));
        }
    }
    Ok(manifest)
}

/// Build an existing graph's Hydraulic node using bounded-memory 2D tiles.
///
/// Only *regionalizable* upstream Height graphs can be used. The destination
/// is a new `.hydraulic.tiles` folder containing six named PNG16 tile sets,
/// a build report and, optionally, a restartable hydraulic checkpoint.
/// All results are first written into a sibling temporary directory and only
/// published when the entire build and numerical audit succeed.
pub fn build_hydraulic_regions(
    project: &Project, hydraulic_node: u64, tiles: usize,
    output_stem: &Path, edge: WorldEdge, checkpoint: bool,
) -> Result<PathBuf, String> {
    let started = Instant::now();
    let size = project.resolution;
    if !(32..=MAX_REGION_RESOLUTION).contains(&size) {
        return Err("Regional hydraulic resolution must be within 32..=8192".into());
    }
    let layout = RegionLayout::new(size, tiles)?;
    let minimum = size.div_ceil(MAX_HYDRAULIC_TILE_EDGE);
    if tiles < minimum {
        return Err(format!(
            "Hydraulic build at {size}² needs at least {minimum} tiles per axis              (at most {MAX_HYDRAULIC_TILE_EDGE} owned pixels per tile)"
        ));
    }
    let (source, params) = source_and_parameters(project, hydraulic_node)?;

    let destination = output_stem.with_extension("hydraulic.tiles");
    if destination.exists() {
        return Err(format!("Hydraulic output already exists: {}", destination.display()));
    }
    let parent = destination.parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|err| err.to_string())?;

    // The row callback evaluates a real procedural graph on absolute world
    // samples. Local recursive halos are handled by the existing evaluator;
    // unsupported global ancestors were rejected during preflight.
    let mut state = DiskTransport::from_rows(size, |y, height, water, sediment| {
        let (field, _) = evaluate_region(
            &project.graph, size, &source,
            Region { x: 0, y, width: size, height: 1 },
        )?;
        if field.kind != PortType::Height || field.values.len() != size {
            return Err("Hydraulic source returned a non-Height row".into());
        }
        height.copy_from_slice(&field.values);
        water.fill(0.0);
        sediment.fill(0.0);
        Ok(())
    })?;

    let start_solid = state.solid_mass()?;
    let start_water = state.mass()?.0;
    let cores = disjoint_tile_cores(size, tiles);
    let mut totals = MassLedger::default();
    let mut tick_params = params;
    tick_params.iterations = 1;
    for _ in 0..params.iterations {
        let step = state.step_eroding_tiles(&cores, edge, tick_params, || false)?;
        totals.rainfall += step.rainfall;
        totals.evaporated += step.evaporated;
        totals.water_outflow += step.water_outflow;
        totals.sediment_outflow += step.sediment_outflow;
    }

    let (final_water, _) = state.mass()?;
    let final_solid = state.solid_mass()?;
    let water_residual = start_water + totals.rainfall - totals.evaporated -
        totals.water_outflow - final_water;
    let solid_residual = start_solid - totals.sediment_outflow - final_solid;
    let water_budget = (start_water + totals.rainfall).max(1.0);
    let solid_budget = start_solid.max(1.0);
    let water_tolerance = (water_budget * 0.0001).max(0.01);
    let solid_tolerance = (solid_budget * 0.0001).max(0.01);
    if !water_residual.is_finite() || !solid_residual.is_finite() ||
        water_residual.abs() > water_tolerance || solid_residual.abs() > solid_tolerance
    {
        return Err(format!(
            "Regional hydraulic mass audit failed: water residual={water_residual},              solid residual={solid_residual}, tolerances={water_tolerance}/{solid_tolerance}"
        ));
    }

    let stage = tempfile::Builder::new().prefix(".gaia-hydraulic-")
        .tempdir_in(parent).map_err(|err| err.to_string())?;
    let exported = export_tiled_fields(&mut state, stage.path(), layout)?;
    let mut report = json!({
        "application": "gaia",
        "version": env!("CARGO_PKG_VERSION"),
        "mode": "hydraulic_region_build",
        "execution": "bounded tile-local disk-backed hydraulic erosion",
        "resolution": size,
        "hydraulic_node": hydraulic_node,
        "upstream_height_node": source.node,
        "upstream_height_port": source.port,
        "iterations": params.iterations,
        "tiles_per_axis": tiles,
        "tile_count": exported.len(),
        "max_owned_tile_edge": MAX_HYDRAULIC_TILE_EDGE,
        "output_format": "PNG16 normalized 0..1 (clipped if raw values exceed range)",
        "channels": CHANNELS,
        "overlap_pixels": 1,
        "boundary": if edge == WorldEdge::Open { "open" } else { "closed" },
        "parameters": {
            "rain": params.rain,
            "evaporation": params.evaporation,
            "capacity": params.capacity,
            "erosion_rate": params.erosion_rate,
            "deposition_rate": params.deposition_rate,
        },
        "water_ledger": {
            "initial": start_water,
            "rainfall": totals.rainfall,
            "evaporated": totals.evaporated,
            "external_outflow": totals.water_outflow,
            "final": final_water,
            "residual": water_residual,
            "tolerance": water_tolerance,
        },
        "solid_ledger": {
            "initial_rock_and_suspended": start_solid,
            "external_sediment_outflow": totals.sediment_outflow,
            "final_rock_and_suspended": final_solid,
            "residual": solid_residual,
            "tolerance": solid_tolerance,
        },
        "tiles": exported,
        "elapsed_ms": started.elapsed().as_millis(),
        "checkpoint": if checkpoint { Some("checkpoint") } else { None },
    });
    if checkpoint {
        state.save_checkpoint(&stage.path().join("checkpoint"), || false)?;
    }
    let report_file = stage.path().join("report.json");
    fs::write(&report_file, serde_json::to_vec_pretty(&report)
        .map_err(|err| err.to_string())?)
        .map_err(|err| err.to_string())?;
    // A second check keeps this invariant obvious even if further I/O is
    // added between preflight and publication.
    if destination.exists() {
        return Err("Hydraulic destination appeared while building".into());
    }
    let staging_path = stage.keep();
    match fs::rename(&staging_path, &destination) {
        Ok(()) => Ok(destination),
        Err(err) => {
            let _ = fs::remove_dir_all(&staging_path);
            Err(format!("Unable to publish completed hydraulic build: {err}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::PortRef;
    use crate::terrain::{Graph, Node};

    fn project(n: usize, upstream_global: bool) -> Project {
        let mut nodes = vec![Node {
            id: 1,
            kind: NodeKind::Mountain { seed: 42, frequency: 4.0, height: 0.7 },
            inputs: vec![], position: [0.0, 0.0],
        }];
        let source = if upstream_global {
            nodes.push(Node {
                id: 2,
                kind: NodeKind::Erosion { iterations: 2, talus: 0.01, strength: 0.2 },
                inputs: vec![PortRef::height(1)], position: [100.0, 0.0],
            });
            2
        } else { 1 };
        nodes.push(Node {
            id: 3,
            kind: NodeKind::Hydraulic {
                params: HydraulicParameters {
                    iterations: 3,
                    ..Default::default()
                }
            },
            inputs: vec![PortRef::height(source)], position: [200.0, 0.0],
        });
        Project {
            resolution: n,
            graph: Graph { nodes, output: 3 },
        }
    }

    fn load_png(path: &Path) -> (usize, usize, Vec<u8>) {
        let decoder = png::Decoder::new(fs::File::open(path).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let mut bytes = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut bytes).unwrap();
        (info.width as usize, info.height as usize,
            bytes[..info.buffer_size()].to_vec())
    }

    #[test]
    fn builds_all_six_png_tiles_with_matching_shared_seams_and_restartable_state() {
        let project = project(32, false);
        let dir = tempfile::tempdir().unwrap();
        let stem = dir.path().join("terrain");
        let output = build_hydraulic_regions(
            &project, 3, 2, &stem, WorldEdge::Open, true,
        ).unwrap();
        assert!(output.is_dir());
        let report: serde_json::Value = serde_json::from_slice(
            &fs::read(output.join("report.json")).unwrap()).unwrap();
        assert_eq!(report["mode"], "hydraulic_region_build");
        assert_eq!(report["iterations"], 3);
        assert_eq!(report["tile_count"], 4);
        assert_eq!(report["water_ledger"]["initial"], 0.0);
        for channel in CHANNELS {
            let left = output.join(channel).join("x00_y00.png");
            let right = output.join(channel).join("x01_y00.png");
            assert!(left.is_file());
            assert!(right.is_file());
            let (lw, lh, lbytes) = load_png(&left);
            let (rw, rh, rbytes) = load_png(&right);
            assert_eq!(lh, rh);
            assert_eq!(lw, 17);
            assert_eq!(rw, 16);
            for row in 0..lh {
                let a = &lbytes[(row * lw + lw - 1) * 2 .. (row * lw + lw) * 2];
                let b = &rbytes[row * rw * 2 .. row * rw * 2 + 2];
                assert_eq!(a, b, "{channel} has a discontinuous shared border");
            }
        }
        let restored = DiskTransport::open_checkpoint(
            &output.join("checkpoint"), || false,
        ).unwrap();
        assert_eq!(restored.size(), 32);
        assert!(build_hydraulic_regions(
            &project, 3, 2, &stem, WorldEdge::Open, false,
        ).is_err(), "Existing output should never be overwritten");
    }

    #[test]
    fn preflight_rejects_unsupported_global_ancestors_and_oversize_tiles() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("invalid");
        assert!(build_hydraulic_regions(
            &project(32, true), 3, 2, &output, WorldEdge::Closed, false,
        ).unwrap_err().contains("Region mode does not support"));
        assert!(!output.with_extension("hydraulic.tiles").exists());

        let big = project(1024, false);
        let error = build_hydraulic_regions(
            &big, 3, 2, &output, WorldEdge::Closed, false,
        ).unwrap_err();
        assert!(error.contains("at least 4 tiles"), "{error}");
        assert!(!output.with_extension("hydraulic.tiles").exists());
    }

    #[test]
    fn refuses_non_hydraulic_selection_without_creating_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("wrong-node");
        let err = build_hydraulic_regions(
            &project(32, false), 1, 2, &output, WorldEdge::Open, false,
        ).unwrap_err();
        assert!(err.contains("not Hydraulic"));
        assert!(!output.with_extension("hydraulic.tiles").exists());
    }
}
