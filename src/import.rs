//! Import external grayscale PNG heightfields with deterministic bilinear
//! resampling into Gaia's square native field layout.
use crate::terrain::HeightField;
use std::{fs::File, path::Path};

pub fn read_heightmap(path: &Path, target_size: usize, scale: f32) -> Result<HeightField, String> {
    if !(32..=512).contains(&target_size) {
        return Err("Imported terrain resolution must be 32–512".into());
    }
    if !scale.is_finite() || !(0.0..=8.0).contains(&scale) {
        return Err("Import height scale must be finite and between 0 and 8".into());
    }
    let file = File::open(path).map_err(|e| format!("Heightmap {}: {e}", path.display()))?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().map_err(|e| format!("PNG header: {e}"))?;
    let mut bytes = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut bytes).map_err(|e| format!("PNG data: {e}"))?;
    if info.color_type != png::ColorType::Grayscale {
        return Err("Heightmap import currently accepts grayscale PNG only".into());
    }
    if info.bit_depth != png::BitDepth::Eight && info.bit_depth != png::BitDepth::Sixteen {
        return Err("Heightmap import requires 8-bit or 16-bit grayscale PNG".into());
    }
    let width = info.width as usize;
    let height = info.height as usize;
    if width < 2 || height < 2 || width > 8192 || height > 8192 {
        return Err("Heightmap dimensions must be within 2–8192 on each axis".into());
    }
    let bytes_per_pixel = if info.bit_depth == png::BitDepth::Sixteen { 2 } else { 1 };
    let len = width.checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .ok_or("Heightmap dimensions overflow")?;
    if info.buffer_size() != len {
        return Err("Heightmap pixel layout does not match grayscale dimensions".into());
    }
    let values: Vec<f32> = if bytes_per_pixel == 2 {
        bytes[..len].chunks_exact(2).map(|pair| {
            u16::from_be_bytes([pair[0], pair[1]]) as f32 / 65535.0
        }).collect()
    } else {
        bytes[..len].iter().map(|&value| value as f32 / 255.0).collect()
    };
    let source = |x: usize, y: usize| values[y * width + x];
    let mut output = HeightField::flat(target_size, 0.0);
    for y in 0..target_size {
        let v = y as f32 * (height - 1) as f32 / (target_size - 1) as f32;
        let iy = v.floor() as usize;
        let ty = v - iy as f32;
        let ay = (iy + 1).min(height - 1);
        for x in 0..target_size {
            let u = x as f32 * (width - 1) as f32 / (target_size - 1) as f32;
            let ix = u.floor() as usize;
            let tx = u - ix as f32;
            let ax = (ix + 1).min(width - 1);
            let a = source(ix, iy) * (1.0 - tx) + source(ax, iy) * tx;
            let b = source(ix, ay) * (1.0 - tx) + source(ax, ay) * tx;
            output.values[y * target_size + x] = (a * (1.0 - ty) + b * ty) * scale;
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::export_png16;

    #[test]
    fn roundtrip_png16_is_within_quantization_error() {
        let source = HeightField {
            size: 32,
            values: (0..1024).map(|i| i as f32 / 1023.0).collect(),
        };
        let path = std::env::temp_dir().join(format!(
            "gaia-import-test-{}.png", std::process::id()
        ));
        export_png16(&source, &path).unwrap();
        let imported = read_heightmap(&path, 32, 1.0).unwrap();
        for (a, b) in source.values.iter().zip(&imported.values) {
            assert!((a - b).abs() <= 1.0 / 65535.0 + 1e-6);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn resamples_and_scales_png16() {
        let source = HeightField {
            size: 32,
            values: (0..32).flat_map(|_| (0..32).map(|x| x as f32 / 31.0)).collect(),
        };
        let path = std::env::temp_dir().join(format!(
            "gaia-import-resize-test-{}.png", std::process::id()
        ));
        export_png16(&source, &path).unwrap();
        let imported = read_heightmap(&path, 64, 0.5).unwrap();
        assert_eq!(imported.size, 64);
        assert!(imported.get(0, 0) <= 1e-5);
        assert!((imported.get(63, 0) - 0.5).abs() < 1e-4);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn handles_missing_input() {
        let missing = Path::new("gaia-nonexistent-heightmap-000000.png");
        assert!(read_heightmap(missing, 64, 1.0).is_err());
    }
}
