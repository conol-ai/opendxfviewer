//! The drawing canvas: a custom Makepad widget that renders a [`Scene`] and handles navigation.
//!
//! Two things make this fast enough for CAD-sized drawings.
//!
//! **Makepad retains draw lists.** `draw_walk` runs only when something calls `redraw`, and between
//! redraws the emitted instance buffers stay on the GPU. A view nobody is touching costs nothing,
//! so the expensive path is only paid while the user is actually moving.
//!
//! **One instanced quad per segment.** Geometry goes out as raw `f32` through
//! `cx.begin_many_instances`, which lands the whole drawing in a single draw call. The vertex
//! shader builds an oriented quad around each segment rather than its bounding box, so a long
//! diagonal costs its own area in fragments instead of the square of its length.

use makepad_widgets::*;

use crate::camera::Camera;
use crate::geom::{v2, Aabb, V2};
// The Live derive's type parser does not accept `::` paths in field types, so every
// field type below has to be a bare identifier.
use crate::render::{self, Batch, Seg, Style};
use crate::scene::{HAlign, Rgb, Scene, VAlign};

live_design! {
    use link::theme::*;
    use link::shaders::*;
    use link::widgets::*;

    // An oriented quad per segment, with a capsule SDF for round caps and 1px antialiasing.
    //
    // `rect_pos` and `rect_size` are reused as the segment's start point and its delta: the
    // vertex shader is overridden, so their usual meaning never applies. That does mean the
    // shader's own Area no longer describes a rectangle, which is why hit testing goes through
    // the widget's area instead.
    pub DrawSeg = {{DrawSeg}} {
        varying sp: vec2

        fn vertex(self) -> vec4 {
            let a = self.rect_pos;
            let d = self.rect_size;
            let len = length(d);
            // A dot is a zero-length segment; give it an arbitrary direction rather than 0/0.
            let sl = max(len, 0.0001);
            let dir = mix(vec2(1.0, 0.0), d / sl, step(0.0001, len));
            let nrm = vec2(-dir.y, dir.x);
            let hw = self.half_width + 1.0;

            let along = self.geom_pos.x * (sl + 2.0 * hw) - hw;
            let across = (self.geom_pos.y - 0.5) * 2.0 * hw;
            let p = a + dir * along + nrm * across;

            self.sp = p;
            // x runs 0..1 along the segment, y runs -1..1 across it.
            self.pos = vec2(along / sl, across / hw);
            self.world = self.view_transform * vec4(p.x, p.y, self.draw_depth + self.draw_zbias, 1.0);
            return self.camera_projection * (self.camera_view * self.world);
        }

        fn pixel(self) -> vec4 {
            // MPSL has no discard, so clip by multiplying the alpha out.
            let ins = step(self.draw_clip.xy, self.sp) * step(self.sp, self.draw_clip.zw);
            let keep = ins.x * ins.y;

            let sl = max(length(self.rect_size), 0.0001);
            let across_px = abs(self.pos.y) * (self.half_width + 1.0);
            let cap = max(0.0, max(-self.pos.x, self.pos.x - 1.0)) * sl;
            let d = length(vec2(cap, across_px)) - self.half_width;

            let aa = 1.0 / max(length(vec2(length(dFdx(self.sp)), length(dFdy(self.sp)))), 0.0001);
            let alpha = clamp(0.5 - d * aa, 0.0, 1.0) * keep;
            return vec4(self.color.rgb * alpha, alpha);
        }
    }

    // A filled triangle, drawn as a quad with two corners collapsed onto the third.
    pub DrawTri = {{DrawTri}} {
        varying sp: vec2

        fn vertex(self) -> vec4 {
            let gx = self.geom_pos.x;
            let gy = self.geom_pos.y;
            // (0,0)->a  (1,0)->b  (0,1)->c  (1,1)->c
            let p = self.pa + (self.pb - self.pa) * gx * (1.0 - gy) + (self.pc - self.pa) * gy;
            self.sp = p;
            self.world = self.view_transform * vec4(p.x, p.y, self.draw_depth + self.draw_zbias, 1.0);
            return self.camera_projection * (self.camera_view * self.world);
        }

        fn pixel(self) -> vec4 {
            let ins = step(self.draw_clip.xy, self.sp) * step(self.sp, self.draw_clip.zw);
            let keep = ins.x * ins.y;
            return vec4(self.color.rgb * self.color.a * keep, self.color.a * keep);
        }
    }

    pub DxfCanvasBase = {{DxfCanvas}} {}

    pub DxfCanvas = <DxfCanvasBase> {
        width: Fill, height: Fill,
        draw_bg: { color: #1b1b1e }
        draw_seg: {}
        draw_tri: {}
        draw_text: {
            color: #d0d0d4
            text_style: <THEME_FONT_REGULAR> { font_size: 9.0 }
        }
    }
}

/// One instanced line segment.
#[derive(Live, LiveRegister)]
#[repr(C)]
pub struct DrawSeg {
    #[deref]
    pub draw_super: DrawQuad,
    #[calc]
    pub half_width: f32,
    #[calc]
    pub color: Vec4,
}

/// Initialise a `DrawQuad`-derived shader's `DrawVars` on apply.
///
/// `derive(LiveHook)` produces an *empty* impl, which leaves `draw_vars.draw_shader` as `None`:
/// nothing renders and nothing is logged. Every shader struct here needs these two hooks, and they
/// are identical for all of them.
macro_rules! impl_draw_quad_live_hook {
    ($ty:ty) => {
        impl LiveHook for $ty {
            fn before_apply(
                &mut self,
                cx: &mut Cx,
                apply: &mut Apply,
                index: usize,
                nodes: &[LiveNode],
            ) {
                let geom = &self.draw_super.geometry;
                self.draw_super.draw_vars.before_apply_init_shader(cx, apply, index, nodes, geom);
            }
            fn after_apply(
                &mut self,
                cx: &mut Cx,
                apply: &mut Apply,
                index: usize,
                nodes: &[LiveNode],
            ) {
                let geom = &self.draw_super.geometry;
                self.draw_super.draw_vars.after_apply_update_self(cx, apply, index, nodes, geom);
            }
        }
    };
}

impl_draw_quad_live_hook!(DrawSeg);

/// One instanced filled triangle.
#[derive(Live, LiveRegister)]
#[repr(C)]
pub struct DrawTri {
    #[deref]
    pub draw_super: DrawQuad,
    #[calc]
    pub pa: Vec2,
    #[calc]
    pub pb: Vec2,
    #[calc]
    pub pc: Vec2,
    #[calc]
    pub color: Vec4,
}

impl_draw_quad_live_hook!(DrawTri);

/// What the canvas tells the app about.
#[derive(Clone, Debug, DefaultNone)]
pub enum DxfCanvasAction {
    /// The view moved; the app updates the status bar.
    ViewChanged,
    /// The pointer moved to this world position.
    Hover(V2),
    None,
}

/// How long after the last navigation input to redraw at full detail.
const SETTLE_SECONDS: f64 = 0.12;
/// Fraction of the viewport an arrow-key press pans.
const KEY_PAN_FRACTION: f64 = 0.15;
/// Zoom factor per keyboard zoom step.
const KEY_ZOOM_STEP: f64 = 1.25;

#[derive(Live, LiveHook, Widget)]
pub struct DxfCanvas {
    #[redraw]
    #[rust]
    area: Area,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,

    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_seg: DrawSeg,
    #[live]
    draw_tri: DrawTri,
    #[live]
    draw_text: DrawText,

    /// Owned outright rather than shared: the layer panel toggles visibility on it, and an `Rc`
    /// would either force a copy of the whole scene or an interior-mutability wrapper.
    #[rust]
    scene: Scene,
    #[rust]
    cam: Camera,
    #[rust]
    style: Style,
    #[rust]
    batch: Batch,

    /// Camera centre when the current drag began. Finger events report a cumulative delta from the
    /// press, so panning has to be computed from a snapshot rather than accumulated.
    #[rust]
    pan_from: Option<V2>,
    /// True while the user is navigating, which switches to the cheaper preview style.
    #[rust]
    moving: bool,
    #[rust]
    settle: Timer,
    /// Set once the first draw has run, so the camera can be fitted against a real viewport.
    #[rust]
    fitted: bool,
    #[rust]
    pending_fit: bool,
    #[rust]
    last_hover: V2,
    /// Segments emitted by the last draw, for the status bar.
    #[rust]
    pub last_seg_count: usize,
}

impl DxfCanvas {
    pub fn set_scene(&mut self, cx: &mut Cx, scene: Scene) {
        self.scene = scene;
        self.pending_fit = true;
        self.fitted = false;
        self.redraw(cx);
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Show or hide one layer and redraw. Out-of-range indices are ignored rather than panicking:
    /// the panel's list can lag a newly loaded drawing by one frame.
    pub fn set_layer_visible(&mut self, cx: &mut Cx, layer: usize, visible: bool) {
        if let Some(l) = self.scene.layers.get_mut(layer) {
            if l.visible != visible {
                l.visible = visible;
                self.redraw(cx);
            }
        }
    }

    /// Show every layer that the file itself did not switch off.
    pub fn show_all_layers(&mut self, cx: &mut Cx) {
        for l in &mut self.scene.layers {
            l.visible = true;
        }
        self.redraw(cx);
    }

    pub fn camera(&self) -> &Camera {
        &self.cam
    }

    pub fn style_mut(&mut self) -> &mut Style {
        &mut self.style
    }

    pub fn style(&self) -> &Style {
        &self.style
    }

    /// Frame the visible geometry. Deferred until a draw has given us a real viewport.
    pub fn zoom_to_fit(&mut self, cx: &mut Cx) {
        let b = self.scene.visible_bounds();
        self.cam.fit(&b);
        self.pending_fit = false;
        self.redraw(cx);
    }

    pub fn zoom_by(&mut self, cx: &mut Cx, factor: f64) {
        let anchor = self.cam.view.center();
        self.cam.zoom_at(anchor, factor);
        self.mark_moving(cx);
    }

    pub fn pan_by(&mut self, cx: &mut Cx, delta: V2) {
        self.cam.pan_screen(delta);
        self.mark_moving(cx);
    }

    /// Note that the view is in motion and schedule a full-detail redraw once it settles.
    fn mark_moving(&mut self, cx: &mut Cx) {
        self.moving = true;
        cx.stop_timer(self.settle);
        self.settle = cx.start_timeout(SETTLE_SECONDS);
        self.redraw(cx);
    }

    fn emit_view_changed(&self, cx: &mut Cx, scope: &mut Scope) {
        cx.widget_action(self.widget_uid(), &scope.path, DxfCanvasAction::ViewChanged);
    }
}

impl Widget for DxfCanvas {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // Consume the raw wheel event before anything else can scroll on it. `Hit::FingerScroll`
        // carries no handled flags, so an enclosing scrolling view would otherwise react too.
        let mut wheel = None;
        if let Event::Scroll(e) = event {
            if self.area.rect(cx).contains(e.abs) {
                wheel = Some((dv(e.abs), dv(e.scroll), e.is_mouse));
                e.handled_x.set(true);
                e.handled_y.set(true);
            }
        }
        if let Some((abs, scroll, is_mouse)) = wheel {
            // macOS multiplies wheel deltas by 32 while trackpad deltas stay raw, so the two need
            // different sensitivities. An exponential keeps zooming in and out symmetric.
            let per_unit = if is_mouse { 1.0 / 400.0 } else { 1.0 / 250.0 };
            let factor = (-scroll.y * per_unit).clamp(-2.0, 2.0).exp();
            self.cam.zoom_at(abs, factor);
            self.mark_moving(cx);
            self.emit_view_changed(cx, scope);
        }

        if self.settle.is_event(event).is_some() {
            self.settle = Timer::empty();
            if self.moving {
                self.moving = false;
                self.redraw(cx);
            }
        }

        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(_) => cx.set_cursor(MouseCursor::Crosshair),
            Hit::FingerHoverOver(e) => {
                let w = self.cam.screen_to_world(dv(e.abs));
                if w != self.last_hover {
                    self.last_hover = w;
                    cx.widget_action(self.widget_uid(), &scope.path, DxfCanvasAction::Hover(w));
                }
            }
            Hit::FingerDown(e) => {
                // Without key focus, Hit::KeyDown never arrives and the shortcuts silently do
                // nothing.
                cx.set_key_focus(self.area);
                self.pan_from = Some(self.cam.center);
                cx.set_cursor(MouseCursor::Grabbing);
                let _ = e;
            }
            Hit::FingerMove(e) => {
                if let Some(from) = self.pan_from {
                    // `abs_start` is where the press landed, so this delta is cumulative. Panning
                    // from a snapshot avoids the drift an incremental sum would accumulate.
                    let d = dv(e.abs) - dv(e.abs_start);
                    self.cam.center = from - v2(d.x, -d.y) / self.cam.scale;
                    self.mark_moving(cx);
                    self.emit_view_changed(cx, scope);
                }
            }
            Hit::FingerUp(_) => {
                self.pan_from = None;
                cx.set_cursor(MouseCursor::Crosshair);
            }
            Hit::KeyDown(e) => {
                let vs = self.cam.view.size();
                let step = vs.x.min(vs.y) * KEY_PAN_FRACTION;
                let handled = match e.key_code {
                    KeyCode::ArrowLeft => {
                        self.pan_by(cx, v2(step, 0.0));
                        true
                    }
                    KeyCode::ArrowRight => {
                        self.pan_by(cx, v2(-step, 0.0));
                        true
                    }
                    KeyCode::ArrowUp => {
                        self.pan_by(cx, v2(0.0, step));
                        true
                    }
                    KeyCode::ArrowDown => {
                        self.pan_by(cx, v2(0.0, -step));
                        true
                    }
                    // `Equals` is the unshifted `+`, which is where users reach for zoom in.
                    KeyCode::Equals | KeyCode::NumpadAdd => {
                        self.zoom_by(cx, KEY_ZOOM_STEP);
                        true
                    }
                    KeyCode::Minus | KeyCode::NumpadSubtract => {
                        self.zoom_by(cx, 1.0 / KEY_ZOOM_STEP);
                        true
                    }
                    KeyCode::KeyF | KeyCode::Home => {
                        self.zoom_to_fit(cx);
                        true
                    }
                    _ => false,
                };
                if handled {
                    self.emit_view_changed(cx, scope);
                }
            }
            _ => (),
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle_with_area(&mut self.area, walk);
        self.cam.set_view(Aabb::new(dv(rect.pos), dv(rect.pos + rect.size)));

        // Zoom-to-fit needs a viewport, and layout only produces one inside the draw pass — so
        // a fit requested from anywhere else is deferred to here.
        if (self.pending_fit || !self.fitted) && rect.size.x > 1.0 && rect.size.y > 1.0 {
            let b = self.scene.visible_bounds();
            if !b.is_empty() {
                self.cam.fit(&b);
                self.fitted = true;
            }
            self.pending_fit = false;
        }

        self.draw_bg.draw_abs(cx, rect);

        let style = if self.moving { self.style.preview() } else { self.style };
        // Move the scene aside so the batch can borrow it while `self` is borrowed mutably. This
        // is a move of a handful of Vec headers, not a copy of the geometry.
        let scene = std::mem::take(&mut self.scene);
        render::build(&scene, &self.cam, &style, &mut self.batch);
        self.scene = scene;
        self.last_seg_count = self.batch.segs.len();

        let clip = vec4(
            rect.pos.x as f32,
            rect.pos.y as f32,
            (rect.pos.x + rect.size.x) as f32,
            (rect.pos.y + rect.size.y) as f32,
        );

        // Filled areas first so strokes read on top of them.
        if !self.batch.tris.is_empty() {
            self.draw_tri.draw_super.draw_clip = clip;
            if let Some(mut mi) = cx.begin_many_instances(&self.draw_tri.draw_super.draw_vars) {
                mi.instances.reserve(self.batch.tris.len() * TRI_SLOTS);
                for t in &self.batch.tris {
                    self.draw_tri.pa = fv(t.a);
                    self.draw_tri.pb = fv(t.b);
                    self.draw_tri.pc = fv(t.c);
                    self.draw_tri.color = rgba(t.color);
                    mi.instances.extend_from_slice(self.draw_tri.draw_super.draw_vars.as_slice());
                }
                let area = cx.end_many_instances(mi);
                self.draw_tri.draw_super.draw_vars.area =
                    cx.update_area_refs(self.draw_tri.draw_super.draw_vars.area, area);
            }
        }

        if !self.batch.segs.is_empty() {
            self.draw_seg.draw_super.draw_clip = clip;
            // The plain `cx.begin_many_instances`, not DrawQuad's aligned variant: the aligned one
            // registers an align entry, and every enclosing end_turtle then rewrites four floats
            // per instance. At half a million segments that scatter dominates the frame.
            if let Some(mut mi) = cx.begin_many_instances(&self.draw_seg.draw_super.draw_vars) {
                mi.instances.reserve(self.batch.segs.len() * SEG_SLOTS);
                for s in &self.batch.segs {
                    push_seg(&mut self.draw_seg, &mut mi.instances, s);
                }
                let area = cx.end_many_instances(mi);
                self.draw_seg.draw_super.draw_vars.area =
                    cx.update_area_refs(self.draw_seg.draw_super.draw_vars.area, area);
            }
        }

        for run in &self.batch.runs {
            // Makepad's text pipeline has no rotation, so rotated DXF text is drawn upright at the
            // right place rather than skipped. Sizing goes through a measure pass because font size
            // is in points and DXF text height is in drawing units.
            let base = self.draw_text.text_style.font_size.max(1.0);
            self.draw_text.font_scale = 1.0;
            let laid =
                self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), &run.text);
            let h = laid.size_in_lpxs.height.max(1.0);
            let scale = (run.height as f32 / h) * CAP_HEIGHT_RATIO;
            self.draw_text.font_scale = scale;
            let w = (laid.size_in_lpxs.width * scale) as f64;
            let th = (h * scale) as f64;
            let x = match run.halign {
                HAlign::Left => run.pos.x,
                HAlign::Center => run.pos.x - w * 0.5,
                HAlign::Right => run.pos.x - w,
            };
            // `draw_abs` positions the top of the run; DXF anchors are relative to the baseline.
            let y = match run.valign {
                VAlign::Baseline | VAlign::Bottom => run.pos.y - th * BASELINE_RATIO,
                VAlign::Middle => run.pos.y - th * 0.5,
                VAlign::Top => run.pos.y,
            };
            self.draw_text.color = rgba(run.color);
            self.draw_text.draw_abs(cx, dvec2(x, y), &run.text);
            let _ = base;
        }

        DrawStep::done()
    }
}

