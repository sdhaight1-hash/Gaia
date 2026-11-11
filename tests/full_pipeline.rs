//! End-to-end regression for the full CPU/headless terrain workflow.
//! This runs the real CLI and inspects exported file headers, not just nodes.
use gaia::hydrology::HydraulicParameters;
use gaia::pipeline::PortRef;
use gaia::terrain::{NodeKind, Project};
use std::{path::Path, process::Command};

fn run_builder(scene: &Path, stem: &Path, port: &str) {
    let result = Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(scene)
        .arg("--output").arg(stem)
        .arg("--port").arg(port)
        .arg("--no-obj")
        .output().expect("gaia-build executable should launch");
    assert!(result.status.success(),
        "gaia-build failed: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr));
}

#[test]
fn island_hydraulic_drainage_mask_and_rgba_export_roundtrip() {
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "gaia-workflow-{}-{id}", std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let mut scene = Project::default();
    scene.resolution = 32;
    let island = scene.graph.add(NodeKind::Island {
        seed: 123, radius: 0.62, height: 0.85, roughness: 0.5,
    });
    let hydraulic = scene.graph.add(NodeKind::Hydraulic {
        params: HydraulicParameters { iterations: 6, ..Default::default() },
    });
    scene.graph.connect(hydraulic, 0, PortRef::height(island)).unwrap();

    let drainage = scene.graph.add(NodeKind::Drainage {
        min_catchment_fraction: 0.03,
    });
    scene.graph.connect(drainage, 0, PortRef::height(hydraulic)).unwrap();
    let levels = scene.graph.add(NodeKind::MaskLevels {
        black: 0.0, white: 1.0, gamma: 0.8, invert: false,
    });
    scene.graph.connect(levels, 0, PortRef::new(drainage, "accumulation")).unwrap();

    let earthy = scene.graph.add(NodeKind::Colorize {
        low: [0.14, 0.25, 0.08], high: [0.75, 0.73, 0.64],
        midpoint: 0.48, contrast: 1.2,
    });
    let rocky = scene.graph.add(NodeKind::Colorize {
        low: [0.28, 0.27, 0.25], high: [0.90, 0.88, 0.86],
        midpoint: 0.5, contrast: 1.0,
    });
    scene.graph.connect(earthy, 0, PortRef::height(hydraulic)).unwrap();
    scene.graph.connect(rocky, 0, PortRef::height(hydraulic)).unwrap();

    let mixer = scene.graph.add(NodeKind::ColorMixer { mix: 0.75 });
    scene.graph.connect(mixer, 0, PortRef::new(earthy, "color")).unwrap();
    scene.graph.connect(mixer, 1, PortRef::new(rocky, "color")).unwrap();
    scene.graph.connect(mixer, 2, PortRef::new(levels, "mask")).unwrap();

    let project_file = dir.join("complete.gaia.json");
    scene.save(&project_file).unwrap();
    let loaded = Project::load(&project_file).unwrap();
    assert_eq!(loaded.graph.nodes.len(), scene.graph.nodes.len());

    let stem_color = dir.join("builds").join("surface");
    run_builder(&project_file, &stem_color, &format!("{mixer}:color"));
    assert!(!stem_color.with_extension("obj").exists());
    let file = std::fs::File::open(stem_color.with_extension("png")).unwrap();
    let mut reader = png::Decoder::new(std::io::BufReader::new(file)).read_info().unwrap();
    let mut bytes = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut bytes).unwrap();
    assert_eq!((info.width, info.height), (32, 32));
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    assert!(bytes[..info.buffer_size()].iter().any(|&v| v != 0));

    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(stem_color.with_extension("report.json")).unwrap()
    ).unwrap();
    assert_eq!(report["type"], "Color");
    assert_eq!(report["png_format"], "RGB8");
    assert_eq!(report["port"], "color");

    let stem_channels = dir.join("builds").join("channels");
    run_builder(&project_file, &stem_channels, &format!("{drainage}:channels"));
    let file = std::fs::File::open(stem_channels.with_extension("png")).unwrap();
    let info = png::Decoder::new(std::io::BufReader::new(file)).read_info().unwrap();
    assert_eq!(info.info().color_type, png::ColorType::Grayscale);
    assert_eq!(info.info().bit_depth, png::BitDepth::Sixteen);

    std::fs::remove_dir_all(&dir).unwrap();
}
