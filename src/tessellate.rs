//! Turning DXF curve definitions into polylines.
//!
//! Everything here works to a **chord tolerance**: the maximum distance between the true curve and
//! the polyline that replaces it. `convert` flattens at a tolerance derived from the drawing extent,
//! and [`crate::canvas`] can refine a curve again at screen tolerance once the user zooms past that.

use crate::geom::{Aabb, V2};

/// Upper bound on segments per curve. A pathological radius or tolerance must not be able to
/// allocate unboundedly.
pub const MAX_SEGMENTS: usize = 8192;
/// Lower bound, so a curve never degenerates into a single chord.
pub const MIN_SEGMENTS: usize = 4;

/// How finely to flatten.
#[derive(Copy, Clone, Debug)]
pub struct Tol {
    /// Maximum chord deviation, in the same units as the geometry.
    pub chord: f64,
    pub max_segments: usize,
}

impl Default for Tol {
    fn default() -> Self {
        Tol { chord: 1e-3, max_segments: 512 }
    }
}

impl Tol {
    pub fn new(chord: f64) -> Tol {
        Tol { chord: chord.max(f64::MIN_POSITIVE), ..Tol::default() }
    }
    pub fn with_max(mut self, n: usize) -> Tol {
        self.max_segments = n.clamp(MIN_SEGMENTS, MAX_SEGMENTS);
        self
    }
}

/// Segments needed to hold a circular arc of `radius` sweeping `sweep` radians within `tol`.
///
/// The sagitta of a chord subtending `d` on radius `r` is `r(1 - cos(d/2))`, so keeping it under
/// `tol` means `d <= 2·acos(1 - tol/r)`.
pub fn arc_segments(radius: f64, sweep: f64, tol: Tol) -> usize {
    let sweep = sweep.abs();
    if !(radius.is_finite() && sweep.is_finite()) || radius <= 0.0 || sweep <= 0.0 {
        return 1;
    }
    let ratio = tol.chord / radius;
    // A tolerance at or above the radius cannot constrain anything; fall back to the minimum.
    let step = if ratio >= 1.0 { std::f64::consts::PI } else { 2.0 * (1.0 - ratio).acos() };
    if !step.is_finite() || step <= 0.0 {
        return tol.max_segments;
    }
    ((sweep / step).ceil() as usize).clamp(MIN_SEGMENTS.min(tol.max_segments), tol.max_segments)
}

/// Append a circular arc to `out`. `sweep` is signed: positive is counter-clockwise.
///
/// The first point is emitted only when `out` is empty, so arcs chain onto a polyline without
/// duplicating the shared vertex.
pub fn flatten_arc(center: V2, radius: f64, start: f64, sweep: f64, tol: Tol, out: &mut Vec<V2>) {
    let n = arc_segments(radius, sweep, tol);
    if out.is_empty() {
        out.push(center + V2::from_angle(start) * radius);
    }
    for i in 1..=n {
        let a = start + sweep * (i as f64 / n as f64);
        out.push(center + V2::from_angle(a) * radius);
    }
}

/// Append an ellipse arc: `center + major·cos(t) + minor·sin(t)` over `[start, start + sweep]`.
///
/// Segment count uses the major radius, which over-tessellates the flatter ends slightly rather
/// than under-tessellating the tight ones.
pub fn flatten_ellipse(
    center: V2,
    major: V2,
    minor: V2,
    start: f64,
    sweep: f64,
    tol: Tol,
    out: &mut Vec<V2>,
) {
    let r = major.len().max(minor.len());
    let n = arc_segments(r, sweep, tol);
    let at = |t: f64| {
        let (s, c) = t.sin_cos();
        center + major * c + minor * s
    };
    if out.is_empty() {
        out.push(at(start));
    }
    for i in 1..=n {
        out.push(at(start + sweep * (i as f64 / n as f64)));
    }
}

/// A circular arc recovered from a polyline vertex bulge.
#[derive(Copy, Clone, Debug)]
pub struct BulgeArc {
    pub center: V2,
    pub radius: f64,
    pub start: f64,
    /// Signed sweep; positive is counter-clockwise, matching a positive bulge.
    pub sweep: f64,
}

/// Recover the arc a bulged polyline segment describes.
///
/// DXF stores `bulge = tan(θ/4)` where `θ` is the included angle, signed so that a positive bulge
/// sweeps counter-clockwise. Returns `None` for a straight or degenerate segment, in which case the
/// caller should emit a plain chord.
pub fn bulge_arc(p0: V2, p1: V2, bulge: f64) -> Option<BulgeArc> {
    if !bulge.is_finite() || bulge == 0.0 {
        return None;
    }
    let chord = p1 - p0;
    let c = chord.len();
    if !chord.is_finite() || c <= 0.0 {
        return None;
    }
    let theta = 4.0 * bulge.atan();
    let half = theta * 0.5;
    let sin_half = half.sin();
    if sin_half.abs() < 1e-15 {
        return None;
    }
    let radius = (c / (2.0 * sin_half)).abs();
    // Distance from the chord midpoint to the centre: r·cos(θ/2) = (c/2)/tan(θ/2). The sign of
    // tan(θ/2) puts the centre on the correct side for major arcs (|bulge| > 1) too.
    let d = (c * 0.5) / half.tan();
    let center = (p0 + p1) * 0.5 + chord.perp() / c * d;
    let start = (p0 - center).angle();
    Some(BulgeArc { center, radius, start, sweep: theta })
}

