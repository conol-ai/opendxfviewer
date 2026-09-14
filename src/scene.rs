//! The render model.
//!
//! [`convert`](crate::convert) flattens a DXF drawing into a `Scene` once, at load time: blocks are
//! instanced, OCS coordinates are mapped into world space, and every curve becomes a polyline. The
//! renderer then only ever walks flat arrays.
//!
//! Vertices live in one shared `f64` arena and primitives reference slices of it. That keeps the
//! draw loop sequential over memory, and keeps precision: DXF survey drawings carry coordinates in
//! the millions, where `f32` would quantise to centimetres.

use crate::geom::{Aabb, V2};

/// A resolved 8-bit-per-channel colour.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const WHITE: Rgb = Rgb(255, 255, 255);
    pub const BLACK: Rgb = Rgb(0, 0, 0);

    /// Relative luminance, used to decide whether ACI 7 should render black or white.
    pub fn luma(self) -> f32 {
        (0.2126 * self.0 as f32 + 0.7152 * self.1 as f32 + 0.0722 * self.2 as f32) / 255.0
    }
}

/// Which curve a polyline was flattened from, kept so the renderer can re-tessellate it at higher
/// resolution when the user zooms past the fidelity baked in at load time.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum CurveSource {
    /// Already straight — nothing to refine.
    None,
    /// A circle or circular arc in world space. `sweep` is signed; a full circle sweeps ±2π.
    Arc { center: V2, radius: f64, start: f64, sweep: f64 },
    /// An ellipse or elliptical arc: `center + major*cos(t) + minor*sin(t)` for `t` in the sweep.
    Ellipse { center: V2, major: V2, minor: V2, start: f64, sweep: f64 },
    /// A NURBS curve; indices point into [`Scene::splines`].
    Spline { index: u32 },
    /// A polyline carrying per-vertex bulges; indices point into [`Scene::bulges`].
    ///
    /// Kept for the same reason as a spline: rounded corners are how CAD outlines are actually
    /// drawn, and flattening them once at load leaves them visibly faceted at any closer zoom.
    Bulge { index: u32 },
}

/// A polyline with per-vertex bulges, retained so it can be re-tessellated when the view zooms in.
#[derive(Clone, Debug)]
pub struct BulgePoly {
    /// `(position, bulge)`, where the bulge describes the segment *leaving* that vertex.
    pub pts: Vec<(V2, f64)>,
    pub closed: bool,
}

/// A polyline: the single geometric primitive everything else is reduced to.
#[derive(Clone, Debug)]
pub struct Poly {
    /// Range into [`Scene::verts`].
    pub start: u32,
    pub len: u32,
    pub layer: u16,
    /// Fully resolved colour — ByLayer/ByBlock are already applied by `convert`.
    pub color: Rgb,
    /// DXF lineweight in hundredths of a millimetre, or `None` for the layer default.
    pub lineweight: Option<i16>,
    /// Index into [`Scene::linetypes`].
    pub linetype: u16,
    /// The entity's own dash-length multiplier (DXF group 48), on top of the drawing's `$LTSCALE`.
    pub linetype_scale: f32,
    pub closed: bool,
    /// True for RAY and XLINE, which are infinite. They are drawn, but they are left out of the
    /// drawing extent — otherwise one construction line makes zoom-to-fit show empty space.
    pub unbounded: bool,
    pub bbox: Aabb,
    pub source: CurveSource,
}

/// A standalone POINT entity, drawn as a dot at a fixed pixel size.
#[derive(Clone, Debug)]
pub struct Dot {
    pub pos: V2,
    pub layer: u16,
    pub color: Rgb,
}

/// A filled triangle from SOLID / TRACE / 3DFACE / a hatch.
#[derive(Clone, Debug)]
pub struct Tri {
    pub a: V2,
    pub b: V2,
    pub c: V2,
    pub layer: u16,
    pub color: Rgb,
    pub bbox: Aabb,
}

/// Where a text run sits, relative to its anchor.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VAlign {
    Baseline,
    Bottom,
    Middle,
    Top,
}

