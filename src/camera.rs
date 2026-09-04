//! The world↔screen mapping.
//!
//! DXF is Y-up, screens are Y-down, so the vertical axis is flipped here and nowhere else.

use crate::geom::{v2, Aabb, V2};

/// How much of the viewport a zoom-to-fit leaves as margin, per side.
const FIT_MARGIN: f64 = 0.04;
/// Zoom is clamped so a pathological drawing extent cannot produce a degenerate transform.
const MIN_SCALE: f64 = 1e-12;
const MAX_SCALE: f64 = 1e12;

#[derive(Copy, Clone, Debug)]
pub struct Camera {
    /// World point shown at the centre of the viewport.
    pub center: V2,
    /// Screen pixels per world unit.
    pub scale: f64,
    /// Viewport in screen coordinates, as handed to us by the widget's layout.
    pub view: Aabb,
}

impl Default for Camera {
    fn default() -> Self {
        Camera { center: V2::ZERO, scale: 1.0, view: Aabb::new(V2::ZERO, v2(1.0, 1.0)) }
    }
}

impl Camera {
    pub fn world_to_screen(&self, p: V2) -> V2 {
        let c = self.view.center();
        v2(c.x + (p.x - self.center.x) * self.scale, c.y - (p.y - self.center.y) * self.scale)
    }

    pub fn screen_to_world(&self, p: V2) -> V2 {
        let c = self.view.center();
        v2(self.center.x + (p.x - c.x) / self.scale, self.center.y - (p.y - c.y) / self.scale)
    }

    /// The world-space region currently visible, expanded by `margin` screen pixels.
    pub fn visible_world(&self, margin: f64) -> Aabb {
        let e = self.view.expand(margin);
        let a = self.screen_to_world(e.min);
        let b = self.screen_to_world(e.max);
        Aabb::new(a.min(b), a.max(b))
    }

    /// Pan by a screen-space delta (a drag vector).
    pub fn pan_screen(&mut self, delta: V2) {
        self.center = self.center - v2(delta.x, -delta.y) / self.scale;
    }

    /// Zoom by `factor`, holding the world point under `anchor` (a screen position) fixed.
    pub fn zoom_at(&mut self, anchor: V2, factor: f64) {
        if !(factor.is_finite() && factor > 0.0) {
            return;
        }
        let before = self.screen_to_world(anchor);
        self.scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let after = self.screen_to_world(anchor);
        self.center += before - after;
    }

    /// Frame `b` in the viewport. An empty or degenerate box still yields a usable view.
    pub fn fit(&mut self, b: &Aabb) {
        let vs = self.view.size();
        if vs.x <= 0.0 || vs.y <= 0.0 {
            return;
        }
        if b.is_empty() {
            self.center = V2::ZERO;
            self.scale = 1.0;
            return;
        }
        self.center = b.center();
        let s = b.size();
        let usable = v2(vs.x * (1.0 - 2.0 * FIT_MARGIN), vs.y * (1.0 - 2.0 * FIT_MARGIN));
        // A drawing can be a single point or a perfectly straight line: fall back on the other
        // axis rather than dividing by zero.
        let sx = if s.x > 0.0 { usable.x / s.x } else { f64::INFINITY };
        let sy = if s.y > 0.0 { usable.y / s.y } else { f64::INFINITY };
        let fit = sx.min(sy);
        self.scale =
            if fit.is_finite() && fit > 0.0 { fit.clamp(MIN_SCALE, MAX_SCALE) } else { 1.0 };
    }

    /// Keep the world point at the viewport centre fixed when the widget is resized.
    pub fn set_view(&mut self, view: Aabb) {
        self.view = view;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Camera {
        Camera {
            center: v2(100.0, 50.0),
            scale: 2.0,
            view: Aabb::new(v2(0.0, 0.0), v2(800.0, 600.0)),
        }
    }

    fn close(a: V2, b: V2) -> bool {
        (a - b).len() < 1e-9
    }

    #[test]
    fn screen_and_world_round_trip() {
        let c = cam();
        for p in [v2(0.0, 0.0), v2(123.0, -456.0), v2(1e6, 1e6)] {
            assert!(close(c.screen_to_world(c.world_to_screen(p)), p));
        }
    }

    #[test]
    fn y_axis_is_flipped() {
        let c = cam();
        let below = c.world_to_screen(v2(100.0, 0.0));
        let above = c.world_to_screen(v2(100.0, 100.0));
        assert!(above.y < below.y, "world +Y must map to screen -Y");
    }

    #[test]
    fn camera_center_maps_to_view_center() {
        let c = cam();
        assert!(close(c.world_to_screen(c.center), c.view.center()));
    }

    #[test]
    fn zoom_holds_the_anchor_point() {
        let mut c = cam();
        let anchor = v2(700.0, 100.0);
        let before = c.screen_to_world(anchor);
        c.zoom_at(anchor, 1.7);
        assert!(close(c.screen_to_world(anchor), before));
        assert!((c.scale - 3.4).abs() < 1e-9);
    }

    #[test]
    fn zoom_ignores_nonsense_factors() {
        let mut c = cam();
        let s = c.scale;
        c.zoom_at(v2(1.0, 1.0), 0.0);
        c.zoom_at(v2(1.0, 1.0), -2.0);
        c.zoom_at(v2(1.0, 1.0), f64::NAN);
        assert_eq!(c.scale, s);
    }

    #[test]
    fn pan_moves_the_drawing_with_the_cursor() {
        let mut c = cam();
        let world_under_cursor = c.screen_to_world(v2(400.0, 300.0));
        c.pan_screen(v2(50.0, 20.0));
        assert!(close(c.screen_to_world(v2(450.0, 320.0)), world_under_cursor));
    }

    #[test]
    fn fit_frames_the_box_with_margin() {
        let mut c = cam();
        let b = Aabb::new(v2(0.0, 0.0), v2(100.0, 100.0));
        c.fit(&b);
        assert!(close(c.center, v2(50.0, 50.0)));
        // 600px tall viewport is the tighter axis: 600 * 0.92 / 100.
        assert!((c.scale - 5.52).abs() < 1e-9, "{}", c.scale);
        let vis = c.visible_world(0.0);
        assert!(vis.contains(b.min) && vis.contains(b.max));
    }

    #[test]
    fn fit_survives_degenerate_extents() {
        let mut c = cam();
        c.fit(&Aabb::EMPTY);
        assert!(c.scale.is_finite() && c.scale > 0.0);

        // A drawing that is a single horizontal line has zero height.
        let mut c = cam();
        c.fit(&Aabb::new(v2(0.0, 5.0), v2(100.0, 5.0)));
        assert!(c.scale.is_finite() && c.scale > 0.0);
        assert!(close(c.center, v2(50.0, 5.0)));

        // A single point has zero extent on both axes.
        let mut c = cam();
        c.fit(&Aabb::point(v2(7.0, 8.0)));
        assert!(c.scale.is_finite() && c.scale > 0.0);
    }

    #[test]
    fn visible_world_covers_the_viewport() {
        let c = cam();
        let vis = c.visible_world(0.0);
        for corner in [c.view.min, c.view.max, v2(c.view.min.x, c.view.max.y)] {
            assert!(vis.contains(c.screen_to_world(corner)));
        }
    }
}
