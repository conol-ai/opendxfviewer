//! Small f64 geometry primitives.
//!
//! Everything upstream of the renderer works in `f64` world coordinates. DXF files routinely carry
//! survey coordinates in the millions, where `f32` would quantise to centimetres; we only narrow to
//! `f32` once a point has been mapped into screen space.

use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub};

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct V2 {
    pub x: f64,
    pub y: f64,
}

pub const fn v2(x: f64, y: f64) -> V2 {
    V2 { x, y }
}

impl V2 {
    pub const ZERO: V2 = v2(0.0, 0.0);

    pub fn splat(v: f64) -> V2 {
        v2(v, v)
    }
    pub fn dot(self, o: V2) -> f64 {
        self.x * o.x + self.y * o.y
    }
    pub fn cross(self, o: V2) -> f64 {
        self.x * o.y - self.y * o.x
    }
    pub fn len(self) -> f64 {
        self.dot(self).sqrt()
    }
    pub fn len_sq(self) -> f64 {
        self.dot(self)
    }
    pub fn dist(self, o: V2) -> f64 {
        (self - o).len()
    }
    /// Unit vector, or `(1, 0)` for a zero-length input so callers never see NaN.
    pub fn norm(self) -> V2 {
        let l = self.len();
        if l > f64::MIN_POSITIVE {
            self / l
        } else {
            v2(1.0, 0.0)
        }
    }
    /// Rotated 90° counter-clockwise.
    pub fn perp(self) -> V2 {
        v2(-self.y, self.x)
    }
    pub fn angle(self) -> f64 {
        self.y.atan2(self.x)
    }
    pub fn from_angle(a: f64) -> V2 {
        v2(a.cos(), a.sin())
    }
    pub fn rotate(self, a: f64) -> V2 {
        let (s, c) = a.sin_cos();
        v2(self.x * c - self.y * s, self.x * s + self.y * c)
    }
    pub fn min(self, o: V2) -> V2 {
        v2(self.x.min(o.x), self.y.min(o.y))
    }
    pub fn max(self, o: V2) -> V2 {
        v2(self.x.max(o.x), self.y.max(o.y))
    }
    pub fn lerp(self, o: V2, t: f64) -> V2 {
        self + (o - self) * t
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl Add for V2 {
    type Output = V2;
    fn add(self, o: V2) -> V2 {
        v2(self.x + o.x, self.y + o.y)
    }
}
impl AddAssign for V2 {
    fn add_assign(&mut self, o: V2) {
        *self = *self + o;
    }
}
impl Sub for V2 {
    type Output = V2;
    fn sub(self, o: V2) -> V2 {
        v2(self.x - o.x, self.y - o.y)
    }
}
impl Mul<f64> for V2 {
    type Output = V2;
    fn mul(self, s: f64) -> V2 {
        v2(self.x * s, self.y * s)
    }
}
impl Div<f64> for V2 {
    type Output = V2;
    fn div(self, s: f64) -> V2 {
        v2(self.x / s, self.y / s)
    }
}
impl Neg for V2 {
    type Output = V2;
    fn neg(self) -> V2 {
        v2(-self.x, -self.y)
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct V3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

pub const fn v3(x: f64, y: f64, z: f64) -> V3 {
    V3 { x, y, z }
}

impl V3 {
    pub const ZERO: V3 = v3(0.0, 0.0, 0.0);
    pub const Z: V3 = v3(0.0, 0.0, 1.0);

    pub fn dot(self, o: V3) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: V3) -> V3 {
        v3(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    pub fn len(self) -> f64 {
        self.dot(self).sqrt()
    }
    /// Unit vector, or `+Z` for a degenerate input (DXF files do contain zero extrusion vectors).
    pub fn norm(self) -> V3 {
        let l = self.len();
        if l > f64::MIN_POSITIVE {
            self / l
        } else {
            V3::Z
        }
    }
    pub fn xy(self) -> V2 {
        v2(self.x, self.y)
    }
}

impl Add for V3 {
    type Output = V3;
    fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for V3 {
    type Output = V3;
    fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f64> for V3 {
    type Output = V3;
    fn mul(self, s: f64) -> V3 {
        v3(self.x * s, self.y * s, self.z * s)
    }
}
impl Div<f64> for V3 {
    type Output = V3;
    fn div(self, s: f64) -> V3 {
        v3(self.x / s, self.y / s, self.z / s)
    }
}

/// A 2D affine transform stored as a 3x2 matrix in column-major order:
/// `[ax bx cx]` / `[ay by cy]`, mapping `p -> a*p.x + b*p.y + c`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Xform {
    pub a: V2,
    pub b: V2,
    pub c: V2,
}

impl Default for Xform {
    fn default() -> Self {
        Xform::IDENTITY
    }
}

impl Xform {
    pub const IDENTITY: Xform = Xform { a: v2(1.0, 0.0), b: v2(0.0, 1.0), c: V2::ZERO };

    pub fn translate(t: V2) -> Xform {
        Xform { c: t, ..Xform::IDENTITY }
    }
    pub fn scale(s: V2) -> Xform {
        Xform { a: v2(s.x, 0.0), b: v2(0.0, s.y), c: V2::ZERO }
    }
    pub fn rotate(a: f64) -> Xform {
        let (s, c) = a.sin_cos();
        Xform { a: v2(c, s), b: v2(-s, c), c: V2::ZERO }
    }

    pub fn apply(&self, p: V2) -> V2 {
        self.a * p.x + self.b * p.y + self.c
    }
    /// Transform a direction: like [`Xform::apply`] but without the translation.
    pub fn apply_dir(&self, d: V2) -> V2 {
        self.a * d.x + self.b * d.y
    }

    /// `self ∘ rhs` — applies `rhs` first, then `self`.
    pub fn then(&self, outer: &Xform) -> Xform {
        Xform {
            a: outer.apply_dir(self.a),
            b: outer.apply_dir(self.b),
            c: outer.apply(self.c),
        }
    }

    pub fn det(&self) -> f64 {
        self.a.cross(self.b)
    }
    /// True when the transform flips handedness (a mirrored INSERT), which reverses arc sweeps.
    pub fn is_mirrored(&self) -> bool {
        self.det() < 0.0
    }
    /// Uniform-ish scale factor, used to keep tessellation tolerance meaningful under scaling.
    pub fn mean_scale(&self) -> f64 {
        ((self.a.len() * self.b.len()).abs()).sqrt().max(f64::MIN_POSITIVE)
    }
    /// True when the transform preserves circles (equal axis lengths and perpendicular axes),
    /// so a circle/arc stays a circle/arc instead of becoming an ellipse.
    pub fn is_conformal(&self) -> bool {
        let (la, lb) = (self.a.len(), self.b.len());
        (la - lb).abs() <= 1e-9 * la.max(lb).max(1.0) && self.a.dot(self.b).abs() <= 1e-9 * la * lb
    }
}

/// Axis-aligned bounding box. An empty box has `min > max` and absorbs points correctly.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Aabb {
    pub min: V2,
    pub max: V2,
}

impl Default for Aabb {
    fn default() -> Self {
        Aabb::EMPTY
    }
}

impl Aabb {
    pub const EMPTY: Aabb =
        Aabb { min: v2(f64::INFINITY, f64::INFINITY), max: v2(f64::NEG_INFINITY, f64::NEG_INFINITY) };

    pub fn new(min: V2, max: V2) -> Aabb {
        Aabb { min, max }
    }
    pub fn point(p: V2) -> Aabb {
        Aabb { min: p, max: p }
    }
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }
    pub fn add_point(&mut self, p: V2) {
        if p.is_finite() {
            self.min = self.min.min(p);
            self.max = self.max.max(p);
        }
    }
    pub fn union(&self, o: &Aabb) -> Aabb {
        if self.is_empty() {
            *o
        } else if o.is_empty() {
            *self
        } else {
            Aabb { min: self.min.min(o.min), max: self.max.max(o.max) }
        }
    }
    pub fn size(&self) -> V2 {
        if self.is_empty() {
            V2::ZERO
        } else {
            self.max - self.min
        }
    }
    pub fn center(&self) -> V2 {
        if self.is_empty() {
            V2::ZERO
        } else {
            (self.min + self.max) * 0.5
        }
    }
    pub fn expand(&self, m: f64) -> Aabb {
        if self.is_empty() {
            *self
        } else {
            Aabb { min: self.min - V2::splat(m), max: self.max + V2::splat(m) }
        }
    }
    pub fn intersects(&self, o: &Aabb) -> bool {
        !(self.is_empty()
            || o.is_empty()
            || self.max.x < o.min.x
            || self.min.x > o.max.x
            || self.max.y < o.min.y
            || self.min.y > o.max.y)
    }
    pub fn contains(&self, p: V2) -> bool {
        !self.is_empty()
            && p.x >= self.min.x
            && p.x <= self.max.x
            && p.y >= self.min.y
            && p.y <= self.max.y
    }
}

impl FromIterator<V2> for Aabb {
    fn from_iter<I: IntoIterator<Item = V2>>(it: I) -> Aabb {
        let mut b = Aabb::EMPTY;
        for p in it {
            b.add_point(p);
        }
        b
    }
}

/// Clip a segment to `r` with Liang–Barsky, returning the surviving portion.
///
/// The renderer relies on this: emitting an oriented quad per segment is only cheap if segments
/// that stretch far outside the viewport are trimmed to it first.
pub fn clip_segment(mut p0: V2, mut p1: V2, r: &Aabb) -> Option<(V2, V2)> {
    if r.is_empty() {
        return None;
    }
    let d = p1 - p0;
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-d.x, p0.x - r.min.x),
        (d.x, r.max.x - p0.x),
        (-d.y, p0.y - r.min.y),
        (d.y, r.max.y - p0.y),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None; // parallel to this edge and outside it
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                if t > t1 {
                    return None;
                }
                t0 = t0.max(t);
            } else {
                if t < t0 {
                    return None;
                }
                t1 = t1.min(t);
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    let (a, b) = (p0 + d * t0, p0 + d * t1);
    p0 = a;
    p1 = b;
    Some((p0, p1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn xform_composition_order_matches_apply_order() {
        // `then` must mean "rhs applied after self": rotate 90 deg, then translate by (10, 0).
        let t = Xform::rotate(std::f64::consts::FRAC_PI_2).then(&Xform::translate(v2(10.0, 0.0)));
        let p = t.apply(v2(1.0, 0.0));
        assert!(close(p.x, 10.0) && close(p.y, 1.0), "{p:?}");
    }

    #[test]
    fn mirrored_transform_reports_negative_determinant() {
        assert!(Xform::scale(v2(-1.0, 1.0)).is_mirrored());
        assert!(!Xform::scale(v2(2.0, 3.0)).is_mirrored());
        assert!(!Xform::rotate(1.0).is_mirrored());
    }

    #[test]
    fn conformal_only_for_rotation_and_uniform_scale() {
        assert!(Xform::rotate(0.7).is_conformal());
        assert!(Xform::scale(v2(3.0, 3.0)).is_conformal());
        assert!(!Xform::scale(v2(2.0, 0.5)).is_conformal());
    }

    #[test]
    fn empty_aabb_absorbs_first_point() {
        let mut b = Aabb::EMPTY;
        assert!(b.is_empty());
        b.add_point(v2(3.0, 4.0));
        assert_eq!(b, Aabb::point(v2(3.0, 4.0)));
        assert!(!b.is_empty());
    }

    #[test]
    fn aabb_ignores_non_finite_points() {
        let mut b = Aabb::point(V2::ZERO);
        b.add_point(v2(f64::NAN, 1.0));
        b.add_point(v2(f64::INFINITY, 1.0));
        assert_eq!(b, Aabb::point(V2::ZERO));
    }

    #[test]
    fn clip_segment_trims_to_box() {
        let r = Aabb::new(v2(0.0, 0.0), v2(10.0, 10.0));
        let (a, b) = clip_segment(v2(-5.0, 5.0), v2(15.0, 5.0), &r).unwrap();
        assert!(close(a.x, 0.0) && close(b.x, 10.0));
        assert!(clip_segment(v2(-5.0, 20.0), v2(15.0, 20.0), &r).is_none());
        // A fully inside segment is returned unchanged.
        let (a, b) = clip_segment(v2(1.0, 1.0), v2(2.0, 2.0), &r).unwrap();
        assert_eq!((a, b), (v2(1.0, 1.0), v2(2.0, 2.0)));
        // A degenerate point inside survives; outside it does not.
        assert!(clip_segment(v2(5.0, 5.0), v2(5.0, 5.0), &r).is_some());
        assert!(clip_segment(v2(50.0, 5.0), v2(50.0, 5.0), &r).is_none());
    }
}
