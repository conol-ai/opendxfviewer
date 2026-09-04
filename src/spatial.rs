//! Uniform-grid culling.
//!
//! A DXF drawing is overwhelmingly local: zoomed in on one detail, almost nothing else is on
//! screen. Bucketing primitives into a grid once at load time turns "what is visible" from a scan
//! of every primitive into a scan of a handful of cells.
//!
//! A grid rather than a BVH because CAD geometry is close to uniformly distributed over the sheet,
//! build time is linear, and lookups need no traversal.

use crate::geom::{v2, Aabb};
use crate::scene::{Grid, PrimKind, Scene};

/// Target average occupancy. Cells this size keep the per-cell lists short without exploding the
/// offset table on drawings whose primitives cluster.
const TARGET_PER_CELL: f64 = 4.0;
const MAX_CELLS: usize = 1 << 20;
/// A primitive touching more cells than this goes on the always-reported list rather than being
/// written into each one. Without a cap, a few drawing-wide construction lines would each add an
/// entry per cell and the index would outgrow the scene it indexes.
const MAX_CELLS_PER_PRIM: u64 = 256;

impl Scene {
    /// Bucket every primitive into a uniform grid. Idempotent; safe to call after edits.
    pub fn build_index(&mut self) {
        let bounds = self.bounds;
        let mut prims: Vec<(PrimKind, u32, Aabb)> = Vec::with_capacity(
            self.polys.len() + self.dots.len() + self.tris.len() + self.texts.len(),
        );
        for (i, p) in self.polys.iter().enumerate() {
            prims.push((PrimKind::Poly, i as u32, p.bbox));
        }
        for (i, d) in self.dots.iter().enumerate() {
            prims.push((PrimKind::Dot, i as u32, Aabb::point(d.pos)));
        }
        for (i, t) in self.tris.iter().enumerate() {
            prims.push((PrimKind::Tri, i as u32, t.bbox));
        }
        for (i, t) in self.texts.iter().enumerate() {
            prims.push((PrimKind::Text, i as u32, t.bbox));
        }

        self.index = build(bounds, &prims);
    }

    /// Call `f` for every primitive whose bounding box may intersect `area`.
    ///
    /// A primitive spanning several cells is reported once per cell it occupies, so callers that
    /// cannot tolerate duplicates must deduplicate. The renderer can: drawing a segment twice is
    /// only wasted work, and the common case (a primitive inside one cell) has no duplicates.
    pub fn query(&self, area: &Aabb, mut f: impl FnMut(PrimKind, u32)) {
        let g = &self.index;
        if g.is_empty() {
            // No index built: fall back to reporting everything rather than drawing nothing.
            for i in 0..self.polys.len() {
                f(PrimKind::Poly, i as u32);
            }
            for i in 0..self.dots.len() {
                f(PrimKind::Dot, i as u32);
            }
            for i in 0..self.tris.len() {
                f(PrimKind::Tri, i as u32);
            }
            for i in 0..self.texts.len() {
                f(PrimKind::Text, i as u32);
            }
            return;
        }
        for &(kind, idx) in &g.large {
            f(kind, idx);
        }
        let (x0, y0, x1, y1) = g.cell_range(area);
        for cy in y0..=y1 {
            let row = cy * g.cols;
            for cx in x0..=x1 {
                let c = (row + cx) as usize;
                for &(kind, idx) in &g.items[g.cells[c] as usize..g.cells[c + 1] as usize] {
                    f(kind, idx);
                }
            }
        }
    }
}

impl Grid {
    /// Inclusive cell range covering `area`, clamped to the grid.
    fn cell_range(&self, area: &Aabb) -> (u32, u32, u32, u32) {
        let clamp = |v: f64, n: u32| -> u32 {
            if !v.is_finite() {
                if v > 0.0 {
                    n - 1
                } else {
                    0
                }
            } else {
                (v.floor().max(0.0) as u32).min(n - 1)
            }
        };
        let rel_min = area.min - self.bounds.min;
        let rel_max = area.max - self.bounds.min;
        (
            clamp(rel_min.x / self.cell.x, self.cols),
            clamp(rel_min.y / self.cell.y, self.rows),
            clamp(rel_max.x / self.cell.x, self.cols),
            clamp(rel_max.y / self.cell.y, self.rows),
        )
    }
}

