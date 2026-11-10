//! Standalone deterministic terrain build tool.
//! No window or GPU required; intended for production pipelines and CI.
use gaia::pipeline::{PortRef, PortType};
use gaia::terrain::{export_obj, export_png16, Project};
use serde_json::json;
use std::{env, path::{Path, PathBuf}, process, time::Instant};

const HELP: &str = r#"Gaia headless terrain builder
Usage: gaia-build --project FILE --output STEM [--resolution N] [--port NODE:PORT] [--no-obj]
       gaia-build --demo --output STEM [--resolution N] [--no-obj]

Examples:
  gaia-build --project terrain.gaia.json --output builds/mountain
  gaia-build --project terrain.gaia.json --output builds/wear --port 2:wear --no-obj
  gaia-build --demo --output builds/sample --resolution 256

Exports normalized PNG16 and, for height outputs, OBJ. Writes STEM.report.json.
N must be 32 through 512 (current in-memory engine limit).
"#;

#[derive(Debug)]
struct Options {
    project: Option<PathBuf>,
    output: PathBuf,
    resolution: Option<usize>,
    selected: Option<PortRef>,
    demo: bool,
    no_obj: bool,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Options>, String> {
    let mut it = args.into_iter();
    let mut project = None;
    let mut output = None;
    let mut resolution = None;
    let mut selected = None;
    let mut demo = false;
    let mut no_obj = false;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--demo" => demo = true,
            "--no-obj" => no_obj = true,
            "--project" => {
                project = Some(PathBuf::from(it.next().ok_or("--project requires a file")?));
            }
            "--output" => {
                output = Some(PathBuf::from(it.next().ok_or("--output requires a filename stem")?));
            }
            "--resolution" => {
                let value = it.next().ok_or("--resolution requires an integer")?;
                let size: usize = value.parse().map_err(|_| "resolution must be an integer")?;
                if !(32..=512).contains(&size) {
                    return Err("resolution must be between 32 and 512".into());
                }
                resolution = Some(size);
            }
            "--port" => {
                let value = it.next().ok_or("--port requires NODE:PORT")?;
                let (node, port) = value.split_once(':').ok_or("--port must be NODE:PORT")?;
                let node = node.parse::<u64>().map_err(|_| "port node must be numeric")?;
                if port.is_empty() || node == 0 {
                    return Err("port must have a positive node ID and non-empty name".into());
                }
                selected = Some(PortRef::new(node, port));
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    if !demo && project.is_none() {
        return Err("specify --project FILE or --demo".into());
    }
    if demo && project.is_some() {
        return Err("use either --demo or --project, not both".into());
    }
    let output = output.ok_or("--output STEM is required")?;
    if output.as_os_str().is_empty() {
        return Err("--output must be non-empty".into());
    }
    Ok(Some(Options { project, output, resolution, selected, demo, no_obj }))
}

fn build(options: Options) -> Result<(), String> {
    let started = Instant::now();
    let mut project = if options.demo {
        Project::default()
    } else {
        Project::load(options.project.as_ref().expect("parser requires path"))?
    };
    if let Some(size) = options.resolution {
        project.resolution = size;
    }
    let output = options.selected
        .unwrap_or_else(|| PortRef::height(project.graph.output));
    let field = project.graph.evaluate_port(project.resolution, &output)?;
    if let Some(parent) = options.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let png = options.output.with_extension("png");
    export_png16(&field.data, &png)?;
    let mut obj = None;
    if !options.no_obj && field.kind == PortType::Height {
        let file = options.output.with_extension("obj");
        export_obj(&field.data, &file)?;
        obj = Some(file.display().to_string());
    }
    let (min, max, sum) = field.data.values.iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY, 0.0_f64), |(min, max, sum), &v| {
            (min.min(v), max.max(v), sum + f64::from(v))
        });
    let report = json!({
        "application": "gaia",
        "version": env!("CARGO_PKG_VERSION"),
        "resolution": project.resolution,
        "graph_node_count": project.graph.nodes.len(),
        "node": output.node,
        "port": output.port,
        "type": field.kind.label(),
        "min_raw": min,
        "max_raw": max,
        "mean_raw": sum / field.data.values.len() as f64,
        "png16": png.display().to_string(),
        "obj": obj,
        "png_normalization": "clamp raw values to [0,1] and multiply by 65535",
        "duration_ms": started.elapsed().as_millis(),
    });
    let report_path = options.output.with_extension("report.json");
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report)
        .map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("Built {}² {} #{}:{} → {}",
        project.resolution, field.kind.label(), output.node, output.port,
        png.display());
    println!("Report: {}", report_path.display());
    Ok(())
}

fn main() {
    match parse_args(env::args().skip(1)) {
        Ok(None) => print!("{HELP}"),
        Ok(Some(options)) => {
            if let Err(error) = build(options) {
                eprintln!("Gaia build error: {error}");
                process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("{error}\n\n{HELP}");
            process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_valid_arguments() {
        assert!(parse_args(Vec::<String>::new()).is_err());
        assert!(parse_args(vec!["--demo".into()]).is_err());
        assert!(parse_args(vec![
            "--demo".into(), "--output".into(), "x".into(), "--resolution".into(),
            "4096".into()
        ]).is_err());
        assert!(parse_args(vec!["--help".into()]).unwrap().is_none());
    }

    #[test]
    fn accepts_mask_output_without_mesh() {
        let options = parse_args(vec![
            "--demo".into(), "--output".into(), "out".into(),
            "--port".into(), "2:wear".into(), "--no-obj".into(),
        ]).unwrap().unwrap();
        assert_eq!(options.selected.unwrap(), PortRef::new(2, "wear"));
        assert!(options.no_obj);
    }

    #[test]
    fn builds_without_gpu_or_window() {
        let path = std::env::temp_dir().join(format!(
            "gaia-cli-test-{}-{}", process::id(), std::thread::current().name().unwrap_or("test")
        ));
        let stem = path.join("terrain");
        build(Options {
            project: None,
            output: stem.clone(),
            resolution: Some(32),
            selected: None,
            demo: true,
            no_obj: false,
        }).unwrap();
        assert!(stem.with_extension("png").exists());
        assert!(stem.with_extension("obj").exists());
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(stem.with_extension("report.json")).unwrap()
        ).unwrap();
        assert_eq!(report["resolution"], 32);
        std::fs::remove_dir_all(&path).unwrap();
    }
}
