//! Times parse, convert and per-frame batching. Development aid.
use opendxfviewer::{
    camera::Camera,
    convert,
    geom::{v2, Aabb, V2},
    render,
};
use std::time::Instant;

/// Resident set size in MB, so the numbers below say where the memory actually goes.
fn rss_mb() -> u64 {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output();
        if let Ok(o) = out {
            if let Ok(s) = String::from_utf8(o.stdout) {
                if let Ok(kb) = s.trim().parse::<u64>() {
                    return kb / 1024;
                }
            }
        }
    }
    0
}

fn main() {
    let f = std::env::args().nth(1).expect("usage: bench <file.dxf>");
    let base = rss_mb();
    let t0 = Instant::now();
    let dr = opendxfviewer::read::load(&f).unwrap();
    let parse = t0.elapsed();
    let after_parse = rss_mb();

    let t1 = Instant::now();
    let s = convert::convert(&dr, &convert::Options::default());
    let conv = t1.elapsed();
    let after_convert = rss_mb();
    drop(dr);
    let after_drop = rss_mb();

    println!("{f}");
    println!("  rss      base {base} MB -> parsed {after_parse} MB -> converted {after_convert} MB -> parser dropped {after_drop} MB");
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
        let _ = rss_mb();
        println!("  frame {label:>6}  {per:>9.2?}  {:>8} segs {:>7} tris {:>6} runs  (considered {}, drawn {})",
            b.segs.len(), b.tris.len(), b.runs.len(), b.considered, b.drawn);
    }
}