fn build(bounds: Aabb, prims: &[(PrimKind, u32, Aabb)]) -> Grid {
    if prims.is_empty() || bounds.is_empty() {
        return Grid::default();
    }
    let size = bounds.size();
    // A drawing can be perfectly flat on one axis; give that axis a single column of finite width.
    let ext = v2(size.x.max(f64::MIN_POSITIVE), size.y.max(f64::MIN_POSITIVE));
    let area = ext.x * ext.y;

    let target = (prims.len() as f64 / TARGET_PER_CELL).max(1.0);
    // Cells that are square in world space: n_cells = target, aspect preserved.
    let cell_side = (area / target).sqrt().max(f64::MIN_POSITIVE);
    let mut cols = (ext.x / cell_side).ceil().max(1.0).min(MAX_CELLS as f64) as u32;
    let mut rows = (ext.y / cell_side).ceil().max(1.0).min(MAX_CELLS as f64) as u32;
    while cols as usize * rows as usize > MAX_CELLS {
        cols = (cols / 2).max(1);
        rows = (rows / 2).max(1);
    }
    let cell = v2(ext.x / cols as f64, ext.y / rows as f64);
    let n = cols as usize * rows as usize;

    let g = Grid {
        bounds,
        cols,
        rows,
        cell,
        cells: vec![0; n + 1],
        items: Vec::new(),
        large: Vec::new(),
    };

    // Two passes: count per cell, prefix-sum into offsets, then scatter. No per-cell Vec.
    let mut counts = vec![0u32; n];
    let mut large: Vec<(PrimKind, u32)> = Vec::new();
    // `None` marks a primitive that went on the always-reported list instead of into cells.
    let mut spans: Vec<Option<(u32, u32, u32, u32)>> = Vec::with_capacity(prims.len());
    for &(kind, idx, b) in prims {
        let b = if b.is_empty() { Aabb::point(bounds.min) } else { b };
        let r = g.cell_range(&b);
        let touched = (r.2 - r.0 + 1) as u64 * (r.3 - r.1 + 1) as u64;
        if touched > MAX_CELLS_PER_PRIM {
            large.push((kind, idx));
            spans.push(None);
            continue;
        }
        spans.push(Some(r));
        for cy in r.1..=r.3 {
            for cx in r.0..=r.2 {
                counts[(cy * cols + cx) as usize] += 1;
            }
        }
    }

    let mut cells = vec![0u32; n + 1];
    let mut acc = 0u32;
    for i in 0..n {
        cells[i] = acc;
        acc = acc.saturating_add(counts[i]);
    }
    cells[n] = acc;

    let mut items = vec![(PrimKind::Poly, 0u32); acc as usize];
    let mut cursor = cells.clone();
    for (&(kind, idx, _), span) in prims.iter().zip(&spans) {
        let Some(r) = span else { continue };
        for cy in r.1..=r.3 {
            for cx in r.0..=r.2 {
                let c = (cy * cols + cx) as usize;
                items[cursor[c] as usize] = (kind, idx);
                cursor[c] += 1;
            }
        }
    }

    Grid { bounds, cols, rows, cell, cells, items, large }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{CurveSource, Dot, Poly, Rgb};
    use std::collections::HashSet;

    fn poly_at(b: Aabb) -> Poly {
        Poly {
            start: 0,
            len: 0,
            layer: 0,
            color: Rgb::WHITE,
            lineweight: None,
            linetype: 0,
            linetype_scale: 1.0,
            closed: false,
            unbounded: false,
            bbox: b,
            source: CurveSource::None,
        }
    }

    /// A grid of unit squares, one per integer cell in a 32x32 world.
    fn grid_scene(n: i32) -> Scene {
        let mut s = Scene::default();
        for y in 0..n {
            for x in 0..n {
                let (x, y) = (x as f64, y as f64);
                s.polys.push(poly_at(Aabb::new(v2(x, y), v2(x + 0.9, y + 0.9))));
            }
        }
        s.bounds = Aabb::new(v2(0.0, 0.0), v2(n as f64, n as f64));
        s.build_index();
        s
    }

    fn hits(s: &Scene, area: Aabb) -> HashSet<(u8, u32)> {
        let mut out = HashSet::new();
        s.query(&area, |k, i| {
            out.insert((k as u8, i));
        });
        out
    }

    /// The invariant that matters: a query never misses a primitive it overlaps.
    fn brute(s: &Scene, area: &Aabb) -> HashSet<(u8, u32)> {
        let mut out = HashSet::new();
        for (i, p) in s.polys.iter().enumerate() {
            if p.bbox.intersects(area) {
                out.insert((PrimKind::Poly as u8, i as u32));
            }
        }
        for (i, d) in s.dots.iter().enumerate() {
            if area.contains(d.pos) {
                out.insert((PrimKind::Dot as u8, i as u32));
            }
        }
        out
    }

    #[test]
    fn query_is_conservative_over_many_windows() {
        let s = grid_scene(32);
        for &(x, y, w, h) in &[
            (0.0, 0.0, 1.0, 1.0),
            (5.5, 7.5, 3.0, 2.0),
            (-10.0, -10.0, 5.0, 5.0),
            (0.0, 0.0, 32.0, 32.0),
            (31.5, 31.5, 10.0, 10.0),
            (15.0, 0.0, 0.01, 32.0),
        ] {
            let area = Aabb::new(v2(x, y), v2(x + w, y + h));
            let got = hits(&s, area);
            for want in brute(&s, &area) {
                assert!(got.contains(&want), "missed {want:?} for {area:?}");
            }
        }
    }

    #[test]
    fn query_actually_narrows() {
        let s = grid_scene(32);
        let one = hits(&s, Aabb::new(v2(0.0, 0.0), v2(0.5, 0.5)));
        assert!(one.len() < 40, "expected a small candidate set, got {}", one.len());
        let all = hits(&s, Aabb::new(v2(0.0, 0.0), v2(32.0, 32.0)));
        assert_eq!(all.len(), 32 * 32);
    }

    #[test]
    fn out_of_bounds_query_returns_nothing_real() {
        let s = grid_scene(8);
        // Far outside: clamping means we may touch a border cell, but nothing there overlaps.
        let area = Aabb::new(v2(1e6, 1e6), v2(1e6 + 1.0, 1e6 + 1.0));
        for want in brute(&s, &area) {
            assert!(hits(&s, area).contains(&want));
        }
        assert!(brute(&s, &area).is_empty());
    }

    #[test]
    fn unindexed_scene_falls_back_to_everything() {
        let mut s = Scene::default();
        s.polys.push(poly_at(Aabb::new(v2(0.0, 0.0), v2(1.0, 1.0))));
        s.dots.push(Dot { pos: v2(5.0, 5.0), layer: 0, color: Rgb::WHITE });
        // No build_index() call.
        let got = hits(&s, Aabb::new(v2(-1e9, -1e9), v2(-1e8, -1e8)));
        assert_eq!(got.len(), 2, "an unbuilt index must not silently hide geometry");
    }

    #[test]
    fn degenerate_extents_do_not_panic() {
        for bounds in [
            Aabb::point(v2(3.0, 3.0)),
            Aabb::new(v2(0.0, 5.0), v2(100.0, 5.0)), // zero height
            Aabb::new(v2(5.0, 0.0), v2(5.0, 100.0)), // zero width
        ] {
            let mut s = Scene::default();
            s.polys.push(poly_at(bounds));
            s.bounds = bounds;
            s.build_index();
            assert!(!hits(&s, bounds.expand(1.0)).is_empty());
        }
    }

    #[test]
    fn a_primitive_spanning_the_whole_grid_is_not_written_into_every_cell() {
        // Without a cap, a drawing-wide line costs one index entry per cell, and a handful of
        // them make the index larger than the scene it indexes.
        let mut s = grid_scene(64);
        let cells = s.index.cells.len();
        let before = s.index.items.len();
        for _ in 0..20 {
            s.polys.push(poly_at(Aabb::new(v2(-1e6, -1e6), v2(1e6, 1e6))));
        }
        s.build_index();
        assert!(
            s.index.items.len() < before + 20 * MAX_CELLS_PER_PRIM as usize,
            "index grew by {} entries for 20 primitives",
            s.index.items.len() - before
        );
        assert_eq!(s.index.large.len(), 20, "they should be on the always-reported list");
        // And they are still found, from anywhere.
        for &(x, y) in &[(0.0, 0.0), (32.0, 32.0), (-500.0, 500.0)] {
            let got = hits(&s, Aabb::new(v2(x, y), v2(x + 0.1, y + 0.1)));
            for i in (s.polys.len() - 20)..s.polys.len() {
                assert!(got.contains(&(0u8, i as u32)), "large primitive {i} missed at {x},{y}");
            }
        }
        let _ = cells;
    }

    #[test]
    fn one_huge_primitive_among_many_small_ones_is_always_found() {
        let mut s = grid_scene(16);
        s.polys.push(poly_at(Aabb::new(v2(-100.0, -100.0), v2(100.0, 100.0))));
        s.bounds = s.bounds.union(&s.polys.last().unwrap().bbox);
        s.build_index();
        let big = (PrimKind::Poly as u8, (s.polys.len() - 1) as u32);
        for &(x, y) in &[(0.0, 0.0), (8.0, 8.0), (-50.0, 50.0), (99.0, -99.0)] {
            let area = Aabb::new(v2(x, y), v2(x + 0.1, y + 0.1));
            assert!(hits(&s, area).contains(&big), "big primitive missed at {x},{y}");
        }
    }
}
