# opendxfviewer

[![CI](https://github.com/conol-ai/opendxfviewer/actions/workflows/ci.yml/badge.svg)](https://github.com/conol-ai/opendxfviewer/actions/workflows/ci.yml)

A fast, open source viewer for DXF drawings, written in Rust with
[Makepad](https://makepad.dev).

It opens a DXF file, draws it, and lets you move around it. That is the whole
scope: there is no editing, no conversion, and no CAD kernel. What it aims to do
well is show you what is actually in the file — including the parts most viewers
quietly drop.

**DXF only — it does not read DWG.** DWG is Autodesk's closed binary format and
needs a different parser entirely; export or convert to DXF first. Hand it a
DWG and it will say so rather than failing with a parse error.

![opendxfviewer showing a drawing with its layer panel](docs/screenshot.png)

## Install and run

Take a package for your platform from
[Releases](https://github.com/conol-ai/opendxfviewer/releases), or build it
yourself:

```sh
cargo install opendxfviewer            # from crates.io
cargo run --release -- drawing.dxf     # from a clone; or just `cargo run --release`
```

A released package carries Makepad's fonts next to the executable, since Makepad
resolves them by path rather than embedding them, so keep the archive's layout
instead of moving the binary out on its own. The macOS app is signed ad-hoc
rather than notarised, so the first launch needs right-click → **Open**.

A drawing can also be opened with the **Open…** button, `Cmd`/`Ctrl`+`O`, or by
dropping a `.dxf` file onto the window.

## Using it

| | |
|---|---|
| Pan | drag, or the arrow keys |
| Zoom | scroll wheel or trackpad, at the cursor |
| Zoom to fit | `F`, `Home`, or the **Fit** button |
| Zoom step | `+` / `−` |
| Layers | toggle in the left panel; **All** brings everything back |
| Line weights | toggle in the toolbar to draw the file's stored widths |
| Dashes | toggle to draw the file's linetypes, or force everything solid |
| Background | toggle **Light** for a paper sheet; colours re-resolve to suit it |

The status bar shows the cursor position in drawing units, the current scale,
how much of the file was drawn, and anything that could not be.

## What it draws

`LINE`, `CIRCLE`, `ARC`, `ELLIPSE`, `LWPOLYLINE` (with bulges), `POLYLINE`,
`SPLINE` (rational and periodic NURBS, and fit-point-only splines), `POINT`,
`INSERT` including nested, mirrored, non-uniformly scaled and `MINSERT` arrays,
`TEXT`, `MTEXT`, `ATTRIB`, `SOLID`, `TRACE`, `3DFACE`, `LEADER`, `MLINE`,
`RAY`, `XLINE`, and dimensions via their pre-rendered geometry blocks.

These DXF details are handled once, at load, so the renderer never sees them:

- **Text encoding.** DXF moved to UTF-8 at R2007; older files use the code page
  in `$DWGCODEPAGE`. The `dxf` crate's `load_file` hardcodes Windows-1252, so
  every non-ASCII layer name and text entity in a modern file comes back
  mangled. `read.rs` picks the encoding from the header instead.

- **OCS → WCS.** Planar entities store coordinates in a plane defined by an
  extrusion vector; the arbitrary axis algorithm turns that into a basis. The
  view is an orthographic projection down world Z, so a circle drawn in a plane
  perpendicular to the screen correctly collapses to a line rather than
  silently becoming a circle in the wrong place.
- **Blocks.** `INSERT` references are expanded in place with their full
  transform, guarded against self-reference and unbounded nesting.
- **Colour and layer inheritance.** `ByLayer`, `ByBlock`, and the rule that
  entities drawn on layer `0` inside a block adopt the layer of the `INSERT`
  that placed them. `ByBlock` resolves against the *nearest* enclosing
  reference, not the outermost one — a distinction that is easy to get
  backwards and is pinned by a test.
- **Curves.** Arcs, bulges, ellipses and splines all become polylines, at a
  tolerance relative to each curve's own size — and the curve definition is
  kept, so zooming in re-tessellates rather than magnifying the facets.

- **Text orientation.** A `TEXT` rotation, an `MTEXT` direction vector or
  flipped normal, the width factor, the backward and upside-down generation
  flags and a mirrored `INSERT` all reduce at load to a baseline angle, a
  stretch and one mirror bit.
  Makepad's text pipeline only lays text out upright, so the canvas lets it do
  that and then turns, stretches and mirrors every glyph about the run's
  anchor in the vertex shader. Labels in a mirrored block read mirrored, as
  they do in CAD.

- **Control codes.** `%%c`, `%%d` and `%%p` become Ø, ° and ±, so a bore
  callout reads as a bore callout, and MTEXT's `\U+XXXX` escapes become the
  characters they name. MTEXT's inline formatting is stripped to its text and
  line breaks.

- **Legibility.** DXF palettes assume a black sheet, so yellow and green are
  nearly invisible on a white one. Palette colours that would disappear against
  the current background are pulled back far enough to read, keeping their hue;
  true colours, which the author chose explicitly, are left as written.

- **Linetypes.** Dash patterns are stored in drawing units and scaled by both
  `$LTSCALE` and the entity's own multiplier, so how a dashed line looks depends
  on the zoom. Dashes run continuously along a polyline rather than restarting
  at each vertex, and a pattern whose whole cycle falls under a few pixels is
  drawn solid instead of as noise. Only the on-screen part of a span is stepped
  through: the pattern repeats, so the rest folds into the phase arithmetically
  rather than costing an iteration per dash nobody sees.

### Known limitations

- **`HATCH` is not drawn.** The `dxf` crate does not parse it, so the entity
  never reaches us. Hatched regions show their boundary only if the file also
  stores one as a separate entity.
- **Text is drawn in one font.** A `STYLE`'s font, its oblique angle, and the
  vertical-text flag are not honoured: every run uses the viewer's own face.
- **`MTEXT` formatting is stripped**, not rendered: line breaks are honoured,
  inline colour, font and stacking codes are removed.
- **Splines with only fit points** are approximated with a centripetal
  Catmull-Rom curve through those points. The authoring program's own
  interpolation is not recorded in the file, so no reader can reproduce it
  exactly; this one at least passes through every fit point.
- **`MLINE` is drawn as its centreline**, since the offsets live in the
  `MLINESTYLE` table.
- Anything else unsupported is **counted and reported in the status bar**
  rather than dropped silently.

### Files converted from DWG

DXF produced by a converter is not always what a CAD program would write, and
three cases are repaired on load rather than rejected:

- **32-bit fields written unsigned.** True colour is `0xC2RRGGBB`, above
  `i32::MAX`; read as signed it overflows and the file fails outright.
- **UTF-8 characters split across an MTEXT continuation.** LibreDWG chunks long
  text every 250 *bytes*, so a multi-byte character can be cut in half. The
  chunks concatenate into valid text, so the split is moved onto a character
  boundary.
- **Thumbnail previews.** A preview bitmap with an unrecognised header made the
  parser reject the whole drawing. It is a picture of the file; it is dropped.

### Hostile input

A viewer opens files it did not write, so malformed and deliberately hostile
DXF is a normal case rather than an edge case. Blocks that reference themselves
or each other are detected and reported; nesting stops at a depth limit;
conversion stops at four separate budgets — primitives, vertices, block-array
cells and retained text bytes — all latched so a runaway array stops *working*,
not merely stops emitting. They are separate because each catches something the
others cannot see: a block can expand to *nothing* (an empty block, a
zero-radius circle), so no output counter moves however many cells are laid
out; and text has no vertices, so a block of long labels under a large array
could copy its strings until memory ran out. Angles of
infinity, coordinates that overflow when subtracted, splines whose declared
degree exceeds their control points, and drawings with more layers than the
layer id can address are all handled rather than crashing or hanging. A 30000 x
30000 `MINSERT` array of a twenty-circle block converts in under two seconds
and half a gigabyte, with a warning saying it was cut short.

## Performance

Measured with `cargo run --release --example bench`, on an otherwise idle
Apple M-series laptop, against a generated 169,229-entity drawing
(`python3 tools/gen_fixtures.py --huge`). The idle part matters: under load the
same build reports three to four times these figures, so a number that looks
like a regression is worth re-taking on a quiet machine first.

| | |
|---|---|
| Parse | 190 ms |
| Convert to a scene | 48 ms |
| Frame, whole drawing on screen | 8.4 ms (530k segments) |
| Frame, zoomed in 4× | 2.3 ms |
| Frame, zoomed in 64× | 21 µs |
| Frame, zoomed in 1024× | 1.2 µs |

Three things make that work:

- **Makepad retains draw lists**, so `draw_walk` runs only on redraw. A view
  nobody is touching costs nothing; the numbers above are what a *change*
  costs, not a steady-state frame. The canvas owns its own draw list, which is
  what makes that true of it specifically — sharing the parent's meant a
  status-bar label following the cursor rebuilt the scene on every mouse-move.
- **A uniform grid culls by viewport**, so cost tracks what is on screen rather
  than the size of the drawing — which is why zooming in gets cheaper.
- **One instanced quad per segment**, emitted as raw floats into a single draw
  call. The vertex shader builds an oriented quad rather than a bounding box,
  so a long diagonal costs its own area in fragments instead of the square of
  its length.

Peak memory is dominated by the parser, not by the viewer: `dxf` builds about
1.7 KB of `Entity` per entity, so the 169k-entity file peaks near 300 MB during
load. The scene it converts to is 73 MB, most of which is 2.9M `f64` vertices.

Vertices stay `f64` all the way to screen space on purpose. DXF survey drawings
carry coordinates in the millions, where `f32` quantises to centimetres.

## Building

```sh
cargo test            # 182 tests, no GPU needed
cargo clippy --all-targets -- -D warnings
cargo run --release -- tests/fixtures/basic.dxf
```

Linux additionally needs `libx11-dev libxcursor-dev libgl1-mesa-dev
libasound2-dev`.

### Packaging

`python3 tools/package.py` writes the archive a release is made of into `dist/`:
a `.app` zip on macOS, a `.tar.gz` on Linux, a `.zip` on Windows, for whichever
machine it runs on. It is not a plain `cargo build` with a `tar` around it — it
sets `MAKEPAD_PACKAGE_DIR` and copies Makepad's fonts into the archive, because a
binary built any other way looks for those fonts in the *build* machine's cargo
registry and panics on a machine that has no such directory.

Pushing a `vX.Y.Z` tag runs that script on four runners — macOS appears twice
because Makepad cannot cross-compile between Apple architectures — attaches the
results to a GitHub release, and publishes to crates.io. The same workflow can be
run by hand from the Actions tab to get packages without tagging anything.
Publishing needs a `CARGO_REGISTRY_TOKEN` secret on the repository; without it the
release is still cut and only the crates.io step is skipped.

## Layout

| | |
|---|---|
| `geom.rs` | `f64` vectors, affine transforms, AABBs, segment clipping |
| `read.rs` | choosing a file's text encoding from its header |
| `aci.rs` | the AutoCAD Color Index palette, and keeping it legible |
| `tessellate.rs` | arcs, bulges, ellipses, NURBS → polylines |
| `convert.rs` | `dxf::Drawing` → `Scene`: OCS, blocks, colours, curves |
| `scene.rs` | the render model |
| `spatial.rs` | uniform-grid view culling |
| `camera.rs` | the world ↔ screen mapping |
| `render.rs` | scene + camera → a batch of screen-space primitives |
| `canvas.rs` | the Makepad widget: shaders, drawing, navigation |
| `layers.rs` | the layer panel |
| `app.rs` | window chrome and file loading |

`render.rs` is deliberately separate from `canvas.rs`: the decisions that govern
how a drawing looks — culling, decimation, line weights, when to re-tessellate a
curve — are then testable without a GPU.

If you are touching the Makepad side, read
[`docs/makepad-notes.md`](docs/makepad-notes.md) first. Several of Makepad 1.0's
rules fail at runtime with no compiler help, and a few fail with no message at
all; that file records the ones this project hit, and what each looks like when
you get it wrong.

### Test fixtures

`tests/fixtures/` holds hand-written ASCII DXF covering the cases that break
readers: bulged polylines, OCS extrusion normals including the `-Z` mirror and
the 1/64 threshold, nested and mirrored and arrayed `INSERT`s, rational and
periodic splines, every text justification, and deliberately degenerate
geometry (zero radii, zero-length lines, empty polylines, coordinates at 1e9).
Every entity type listed above appears in at least one fixture, so none of them
can quietly stop working.
Regenerate them with `python3 tools/gen_fixtures.py`; add `--all` for the two
large performance fixtures, which are not checked in.

## License

MIT. The AutoCAD Color Index table in `src/aci.rs` is transcribed from
[ezdxf](https://github.com/mozman/ezdxf), also MIT. Makepad and the rest of the
dependency tree are `MIT OR Apache-2.0`.

The released packages additionally contain the fonts Makepad's theme names —
IBM Plex Sans, Liberation Mono, LXGW WenKai, Noto Color Emoji and Font Awesome's
icon face, all under the SIL Open Font License. They are listed in
[`packaging/THIRD-PARTY-NOTICES.md`](packaging/THIRD-PARTY-NOTICES.md), which
ships inside each package. Nothing in this repository contains a font file.

DXF is a trademark of Autodesk, Inc. This project is not affiliated with
Autodesk.
