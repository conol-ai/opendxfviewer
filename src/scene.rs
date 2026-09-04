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
    pub closed: bool,
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
    /// Horizontal stretch applied to glyph advances (DXF group 41).
    pub width_factor: f64,
    /// Italic slant in radians (DXF group 51).
    pub oblique: f64,
    pub halign: HAlign,
    pub valign: VAlign,
    pub layer: u16,
    pub color: Rgb,
    pub bbox: Aabb,
}

/// A NURBS definition retained for on-demand refinement.
#[derive(Clone, Debug)]
pub struct Spline {
    pub degree: usize,
    pub ctrl: Vec<V2>,
    pub knots: Vec<f64>,
    /// Empty when the spline is non-rational.
    pub weights: Vec<f64>,
    pub closed: bool,
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

/// Everything `convert` could not represent, surfaced in the UI rather than silently dropped.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub entities_read: usize,
    pub entities_drawn: usize,
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
    pub splines: Vec<Spline>,
    pub layers: Vec<Layer>,
    pub linetypes: Vec<Linetype>,
    pub bounds: Aabb,
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
        self.polys.is_empty() && self.dots.is_empty() && self.tris.is_empty() && self.texts.is_empty()
    }

    /// Rebuild [`Scene::bounds`] from the primitives that are currently on a visible layer.
    pub fn visible_bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for p in &self.polys {
            if self.is_visible(p.layer) {
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
}

impl Grid {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
