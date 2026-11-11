//! Deterministic priority-flood conditioning for drainage networks.
//!
//! All four map edges are potential outlets. Interior minima are raised to a
//! spill level with an optional small monotonically increasing downstream
//! grade. This changes a working heightfield: it does NOT simulate rainfall,
//! physical lake volume, or sediment conservation.
use crate::terrain::HeightField;
use std::{cmp::Ordering, collections::BinaryHeap};

#[derive(Clone, Debug)]
pub struct DepressionFields {
    /// Filled height surface usable as input to the existing D8 drainage node.
    pub height: HeightField,
    /// Amount added to each input height sample, in the input height units.
    /// A positive grade also introduces small values on otherwise flat terrain.
    pub fill_depth: HeightField,
    pub raised_cells: usize,
    pub fill_sum: f64,
}

#[derive(Clone, Copy, Debug)]
struct HeapCell {
    height: f32,
    index: usize,
}

impl PartialEq for HeapCell {
    fn eq(&self, other: &Self) -> bool {
        self.height.to_bits() == other.height.to_bits() && self.index == other.index
    }
}
impl Eq for HeapCell {}
impl PartialOrd for HeapCell {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapCell {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse the height ordering for a min-heap; tie-break by index for
        // stable, seed-independent results on plateaus.
        other.height.total_cmp(&self.height)
            .then_with(|| other.index.cmp(&self.index))
    }
}