/// Flatten one polyline span, honouring per-vertex bulges.
///
/// `pts` are `(position, bulge)` pairs where the bulge describes the segment *leaving* that vertex.
pub fn flatten_bulge_poly(pts: &[(V2, f64)], closed: bool, tol: Tol, out: &mut Vec<V2>) {
    if pts.is_empty() {
        return;
    }
    if pts.len() == 1 {
        out.push(pts[0].0);
        return;
    }
    out.push(pts[0].0);
    let n = pts.len();
    let last = if closed { n } else { n - 1 };
    for i in 0..last {
        let (p0, b) = pts[i];
        let p1 = pts[(i + 1) % n].0;
        match bulge_arc(p0, p1, b) {
            Some(a) => {
                // `out` is non-empty, so flatten_arc skips the shared start vertex for us.
                flatten_arc(a.center, a.radius, a.start, a.sweep, tol, out);
                // Land exactly on the stored vertex; the arc reconstruction is only as exact as
                // the bulge, and a visible gap at a vertex is worse than a tiny kink.
                if let Some(l) = out.last_mut() {
                    *l = p1;
                }
            }
            None => out.push(p1),
        }
    }
}

/// A NURBS curve.
#[derive(Clone, Debug)]
pub struct Nurbs {
    pub degree: usize,
    pub ctrl: Vec<V2>,
    pub knots: Vec<f64>,
    /// Empty for a non-rational curve.
    pub weights: Vec<f64>,
}

impl Nurbs {
    /// Build an evaluable curve from what a DXF SPLINE actually carries.
    ///
    /// DXF splines in the wild are frequently inconsistent — knot counts that do not match the
    /// control points, closed curves that do not repeat their control points, degrees above what
    /// the control points can support. Rather than reject those, repair them: a viewer that draws
    /// a slightly-wrong curve is more useful than one that draws nothing.
    pub fn repair(
        mut degree: usize,
        mut ctrl: Vec<V2>,
        mut knots: Vec<f64>,
        weights: Vec<f64>,
        closed: bool,
    ) -> Option<Nurbs> {
        ctrl.retain(|p| p.is_finite());
        if ctrl.len() < 2 {
            return None;
        }
        let mut weights =
            if weights.len() == ctrl.len() && weights.iter().all(|w| w.is_finite() && *w > 0.0) {
                weights
            } else {
                Vec::new()
            };

        // Clamp first: the wrap below repeats exactly `degree` control points, so clamping
        // afterwards would leave the curve wrapped for one degree and evaluated at another, and
        // it would not close.
        degree = degree.clamp(1, ctrl.len() - 1);

        if closed {
            // A periodic curve is closed by wrapping `degree` control points onto the end. Files
            // that already repeat them are left alone.
            let wraps_already = ctrl.len() > degree
                && (0..degree).all(|i| ctrl[i].dist(ctrl[ctrl.len() - degree + i]) < 1e-9);
            if !wraps_already {
                let head: Vec<V2> = ctrl[..degree.min(ctrl.len())].to_vec();
                if !weights.is_empty() {
                    let wh: Vec<f64> = weights[..degree.min(weights.len())].to_vec();
                    weights.extend(wh);
                }
                ctrl.extend(head);
                knots.clear(); // the file's knot vector no longer matches
            }
        }

        let want = ctrl.len() + degree + 1;
        let usable = knots.len() == want
            && knots.iter().all(|k| k.is_finite())
            && knots.windows(2).all(|w| w[0] <= w[1]);
        if !usable {
            knots = if closed {
                uniform_knots(ctrl.len(), degree)
            } else {
                clamped_knots(ctrl.len(), degree)
            };
        }
        if !weights.is_empty() && weights.len() != ctrl.len() {
            weights.clear();
        }
        Some(Nurbs { degree, ctrl, knots, weights })
    }

    /// Valid parameter domain, `[U[p], U[n+1]]`.
    pub fn domain(&self) -> (f64, f64) {
        let p = self.degree;
        let n = self.ctrl.len() - 1;
        (self.knots[p], self.knots[n + 1])
    }

