//! Snow → normals and multi-output export regression on a real graph.
use gaia::{
    pipeline::{PortRef, PortType},
    terrain::{NodeKind, Project},
};
use std::process::Command;

#[test]
fn snow_node_exposes_height_depth_and_coverage_through_batch_export() {
    let mut project = Project::default();
    project.resolution = 32;
    let snow = project.graph.add_from(NodeKind::Snow {
        seed: 42, snowline: 0.20, intensity: 0.16, melt: 0.1,
        adherence: 0.45, settle: 4,
    }, &PortRef::height(1));
    let normals = project.graph.add_from(NodeKind::Normals {
        strength: 1.0, flip_green: false
    }, &PortRef::height(snow));
    let height = project.graph.evaluate_port(32, &PortRef::height(snow)).unwrap();
    let depth = project.graph.evaluate_port(32,
        &PortRef::new(snow, "snow_depth")).unwrap();
    let coverage = project.graph.evaluate_port(32,
        &PortRef::new(snow, "coverage")).unwrap();
    let normal = project.graph.evaluate_port(32,
        &PortRef::new(normals, "normal")).unwrap();
    assert_eq!((height.kind, depth.kind, coverage.kind, normal.kind),
        (PortType::Height, PortType::Height, PortType::Mask, PortType::Color));
    assert!(depth.data.values.iter().any(|&v| v > 0.0));
    assert!(coverage.data.values.iter().all(|v| (0.0..=1.0).contains(v)));
    assert!(height.data.values.iter().all(|v| v.is_finite()));
    let root = std::env::temp_dir().join(format!(
        "gaia-snow-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let scene = root.join("snow.gaia.json");
    project.save(&scene).unwrap();
    assert_eq!(Project::load(&scene).unwrap().graph.evaluate_port(32,
        &PortRef::new(snow,"snow_depth")).unwrap().data, depth.data);
    let stem = root.join("out").join("snow");
    let result = Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&scene)
        .arg("--output").arg(&stem)
        .args(["--all-outputs", &snow.to_string(), "--tiles", "2", "--no-obj"])
        .output().unwrap();
    assert!(result.status.success(), "build: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr));
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("out/snow.batch.report.json")).unwrap()
    ).unwrap();
    assert_eq!(report["output_count"], 3);
    assert_eq!(report["computed_nodes"], 2);
    for (port, kind) in [("height","Height"),("snow_depth","Height"),("coverage","Mask")] {
        let name = format!("snow.node{snow}.{port}");
        let img = root.join("out").join(format!("{name}.png"));
        assert!(img.is_file());
        let reader = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(img).unwrap()
        )).read_info().unwrap();
        assert_eq!(reader.info().color_type, png::ColorType::Grayscale);
        assert_eq!(reader.info().bit_depth, png::BitDepth::Sixteen);
        assert!(root.join("out").join(format!("{name}.tiles/x01_y01.png")).is_file());
        assert!(report["outputs"].as_array().unwrap().iter()
            .any(|entry| entry["port"] == port && entry["type"] == kind));
    }
    std::fs::remove_dir_all(root).unwrap();
}
