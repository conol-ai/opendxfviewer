//! Turning a scene plus a camera into a flat batch of screen-space primitives.
//!
//! Kept separate from [`crate::canvas`] so the decisions that govern how a drawing looks — which
//! primitives survive culling, how thick a line is, when a curve is re-tessellated, what counts as
//! too small to draw — are testable without a GPU.
//!
//! The batch is rebuilt whenever the view changes. That is affordable because culling bounds the
//! work by what is on screen rather than by the size of the drawing.

use crate::camera::Camera;
use crate::geom::{clip_segment, v2, Aabb, V2};
use crate::scene::{CurveSource, HAlign, PrimKind, Rgb, Scene, VAlign};
use crate::tessellate::{self as tess, Tol};

/// A line segment ready to hand to the GPU, in screen pixels.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Seg {
    pub a: V2,
    pub b: V2,
    /// Half the stroke width, in pixels.
    pub half_w: f32,
    pub color: Rgb,
}

/// A filled triangle in screen pixels.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FillTri {
    pub a: V2,
    pub b: V2,
    pub c: V2,
    pub color: Rgb,
}

/// A text run in screen pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub text: String,
    pub pos: V2,
    /// Cap height in pixels.
    pub height: f64,
    /// Baseline angle in radians, in screen space (so already Y-flipped).
    pub rotation: f64,
    pub halign: HAlign,
    pub valign: VAlign,
    pub color: Rgb,
}

/// How to draw.
#[derive(Copy, Clone, Debug)]
pub struct Style {
    /// Stroke width in pixels when line weights are off, or the floor when they are on.
    pub line_width: f32,
    /// Honour each entity's DXF lineweight instead of drawing everything hairline.
    pub use_lineweights: bool,
    /// Pixels per millimetre, used to turn a DXF lineweight into a stroke width.
    pub px_per_mm: f32,
    /// Diameter of a POINT entity's dot, in pixels.
    pub point_size: f32,
    /// Primitives whose screen bounding box is smaller than this are collapsed to a dot.
    pub min_feature_px: f64,
    /// Text smaller than this is not worth drawing.
    pub min_text_px: f64,
    /// Text larger than this is drawn as its outline box instead, to bound glyph work.
    pub max_text_px: f64,
    /// Stop after this many segments in one frame.
    pub max_segments: usize,
    /// Draw the file's dash patterns instead of every line solid.
    pub use_linetypes: bool,
    /// A dash pattern whose full cycle is shorter than this on screen is drawn solid. Below a few
    /// pixels a dash reads as noise, and costs a quad per dash to say nothing.
    pub min_dash_px: f64,
}

impl Style {
    /// A coarser variant for use while the user is actively panning or zooming.
    ///
    /// Dropping detail during a drag and restoring it when the view settles is the standard CAD
    /// trade: a dense drawing stays responsive under the hand, and the frame the user actually
    /// stops to look at is drawn in full.
    pub fn preview(&self) -> Style {
        Style {
            min_feature_px: self.min_feature_px.max(4.0),
            min_text_px: self.min_text_px.max(8.0),
            max_segments: self.max_segments.min(400_000),
            min_dash_px: self.min_dash_px.max(16.0),
            ..*self
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Style {
            line_width: 1.2,
            use_lineweights: false,
            px_per_mm: 3.78,
            point_size: 3.0,
            min_feature_px: 1.5,
            min_text_px: 4.0,
            max_text_px: 4000.0,
            max_segments: 1_500_000,
            use_linetypes: true,
            min_dash_px: 6.0,
        }
    }
}

/// The result of one build pass.
///
/// Reuse the same `Batch` across frames: it owns the scratch buffers, so a steady-state pan or zoom
/// allocates nothing.
#[derive(Clone, Debug, Default)]
pub struct Batch {
    pub segs: Vec<Seg>,
    pub tris: Vec<FillTri>,
    pub runs: Vec<Run>,
    /// Primitives the spatial index offered.
    pub considered: usize,
    /// Primitives that survived culling and were drawn.
    pub drawn: usize,
    /// True when [`Style::max_segments`] cut the frame short.
    pub truncated: bool,

    /// Per-primitive "last frame that touched this", one array per [`PrimKind`].
    ///
    /// The grid reports a primitive once per cell it spans, so a long line comes back many times
    /// and has to be deduplicated. A generation stamp does that with one array read and no
    /// hashing — at a few hundred thousand candidates per frame, hashing was the single largest
    /// cost in the whole draw path.
    seen: [Vec<u32>; 4],
    generation: u32,
    /// Scratch for re-tessellated curves.
    buf: Vec<V2>,
}

impl Batch {
    pub fn clear(&mut self) {
        self.segs.clear();
        self.tris.clear();
        self.runs.clear();
        self.considered = 0;
        self.drawn = 0;
        self.truncated = false;
    }

    /// Start a frame: bump the generation and make sure the stamp arrays cover the scene.
    fn begin(&mut self, scene: &Scene) {
        self.clear();
        let sizes = [scene.polys.len(), scene.dots.len(), scene.tris.len(), scene.texts.len()];
        // Wrapping would make a stale stamp look current, so restart the arrays instead.
        let (generation, reset) = match self.generation.checked_add(1) {
            Some(g) => (g, false),
            None => (1, true),
        };
        self.generation = generation;
        for (a, n) in self.seen.iter_mut().zip(sizes) {
            if a.len() != n || reset {
                a.clear();
                a.resize(n, 0);
            }
        }
    }

