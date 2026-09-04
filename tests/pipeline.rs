//! End-to-end checks: every fixture, all the way from bytes on disk to a draw batch.
//!
//! The unit tests cover each stage in isolation; these exist to catch the seams between them, and
//! to assert the invariants that must hold for *any* input rather than for one hand-picked file.

use std::path::PathBuf;

use opendxfviewer::camera::Camera;
use opendxfviewer::convert::{convert, Options};
use opendxfviewer::geom::{v2, Aabb, V2};
use opendxfviewer::render::{self, Batch, Style};
use opendxfviewer::scene::Scene;

fn fixtures() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dxf")))
        // The two performance fixtures are generated on demand and are not checked in.
        .filter(|p| {
            !matches!(p.file_name().and_then(|s| s.to_str()), Some("large.dxf" | "huge.dxf"))
        })
        .collect();
    v.sort();
    assert!(v.len() >= 8, "expected the fixture set, found {}", v.len());
    v
}

fn scene(path: &PathBuf) -> Scene {
    let dr = opendxfviewer::read::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    convert(&dr, &Options::default())
}

fn view(w: f64, h: f64) -> Aabb {
    Aabb::new(V2::ZERO, v2(w, h))
}

#[test]
fn every_fixture_loads_converts_and_draws() {
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let s = scene(&path);

        // Structural invariants that must hold whatever the file contained.
        let n_layers = s.layers.len() as u16;
        assert!(n_layers > 0, "{name}: a scene always has at least layer 0");
        for p in &s.polys {
            assert!(p.layer < n_layers, "{name}: layer id out of range");
            assert!(p.len >= 2, "{name}: a polyline with {} vertices survived", p.len);
            assert!(
                (p.start + p.len) as usize <= s.verts.len(),
                "{name}: vertex range overruns the arena"
            );
            assert!(!p.bbox.is_empty(), "{name}: a polyline with an empty bbox survived");
            for v in s.vertices(p) {
                assert!(v.is_finite(), "{name}: non-finite vertex {v:?}");
            }
        }
        for d in &s.dots {
            assert!(d.layer < n_layers && d.pos.is_finite(), "{name}: bad dot");
        }
        for t in &s.tris {
            assert!(t.layer < n_layers, "{name}: bad triangle layer");
            assert!(t.a.is_finite() && t.b.is_finite() && t.c.is_finite(), "{name}: bad triangle");
        }
        for t in &s.texts {
            assert!(t.layer < n_layers, "{name}: bad text layer");
            assert!(t.height > 0.0 && t.height.is_finite(), "{name}: text height {}", t.height);
            assert!(!t.text.is_empty(), "{name}: empty text survived");
        }

        // The per-layer counts are what the status bar and the panel report.
        let counted: u32 = s.layers.iter().map(|l| l.count).sum();
        assert_eq!(counted as usize, s.stats.primitives, "{name}: layer counts disagree");
        assert_eq!(
            s.stats.primitives,
            s.polys.len() + s.dots.len() + s.tris.len() + s.texts.len(),
            "{name}: primitive count disagrees with the arrays"
        );

        // And it draws, at a range of scales, without producing anything the GPU cannot take.
        let mut cam = Camera { view: view(1280.0, 800.0), ..Camera::default() };
        cam.fit(&s.bounds);
        let base = cam.scale;
        let mut batch = Batch::default();
        for mul in [1e-6, 1e-3, 0.5, 1.0, 4.0, 1e3, 1e6] {
            cam.scale = base * mul;
            render::build(&s, &cam, &Style::default(), &mut batch);
            for seg in &batch.segs {
                assert!(seg.a.is_finite() && seg.b.is_finite(), "{name} @ {mul}: {seg:?}");
                assert!(seg.half_w.is_finite() && seg.half_w > 0.0, "{name} @ {mul}: width");
            }
            for t in &batch.tris {
                assert!(t.a.is_finite() && t.b.is_finite() && t.c.is_finite(), "{name} @ {mul}");
            }
            for r in &batch.runs {
                assert!(
                    r.pos.is_finite() && r.height.is_finite() && r.height > 0.0,
                    "{name} @ {mul}"
                );
            }
        }
    }
}

#[test]
fn a_fitted_view_frames_the_whole_drawing() {
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let s = scene(&path);
        if s.bounds.is_empty() {
            continue; // empty.dxf has nothing to frame
        }
        let mut cam = Camera { view: view(1280.0, 800.0), ..Camera::default() };
        cam.fit(&s.bounds);
        let visible = cam.visible_world(0.0);
        assert!(visible.contains(s.bounds.min), "{name}: min corner off screen");
        assert!(visible.contains(s.bounds.max), "{name}: max corner off screen");

        // Every non-text primitive is drawn at a fitted view. Text is additionally drawn when it
        // is large enough on screen to read, so it is a range rather than an equality.
        let mut batch = Batch::default();
        render::build(&s, &cam, &Style::default(), &mut batch);
        let solid = s.polys.len() + s.dots.len() + s.tris.len();
        assert!(
            batch.drawn >= solid,
            "{name}: a fitted view dropped geometry ({} drawn, {solid} expected at least)",
            batch.drawn
        );
        assert!(
            batch.drawn <= solid + s.texts.len(),
            "{name}: drew more primitives than the scene contains"
        );
    }
}

