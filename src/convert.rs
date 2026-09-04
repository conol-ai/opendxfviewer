//! Flattening a parsed [`dxf::Drawing`] into a render-ready [`Scene`].
//!
//! This is where DXF's indirections get resolved, once, so the renderer never has to:
//!
//! * **OCS → WCS.** Most planar entities store coordinates in an "object coordinate system"
//!   defined by an extrusion vector. The arbitrary axis algorithm turns that into a basis.
//! * **Blocks.** INSERT references are expanded in place with their full transform, including
//!   nested inserts, mirrored and non-uniform scales, and MINSERT arrays.
//! * **Colour and layer inheritance.** `ByLayer`, `ByBlock`, and the rule that entities drawn on
//!   layer `0` inside a block adopt the layer of the INSERT that placed them.
//! * **Curves.** Arcs, bulges, ellipses and splines all become polylines.
//!
//! Anything that cannot be represented is counted in [`Scene::stats`] rather than dropped
//! silently — a viewer that quietly omits geometry is worse than one that says what it missed.

use std::collections::{BTreeMap, HashMap, HashSet};

use dxf::entities::{Entity, EntityType};
use dxf::enums::{HorizontalTextJustification, VerticalTextJustification};
use dxf::{Block, Drawing, Point, Vector};

use crate::aci;
use crate::geom::{v2, v3, Aabb, Xform, V2, V3};
use crate::scene::{
    CurveSource, Dot, HAlign, Layer, Linetype, Poly, Rgb, Scene, Spline, Text, Tri, Units, VAlign,
};
use crate::tessellate::{self as tess, Tol};

/// Knobs for the conversion.
#[derive(Copy, Clone, Debug)]
pub struct Options {
    /// Chooses whether ACI 7 resolves to white or black.
    pub dark_background: bool,
    /// Chord tolerance as a fraction of each curve's own size. Relative rather than absolute so a
    /// 5 mm fillet and a 500 m arc are both smooth without a prepass over the drawing extent.
    ///
    /// This only sets the *baseline*: the renderer re-tessellates a curve when the view zooms past
    /// what the stored vertices can carry, so the baseline is chosen for memory, not for fidelity.
    /// 4e-3 puts about 35 vertices on a full circle, which is smooth at 1:1.
    pub curve_quality: f64,
    /// How deep nested INSERTs may go before we stop and warn.
    pub max_block_depth: u32,
    /// Total primitives after which conversion stops. Guards against a block-array bomb.
    pub max_primitives: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            dark_background: true,
            curve_quality: 4e-3,
            max_block_depth: 16,
            max_primitives: 4_000_000,
        }
    }
}

/// Convert a drawing. Never fails: unreadable geometry is reported through [`Scene::stats`].
pub fn convert(drawing: &Drawing, opts: &Options) -> Scene {
    let mut c = Ctx::new(drawing, *opts);
    c.run();
    c.finish()
}

// ---------------------------------------------------------------------------------------------

/// What an entity inherits from the INSERT chain that placed it.
#[derive(Clone, Copy)]
struct Inherit {
    xform: Xform,
    /// Layer to use for entities drawn on layer `0` inside a block.
    layer: u16,
    /// Colour to use for entities whose colour is `ByBlock`.
    color: Rgb,
    depth: u32,
}

impl Inherit {
    fn root(default_layer: u16, fg: Rgb) -> Inherit {
        Inherit { xform: Xform::IDENTITY, layer: default_layer, color: fg, depth: 0 }
    }
}

struct Ctx<'a> {
    dr: &'a Drawing,
    opts: Options,
    blocks: HashMap<String, &'a Block>,
    /// Keyed by upper-cased name: DXF table names are case-insensitive, so an entity on "Walls"
    /// and one on "WALLS" belong to the same layer.
    layer_ids: HashMap<String, u16>,
    linetype_ids: HashMap<String, u16>,
    /// Blocks currently being expanded, so a self-referential drawing cannot loop forever.
    active: HashSet<String>,
    scene: Scene,
    unsupported: BTreeMap<&'static str, usize>,
    warned_missing: HashSet<String>,
    truncated: bool,
}

