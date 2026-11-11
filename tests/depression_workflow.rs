//! Regression for a full pit-conditioning → drainage → export workflow.
//! Requires neither a graphics adapter nor a window.
use gaia::{
    pipeline::{PortRef, PortType},
    terrain::{NodeKind, Project},
};
use std::{process::Command, path::Path};

#[test]
fn island_depression_fill_drainage_and_water_depth_build_roundtrip() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir()
        .join(format!("gaia-depression-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let mut project = Project::default();
    project.resolution = 32;
    let island = project.graph.add(NodeKind::Island {
        seed: 41, radius: 0.65, height: 0.9, roughness: 0.7,
    });
    let fill = project.graph.add_from(
        NodeKind::DepressionFill { grade: 0.0001 },
        &PortRef::height(island));
    let drainage = project.graph.add_from(
        NodeKind::Drainage { min_catchment_fraction: 0.025 },
        &PortRef::height(fill));
    assert_eq!(project.graph.get(drainage).unwrap().inputs[0],
        PortRef::height(fill));
    let height = project.graph.evaluate_port(32, &PortRef::height(fill)).unwrap();
    let depth = project.graph.evaluate_port(32,
        &PortRef::new(fill, "fill_depth")).unwrap();
    let channels = project.graph.evaluate_port(32,
        &PortRef::new(drainage, "channels")).unwrap();
    assert_eq!((height.kind, depth.kind, channels.kind),
        (PortType::Height, PortType::Water, PortType::Mask));
    assert!(depth.data.values.iter().all(|v| v.is_finite() && *v >= 0.0));
    for y in 1..31 {
        for x in 1..31 {
            let h = height.data.get(x, y);
            assert!((-1_isize..=1).any(|dy| (-1_isize..=1).any(|dx| {
                if dx == 0 && dy == 0 { return false; }
                let xx = (x as isize + dx) as usize;
                let yy = (y as isize + dy) as usize;
                height.data.get(xx, yy) < h
            })), "No downhill outlet at ({x},{y})");
        }
    }

    let scene = root.join("drainage.gaia.json");
    project.save(&scene).unwrap();
    let restored = Project::load(&scene).unwrap();
    assert_eq!(
        restored.graph.evaluate_port(32, &PortRef::height(fill)).unwrap().data,
        height.data
    );
    let stem = root.join("outputs").join("filled");
    let result = Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&scene)
        .arg("--output").arg(&stem)
        .arg("--port").arg(format!("{fill}:fill_depth"))
        .arg("--no-obj")
        .output().unwrap();
    assert!(result.status.success(), "Gaia build: {}\\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr));
    let exported = stem.with_extension("png");
    assert!(Path::new(&exported).is_file());
    let reader = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(&exported).unwrap()
    ));
    let png = reader.read_info().unwrap();
    assert_eq!((png.info().width, png.info().height), (32, 32));
    assert_eq!(png.info().color_type, png::ColorType::Grayscale);
    assert_eq!(png.info().bit_depth, png::BitDepth::Sixteen);
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(stem.with_extension("report.json")).unwrap()
    ).unwrap();
    assert_eq!(report["type"], "Water");
    assert_eq!(report["port"], "fill_depth");
    assert_eq!(report["png_format"], "Grayscale16");
    assert!(!stem.with_extension("obj").exists());
    std::fs::remove_dir_all(root).unwrap();
}
