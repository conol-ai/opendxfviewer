# Makepad notes

Makepad 1.0 has almost no published examples, and several of its rules fail at *runtime* with no
compiler help — or with no message at all. Everything below was verified against
`makepad-widgets 1.0.0` on this project; each entry says what actually happens when you get it
wrong, because the symptom is usually not the cause.

If you are changing `src/canvas.rs`, `src/layers.rs` or `src/app.rs`, read this first.

## The DSL is checked at runtime, not at compile time

`live_design! { }` compiles as an opaque token blob. A malformed block builds cleanly and then
prints an error at startup — and if you are not looking at stdout, the window simply comes up
blank or the app appears to hang.

**Always launch the binary after editing a `live_design!` block.** The reported position is a byte
offset into the macro text, not a source line, unless you build with
`MAKEPAD=lines cargo +nightly build`.

## `live_design!` must not live in `src/main.rs`

The macro rewrites `crate::` to the defining module's id, and `module_path!()` at a crate root is a
single segment whose id is `LiveId(0)`. A DSL block at the crate root therefore cannot resolve
`use crate::…`.

Symptom: a clean build, then `Can't find live definition of X — did you forget to call live_design
for it` followed by `target class not found`.

This is why `main.rs` is a three-line stub and everything lives in `app.rs`.

## Registration is manual and silent when missing

`#[derive(Live)]` generates `live_design_with`, which calls `LiveRegister::live_register`. Nothing
walks the module tree for you. Every module with a `live_design!` block must be listed:

```rust
impl LiveRegister for App {
    fn live_register(cx: &mut Cx) {
        crate::makepad_widgets::live_design(cx);
        crate::canvas::live_design(cx);
        crate::layers::live_design(cx);
    }
}
```

Miss one and you get `target class not found` at runtime, with a clean compile.

Only `pub Name = …` entries in a DSL block are visible to another module's `use crate::mod::*`.

## `:` and `=` mean different things

`foo: <View> {}` sets a **field**, and only works if the parent struct has a live field called
`foo` (`Splitter`'s `a` and `b`, `View`'s `scroll_bars`). `foo = <View> {}` creates a **named
child** addressable by `id!(foo)`.

You cannot combine them. `b: canvas = <DxfCanvas> {}` is a parse error
(*Unexpected token = in class body*), which is why the canvas sits inside a wrapper view:

```
b: <View> { canvas = <DxfCanvas> { width: Fill, height: Fill } }
```

## `#[derive(Widget)]` does not implement `Widget`

It implements `WidgetNode` and `LiveRegister` and generates the `Ref` helper types. You write
`impl Widget for X` yourself.

It also requires at least one `#[redraw]`, `#[deref]` or `#[wrap]` field, or the derive fails with
*Need either a field marked redraw or deref or wrap to find redraw method*. `#[redraw] #[rust]
area: Area` is the minimum.

`WidgetMatchEvent::handle_actions` is **not** dispatched for you — call
`self.widget_match_event(cx, event, scope)` from `handle_event`, or your buttons silently do
nothing.

## The `Live` derive cannot parse `::` in a field type

`#[rust] style: render::Style` fails with *Unexpected field form*, pointing at the `derive` line
rather than the field. Import the type and name it bare: `#[rust] style: Style`.

Every `#[rust]` field must implement `Default`, or you get an `E0277` on the derive line. Use
`#[rust(expr)]` for a different initial value.

`#[walk]` and `#[layout]` are **splats**: in the DSL you write `width: Fill, flow: Down` at the
widget's top level, not `walk: { width: Fill }`.

## `derive(LiveHook)` on a shader struct renders nothing, silently

`#[derive(LiveHook)]` generates an *empty* impl. A struct with `#[deref] draw_vars: DrawVars` needs
`before_apply` / `after_apply` to initialise its shader; without them `draw_vars.draw_shader` stays
`None`, nothing draws, and **nothing is logged**. Both `DrawSeg` and `DrawTri` write them by hand —
see `canvas.rs`.

## Uniforms are declared in the DSL, instances are struct fields

Every field after `#[deref] draw_vars: DrawVars` becomes a per-**instance** attribute, whatever
attribute you put on it. `#[live] view_offset: Vec2` is an instance, not a uniform. A real uniform
is `uniform view_offset: vec2` inside the `live_design!` block.