impl<'a> Ctx<'a> {
    fn new(dr: &'a Drawing, opts: Options) -> Ctx<'a> {
        let ltscale = dr.header.line_type_scale;
        let mut scene = Scene {
            units: units_of(dr),
            // A zero or nonsensical $LTSCALE would collapse every dash to nothing.
            linetype_scale: if ltscale.is_finite() && ltscale > 0.0 { ltscale } else { 1.0 },
            ..Scene::default()
        };

        // Linetypes first: layers reference them by name.
        let mut linetype_ids = HashMap::new();
        scene.linetypes.push(Linetype { name: "CONTINUOUS".into(), ..Linetype::default() });
        linetype_ids.insert("CONTINUOUS".to_string(), 0u16);
        for lt in dr.line_types() {
            let key = lt.name.to_uppercase();
            if linetype_ids.contains_key(&key) {
                continue;
            }
            let pattern: Vec<f64> =
                lt.dash_dot_space_lengths.iter().copied().filter(|v| v.is_finite()).collect();
            let total = if lt.total_pattern_length.is_finite() && lt.total_pattern_length > 0.0 {
                lt.total_pattern_length
            } else {
                pattern.iter().map(|v| v.abs()).sum()
            };
            let id = scene.linetypes.len() as u16;
            scene.linetypes.push(Linetype { name: lt.name.clone(), pattern, total });
            linetype_ids.insert(key, id);
        }

        let fg = if opts.dark_background { Rgb::WHITE } else { Rgb::BLACK };
        let mut layer_ids = HashMap::new();
        // Layer 0 always exists, whether or not the file bothered to define it.
        scene.layers.push(Layer {
            name: "0".into(),
            color: fg,
            visible_in_file: true,
            visible: true,
            lineweight: None,
            linetype: 0,
            count: 0,
        });
        layer_ids.insert("0".to_string(), 0u16);
        for l in dr.layers() {
            let color = match l.color.index() {
                Some(i) => aci::rgb(i as i16, opts.dark_background),
                // A layer whose colour index is negated is switched off; the index itself is not
                // recoverable through this crate's API, so fall back to the foreground.
                None => fg,
            };
            let lineweight = lw(l.line_weight.raw_value());
            let linetype = *linetype_ids.get(&l.line_type_name.to_uppercase()).unwrap_or(&0);
            let key = l.name.to_uppercase();
            if let Some(&id) = layer_ids.get(&key) {
                let e = &mut scene.layers[id as usize];
                e.color = color;
                e.visible_in_file = l.is_layer_on;
                e.lineweight = lineweight;
                e.linetype = linetype;
                continue;
            }
            let id = scene.layers.len() as u16;
            scene.layers.push(Layer {
                name: l.name.clone(),
                color,
                visible_in_file: l.is_layer_on,
                visible: true,
                lineweight,
                linetype,
                count: 0,
            });
            layer_ids.insert(key, id);
        }

        Ctx {
            dr,
            opts,
            blocks: dr.blocks().map(|b| (b.name.to_uppercase(), b)).collect(),
            layer_ids,
            linetype_ids,
            active: HashSet::new(),
            scene,
            unsupported: BTreeMap::new(),
            warned_missing: HashSet::new(),
            truncated: false,
        }
    }

    fn run(&mut self) {
        let fg = if self.opts.dark_background { Rgb::WHITE } else { Rgb::BLACK };
        let root = Inherit::root(0, fg);
        // `entities()` borrows the drawing, which we also need mutably through `self`; collect the
        // references first. They are pointers, so this is cheap relative to parsing.
        let ents: Vec<&Entity> = self.dr.entities().collect();
        self.scene.stats.entities_read = ents.len();
        for e in ents {
            let before = self.scene.stats.primitives;
            self.emit(e, &root);
            if self.scene.stats.primitives == before {
                self.scene.stats.entities_skipped += 1;
            }
        }
    }

    fn finish(mut self) -> Scene {
        self.scene.stats.unsupported =
            self.unsupported.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        if self.truncated {
            self.scene.stats.warnings.push(format!(
                "Stopped after {} primitives; the drawing is larger than the viewer's limit.",
                self.opts.max_primitives
            ));
        }
        let mut b = Aabb::EMPTY;
        for p in &self.scene.polys {
            if !p.unbounded {
                b = b.union(&p.bbox);
            }
        }
        for d in &self.scene.dots {
            b.add_point(d.pos);
        }
        for t in &self.scene.tris {
            b = b.union(&t.bbox);
        }
        for t in &self.scene.texts {
            b = b.union(&t.bbox);
        }
        self.scene.bounds = b;
        self.scene.build_index();
        self.scene
    }

    // -- resolution helpers ---------------------------------------------------------------

    fn layer_of(&mut self, e: &'a Entity, inh: &Inherit) -> u16 {
        let name = e.common.layer.as_str();
        // Inside a block, layer "0" means "whatever layer the INSERT was on".
        if inh.depth > 0 && name == "0" {
            return inh.layer;
        }
        match self.layer_ids.get(&name.to_uppercase()) {
            Some(&id) => id,
            // A file may reference a layer it never defined in the table. Record it, so a
            // drawing with a thousand entities on one undefined layer produces one layer and not
            // a thousand — which is why `emit` takes `&'a Entity` rather than a short borrow.
            None => {
                let fg = if self.opts.dark_background { Rgb::WHITE } else { Rgb::BLACK };
                let id = self.scene.layers.len() as u16;
                self.scene.layers.push(Layer {
                    name: name.to_string(),
                    color: fg,
                    visible_in_file: true,
                    visible: true,
                    lineweight: None,
                    linetype: 0,
                    count: 0,
                });
                self.layer_ids.insert(name.to_uppercase(), id);
                id
            }
        }
    }

    fn color_of(&self, e: &Entity, inh: &Inherit, layer: u16) -> Rgb {
        // A 24-bit true colour, when present, wins over the palette index.
        let tc = e.common.color_24_bit;
        if tc != 0 {
            return Rgb((tc >> 16) as u8, (tc >> 8) as u8, tc as u8);
        }
        let c = &e.common.color;
        if c.is_by_block() {
            return inh.color;
        }
        if let Some(i) = c.index() {
            return aci::rgb(i as i16, self.opts.dark_background);
        }
        // ByLayer, ByEntity, or a switched-off marker: take the layer's colour.
        self.scene.layers.get(layer as usize).map(|l| l.color).unwrap_or(inh.color)
    }

    fn linetype_of(&self, e: &Entity, layer: u16) -> u16 {
        let n = e.common.line_type_name.as_str();
        if n.is_empty() || n.eq_ignore_ascii_case("BYLAYER") || n.eq_ignore_ascii_case("BYBLOCK") {
            return self.scene.layers.get(layer as usize).map(|l| l.linetype).unwrap_or(0);
        }
        *self.linetype_ids.get(&n.to_uppercase()).unwrap_or(&0)
    }

    fn style(&mut self, e: &'a Entity, inh: &Inherit) -> Style {
        let layer = self.layer_of(e, inh);
        let scale = e.common.line_type_scale;
        Style {
            layer,
            color: self.color_of(e, inh, layer),
            lineweight: lw(e.common.lineweight_enum_value),
            linetype: self.linetype_of(e, layer),
            linetype_scale: if scale.is_finite() && scale > 0.0 { scale as f32 } else { 1.0 },
        }
    }

    fn full(&self) -> bool {
        self.scene.polys.len() + self.scene.tris.len() + self.scene.dots.len()
            >= self.opts.max_primitives
    }

    // -- emitters -------------------------------------------------------------------------

    fn push_poly(&mut self, mut pts: Vec<V2>, closed: bool, st: Style, source: CurveSource) {
        tess::cleanup(&mut pts, closed);
        if pts.len() < 2 {
            // A one-point polyline is still something the user drew; show it as a dot.
            if let Some(&p) = pts.first() {
                self.push_dot(p, st);
            }
            return;
        }
        let bbox = tess::bounds(&pts);
        if bbox.is_empty() {
            return;
        }
        let start = self.scene.verts.len() as u32;
        self.scene.verts.extend_from_slice(&pts);
        self.scene.polys.push(Poly {
            start,
            len: pts.len() as u32,
            layer: st.layer,
            color: st.color,
            lineweight: st.lineweight,
            linetype: st.linetype,
            linetype_scale: st.linetype_scale,
            closed,
            unbounded: false,
            bbox,
            source,
        });
        self.bump(st.layer);
    }

    /// A construction line: drawn, but kept out of the drawing's extent.
    fn push_unbounded(&mut self, pts: Vec<V2>, st: Style) {
        let before = self.scene.polys.len();
        self.push_poly(pts, false, st, CurveSource::None);
        if let Some(p) = self.scene.polys.get_mut(before) {
            p.unbounded = true;
        }
    }

    fn push_dot(&mut self, p: V2, st: Style) {
        if !p.is_finite() {
            return;
        }
        self.scene.dots.push(Dot { pos: p, layer: st.layer, color: st.color });
        self.bump(st.layer);
    }

    fn push_tri(&mut self, a: V2, b: V2, c: V2, st: Style) {
        if !(a.is_finite() && b.is_finite() && c.is_finite()) {
            return;
        }
        // Degenerate triangles contribute nothing but cost a draw slot.
        if (b - a).cross(c - a).abs() <= 0.0 {
            return;
        }
        let bbox = [a, b, c].into_iter().collect();
        self.scene.tris.push(Tri { a, b, c, layer: st.layer, color: st.color, bbox });
        self.bump(st.layer);
    }

    fn bump(&mut self, layer: u16) {
        if let Some(l) = self.scene.layers.get_mut(layer as usize) {
            l.count += 1;
        }
        self.scene.stats.primitives += 1;
    }

    fn warn(&mut self, msg: String) {
        if self.scene.stats.warnings.len() < 64 && self.warned_missing.insert(msg.clone()) {
            self.scene.stats.warnings.push(msg);
        }
    }

    fn emit(&mut self, e: &'a Entity, inh: &Inherit) {
        if self.full() {
            self.truncated = true;
            return;
        }
        if !e.common.is_visible {
            return;
        }
        let st = self.style(e, inh);
        let q = self.opts.curve_quality;

        match &e.specific {
            EntityType::Line(l) => {
                let a = inh.xform.apply(pt(&l.p1));
                let b = inh.xform.apply(pt(&l.p2));
                self.push_poly(vec![a, b], false, st, CurveSource::None);
            }

            EntityType::Circle(c) => {
                let x = ocs(&c.normal, e.common.elevation + c.center.z).then(&inh.xform);
                self.arc_like(pt(&c.center), c.radius, 0.0, std::f64::consts::TAU, &x, true, st, q);
            }

            EntityType::Arc(a) => {
                let x = ocs(&a.normal, e.common.elevation + a.center.z).then(&inh.xform);
                let (s, sweep) = tess::arc_sweep_from_degrees(a.start_angle, a.end_angle);
                self.arc_like(pt(&a.center), a.radius, s, sweep, &x, false, st, q);
            }

            EntityType::Ellipse(el) => {
                // ELLIPSE stores its centre and major axis in WCS, unlike ARC and CIRCLE.
                let n = vec3(&el.normal).norm();
                let maj3 = vec3(&el.major_axis);
                // The minor axis is the major rotated 90 degrees about the extrusion normal, so a
                // -Z extrusion traces the arc the other way round, as it should.
                let min3 = n.cross(maj3) * el.minor_axis_ratio.abs();
                let center = inh.xform.apply(pt3(&el.center));
                let u = inh.xform.apply_dir(maj3.xy());
                let v = inh.xform.apply_dir(min3.xy());
                let (s, sweep) = tess::ellipse_sweep(el.start_parameter, el.end_parameter);
                self.ellipse_like(center, u, v, s, sweep, st, q, closed_sweep(sweep));
            }

            EntityType::LwPolyline(p) => {
                let x = ocs(&p.extrusion_direction, e.common.elevation).then(&inh.xform);
                let pts: Vec<(V2, f64)> = p
                    .vertices
                    .iter()
                    .map(|v| (x.apply(v2(v.x, v.y)), bulge_for(&x, v.bulge)))
                    .collect();
                let closed = p.is_closed();
                let mut out = Vec::with_capacity(pts.len() * 2);
                tess::flatten_bulge_poly(&pts, closed, tol_for(&pts, q), &mut out);
                self.push_poly(out, closed, st, CurveSource::None);
            }

            EntityType::Polyline(p) => self.polyline(p, e, inh, st, q),

            EntityType::Spline(s) => self.spline(s, inh, st, q),

            // POINT carries an extrusion vector, but only to orient the marker glyph: its
            // location is in WCS, not in the plane the extrusion defines.
            EntityType::ModelPoint(p) => self.push_dot(inh.xform.apply(pt3(&p.location)), st),

            EntityType::Insert(i) => self.insert(i, e, inh, st),

            EntityType::Solid(s) => {
                let x = ocs(&s.extrusion_direction, e.common.elevation).then(&inh.xform);
                // SOLID and TRACE store their last two corners in the opposite order to the one
                // they are drawn in, so the quad is 1-2-4-3, not 1-2-3-4.
                self.quad(
                    x.apply(pt(&s.first_corner)),
                    x.apply(pt(&s.second_corner)),
                    x.apply(pt(&s.fourth_corner)),
                    x.apply(pt(&s.third_corner)),
                    st,
                );
            }

            EntityType::Trace(s) => {
                let x = ocs(&s.extrusion_direction, e.common.elevation).then(&inh.xform);
                self.quad(
                    x.apply(pt(&s.first_corner)),
                    x.apply(pt(&s.second_corner)),
                    x.apply(pt(&s.fourth_corner)),
                    x.apply(pt(&s.third_corner)),
                    st,
                );
            }

            EntityType::Face3D(f) => {
                let x = &inh.xform;
                self.quad(
                    x.apply(pt3(&f.first_corner)),
                    x.apply(pt3(&f.second_corner)),
                    x.apply(pt3(&f.third_corner)),
                    x.apply(pt3(&f.fourth_corner)),
                    st,
                );
            }

            EntityType::Text(t) => {
                let x = ocs(&t.normal, e.common.elevation).then(&inh.xform);
                self.text_entity(&TextRun::from_text(t), &x, st);
            }

            EntityType::Attribute(a) => {
                if a.is_invisible() {
                    return;
                }
                let x = ocs(&a.normal, e.common.elevation).then(&inh.xform);
                self.text_entity(&TextRun::from_attribute(a), &x, st);
            }

            // An ATTDEF is a placeholder in a block definition; drawing its prompt text in model
            // space would show template text AutoCAD does not display.
            EntityType::AttributeDefinition(_) => {}

            EntityType::MText(m) => self.mtext(m, inh, st),

            EntityType::Leader(l) => {
                let pts: Vec<V2> = l.vertices.iter().map(|p| inh.xform.apply(pt3(p))).collect();
                self.push_poly(pts, false, st, CurveSource::None);
            }

            EntityType::MLine(m) => {
                // Without the MLINESTYLE table we cannot offset the individual elements; the
                // centreline is the honest approximation.
                let mut pts: Vec<V2> = m.vertices.iter().map(|p| inh.xform.apply(pt3(p))).collect();
                if pts.is_empty() {
                    pts.push(inh.xform.apply(pt3(&m.start_point)));
                }
                self.push_poly(pts, m.flags & 2 != 0, st, CurveSource::None);
                self.note_partial("MLINE (drawn as its centreline)");
            }

            // RAY and XLINE are unbounded. Clip them to a generous multiple of the drawing so they
            // read as construction lines without dominating the extent used for zoom-to-fit.
            EntityType::Ray(r) => {
                let o = inh.xform.apply(pt3(&r.start_point));
                let d = inh.xform.apply_dir(vec3(&r.unit_direction_vector).xy()).norm();
                self.push_unbounded(vec![o, o + d * RAY_LENGTH], st);
            }
            EntityType::XLine(r) => {
                let o = inh.xform.apply(pt3(&r.first_point));
                let d = inh.xform.apply_dir(vec3(&r.unit_direction_vector).xy()).norm();
                self.push_unbounded(vec![o - d * RAY_LENGTH, o + d * RAY_LENGTH], st);
            }

            // Dimensions carry a pre-rendered anonymous block holding the lines, arrows and text
            // the authoring program produced. Drawing that block is both simpler and more faithful
            // than re-deriving the dimension geometry ourselves.
            EntityType::RotatedDimension(d) => {
                self.dimension(&d.dimension_base.block_name, inh, st)
            }
            EntityType::RadialDimension(d) => self.dimension(&d.dimension_base.block_name, inh, st),
            EntityType::DiameterDimension(d) => {
                self.dimension(&d.dimension_base.block_name, inh, st)
            }
            EntityType::AngularThreePointDimension(d) => {
                self.dimension(&d.dimension_base.block_name, inh, st)
            }
            EntityType::OrdinateDimension(d) => {
                self.dimension(&d.dimension_base.block_name, inh, st)
            }

            EntityType::Seqend(_) | EntityType::Vertex(_) => {} // consumed by their owner

            other => {
                *self.unsupported.entry(type_name(other)).or_insert(0) += 1;
            }
        }
    }

    fn note_partial(&mut self, what: &'static str) {
        *self.unsupported.entry(what).or_insert(0) += 1;
    }

    /// A circle or circular arc living in some object coordinate system.
    ///
    /// Under an arbitrary affine map a circle becomes an ellipse, so both cases go through the same
    /// conjugate-diameter parameterisation; only the conformal case can keep an `Arc` source for
    /// later refinement.
    #[allow(clippy::too_many_arguments)]
    fn arc_like(
        &mut self,
        center: V2,
        radius: f64,
        start: f64,
        sweep: f64,
        x: &Xform,
        closed: bool,
        st: Style,
        q: f64,
    ) {
        if !radius.is_finite() || radius <= 0.0 {
            return;
        }
        let c = x.apply(center);
        let u = x.apply_dir(v2(radius, 0.0));
        let v = x.apply_dir(v2(0.0, radius));
        if x.is_conformal() {
            let r = u.len();
            // The image of the point at angle t is `c + u*cos t + v*sin t`. For a rotation that
            // is angle `rot + t`; for a mirror it is `rot - t`, with the sweep reversed to match.
            // Getting this backwards puts a mirrored arc on the opposite side of its circle.
            let rot = u.angle();
            let (s2, w2) =
                if x.is_mirrored() { (rot - start, -sweep) } else { (rot + start, sweep) };
            let mut out = Vec::new();
            tess::flatten_arc(c, r, s2, w2, Tol::new(r * q), &mut out);
            self.push_poly(
                out,
                closed,
                st,
                CurveSource::Arc { center: c, radius: r, start: s2, sweep: w2 },
            );
        } else {
            self.ellipse_like(c, u, v, start, sweep, st, q, closed);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ellipse_like(
        &mut self,
        center: V2,
        u: V2,
        v: V2,
        start: f64,
        sweep: f64,
        st: Style,
        q: f64,
        closed: bool,
    ) {
        let r = u.len().max(v.len());
        if !r.is_finite() || r <= 0.0 {
            return;
        }
        let tol = Tol::new(r * q);
        let mut out = Vec::new();
        tess::flatten_ellipse(center, u, v, start, sweep, tol, &mut out);
        self.push_poly(
            out,
            closed,
            st,
            CurveSource::Ellipse { center, major: u, minor: v, start, sweep },
        );
    }

    fn quad(&mut self, a: V2, b: V2, c: V2, d: V2, st: Style) {
        self.push_tri(a, b, c, st);
        // A three-cornered SOLID/3DFACE repeats its last corner; the second triangle is degenerate
        // and push_tri drops it.
        self.push_tri(a, c, d, st);
    }

    fn polyline(
        &mut self,
        p: &dxf::entities::Polyline,
        e: &Entity,
        inh: &Inherit,
        st: Style,
        q: f64,
    ) {
        if p.is_polyface_mesh() || p.is_3d_polygon_mesh() {
            // Meshes need face indices we do not reconstruct; draw the vertex ring so the shape is
            // at least visible, and say so.
            let pts: Vec<V2> = p.vertices().map(|v| inh.xform.apply(pt3(&v.location))).collect();
            self.push_poly(pts, false, st, CurveSource::None);
            self.note_partial("POLYLINE mesh (drawn as a vertex ring)");
            return;
        }
        if p.is_3d_polyline() {
            let pts: Vec<V2> = p.vertices().map(|v| inh.xform.apply(pt3(&v.location))).collect();
            self.push_poly(pts, p.is_closed(), st, CurveSource::None);
            return;
        }
        // A 2D polyline is in OCS, and its vertices carry bulges.
        let x = ocs(&p.normal, e.common.elevation).then(&inh.xform);
        let pts: Vec<(V2, f64)> = p
            .vertices()
            .map(|v| (x.apply(v2(v.location.x, v.location.y)), bulge_for(&x, v.bulge)))
            .collect();
        let closed = p.is_closed();
        let mut out = Vec::with_capacity(pts.len() * 2);
        tess::flatten_bulge_poly(&pts, closed, tol_for(&pts, q), &mut out);
        self.push_poly(out, closed, st, CurveSource::None);
    }

    fn spline(&mut self, s: &dxf::entities::Spline, inh: &Inherit, st: Style, q: f64) {
        let closed = s.flags & 1 != 0;
        let ctrl: Vec<V2> = s.control_points.iter().map(|p| inh.xform.apply(pt3(p))).collect();
        let fit: Vec<V2> = s.fit_points.iter().map(|p| inh.xform.apply(pt3(p))).collect();
        let extent = tess::bounds(if ctrl.is_empty() { &fit } else { &ctrl });
        let size = extent.size();
        let tol = Tol::new(size.x.max(size.y).max(f64::MIN_POSITIVE) * q).with_max(2048);

        let mut out = Vec::new();
        if ctrl.len() >= 2 {
            let degree = s.degree_of_curve.max(1) as usize;
            match tess::Nurbs::repair(
                degree,
                ctrl,
                s.knot_values.clone(),
                s.weight_values.clone(),
                closed,
            ) {
                Some(n) => {
                    tess::flatten_nurbs(&n, tol, &mut out);
                    let idx = self.scene.splines.len() as u32;
                    self.scene.splines.push(Spline {
                        degree: n.degree,
                        ctrl: n.ctrl,
                        knots: n.knots,
                        weights: n.weights,
                        closed,
                    });
                    self.push_poly(out, closed, st, CurveSource::Spline { index: idx });
                    return;
                }
                None => self.warn("A SPLINE had too few control points to draw.".into()),
            }
        } else if fit.len() >= 2 {
            // Fit points without control points: the authoring program's interpolation is not
            // recorded, so approximate it with a centripetal Catmull-Rom through the same points.
            tess::flatten_fit_points(&fit, closed, tol, &mut out);
            self.push_poly(out, closed, st, CurveSource::None);
            return;
        }
        if !out.is_empty() {
            self.push_poly(out, closed, st, CurveSource::None);
        }
    }

    fn dimension(&mut self, block_name: &str, inh: &Inherit, st: Style) {
        if block_name.is_empty() {
            self.note_partial("DIMENSION without a geometry block");
            return;
        }
        let Some(&block) = self.blocks.get(&block_name.to_uppercase()) else {
            self.note_partial("DIMENSION without a geometry block");
            return;
        };
        let child =
            Inherit { xform: inh.xform, layer: st.layer, color: st.color, depth: inh.depth + 1 };
        if child.depth > self.opts.max_block_depth {
            return;
        }
        for e in &block.entities {
            self.emit(e, &child);
        }
    }

    fn insert(&mut self, i: &'a dxf::entities::Insert, _e: &'a Entity, inh: &Inherit, st: Style) {
        if inh.depth >= self.opts.max_block_depth {
            self.warn(format!(
                "Stopped expanding blocks at {} levels deep (near \"{}\").",
                self.opts.max_block_depth, i.name
            ));
            return;
        }
        let Some(&block) = self.blocks.get(&i.name.to_uppercase()) else {
            self.warn(format!("Block \"{}\" is referenced but not defined in the file.", i.name));
            return;
        };
        // A block that contains itself, directly or through a chain, would expand forever.
        let name = block.name.to_uppercase();
        if !self.active.insert(name.clone()) {
            self.warn(format!("Block \"{}\" refers to itself; stopped expanding it.", i.name));
            return;
        }

        let n = vec3(&i.extrusion_direction);
        let basis = ocs(&i.extrusion_direction, 0.0);
        // The insertion point is itself in the INSERT's own OCS.
        let loc = ocs(&i.extrusion_direction, i.location.z).apply(v2(i.location.x, i.location.y));
        let _ = n;

        // A zero scale factor collapses the block to nothing; DXF writers emit it for degenerate
        // inserts, and treating it as 1 would draw geometry AutoCAD does not.
        let s = v2(i.x_scale_factor, i.y_scale_factor);
        if !s.is_finite() || s.x == 0.0 || s.y == 0.0 {
            self.active.remove(&name);
            return;
        }
        let rot = i.rotation.to_radians();
        let base = pt3(&block.base_point);

        let cols = i.column_count.max(1) as i32;
        let rows = i.row_count.max(1) as i32;
        // MINSERT lays its copies out along the insert's rotated axes, not the world axes.
        let step = Xform::rotate(rot);

        for row in 0..rows {
            for col in 0..cols {
                if self.full() {
                    self.truncated = true;
                    break;
                }
                let offset =
                    step.apply_dir(v2(col as f64 * i.column_spacing, row as f64 * i.row_spacing));
                let local = Xform::translate(-base)
                    .then(&Xform::scale(s))
                    .then(&Xform::rotate(rot))
                    .then(&Xform::translate(offset));
                let to_world = Xform { a: basis.a, b: basis.b, c: loc };
                let child = Inherit {
                    xform: local.then(&to_world).then(&inh.xform),
                    layer: st.layer,
                    color: st.color,
                    depth: inh.depth + 1,
                };
                for e in &block.entities {
                    self.emit(e, &child);
                }
            }
        }
        self.active.remove(&name);

        // The ATTRIBs written after an INSERT are folded into it by the parser and never appear
        // in the entity stream, so they have to be drawn from here or they are lost entirely —
        // and they carry the text that identifies the part. Their coordinates are already in
        // world space, so they use the parent frame rather than the block's.
        for a in i.attributes() {
            if a.is_invisible() || a.value.trim().is_empty() {
                continue;
            }
            let x = ocs(&a.normal, 0.0).then(&inh.xform);
            self.text_entity(&TextRun::from_attribute(a), &x, st);
        }
    }

    /// Draw one TEXT-shaped entity: TEXT, ATTRIB, and the attributes carried by an INSERT all
    /// share the same fields and the same placement rules.
    fn text_entity(&mut self, r: &TextRun<'_>, x: &Xform, st: Style) {
        let anchor = text_anchor(r.location, r.second, r.halign, r.valign);
        self.text(
            &decode_control_codes(r.value),
            x.apply(anchor),
            r.height,
            r.rotation.to_radians(),
            r.width_factor,
            r.oblique.to_radians(),
            halign(r.halign),
            valign_for(r.halign, r.valign),
            x,
            st,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        s: &str,
        pos: V2,
        height: f64,
        rotation: f64,
        width_factor: f64,
        oblique: f64,
        halign: HAlign,
        valign: VAlign,
        x: &Xform,
        st: Style,
    ) {
        let s = s.trim_end();
        if s.is_empty() || !pos.is_finite() {
            return;
        }
        // The transform carries the block's scale and rotation; text has to follow both.
        let scale = x.mean_scale();
        let height = height * scale;
        if !height.is_finite() || height <= 0.0 {
            return;
        }
        let rotation = rotation + x.apply_dir(v2(1.0, 0.0)).angle();
        let width_factor =
            if width_factor.is_finite() && width_factor > 0.0 { width_factor } else { 1.0 };
        let bbox = text_bbox(s, pos, height, rotation, width_factor, halign, valign);
        self.scene.texts.push(Text {
            text: s.to_string(),
            pos,
            height,
            rotation,
            width_factor,
            oblique: if oblique.is_finite() { oblique } else { 0.0 },
            halign,
            valign,
            layer: st.layer,
            color: st.color,
            bbox,
        });
        self.bump(st.layer);
    }

    fn mtext(&mut self, m: &dxf::entities::MText, inh: &Inherit, st: Style) {
        // Like ELLIPSE, MTEXT stores its insertion point in WCS despite having an extrusion.
        let x = inh.xform;
        // Long MTEXT is split across group 3 chunks with group 1 holding the tail.
        let mut raw = m.extended_text.join("");
        raw.push_str(&m.text);
        let lines = mtext_lines(&raw);
        if lines.is_empty() {
            return;
        }
        let h = m.initial_text_height;
        // MTEXT rotation comes either as an angle or as an explicit X axis direction.
        let xdir = vec3(&m.x_axis_direction).xy();
        let rot = if xdir.len() > 1e-12 { xdir.angle() } else { m.rotation_angle.to_radians() };
        let (ha, va) = attachment(m.attachment_point);
        let pos = x.apply(pt3(&m.insertion_point));

        // Lines run down the page from the attachment point, perpendicular to the baseline.
        let line_h = h * MTEXT_LINE_SPACING;
        let down = v2(rot.sin(), -rot.cos());
        let block_h = line_h * lines.len() as f64;
        let first_offset = match va {
            VAlign::Top => 0.0,
            VAlign::Middle => -block_h * 0.5,
            _ => -block_h,
        };
        for (i, line) in lines.iter().enumerate() {
            let o = first_offset + line_h * i as f64;
            self.text(line, pos + down * (o + h), h, rot, 1.0, 0.0, ha, VAlign::Baseline, &x, st);
        }
    }
}

// ---------------------------------------------------------------------------------------------

/// Where a RAY or XLINE is cut off, in drawing units.
const RAY_LENGTH: f64 = 1e7;
/// MTEXT default line spacing, as a multiple of the text height.
const MTEXT_LINE_SPACING: f64 = 1.6;
/// Rough advance width of a glyph relative to the cap height, used only for bounding boxes.
const GLYPH_ADVANCE: f64 = 0.6;

/// The fields TEXT, ATTRIB and an INSERT's attributes have in common.
struct TextRun<'a> {
    value: &'a str,
    location: &'a Point,
    second: &'a Point,
    height: f64,
    rotation: f64,
    width_factor: f64,
    oblique: f64,
    halign: HorizontalTextJustification,
    valign: VerticalTextJustification,
}

impl<'a> TextRun<'a> {
    fn from_text(t: &'a dxf::entities::Text) -> TextRun<'a> {
        TextRun {
            value: &t.value,
            location: &t.location,
            second: &t.second_alignment_point,
            height: t.text_height,
            rotation: t.rotation,
            width_factor: t.relative_x_scale_factor,
            oblique: t.oblique_angle,
            halign: t.horizontal_text_justification,
            valign: t.vertical_text_justification,
        }
    }

    fn from_attribute(a: &'a dxf::entities::Attribute) -> TextRun<'a> {
        TextRun {
            value: &a.value,
            location: &a.location,
            second: &a.second_alignment_point,
            height: a.text_height,
            rotation: a.rotation,
            width_factor: a.relative_x_scale_factor,
            oblique: a.oblique_angle,
            halign: a.horizontal_text_justification,
            valign: a.vertical_text_justification,
        }
    }
}

#[derive(Copy, Clone)]
struct Style {
    layer: u16,
    color: Rgb,
    lineweight: Option<i16>,
    linetype: u16,
    linetype_scale: f32,
}

fn pt(p: &Point) -> V2 {
    v2(p.x, p.y)
}
fn pt3(p: &Point) -> V2 {
    v2(p.x, p.y)
}
fn vec3(v: &Vector) -> V3 {
    v3(v.x, v.y, v.z)
}

/// A DXF lineweight in hundredths of a millimetre, or `None` when it is not specified.
///
/// -1 is ByBlock, -2 ByLayer, -3 the drawing default. Zero is nominally a real 0.00 mm weight,
/// but the parser defaults an absent group 370 to zero as well, so the two are indistinguishable
/// — and treating it as unset is what allows a layer's own weight to apply at all. A 0.00 mm line
/// renders as a hairline either way.
fn lw(raw: i16) -> Option<i16> {
    if raw > 0 {
        Some(raw)
    } else {
        None
    }
}

/// The arbitrary axis algorithm.
///
/// Given an extrusion direction, produce the object coordinate system's X and Y axes. The 1/64
/// threshold picks a reference axis that cannot be parallel to the normal, which is what stops the
/// cross product collapsing for extrusions along Z.
fn ocs_basis(n: V3) -> (V3, V3) {
    let n = n.norm();
    let reference = if n.x.abs() < 1.0 / 64.0 && n.y.abs() < 1.0 / 64.0 {
        v3(0.0, 1.0, 0.0)
    } else {
        v3(0.0, 0.0, 1.0)
    };
    let ax = reference.cross(n).norm();
    let ay = n.cross(ax).norm();
    (ax, ay)
}

/// The 2D transform taking object coordinates, at a given elevation, into the top-down world view.
///
/// The view is an orthographic projection down the world Z axis, so this drops the Z component of
/// each basis vector. That is deliberate: a circle drawn in a plane perpendicular to the screen
/// correctly collapses to a line.
fn ocs(normal: &Vector, elevation: f64) -> Xform {
    let n = vec3(normal).norm();
    // The overwhelmingly common case, worth not doing three cross products for.
    if n.z > 0.999_999 && n.x.abs() < 1e-9 && n.y.abs() < 1e-9 {
        return Xform::IDENTITY;
    }
    let (ax, ay) = ocs_basis(n);
    let e = if elevation.is_finite() { elevation } else { 0.0 };
    Xform { a: ax.xy(), b: ay.xy(), c: n.xy() * e }
}

/// A bulge describes a counter-clockwise sweep; a mirroring transform reverses that.
fn bulge_for(x: &Xform, bulge: f64) -> f64 {
    if x.is_mirrored() {
        -bulge
    } else {
        bulge
    }
}

/// Chord tolerance for a polyline, relative to its own extent.
fn tol_for(pts: &[(V2, f64)], quality: f64) -> Tol {
    let b: Aabb = pts.iter().map(|(p, _)| *p).collect();
    let s = b.size();
    Tol::new(s.x.max(s.y).max(f64::MIN_POSITIVE) * quality).with_max(1024)
}

fn closed_sweep(sweep: f64) -> bool {
    (sweep.abs() - std::f64::consts::TAU).abs() < 1e-9
}

fn halign(h: HorizontalTextJustification) -> HAlign {
    use HorizontalTextJustification as H;
    match h {
        H::Left => HAlign::Left,
        H::Center | H::Middle => HAlign::Center,
        H::Right => HAlign::Right,
        // Aligned and Fit stretch the run between two points. We do not stretch, so anchoring at
        // the first of the two keeps the start of the text where the author put it.
        _ => HAlign::Left,
    }
}

/// Vertical alignment, accounting for the one horizontal code that also sets it.
///
/// `Middle` (group 72 = 4) is not "centre horizontally": it centres the text on the alignment
/// point in *both* directions, whatever group 73 says. Treating it as a horizontal-only code
/// drops the run half a line below where AutoCAD puts it.
fn valign_for(h: HorizontalTextJustification, v: VerticalTextJustification) -> VAlign {
    if matches!(h, HorizontalTextJustification::Middle) {
        return VAlign::Middle;
    }
    valign(v)
}

/// Decode the `%%` control codes DXF uses for characters its text encoding cannot carry.
///
/// `%%c` is the diameter sign and appears on almost every mechanical drawing; without this a
/// bore callout reads "%%c44" instead of "Ø44". Overscore and underscore toggles carry no glyph,
/// so they are simply removed.
pub fn decode_control_codes(s: &str) -> String {
    if !s.contains("%%") {
        return s.to_string();
    }
    let b: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == '%' && i + 2 < b.len() && b[i + 1] == '%' {
            let (skip, ch) = match b[i + 2] {
                'd' | 'D' => (3, Some('°')),
                'c' | 'C' => (3, Some('\u{2300}')),
                'p' | 'P' => (3, Some('±')),
                '%' => (3, Some('%')),
                // %%o and %%u toggle overscore and underscore, which have no glyph of their own.
                'o' | 'O' | 'u' | 'U' | 'k' | 'K' => (3, None),
                // %%nnn is a three-digit character code.
                c if c.is_ascii_digit() => {
                    let mut n = 0u32;
                    let mut k = 0;
                    while k < 3 && i + 2 + k < b.len() && b[i + 2 + k].is_ascii_digit() {
                        n = n * 10 + b[i + 2 + k].to_digit(10).unwrap();
                        k += 1;
                    }
                    (2 + k, char::from_u32(n))
                }
                _ => (2, None),
            };
            if let Some(c) = ch {
                out.push(c);
            }
            i += skip;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn valign(v: VerticalTextJustification) -> VAlign {
    use VerticalTextJustification as V;
    match v {
        V::Baseline => VAlign::Baseline,
        V::Bottom => VAlign::Bottom,
        V::Middle => VAlign::Middle,
        V::Top => VAlign::Top,
    }
}

/// Which point a TEXT entity actually sits on.
///
/// DXF keeps the insertion point in group 10 but ignores it whenever the text is justified: the
/// alignment point in group 11 takes over. Only plain left/baseline text uses group 10.
fn text_anchor(
    location: &Point,
    second: &Point,
    h: HorizontalTextJustification,
    v: VerticalTextJustification,
) -> V2 {
    let uses_second = !(matches!(h, HorizontalTextJustification::Left)
        && matches!(v, VerticalTextJustification::Baseline));
    if uses_second && (second.x != 0.0 || second.y != 0.0 || second.z != 0.0) {
        pt(second)
    } else {
        pt(location)
    }
}

fn attachment(a: dxf::enums::AttachmentPoint) -> (HAlign, VAlign) {
    use dxf::enums::AttachmentPoint as A;
    match a {
        A::TopLeft => (HAlign::Left, VAlign::Top),
        A::TopCenter => (HAlign::Center, VAlign::Top),
        A::TopRight => (HAlign::Right, VAlign::Top),
        A::MiddleLeft => (HAlign::Left, VAlign::Middle),
        A::MiddleCenter => (HAlign::Center, VAlign::Middle),
        A::MiddleRight => (HAlign::Right, VAlign::Middle),
        A::BottomLeft => (HAlign::Left, VAlign::Bottom),
        A::BottomCenter => (HAlign::Center, VAlign::Bottom),
        A::BottomRight => (HAlign::Right, VAlign::Bottom),
    }
}

/// Split MTEXT into display lines, dropping the inline formatting codes.
///
/// MTEXT is a small markup language. A viewer does not need to honour the styling, but it does need
/// the text itself and the line breaks, so strip the control sequences rather than showing them.
pub fn mtext_lines(raw: &str) -> Vec<String> {
    let mut lines = vec![String::new()];
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some('P') => lines.push(String::new()),
                // An escaped literal.
                Some(e @ ('\\' | '{' | '}')) => lines.last_mut().unwrap().push(e),
                // \~ is a non-breaking space.
                Some('~') => lines.last_mut().unwrap().push('\u{00a0}'),
                // Codes that run until a semicolon: colour, font, height, width, tracking, ...
                Some('C' | 'c' | 'F' | 'f' | 'H' | 'W' | 'T' | 'Q' | 'A' | 'p') => {
                    for n in it.by_ref() {
                        if n == ';' {
                            break;
                        }
                    }
                }
                // \S is a stacked fraction: "\S numerator ^ denominator ;". Treating it as a
                // single-character toggle leaves the parts and the terminating semicolon in the
                // text, so it runs to the semicolon like the other lettered codes. The numerator
                // and denominator are kept, separated by a slash.
                Some('S') => {
                    for n in it.by_ref() {
                        match n {
                            ';' => break,
                            '^' | '#' => lines.last_mut().unwrap().push('/'),
                            other => lines.last_mut().unwrap().push(other),
                        }
                    }
                }
                // Single-character toggles: underline, overline, strikethrough.
                Some('L' | 'l' | 'O' | 'o' | 'K' | 'k') => {}
                Some(other) => lines.last_mut().unwrap().push(other),
                None => {}
            },
            // Group braces only scope formatting.
            '{' | '}' => {}
            '\n' => lines.push(String::new()),
            '\r' => {}
            other => lines.last_mut().unwrap().push(other),
        }
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.iter().map(|l| decode_control_codes(l)).collect()
}

/// A conservative bounding box for a text run.
///
/// The real extent depends on the font, which the renderer picks; this only has to be good enough
/// for culling and zoom-to-fit, so it assumes a fixed advance and errs wide.
fn text_bbox(
    s: &str,
    pos: V2,
    height: f64,
    rotation: f64,
    width_factor: f64,
    halign: HAlign,
    valign: VAlign,
) -> Aabb {
    let w = s.chars().count() as f64 * height * GLYPH_ADVANCE * width_factor;
    let x0 = match halign {
        HAlign::Left => 0.0,
        HAlign::Center => -w * 0.5,
        HAlign::Right => -w,
    };
    let y0 = match valign {
        VAlign::Baseline => 0.0,
        VAlign::Bottom => 0.0,
        VAlign::Middle => -height * 0.5,
        VAlign::Top => -height,
    };
    let dir = V2::from_angle(rotation);
    let up = dir.perp();
    [(x0, y0), (x0 + w, y0), (x0, y0 + height), (x0 + w, y0 + height)]
        .into_iter()
        .map(|(a, b)| pos + dir * a + up * b)
        .collect()
}

fn units_of(dr: &Drawing) -> Units {
    use dxf::enums::Units as U;
    match dr.header.default_drawing_units {
        U::Unitless => Units::Unitless,
        U::Inches | U::USSurveyInch => Units::Inches,
        U::Feet | U::USSurveyFeet => Units::Feet,
        U::Miles | U::USSurveyMile => Units::Miles,
        U::Millimeters => Units::Millimeters,
        U::Centimeters => Units::Centimeters,
        U::Meters => Units::Meters,
        U::Kilometers => Units::Kilometers,
        U::Microinches => Units::Microinches,
        U::Mils => Units::Mils,
        U::Yards | U::USSurveyYard => Units::Yards,
        U::Angstroms => Units::Angstroms,
        U::Nanometers => Units::Nanometers,
        U::Microns => Units::Microns,
        U::Decimeters => Units::Decimeters,
        U::Decameters => Units::Decameters,
        U::Hectometers => Units::Hectometers,
        U::Gigameters => Units::Gigameters,
        U::AstronomicalUnits => Units::AstronomicalUnits,
        U::LightYears => Units::LightYears,
        U::Parsecs => Units::Parsecs,
    }
}

fn type_name(e: &EntityType) -> &'static str {
    use EntityType as E;
    match e {
        E::Face3D(_) => "3DFACE",
        E::Solid3D(_) => "3DSOLID",
        E::ProxyEntity(_) => "ACAD_PROXY_ENTITY",
        E::Arc(_) => "ARC",
        E::ArcAlignedText(_) => "ARCALIGNEDTEXT",
        E::AttributeDefinition(_) => "ATTDEF",
        E::Attribute(_) => "ATTRIB",
        E::Body(_) => "BODY",
        E::Circle(_) => "CIRCLE",
        E::RotatedDimension(_)
        | E::RadialDimension(_)
        | E::DiameterDimension(_)
        | E::AngularThreePointDimension(_)
        | E::OrdinateDimension(_) => "DIMENSION",
        E::Ellipse(_) => "ELLIPSE",
        E::Helix(_) => "HELIX",
        E::Image(_) => "IMAGE",
        E::Insert(_) => "INSERT",
        E::Leader(_) => "LEADER",
        E::Light(_) => "LIGHT",
        E::Line(_) => "LINE",
        E::LwPolyline(_) => "LWPOLYLINE",
        E::MLine(_) => "MLINE",
        E::MText(_) => "MTEXT",
        E::OleFrame(_) => "OLEFRAME",
        E::Ole2Frame(_) => "OLE2FRAME",
        E::ModelPoint(_) => "POINT",
        E::Polyline(_) => "POLYLINE",
        E::Ray(_) => "RAY",
        E::Region(_) => "REGION",
        E::RText(_) => "RTEXT",
        E::Section(_) => "SECTION",
        E::Seqend(_) => "SEQEND",
        E::Shape(_) => "SHAPE",
        E::Solid(_) => "SOLID",
        E::Spline(_) => "SPLINE",
        E::Text(_) => "TEXT",
        E::Tolerance(_) => "TOLERANCE",
        E::Trace(_) => "TRACE",
        E::DgnUnderlay(_) => "DGNUNDERLAY",
        E::DwfUnderlay(_) => "DWFUNDERLAY",
        E::PdfUnderlay(_) => "PDFUNDERLAY",
        E::Vertex(_) => "VERTEX",
        E::Wipeout(_) => "WIPEOUT",
        E::XLine(_) => "XLINE",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_1_SQRT_2;

    fn load(name: &str) -> Scene {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let dr = crate::read::load(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        convert(&dr, &Options::default())
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // -- the arbitrary axis algorithm ------------------------------------------------------

    #[test]
    fn ocs_for_plain_z_extrusion_is_the_identity() {
        assert_eq!(ocs(&Vector::new(0.0, 0.0, 1.0), 0.0), Xform::IDENTITY);
    }

    #[test]
    fn ocs_for_negative_z_mirrors_x() {
        // This is the case that makes the 1/64 rule necessary: crossing world Z with -Z would
        // collapse, so the algorithm switches to world Y.
        let x = ocs(&Vector::new(0.0, 0.0, -1.0), 0.0);
        let p = x.apply(v2(3.0, 5.0));
        assert!(close(p.x, -3.0) && close(p.y, 5.0), "{p:?}");
        assert!(x.is_mirrored());
    }

    #[test]
    fn ocs_basis_is_orthonormal_for_arbitrary_normals() {
        for n in [
            v3(1.0, 0.0, 0.0),
            v3(0.0, 1.0, 0.0),
            v3(0.001, 0.001, 1.0),
            v3(-0.4, 0.7, 0.3),
            v3(0.02, 0.0, 1.0), // just above the 1/64 threshold
            v3(0.01, 0.0, 1.0), // just below it
        ] {
            let n = n.norm();
            let (ax, ay) = ocs_basis(n);
            assert!(close(ax.len(), 1.0), "|Ax| = {} for {n:?}", ax.len());
            assert!(close(ay.len(), 1.0), "|Ay| = {} for {n:?}", ay.len());
            assert!(ax.dot(ay).abs() < 1e-9, "Ax.Ay = {} for {n:?}", ax.dot(ay));
            assert!(ax.dot(n).abs() < 1e-9 && ay.dot(n).abs() < 1e-9);
            // Right-handed: Ax x Ay = N.
            let c = ax.cross(ay);
            assert!((c - n).len() < 1e-9, "Ax x Ay = {c:?}, expected {n:?}");
        }
    }

    #[test]
    fn ocs_for_a_degenerate_normal_does_not_produce_nan() {
        let x = ocs(&Vector::new(0.0, 0.0, 0.0), 0.0);
        assert!(x.apply(v2(1.0, 1.0)).is_finite());
    }

    #[test]
    fn a_plane_perpendicular_to_the_view_collapses_to_a_line() {
        // Top-down orthographic: a circle standing on edge is a line, not a circle.
        let s = load("ocs_3d.dxf");
        let flat =
            s.polys.iter().filter(|p| p.bbox.size().x < 1e-9 || p.bbox.size().y < 1e-9).count();
        assert!(flat >= 1, "expected an edge-on entity to flatten, got none");
    }

    // -- fixtures --------------------------------------------------------------------------

    #[test]
    fn basic_fixture_produces_the_expected_shape() {
        let s = load("basic.dxf");
        assert!(!s.is_empty());
        assert_eq!(s.units, Units::Millimeters);
        assert_eq!(s.stats.entities_read, 38);
        // 10 grid lines + frame + 2 diagonals + 2 circles + 3 arcs = 18 polylines, 20 points.
        assert_eq!(s.dots.len(), 20);
        assert_eq!(s.polys.len(), 18);
        // Layers from the table, plus the implicit "0".
        let names: Vec<&str> = s.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["0", "WALLS", "DIMS", "HIDDEN"]);
        assert!(s.bounds.size().x > 100.0 && s.bounds.size().y > 100.0);
    }

    #[test]
    fn by_layer_and_by_block_colours_resolve() {
        let s = load("basic.dxf");
        // The frame is ByLayer on WALLS, whose ACI is 1 (red).
        let walls = s.layers.iter().position(|l| l.name == "WALLS").unwrap() as u16;
        assert_eq!(s.layers[walls as usize].color, aci::PALETTE[1]);
        let frame = s.polys.iter().find(|p| p.layer == walls).unwrap();
        assert_eq!(frame.color, aci::PALETTE[1]);
        // A ByBlock entity at top level falls back to the foreground.
        assert!(s.polys.iter().any(|p| p.color == Rgb::WHITE));
        // An explicit index wins over the layer.
        assert!(s.polys.iter().any(|p| p.color == aci::PALETTE[2]));
    }

    #[test]
    fn arcs_keep_a_refinable_source() {
        let s = load("basic.dxf");
        let arcs: Vec<&Poly> =
            s.polys.iter().filter(|p| matches!(p.source, CurveSource::Arc { .. })).collect();
        assert_eq!(arcs.len(), 5, "2 circles + 3 arcs");
        for a in &arcs {
            let CurveSource::Arc { center, radius, sweep, .. } = a.source else { unreachable!() };
            assert!(center.dist(v2(100.0, 75.0)) < 1e-9);
            assert!((30.0..=60.0).contains(&radius));
            assert!(sweep.abs() > 0.0);
        }
    }

    #[test]
    fn a_wrapping_arc_sweeps_the_short_way_round() {
        let s = load("basic.dxf");
        // ARC 270 -> 45 is a 135 degree sweep through 360, not a 225 degree one backwards.
        let a = s
            .polys
            .iter()
            .find_map(|p| match p.source {
                CurveSource::Arc { radius, sweep, .. } if (radius - 55.0).abs() < 1e-9 => {
                    Some(sweep)
                }
                _ => None,
            })
            .expect("the r=55 arc");
        assert!(close(a, 135f64.to_radians()), "sweep {a}");
    }

    #[test]
    fn polylines_with_bulges_become_real_arcs() {
        let s = load("polylines.dxf");
        // Two vertices, both bulge 1: the DXF idiom for a full circle.
        let circle = s
            .polys
            .iter()
            .find(|p| p.closed && p.bbox.center().dist(v2(90.0, 0.0)) < 1e-6)
            .expect("the bulge circle");
        for &v in s.vertices(circle) {
            assert!((v.dist(v2(90.0, 0.0)) - 20.0).abs() < 0.05, "{v:?} off the circle");
        }
        assert!(circle.len > 16, "a circle needs more than {} vertices", circle.len);
    }

    #[test]
    fn a_single_vertex_polyline_becomes_a_dot_rather_than_vanishing() {
        let s = load("polylines.dxf");
        assert!(s.dots.iter().any(|d| d.pos.dist(v2(200.0, 200.0)) < 1e-9));
    }

    #[test]
    fn old_style_polylines_are_read_too() {
        let s = load("polylines.dxf");
        // The POLYLINE/VERTEX/SEQEND shape has vertices at x in [130, 180], but its two bulged
        // sides arc outward past both, to roughly [117.5, 192.5].
        let p = s
            .polys
            .iter()
            .find(|p| p.closed && p.bbox.min.x > 100.0)
            .expect("the old-style POLYLINE");
        assert!(p.len > 8, "bulges were not flattened: {} vertices", p.len);
        assert!(p.bbox.min.x < 130.0 && p.bbox.max.x > 180.0, "{:?}", p.bbox);
        assert_eq!(p.color, aci::PALETTE[6]);
    }

    #[test]
    fn ellipses_and_splines_are_flattened() {
        let s = load("curves.dxf");
        let ellipses =
            s.polys.iter().filter(|p| matches!(p.source, CurveSource::Ellipse { .. })).count();
        assert_eq!(ellipses, 4);
        let splines =
            s.polys.iter().filter(|p| matches!(p.source, CurveSource::Spline { .. })).count();
        assert_eq!(splines, 4);
        assert_eq!(s.splines.len(), 4);
        for p in &s.polys {
            assert!(p.len >= 2, "an entity flattened to {} vertices", p.len);
            for v in s.vertices(p) {
                assert!(v.is_finite(), "non-finite vertex {v:?}");
            }
        }
    }

    #[test]
    fn a_rational_spline_traces_its_exact_arc() {
        let s = load("curves.dxf");
        // The weighted quadratic in the fixture is a quarter circle of radius 40 about (200, 0).
        let p = s
            .polys
            .iter()
            .find(|p| matches!(p.source, CurveSource::Spline { .. }) && p.bbox.min.x >= 199.0)
            .expect("the rational spline");
        // Weights 1, cos(45 deg), 1 over a right-angle corner give an exact quarter circle:
        // tangent to the horizontal at (200,0) and to the vertical at (240,40), so the centre is
        // at (200,40).
        assert!((FRAC_1_SQRT_2 - 0.5f64.sqrt()).abs() < 1e-12);
        for &v in s.vertices(p) {
            assert!((v.dist(v2(200.0, 40.0)) - 40.0).abs() < 0.05, "{v:?}");
        }
    }

    // -- blocks ----------------------------------------------------------------------------

    #[test]
    fn nested_blocks_expand_with_their_transforms() {
        let s = load("blocks.dxf");
        assert!(!s.polys.is_empty());
        // PLATE contains four BOLTs, each three primitives, plus the plate outline: 13 per insert.
        // Five PLATE inserts plus an 8x3 BOLT array of 3 primitives each.
        let expected = 5 * 13 + 8 * 3 * 3;
        assert_eq!(s.polys.len(), expected, "block expansion count");
    }

    #[test]
    fn a_mirrored_insert_flips_its_contents() {
        let s = load("blocks.dxf");
        // The mirrored PLATE sits at (0, 90) with x scale -1; its bolts must land at mirrored x.
        let near = |x: f64, y: f64| s.polys.iter().any(|p| p.bbox.center().dist(v2(x, y)) < 1.0);
        assert!(near(12.0, 78.0) && near(-12.0, 78.0), "mirrored bolt positions");
    }

    #[test]
    fn a_scaled_insert_scales_its_contents() {
        let s = load("blocks.dxf");
        // PLATE at (80, 0) is scaled 1.5x, so its outline is 60 wide rather than 40.
        let plate = s
            .polys
            .iter()
            .filter(|p| p.bbox.center().dist(v2(80.0, 0.0)) < 1.0)
            .max_by(|a, b| a.bbox.size().x.total_cmp(&b.bbox.size().x))
            .expect("the scaled plate outline");
        assert!(close(plate.bbox.size().x, 60.0), "width {}", plate.bbox.size().x);
    }

    #[test]
    fn a_minsert_array_places_every_copy() {
        let s = load("blocks.dxf");
        // 8 columns x 3 rows of BOLT at 20 unit spacing from (0, 200).
        for col in 0..8 {
            for row in 0..3 {
                let c = v2(col as f64 * 20.0, 200.0 + row as f64 * 20.0);
                assert!(
                    s.polys.iter().any(|p| p.bbox.center().dist(c) < 1.0),
                    "array cell {col},{row} at {c:?} is missing"
                );
            }
        }
    }

    #[test]
    fn by_block_takes_the_colour_of_the_reference_that_placed_it() {
        let s = load("blocks.dxf");
        // Every entity in BOLT is ByBlock. The array at y=200 inserts BOLT directly with ACI 4.
        let arrayed: Vec<&Poly> =
            s.polys.iter().filter(|p| p.bbox.min.y > 190.0 && p.bbox.max.y < 250.0).collect();
        assert_eq!(arrayed.len(), 8 * 3 * 3);
        assert!(
            arrayed.iter().all(|p| p.color == aci::PALETTE[4]),
            "ByBlock colour did not inherit"
        );
    }

    #[test]
    fn by_block_resolves_against_the_nearest_reference_not_the_outermost() {
        let s = load("blocks.dxf");
        // PLATE is inserted with ACI 1, but the BOLT references *inside* PLATE are ByLayer on
        // layer 0, so they resolve to layer 0's own colour first. Their ByBlock contents then
        // inherit that, not PLATE's red. This is what AutoCAD does, and it is easy to get wrong.
        let bolts: Vec<&Poly> = s
            .polys
            .iter()
            .filter(|p| p.bbox.center().dist(v2(80.0, 0.0)) < 40.0 && p.bbox.size().x < 20.0)
            .collect();
        assert!(!bolts.is_empty());
        assert!(bolts.iter().all(|p| p.color == Rgb::WHITE), "{:?}", bolts[0].color);
        // The PLATE outline itself is ByLayer on PARTS, which is yellow whatever the insert says.
        let outline = s
            .polys
            .iter()
            .filter(|p| p.bbox.center().dist(v2(80.0, 0.0)) < 1.0)
            .max_by(|a, b| a.bbox.size().x.total_cmp(&b.bbox.size().x))
            .unwrap();
        assert_eq!(outline.color, aci::PALETTE[2]);
    }

    #[test]
    fn a_missing_block_is_reported_not_ignored() {
        let s = load("blocks.dxf");
        assert!(
            s.stats.warnings.iter().any(|w| w.contains("MISSING_BLOCK")),
            "warnings: {:?}",
            s.stats.warnings
        );
    }

    // -- text ------------------------------------------------------------------------------

    #[test]
    fn text_justification_selects_the_right_anchor() {
        let s = load("text.dxf");
        let by = |t: &str| s.texts.iter().find(|x| x.text == t).unwrap();
        assert_eq!(by("left/base").halign, HAlign::Left);
        assert_eq!(by("left/base").valign, VAlign::Baseline);
        assert_eq!(by("center/base").halign, HAlign::Center);
        assert_eq!(by("right/top").halign, HAlign::Right);
        assert_eq!(by("right/top").valign, VAlign::Top);
        assert_eq!(by("center/middle").valign, VAlign::Middle);
        // Left/baseline text uses group 10; everything else uses the alignment point in group 11.
        assert!(by("left/base").pos.dist(v2(0.0, 100.0)) < 1e-9);
        assert!(by("center/base").pos.dist(v2(0.0, 80.0)) < 1e-9);
    }

    #[test]
    fn rotated_text_keeps_its_angle() {
        let s = load("text.dxf");
        let t = s.texts.iter().find(|x| x.text == "Rotated 30deg").unwrap();
        assert!(close(t.rotation, 30f64.to_radians()));
    }

    #[test]
    fn empty_and_zero_height_text_is_dropped() {
        let s = load("text.dxf");
        assert!(s.texts.iter().all(|t| !t.text.is_empty() && t.height > 0.0));
        let s = load("malformed.dxf");
        assert!(s.texts.iter().all(|t| t.height > 0.0));
    }

    #[test]
    fn mtext_splits_on_its_line_breaks() {
        let s = load("text.dxf");
        for want in ["MText line one", "line two", "line three"] {
            assert!(s.texts.iter().any(|t| t.text == want), "missing MTEXT line {want:?}");
        }
    }

    #[test]
    fn control_codes_become_the_characters_they_stand_for() {
        assert_eq!(decode_control_codes("%%c44 H7"), "\u{2300}44 H7");
        assert_eq!(decode_control_codes("45%%d"), "45°");
        assert_eq!(decode_control_codes("%%p0.1"), "±0.1");
        assert_eq!(decode_control_codes("100%%%"), "100%");
        assert_eq!(decode_control_codes("%%uunderlined%%u"), "underlined");
        assert_eq!(decode_control_codes("%%C%%D%%P"), "\u{2300}°±");
        // A three-digit character code.
        assert_eq!(decode_control_codes("%%176"), "°");
        // Plain text is untouched, and a stray %% at the end does not panic or eat characters.
        assert_eq!(decode_control_codes("no codes here"), "no codes here");
        assert_eq!(decode_control_codes("trailing%%"), "trailing%%");
        assert_eq!(decode_control_codes("%%"), "%%");
        assert_eq!(decode_control_codes(""), "");
        // Multi-byte input must not be split mid-character.
        assert_eq!(decode_control_codes("図%%c面"), "図\u{2300}面");
    }

    #[test]
    fn middle_justification_centres_vertically_as_well() {
        use HorizontalTextJustification as H;
        use VerticalTextJustification as V;
        // Group 72 = 4 means "middle", which centres in both directions whatever group 73 says.
        assert_eq!(valign_for(H::Middle, V::Baseline), VAlign::Middle);
        assert_eq!(valign_for(H::Middle, V::Top), VAlign::Middle);
        // Every other horizontal code leaves the vertical one alone.
        assert_eq!(valign_for(H::Center, V::Baseline), VAlign::Baseline);
        assert_eq!(valign_for(H::Left, V::Top), VAlign::Top);
        assert_eq!(valign_for(H::Right, V::Bottom), VAlign::Bottom);
    }

    #[test]
    fn mtext_formatting_codes_are_stripped() {
        assert_eq!(mtext_lines(r"plain"), ["plain"]);
        assert_eq!(mtext_lines(r"a\Pb\Pc"), ["a", "b", "c"]);
        assert_eq!(mtext_lines(r"{\C1;red} text"), ["red text"]);
        assert_eq!(mtext_lines(r"\fArial|b1;bold"), ["bold"]);
        assert_eq!(mtext_lines(r"\Lunder\lline"), ["underline"]);
        // A literal space after a toggle is content, not part of the code.
        assert_eq!(mtext_lines(r"\L a\l b"), [" a b"]);
        assert_eq!(mtext_lines(r"a\\b"), [r"a\b"]);
        assert_eq!(mtext_lines(r"x\{y\}"), ["x{y}"]);
        assert_eq!(mtext_lines(r"\H2.5x;big"), ["big"]);
        // A stacked fraction keeps both parts rather than leaking its terminator.
        assert_eq!(mtext_lines(r"1\S+0.1^-0.2;"), ["1+0.1/-0.2"]);
        assert_eq!(mtext_lines(r"\S1#2; nominal"), ["1/2 nominal"]);
        // Control codes are decoded in MTEXT too.
        assert_eq!(mtext_lines("%%c20"), ["\u{2300}20"]);
        assert!(mtext_lines("").is_empty());
        assert!(mtext_lines(r"\P\P").is_empty());
    }

    #[test]
    fn mtext_lines_stack_downward_from_the_attachment_point() {
        let s = load("text.dxf");
        let one = s.texts.iter().find(|t| t.text == "MText line one").unwrap();
        let two = s.texts.iter().find(|t| t.text == "line two").unwrap();
        let three = s.texts.iter().find(|t| t.text == "line three").unwrap();
        assert!(one.pos.y > two.pos.y && two.pos.y > three.pos.y, "MTEXT lines out of order");
    }

    // -- robustness ------------------------------------------------------------------------

    #[test]
    fn text_is_decoded_with_the_encoding_the_header_implies() {
        // showcase.dxf is R2007, so UTF-8.
        let s = load("showcase.dxf");
        assert!(
            s.texts.iter().any(|t| t.text.contains('Ø')),
            "UTF-8 diameter sign was mangled: {:?}",
            s.texts.iter().map(|t| &t.text).collect::<Vec<_>>()
        );
        // latin1.dxf is R2000 and written in Windows-1252.
        let s = load("latin1.dxf");
        let all: Vec<&str> = s.texts.iter().map(|t| t.text.as_str()).collect();
        assert!(all.iter().any(|t| t.contains("Ø20")), "{all:?}");
        assert!(all.iter().any(|t| t.contains("für")), "{all:?}");
        assert!(all.iter().any(|t| t.contains("Maßstab")), "{all:?}");
    }

    #[test]
    fn an_empty_drawing_converts_to_an_empty_scene() {
        let s = load("empty.dxf");
        assert!(s.is_empty());
        assert!(s.bounds.is_empty());
        assert_eq!(s.stats.primitives, 0);
    }

    #[test]
    fn malformed_geometry_never_panics_or_leaks_nan() {
        let s = load("malformed.dxf");
        assert!(s.bounds.is_empty() || s.bounds.min.is_finite());
        for p in &s.polys {
            assert!(p.len >= 2);
            for v in s.vertices(p) {
                assert!(v.is_finite(), "non-finite vertex from malformed input: {v:?}");
            }
        }
        for t in &s.tris {
            assert!(t.a.is_finite() && t.b.is_finite() && t.c.is_finite());
        }
        // Zero and negative radii, zero sweeps and zero-length lines all drop out.
        assert!(s.polys.iter().all(|p| !p.bbox.is_empty()));
    }

    #[test]
    fn a_zero_scale_insert_draws_nothing() {
        let s = load("malformed.dxf");
        assert!(s.polys.iter().all(|p| p.bbox.center().dist(v2(0.0, 40.0)) > 1.0));
    }

    #[test]
    fn an_undefined_layer_is_registered_once_not_once_per_entity() {
        // The dxf crate happens to backfill referenced-but-undefined layers into the table, so
        // this exercises the fallback directly: a layer name that reaches layer_of without ever
        // having been in layer_ids must produce exactly one Layer however many entities use it.
        let dr =
            crate::read::load(format!("{}/tests/fixtures/basic.dxf", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
        let mut c = Ctx::new(&dr, Options::default());
        c.layer_ids.clear();
        let ents: Vec<&Entity> = dr.entities().collect();
        let before = c.scene.layers.len();
        let root = Inherit::root(0, Rgb::WHITE);
        let ids: Vec<u16> = ents.iter().map(|e| c.layer_of(e, &root)).collect();
        // basic.dxf uses four distinct layer names.
        assert_eq!(c.scene.layers.len(), before + 4, "one Layer per distinct name, not per entity");
        // Every entity on the same layer name must land on the same id.
        for (a, b) in ents.iter().zip(&ids) {
            let again = c.layer_of(a, &root);
            assert_eq!(again, *b, "layer id for {:?} was not stable", a.common.layer);
        }
        assert_eq!(c.scene.layers.len(), before + 4, "a second pass must not add more layers");
    }

    #[test]
    fn a_mirrored_arc_lands_on_the_right_side_of_its_circle() {
        // Under a -Z extrusion the OCS x axis is negated, so the image of OCS angle t is at
        // (pi - t), not (pi + t). An arc starting at zero cannot tell those apart; this one
        // starts at 45 degrees, where the two answers differ by 90.
        let s = load("ocs_3d.dxf");
        let p = s
            .polys
            .iter()
            .find(|p| {
                matches!(p.source, CurveSource::Arc { center, radius, .. }
                if center.dist(v2(0.0, -40.0)) < 1e-9 && (radius - 20.0).abs() < 1e-9)
            })
            .expect("the mirrored arc");
        let CurveSource::Arc { start, sweep, .. } = p.source else { unreachable!() };
        // OCS 45..135 mirrors to WCS 135..45, i.e. it starts at 135 and sweeps backwards.
        assert!(close(start, 135f64.to_radians()), "start {}", start.to_degrees());
        assert!(close(sweep, -90f64.to_radians()), "sweep {}", sweep.to_degrees());
        // Which puts it across the top of its circle, not down one side. The bbox is of the
        // tessellated polyline, so it sits within the chord tolerance of the true arc.
        let tol = 0.1;
        assert!(
            (p.bbox.min.y - (-40.0 + 20.0 * 45f64.to_radians().sin())).abs() < tol,
            "{:?}",
            p.bbox
        );
        assert!((p.bbox.max.y - (-20.0)).abs() < tol, "{:?}", p.bbox);
        assert!(p.bbox.min.x < -14.0 && p.bbox.max.x > 14.0, "{:?}", p.bbox);
    }

    #[test]
    fn the_one_sixty_fourth_threshold_switches_the_reference_axis() {
        // Below the threshold the algorithm crosses with world Y, above it with world Z. An
        // extrusion just either side of 1/64 must therefore give visibly different bases.
        let below = ocs(&Vector::new(0.01, 0.0, 1.0), 0.0);
        let above = ocs(&Vector::new(0.02, 0.0, 1.0), 0.0);
        assert!(below.a.dist(above.a) > 0.5, "the two branches produced the same X axis");
        // The below-threshold branch crosses world Y with a near-Z normal, giving an X axis that
        // points roughly along world +X; the above-threshold branch gives roughly world +Y.
        assert!(below.a.x.abs() > 0.9, "{:?}", below.a);
        assert!(above.a.y.abs() > 0.9, "{:?}", above.a);
    }

    #[test]
    fn attributes_written_after_an_insert_are_drawn() {
        // The parser folds them into the INSERT, so they never reach the entity stream: reading
        // them back out is the only way they get drawn at all.
        let s = load("annotation.dxf");
        let by = |t: &str| s.texts.iter().find(|x| x.text == t);
        assert!(
            by("PUMP-101").is_some(),
            "{:?}",
            s.texts.iter().map(|t| &t.text).collect::<Vec<_>>()
        );
        // An attribute inherits the colour of the INSERT that carries it.
        assert_eq!(by("PUMP-101").unwrap().color, aci::PALETTE[2]);
        // Control codes are decoded in attribute values too.
        assert!(by("\u{2300}50 BORE").is_some(), "control code not decoded in an attribute");
        assert_eq!(by("\u{2300}50 BORE").unwrap().color, aci::PALETTE[3]);
        // An attribute sits where the file puts it, in world space, not in block space.
        assert!(by("PUMP-101").unwrap().pos.dist(v2(-8.0, -2.0)) < 1e-9);
    }

    #[test]
    fn construction_lines_are_drawn_but_left_out_of_the_extent() {
        let s = load("annotation.dxf");
        // Both a RAY and an XLINE are present and enormous.
        let far = s.polys.iter().filter(|p| p.unbounded).count();
        assert_eq!(far, 2, "the RAY and XLINE should both be drawn");
        assert!(s.polys.iter().any(|p| p.unbounded && p.bbox.size().x > 1e6));
        // But the drawing's extent is the size of the actual parts, so Fit frames those.
        assert!(
            s.bounds.size().x < 200.0,
            "an infinite line dragged out the extent: {:?}",
            s.bounds
        );
        assert!(s.visible_bounds().size().x < 200.0, "{:?}", s.visible_bounds());
    }

    #[test]
    fn a_zero_scale_insert_of_a_real_block_draws_nothing() {
        // The earlier version of this test named a block that did not exist, so the INSERT exited
        // on the missing-block branch and never reached the scale check at all.
        let s = load("malformed.dxf");
        let near = |x: f64, y: f64| s.polys.iter().any(|p| p.bbox.center().dist(v2(x, y)) < 6.0);
        assert!(near(40.0, 40.0), "the control insert at a usable scale should draw");
        assert!(!near(0.0, 40.0), "a zero-scale insert should draw nothing");
    }

    #[test]
    fn a_layer_name_is_matched_without_regard_to_case() {
        // AutoCAD treats table names case-insensitively, so "Walls" and "WALLS" are one layer.
        let dr =
            crate::read::load(format!("{}/tests/fixtures/basic.dxf", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
        let c = Ctx::new(&dr, Options::default());
        let n = c.scene.layers.len();
        for name in ["WALLS", "walls", "Walls", "wAlLs"] {
            assert!(c.layer_ids.contains_key(&name.to_uppercase()), "{name}");
        }
        assert_eq!(n, 4, "case variants must not create extra layers");
    }

    #[test]
    fn every_primitive_points_at_a_real_layer() {
        for f in
            ["basic.dxf", "polylines.dxf", "curves.dxf", "blocks.dxf", "text.dxf", "ocs_3d.dxf"]
        {
            let s = load(f);
            let n = s.layers.len() as u16;
            for p in &s.polys {
                assert!(p.layer < n, "{f}: layer {} out of range", p.layer);
                assert!(p.start + p.len <= s.verts.len() as u32, "{f}: vertex range overruns");
            }
            for d in &s.dots {
                assert!(d.layer < n);
            }
            for t in &s.texts {
                assert!(t.layer < n);
            }
            // Layer counts must add up to what was drawn.
            let total: u32 = s.layers.iter().map(|l| l.count).sum();
            assert_eq!(total as usize, s.stats.primitives, "{f}: layer counts disagree");
        }
    }

    #[test]
    fn solids_produce_two_triangles_in_draw_order() {
        let s = load("ocs_3d.dxf");
        // The SOLID spans (60,0)-(90,30); its corners are stored 1-2-4-3.
        let tris: Vec<&Tri> = s.tris.iter().filter(|t| t.bbox.min.x >= 59.0).collect();
        assert_eq!(tris.len(), 2);
        let b = tris.iter().fold(Aabb::EMPTY, |a, t| a.union(&t.bbox));
        assert!(close(b.min.x, 60.0) && close(b.max.x, 90.0));
        assert!(close(b.min.y, 0.0) && close(b.max.y, 30.0));
    }

    #[test]
    fn the_spatial_index_is_built_and_covers_the_scene() {
        let s = load("basic.dxf");
        assert!(!s.index.is_empty());
        let mut seen = 0;
        s.query(&s.bounds, |_, _| seen += 1);
        assert!(seen >= s.polys.len() + s.dots.len());
    }

    #[test]
    fn a_block_instance_produces_more_primitives_than_entities() {
        let s = load("blocks.dxf");
        // Seven INSERTs expand into 137 polylines, so the two counters must not be conflated.
        assert_eq!(s.stats.entities_read, 7);
        assert_eq!(s.stats.primitives, 137);
        // The dangling INSERT drew nothing.
        assert_eq!(s.stats.entities_skipped, 1);
    }

    #[test]
    fn unsupported_entities_are_counted_rather_than_dropped_silently() {
        let s = load("polylines.dxf");
        // Nothing unsupported in this file, but the field must exist and be consistent.
        assert!(s.stats.unsupported.iter().all(|(_, n)| *n > 0));
        assert!(s.stats.primitives > 0);
    }
}
