//! Material masks are serializable, composable and exportable as four PNG16 maps.
use gaia::{pipeline::{GraphCache, PortRef, PortType}, terrain::{NodeKind, Project}};
use std::process::Command;

#[test]
fn masks_roundtrip_compose_and_export_all_outputs() {
    let mut project = Project::default();
    project.resolution = 32;
    let material = project.graph.add_from(NodeKind::MaterialMasks {
        sea_level: 0.25, beach_width: 0.08,
        rock_slope: 0.32, rock_blend: 0.10,
        snowline: 0.8, snow_blend: 0.12,
    }, &PortRef::height(1));
    let mut cache = GraphCache::default();
    let mut masks = Vec::new();
    for name in ["sand", "soil", "rock", "snow"] {
        let field = cache.evaluate(&project.graph, 32, &PortRef::new(material, name)).unwrap();
        assert_eq!(field.kind, PortType::Mask);
        masks.push(field.data);
    }
    let packed = cache.evaluate(&project.graph, 32,
        &PortRef::new(material, "splat")).unwrap();
    assert_eq!(packed.kind, PortType::Splat);
    let rgba = packed.color.as_ref().unwrap();
    for i in 0..1024 {
        assert_eq!(rgba.pixels[i], [masks[0].values[i], masks[1].values[i],
            masks[2].values[i], masks[3].values[i]]);
    }
    assert_eq!(cache.stats.computed, 0);
    assert_eq!(cache.stats.reused, 2);
    for i in 0..1024 {
        assert!((masks.iter().map(|map| map.values[i]).sum::<f32>() - 1.0).abs() < 1e-5);
    }
    let land = project.graph.add_from(NodeKind::Colorize {
        low: [0.12, 0.24, 0.09], high: [0.42, 0.56, 0.34],
        midpoint: 0.5, contrast: 1.0,
    }, &PortRef::height(1));
    let rock = project.graph.add_from(NodeKind::Colorize {
        low: [0.3, 0.3, 0.32], high: [0.8, 0.8, 0.8],
        midpoint: 0.5, contrast: 1.0,
    }, &PortRef::height(1));
    let mix = project.graph.add(NodeKind::ColorMixer { mix: 1.0 });
    project.graph.connect(mix, 0, PortRef::new(land, "color")).unwrap();
    project.graph.connect(mix, 1, PortRef::new(rock, "color")).unwrap();
    project.graph.connect(mix, 2, PortRef::new(material, "rock")).unwrap();
    assert!(project.graph.evaluate_port(32, &PortRef::new(mix, "color")).unwrap().color.is_some());

    let dir = std::env::temp_dir().join(format!("gaia-material-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    let scene = dir.join("surface.gaia.json");
    project.save(&scene).unwrap();
    let restored = Project::load(&scene).unwrap();
    assert_eq!(restored.graph.get(material).unwrap().kind.label(), "Material Masks");
    let stem = dir.join("out").join("layers");
    let result = Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&scene).arg("--output").arg(&stem)
        .args(["--all-outputs", &material.to_string(), "--no-obj", "--tiles", "2"])
        .output().unwrap();
    assert!(result.status.success(), "CLI:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr));
    for name in ["sand", "soil", "rock", "snow"] {
        let path = stem.with_file_name(format!("layers.node{material}.{name}.png"));
        let img = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(path).unwrap())).read_info().unwrap();
        assert_eq!(img.info().color_type, png::ColorType::Grayscale);
        assert_eq!(img.info().bit_depth, png::BitDepth::Sixteen);
        assert_eq!((img.info().width, img.info().height), (32, 32));
    }
    let splat_path = stem.with_file_name(format!("layers.node{material}.splat.png"));
    let mut splat = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(splat_path).unwrap())).read_info().unwrap();
    assert_eq!(splat.info().color_type, png::ColorType::Rgba);
    assert_eq!(splat.info().bit_depth, png::BitDepth::Eight);
    let mut bytes = vec![0; splat.output_buffer_size()];
    splat.next_frame(&mut bytes).unwrap();
    for channels in bytes.chunks_exact(4) {
        let sum: i32 = channels.iter().map(|v| i32::from(*v)).sum();
        assert_eq!(sum, 255, "packed weights must sum to exactly 255");
    }
    // Every tiled RGBA edge is identical after independent quantization.
    let tiles = stem.with_file_name(format!("layers.node{material}.splat.tiles"));
    let read_tile = |x: usize, y: usize| {
        let file = tiles.join(format!("x{x:02}_y{y:02}.png"));
        let mut png = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(file).unwrap())).read_info().unwrap();
        assert_eq!(png.info().color_type, png::ColorType::Rgba);
        let n = png.info().width as usize;
        assert_eq!(n, png.info().height as usize);
        let mut bytes = vec![0; png.output_buffer_size()];
        png.next_frame(&mut bytes).unwrap();
        (n, bytes)
    };
    let (n, left) = read_tile(0, 0);
    let (_, right) = read_tile(1, 0);
    let (_, bottom) = read_tile(0, 1);
    for row in 0..n {
        let offset = (row * n + n - 1) * 4;
        assert_eq!(&left[offset..offset + 4], &right[row * n * 4..row * n * 4 + 4]);
        let top = ((n - 1) * n + row) * 4;
        let bottom_edge = row * 4;
        assert_eq!(&left[top..top + 4], &bottom[bottom_edge..bottom_edge + 4]);
    }
    std::fs::remove_dir_all(dir).unwrap();
}
