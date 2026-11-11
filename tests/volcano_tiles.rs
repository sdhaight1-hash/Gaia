//! Full-workflow regression for freshly implemented terrain operators,
//! colorization and seam-preserving game-engine tile exports.
use gaia::hydrology::HydraulicParameters;
use gaia::pipeline::{PortRef, PortType};
use gaia::terrain::{NodeKind, Project};
use std::{path::Path, process::Command};

fn png_rgb8(path: &Path) -> (usize, Vec<u8>) {
    let input = std::fs::File::open(path).unwrap();
    let mut reader = png::Decoder::new(std::io::BufReader::new(input))
        .read_info().unwrap();
    let mut bytes = vec![0_u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut bytes).unwrap();
    assert_eq!(info.width, info.height);
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    bytes.truncate(info.buffer_size());
    (info.width as usize, bytes)
}

#[test]
fn volcano_adjust_stratify_hydraulic_color_and_rgb_tiles_integrate() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir()
        .join(format!("gaia-volcano-tiles-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let mut project = Project::default();
    project.resolution = 32;
    let volcano = project.graph.add(NodeKind::Volcano {
        seed: 92, radius: 0.35, height: 0.9, mouth: 0.28,
        bulk: 0.5, roughness: 0.4, center_x: 0.5, center_y: 0.5,
    });
    let adjust = project.graph.add_from(NodeKind::Adjust {
        multiply: 1.0, shaper: 0.4, clamp_min: 0.0, clamp_max: 4.0,
        autolevel: false, invert: false,
    }, &PortRef::height(volcano));
    let strata = project.graph.add_from(NodeKind::Stratify {
        seed: 17, spacing: 0.07, intensity: 0.6,
        warp: 0.4, tilt: 0.2, direction: 25.0,
    }, &PortRef::height(adjust));
    let hydro = project.graph.add_from(NodeKind::Hydraulic {
        params: HydraulicParameters { iterations: 4, ..Default::default() },
    }, &PortRef::height(strata));
    let color = project.graph.add_from(NodeKind::Colorize {
        low: [0.16, 0.34, 0.21], high: [0.88, 0.76, 0.68],
        midpoint: 0.52, contrast: 1.3,
    }, &PortRef::height(hydro));

    let expected = project.graph.evaluate_port(32, &PortRef::new(color, "color")).unwrap();
    assert_eq!(expected.kind, PortType::Color);
    assert!(expected.color.as_ref().unwrap().pixels
        .iter().all(|px| px.iter().all(|v| v.is_finite())));

    let scene = root.join("volcano.gaia.json");
    project.save(&scene).unwrap();
    let stem = root.join("export").join("colored");
    let command = Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .args(["--project"]).arg(&scene)
        .args(["--output"]).arg(&stem)
        .args(["--port", &format!("{color}:color"), "--tiles", "2", "--no-obj"])
        .output().unwrap();
    assert!(command.status.success(), "CLI: {}\n{}",
        String::from_utf8_lossy(&command.stdout),
        String::from_utf8_lossy(&command.stderr));

    let directory = stem.with_extension("tiles");
    let (n, left) = png_rgb8(&directory.join("x00_y00.png"));
    let (other_n, right) = png_rgb8(&directory.join("x01_y00.png"));
    let (down_n, bottom) = png_rgb8(&directory.join("x00_y01.png"));
    assert_eq!((n, other_n, down_n), (17,17,17));
    let stride = n * 3;
    for i in 0..n {
        assert_eq!(
            &left[i*stride+(n-1)*3..i*stride+n*3],
            &right[i*stride..i*stride+3]
        );
        assert_eq!(
            &left[(n-1)*stride+i*3..(n-1)*stride+i*3+3],
            &bottom[i*3..i*3+3]
        );
    }
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(stem.with_extension("tiles.json")).unwrap()).unwrap();
    assert_eq!(manifest["tile_count"], 4);
    assert_eq!(manifest["overlap_pixels"], 1);
    assert_eq!(manifest["format"], "RGB8");
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(stem.with_extension("report.json")).unwrap()).unwrap();
    assert_eq!(report["type"], "Color");
    assert_eq!(report["png_format"], "RGB8");
    assert!(!stem.with_extension("obj").exists());
    std::fs::remove_dir_all(root).unwrap();
}
