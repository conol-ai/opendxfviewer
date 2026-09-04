//! Times parse, convert and per-frame batching. Development aid.
use opendxfviewer::{
    camera::Camera,
    convert,
    geom::{v2, Aabb, V2},
    render,
};
use std::time::Instant;

fn main() {
    let f = std::env::args().nth(1).expect("usage: bench <file.dxf>");
    let t0 = Instant::now();
    let dr = dxf::Drawing::load_file(&f).unwrap();
    let parse = t0.elapsed();

    let t1 = Instant::now();
    let s = convert::convert(&dr, &convert::Options::default());
    let conv = t1.elapsed();

    println!("{f}");
    println!("  parse    {parse:>10.2?}  ({} entities)", s.stats.entities_read);
    println!(
        "  convert  {conv:>10.2?}  ({} polys, {} verts, {} dots, {} tris, {} texts)",
        s.polys.len(),
        s.verts.len(),
        s.dots.len(),
        s.tris.len(),
        s.texts.len()
    );
    println!("  memory   {:>10} MB verts", s.verts.len() * 16 / 1_000_000);

    let mut cam = Camera { view: Aabb::new(V2::ZERO, v2(1920.0, 1080.0)), ..Camera::default() };
    cam.fit(&s.bounds);
    let style = render::Style {
        min_feature_px: std::env::var("MIN_FEATURE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.5),
        ..render::Style::default()
    };
    let mut b = render::Batch::default();

    for (label, mul, off) in
        [("fit", 1.0, 0.0), ("4x", 4.0, 0.1), ("64x", 64.0, 0.2), ("1024x", 1024.0, 0.3)]
    {
        let mut c = cam;
        c.scale *= mul;
        c.center = s.bounds.center() + s.bounds.size() * off;
        render::build(&s, &c, &style, &mut b); // warm
        let t = Instant::now();
        let n = 20;
        for _ in 0..n {
            render::build(&s, &c, &style, &mut b);
        }
        let per = t.elapsed() / n;
        println!("  frame {label:>6}  {per:>9.2?}  {:>8} segs {:>7} tris {:>6} runs  (considered {}, drawn {})",
            b.segs.len(), b.tris.len(), b.runs.len(), b.considered, b.drawn);
    }
}
