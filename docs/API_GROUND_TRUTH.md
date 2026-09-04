# Verified API ground truth

Everything here was verified on this machine by reading crate source or by running
`cargo run --example probe`. Do NOT contradict it from memory.

## Local source paths
```
REG=~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f
$REG/makepad-widgets-1.0.0   $REG/makepad-draw-1.0.0   $REG/makepad-platform-1.0.0
$REG/dxf-0.6.1
# dxf's entity/table/header structs are CODE-GENERATED into OUT_DIR, not in src/:
target/debug/build/dxf-*/out/generated/{entities,tables,header,objects}.rs
```

## dxf 0.6.1 — confirmed by running code

* `Drawing::load_file(path) -> DxfResult<Drawing>`
* `drawing.entities()` / `.blocks()` / `.layers()` are **methods returning iterators**.
* `drawing.header` is a **field**: `.default_drawing_units` (`Units` enum, e.g. `Units::Millimeters`),
  `.minimum_drawing_extents` / `.maximum_drawing_extents` (`Point`).
* `Entity { common: EntityCommon, specific: EntityType }`.
  `common`: `.layer: String`, `.color: Color`, `.line_type_name: String` (`"BYLAYER"` default).
* `Color`: `.index() -> Option<u8>` (None for ByLayer(256)/ByBlock(0)), `.is_by_layer()`,
  `.is_by_block()`, `.raw_value` field.
* `Layer`: `.name`, `.color: Color`, `.line_type_name`.
* `Block`: `.name: String`, `.base_point: Point`, `.entities: Vec<Entity>` (a **field**, not a method).
* `EntityType` has 45 variants. Notable spellings:
  `Face3D`, `Solid3D`, `ModelPoint` (**this is POINT**), `LwPolyline`, `MText`, `Seqend`,
  `RotatedDimension` / `RadialDimension` / `DiameterDimension` /
  `AngularThreePointDimension` / `OrdinateDimension`, `XLine`, `Ray`, `Trace`, `Wipeout`.
* `Arc { center: Point, radius: f64, start_angle: f64, end_angle: f64, normal: Vector, .. }` —
  angles in **degrees**.
* `Circle { center, radius, normal, .. }`
* `Line { p1: Point, p2: Point, .. }`  (NOT `first_point`)
* `LwPolyline { flags: i32, constant_width: f64, thickness: f64,
   vertices: Vec<LwPolylineVertex>, extrusion_direction: Vector }`;
  `LwPolylineVertex { x, y, id, starting_width, ending_width, bulge }`. Closed = `flags & 1`.
* `Polyline` (old style): `.vertices()` **method** returning an iterator of `&Vertex`; `.flags: i32`.
* `Ellipse { center, major_axis: Vector, minor_axis_ratio: f64,
   start_parameter: f64, end_parameter: f64, normal }` — parameters in **radians**.
* `Spline { degree_of_curve: i32, control_points: Vec<Point>, knot_values: Vec<f64>,
   weight_values: Vec<f64> (NOT `weights`), fit_points: Vec<Point>, flags: i32, normal }`.
  Closed = `flags & 1`; planar = `flags & 8`.
* `Insert { name: String, location: Point, x_scale_factor, y_scale_factor, z_scale_factor,
   rotation (degrees), column_count, row_count, column_spacing, row_spacing, .. }`
* `Text { value: String, location: Point, text_height: f64, rotation: f64,
   horizontal_text_justification: HorizontalTextJustification,
   vertical_text_justification: VerticalTextJustification,
   second_alignment_point: Point, .. }`
* `MText { text: String, insertion_point: Point, initial_text_height: f64,
   attachment_point: AttachmentPoint, .. }`
* `Solid`/`Face3D`/`Trace`: `.first_corner .second_corner .third_corner .fourth_corner`.
  NB: for SOLID/TRACE the 3rd and 4th corners are **swapped** relative to draw order.

## makepad 1.0 — confirmed by reading source

* `DrawQuad` (`makepad-draw/src/shader/draw_quad.rs`) is the base of every 2D shader.
  `#[repr(C)]`, `#[deref] draw_vars: DrawVars`, then `#[calc] rect_pos: Vec2`,
  `#[calc] rect_size: Vec2`, `#[calc] draw_clip: Vec4`, `#[live] depth_clip`, `#[live] draw_depth`.
  Every `#[calc]`/`#[live]` scalar field after `draw_vars` becomes a per-**instance** shader var.
* Instancing fast path — this is how to draw 100k+ primitives:
  ```rust
  self.draw_seg.begin_many_instances(cx);
  for s in segments { self.draw_seg.p0 = ..; self.draw_seg.draw_abs(cx, rect); }
  self.draw_seg.end_many_instances(cx);
  ```
  `draw_abs` → `draw()` → when `many_instances` is `Some`, appends `draw_vars.as_slice()` to a
  `Vec<f32>` instead of allocating a draw call. Cheap.
* `DrawLine` exists (`shader/draw_line.rs`) but its `draw_line_abs` splits **each** line into up to
  `ceil(inner/scaledup)` quads. Unusable at CAD scale — write a custom one-quad-per-segment shader.
* Retained mode: `DrawList2d::begin(cx, walk) -> Redrawing`. If the list already has draw items and
  nothing marked it dirty, it returns `Redrawing::No` and you **skip** emitting geometry entirely
  (`draw_list_2d.rs:82`). A static view costs zero CPU; only `redraw()` forces re-emission.
* Shader vars available in `fn vertex()`: `self.geom_pos`, `self.rect_pos`, `self.rect_size`,
  `self.draw_clip`, `self.view_shift`, `self.view_clip`, `self.view_transform`,
  `self.camera_view`, `self.camera_projection`, `self.draw_depth`, `self.draw_zbias`.
  `DrawQuad` provides `clip_and_transform_vertex(rect_pos, rect_size)`; override `fn vertex()` to
  build your own geometry, and `fn pixel()` for shading.
