//! Dump a converted scene. Development aid.
use opendxfviewer::{convert, scene::CurveSource};

fn main() {
    for f in std::env::args().skip(1) {
        let dr = opendxfviewer::read::load(&f).unwrap();
        let s = convert::convert(&dr, &convert::Options::default());
        println!(
            "== {f}: {} polys {} dots {} tris {} texts",
            s.polys.len(),
            s.dots.len(),
            s.tris.len(),
            s.texts.len()
        );
        for (i, l) in s.layers.iter().enumerate() {
            println!(
                "  layer[{i}] {:?} {:?} count={} on={}",
                l.name, l.color, l.count, l.visible_in_file
            );
        }
        for p in &s.polys {
            let k = match p.source {
                CurveSource::None => "line".to_string(),
                CurveSource::Arc { center, radius, sweep, .. } => {
                    format!("arc c={center:?} r={radius:.2} sw={:.1}deg", sweep.to_degrees())
                }
                CurveSource::Ellipse { .. } => "ellipse".into(),
                CurveSource::Spline { index } => format!("spline#{index}"),
            };
            println!(
                "  poly len={:<4} closed={:<5} L{:<2} {:?} bbox=({:.1},{:.1})-({:.1},{:.1}) {k}",
                p.len,
                p.closed,
                p.layer,
                p.color,
                p.bbox.min.x,
                p.bbox.min.y,
                p.bbox.max.x,
                p.bbox.max.y
            );
        }
        for t in &s.texts {
            println!(
                "  text {:?} at ({:.1},{:.1}) h={} ha={:?} va={:?} {:?}",
                t.text, t.pos.x, t.pos.y, t.height, t.halign, t.valign, t.color
            );
        }
        println!(
            "  stats: read={} drawn={} unsupported={:?}",
            s.stats.entities_read, s.stats.entities_drawn, s.stats.unsupported
        );
        for w in &s.stats.warnings {
            println!("  warn: {w}");
        }
    }
}