    fn first_sighting(&mut self, kind: PrimKind, idx: u32) -> bool {
        let a = &mut self.seen[kind as usize];
        match a.get_mut(idx as usize) {
            Some(slot) if *slot == self.generation => false,
            Some(slot) => {
                *slot = self.generation;
                true
            }
            // The stamp arrays are sized from the scene, so this only happens if the scene changed
            // underneath us. Drawing it twice is better than not drawing it.
            None => true,
        }
    }
}

/// Build the batch for `cam`'s current view of `scene`.
pub fn build(scene: &Scene, cam: &Camera, style: &Style, out: &mut Batch) {
    out.begin(scene);
    if scene.is_empty() {
        return;
    }

    // A margin so a wide stroke whose centreline is just off screen still contributes.
    let margin = style.line_width as f64 * 2.0 + style.point_size as f64 + 2.0;
    let world_view = cam.visible_world(margin);
    let clip = cam.view.expand(margin);

    // Take the scratch buffer out so the loop can borrow `out` mutably alongside it.
    let mut buf = std::mem::take(&mut out.buf);
    buf.clear();

    scene.query(&world_view, |kind, idx| {
        out.considered += 1;
        if out.segs.len() >= style.max_segments {
            out.truncated = true;
            return;
        }
        if !out.first_sighting(kind, idx) {
            return;
        }
        match kind {
            PrimKind::Poly => {
                let p = &scene.polys[idx as usize];
                if !scene.is_visible(p.layer) || !p.bbox.intersects(&world_view) {
                    return;
                }
                let sb = screen_bbox(cam, &p.bbox);
                let s = sb.size();
                // Something the size of a pixel does not need its vertices walked.
                if s.x < style.min_feature_px && s.y < style.min_feature_px {
                    push_dot(out, sb.center(), (style.line_width * 0.5).max(0.5), p.color, &clip);
                    out.drawn += 1;
                    return;
                }
                let hw = half_width(style, p.lineweight, scene, p.layer);
                let dash = dash_pattern(scene, p, cam, style);
                buf.clear();
                let pts = refined(scene, idx as usize, cam, &mut buf);
                // Nothing wider than a pixel apart can be told apart on screen, so skip vertices
                // rather than transforming every one of a curve stored for a closer view.
                let stride = stride_for(pts.len(), s.x.max(s.y));
                emit_polyline(out, cam, pts, stride, p.closed, hw, p.color, &clip, style, dash);
                out.drawn += 1;
            }
            PrimKind::Dot => {
                let d = &scene.dots[idx as usize];
                if !scene.is_visible(d.layer) || !world_view.contains(d.pos) {
                    return;
                }
                push_dot(out, cam.world_to_screen(d.pos), style.point_size * 0.5, d.color, &clip);
                out.drawn += 1;
            }
            PrimKind::Tri => {
                let t = &scene.tris[idx as usize];
                if !scene.is_visible(t.layer) || !t.bbox.intersects(&world_view) {
                    return;
                }
                let (a, b, c) =
                    (cam.world_to_screen(t.a), cam.world_to_screen(t.b), cam.world_to_screen(t.c));
                // A triangle thinner than a pixel renders as nothing; draw its edges instead so
                // edge-on 3D faces stay visible.
                if (b - a).cross(c - a).abs() < 1.0 {
                    let hw = style.line_width * 0.5;
                    for (p, q) in [(a, b), (b, c), (c, a)] {
                        push_seg(out, p, q, hw, t.color, &clip);
                    }
                } else {
                    out.tris.push(FillTri { a, b, c, color: t.color });
                }
                out.drawn += 1;
            }
            PrimKind::Text => {
                let t = &scene.texts[idx as usize];
                if !scene.is_visible(t.layer) || !t.bbox.intersects(&world_view) {
                    return;
                }
                let h = t.height * cam.scale;
                if h < style.min_text_px {
                    return;
                }
                if h > style.max_text_px {
                    // Absurdly zoomed-in text: outline where it sits rather than rasterising a
                    // glyph the size of the window.
                    let sb = screen_bbox(cam, &t.bbox);
                    for (p, q) in bbox_edges(&sb) {
                        push_seg(out, p, q, style.line_width * 0.5, t.color, &clip);
                    }
                    out.drawn += 1;
                    return;
                }
                out.runs.push(Run {
                    text: t.text.clone(),
                    pos: cam.world_to_screen(t.pos),
                    height: h,
                    // Screen Y points down, so a counter-clockwise world angle is clockwise here.
                    rotation: -t.rotation,
                    halign: t.halign,
                    valign: t.valign,
                    color: t.color,
                });
                out.drawn += 1;
            }
        }
    });
    out.buf = buf;
}

/// Vertices for a polyline, re-tessellating the curve it came from when the view has zoomed past
/// the fidelity baked in at load time.
///
/// Returns either the stored vertices or a freshly built buffer, so the caller keeps one scratch
/// allocation across the whole frame.
fn refined<'a>(scene: &'a Scene, idx: usize, cam: &Camera, buf: &'a mut Vec<V2>) -> &'a [V2] {
    let p = &scene.polys[idx];
    let stored = &scene.verts[p.start as usize..(p.start + p.len) as usize];
    // Chord error is a world-space quantity; on screen it is that times the zoom.
    let screen_tol = 0.35;
    let tol = Tol::new(screen_tol / cam.scale.max(f64::MIN_POSITIVE)).with_max(4096);

    match p.source {
        CurveSource::None => stored,
        CurveSource::Arc { center, radius, start, sweep } => {
            let want = tess::arc_segments(radius, sweep, tol);
            if want <= stored.len() {
                return stored;
            }
            tess::flatten_arc(center, radius, start, sweep, tol, buf);
            buf
        }
        CurveSource::Ellipse { center, major, minor, start, sweep } => {
            let r = major.len().max(minor.len());
            let want = tess::arc_segments(r, sweep, tol);
            if want <= stored.len() {
                return stored;
            }
            tess::flatten_ellipse(center, major, minor, start, sweep, tol, buf);
            buf
        }
        CurveSource::Spline { index } => {
            let Some(s) = scene.splines.get(index as usize) else { return stored };
            // Splines have no closed-form segment count; only refine once the drawing is large
            // enough on screen for facets to be visible at all.
            let sb = screen_bbox(cam, &p.bbox).size();
            if sb.x.max(sb.y) < stored.len() as f64 * 3.0 {
                return stored;
            }
            // Borrowed, not cloned: this runs for every visible spline on every redraw, and
            // copying three Vecs per curve per frame was the whole cost of the branch.
            tess::flatten_nurbs(s, tol.with_max(2048), buf);
            buf
        }
    }
}

/// A dash pattern resolved into screen pixels, or `None` for a solid line.
///
/// DXF stores dash lengths in drawing units, scaled by the drawing's `$LTSCALE` and again by the
/// entity's own multiplier, so the on-screen length also depends on the zoom.
#[derive(Copy, Clone)]
struct Dash<'a> {
    pattern: &'a [f64],
    /// Drawing units to screen pixels, including both linetype scales.
    scale: f64,
    total: f64,
}