/// One line of text placed in world space.
#[derive(Clone, Debug)]
pub struct Text {
    pub text: String,
    /// Anchor point; [`Text::halign`]/[`Text::valign`] say which part of the run sits here.
    pub pos: V2,
    /// Cap height in world units.
    pub height: f64,
    /// Baseline direction, in radians CCW from +X.
    pub rotation: f64,
    /// Horizontal stretch applied to glyph advances (DXF group 41). Used for the run's extent,
    /// which is what culling and zoom-to-fit need; the renderer cannot stretch glyphs.
    pub width_factor: f64,
    pub halign: HAlign,
    pub valign: VAlign,
    pub layer: u16,
    pub color: Rgb,
    pub bbox: Aabb,
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub name: String,
    pub color: Rgb,
    /// Layers can be off (negative colour index) or frozen in the source file.
    pub visible_in_file: bool,
    /// Toggled by the user in the layer panel.
    pub visible: bool,
    pub lineweight: Option<i16>,
    pub linetype: u16,
    /// How many primitives reference this layer, shown in the panel.
    pub count: u32,
}

/// A dash pattern in drawing units. An empty `pattern` means solid.
#[derive(Clone, Debug, Default)]
pub struct Linetype {
    pub name: String,
    /// Positive = dash, negative = gap, zero = dot.
    pub pattern: Vec<f64>,
    pub total: f64,
}

impl Linetype {
    pub fn is_solid(&self) -> bool {
        self.pattern.is_empty() || self.total <= 0.0
    }
}

/// Everything `convert` could not represent, surfaced in the UI rather than silently dropped.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Entities in the file's ENTITIES section.
    pub entities_read: usize,
    /// Renderable primitives produced from them. Higher than `entities_read` whenever a block is
    /// instanced, since one INSERT expands into everything the block contains.
    pub primitives: usize,
    /// Entities that produced nothing at all, because they were unsupported or degenerate.
    pub entities_skipped: usize,
    /// Entity type name -> how many were skipped.
    pub unsupported: Vec<(String, usize)>,
    /// Non-fatal problems: missing blocks, malformed splines, recursion limits.
    pub warnings: Vec<String>,
    pub load_ms: u32,
}

/// Drawing units from `$INSUNITS`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Units {
    #[default]
    Unitless,
    Inches,
    Feet,
    Miles,
    Millimeters,
    Centimeters,
    Meters,
    Kilometers,
    Microinches,
    Mils,
    Yards,
    Angstroms,
    Nanometers,
    Microns,
    Decimeters,
    Decameters,
    Hectometers,
    Gigameters,
    AstronomicalUnits,
    LightYears,
    Parsecs,
}

impl Units {
    /// Short suffix for the status bar.
    pub fn suffix(self) -> &'static str {
        use Units::*;
        match self {
            Unitless => "",
            Inches => "in",
            Feet => "ft",
            Miles => "mi",
            Millimeters => "mm",
            Centimeters => "cm",
            Meters => "m",
            Kilometers => "km",
            Microinches => "µin",
            Mils => "mil",
            Yards => "yd",
            Angstroms => "Å",
            Nanometers => "nm",
            Microns => "µm",
            Decimeters => "dm",
            Decameters => "dam",
            Hectometers => "hm",
            Gigameters => "Gm",
            AstronomicalUnits => "AU",
            LightYears => "ly",
            Parsecs => "pc",
        }
    }
}

/// The flattened drawing.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub verts: Vec<V2>,
    pub polys: Vec<Poly>,
    pub dots: Vec<Dot>,
    pub tris: Vec<Tri>,
    pub texts: Vec<Text>,
    /// NURBS definitions retained so a curve can be re-tessellated when the view zooms in.
    pub splines: Vec<crate::tessellate::Nurbs>,
    /// Bulged polylines retained for on-demand refinement.
    pub bulges: Vec<BulgePoly>,
    pub layers: Vec<Layer>,
    pub linetypes: Vec<Linetype>,
    pub bounds: Aabb,
    /// The drawing's `$LTSCALE`: a global multiplier on every dash pattern.
    pub linetype_scale: f64,
    pub units: Units,
    pub stats: Stats,
    /// Built by [`Scene::build_index`]; empty until then.
    pub index: Grid,
}

impl Scene {
    pub fn vertices(&self, p: &Poly) -> &[V2] {
        &self.verts[p.start as usize..(p.start + p.len) as usize]
    }

    pub fn is_visible(&self, layer: u16) -> bool {
        self.layers.get(layer as usize).is_none_or(|l| l.visible && l.visible_in_file)
    }

