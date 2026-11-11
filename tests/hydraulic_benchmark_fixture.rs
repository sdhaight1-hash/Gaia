use gaia::hydraulic_build::build_hydraulic_regions;
use gaia::hydraulic_exchange::WorldEdge;
use gaia::terrain::{NodeKind, Project};
use std::path::Path;

#[test]
fn benchmark_fixture_is_a_real_saved_hydraulic_graph_and_builds_seamless_outputs() {
    let project_file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hydraulic-benchmark.gaia.json");
    let mut project = Project::load(&project_file).unwrap();
    assert!(matches!(
        &project.graph.get(2).unwrap().kind, NodeKind::Hydraulic { .. }
    ));
    // Keep the cross-platform unit fixture small; Windows release CI measures
    // process RSS and SHA-256 outputs at a more representative resolution.
    project.resolution = 32;
    let dir = tempfile::tempdir().unwrap();
    let stem = dir.path().join("fixture");
    let output = build_hydraulic_regions(
        &project, 2, 2, &stem, WorldEdge::Open, false,
    ).unwrap();
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(output.join("report.json")).unwrap(),
    ).unwrap();
    assert_eq!(report["tile_count"], 4);
    assert_eq!(report["iterations"], 2);
    assert_eq!(report["channels"].as_array().unwrap().len(), 6);
    for name in ["height", "water", "sediment", "wear", "deposits", "flow"] {
        assert!(output.join(name).join("x00_y00.png").exists());
        assert!(output.join(name).join("x01_y01.png").exists());
    }
    assert!(report["water_ledger"]["residual"].as_f64().unwrap().abs() <
        report["water_ledger"]["tolerance"].as_f64().unwrap());
    assert!(report["solid_ledger"]["residual"].as_f64().unwrap().abs() <
        report["solid_ledger"]["tolerance"].as_f64().unwrap());
}