    /// Evaluate at `u` with de Boor's algorithm, in homogeneous coordinates when rational.
    pub fn eval(&self, u: f64) -> V2 {
        let p = self.degree;
        let n = self.ctrl.len() - 1;
        let (lo, hi) = self.domain();
        let u = u.clamp(lo, hi);

        // Knot span: the largest k in [p, n] with U[k] <= u.
        let mut k = p;
        while k < n && self.knots[k + 1] <= u {
            k += 1;
        }

        let w = |i: usize| if self.weights.is_empty() { 1.0 } else { self.weights[i] };
        // Homogeneous control points (w·P, w).
        let mut d: Vec<(V2, f64)> = (0..=p)
            .map(|j| {
                let i = k - p + j;
                (self.ctrl[i] * w(i), w(i))
            })
            .collect();

        for r in 1..=p {
            for j in (r..=p).rev() {
                let i = k - p + j;
                let lo_k = self.knots[i];
                let hi_k = self.knots[i + p + 1 - r];
                let a = if (hi_k - lo_k).abs() < f64::EPSILON {
                    0.0
                } else {
                    (u - lo_k) / (hi_k - lo_k)
                };
                d[j] = (d[j - 1].0 * (1.0 - a) + d[j].0 * a, d[j - 1].1 * (1.0 - a) + d[j].1 * a);
            }
        }
        let (num, den) = d[p];
        if den.abs() > f64::MIN_POSITIVE {
            num / den
        } else {
            num
        }
    }
}

fn clamped_knots(n_ctrl: usize, degree: usize) -> Vec<f64> {
    let inner = n_ctrl - degree; // number of distinct interior spans + 1
    let mut k = Vec::with_capacity(n_ctrl + degree + 1);
    k.extend(std::iter::repeat_n(0.0, degree + 1));
    for i in 1..inner {
        k.push(i as f64 / inner as f64);
    }
    k.extend(std::iter::repeat_n(1.0, degree + 1));
    debug_assert_eq!(k.len(), n_ctrl + degree + 1);
    k
}

fn uniform_knots(n_ctrl: usize, degree: usize) -> Vec<f64> {
    (0..n_ctrl + degree + 1).map(|i| i as f64).collect()
}

