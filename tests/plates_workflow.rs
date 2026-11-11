//! Full graph/build regression for seeded tectonic plate region maps.
use gaia::{
    pipeline::{PortRef, PortType},
    terrain::{NodeKind, Project},
};
use std::process::Command;

#[test]
fn seeded_plates_erosion_and_all_derived_maps_export() {
    let mut project=Project::default();
    project.resolution=32;
    let plates=project.graph.add(NodeKind::Plates {
        seed:72,count:10,continental:0.55,uplift:0.9,warp:0.6,
    });
    let erosion=project.graph.add_from(NodeKind::Erosion {
        iterations:3,talus:0.008,strength:0.22,
    }, &PortRef::height(plates));
    let drainage=project.graph.add_from(NodeKind::Drainage {
        min_catchment_fraction:0.02
    }, &PortRef::height(erosion));
    let plate_height=project.graph.evaluate_port(32,
        &PortRef::height(plates)).unwrap();
    let downstream=project.graph.evaluate_port(32,
        &PortRef::new(drainage,"channels")).unwrap();
    assert_eq!(plate_height.kind,PortType::Height);
    assert_eq!(downstream.kind,PortType::Mask);
    assert!(downstream.data.values.iter().all(|v|v.is_finite()));
    for port in ["boundaries","convergence","divergence","plate_ids","continental"] {
        let mask=project.graph.evaluate_port(32,
            &PortRef::new(plates,port)).unwrap();
        assert_eq!(mask.kind,PortType::Mask);
        assert!(mask.data.values.iter().all(|v|v.is_finite()&&(0.0..=1.0).contains(v)));
    }

    let root=std::env::temp_dir().join(format!("gaia-plates-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let scene=root.join("plates.gaia.json");
    project.save(&scene).unwrap();
    assert_eq!(plate_height.data,Project::load(&scene).unwrap()
        .graph.evaluate_port(32,&PortRef::height(plates)).unwrap().data);
    let stem=root.join("out").join("plate-map");
    let build=Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&scene).arg("--output").arg(&stem)
        .args(["--all-outputs",&plates.to_string(),"--no-obj","--tiles","2"])
        .output().unwrap();
    assert!(build.status.success(),"build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr));
    let report:serde_json::Value=serde_json::from_slice(
        &std::fs::read(root.join("out/plate-map.batch.report.json")).unwrap()
    ).unwrap();
    assert_eq!(report["output_count"],6);
    assert_eq!(report["computed_nodes"],1);
    assert!(report["reused_nodes"].as_u64().unwrap()>=5);
    for (port,ty) in [("height","Height"),("boundaries","Mask"),
        ("convergence","Mask"),("divergence","Mask"),
        ("plate_ids","Mask"),("continental","Mask")] {
        let name=format!("plate-map.node{plates}.{port}");
        let file=root.join("out").join(format!("{name}.png"));
        assert!(file.is_file());
        let reader=png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(file).unwrap())).read_info().unwrap();
        assert_eq!(reader.info().color_type,png::ColorType::Grayscale);
        assert_eq!(reader.info().bit_depth,png::BitDepth::Sixteen);
        assert!(root.join("out").join(format!("{name}.tiles/x01_y01.png")).is_file());
        assert!(report["outputs"].as_array().unwrap()
            .iter().any(|x|x["port"]==port&&x["type"]==ty));
    }
    std::fs::remove_dir_all(root).unwrap();
}
