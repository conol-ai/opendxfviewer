const USAGE: &str = "\
opendxfviewer — a viewer for DXF drawings

USAGE:
    opendxfviewer [FILE.dxf]

With no file, the window opens empty; use the Open button, Cmd/Ctrl+O, or drop a
.dxf file onto it.

    -h, --help       print this message
    -V, --version    print the version
";

fn main() {
    // The window is the product, so anything that is not a flag is treated as a path and left to
    // the app to report on. Only the two conventional flags short-circuit.
    match std::env::args().nth(1).as_deref() {
        Some("-h" | "--help") => print!("{USAGE}"),
        Some("-V" | "--version") => println!("opendxfviewer {}", env!("CARGO_PKG_VERSION")),
        _ => opendxfviewer::app::app_main(),
    }
}