fn dash_pattern<'a>(
    scene: &'a Scene,
    p: &crate::scene::Poly,
    cam: &Camera,
    style: &Style,
) -> Option<Dash<'a>> {
    if !style.use_linetypes {
        return None;
    }
    let lt = scene.linetypes.get(p.linetype as usize)?;
    if lt.is_solid() {
        return None;
    }
    let scale = scene.linetype_scale * p.linetype_scale as f64 * cam.scale;
    let total = lt.total * scale;
    if !total.is_finite() || total < style.min_dash_px {
        return None;
    }
    Some(Dash { pattern: &lt.pattern, scale, total })
}

impl Dash<'_> {
    /// Walk `len` pixels from pattern offset `at`, calling `f(start, end)` for each lit run.
    ///
    /// Returns the offset to resume from, so a polyline's dashes carry across its vertices rather
    /// than restarting at every corner — which is what makes a dashed circle look right.
    fn walk(&self, at: f64, len: f64, mut f: impl FnMut(f64, f64)) -> f64 {
        let mut pos = 0.0;
        let mut cursor = at.rem_euclid(self.total.max(f64::MIN_POSITIVE));
        // Find which element of the pattern `cursor` lands in, and how far into it.
        let (mut i, mut used) = (0usize, cursor);
        while i < self.pattern.len() {
            let e = (self.pattern[i] * self.scale).abs().max(DOT_PX);
            if used < e {
                break;
            }
            used -= e;
            i += 1;
        }
        if i >= self.pattern.len() {
            i = 0;
            used = 0.0;
        }
        while pos < len {
            let e = (self.pattern[i] * self.scale).abs().max(DOT_PX);
            let take = (e - used).min(len - pos);
            // A zero-length element is a dot, which the caller draws as a round cap.
            if self.pattern[i] >= 0.0 {
                f(pos, pos + take);
            }
            pos += take;
            used += take;
            if used >= e - 1e-12 {
                used = 0.0;
                i = (i + 1) % self.pattern.len();
            }
        }
        cursor += len;
        cursor.rem_euclid(self.total.max(f64::MIN_POSITIVE))
    }
}

/// On-screen length of a pattern element that the file gives as zero, i.e. a dot.
const DOT_PX: f64 = 0.6;

