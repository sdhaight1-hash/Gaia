//! Actual CLI workflow for AO and directional light masks.
use gaia::{pipeline::{PortRef,PortType},terrain::{NodeKind,Project}};
use std::process::Command;

#[test]
fn terrain_lighting_masks_drive_color_mixer_and_export() {
    let mut project=Project::default();
    project.resolution=32;
    let ao=project.graph.add_from(NodeKind::AmbientOcclusion{
        radius:10,strength:0.8,
    },&PortRef::height(1));
    let sun=project.graph.add_from(NodeKind::Sunlight{
        azimuth:45.0,elevation:30.0,distance:24,
    },&PortRef::height(1));
    let light=project.graph.evaluate_port(32,&PortRef::new(sun,"light")).unwrap();
    let ambient=project.graph.evaluate_port(32,&PortRef::new(ao,"ao")).unwrap();
    assert_eq!((light.kind,ambient.kind),(PortType::Mask,PortType::Mask));
    for map in [&light.data,&ambient.data] {
        assert!(map.values.iter().all(|v|v.is_finite()&&(0.0..=1.0).contains(v)));
    }
    let grass=project.graph.add_from(NodeKind::Colorize{
        low:[0.08,0.24,0.08],high:[0.4,0.55,0.32],
        midpoint:0.5,contrast:1.0
    },&PortRef::height(1));
    let rock=project.graph.add_from(NodeKind::Colorize{
        low:[0.26,0.28,0.33],high:[0.88,0.87,0.81],
        midpoint:0.5,contrast:1.0
    },&PortRef::height(1));
    let mixer=project.graph.add(NodeKind::ColorMixer{mix:0.9});
    project.graph.connect(mixer,0,PortRef::new(grass,"color")).unwrap();
    project.graph.connect(mixer,1,PortRef::new(rock,"color")).unwrap();
    project.graph.connect(mixer,2,PortRef::new(ao,"ao")).unwrap();
    let composite=project.graph.evaluate_port(32,
        &PortRef::new(mixer,"color")).unwrap();
    assert_eq!(composite.kind,PortType::Color);
    assert!(composite.color.as_ref().unwrap().pixels
        .iter().all(|rgba|rgba.iter().all(|v|v.is_finite())));

    let root=std::env::temp_dir().join(format!("gaia-lighting-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let scene=root.join("lighting.gaia.json");
    project.save(&scene).unwrap();
    assert_eq!(ambient.data,Project::load(&scene).unwrap()
        .graph.evaluate_port(32,&PortRef::new(ao,"ao")).unwrap().data);

    let stem=root.join("out").join("ao");
    let output=Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&scene).arg("--output").arg(&stem)
        .args(["--port",&format!("{ao}:ao"),"--no-obj"])
        .output().unwrap();
    assert!(output.status.success(),"AO builder:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    let mut reader=png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(stem.with_extension("png")).unwrap()
    )).read_info().unwrap();
    assert_eq!(reader.info().color_type,png::ColorType::Grayscale);
    assert_eq!(reader.info().bit_depth,png::BitDepth::Sixteen);
    let mut buffer=vec![0;reader.output_buffer_size()];
    reader.next_frame(&mut buffer).unwrap();

    let stem=root.join("out").join("material");
    let output=Command::new(env!("CARGO_BIN_EXE_gaia-build"))
        .arg("--project").arg(&scene).arg("--output").arg(&stem)
        .args(["--port",&format!("{mixer}:color"),"--no-obj"])
        .output().unwrap();
    assert!(output.status.success(),"Material builder:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    let reader=png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(stem.with_extension("png")).unwrap()
    )).read_info().unwrap();
    assert_eq!(reader.info().color_type,png::ColorType::Rgb);
    assert_eq!(reader.info().bit_depth,png::BitDepth::Eight);
    std::fs::remove_dir_all(root).unwrap();
}