/// Adaptively flatten a NURBS curve to `tol`.
///
/// Subdivides a parameter interval while the true midpoint strays further than `tol` from the
/// chord, which spends segments where the curve actually bends.
pub fn flatten_nurbs(c: &Nurbs, tol: Tol, out: &mut Vec<V2>) {
    let (lo, hi) = c.domain();
    if !(lo.is_finite() && hi.is_finite()) || hi <= lo {
        return;
    }
    // Seed with the knot spans so a curve with sharp interior features is never missed by a
    // subdivision that happens to sample symmetric points.
    let mut seeds: Vec<f64> = c.knots.iter().copied().filter(|&k| k > lo && k < hi).collect();
    seeds.dedup();
    let mut params = Vec::with_capacity(seeds.len() + 2);
    params.push(lo);
    params.extend(seeds);
    params.push(hi);

    let budget = tol.max_segments;
    if out.is_empty() {
        out.push(c.eval(lo));
    }
    for w in params.windows(2) {
        subdivide(c, w[0], w[1], c.eval(w[0]), c.eval(w[1]), tol, budget, 0, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn subdivide(
    c: &Nurbs,
    t0: f64,
    t1: f64,
    p0: V2,
    p1: V2,
    tol: Tol,
    budget: usize,
    depth: u32,
    out: &mut Vec<V2>,
) {
    let tm = 0.5 * (t0 + t1);
    let pm = c.eval(tm);
    let flat = point_segment_dist(pm, p0, p1) <= tol.chord;
    if depth >= 16 || out.len() >= budget || (flat && depth >= 2) {
        out.push(p1);
        return;
    }
    subdivide(c, t0, tm, p0, pm, tol, budget, depth + 1, out);
    subdivide(c, tm, t1, pm, p1, tol, budget, depth + 1, out);
}

fn point_segment_dist(p: V2, a: V2, b: V2) -> f64 {
    let ab = b - a;
    let l2 = ab.len_sq();
    if l2 <= f64::MIN_POSITIVE {
        return p.dist(a);
    }
    let t = ((p - a).dot(ab) / l2).clamp(0.0, 1.0);
    p.dist(a + ab * t)
}

/// Interpolate a centripetal Catmull-Rom spline through `pts`.
///
/// Used for DXF splines that carry fit points but no control points. It is an approximation of what
/// the authoring program computed, but it passes through every fit point, which is the property a
/// reader can actually see.
pub fn flatten_fit_points(pts: &[V2], closed: bool, tol: Tol, out: &mut Vec<V2>) {
    let pts: Vec<V2> = dedup_consecutive(pts);
    if pts.len() < 2 {
        out.extend(pts);
        return;
    }
    if pts.len() == 2 {
        out.extend(pts);
        return;
    }
    let n = pts.len();
    let at = |i: isize| -> V2 {
        if closed {
            pts[i.rem_euclid(n as isize) as usize]
        } else {
            pts[i.clamp(0, n as isize - 1) as usize]
        }
    };
    if out.is_empty() {
        out.push(pts[0]);
    }
    let spans = if closed { n } else { n - 1 };
    for i in 0..spans as isize {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        // Centripetal parameterisation (alpha = 0.5) — no cusps or self-intersections, unlike the
        // uniform variant, on the unevenly spaced fit points DXF files contain.
        let t = |ta: f64, a: V2, b: V2| ta + a.dist(b).sqrt().max(1e-9);
        let (t0, t1) = (0.0, t(0.0, p0, p1));
        let (t2, t3) = (t(t1, p1, p2), 0.0);
        let t3 = t(t2, p2, p3) + t3;
        let steps = catmull_steps(p1, p2, tol);
        for s in 1..=steps {
            let tt = t1 + (t2 - t1) * (s as f64 / steps as f64);
            out.push(catmull(p0, p1, p2, p3, t0, t1, t2, t3, tt));
        }
    }
}

fn catmull_steps(a: V2, b: V2, tol: Tol) -> usize {
    // Chord length over tolerance bounds how much curvature error a span can hide.
    let n = (a.dist(b) / tol.chord.max(f64::MIN_POSITIVE)).sqrt().ceil();
    (n as usize).clamp(4, tol.max_segments.min(64))
}

#[allow(clippy::too_many_arguments)]
fn catmull(p0: V2, p1: V2, p2: V2, p3: V2, t0: f64, t1: f64, t2: f64, t3: f64, t: f64) -> V2 {
    let lerp = |a: V2, b: V2, ta: f64, tb: f64| -> V2 {
        if (tb - ta).abs() < 1e-12 {
            a
        } else {
            a * ((tb - t) / (tb - ta)) + b * ((t - ta) / (tb - ta))
        }
    };
    let a1 = lerp(p0, p1, t0, t1);
    let a2 = lerp(p1, p2, t1, t2);
    let a3 = lerp(p2, p3, t2, t3);
    let b1 = lerp(a1, a2, t0, t2);
    let b2 = lerp(a2, a3, t1, t3);
    lerp(b1, b2, t1, t2)
}

fn dedup_consecutive(pts: &[V2]) -> Vec<V2> {
    let mut out: Vec<V2> = Vec::with_capacity(pts.len());
    for &p in pts {
        if p.is_finite() && out.last().is_none_or(|l| l.dist(p) > 1e-12) {
            out.push(p);
        }
    }
    out
}

/// Drop repeated points and, for a closed shape, a final vertex duplicating the first.
pub fn cleanup(pts: &mut Vec<V2>, closed: bool) {
    pts.retain(|p| p.is_finite());
    pts.dedup_by(|a, b| a.dist(*b) <= 0.0);
    if closed && pts.len() > 2 {
        if let (Some(&f), Some(&l)) = (pts.first(), pts.last()) {
            if f.dist(l) <= 0.0 {
                pts.pop();
            }
        }
    }
}

pub fn bounds(pts: &[V2]) -> Aabb {
    pts.iter().copied().collect()
}

/// Convenience: a whole circle as a closed polyline.
pub fn flatten_circle(center: V2, radius: f64, tol: Tol, out: &mut Vec<V2>) {
    let n = arc_segments(radius, std::f64::consts::TAU, tol);
    for i in 0..n {
        let a = std::f64::consts::TAU * (i as f64 / n as f64);
        out.push(center + V2::from_angle(a) * radius);
    }
}

/// Wrap an angle into `(0, TAU]`, treating a whole number of turns as a full turn.
///
/// Modular rather than a loop: a DXF file can carry an angle of 1e300 or an infinity, and
/// subtracting TAU until it fits would never finish. A viewer that hangs on a malformed file is
/// worse than one that draws it oddly.
fn wrap_sweep(sweep: f64) -> f64 {
    use std::f64::consts::TAU;
    if !sweep.is_finite() {
        return TAU;
    }
    let w = sweep.rem_euclid(TAU);
    // rem_euclid maps an exact multiple of TAU to 0, which for an arc means a full turn.
    if w <= 0.0 {
        TAU
    } else {
        w
    }
}

/// Normalise a DXF arc's degree-based start/end angles into `(start_radians, signed_sweep)`.
///
/// DXF arcs always sweep counter-clockwise from start to end, so an end angle below the start wraps
/// through 360°. Equal angles mean a full circle, not an empty arc.
pub fn arc_sweep_from_degrees(start_deg: f64, end_deg: f64) -> (f64, f64) {
    let start = start_deg.to_radians();
    let end = end_deg.to_radians();
    if !start.is_finite() {
        return (0.0, std::f64::consts::TAU);
    }
    (start, wrap_sweep(end - start))
}

/// Normalise a DXF ellipse's start/end parameters into `(start, signed_sweep)`.
pub fn ellipse_sweep(start: f64, end: f64) -> (f64, f64) {
    if !start.is_finite() {
        return (0.0, std::f64::consts::TAU);
    }
    let sweep = end - start;
    // A full ellipse is stored as 0..2pi; anything that comes back as no sweep at all is one too.
    if sweep.abs() < 1e-12 {
        return (start, std::f64::consts::TAU);
    }
    (start, wrap_sweep(sweep))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::v2;
    use std::f64::consts::{FRAC_PI_2, PI, TAU};

    fn tol() -> Tol {
        Tol::new(1e-4).with_max(4096)
    }

    /// Largest distance from any sampled point to the nearest polyline segment.
    fn max_dev(poly: &[V2], sample: impl Fn(f64) -> V2, n: usize) -> f64 {
        (0..=n)
            .map(|i| {
                let p = sample(i as f64 / n as f64);
                poly.windows(2)
                    .map(|w| point_segment_dist(p, w[0], w[1]))
                    .fold(f64::INFINITY, f64::min)
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn arc_respects_chord_tolerance() {
        for &r in &[0.1, 1.0, 100.0, 1e5] {
            for &sweep in &[0.2, FRAC_PI_2, PI, TAU] {
                let t = Tol::new(r * 1e-4).with_max(8192);
                let mut out = Vec::new();
                flatten_arc(V2::ZERO, r, 0.3, sweep, t, &mut out);
                let dev = max_dev(&out, |u| V2::from_angle(0.3 + sweep * u) * r, 997);
                assert!(dev <= t.chord * 1.5, "r={r} sweep={sweep} dev={dev} tol={}", t.chord);
            }
        }
    }

    #[test]
    fn arc_endpoints_are_exact() {
        let mut out = Vec::new();
        flatten_arc(v2(3.0, 4.0), 2.0, 0.5, 1.25, tol(), &mut out);
        assert!(out.first().unwrap().dist(v2(3.0, 4.0) + V2::from_angle(0.5) * 2.0) < 1e-12);
        assert!(out.last().unwrap().dist(v2(3.0, 4.0) + V2::from_angle(1.75) * 2.0) < 1e-12);
    }

    #[test]
    fn arc_segment_count_is_bounded_for_absurd_inputs() {
        assert_eq!(arc_segments(0.0, 1.0, tol()), 1);
        assert_eq!(arc_segments(-5.0, 1.0, tol()), 1);
        assert_eq!(arc_segments(1.0, 0.0, tol()), 1);
        assert_eq!(arc_segments(f64::NAN, 1.0, tol()), 1);
        assert!(arc_segments(1e30, TAU, Tol::new(1e-9).with_max(600)) <= 600);
        // Tolerance coarser than the radius still produces a usable ring.
        assert!(arc_segments(0.001, TAU, Tol::new(10.0)) >= 4);
    }

    #[test]
    fn bulge_quarter_circle() {
        // tan(90/4 deg) = 0.41421...: the canonical rounded-corner bulge.
        let b = (FRAC_PI_2 / 4.0).tan();
        let a = bulge_arc(v2(1.0, 0.0), v2(0.0, 1.0), b).unwrap();
        assert!(a.center.len() < 1e-12, "centre {:?}", a.center);
        assert!((a.radius - 1.0).abs() < 1e-12);
        assert!((a.sweep - FRAC_PI_2).abs() < 1e-12);
        assert!((a.start - 0.0).abs() < 1e-12);
    }

    #[test]
    fn bulge_sign_sets_direction() {
        let cw = bulge_arc(v2(0.0, 0.0), v2(10.0, 0.0), -1.0).unwrap();
        let ccw = bulge_arc(v2(0.0, 0.0), v2(10.0, 0.0), 1.0).unwrap();
        assert!(cw.sweep < 0.0 && ccw.sweep > 0.0);
        // A bulge of ±1 is a semicircle: centre at the chord midpoint, radius = half the chord.
        for a in [cw, ccw] {
            assert!(a.center.dist(v2(5.0, 0.0)) < 1e-12, "{:?}", a.center);
            assert!((a.radius - 5.0).abs() < 1e-12);
            assert!((a.sweep.abs() - PI).abs() < 1e-12);
        }
        // Travelling counter-clockwise from (0,0) to (10,0) goes *under* the chord: the centre
        // is at (5,0), so the sweep runs from angle pi up through 3pi/2 at (5,-5). A positive
        // bulge arcing upward would be a clockwise traversal.
        let mut out = Vec::new();
        flatten_arc(ccw.center, ccw.radius, ccw.start, ccw.sweep, tol(), &mut out);
        assert!(out[out.len() / 2].y < -4.9, "ccw apex {:?}", out[out.len() / 2]);
        let mut out = Vec::new();
        flatten_arc(cw.center, cw.radius, cw.start, cw.sweep, tol(), &mut out);
        assert!(out[out.len() / 2].y > 4.9, "cw apex {:?}", out[out.len() / 2]);
    }

    #[test]
    fn major_bulge_puts_centre_on_the_far_side() {
        // |bulge| > 1 means a sweep past 180 degrees; the centre crosses the chord.
        let a = bulge_arc(v2(0.0, 0.0), v2(10.0, 0.0), 2.0).unwrap();
        assert!(a.sweep > PI, "sweep {}", a.sweep);
        assert!(a.center.y < 0.0, "centre should be below the chord, got {:?}", a.center);
        assert!((a.center.dist(v2(0.0, 0.0)) - a.radius).abs() < 1e-9);
        assert!((a.center.dist(v2(10.0, 0.0)) - a.radius).abs() < 1e-9);
    }

    #[test]
    fn bulge_rejects_degenerate_segments() {
        assert!(bulge_arc(v2(0.0, 0.0), v2(1.0, 0.0), 0.0).is_none());
        assert!(bulge_arc(v2(0.0, 0.0), v2(0.0, 0.0), 0.5).is_none());
        assert!(bulge_arc(v2(0.0, 0.0), v2(1.0, 0.0), f64::NAN).is_none());
    }

    #[test]
    fn two_semicircle_bulges_make_a_circle() {
        // The DXF idiom for a circle drawn as a polyline: two vertices, both bulge 1.
        let mut out = Vec::new();
        flatten_bulge_poly(&[(v2(-5.0, 0.0), 1.0), (v2(5.0, 0.0), 1.0)], true, tol(), &mut out);
        for p in &out {
            assert!((p.len() - 5.0).abs() < 1e-3, "point {p:?} is not on the circle");
        }
        assert!(out.iter().any(|p| p.y > 4.9) && out.iter().any(|p| p.y < -4.9));
    }

    #[test]
    fn bulge_poly_lands_exactly_on_stored_vertices() {
        let pts = [(v2(0.0, 0.0), 0.3), (v2(10.0, 1.0), -0.7), (v2(4.0, 8.0), 0.0)];
        let mut out = Vec::new();
        flatten_bulge_poly(&pts, false, tol(), &mut out);
        assert!(out.first().unwrap().dist(pts[0].0) < 1e-12);
        assert!(out.last().unwrap().dist(pts[2].0) < 1e-12);
        for (p, _) in &pts {
            assert!(out.iter().any(|q| q.dist(*p) < 1e-9), "vertex {p:?} missing from output");
        }
    }

    #[test]
    fn ellipse_matches_its_parameterisation() {
        let (c, maj) = (v2(2.0, -1.0), v2(30.0, 40.0)); // |major| = 50, rotated
        let minor = maj.perp() * 0.3;
        let t = Tol::new(1e-3).with_max(8192);
        let mut out = Vec::new();
        flatten_ellipse(c, maj, minor, 0.0, TAU, t, &mut out);
        let dev = max_dev(
            &out,
            |u| {
                let (s, co) = (TAU * u).sin_cos();
                c + maj * co + minor * s
            },
            1499,
        );
        assert!(dev <= t.chord * 2.0, "dev {dev}");
    }

    #[test]
    fn arc_sweep_from_degrees_always_goes_counter_clockwise() {
        let (s, w) = arc_sweep_from_degrees(0.0, 90.0);
        assert!((s).abs() < 1e-12 && (w - FRAC_PI_2).abs() < 1e-12);
        // Wrapping through 360.
        let (s, w) = arc_sweep_from_degrees(270.0, 45.0);
        assert!((s - 270f64.to_radians()).abs() < 1e-12);
        assert!((w - 135f64.to_radians()).abs() < 1e-12);
        // Negative start angle.
        let (_, w) = arc_sweep_from_degrees(-30.0, 30.0);
        assert!((w - 60f64.to_radians()).abs() < 1e-12);
        // Equal angles mean a full circle.
        let (_, w) = arc_sweep_from_degrees(45.0, 45.0);
        assert!((w - TAU).abs() < 1e-12);
    }

    #[test]
    fn a_nonsense_angle_cannot_hang_the_normalisers() {
        // Subtracting TAU until an infinity fits would never finish. Every one of these must
        // return promptly with a usable sweep.
        for (s, e) in [
            (0.0, f64::INFINITY),
            (0.0, f64::NEG_INFINITY),
            (f64::NEG_INFINITY, 0.0),
            (f64::INFINITY, f64::INFINITY),
            (f64::MAX, f64::MIN),
            (f64::NAN, 90.0),
            (0.0, f64::NAN),
            (0.0, 1e308),
            (-1e308, 1e308),
        ] {
            let (start, sweep) = arc_sweep_from_degrees(s, e);
            assert!(start.is_finite(), "({s}, {e}) -> start {start}");
            assert!(
                sweep.is_finite() && sweep > 0.0 && sweep <= TAU + 1e-12,
                "({s}, {e}) -> {sweep}"
            );
            let (start, sweep) = ellipse_sweep(s, e);
            assert!(start.is_finite(), "({s}, {e}) -> start {start}");
            assert!(
                sweep.is_finite() && sweep > 0.0 && sweep <= TAU + 1e-12,
                "({s}, {e}) -> {sweep}"
            );
        }
    }

    #[test]
    fn a_sweep_of_several_whole_turns_is_one_turn() {
        let (_, w) = arc_sweep_from_degrees(0.0, 720.0);
        assert!((w - TAU).abs() < 1e-9, "{w}");
        let (_, w) = arc_sweep_from_degrees(0.0, 450.0);
        assert!((w - FRAC_PI_2).abs() < 1e-9, "{w}");
    }

    #[test]
    fn ellipse_sweep_handles_wrap_and_full() {
        let (_, w) = ellipse_sweep(0.0, TAU);
        assert!((w - TAU).abs() < 1e-12);
        let (_, w) = ellipse_sweep(1.5 * PI, 0.5 * PI);
        assert!((w - PI).abs() < 1e-12);
        let (_, w) = ellipse_sweep(1.0, 1.0);
        assert!((w - TAU).abs() < 1e-12);
    }

    // ---- NURBS -----------------------------------------------------------------

    fn nurbs(deg: usize, ctrl: &[V2], knots: &[f64], w: &[f64], closed: bool) -> Nurbs {
        Nurbs::repair(deg, ctrl.to_vec(), knots.to_vec(), w.to_vec(), closed).unwrap()
    }

    #[test]
    fn clamped_spline_interpolates_its_end_points() {
        let ctrl = [v2(0.0, 0.0), v2(1.0, 3.0), v2(4.0, 3.0), v2(5.0, 0.0)];
        let c = nurbs(3, &ctrl, &[0., 0., 0., 0., 1., 1., 1., 1.], &[], false);
        let (lo, hi) = c.domain();
        assert!(c.eval(lo).dist(ctrl[0]) < 1e-12, "{:?}", c.eval(lo));
        assert!(c.eval(hi).dist(ctrl[3]) < 1e-12, "{:?}", c.eval(hi));
    }

    #[test]
    fn degree_one_spline_is_the_control_polygon() {
        let ctrl = [v2(0.0, 0.0), v2(10.0, 5.0), v2(20.0, 0.0)];
        let c = nurbs(1, &ctrl, &[], &[], false);
        let (lo, hi) = c.domain();
        for i in 0..=20 {
            let u = lo + (hi - lo) * i as f64 / 20.0;
            let p = c.eval(u);
            let d = ctrl
                .windows(2)
                .map(|w| point_segment_dist(p, w[0], w[1]))
                .fold(f64::INFINITY, f64::min);
            assert!(d < 1e-9, "u={u} strayed {d} from the control polygon");
        }
    }

    #[test]
    fn rational_quadratic_is_an_exact_circular_arc() {
        // The classic NURBS circle segment: weights 1, cos(45 deg), 1 over a right-angle corner.
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let ctrl = [v2(1.0, 0.0), v2(1.0, 1.0), v2(0.0, 1.0)];
        let c = nurbs(2, &ctrl, &[0., 0., 0., 1., 1., 1.], &[1.0, w, 1.0], false);
        let (lo, hi) = c.domain();
        for i in 0..=64 {
            let u = lo + (hi - lo) * i as f64 / 64.0;
            assert!((c.eval(u).len() - 1.0).abs() < 1e-12, "u={u} r={}", c.eval(u).len());
        }
    }

    #[test]
    fn flatten_nurbs_meets_tolerance() {
        let ctrl = [v2(0., 0.), v2(20., 60.), v2(60., -40.), v2(100., 60.), v2(140., 0.)];
        let c = nurbs(3, &ctrl, &[], &[], false);
        let t = Tol::new(0.05).with_max(4096);
        let mut out = Vec::new();
        flatten_nurbs(&c, t, &mut out);
        assert!(out.len() > 8);
        let (lo, hi) = c.domain();
        let dev = max_dev(&out, |u| c.eval(lo + (hi - lo) * u), 2003);
        assert!(dev <= t.chord * 2.0, "dev {dev}");
    }

    #[test]
    fn closed_spline_returns_to_its_start() {
        let ctrl = [v2(0.0, 0.0), v2(10.0, 0.0), v2(10.0, 10.0), v2(0.0, 10.0)];
        let c = nurbs(3, &ctrl, &[], &[], true);
        let (lo, hi) = c.domain();
        assert!(c.eval(lo).dist(c.eval(hi)) < 1e-9, "{:?} vs {:?}", c.eval(lo), c.eval(hi));
    }

    #[test]
    fn a_closed_spline_closes_whatever_degree_the_file_claims() {
        // The wrap repeats `degree` control points, so the degree has to be settled before the
        // wrap rather than after it, or the two disagree and the curve gapes open.
        let ctrl = [v2(0.0, 0.0), v2(10.0, 0.0), v2(10.0, 10.0), v2(0.0, 10.0)];
        for claimed in [1, 2, 3, 5, 9, 100] {
            let c = Nurbs::repair(claimed, ctrl.to_vec(), vec![], vec![], true).unwrap();
            let (lo, hi) = c.domain();
            let gap = c.eval(lo).dist(c.eval(hi));
            assert!(gap < 1e-9, "degree {claimed} left a gap of {gap}");
        }
    }

    #[test]
    fn repair_rescues_the_junk_files_contain() {
        // Degree higher than the control points can support.
        let c = Nurbs::repair(7, vec![v2(0., 0.), v2(1., 1.)], vec![], vec![], false).unwrap();
        assert_eq!(c.degree, 1);
        // Knot vector of the wrong length is replaced.
        let c = Nurbs::repair(3, vec![v2(0., 0.); 5], vec![0.0; 3], vec![], false).unwrap();
        assert_eq!(c.knots.len(), 5 + 3 + 1);
        // Non-monotonic knots are replaced.
        let c =
            Nurbs::repair(2, vec![v2(0., 0.); 4], vec![5., 1., 9., 0., 2., 7., 3.], vec![], false)
                .unwrap();
        assert!(c.knots.windows(2).all(|w| w[0] <= w[1]));
        // Bad weights are dropped rather than producing NaN.
        let c = Nurbs::repair(2, vec![v2(0., 0.); 3], vec![], vec![1.0, 0.0, -1.0], false).unwrap();
        assert!(c.weights.is_empty());
        assert!(c.eval(c.domain().0).is_finite());
        // Too few control points is genuinely unusable.
        assert!(Nurbs::repair(3, vec![v2(0., 0.)], vec![], vec![], false).is_none());
    }

    #[test]
    fn nurbs_evaluation_never_returns_nan() {
        // Duplicated knots and coincident control points are both common in real files.
        let c = nurbs(
            3,
            &[v2(0., 0.), v2(0., 0.), v2(0., 0.), v2(5., 5.), v2(5., 5.)],
            &[0., 0., 0., 0., 0.5, 1., 1., 1., 1.],
            &[],
            false,
        );
        let (lo, hi) = c.domain();
        for i in 0..=100 {
            assert!(c.eval(lo + (hi - lo) * i as f64 / 100.0).is_finite());
        }
    }

    // ---- fit points ------------------------------------------------------------

    #[test]
    fn fit_point_curve_passes_through_every_point() {
        let pts = [v2(0., 0.), v2(10., 20.), v2(30., -5.), v2(50., 15.), v2(70., 0.)];
        let mut out = Vec::new();
        flatten_fit_points(&pts, false, Tol::new(0.01), &mut out);
        for p in &pts {
            let d = out.iter().map(|q| q.dist(*p)).fold(f64::INFINITY, f64::min);
            assert!(d < 1e-6, "fit point {p:?} missed by {d}");
        }
        assert!(out.first().unwrap().dist(pts[0]) < 1e-12);
        assert!(out.last().unwrap().dist(pts[4]) < 1e-12);
    }

    #[test]
    fn closed_fit_point_curve_comes_back_round() {
        let pts = [v2(0., 0.), v2(10., 0.), v2(10., 10.), v2(0., 10.)];
        let mut out = Vec::new();
        flatten_fit_points(&pts, true, Tol::new(0.01), &mut out);
        assert!(out.last().unwrap().dist(pts[0]) < 1e-6);
    }

    #[test]
    fn fit_points_survive_duplicates_and_tiny_inputs() {
        let mut out = Vec::new();
        flatten_fit_points(&[v2(1., 1.), v2(1., 1.), v2(1., 1.)], false, Tol::new(0.1), &mut out);
        assert_eq!(out.len(), 1);
        out.clear();
        flatten_fit_points(&[], false, Tol::new(0.1), &mut out);
        assert!(out.is_empty());
        out.clear();
        flatten_fit_points(&[v2(0., 0.), v2(1., 0.)], false, Tol::new(0.1), &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn cleanup_removes_duplicates_and_the_closing_vertex() {
        let mut p = vec![v2(0., 0.), v2(0., 0.), v2(1., 0.), v2(1., 1.), v2(0., 0.)];
        cleanup(&mut p, true);
        assert_eq!(p, vec![v2(0., 0.), v2(1., 0.), v2(1., 1.)]);

        let mut p = vec![v2(0., 0.), v2(f64::NAN, 1.), v2(1., 0.)];
        cleanup(&mut p, false);
        assert_eq!(p, vec![v2(0., 0.), v2(1., 0.)]);
    }
}