#[test]
fn hiding_every_layer_draws_nothing_and_showing_them_again_restores_it() {
    let path = fixtures().into_iter().find(|p| p.ends_with("showcase.dxf")).unwrap();
    let mut s = scene(&path);
    let mut cam = Camera { view: view(1280.0, 800.0), ..Camera::default() };
    cam.fit(&s.bounds);

    let mut batch = Batch::default();
    render::build(&s, &cam, &Style::default(), &mut batch);
    let full = batch.drawn;
    assert!(full > 0);

    for l in &mut s.layers {
        l.visible = false;
    }
    render::build(&s, &cam, &Style::default(), &mut batch);
    assert_eq!(batch.drawn, 0, "hiding every layer must draw nothing");
    assert!(batch.segs.is_empty() && batch.runs.is_empty());

    for l in &mut s.layers {
        l.visible = true;
    }
    render::build(&s, &cam, &Style::default(), &mut batch);
    assert_eq!(batch.drawn, full, "restoring the layers must restore the drawing exactly");
}

#[test]
fn hiding_one_layer_removes_exactly_its_primitives() {
    let path = fixtures().into_iter().find(|p| p.ends_with("showcase.dxf")).unwrap();
    let mut s = scene(&path);
    let mut cam = Camera { view: view(1280.0, 800.0), ..Camera::default() };
    cam.fit(&s.bounds);
    let mut batch = Batch::default();
    render::build(&s, &cam, &Style::default(), &mut batch);
    let full = batch.drawn;

    for i in 0..s.layers.len() {
        // Text is counted in `drawn` only when it is large enough to draw, so compare against a
        // layer's non-text primitives.
        let expected: usize = s.polys.iter().filter(|p| p.layer as usize == i).count()
            + s.dots.iter().filter(|d| d.layer as usize == i).count()
            + s.tris.iter().filter(|t| t.layer as usize == i).count();
        if expected == 0 {
            continue;
        }
        s.layers[i].visible = false;
        render::build(&s, &cam, &Style::default(), &mut batch);
        let after = batch.drawn;
        s.layers[i].visible = true;
        assert_eq!(
            full - after,
            expected,
            "hiding layer {:?} changed the drawn count by the wrong amount",
            s.layers[i].name
        );
    }
}

#[test]
fn a_scene_survives_being_converted_twice_identically() {
    // The converter must not depend on any state carried between runs.
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let a = scene(&path);
        let b = scene(&path);
        assert_eq!(a.verts.len(), b.verts.len(), "{name}: vertex count differs between runs");
        assert_eq!(a.polys.len(), b.polys.len(), "{name}: polyline count differs");
        assert_eq!(a.stats.primitives, b.stats.primitives, "{name}: primitive count differs");
        assert_eq!(a.bounds, b.bounds, "{name}: bounds differ");
        assert_eq!(a.index.items.len(), b.index.items.len(), "{name}: index differs");
    }
}

#[test]
fn the_spatial_index_never_hides_a_primitive() {
    // The property the whole culling scheme rests on: a query may over-report, never under-report.
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let s = scene(&path);
        if s.bounds.is_empty() {
            continue;
        }
        let size = s.bounds.size();
        let step = v2(size.x.max(1e-9) / 7.0, size.y.max(1e-9) / 7.0);
        for iy in 0..7 {
            for ix in 0..7 {
                let lo = s.bounds.min + v2(step.x * ix as f64, step.y * iy as f64);
                let area = Aabb::new(lo, lo + step);
                let mut got = std::collections::HashSet::new();
                s.query(&area, |k, i| {
                    got.insert((k as u8, i));
                });
                for (i, p) in s.polys.iter().enumerate() {
                    if p.bbox.intersects(&area) {
                        assert!(got.contains(&(0u8, i as u32)), "{name}: missed polyline {i}");
                    }
                }
            }
        }
    }
}

#[test]
fn a_file_that_is_not_dxf_is_reported_rather_than_panicking() {
    let dir = std::env::temp_dir().join("opendxfviewer-tests");
    std::fs::create_dir_all(&dir).unwrap();

    for (name, bytes) in [
        ("empty", &b""[..]),
        ("garbage", &b"this is not a DXF file at all\n\x00\xff\xfe"[..]),
        ("truncated", &b"0\nSECTION\n2\nENTITIES\n0\nLINE\n8\n0\n10\n"[..]),
        ("html", &b"<!doctype html><html><body>404</body></html>"[..]),
    ] {
        let p = dir.join(format!("{name}.dxf"));
        std::fs::write(&p, bytes).unwrap();
        // Either it parses into something drawable, or it reports an error. Never a panic.
        match opendxfviewer::read::load(&p) {
            Ok(dr) => {
                let s = convert(&dr, &Options::default());
                let mut cam = Camera { view: view(800.0, 600.0), ..Camera::default() };
                cam.fit(&s.bounds);
                let mut b = Batch::default();
                render::build(&s, &cam, &Style::default(), &mut b);
            }
            Err(e) => assert!(!e.is_empty(), "{name}: an error with no message"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
