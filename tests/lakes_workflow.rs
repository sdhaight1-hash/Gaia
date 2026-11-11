//! End-to-end static closed-basin lake field generation and multi-output build.
use gaia::{
    pipeline::{PortRef, PortType},
    terrain::{export_png16, HeightField, NodeKind, Project},
};
use std::process::Command;

#[test]
fn imported_pits_build_flat_lake_surfaces_and_typed_water_maps() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir()
        .join(format!("gaia-lake-batch-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let n = 32;
    let mut landscape = HeightField::flat(n, 0.8);
    for i in 0..n {
        landscape.values[i] = 0.1;
        landscape.values[(n-1)*n+i] = 0.1;
        landscape.values[i*n] = 0.1;
        landscape.values[i*n+n-1] = 0.1;
    }
    // Each depression occupies 2x2 cells so min_area=2 keeps them while
    // isolated noise pits can be filtered.
    for (cx, cy, bed) in [(8, 9, 0.15), (22, 23, 0.23)] {
        for dy in 0..2 {
            for dx in 0..2 {
                landscape.values[(cy+dy)*n+cx+dx] = bed;
            }
        }
    }
    let image_path = root.join("land.png");
    export_png16(&landscape, &image_path).unwrap();
    let mut project = Project::default();
    project.resolution = n;
    let import = project.graph.add(NodeKind::ImportHeight {
        path: image_path.to_string_lossy().to_string(), scale: 1.0,
    });
    let lake = project.graph.add_from(NodeKind::Lake {
        fill_fraction: 0.8, minimum_area: 2, minimum_depth: 0.05
    }, &PortRef::height(import));
    let water = project.graph.evaluate_port(n, &PortRef::new(lake, "water")).unwrap();
    let surface = project.graph.evaluate_port(n, &PortRef::new(lake, "surface")).unwrap();
    let terrain = project.graph.evaluate_port(n, &PortRef::height(lake)).unwrap();
    let mask = project.graph.evaluate_port(n, &PortRef::new(lake, "coverage")).unwrap();
    assert_eq!((water.kind, surface.kind, terrain.kind, mask.kind),
        (PortType::Water, PortType::Height, PortType::Height, PortType::Mask));
    assert_eq!(mask.data.values.iter().filter(|&&v| v > 0.0).count(), 8);
    assert!(surface.data.get(8, 9) > terrain.data.get(8, 9));
    assert!(water.data.values.iter().all(|v| v.is_finite() && *v >= 0.0));

    let scene = root.join("lake.gaia.json");
    project.save(&scene).unwrap();
    let again = Project::load(&scene).unwrap();
    assert_eq!(water.data,
        again.graph.evaluate_port(n, &PortRef::new(lake, "water")).unwrap().data);

    let stem = root.join("out").join("lake");
    let cli = Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&scene).arg("--output").arg(&stem)
        .args(["--all-outputs", &lake.to_string(), "--no-obj", "--tiles", "2"])
        .output().unwrap();
    assert!(cli.status.success(), "Batch failed:\n{}\n{}",
        String::from_utf8_lossy(&cli.stdout),
        String::from_utf8_lossy(&cli.stderr));
    let manifest_path = root.join("out").join("lake.batch.report.json");
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(manifest_path).unwrap()).unwrap();
    assert_eq!(report["output_count"], 4);
    assert_eq!(report["computed_nodes"], 2);
    assert!(report["reused_nodes"].as_u64().unwrap() >= 6);
    for (port, ty) in [("height","Height"), ("surface","Height"),
                       ("water","Water"), ("coverage","Mask")] {
        let stem = root.join("out").join(format!("lake.node{lake}.{port}"));
        let png_file = stem.with_file_name(format!(
            "{}.png", stem.file_name().unwrap().to_string_lossy()));
        assert!(png_file.exists(), "Missing {port} PNG");
        let data = std::fs::File::open(&png_file).unwrap();
        let reader = png::Decoder::new(std::io::BufReader::new(data))
            .read_info().unwrap();
        assert_eq!(reader.info().color_type, png::ColorType::Grayscale);
        assert_eq!(reader.info().bit_depth, png::BitDepth::Sixteen);
        assert!(report["outputs"].as_array().unwrap().iter().any(|v|
            v["port"] == port && v["type"] == ty));
        let tile_folder = stem.with_file_name(format!(
            "{}.tiles", stem.file_name().unwrap().to_string_lossy()));
        assert!(tile_folder.join("x01_y01.png").exists());
    }
    std::fs::remove_dir_all(root).unwrap();
}