/// With grade > 0, every non-edge cell has at least one strictly downhill D8
/// neighbor and therefore a path to a map boundary. Zero grade preserves
/// flat water surfaces but can leave flat regions terminal in downstream D8.
pub fn fill_depressions(
    source: &HeightField, grade: f32, cancelled: impl Fn() -> bool,
) -> Result<DepressionFields, String> {
    let n = source.size;
    let count = n.checked_mul(n).ok_or("Depression grid dimensions overflow")?;
    if n < 2 || source.values.len() != count {
        return Err("Depression fill requires a complete height grid of at least 2x2".into());
    }
    if source.values.iter().any(|height| !height.is_finite()) {
        return Err("Depression fill requires finite terrain heights".into());
    }
    if !grade.is_finite() || !(0.0..=0.01).contains(&grade) {
        return Err("Depression fill grade must be finite and within 0–0.01".into());
    }

    const NEIGHBORS: [(isize, isize); 8] = [
        (-1, 0), (0, -1), (1, 0), (0, 1),
        (-1, -1), (1, -1), (-1, 1), (1, 1),
    ];

    let mut conditioned = source.clone();
    let mut seen = vec![false; count];
    let mut heap = BinaryHeap::new();

    // Boundary cells are fixed outlet heights, so no ocean-level assumptions.
    for y in 0..n {
        for x in 0..n {
            if x != 0 && y != 0 && x != n - 1 && y != n - 1 { continue; }
            let index = y * n + x;
            seen[index] = true;
            heap.push(HeapCell { height: source.values[index], index });
        }
    }

    let mut processed = 0usize;
    while let Some(cell) = heap.pop() {
        processed += 1;
        if processed % 2048 == 0 && cancelled() {
            return Err("Build cancelled".into());
        }
        let x = (cell.index % n) as isize;
        let y = (cell.index / n) as isize;
        for (dx, dy) in NEIGHBORS {
            let xx = x + dx;
            let yy = y + dy;
            if xx < 0 || yy < 0 || xx >= n as isize || yy >= n as isize {
                continue;
            }
            let index = yy as usize * n + xx as usize;
            if seen[index] { continue; }
            seen[index] = true;
            let original = source.values[index];
            let filled = if grade == 0.0 {
                original.max(cell.height)
            } else {
                // At high elevations grade may be below one float ULP.
                // next_up guarantees a strict downhill edge on the f32 map.
                original.max((cell.height + grade).max(cell.height.next_up()))
            };
            if !filled.is_finite() {
                return Err("Depression fill exceeded finite terrain height range".into());
            }
            conditioned.values[index] = filled;
            heap.push(HeapCell { height: filled, index });
        }
    }
    if cancelled() { return Err("Build cancelled".into()); }

    let mut depth = HeightField::flat(n, 0.0);
    let mut raised_cells = 0;
    let mut fill_sum = 0.0_f64;
    for i in 0..count {
        let amount = (conditioned.values[i] - source.values[i]).max(0.0);
        depth.values[i] = amount;
        if amount > 0.0 { raised_cells += 1; }
        fill_sum += f64::from(amount);
    }
    Ok(DepressionFields {
        height: conditioned,
        fill_depth: depth,
        raised_cells,
        fill_sum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drainage::drainage;

    fn has_downhill_neighbor(grid: &HeightField, x: usize, y: usize) -> bool {
        let n = grid.size as isize;
        let h = grid.get(x, y);
        (-1..=1).any(|dy| (-1..=1).any(|dx| {
            let xx = x as isize + dx;
            let yy = y as isize + dy;
            (dx != 0 || dy != 0) && xx >= 0 && yy >= 0
                && xx < n && yy < n && grid.get(xx as usize, yy as usize) < h
        }))
    }

    #[test]
    fn fills_enclosed_pit_and_creates_outlet_connected_routing() {
        let n = 33;
        let mut source = HeightField::flat(n, 0.6);
        for x in 0..n {
            source.values[x] = 0.0;
            source.values[(n-1)*n + x] = 0.0;
        }
        for y in 0..n {
            source.values[y*n] = 0.0;
            source.values[y*n+n-1] = 0.0;
        }
        source.values[(n/2)*n + n/2] = 0.1;

        let output = fill_depressions(&source, 0.0001, || false).unwrap();
        assert!(output.height.get(n/2, n/2) >= 0.6);
        assert!(output.fill_depth.get(n/2, n/2) > 0.5);
        assert!(output.raised_cells > 1);
        for i in 0..n {
            assert_eq!(output.height.get(0, i), source.get(0, i));
            assert_eq!(output.height.get(n-1, i), source.get(n-1, i));
            assert_eq!(output.height.get(i, 0), source.get(i, 0));
            assert_eq!(output.height.get(i, n-1), source.get(i, n-1));
        }
        for y in 1..(n-1) {
            for x in 1..(n-1) {
                assert!(has_downhill_neighbor(&output.height, x, y),
                    "conditioned height is trapped at ({x},{y})");
            }
        }
        let analyzed = drainage(&output.height, 0.02).unwrap();
        assert_eq!(analyzed.outlet_area, (n*n) as u64);
        assert_eq!(output.height, fill_depressions(&source, 0.0001, || false)
            .unwrap().height);
        let actual_delta: f64 = output.height.values.iter().zip(&source.values)
            .map(|(a,b)| f64::from(a-b)).sum();
        assert!((actual_delta - output.fill_sum).abs() < 1e-7);
    }

    #[test]
    fn flat_terrain_no_grade_is_identity_positive_grade_routes_to_edge() {
        let original = HeightField::flat(32, 0.4);
        let flat = fill_depressions(&original, 0.0, || false).unwrap();
        assert_eq!(flat.height, original);
        assert_eq!(flat.raised_cells, 0);
        let graded = fill_depressions(&original, 0.0001, || false).unwrap();
        assert!(graded.fill_sum > 0.0);
        for y in 1..31 {
            for x in 1..31 {
                assert!(has_downhill_neighbor(&graded.height, x, y));
            }
        }
        assert_eq!(drainage(&graded.height, 0.01).unwrap().outlet_area, 1024);
    }

    #[test]
    fn rejects_invalid_data_and_cancels() {
        let source = HeightField::flat(64, 0.5);
        assert!(fill_depressions(&source, -0.001, || false).is_err());
        assert!(fill_depressions(&source, f32::NAN, || false).is_err());
        assert!(fill_depressions(&source, 0.1, || false).is_err());
        let mut invalid = source.clone();
        invalid.values[31] = f32::INFINITY;
        assert!(fill_depressions(&invalid, 0.0001, || false).is_err());
        assert!(fill_depressions(&source, 0.0001, || true).is_err());
        assert_eq!(fill_depressions(&HeightField::flat(2, 0.3),
            0.0001, || false).unwrap().raised_cells, 0);
    }
}
