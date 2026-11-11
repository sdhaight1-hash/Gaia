//! Export-stage tile subdivision with a shared one-pixel border.
//! The entire graph is evaluated first. This is NOT streamed/tiled execution:
//! world-space evaluation stays global, keeping colors, masks and water
//! consistent across tile boundaries at the expense of full-grid memory.
use crate::{color::ColorField, terrain::HeightField};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileLayout {
    pub tiles_per_axis: usize,
    pub pixels_per_tile: usize,
    pub stitched_pixels: usize,
    pub overlap_pixels: usize,
}

pub fn tile_layout(source_size: usize, tiles_per_axis: usize) -> Result<TileLayout, String> {
    if source_size < 3 || !(2..=8).contains(&tiles_per_axis) ||
        tiles_per_axis >= source_size
    {
        return Err("Tiling requires at least a 3x3 source and 2–8 tiles per axis".into());
    }
    // One common global sample lattice: adjacent tiles use identical edge
    // coordinates. Non-divisible sources are bilinearly resampled uniformly,
    // rather than cropping a pixel or normalizing each tile independently.
    let stride = (source_size - 1).div_ceil(tiles_per_axis);
    let pixels_per_tile = stride + 1;
    let stitched_pixels = stride * tiles_per_axis + 1;
    Ok(TileLayout {
        tiles_per_axis, pixels_per_tile, stitched_pixels,
        overlap_pixels: 1,
    })
}

fn sample_bilinear(
    size: usize, x: f32, y: f32, sample: impl Fn(usize, usize) -> f32
) -> f32 {
    let ix = x.floor() as usize;
    let iy = y.floor() as usize;
    let ix1 = (ix + 1).min(size - 1);
    let iy1 = (iy + 1).min(size - 1);
    let tx = x - ix as f32;
    let ty = y - iy as f32;
    let a = sample(ix, iy) * (1.0 - tx) + sample(ix1, iy) * tx;
    let b = sample(ix, iy1) * (1.0 - tx) + sample(ix1, iy1) * tx;
    a * (1.0 - ty) + b * ty
}

fn tile_coordinates(
    source_size: usize, layout: TileLayout, tile_x: usize, tile_y: usize,
    x: usize, y: usize,
) -> (f32, f32) {
    let stride = layout.pixels_per_tile - 1;
    let gx = tile_x * stride + x;
    let gy = tile_y * stride + y;
    let scale = (source_size - 1) as f32 / (layout.stitched_pixels - 1) as f32;
    (gx as f32 * scale, gy as f32 * scale)
}

fn check_tile(layout: TileLayout, x: usize, y: usize) -> Result<(), String> {
    if x >= layout.tiles_per_axis || y >= layout.tiles_per_axis {
        Err(format!("Tile ({x},{y}) is outside the {}×{} grid",
            layout.tiles_per_axis, layout.tiles_per_axis))
    } else {
        Ok(())
    }
}

pub fn tile_height(
    source: &HeightField, layout: TileLayout, x: usize, y: usize
) -> Result<HeightField, String> {
    check_tile(layout, x, y)?;
    if source.size < 3 || source.values.len() != source.size * source.size {
        return Err("Invalid source heightfield for tiling".into());
    }
    let n = layout.pixels_per_tile;
    let mut tile = HeightField::flat(n, 0.0);
    for row in 0..n {
        for col in 0..n {
            let (u, v) = tile_coordinates(source.size, layout, x, y, col, row);
            tile.values[row * n + col] = sample_bilinear(source.size, u, v,
                |ix, iy| source.values[iy * source.size + ix]);
        }
    }
    Ok(tile)
}

pub fn tile_color(
    source: &ColorField, layout: TileLayout, x: usize, y: usize
) -> Result<ColorField, String> {
    check_tile(layout, x, y)?;
    if source.size < 3 || source.pixels.len() != source.size * source.size {
        return Err("Invalid source colorfield for tiling".into());
    }
    let n = layout.pixels_per_tile;
    let mut tile = ColorField::flat(n, [0.0; 4]);
    for row in 0..n {
        for col in 0..n {
            let (u, v) = tile_coordinates(source.size, layout, x, y, col, row);
            for channel in 0..4 {
                tile.pixels[row * n + col][channel] =
                    sample_bilinear(source.size, u, v,
                        |ix, iy| source.pixels[iy * source.size + ix][channel]);
            }
        }
    }
    Ok(tile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_tiles_share_exact_one_pixel_edge_on_nondivisible_grid() {
        let size = 32;
        let source = HeightField {
            size,
            values: (0..size).flat_map(|y| (0..size)
                .map(move |x| (x + y * 2) as f32 / (3 * (size - 1)) as f32))
                .collect(),
        };
        for count in [2, 3, 4] {
            let layout = tile_layout(size, count).unwrap();
            assert_eq!((layout.pixels_per_tile - 1) * count + 1,
                layout.stitched_pixels);
            let a = tile_height(&source, layout, 0, 0).unwrap();
            let b = tile_height(&source, layout, 1, 0).unwrap();
            let c = tile_height(&source, layout, 0, 1).unwrap();
            let n = layout.pixels_per_tile;
            for i in 0..n {
                assert_eq!(a.get(n-1, i), b.get(0, i));
                assert_eq!(a.get(i, n-1), c.get(i, 0));
            }
        }
    }

    #[test]
    fn rgba_tiles_have_matching_edges_and_original_corners() {
        let size = 33;
        let source = ColorField {
            size, pixels: (0..size).flat_map(|y| (0..size).map(move |x|
                [x as f32 / (size-1) as f32,
                 y as f32 / (size-1) as f32,
                 (x+y) as f32 / (2*(size-1)) as f32, 1.0]
            )).collect()
        };
        let layout = tile_layout(size, 2).unwrap();
        let left = tile_color(&source, layout, 0, 0).unwrap();
        let right = tile_color(&source, layout, 1, 0).unwrap();
        let bottom = tile_color(&source, layout, 0, 1).unwrap();
        let n = layout.pixels_per_tile;
        assert_eq!(left.pixels[0], source.pixels[0]);
        assert_eq!(left.pixels[(n-1)*n+(n-1)], right.pixels[(n-1)*n]);
        for y in 0..n {
            assert_eq!(left.pixels[y*n+n-1], right.pixels[y*n]);
            assert_eq!(left.pixels[(n-1)*n+y], bottom.pixels[y]);
        }
    }

    #[test]
    fn layout_rejects_invalid_tile_count_or_coordinates() {
        assert!(tile_layout(32, 1).is_err());
        assert!(tile_layout(32, 9).is_err());
        assert!(tile_layout(4, 4).is_err());
        let layout = tile_layout(32, 2).unwrap();
        assert!(tile_height(&HeightField::flat(32, 0.1), layout, 2, 0).is_err());
        assert!(tile_color(&ColorField::flat(32, [0.1; 4]), layout, 0, 2).is_err());
    }
}