That matters for batching: a uniform difference **splits the draw call**, and
`find_appendable_drawcall` compares all 256 uniform slots per candidate. Per-entity data such as
colour and line width must be an instance, or emission becomes quadratic.

## `cx.begin_many_instances`, not `DrawQuad::begin_many_instances`

`DrawQuad`'s method uses the *aligned* variant, which registers an align entry. Every enclosing
`end_turtle` then rewrites four floats of `draw_clip` per instance. At half a million segments that
scatter dominates the frame.

`canvas.rs` calls `cx.begin_many_instances(&draw_vars)` (reached through `Deref` on `Cx2d`) and
sets `draw_clip` itself, once, before the loop.

## Makepad is retained-mode

`draw_walk` runs only when something called `redraw`. Between redraws the emitted instance buffers
stay alive on both sides. A view nobody is touching costs nothing.

Two consequences the code depends on:

- Re-emitting the whole batch on every view change is affordable, because "every view change" is
  far rarer than "every frame".
- Anything that redraws unconditionally destroys that. Never read `self.time` in a shader: it sets
  `uses_time`, which force-dirties every pass every frame, forever.

An action emitted from inside `draw_walk` is **not reliably dispatched**. `app.rs` polls the
camera's scale on each event instead of listening for one, because zoom-to-fit can only run inside
the draw pass.

## Input

There is no `Event::FingerDown`. Those are `Hit` variants, reached through `event.hits(cx, area)`,
and `hits` returns `Nothing` until `area` has been populated by a draw pass.

- `Hit::KeyDown` never arrives without `cx.set_key_focus(self.area)` — do it on `Hit::FingerDown`.
- `Hit::FingerScroll` carries no `handled` flags, so a canvas inside a scrolling view would zoom
  *and* scroll. Match the raw `Event::Scroll` first and set `handled_x` / `handled_y`.
- `FingerMoveEvent::abs_start` is where the press landed, so its delta is cumulative. Pan from a
  snapshot taken at `FingerDown`; accumulating per event drifts.
- On macOS wheel deltas are pre-multiplied by 32 while trackpad deltas are raw; `e.is_mouse`
  distinguishes them, and it is only available on the raw event.
- `MouseCursor::ZoomIn` and friends do not exist — they are inside a block comment in the source.

## Files

`Cx::open_system_openfile_dialog()` exists, compiles, and does nothing on macOS but print to
stdout. There is no result event. Use `rfd`.

`rfd` must run on the **main thread**: Makepad never calls `[NSApp run]`, so `NSApplication
isRunning` is false and `rfd` panics from a worker believing it is headless. Call it from
`handle_actions`. Parsing, which is the slow part, goes the other way — onto a worker, returning
through `UiRunner::defer`, which only runs if `self.ui_runner().handle(...)` is pumped every event.

Drag and drop is **not** routed by `MatchEvent`; match `Event::Drag` / `Event::Drop` yourself. A
drop never arrives unless you write `DragResponse::Copy` into the drag event's response mutex, and
dropped paths arrive with percent escapes intact.

## Widgets

- `CheckBoxRef::set_text` takes no `cx`. Every other `Ref`'s `set_text` does.
- `Splitter` is not in the prelude; `axis: Horizontal` means panes side by side, i.e. a *vertical*
  divider.
- A `Name = <Class> {}` child of a `PortalList` is a **template**, never drawn. Instantiate it with
  `list.item(cx, i, live_id!(Name))` — `live_id!`, not `id!` — call `set_item_range` before the
  `next_visible_item` loop, and `item.draw_all(...)` inside it or the rows come out blank.
- A `PortalList` step loop must live in a `#[derive(Widget)]` component's `draw_walk`. With
  `ui: <Root>`, `Root` builds the `Cx2d` itself and you cannot get one in `AppMain::handle_event`.
- `Label` has no background; wrap it in a `View` if you need one.

## Types

`Vec4` and everything in a shader is `f32`. `DVec2` and `Rect` are `f64`. `vec2()` and `dvec2()`
are different constructors, and mixing them is a type error with a confusing span.
