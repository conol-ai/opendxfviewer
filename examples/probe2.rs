fn main() {
    let f = std::env::args().nth(1).unwrap();
    let d = opendxfviewer::read::load(&f).unwrap();
    let names: Vec<&str> = d.layers().map(|l| l.name.as_str()).collect();
    println!("layers: {names:?}");
}