/// Instance slots per segment: DrawQuad's ten, plus half_width and colour.
const SEG_SLOTS: usize = 15;
/// Instance slots per triangle: DrawQuad's ten, plus three points and a colour.
const TRI_SLOTS: usize = 20;
/// A font's reported line height is taller than its cap height; DXF text height is the cap height.
const CAP_HEIGHT_RATIO: f32 = 1.32;
/// Where the baseline sits within the laid-out run height, measured from the top.
const BASELINE_RATIO: f64 = 0.79;

#[inline]
fn push_seg(d: &mut DrawSeg, out: &mut Vec<f32>, s: &Seg) {
    // The vertex shader reads rect_pos as the segment start and rect_size as its delta.
    d.draw_super.rect_pos = fv(s.a);
    d.draw_super.rect_size = fv(s.b - s.a);
    d.half_width = s.half_w;
    d.color = rgba(s.color);
    out.extend_from_slice(d.draw_super.draw_vars.as_slice());
}

#[inline]
fn fv(p: V2) -> Vec2 {
    vec2(p.x as f32, p.y as f32)
}

#[inline]
fn dv(p: DVec2) -> V2 {
    v2(p.x, p.y)
}

#[inline]
fn rgba(c: Rgb) -> Vec4 {
    vec4(c.0 as f32 / 255.0, c.1 as f32 / 255.0, c.2 as f32 / 255.0, 1.0)
}
