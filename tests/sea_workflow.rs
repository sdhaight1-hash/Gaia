//! Coastline to game-engine outputs regression using real CLI build.
use gaia::{pipeline::{PortRef,PortType}, terrain::{NodeKind,Project}};
use std::process::Command;

#[test]
fn island_to_sea_to_normals_and_multioutput_tiled_export() {
    let mut project=Project::default();
    project.resolution=32;
    let island=project.graph.add(NodeKind::Island {
        seed:41,radius:0.65,height:1.0,roughness:0.7,
    });
    let sea=project.graph.add_from(NodeKind::Sea {
        level:0.08,shore_width:4,erosion:0.35,
    },&PortRef::height(island));
    let normals=project.graph.add_from(NodeKind::Normals {
        strength:0.8,flip_green:false,
    },&PortRef::new(sea,"surface"));
    let water=project.graph.evaluate_port(32,&PortRef::new(sea,"water")).unwrap();
    let surface=project.graph.evaluate_port(32,&PortRef::new(sea,"surface")).unwrap();
    let bottom=project.graph.evaluate_port(32,&PortRef::height(sea)).unwrap();
    let shore=project.graph.evaluate_port(32,&PortRef::new(sea,"shore")).unwrap();
    let wet=project.graph.evaluate_port(32,&PortRef::new(sea,"wet")).unwrap();
    let normal=project.graph.evaluate_port(32,&PortRef::new(normals,"normal")).unwrap();
    assert_eq!((water.kind,surface.kind,shore.kind,normal.kind),
        (PortType::Water,PortType::Height,PortType::Mask,PortType::Color));
    assert!(water.data.values.iter().any(|&v|v>0.0));
    assert!(shore.data.values.iter().any(|&v|v>0.0));
    assert!(wet.data.values.iter().any(|&v|v==0.0));
    assert!(wet.data.values.iter().any(|&v|v==1.0));
    for (s,h) in surface.data.values.iter().zip(&bottom.data.values) {
        assert!(*s>=*h);
    }
    let root=std::env::temp_dir().join(format!("gaia-sea-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let scene=root.join("coast.gaia.json");
    project.save(&scene).unwrap();
    let loaded=Project::load(&scene).unwrap();
    assert_eq!(shore.data,loaded.graph.evaluate_port(32,
        &PortRef::new(sea,"shore")).unwrap().data);
    let stem=root.join("out").join("coast");
    let result=Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&scene).arg("--output").arg(&stem)
        .args(["--all-outputs",&sea.to_string(),"--no-obj","--tiles","2"])
        .output().unwrap();
    assert!(result.status.success(),"CLI failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr));
    let report:serde_json::Value=serde_json::from_slice(
        &std::fs::read(root.join("out/coast.batch.report.json")).unwrap()
    ).unwrap();
    assert_eq!(report["output_count"],6);
    assert_eq!(report["computed_nodes"],2);
    assert!(report["reused_nodes"].as_u64().unwrap()>=10);
    for (port,kind) in [("height","Height"),("surface","Height"),
        ("water","Water"),("wet","Mask"),("shore","Mask"),("beach","Mask")] {
        let stem=format!("coast.node{sea}.{port}");
        let img=root.join("out").join(format!("{stem}.png"));
        let decoder=png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(img).unwrap())).read_info().unwrap();
        assert_eq!(decoder.info().color_type,png::ColorType::Grayscale);
        assert_eq!(decoder.info().bit_depth,png::BitDepth::Sixteen);
        assert!(root.join("out").join(format!("{stem}.tiles/x01_y01.png")).is_file());
        assert!(report["outputs"].as_array().unwrap().iter()
            .any(|v|v["port"]==port&&v["type"]==kind));
    }
    std::fs::remove_dir_all(root).unwrap();
}