/// How many vertices to skip for a polyline of `n` points spanning `px` pixels.
///
/// Two samples per pixel is the most a display can resolve; beyond that the extra vertices only
/// cost transforms. Capped so a long polyline never degenerates into a couple of chords.
fn stride_for(n: usize, px: f64) -> usize {
    let budget = (px * 2.0).max(8.0);
    if (n as f64) <= budget {
        1
    } else {
        ((n as f64 / budget).floor() as usize).max(1)
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_polyline(
    out: &mut Batch,
    cam: &Camera,
    pts: &[V2],
    stride: usize,
    closed: bool,
    half_w: f32,
    color: Rgb,
    clip: &Aabb,
    style: &Style,
    dash: Option<Dash<'_>>,
) {
    if pts.len() < 2 {
        if let Some(&p) = pts.first() {
            push_dot(out, cam.world_to_screen(p), half_w, color, clip);
        }
        return;
    }
    debug_assert!(stride >= 1);
    let mut prev = cam.world_to_screen(pts[0]);
    let first = prev;
    // Dashes run continuously along the whole polyline rather than restarting at each vertex.
    let mut phase = 0.0f64;
    // Collapse runs of sub-pixel segments: a curve tessellated for a zoomed-in view emits hundreds
    // of vertices per pixel when zoomed out, and every one costs a quad.
    let min_step = style.min_feature_px.min(1.0);
    let mut pending: Option<V2> = None;
    // Always visit the final vertex, whatever the stride, so the shape closes where it should.
    let last = pts.len() - 1;
    let walk = pts[1..].iter().enumerate().filter_map(|(i, w)| {
        let i = i + 1;
        (i == last || i % stride == 0).then_some(*w)
    });
    for w in walk {
        let s = cam.world_to_screen(w);
        if (s - prev).len() < min_step {
            pending = Some(s);
            continue;
        }
        phase = push_run(out, prev, s, half_w, color, clip, dash, phase);
        prev = s;
        pending = None;
        if out.segs.len() >= style.max_segments {
            out.truncated = true;
            return;
        }
    }
    if let Some(s) = pending {
        phase = push_run(out, prev, s, half_w, color, clip, dash, phase);
        prev = s;
    }
    if closed {
        push_run(out, prev, first, half_w, color, clip, dash, phase);
    }
}

/// Emit one polyline span, either solid or broken into the dash pattern, and return the pattern
/// offset the next span should continue from.
#[allow(clippy::too_many_arguments)]
fn push_run(
    out: &mut Batch,
    a: V2,
    b: V2,
    half_w: f32,
    color: Rgb,
    clip: &Aabb,
    dash: Option<Dash<'_>>,
    phase: f64,
) -> f64 {
    let Some(d) = dash else {
        push_seg(out, a, b, half_w, color, clip);
        return phase;
    };
    let len = (b - a).len();
    if !len.is_finite() || len <= 0.0 {
        return phase;
    }
    let dir = (b - a) / len;
    d.walk(phase, len, |s, e| {
        push_seg(out, a + dir * s, a + dir * e, half_w, color, clip);
    })
}

fn push_seg(out: &mut Batch, a: V2, b: V2, half_w: f32, color: Rgb, clip: &Aabb) {
    if !(a.is_finite() && b.is_finite()) {
        return;
    }
    // Trim to the viewport before emitting: a quad per segment is only cheap if the quad is not
    // kilometres long.
    let pad = half_w as f64 + 2.0;
    if let Some((a, b)) = clip_segment(a, b, &clip.expand(pad)) {
        out.segs.push(Seg { a, b, half_w, color });
    }
}

fn push_dot(out: &mut Batch, p: V2, half_w: f32, color: Rgb, clip: &Aabb) {
    if p.is_finite() && clip.expand(half_w as f64 + 2.0).contains(p) {
        // A zero-length segment is a round cap, which is exactly a dot.
        out.segs.push(Seg { a: p, b: p, half_w, color });
    }
}

fn half_width(style: &Style, lineweight: Option<i16>, scene: &Scene, layer: u16) -> f32 {
    if !style.use_lineweights {
        return style.line_width * 0.5;
    }
    let lw = lineweight.or_else(|| scene.layers.get(layer as usize).and_then(|l| l.lineweight));
    match lw {
        // DXF stores lineweights in hundredths of a millimetre.
        Some(w) if w > 0 => {
            let px = (w as f32 / 100.0) * style.px_per_mm;
            px.max(style.line_width) * 0.5
        }
        _ => style.line_width * 0.5,
    }
}

fn screen_bbox(cam: &Camera, b: &Aabb) -> Aabb {
    if b.is_empty() {
        return Aabb::EMPTY;
    }
    let a = cam.world_to_screen(b.min);
    let c = cam.world_to_screen(b.max);
    Aabb::new(a.min(c), a.max(c))
}

fn bbox_edges(b: &Aabb) -> [(V2, V2); 4] {
    let (lo, hi) = (b.min, b.max);
    let (tl, tr) = (v2(lo.x, lo.y), v2(hi.x, lo.y));
    let (br, bl) = (v2(hi.x, hi.y), v2(lo.x, hi.y));
    [(tl, tr), (tr, br), (br, bl), (bl, tl)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{convert, Options};
    use crate::scene::{Dot, Text};

    fn scene_of(name: &str) -> Scene {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let dr = crate::read::load(&path).unwrap();
        convert(&dr, &Options::default())
    }

    fn fitted(s: &Scene, w: f64, h: f64) -> Camera {
        let mut cam = Camera { view: Aabb::new(V2::ZERO, v2(w, h)), ..Camera::default() };
        cam.fit(&s.bounds);
        cam
    }

    fn build_now(s: &Scene, cam: &Camera) -> Batch {
        let mut b = Batch::default();
        build(s, cam, &Style::default(), &mut b);
        b
    }

    #[test]
    fn a_fitted_view_draws_the_whole_drawing() {
        let s = scene_of("basic.dxf");
        let cam = fitted(&s, 800.0, 600.0);
        let b = build_now(&s, &cam);
        assert!(!b.segs.is_empty());
        assert_eq!(b.drawn, s.polys.len() + s.dots.len(), "every primitive should be on screen");
        assert!(!b.truncated);
    }

    #[test]
    fn everything_emitted_lands_inside_the_viewport() {
        let s = scene_of("basic.dxf");
        let cam = fitted(&s, 800.0, 600.0);
        let b = build_now(&s, &cam);
        let limit = cam.view.expand(32.0);
        for seg in &b.segs {
            assert!(limit.contains(seg.a) && limit.contains(seg.b), "{seg:?} escaped the viewport");
        }
    }

    #[test]
    fn zooming_in_culls_what_is_off_screen() {
        let s = scene_of("basic.dxf");
        let mut cam = fitted(&s, 800.0, 600.0);
        let all = build_now(&s, &cam).drawn;
        // Zoom hard onto one corner.
        cam.scale *= 40.0;
        cam.center = s.bounds.min;
        let near = build_now(&s, &cam);
        assert!(near.drawn < all, "culling did nothing: {} vs {all}", near.drawn);
        assert!(near.considered <= all * 2, "the index handed back too many candidates");
    }

    #[test]
    fn panning_far_away_draws_nothing() {
        let s = scene_of("basic.dxf");
        let mut cam = fitted(&s, 800.0, 600.0);
        cam.center = v2(1e9, 1e9);
        let b = build_now(&s, &cam);
        assert_eq!(b.drawn, 0);
        assert!(b.segs.is_empty());
    }

    #[test]
    fn a_hidden_layer_contributes_nothing() {
        let mut s = scene_of("basic.dxf");
        let cam = fitted(&s, 800.0, 600.0);
        let before = build_now(&s, &cam).drawn;
        let dims = s.layers.iter().position(|l| l.name == "DIMS").unwrap();
        let n_dims = s.layers[dims].count as usize;
        s.layers[dims].visible = false;
        let after = build_now(&s, &cam).drawn;
        assert_eq!(before - after, n_dims, "hiding DIMS should remove exactly its primitives");
    }

    #[test]
    fn a_layer_switched_off_in_the_file_is_hidden_too() {
        let mut s = scene_of("basic.dxf");
        let cam = fitted(&s, 800.0, 600.0);
        s.layers[0].visible_in_file = false;
        let b = build_now(&s, &cam);
        assert!(b.drawn < s.polys.len() + s.dots.len());
    }

    #[test]
    fn zooming_in_on_an_arc_refines_it() {
        let s = scene_of("basic.dxf");
        // Find the r=40 circle and count its segments at two zoom levels.
        let idx = s
            .polys
            .iter()
            .position(|p| matches!(p.source, CurveSource::Arc { radius, .. } if (radius - 40.0).abs() < 1e-9))
            .unwrap();
        let stored = s.polys[idx].len as usize;

        let mut cam = fitted(&s, 800.0, 600.0);
        cam.center = s.polys[idx].bbox.center();
        cam.scale = 4000.0; // the circle is now far wider than the window
        let mut buf = Vec::new();
        let pts = refined(&s, idx, &cam, &mut buf);
        assert!(pts.len() > stored * 4, "expected refinement past {stored}, got {}", pts.len());

        // Zoomed out, the stored tessellation already carries more detail than the view can show,
        // so nothing is rebuilt and the stored vertices are handed back untouched.
        let mut buf = Vec::new();
        let far = Camera { scale: 0.05, ..cam };
        let pts = refined(&s, idx, &far, &mut buf);
        assert_eq!(pts.len(), stored);
        assert!(buf.is_empty(), "the scratch buffer should not have been touched");
    }

    #[test]
    fn tiny_primitives_collapse_to_a_dot_instead_of_a_polyline() {
        let s = scene_of("basic.dxf");
        let mut cam = fitted(&s, 800.0, 600.0);
        // Zoom out until the whole drawing is a few pixels across.
        cam.scale /= 400.0;
        let b = build_now(&s, &cam);
        assert!(b.drawn > 0);
        assert!(
            b.segs.len() <= b.drawn + 4,
            "expected roughly one dot per primitive, got {} segments for {} primitives",
            b.segs.len(),
            b.drawn
        );
    }

    #[test]
    fn sub_pixel_vertices_are_collapsed_when_zoomed_out() {
        // A single finely tessellated circle, viewed small enough that most of its vertices land
        // on the same pixel.
        let mut s = Scene::default();
        let mut pts = Vec::new();
        tess::flatten_circle(V2::ZERO, 100.0, Tol::new(1e-4).with_max(4096), &mut pts);
        assert!(pts.len() > 1000, "need a dense curve to decimate, got {}", pts.len());
        let n = pts.len() as u32;
        s.verts = pts;
        s.polys.push(crate::scene::Poly {
            start: 0,
            len: n,
            layer: 0,
            color: Rgb::WHITE,
            lineweight: None,
            linetype: 0,
            linetype_scale: 1.0,
            closed: true,
            unbounded: false,
            bbox: Aabb::new(v2(-100.0, -100.0), v2(100.0, 100.0)),
            // No curve source, so `refined` cannot rebuild it — decimation is the only lever.
            source: CurveSource::None,
        });
        s.bounds = s.polys[0].bbox;
        s.build_index();

        // 60 px across: two samples per pixel is about 120 segments, not 1000+.
        let cam =
            Camera { center: V2::ZERO, scale: 0.3, view: Aabb::new(V2::ZERO, v2(800.0, 600.0)) };
        let b = build_now(&s, &cam);
        assert!(b.segs.len() < 200, "no decimation: {} segments for {n} vertices", b.segs.len());
        assert!(b.segs.len() > 20, "decimated into a polygon: {} segments", b.segs.len());
        // The ring must still close and still look like a circle.
        for seg in &b.segs {
            let r = (seg.a - cam.view.center()).len();
            assert!((r - 30.0).abs() < 2.0, "decimation deformed the circle: r={r}");
        }
    }

    #[test]
    fn stride_never_reduces_a_polyline_below_a_usable_outline() {
        assert_eq!(stride_for(4, 1000.0), 1, "short polylines are never strided");
        assert_eq!(stride_for(10_000, 1e9), 1, "a huge view keeps every vertex");
        assert!(stride_for(10_000, 100.0) > 1, "a small view should stride");
        // Even at zero screen size the floor keeps at least 8 samples.
        assert_eq!(stride_for(80, 0.0), 10);
        assert!(stride_for(1_000_000, 0.0) <= 125_000);
    }

    #[test]
    fn long_segments_are_clipped_rather_than_emitted_whole() {
        let mut s = Scene::default();
        // One segment spanning a million units through a small window.
        s.verts.extend([v2(-5e5, 0.0), v2(5e5, 0.0)]);
        s.polys.push(crate::scene::Poly {
            start: 0,
            len: 2,
            layer: 0,
            color: Rgb::WHITE,
            lineweight: None,
            linetype: 0,
            linetype_scale: 1.0,
            closed: false,
            unbounded: false,
            bbox: Aabb::new(v2(-5e5, 0.0), v2(5e5, 0.0)),
            source: CurveSource::None,
        });
        s.bounds = s.polys[0].bbox;
        s.build_index();

        let cam =
            Camera { center: V2::ZERO, scale: 1.0, view: Aabb::new(V2::ZERO, v2(800.0, 600.0)) };
        let b = build_now(&s, &cam);
        assert_eq!(b.segs.len(), 1);
        let seg = b.segs[0];
        assert!((seg.b - seg.a).len() < 900.0, "segment was not clipped: {seg:?}");
    }

    #[test]
    fn points_become_round_dots() {
        let mut s = Scene::default();
        s.dots.push(Dot { pos: V2::ZERO, layer: 0, color: Rgb::WHITE });
        s.bounds = Aabb::point(V2::ZERO);
        s.build_index();
        let cam =
            Camera { center: V2::ZERO, scale: 1.0, view: Aabb::new(V2::ZERO, v2(80.0, 60.0)) };
        let b = build_now(&s, &cam);
        assert_eq!(b.segs.len(), 1);
        assert_eq!(b.segs[0].a, b.segs[0].b, "a dot is a zero-length segment");
        assert!(b.segs[0].half_w > 0.0);
    }

    #[test]
    fn text_scales_with_zoom_and_disappears_when_illegible() {
        let mut s = Scene::default();
        s.texts.push(Text {
            text: "hello".into(),
            pos: V2::ZERO,
            height: 10.0,
            rotation: 0.5,
            width_factor: 1.0,
            halign: HAlign::Left,
            valign: VAlign::Baseline,
            layer: 0,
            color: Rgb::WHITE,
            bbox: Aabb::new(v2(-40.0, -20.0), v2(40.0, 20.0)),
        });
        s.bounds = s.texts[0].bbox;
        s.build_index();

        let mut cam =
            Camera { center: V2::ZERO, scale: 2.0, view: Aabb::new(V2::ZERO, v2(800.0, 600.0)) };
        let b = build_now(&s, &cam);
        assert_eq!(b.runs.len(), 1);
        assert!((b.runs[0].height - 20.0).abs() < 1e-9);
        // Screen Y is flipped, so the baseline angle flips with it.
        assert!((b.runs[0].rotation + 0.5).abs() < 1e-9);

        cam.scale = 0.1; // 1px tall
        assert!(build_now(&s, &cam).runs.is_empty());
    }

    /// One horizontal line across the middle of an 800x600 view, with the given DXF lineweight.
    fn one_line(lineweight: Option<i16>) -> Scene {
        let mut s = Scene::default();
        s.verts.extend([v2(-100.0, 0.0), v2(100.0, 0.0)]);
        s.layers.push(crate::scene::Layer {
            name: "0".into(),
            color: Rgb::WHITE,
            visible_in_file: true,
            visible: true,
            lineweight: None,
            linetype: 0,
            count: 1,
        });
        s.polys.push(crate::scene::Poly {
            start: 0,
            len: 2,
            layer: 0,
            color: Rgb::WHITE,
            lineweight,
            linetype: 0,
            linetype_scale: 1.0,
            closed: false,
            unbounded: false,
            bbox: Aabb::new(v2(-100.0, 0.0), v2(100.0, 0.0)),
            source: CurveSource::None,
        });
        s.bounds = s.polys[0].bbox;
        s.build_index();
        s
    }

    #[test]
    fn line_weights_are_honoured_only_when_asked_for() {
        let cam =
            Camera { center: V2::ZERO, scale: 1.0, view: Aabb::new(V2::ZERO, v2(800.0, 600.0)) };
        let heavy = one_line(Some(50)); // 0.50 mm
        let light = one_line(Some(9)); // 0.09 mm, thinner than the hairline floor

        let plain = Style::default();
        let weighted = Style { use_lineweights: true, ..Style::default() };

        let w = |s: &Scene, st: &Style| {
            let mut b = Batch::default();
            build(s, &cam, st, &mut b);
            b.segs[0].half_w
        };

        // Off: every stroke is hairline regardless of what the file says.
        assert_eq!(w(&heavy, &plain), w(&light, &plain));
        // On: 0.50 mm at 3.78 px/mm is 1.89 px wide, comfortably over the floor.
        assert!(w(&heavy, &weighted) > w(&heavy, &plain), "lineweight had no effect");
        assert!((w(&heavy, &weighted) - 0.5 * 0.5 * 3.78).abs() < 1e-5);
        // A lineweight below the hairline floor does not make a line thinner than legible.
        assert_eq!(w(&light, &weighted), w(&light, &plain));
    }

    #[test]
    fn a_collapsed_primitive_is_no_fatter_than_an_ordinary_stroke() {
        let s = scene_of("basic.dxf");
        let mut cam = fitted(&s, 800.0, 600.0);
        let normal =
            build_now(&s, &cam).segs.iter().map(|s| s.half_w).fold(f32::INFINITY, f32::min);
        assert!(normal > 0.0 && normal.is_finite());
        cam.scale /= 400.0; // everything collapses
        let collapsed = build_now(&s, &cam);
        let widest = collapsed.segs.iter().map(|s| s.half_w).fold(0.0f32, f32::max);
        assert!(
            widest <= Style::default().point_size * 0.5 + 1e-6,
            "collapsed dots are {widest} wide"
        );
    }

    #[test]
    fn a_primitive_spanning_many_grid_cells_is_only_drawn_once() {
        let s = scene_of("basic.dxf");
        let cam = fitted(&s, 800.0, 600.0);
        let b = build_now(&s, &cam);
        // The index over-reports; the batch must not.
        assert!(b.considered >= b.drawn);
        assert_eq!(b.drawn, s.polys.len() + s.dots.len());
    }

    // ---- dashed linetypes -------------------------------------------------------------

    fn dashed_scene(pattern: &[f64], ltscale: f64) -> Scene {
        let mut s = Scene { linetype_scale: ltscale, ..Scene::default() };
        s.linetypes.push(crate::scene::Linetype::default()); // 0 = CONTINUOUS
        s.linetypes.push(crate::scene::Linetype {
            name: "DASHED".into(),
            pattern: pattern.to_vec(),
            total: pattern.iter().map(|v| v.abs()).sum(),
        });
        s.layers.push(crate::scene::Layer {
            name: "0".into(),
            color: Rgb::WHITE,
            visible_in_file: true,
            visible: true,
            lineweight: None,
            linetype: 1,
            count: 1,
        });
        // A 1000-unit horizontal line.
        s.verts.extend([v2(0.0, 0.0), v2(1000.0, 0.0)]);
        s.polys.push(crate::scene::Poly {
            start: 0,
            len: 2,
            layer: 0,
            color: Rgb::WHITE,
            lineweight: None,
            linetype: 1,
            linetype_scale: 1.0,
            closed: false,
            unbounded: false,
            bbox: Aabb::new(v2(0.0, 0.0), v2(1000.0, 0.0)),
            source: CurveSource::None,
        });
        s.bounds = s.polys[0].bbox;
        s.build_index();
        s
    }

    fn wide_cam(scale: f64) -> Camera {
        Camera { center: v2(500.0, 0.0), scale, view: Aabb::new(V2::ZERO, v2(2000.0, 400.0)) }
    }

    #[test]
    fn a_dashed_line_becomes_several_segments() {
        // 20 units on, 10 off, at 1 px per unit: a 1000-unit line holds 33 full cycles.
        let s = dashed_scene(&[20.0, -10.0], 1.0);
        let mut b = Batch::default();
        build(&s, &wide_cam(1.0), &Style::default(), &mut b);
        assert!(b.segs.len() >= 30 && b.segs.len() <= 36, "{} dashes", b.segs.len());
        // Every dash is 20 px long and they are 30 px apart — except the last, which the end of
        // the line cuts short (1000 units is 33 and a third cycles).
        let mut xs: Vec<f64> = b.segs.iter().map(|s| s.a.x).collect();
        xs.sort_by(f64::total_cmp);
        let last = xs.last().copied().unwrap();
        for seg in &b.segs {
            let len = (seg.b - seg.a).len();
            if seg.a.x == last {
                assert!(len > 0.0 && len <= 20.5, "final partial dash {len}");
            } else {
                assert!((len - 20.0).abs() < 0.5, "dash length {:?}", seg);
            }
        }
        for w in xs.windows(2) {
            assert!((w[1] - w[0] - 30.0).abs() < 0.5, "dash spacing {:?}", w);
        }
    }

    #[test]
    fn the_same_line_is_solid_without_a_linetype() {
        let mut s = dashed_scene(&[20.0, -10.0], 1.0);
        s.polys[0].linetype = 0;
        let mut b = Batch::default();
        build(&s, &wide_cam(1.0), &Style::default(), &mut b);
        assert_eq!(b.segs.len(), 1);
    }

    #[test]
    fn linetypes_can_be_turned_off_entirely() {
        let s = dashed_scene(&[20.0, -10.0], 1.0);
        let mut b = Batch::default();
        build(&s, &wide_cam(1.0), &Style { use_linetypes: false, ..Style::default() }, &mut b);
        assert_eq!(b.segs.len(), 1);
    }

    #[test]
    fn a_pattern_too_fine_to_see_is_drawn_solid() {
        // Zoomed out until the whole 30-unit cycle is under a pixel, dashes would be noise.
        let s = dashed_scene(&[20.0, -10.0], 1.0);
        let mut b = Batch::default();
        build(&s, &wide_cam(0.01), &Style::default(), &mut b);
        assert_eq!(b.segs.len(), 1, "expected a solid line, got {} dashes", b.segs.len());
    }

    #[test]
    fn ltscale_stretches_the_pattern() {
        let coarse = dashed_scene(&[20.0, -10.0], 4.0);
        let fine = dashed_scene(&[20.0, -10.0], 1.0);
        let mut a = Batch::default();
        build(&coarse, &wide_cam(1.0), &Style::default(), &mut a);
        let mut c = Batch::default();
        build(&fine, &wide_cam(1.0), &Style::default(), &mut c);
        assert!(a.segs.len() * 3 < c.segs.len(), "{} vs {}", a.segs.len(), c.segs.len());
        // Four times the scale means four times the dash.
        let la = (a.segs[0].b - a.segs[0].a).len();
        let lc = (c.segs[0].b - c.segs[0].a).len();
        assert!((la / lc - 4.0).abs() < 0.1, "{la} vs {lc}");
    }

    #[test]
    fn an_entity_scale_multiplies_the_drawing_scale() {
        let mut s = dashed_scene(&[20.0, -10.0], 2.0);
        s.polys[0].linetype_scale = 3.0;
        let mut b = Batch::default();
        build(&s, &wide_cam(1.0), &Style::default(), &mut b);
        // 20 units * 2 * 3 = 120 px per dash.
        assert!(((b.segs[0].b - b.segs[0].a).len() - 120.0).abs() < 1.0, "{:?}", b.segs[0]);
    }

    #[test]
    fn dashes_carry_across_vertices_rather_than_restarting() {
        // Two collinear spans must produce the same dashes as one long span.
        let one = dashed_scene(&[20.0, -10.0], 1.0);
        let mut split = one.clone();
        split.verts = vec![v2(0.0, 0.0), v2(500.0, 0.0), v2(1000.0, 0.0)];
        split.polys[0].len = 3;
        let mut a = Batch::default();
        build(&one, &wide_cam(1.0), &Style::default(), &mut a);
        let mut c = Batch::default();
        build(&split, &wide_cam(1.0), &Style::default(), &mut c);
        assert_eq!(a.segs.len(), c.segs.len(), "the extra vertex changed the dash count");
        let round = |v: f64| (v * 100.0).round();
        let xa: Vec<f64> = a.segs.iter().map(|s| round(s.a.x)).collect();
        let xc: Vec<f64> = c.segs.iter().map(|s| round(s.a.x)).collect();
        assert_eq!(xa, xc, "dashes restarted at the vertex");
    }

    #[test]
    fn a_dot_in_the_pattern_still_draws_something() {
        // CENTER-style: long dash, gap, dot, gap.
        let s = dashed_scene(&[40.0, -10.0, 0.0, -10.0], 1.0);
        let mut b = Batch::default();
        build(&s, &wide_cam(1.0), &Style::default(), &mut b);
        assert!(b.segs.iter().any(|g| (g.b - g.a).len() < 1.0), "no dot was drawn");
        assert!(b.segs.iter().any(|g| (g.b - g.a).len() > 30.0), "no long dash was drawn");
    }

    #[test]
    fn a_degenerate_pattern_cannot_hang_or_explode() {
        for pattern in
            [vec![0.0, 0.0], vec![-5.0, -5.0], vec![1e-12, -1e-12], vec![f64::MAX, -1.0], vec![1.0]]
        {
            let s = dashed_scene(&pattern, 1.0);
            let mut b = Batch::default();
            build(&s, &wide_cam(1.0), &Style::default(), &mut b);
            assert!(b.segs.len() < 10_000, "pattern {pattern:?} produced {} segs", b.segs.len());
            for seg in &b.segs {
                assert!(seg.a.is_finite() && seg.b.is_finite(), "pattern {pattern:?}");
            }
        }
    }

    #[test]
    fn preview_style_is_cheaper_but_still_shows_everything() {
        let s = scene_of("basic.dxf");
        let cam = fitted(&s, 800.0, 600.0);
        let full = Style::default();
        let mut a = Batch::default();
        build(&s, &cam, &full, &mut a);
        let mut b = Batch::default();
        build(&s, &cam, &full.preview(), &mut b);
        assert!(b.segs.len() <= a.segs.len(), "preview emitted more work than full detail");
        // Coarser, but every primitive is still accounted for — preview drops detail, not objects.
        assert_eq!(b.drawn, a.drawn);
    }

    #[test]
    fn an_empty_scene_builds_an_empty_batch() {
        let s = Scene::default();
        let cam = Camera::default();
        let b = build_now(&s, &cam);
        assert!(b.segs.is_empty() && b.tris.is_empty() && b.runs.is_empty());
        assert_eq!(b.drawn, 0);
    }

    #[test]
    fn the_segment_budget_is_respected() {
        let s = scene_of("curves.dxf");
        let cam = fitted(&s, 800.0, 600.0);
        let mut b = Batch::default();
        build(&s, &cam, &Style { max_segments: 20, ..Style::default() }, &mut b);
        assert!(b.truncated);
        assert!(b.segs.len() < 200, "budget overshot badly: {}", b.segs.len());
    }

    #[test]
    fn no_batch_ever_contains_a_non_finite_coordinate() {
        for f in [
            "basic.dxf",
            "polylines.dxf",
            "curves.dxf",
            "blocks.dxf",
            "ocs_3d.dxf",
            "malformed.dxf",
        ] {
            let s = scene_of(f);
            for &scale in &[1e-6, 1.0, 1e3, 1e7] {
                let cam = Camera {
                    center: s.bounds.center(),
                    scale,
                    view: Aabb::new(V2::ZERO, v2(800.0, 600.0)),
                };
                let b = build_now(&s, &cam);
                for seg in &b.segs {
                    assert!(seg.a.is_finite() && seg.b.is_finite(), "{f} @ {scale}: {seg:?}");
                    assert!(seg.half_w.is_finite() && seg.half_w > 0.0);
                }
                for t in &b.tris {
                    assert!(t.a.is_finite() && t.b.is_finite() && t.c.is_finite(), "{f} @ {scale}");
                }
            }
        }
    }
}
