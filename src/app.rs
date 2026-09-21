//! The application shell: window chrome, file loading, and the wiring between them.
//!
//! File loading is split across threads deliberately. The open dialog has to run on the main
//! thread — `rfd` spins its own modal loop and Makepad's event pump is simply blocked while it
//! does — but parsing and converting a large drawing takes hundreds of milliseconds, so that part
//! is handed to a worker and posted back through Makepad's `UiRunner`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use makepad_widgets::*;

use crate::canvas::{DxfCanvasAction, DxfCanvasWidgetRefExt};
use crate::convert;
use crate::layers::{LayerItem, LayerList, LayerPanelAction};
use crate::render;
use crate::scene::{Scene, Stats, Units};

live_design! {
    use link::theme::*;
    use link::shaders::*;
    use link::widgets::*;
    use crate::canvas::*;
    use crate::layers::*;

    Chip = <View> {
        width: Fit, height: Fill,
        align: { y: 0.5 },
        padding: { left: 9.0, right: 9.0 }
    }

    Dim = <Label> { draw_text: { color: #85858f, text_style: { font_size: 8.5 } } }

    App = {{App}} {
        ui: <Root> {
            main_window = <Window> {
                window: { title: "opendxfviewer", inner_size: vec2(1280, 860) },
                pass: { clear_color: #131316 }

                // Showing Makepad's own caption bar keeps the toolbar clear of the macOS
                // window buttons, which otherwise sit on top of the first control.
                caption_bar = {
                    visible: true,
                    draw_bg: { color: #202024 }
                    caption_label = {
                        label = <Label> {
                            text: "opendxfviewer",
                            draw_text: { color: #85858f }
                        }
                    }
                }

                body = <View> {
                    flow: Down,
                    show_bg: true,
                    draw_bg: { color: #131316 }

                    <View> {
                        width: Fill, height: 36.0,
                        flow: Right, spacing: 6.0,
                        align: { y: 0.5 },
                        padding: { left: 8.0, right: 8.0 },
                        show_bg: true,
                        draw_bg: { color: #202024 }

                        open_btn = <Button> { text: "Open…" }
                        fit_btn = <Button> { text: "Fit" }
                        zoom_in_btn = <Button> { text: "+" }
                        zoom_out_btn = <Button> { text: "−" }
                        <View> { width: 10.0, height: Fill }
                        lw_check = <CheckBox> { text: "Line weights" }
                        lt_check = <CheckBox> { text: "Dashes" }
                        bg_check = <CheckBox> { text: "Light" }
                        <View> { width: Fill, height: Fill }
                        title_label = <Label> {
                            text: "Drop a DXF file here, or press Open",
                            draw_text: { color: #b0b0b8 }
                        }
                    }

                    <Splitter> {
                        width: Fill, height: Fill,
                        // Counter-intuitively, a Horizontal splitter lays its panes out left to
                        // right, i.e. the divider itself is vertical.
                        axis: Horizontal,
                        align: FromA(210.0),
                        // `a`/`b` are Splitter *fields*, so they take `:` and cannot themselves
                        // be named. A wrapper view carries the name the app looks the canvas up by.
                        a: <LayerPanel> {},
                        b: <View> {
                            width: Fill, height: Fill,
                            canvas = <DxfCanvas> { width: Fill, height: Fill }
                        }
                    }

                    <View> {
                        width: Fill, height: 25.0,
                        flow: Right,
                        align: { y: 0.5 },
                        show_bg: true,
                        draw_bg: { color: #202024 }

                        <Chip> { coords_label = <Dim> { text: "" } }
                        <Chip> { zoom_label = <Dim> { text: "" } }
                        <Chip> { stats_label = <Dim> { text: "" } }
                        <View> { width: Fill, height: Fill }
                        <Chip> {
                            warn_label = <Label> {
                                text: "",
                                draw_text: { color: #d8a657, text_style: { font_size: 8.5 } }
                            }
                        }
                    }
                }
            }
        }
    }
}

app_main!(App);

/// The file named on the command line, resolved while the working directory is still the one the
/// command was run from.
static STARTUP_FILE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Starts the viewer.
///
/// Makepad resolves every resource it draws with — its fonts, its icons — against a path baked in
/// at compile time, which points into the cargo registry of whichever machine built the binary. A
/// release package is built with `MAKEPAD_PACKAGE_DIR` set to a relative root and ships a copy of
/// those files under it, so the lookup becomes relative to the working directory instead. That
/// only finds them if the working directory is the one the package was unpacked into, so a
/// packaged build moves there before Makepad starts — after resolving the file argument, which the
/// user gave relative to wherever they actually were.
///
/// A plain `cargo build` bakes in paths that are correct on this machine, so there is nothing to
/// move to and only the argument is resolved.
pub fn run() {
    let arg = std::env::args().nth(1).map(PathBuf::from);
    // `join` returns an absolute argument unchanged, so this is only a change for relative ones.
    let arg = match std::env::current_dir() {
        Ok(cwd) => arg.map(|path| cwd.join(path)),
        Err(_) => arg,
    };
    let _ = STARTUP_FILE.set(arg);

    if option_env!("MAKEPAD_PACKAGE_DIR").is_some() {
        if let Some(dir) = std::env::current_exe().ok().as_deref().and_then(Path::parent) {
            let _ = std::env::set_current_dir(dir);
        }
    }

    app_main();
}

/// Falls back to reading the argument directly for anything that starts the app without going
/// through [`run`], such as an example.
fn startup_file() -> Option<PathBuf> {
    STARTUP_FILE.get().cloned().unwrap_or_else(|| std::env::args().nth(1).map(PathBuf::from))
}

#[derive(Live, LiveHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    /// Row data for the layer panel, snapshotted when a drawing loads so the panel never has to
    /// reach into the canvas's scene.
    #[rust]
    layer_list: LayerList,
    #[rust]
    stats: Stats,
    #[rust]
    units: Units,
    #[rust]
    layer_count: usize,
    #[rust]
    file_name: String,
    #[rust(true)]
    dark: bool,
    /// Kept so a background or quality change can rebuild the scene without a re-read.
    #[rust]
    source: Option<PathBuf>,
    #[rust]
    loading: bool,
    /// A file requested while another was still loading. Dropping the request instead would make
    /// the Light toggle silently do nothing on a large drawing, leaving the tick box contradicting
    /// what is on screen.
    #[rust]
    queued: Option<PathBuf>,
    /// Opening the dialog on the very first event is unsafe on macOS: the app has not finished
    /// launching and there is no key window yet. A CLI argument is deferred through this.
    #[rust]
    startup_open: Option<PathBuf>,
    #[rust]
    startup_timer: Timer,
    /// Last zoom level written to the status bar. Zoom-to-fit can only run inside the draw pass,
    /// where an emitted action is not reliably dispatched, so the scale is polled instead.
    #[rust]
    shown_scale: f64,
}

impl LiveRegister for App {
    fn live_register(cx: &mut Cx) {
        crate::makepad_widgets::live_design(cx);
        crate::canvas::live_design(cx);
        crate::layers::live_design(cx);
    }
}

/// What a worker thread hands back.
enum Loaded {
    Ok(Box<Scene>, String, PathBuf),
    /// The message, and the name of the file it is about.
    Err(String, String),
}

impl App {
    /// Read, parse and convert a file on a worker thread.
    fn open(&mut self, cx: &mut Cx, path: PathBuf) {
        if self.loading {
            // Keep only the newest request: if the user toggles twice while a big file is
            // parsing, the second toggle is what they actually want.
            self.queued = Some(path);
            return;
        }
        self.loading = true;
        let shown = file_label(&path);
        self.ui.label(id!(title_label)).set_text(cx, &format!("Loading {shown}…"));
        self.set_warning(cx, "");

        let opts = convert::Options { dark_background: self.dark, ..convert::Options::default() };
        let runner = self.ui_runner();
        std::thread::spawn(move || {
            let name = file_label(&path);
            let result = match load_blocking(&path, &opts) {
                Ok(scene) => Loaded::Ok(Box::new(scene), name, path.clone()),
                Err(msg) => Loaded::Err(msg, name),
            };
            runner.defer(move |app: &mut App, cx: &mut Cx, _scope| app.on_loaded(cx, result));
        });
    }

    fn on_loaded(&mut self, cx: &mut Cx, result: Loaded) {
        self.loading = false;
        // A request that arrived mid-load supersedes this result: start it and let it land.
        if let Some(next) = self.queued.take() {
            self.open(cx, next);
            return;
        }
        match result {
            Loaded::Err(msg, name) => {
                // Name the file that failed. Falling back to the previously loaded one, or to
                // nothing at all, leaves the message floating with no subject.
                self.ui.label(id!(title_label)).set_text(cx, &name);
                self.set_warning(cx, &msg);
            }
            Loaded::Ok(scene, name, path) => {
                self.file_name = name;
                self.source = Some(path);
                let scene = *scene;
                self.layer_list = LayerList(
                    scene
                        .layers
                        .iter()
                        .map(|l| LayerItem {
                            name: l.name.clone(),
                            color: l.color,
                            count: l.count,
                            visible: l.visible,
                            on_in_file: l.visible_in_file,
                        })
                        .collect(),
                );
                self.stats = scene.stats.clone();
                self.units = scene.units;
                self.layer_count = scene.layers.len();
                let warning = first_warning(&scene);
                if let Some(mut c) = self.ui.dxf_canvas(id!(canvas)).borrow_mut() {
                    c.set_scene(cx, scene);
                }
                self.ui.label(id!(title_label)).set_text(cx, &self.file_name.clone());
                self.set_warning(cx, &warning);
                self.refresh_status(cx);
                self.ui.redraw(cx);
            }
        }
    }

    fn refresh_status(&mut self, cx: &mut Cx) {
        let loaded = self.stats.entities_read > 0;
        let stats = if loaded {
            let skipped = if self.stats.entities_skipped > 0 {
                format!(" · {} not drawn", self.stats.entities_skipped)
            } else {
                String::new()
            };
            format!(
                "{} entities · {} primitives · {} layers{skipped} · {} ms",
                self.stats.entities_read,
                self.stats.primitives,
                self.layer_count,
                self.stats.load_ms
            )
        } else {
            String::new()
        };
        self.ui.label(id!(stats_label)).set_text(cx, &stats);

        let scale = self.ui.dxf_canvas(id!(canvas)).borrow().map(|c| c.camera().scale);
        let zoom = match scale {
            Some(scale) if loaded => {
                let unit = self.units.suffix();
                if unit.is_empty() {
                    format!("{} px/unit", Zoom(scale))
                } else {
                    format!("{} px/{unit}", Zoom(scale))
                }
            }
            _ => String::new(),
        };
        self.ui.label(id!(zoom_label)).set_text(cx, &zoom);
    }

    /// Colours are resolved at conversion time, so the panel's swatches follow the background.
    fn set_warning(&mut self, cx: &mut Cx, text: &str) {
        self.ui.label(id!(warn_label)).set_text(cx, text);
    }

    /// Re-run the conversion, which is what resolves ACI colours against the background.
    fn retheme(&mut self, cx: &mut Cx) {
        if let Some(path) = self.source.clone() {
            self.open(cx, path);
        }
    }

    fn with_canvas(&self, cx: &mut Cx, f: impl FnOnce(&mut crate::canvas::DxfCanvas, &mut Cx)) {
        if let Some(mut c) = self.ui.dxf_canvas(id!(canvas)).borrow_mut() {
            f(&mut c, cx);
        }
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        // Dashes are on by default, so the box has to start ticked or it contradicts the drawing.
        self.ui.check_box(id!(lt_check)).set_active(cx, render::Style::default().use_linetypes);
        if let Some(path) = startup_file() {
            // Opening straight from here races the window's own creation, so wait one tick.
            self.startup_open = Some(path);
            self.startup_timer = cx.start_timeout(0.0);
        }
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.ui.button(id!(open_btn)).clicked(actions) {
            self.pick_file(cx);
        }
        if self.ui.button(id!(fit_btn)).clicked(actions) {
            self.with_canvas(cx, |c, cx| c.zoom_to_fit(cx));
            self.refresh_status(cx);
        }
        if self.ui.button(id!(zoom_in_btn)).clicked(actions) {
            self.with_canvas(cx, |c, cx| c.zoom_by(cx, 1.4));
            self.refresh_status(cx);
        }
        if self.ui.button(id!(zoom_out_btn)).clicked(actions) {
            self.with_canvas(cx, |c, cx| c.zoom_by(cx, 1.0 / 1.4));
            self.refresh_status(cx);
        }
        if let Some(on) = self.ui.check_box(id!(lw_check)).changed(actions) {
            self.with_canvas(cx, |c, cx| {
                c.style_mut().use_lineweights = on;
                c.redraw(cx);
            });
        }
        if let Some(on) = self.ui.check_box(id!(lt_check)).changed(actions) {
            self.with_canvas(cx, |c, cx| {
                c.style_mut().use_linetypes = on;
                c.redraw(cx);
            });
        }
        if let Some(light) = self.ui.check_box(id!(bg_check)).changed(actions) {
            self.dark = !light;
            // The sheet and the geometry have to move together, or the drawing goes black on
            // black. The canvas repaints immediately; the colours follow when the reconvert lands.
            let dark = self.dark;
            self.with_canvas(cx, |c, cx| c.set_dark_background(cx, dark));
            self.retheme(cx);
        }

        for action in actions {
            let Some(a) = action.as_widget_action() else { continue };
            match a.cast::<DxfCanvasAction>() {
                DxfCanvasAction::Hover(w) => {
                    let unit = self.units.suffix();
                    let txt = if unit.is_empty() {
                        format!("{:.4}, {:.4}", w.x, w.y)
                    } else {
                        format!("{:.4}, {:.4} {unit}", w.x, w.y)
                    };
                    self.ui.label(id!(coords_label)).set_text(cx, &txt);
                }
                DxfCanvasAction::ViewChanged => self.refresh_status(cx),
                DxfCanvasAction::None => {}
            }
            match a.cast::<LayerPanelAction>() {
                LayerPanelAction::SetVisible(row, on) => {
                    if let Some(l) = self.layer_list.0.get_mut(row) {
                        l.visible = on;
                    }
                    self.with_canvas(cx, |c, cx| c.set_layer_visible(cx, row, on));
                }
                LayerPanelAction::ShowAll => {
                    for l in &mut self.layer_list.0 {
                        l.visible = true;
                    }
                    self.with_canvas(cx, |c, cx| c.show_all_layers(cx));
                    self.ui.redraw(cx);
                }
                LayerPanelAction::None => {}
            }
        }
    }
}

impl AppMain for App {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        // Deferred closures from worker threads only run if the runner is pumped.
        self.ui_runner().handle(cx, event, &mut Scope::empty(), self);

        let scale = self.ui.dxf_canvas(id!(canvas)).borrow().map(|c| c.camera().scale);
        if let Some(scale) = scale {
            if scale != self.shown_scale {
                self.shown_scale = scale;
                self.refresh_status(cx);
            }
        }

        if self.startup_timer.is_event(event).is_some() {
            self.startup_timer = Timer::empty();
            if let Some(p) = self.startup_open.take() {
                self.open(cx, p);
            }
        }

        // Drag and drop is not routed by MatchEvent, so it is handled here. Claiming the drag
        // with DragResponse::Copy is mandatory: without it macOS never delivers the drop.
        match event {
            Event::Drag(de) => {
                let wanted = de
                    .items
                    .iter()
                    .any(|i| matches!(i, DragItem::FilePath { path, .. } if is_droppable(path)));
                if wanted {
                    if let Ok(mut r) = de.response.lock() {
                        *r = DragResponse::Copy;
                    }
                }
            }
            Event::Drop(de) => {
                let first = de.items.iter().find_map(|i| match i {
                    DragItem::FilePath { path, internal_id: None } if is_droppable(path) => {
                        Some(percent_decode(path))
                    }
                    _ => None,
                });
                if let Some(p) = first {
                    self.open(cx, PathBuf::from(p));
                }
            }
            Event::KeyDown(ke)
                if ke.key_code == KeyCode::KeyO && ke.modifiers.logo | ke.modifiers.control =>
            {
                self.pick_file(cx);
            }
            _ => {}
        }

        self.match_event(cx, event);
        // The layer panel reads its rows out of the scope rather than borrowing the canvas.
        let mut layers = std::mem::take(&mut self.layer_list);
        let mut scope = Scope::with_data(&mut layers);
        self.ui.handle_event(cx, event, &mut scope);
        self.layer_list = layers;
    }
}

impl App {
    /// Show the native open dialog.
    ///
    /// This blocks: `NSOpenPanel::runModal` runs its own event loop while Makepad's is stalled.
    /// It has to happen on the main thread — `rfd` panics from a worker because Makepad never
    /// calls `[NSApp run]`, so `isRunning()` is false and `rfd` believes it is headless.
    fn pick_file(&mut self, cx: &mut Cx) {
        let mut d =
            rfd::FileDialog::new().set_title("Open DXF").add_filter("DXF drawing", &["dxf", "DXF"]);
        if let Some(dir) = self.source.as_ref().and_then(|p| p.parent()) {
            d = d.set_directory(dir);
        }
        if let Some(path) = d.pick_file() {
            self.open(cx, path);
        }
    }
}

fn load_blocking(path: &Path, opts: &convert::Options) -> Result<Scene, String> {
    let started = std::time::Instant::now();
    let drawing = crate::read::load(path)?;
    let mut scene = convert::convert(&drawing, opts);
    scene.stats.load_ms = started.elapsed().as_millis() as u32;
    Ok(scene)
}

fn file_label(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Extensions the window accepts on a drop.
///
/// `.dwg` is here despite not being readable: rejecting it at the drop means the file bounces
/// with no explanation at all, and "nothing happened" is a worse answer than "this is a DWG,
/// convert it first". The reader recognises it and says so.
fn is_droppable(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("dxf") || e.eq_ignore_ascii_case("dwg"))
}

/// Makepad hands over the dropped URL with `file://` stripped but percent escapes intact, so a
/// path containing a space arrives as `%20` and will not open.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Formats a zoom factor without a wall of digits at either extreme.
struct Zoom(f64);

impl std::fmt::Display for Zoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let z = self.0;
        if !(0.001..1000.0).contains(&z) {
            write!(f, "{z:.2e}")
        } else if z >= 10.0 {
            write!(f, "{z:.0}")
        } else if z >= 1.0 {
            write!(f, "{z:.2}")
        } else {
            write!(f, "{z:.4}")
        }
    }
}

fn first_warning(s: &Scene) -> String {
    if let Some(w) = s.stats.warnings.first() {
        let more = s.stats.warnings.len() - 1;
        return if more > 0 { format!("{w}  (+{more} more)") } else { w.clone() };
    }
    if !s.stats.unsupported.is_empty() {
        let total: usize = s.stats.unsupported.iter().map(|(_, n)| n).sum();
        let kinds: Vec<&str> =
            s.stats.unsupported.iter().map(|(k, _)| k.as_str()).take(3).collect();
        return format!("{total} entities not drawn: {}", kinds.join(", "));
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_paths_are_percent_decoded() {
        assert_eq!(percent_decode("/Users/x/My%20Drawings/a.dxf"), "/Users/x/My Drawings/a.dxf");
        assert_eq!(percent_decode("/plain/path.dxf"), "/plain/path.dxf");
        // A stray percent at the end must not panic or eat characters.
        assert_eq!(percent_decode("/odd/100%"), "/odd/100%");
        assert_eq!(percent_decode("/odd/%zz"), "/odd/%zz");
        // Multi-byte UTF-8 survives.
        assert_eq!(percent_decode("/a/%E5%9B%B3%E9%9D%A2.dxf"), "/a/図面.dxf");
    }

    #[test]
    fn a_drop_accepts_drawings_and_nothing_else() {
        for ok in ["/a/b.dxf", "/a/b.DXF", "/a/b.Dxf"] {
            assert!(is_droppable(ok), "{ok}");
        }
        // DWG is accepted so the reader can name it. Silently refusing the drop tells the user
        // nothing, which is the worse failure.
        for ok in ["/a/b.dwg", "/a/b.DWG"] {
            assert!(is_droppable(ok), "{ok}");
        }
        for no in ["/a/b.pdf", "/a/dxf", "/a/b.dxf.zip", "/a/b", "/a/b.step"] {
            assert!(!is_droppable(no), "{no}");
        }
    }

    #[test]
    fn zoom_is_readable_at_every_magnitude() {
        assert_eq!(Zoom(1234.0).to_string(), "1.23e3");
        assert_eq!(Zoom(42.0).to_string(), "42");
        assert_eq!(Zoom(3.5).to_string(), "3.50");
        assert_eq!(Zoom(0.25).to_string(), "0.2500");
        assert_eq!(Zoom(1e-9).to_string(), "1.00e-9");
    }

    #[test]
    fn warnings_summarise_rather_than_flood() {
        let mut s = Scene::default();
        assert_eq!(first_warning(&s), "");
        s.stats.unsupported = vec![("HATCH".into(), 3), ("IMAGE".into(), 1)];
        assert_eq!(first_warning(&s), "4 entities not drawn: HATCH, IMAGE");
        s.stats.warnings = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(first_warning(&s), "a  (+2 more)");
    }
}
