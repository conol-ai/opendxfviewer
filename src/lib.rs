//! A fast, open source DXF viewer.
//!
//! The crate is split so the CAD side and the UI side stay independent:
//!
//! * [`geom`] — `f64` vector/transform/AABB primitives shared by everything below.
//! * [`aci`] — the AutoCAD Color Index palette.
//! * [`tessellate`] — turning DXF curve definitions (arcs, bulges, ellipses, NURBS) into polylines.
//! * [`convert`] — walking a parsed [`dxf::Drawing`] into a flat, render-ready [`scene::Scene`],
//!   resolving OCS, block instancing and colours along the way.
//! * [`scene`] — the render model plus the spatial index used for view culling.
//! * [`camera`] — the world↔screen mapping, pan, zoom and fit.
//! * [`canvas`] — the custom Makepad widget that draws a scene and handles navigation.
//! * [`layers`] — the layer visibility panel.
//! * [`app`] — the application shell: window chrome, layer panel, file loading.
pub use makepad_widgets;

pub mod aci;
pub mod app;
pub mod camera;
pub mod canvas;
pub mod convert;
pub mod geom;
pub mod layers;
pub mod render;
pub mod scene;
pub mod spatial;
pub mod tessellate;
