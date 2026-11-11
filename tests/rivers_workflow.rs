//! Engine-level smoke test for deterministic connected-river workflow.
use gaia::{
    pipeline::{PortRef, PortType},
    terrain::{NodeKind, Project},
};
use std::process::Command;

#[test]
fn island_to_rivers_to_normals_exports_png_and_tiled_channels() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!(
        "gaia-river-flow-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let mut scene = Project::default();
    scene.resolution = 32;
    let island = scene.graph.add(NodeKind::Island {
        seed: 41, radius: 0.65, height: 0.85, roughness: 0.7,
    });
    let rivers = scene.graph.add_from(NodeKind::Rivers {
        catchment_fraction: 0.003, width: 2.8, depth: 0.05, grade: 0.0001,
    }, &PortRef::height(island));
    let normals = scene.graph.add_from(NodeKind::Normals {
        strength: 1.0, flip_green: false,
    }, &PortRef::height(rivers));

    let height = scene.graph.evaluate_port(32, &PortRef::height(rivers)).unwrap();
    let corridor = scene.graph.evaluate_port(32,
        &PortRef::new(rivers, "channels")).unwrap();
    let wear = scene.graph.evaluate_port(32,
        &PortRef::new(rivers, "wear")).unwrap();
    let normal = scene.graph.evaluate_port(32,
        &PortRef::new(normals, "normal")).unwrap();
    assert_eq!((height.kind, corridor.kind, wear.kind, normal.kind),
        (PortType::Height, PortType::Mask, PortType::Mask, PortType::Color));
    assert!(corridor.data.values.iter().any(|v| *v > 0.0));
    assert!(wear.data.values.iter().any(|v| *v > 0.0));
    assert!(height.data.values.iter().all(|v| v.is_finite()));
    assert!(normal.color.as_ref().unwrap().pixels
        .iter().all(|rgba| rgba.iter().all(|v| v.is_finite())));
    assert_ne!(height.data, scene.graph.evaluate_port(32,
        &PortRef::height(island)).unwrap().data);

    let project_file = root.join("rivers.gaia.json");
    scene.save(&project_file).unwrap();
    let reloaded = Project::load(&project_file).unwrap();
    assert_eq!(corridor.data, reloaded.graph.evaluate_port(32,
        &PortRef::new(rivers, "channels")).unwrap().data);

    let tile_stem = root.join("out").join("river-channels");
    let exported = Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&project_file)
        .arg("--output").arg(&tile_stem)
        .args(["--port", &format!("{rivers}:channels"),
            "--tiles", "2", "--no-obj"])
        .output().unwrap();
    assert!(exported.status.success(), "CLI failed: {}\n{}",
        String::from_utf8_lossy(&exported.stdout),
        String::from_utf8_lossy(&exported.stderr));
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(tile_stem.with_extension("report.json")).unwrap()
    ).unwrap();
    assert_eq!(report["type"], "Mask");
    assert_eq!(report["png_format"], "Grayscale16");
    assert_eq!(report["port"], "channels");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(tile_stem.with_extension("tiles.json")).unwrap()
    ).unwrap();
    assert_eq!(manifest["tile_count"], 4);
    assert_eq!(manifest["overlap_pixels"], 1);
    assert!(tile_stem.with_extension("tiles")
        .join("x01_y01.png").exists());

    let normal_stem = root.join("out").join("river-normals");
    let exported = Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&project_file)
        .arg("--output").arg(&normal_stem)
        .args(["--port", &format!("{normals}:normal"), "--no-obj"])
        .output().unwrap();
    assert!(exported.status.success(), "Normals build: {}\n{}",
        String::from_utf8_lossy(&exported.stdout),
        String::from_utf8_lossy(&exported.stderr));
    let image = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(normal_stem.with_extension("png")).unwrap()
    )).read_info().unwrap();
    assert_eq!(image.info().color_type, png::ColorType::Rgb);
    assert_eq!(image.info().bit_depth, png::BitDepth::Eight);
    std::fs::remove_dir_all(root).unwrap();
}