    pub fn is_empty(&self) -> bool {
        self.polys.is_empty()
            && self.dots.is_empty()
            && self.tris.is_empty()
            && self.texts.is_empty()
    }

    /// Rebuild [`Scene::bounds`] from the primitives that are currently on a visible layer.
    pub fn visible_bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for p in &self.polys {
            if self.is_visible(p.layer) && !p.unbounded {
                b = b.union(&p.bbox);
            }
        }
        for d in &self.dots {
            if self.is_visible(d.layer) {
                b.add_point(d.pos);
            }
        }
        for t in &self.tris {
            if self.is_visible(t.layer) {
                b = b.union(&t.bbox);
            }
        }
        for t in &self.texts {
            if self.is_visible(t.layer) {
                b = b.union(&t.bbox);
            }
        }
        b
    }
}

/// Which array a [`Grid`] entry refers to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PrimKind {
    Poly,
    Dot,
    Tri,
    Text,
}

/// A uniform grid over the drawing extent, so a zoomed-in view touches only nearby primitives.
///
/// Entries are bucketed by cell in one flat array with a CSR-style offset table: `cells[i]` holds
/// the start of cell `i` in `items`, and `cells[i + 1]` its end.
#[derive(Clone, Debug, Default)]
pub struct Grid {
    pub bounds: Aabb,
    pub cols: u32,
    pub rows: u32,
    pub cell: V2,
    pub cells: Vec<u32>,
    pub items: Vec<(PrimKind, u32)>,
    /// Primitives that span too many cells to bucket. Listing a drawing-wide line in every cell it
    /// touches would cost one entry per cell; these are reported on every query instead.
    pub large: Vec<(PrimKind, u32)>,
}

impl Grid {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.large.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::v2;

    fn poly(layer: u16, b: Aabb) -> Poly {
        Poly {
            start: 0,
            len: 0,
            layer,
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

    fn layer(name: &str) -> Layer {
        Layer {
            name: name.into(),
            color: Rgb::WHITE,
            visible_in_file: true,
            visible: true,
            lineweight: None,
            linetype: 0,
            count: 1,
        }
    }

    /// Two layers, one small and one far away, so hiding either changes the extent visibly.
    fn two_layer_scene() -> Scene {
        let mut s = Scene::default();
        s.layers.push(layer("NEAR"));
        s.layers.push(layer("FAR"));
        s.polys.push(poly(0, Aabb::new(v2(0.0, 0.0), v2(10.0, 10.0))));
        s.polys.push(poly(1, Aabb::new(v2(900.0, 900.0), v2(1000.0, 1000.0))));
        s.bounds = s.polys[0].bbox.union(&s.polys[1].bbox);
        s
    }

    #[test]
    fn visible_bounds_follows_the_layer_panel() {
        // This is what "Fit" frames, so hiding a layer has to shrink it or Fit shows empty space.
        let mut s = two_layer_scene();
        assert_eq!(s.visible_bounds(), s.bounds);

        s.layers[1].visible = false;
        assert_eq!(s.visible_bounds(), Aabb::new(v2(0.0, 0.0), v2(10.0, 10.0)));

        s.layers[0].visible = false;
        assert!(s.visible_bounds().is_empty(), "everything hidden should frame nothing");

        s.layers[0].visible = true;
        s.layers[1].visible = true;
        assert_eq!(s.visible_bounds(), s.bounds, "restoring must restore the extent");
    }

    #[test]
    fn a_layer_switched_off_in_the_file_is_left_out_of_the_extent() {
        let mut s = two_layer_scene();
        s.layers[1].visible_in_file = false;
        assert_eq!(s.visible_bounds(), Aabb::new(v2(0.0, 0.0), v2(10.0, 10.0)));
        // And the user ticking it cannot override the file.
        s.layers[1].visible = true;
        assert_eq!(s.visible_bounds(), Aabb::new(v2(0.0, 0.0), v2(10.0, 10.0)));
    }

    #[test]
    fn construction_lines_never_enter_the_visible_extent() {
        let mut s = two_layer_scene();
        let mut ray = poly(0, Aabb::new(v2(-1e7, -1e7), v2(1e7, 1e7)));
        ray.unbounded = true;
        s.polys.push(ray);
        assert_eq!(s.visible_bounds(), s.bounds, "an infinite line dragged out the extent");
    }
}
