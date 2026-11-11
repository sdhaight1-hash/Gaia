//! Equalize is a real serialized Height→Height node and a PNG16 build target.
use gaia::{
    pipeline::{GraphCache, PortRef, PortType},
    terrain::{NodeKind, Project},
};
use std::process::Command;

#[test]
fn equalize_roundtrips_caches_and_exports_heightmap() {
    let mut project = Project::default();
    project.resolution = 32;
    let equalize = project.graph.add_from(
        NodeKind::Equalize { factor: 0.85 }, &PortRef::height(1));
    assert_eq!(project.graph.get(equalize).unwrap().inputs, vec![PortRef::height(1)]);

    let mut cache = GraphCache::default();
    let original = cache.evaluate(&project.graph, 32, &PortRef::height(1)).unwrap();
    let output = cache.evaluate(&project.graph, 32, &PortRef::height(equalize)).unwrap();
    assert_eq!(output.kind, PortType::Height);
    assert_ne!(original.data, output.data);
    assert_eq!(cache.stats.computed, 1); // cached upstream Mountain
    assert_eq!(cache.stats.reused, 1);
    cache.evaluate(&project.graph, 32, &PortRef::height(equalize)).unwrap();
    assert_eq!(cache.stats.computed, 0);

    let dir = std::env::temp_dir().join(format!(
        "gaia-equalize-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let scene = dir.join("equalize.gaia.json");
    project.save(&scene).unwrap();
    let restored = Project::load(&scene).unwrap();
    let copy = restored.graph.evaluate_port(32, &PortRef::height(equalize)).unwrap();
    assert_eq!(output.data, copy.data);

    let stem = dir.join("builds").join("equalized");
    let result = Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&scene)
        .arg("--output").arg(&stem)
        .args(["--port", &format!("{equalize}:height"), "--no-obj"])
        .output().unwrap();
    assert!(result.status.success(), "gaia-build:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr));

    let reader = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(stem.with_extension("png")).unwrap()
    )).read_info().unwrap();
    assert_eq!(reader.info().color_type, png::ColorType::Grayscale);
    assert_eq!(reader.info().bit_depth, png::BitDepth::Sixteen);
    assert_eq!((reader.info().width, reader.info().height), (32, 32));
    std::fs::remove_dir_all(&dir).unwrap();
}
