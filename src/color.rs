//! Typed RGBA terrain color fields and deterministic altitude/slope palettes.
use crate::terrain::HeightField;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct ColorField {
    pub size: usize,
    pub pixels: Vec<[f32; 4]>,
}

impl ColorField {
    pub fn flat(size: usize, color: [f32; 4]) -> Self {
        Self { size, pixels: vec![color; size * size] }
    }

    /// Grayscale proxy is only for compatibility with scalar thumbnail
    /// consumers. The actual RGBA values remain in the color field.
    pub fn luminance(&self) -> HeightField {
        HeightField {
            size: self.size,
            values: self.pixels.iter().map(|rgba| {
                rgba[0] * 0.2126 + rgba[1] * 0.7152 + rgba[2] * 0.0722
            }).collect(),
        }
    }
}

pub fn altitude_palette(
    height: &HeightField, low: [f32; 3], high: [f32; 3],
    midpoint: f32, contrast: f32,
) -> ColorField {
    let midpoint = midpoint.clamp(0.01, 0.99);
    let contrast = contrast.clamp(0.1, 6.0);
    let mut output = ColorField::flat(height.size, [0.0, 0.0, 0.0, 1.0]);
    for (&h, pixel) in height.values.iter().zip(&mut output.pixels) {
        let t = if h <= midpoint {
            0.5 * (h.max(0.0) / midpoint)
        } else {
            0.5 + 0.5 * ((h - midpoint) / (1.0 - midpoint))
        };
        let shaped = ((t - 0.5) * contrast + 0.5).clamp(0.0, 1.0);
        let blend = shaped * shaped * (3.0 - 2.0 * shaped);
        for channel in 0..3 {
            let a = low[channel].clamp(0.0, 1.0);
            let b = high[channel].clamp(0.0, 1.0);
            pixel[channel] = a * (1.0 - blend) + b * blend;
        }
    }
    output
}

pub fn color_blend(a: &ColorField, b: &ColorField, mask: &HeightField, mix: f32) -> ColorField {
    assert_eq!(a.size, b.size, "color inputs must have same resolution");
    assert_eq!(a.size, mask.size, "mask must match color resolution");
    let mut output = a.clone();
    for (i, pixel) in output.pixels.iter_mut().enumerate() {
        let t = (mix * mask.values[i]).clamp(0.0, 1.0);
        for (channel, value) in pixel.iter_mut().enumerate() {
            *value = a.pixels[i][channel] * (1.0 - t) + b.pixels[i][channel] * t;
        }
    }
    output
}

/// PNG8 RGB export. Intentionally ignores alpha, since opaque landscape color
/// maps are suitable for DCC and game-engine workflows.
pub fn export_rgb8(field: &ColorField, path: &Path) -> Result<(), String> {
    let output = std::io::BufWriter::new(
        std::fs::File::create(path).map_err(|error| error.to_string())?
    );
    let mut encoder = png::Encoder::new(output, field.size as u32, field.size as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(field.pixels.len() * 3);
    for pixel in &field.pixels {
        for &component in pixel.iter().take(3) {
            bytes.push((component.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    writer.write_image_data(&bytes).map_err(|error| error.to_string())
}

/// RGBA8 export for four-channel material splat weights.
/// Unlike an opaque color map, the alpha channel is essential data.
pub fn export_rgba8(field: &ColorField, path: &Path) -> Result<(), String> {
    if field.size == 0 || field.size.checked_mul(field.size) != Some(field.pixels.len())
        || field.pixels.iter().any(|pixel| pixel.iter().any(|c| !c.is_finite()))
    {
        return Err("RGBA8 export requires a finite square colorfield".into());
    }
    let output = std::io::BufWriter::new(
        std::fs::File::create(path).map_err(|error| error.to_string())?
    );
    let mut encoder = png::Encoder::new(output, field.size as u32, field.size as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(field.pixels.len() * 4);
    for pixel in &field.pixels {
        for &component in pixel {
            bytes.push((component.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    writer.write_image_data(&bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_export_preserves_fourth_weight_channel() {
        let field = ColorField::flat(2, [0.1, 0.2, 0.3, 0.4]);
        let path = std::env::temp_dir().join(format!(
            "gaia-splat-{}-{}.png", std::process::id(),
            std::time::SystemTime::now().duration_since(
                std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        export_rgba8(&field, &path).unwrap();
        let mut reader = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(&path).unwrap())).read_info().unwrap();
        assert_eq!(reader.info().color_type, png::ColorType::Rgba);
        let mut buf = vec![0; reader.output_buffer_size()];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(&buf[..4], &[26, 51, 77, 102]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn palette_interpolates_and_is_bounded() {
        let input = HeightField {
            size: 32,
            values: (0..1024).map(|i| i as f32 / 1023.0).collect(),
        };
        let field = altitude_palette(&input, [0.1, 0.3, 0.1],
            [0.9, 0.9, 0.9], 0.5, 1.0);
        assert_eq!(field.size, 32);
        assert_eq!(field.pixels[0], [0.1, 0.3, 0.1, 1.0]);
        assert_eq!(field.pixels[1023], [0.9, 0.9, 0.9, 1.0]);
        assert!(field.pixels.iter().all(|c| c.iter().all(|v| (0.0..=1.0).contains(v))));
    }

    #[test]
    fn masked_color_mixing_respects_endpoints() {
        let low = ColorField::flat(32, [0.1, 0.2, 0.3, 1.0]);
        let high = ColorField::flat(32, [0.7, 0.6, 0.5, 1.0]);
        let mut mask = HeightField::flat(32, 0.0);
        mask.values[1] = 1.0;
        let output = color_blend(&low, &high, &mask, 1.0);
        assert_eq!(output.pixels[0], low.pixels[0]);
        assert_eq!(output.pixels[1], high.pixels[1]);
    }
}
