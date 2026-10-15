//! Exact geometry of the part primitives: volumes, centroids, bounds, support
//! functions, ray intersection, box and sphere tests, and GJK distance with
//! linear shape casts.
//!
//! Everything here works on one part at a time in its own frame or in world
//! space, independent of Jolt: the part queries use it as their narrow phase,
//! and the tests use it as the brute-force reference.

use glam::{DQuat, DVec3, Quat, Vec3};

/// The primitive a part is made of. Sizes are full extents along the part's
/// local axes.
///
/// * `Block`: the box of the size.
/// * `Ball`: a sphere whose diameter is the smallest extent.
/// * `Cylinder`: a cylinder along local X, as long as the X extent, whose
///   diameter is the smaller of the Y and Z extents.
/// * `Wedge`: a ramp. The bottom face (−Y) and the back face (+Z) are full
///   rectangles; the slope rises from the front bottom edge (−Y, −Z) to the
///   back top edge (+Y, +Z).
/// * `CornerWedge`: a pyramid over the full bottom rectangle whose apex is the
///   top corner at (+X, +Y, −Z).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PartShape {
    /// A box.
    #[default]
    Block,
    /// A sphere.
    Ball,
    /// A cylinder along local X.
    Cylinder,
    /// A ramp.
    Wedge,
    /// A corner pyramid.
    CornerWedge,
}

/// A part's shape and size: everything about its geometry except its pose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartGeom {
    /// The primitive.
    pub shape: PartShape,
    /// Full extents along the local axes; each positive and finite.
    pub size: Vec3,
}

/// A position and rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// Position.
    pub pos: Vec3,
    /// Rotation (unit).
    pub rot: Quat,
}

impl Default for Pose {
    fn default() -> Self {
        Pose::IDENTITY
    }
}

impl Pose {
    /// No translation, no rotation.
    pub const IDENTITY: Pose = Pose {
        pos: Vec3::ZERO,
        rot: Quat::IDENTITY,
    };

    /// A pose.
    pub fn new(pos: Vec3, rot: Quat) -> Pose {
        Pose { pos, rot }
    }

    /// `self ∘ other`: `other` expressed in `self`'s frame, taken to world.
    pub fn mul(&self, other: &Pose) -> Pose {
        Pose {
            pos: self.pos + self.rot * other.pos,
            rot: (self.rot * other.rot).normalize(),
        }
    }

    /// The inverse pose.
    pub fn inverse(&self) -> Pose {
        let rot = self.rot.conjugate();
        Pose {
            pos: rot * -self.pos,
            rot,
        }
    }

    /// A point from this frame to world.
    pub fn point(&self, p: Vec3) -> Vec3 {
        self.pos + self.rot * p
    }

    /// A world point into this frame.
    pub fn inv_point(&self, p: Vec3) -> Vec3 {
        self.rot.conjugate() * (p - self.pos)
    }
}

/// Smallest extent accepted along any axis (studs or metres). Smaller sizes
/// are raised to it by [`PartGeom::sanitized`].
pub const MIN_EXTENT: f32 = 0.001;

impl PartGeom {
    /// A geometry.
    pub fn new(shape: PartShape, size: Vec3) -> PartGeom {
        PartGeom { shape, size }
    }

    /// True when every extent is finite and positive.
    pub fn is_valid(&self) -> bool {
        self.size.is_finite() && self.size.min_element() > 0.0
    }

    /// The geometry with every extent at least [`MIN_EXTENT`] and non-finite
    /// extents replaced by it.
    pub fn sanitized(mut self) -> PartGeom {
        let fix = |v: f32| {
            if v.is_finite() {
                v.max(MIN_EXTENT)
            } else {
                MIN_EXTENT
            }
        };
        self.size = Vec3::new(fix(self.size.x), fix(self.size.y), fix(self.size.z));
        self
    }

    /// Half the size.
    pub fn half(&self) -> Vec3 {
        self.size * 0.5
    }

    /// The sphere radius of a ball, the cross-section radius of a cylinder.
    pub fn radius(&self) -> f32 {
        match self.shape {
            PartShape::Ball => self.size.min_element() * 0.5,
            PartShape::Cylinder => self.size.y.min(self.size.z) * 0.5,
            _ => 0.0,
        }
    }

    /// The volume of the solid.
    pub fn volume(&self) -> f32 {
        let s = self.size;
        match self.shape {
            PartShape::Block => s.x * s.y * s.z,
            PartShape::Wedge => s.x * s.y * s.z * 0.5,
            PartShape::CornerWedge => s.x * s.y * s.z / 3.0,
            PartShape::Ball => {
                let r = self.radius();
                4.0 / 3.0 * std::f32::consts::PI * r * r * r
            }
            PartShape::Cylinder => {
                let r = self.radius();
                std::f32::consts::PI * r * r * s.x
            }
        }
    }

    /// The centroid of the solid in the part's frame.
    pub fn centroid(&self) -> Vec3 {
        let h = self.half();
        match self.shape {
            PartShape::Block | PartShape::Ball | PartShape::Cylinder => Vec3::ZERO,
            // The triangle (y, z) = (-h, -h), (-h, +h), (+h, +h) has its centroid at
            // (-h/3, +h/3).
            PartShape::Wedge => Vec3::new(0.0, -h.y / 3.0, h.z / 3.0),
            // A pyramid's centroid lies a quarter of the way from the base centroid to
            // the apex.
            PartShape::CornerWedge => {
                let base = Vec3::new(0.0, -h.y, 0.0);
                let apex = Vec3::new(h.x, h.y, -h.z);
                base + (apex - base) * 0.25
            }
        }
    }

    /// The corners of a polyhedral shape (block, wedge, corner wedge); empty
    /// for a ball or cylinder.
    pub fn vertices(&self) -> Vec<Vec3> {
        let h = self.half();
        match self.shape {
            PartShape::Block => {
                let mut v = Vec::with_capacity(8);
                for x in [-h.x, h.x] {
                    for y in [-h.y, h.y] {
                        for z in [-h.z, h.z] {
                            v.push(Vec3::new(x, y, z));
                        }
                    }
                }
                v
            }
            PartShape::Wedge => vec![
                Vec3::new(-h.x, -h.y, -h.z),
                Vec3::new(h.x, -h.y, -h.z),
                Vec3::new(-h.x, -h.y, h.z),
                Vec3::new(h.x, -h.y, h.z),
                Vec3::new(-h.x, h.y, h.z),
                Vec3::new(h.x, h.y, h.z),
            ],
            PartShape::CornerWedge => vec![
                Vec3::new(-h.x, -h.y, -h.z),
                Vec3::new(h.x, -h.y, -h.z),
                Vec3::new(-h.x, -h.y, h.z),
                Vec3::new(h.x, -h.y, h.z),
                Vec3::new(h.x, h.y, -h.z),
            ],
            PartShape::Ball | PartShape::Cylinder => Vec::new(),
        }
    }

    /// The bounding planes `(outward unit normal, offset)` of a polyhedral shape,
    /// so the solid is `n · p <= d` for every plane; empty for a ball or cylinder.
    pub fn planes(&self) -> Vec<(Vec3, f32)> {
        let h = self.half();
        let mut out = Vec::with_capacity(6);
        match self.shape {
            PartShape::Block => {
                out.push((Vec3::X, h.x));
                out.push((-Vec3::X, h.x));
                out.push((Vec3::Y, h.y));
                out.push((-Vec3::Y, h.y));
                out.push((Vec3::Z, h.z));
                out.push((-Vec3::Z, h.z));
            }
            PartShape::Wedge => {
                out.push((Vec3::X, h.x));
                out.push((-Vec3::X, h.x));
                out.push((-Vec3::Y, h.y));
                out.push((Vec3::Z, h.z));
                // The slope through (y, z) = (-h.y, -h.z) and (h.y, h.z), facing up
                // and forward; it passes through the origin.
                out.push((Vec3::new(0.0, h.z, -h.y).normalize(), 0.0));
            }
            PartShape::CornerWedge => {
                let v = self.vertices();
                out.push((-Vec3::Y, h.y));
                // The side faces each hold the apex and one bottom edge.
                let apex = v[4];
                let base = [v[0], v[1], v[3], v[2]];
                for i in 0..4 {
                    let a = base[i];
                    let b = base[(i + 1) % 4];
                    let n = (b - a).cross(apex - a);
                    if n.length_squared() < 1e-20 {
                        continue; // a degenerate face (the apex above this edge)
                    }
                    let mut n = n.normalize();
                    // Point outward: away from the centroid.
                    let c = self.centroid();
                    if n.dot(a - c) < 0.0 {
                        n = -n;
                    }
                    out.push((n, n.dot(a)));
                }
            }
            PartShape::Ball | PartShape::Cylinder => {}
        }
        out
    }

    /// The farthest point of the solid along `dir` (any length), in the part's
    /// frame.
    pub fn support(&self, dir: DVec3) -> DVec3 {
        let h = self.half().as_dvec3();
        match self.shape {
            PartShape::Block => DVec3::new(
                h.x.copysign(dir.x),
                h.y.copysign(dir.y),
                h.z.copysign(dir.z),
            ),
            PartShape::Ball => {
                let r = f64::from(self.radius());
                let len = dir.length();
                if len > 1e-300 {
                    dir * (r / len)
                } else {
                    DVec3::new(r, 0.0, 0.0)
                }
            }
            PartShape::Cylinder => {
                let r = f64::from(self.radius());
                let radial = DVec3::new(0.0, dir.y, dir.z);
                let len = radial.length();
                let rad = if len > 1e-300 {
                    radial * (r / len)
                } else {
                    DVec3::ZERO
                };
                DVec3::new(h.x.copysign(dir.x), 0.0, 0.0) + rad
            }
            PartShape::Wedge | PartShape::CornerWedge => {
                let mut best = DVec3::ZERO;
                let mut best_d = f64::NEG_INFINITY;
                for v in self.vertices() {
                    let v = v.as_dvec3();
                    let d = v.dot(dir);
                    if d > best_d {
                        best_d = d;
                        best = v;
                    }
                }
                best
            }
        }
    }

    /// The axis-aligned bounds `(min, max)` of the solid at `pose`, in world space.
    pub fn world_aabb(&self, pose: &Pose) -> (Vec3, Vec3) {
        match self.shape {
            PartShape::Ball => {
                let r = Vec3::splat(self.radius());
                (pose.pos - r, pose.pos + r)
            }
            _ => {
                // The support along each world axis gives the exact bounds of a convex
                // solid.
                let rot = pose.rot.as_dquat();
                let inv = rot.conjugate();
                let mut min = Vec3::ZERO;
                let mut max = Vec3::ZERO;
                for (i, axis) in [DVec3::X, DVec3::Y, DVec3::Z].into_iter().enumerate() {
                    let hi = (rot * self.support(inv * axis)).dot(axis) as f32;
                    let lo = (rot * self.support(inv * -axis)).dot(axis) as f32;
                    max[i] = hi;
                    min[i] = lo;
                }
                (pose.pos + min, pose.pos + max)
            }
        }
    }

    /// The axis-aligned bounds of the part's bounding box (its whole size,
    /// whatever its shape) at `pose`.
    pub fn box_aabb(&self, pose: &Pose) -> (Vec3, Vec3) {
        let m = glam::Mat3::from_quat(pose.rot);
        let h = self.half();
        let ext = m.x_axis.abs() * h.x + m.y_axis.abs() * h.y + m.z_axis.abs() * h.z;
        (pose.pos - ext, pose.pos + ext)
    }

    /// Where a ray entering the solid from outside first meets its surface, in
    /// the part's frame: `(t, outward normal)` with `0 <= t <= max_t`, where the
    /// ray is `origin + t * dir`. A ray that starts inside the solid does not hit
    /// it.
    pub fn ray_entry(&self, origin: Vec3, dir: Vec3, max_t: f32) -> Option<(f32, Vec3)> {
        let (o, d) = (origin.as_dvec3(), dir.as_dvec3());
        let max_t = f64::from(max_t);
        let (t, n) = match self.shape {
            PartShape::Block | PartShape::Wedge | PartShape::CornerWedge => {
                clip_planes(&self.planes(), o, d)?
            }
            PartShape::Ball => {
                let r = f64::from(self.radius());
                let b = o.dot(d);
                let a = d.dot(d);
                let c = o.dot(o) - r * r;
                if c <= 0.0 {
                    return None; // inside
                }
                let disc = b * b - a * c;
                if disc < 0.0 || a <= 0.0 {
                    return None;
                }
                let t = (-b - disc.sqrt()) / a;
                if t < 0.0 {
                    return None;
                }
                let p = o + d * t;
                (t, p.normalize_or_zero())
            }
            PartShape::Cylinder => {
                ray_cylinder(f64::from(self.half().x), f64::from(self.radius()), o, d)?
            }
        };
        (t <= max_t).then(|| (t as f32, n.as_vec3()))
    }

    /// True when the local point is inside the solid (or on its surface).
    pub fn contains_local(&self, p: Vec3) -> bool {
        match self.shape {
            PartShape::Ball => p.length() <= self.radius(),
            PartShape::Cylinder => {
                let r = self.radius();
                p.x.abs() <= self.half().x && p.y * p.y + p.z * p.z <= r * r
            }
            _ => self.planes().iter().all(|(n, d)| n.dot(p) <= *d + 1e-6),
        }
    }
}

/// Entry of a ray into the intersection of half-spaces `n · p <= d`; `None` if
/// it misses or starts inside.
fn clip_planes(planes: &[(Vec3, f32)], o: DVec3, d: DVec3) -> Option<(f64, DVec3)> {
    let mut t_in = f64::NEG_INFINITY;
    let mut t_out = f64::INFINITY;
    let mut n_in = DVec3::ZERO;
    for (n, off) in planes {
        let n = n.as_dvec3();
        let off = f64::from(*off);
        let denom = n.dot(d);
        let dist = off - n.dot(o); // >= 0 when the origin is inside this half-space
        if denom.abs() < 1e-15 {
            if dist < 0.0 {
                return None; // parallel and outside
            }
            continue;
        }
        let t = dist / denom;
        if denom < 0.0 {
            // Entering this half-space.
            if t > t_in {
                t_in = t;
                n_in = n;
            }
        } else if t < t_out {
            t_out = t;
        }
        if t_in > t_out {
            return None;
        }
    }
    if t_in < 0.0 || !t_in.is_finite() {
        return None; // starts inside (or never enters)
    }
    Some((t_in, n_in))
}

/// Entry of a ray into the cylinder `|x| <= hx`, `y² + z² <= r²`.
fn ray_cylinder(hx: f64, r: f64, o: DVec3, d: DVec3) -> Option<(f64, DVec3)> {
    // The slab along X.
    let (mut t_in, mut t_out, mut n_in);
    if d.x.abs() < 1e-15 {
        if o.x.abs() > hx {
            return None;
        }
        t_in = f64::NEG_INFINITY;
        t_out = f64::INFINITY;
        n_in = DVec3::ZERO;
    } else {
        let t1 = (-hx - o.x) / d.x;
        let t2 = (hx - o.x) / d.x;
        if t1 < t2 {
            t_in = t1;
            t_out = t2;
            n_in = -DVec3::X;
        } else {
            t_in = t2;
            t_out = t1;
            n_in = DVec3::X;
        }
    }
    // The infinite cylinder around X.
    let a = d.y * d.y + d.z * d.z;
    let b = o.y * d.y + o.z * d.z;
    let c = o.y * o.y + o.z * o.z - r * r;
    if a < 1e-30 {
        if c > 0.0 {
            return None;
        }
    } else {
        let disc = b * b - a * c;
        if disc < 0.0 {
            return None;
        }
        let s = disc.sqrt();
        let c1 = (-b - s) / a;
        let c2 = (-b + s) / a;
        if c1 > t_in {
            t_in = c1;
            let p = o + d * c1;
            n_in = DVec3::new(0.0, p.y, p.z).normalize_or_zero();
        }
        t_out = t_out.min(c2);
    }
    if t_in > t_out || t_in < 0.0 || !t_in.is_finite() {
        return None;
    }
    Some((t_in, n_in))
}

/// Whether two oriented boxes overlap (separating axis test). Boxes that only
/// touch count as overlapping.
pub fn obb_overlap(a: &Pose, ha: Vec3, b: &Pose, hb: Vec3) -> bool {
    let ra = glam::DMat3::from_quat(a.rot.as_dquat());
    let rb = glam::DMat3::from_quat(b.rot.as_dquat());
    let ax = [ra.x_axis, ra.y_axis, ra.z_axis];
    let bx = [rb.x_axis, rb.y_axis, rb.z_axis];
    let (ha, hb) = (ha.as_dvec3(), hb.as_dvec3());
    let t = (b.pos - a.pos).as_dvec3();
    let proj = |axis: DVec3| -> bool {
        let len2 = axis.length_squared();
        if len2 < 1e-18 {
            return true; // a degenerate cross product separates nothing
        }
        let pa = ha.x * ax[0].dot(axis).abs()
            + ha.y * ax[1].dot(axis).abs()
            + ha.z * ax[2].dot(axis).abs();
        let pb = hb.x * bx[0].dot(axis).abs()
            + hb.y * bx[1].dot(axis).abs()
            + hb.z * bx[2].dot(axis).abs();
        t.dot(axis).abs() <= pa + pb + 1e-9 * len2.sqrt()
    };
    for i in 0..3 {
        if !proj(ax[i]) || !proj(bx[i]) {
            return false;
        }
    }
    for a in ax {
        for b in bx {
            if !proj(a.cross(b)) {
                return false;
            }
        }
    }
    true
}

/// Whether an oriented box and a sphere overlap (touching counts).
pub fn obb_sphere_overlap(a: &Pose, ha: Vec3, center: Vec3, radius: f32) -> bool {
    let local = a.inv_point(center);
    let closest = local.clamp(-ha, ha);
    (local - closest).length_squared() <= radius * radius
}

/// A convex solid in world space, for GJK.
pub trait Convex {
    /// The farthest world point along `dir`.
    fn support(&self, dir: DVec3) -> DVec3;
    /// Any point inside.
    fn center(&self) -> DVec3;
}

/// A part's solid at a pose, optionally grown (positive `margin`) or shrunk
/// (negative) by a distance along every support direction, and moved by
/// `offset`.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    /// The geometry.
    pub geom: PartGeom,
    /// Its position.
    pub pos: DVec3,
    /// Its rotation.
    pub rot: DQuat,
    /// Added along every support direction.
    pub margin: f64,
}

impl Placed {
    /// The part at `pose`.
    pub fn new(geom: PartGeom, pose: &Pose) -> Placed {
        Placed {
            geom,
            pos: pose.pos.as_dvec3(),
            rot: pose.rot.as_dquat().normalize(),
            margin: 0.0,
        }
    }
}

impl Convex for Placed {
    fn support(&self, dir: DVec3) -> DVec3 {
        let local = self.rot.conjugate() * dir;
        let p = self.pos + self.rot * self.geom.support(local);
        if self.margin != 0.0 {
            let len = dir.length();
            if len > 1e-300 {
                return p + dir * (self.margin / len);
            }
        }
        p
    }

    fn center(&self) -> DVec3 {
        self.pos + self.rot * self.geom.centroid().as_dvec3()
    }
}

/// A point of the Minkowski difference `A - B` with the points it came from.
#[derive(Clone, Copy, Debug)]
struct Vert {
    w: DVec3,
    a: DVec3,
    b: DVec3,
}

fn minkowski(a: &dyn Convex, b: &dyn Convex, d: DVec3) -> Vert {
    let pa = a.support(d);
    let pb = b.support(-d);
    Vert {
        w: pa - pb,
        a: pa,
        b: pb,
    }
}

/// The result of [`gjk_distance`].
#[derive(Clone, Copy, Debug)]
pub struct Separation {
    /// The distance between the solids; 0 when they overlap.
    pub distance: f64,
    /// The closest point on `a` (meaningful when separated).
    pub on_a: DVec3,
    /// The closest point on `b` (meaningful when separated).
    pub on_b: DVec3,
}

/// The closest point of a simplex to the origin, reducing the simplex to the
/// points that support it; returns the barycentric weights of the kept points.
fn closest_on_simplex(s: &mut Vec<Vert>) -> (DVec3, Vec<f64>) {
    match s.len() {
        1 => (s[0].w, vec![1.0]),
        2 => {
            let (a, b) = (s[0].w, s[1].w);
            let ab = b - a;
            let t = (-a).dot(ab) / ab.dot(ab).max(1e-300);
            if t <= 0.0 {
                s.truncate(1);
                (a, vec![1.0])
            } else if t >= 1.0 {
                s.remove(0);
                (b, vec![1.0])
            } else {
                (a + ab * t, vec![1.0 - t, t])
            }
        }
        3 => closest_on_triangle(s),
        _ => closest_on_tetra(s),
    }
}

/// Ericson's closest point on a triangle, reducing the simplex.
fn closest_on_triangle(s: &mut Vec<Vert>) -> (DVec3, Vec<f64>) {
    let (a, b, c) = (s[0].w, s[1].w, s[2].w);
    let p = DVec3::ZERO;
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        s.truncate(1);
        return (a, vec![1.0]);
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        let v = s[1];
        s.clear();
        s.push(v);
        return (b, vec![1.0]);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        s.truncate(2);
        return (a + ab * v, vec![1.0 - v, v]);
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        let v = s[2];
        s.clear();
        s.push(v);
        return (c, vec![1.0]);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        s.remove(1);
        return (a + ac * w, vec![1.0 - w, w]);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        s.remove(0);
        return (b + (c - b) * w, vec![1.0 - w, w]);
    }
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    (a + ab * v + ac * w, vec![1.0 - v - w, v, w])
}

/// The closest point of a tetrahedron to the origin; the origin inside gives
/// distance 0 and keeps all four points.
fn closest_on_tetra(s: &mut Vec<Vert>) -> (DVec3, Vec<f64>) {
    let pts = [s[0], s[1], s[2], s[3]];
    let faces = [[0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 3, 1], [1, 2, 3, 0]];
    // The origin is inside when it is on the same side of every face as the
    // opposite vertex.
    let mut inside = true;
    let mut best: Option<(f64, Vec<Vert>, DVec3, Vec<f64>)> = None;
    for f in faces {
        let (a, b, c, d) = (pts[f[0]].w, pts[f[1]].w, pts[f[2]].w, pts[f[3]].w);
        let n = (b - a).cross(c - a);
        let side_o = n.dot(-a);
        let side_d = n.dot(d - a);
        if side_o * side_d < 0.0 || side_d == 0.0 {
            inside = false;
            let mut tri = vec![pts[f[0]], pts[f[1]], pts[f[2]]];
            let (p, w) = closest_on_triangle(&mut tri);
            let dist = p.length_squared();
            if best.as_ref().is_none_or(|(bd, ..)| dist < *bd) {
                best = Some((dist, tri, p, w));
            }
        }
    }
    if inside {
        return (DVec3::ZERO, vec![0.25; 4]);
    }
    let (_, tri, p, w) = best.expect("a face separates the origin when it is outside");
    *s = tri;
    (p, w)
}

/// The distance between two convex solids (GJK).
pub fn gjk_distance(a: &dyn Convex, b: &dyn Convex) -> Separation {
    let mut dir = a.center() - b.center();
    if dir.length_squared() < 1e-24 {
        dir = DVec3::X;
    }
    let mut simplex: Vec<Vert> = vec![minkowski(a, b, -dir)];
    let mut closest = simplex[0].w;
    let mut weights = vec![1.0];
    for _ in 0..64 {
        let dist2 = closest.length_squared();
        if dist2 < 1e-20 {
            return Separation {
                distance: 0.0,
                on_a: DVec3::ZERO,
                on_b: DVec3::ZERO,
            };
        }
        let v = minkowski(a, b, -closest);
        // No progress toward the origin: `closest` is the answer.
        let progress = dist2 - closest.dot(v.w);
        if progress <= 1e-12 * dist2.max(1e-12)
            || simplex.iter().any(|p| (p.w - v.w).length_squared() < 1e-24)
        {
            break;
        }
        simplex.push(v);
        let (p, w) = closest_on_simplex(&mut simplex);
        closest = p;
        weights = w;
        if simplex.len() == 4 && closest.length_squared() < 1e-20 {
            return Separation {
                distance: 0.0,
                on_a: DVec3::ZERO,
                on_b: DVec3::ZERO,
            };
        }
    }
    let mut on_a = DVec3::ZERO;
    let mut on_b = DVec3::ZERO;
    for (p, w) in simplex.iter().zip(&weights) {
        on_a += p.a * *w;
        on_b += p.b * *w;
    }
    Separation {
        distance: closest.length(),
        on_a,
        on_b,
    }
}

/// Whether two convex solids overlap. Solids that only touch may go either
/// way; shrink one (a negative [`Placed::margin`]) to ignore touching.
pub fn overlaps(a: &dyn Convex, b: &dyn Convex) -> bool {
    gjk_distance(a, b).distance <= 1e-9
}

/// A convex solid moved by `offset`.
pub struct Moved<'a> {
    /// The solid.
    pub inner: &'a dyn Convex,
    /// The translation.
    pub offset: DVec3,
}

impl Convex for Moved<'_> {
    fn support(&self, dir: DVec3) -> DVec3 {
        self.inner.support(dir) + self.offset
    }
    fn center(&self) -> DVec3 {
        self.inner.center() + self.offset
    }
}

/// The first contact of `moving` swept by `motion` against `target`:
/// `(fraction of the motion, contact point on target, normal of target at it)`.
/// `None` when they never touch within the motion or already overlap at the
/// start (a cast ignores what it starts in).
pub fn shape_cast(
    moving: &dyn Convex,
    target: &dyn Convex,
    motion: DVec3,
    tolerance: f64,
) -> Option<(f64, DVec3, DVec3)> {
    let len = motion.length();
    if len <= 0.0 {
        return None;
    }
    let start = gjk_distance(moving, target);
    if start.distance <= tolerance {
        return None;
    }
    // Conservative advancement: the solids cannot meet before they close the
    // current gap along the separating direction.
    let mut t = 0.0f64;
    let mut sep = start;
    for _ in 0..64 {
        let n = (sep.on_a - sep.on_b) / sep.distance.max(1e-300); // from target to moving
        let closing = -motion.dot(n);
        if closing <= 1e-12 * len {
            return None;
        }
        t += (sep.distance - tolerance * 0.5) / closing;
        if t > 1.0 {
            return None;
        }
        let moved = Moved {
            inner: moving,
            offset: motion * t,
        };
        sep = gjk_distance(&moved, target);
        if sep.distance <= tolerance {
            let normal = if sep.distance > 1e-12 {
                (sep.on_a - sep.on_b) / sep.distance
            } else {
                n
            };
            let point = if sep.distance > 1e-12 {
                sep.on_b
            } else {
                // Touching within GJK's precision: use the support point of the target
                // toward the moving solid.
                target.support(normal)
            };
            return Some((t, point, normal));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(shape: PartShape, x: f32, y: f32, z: f32) -> PartGeom {
        PartGeom::new(shape, Vec3::new(x, y, z))
    }

    #[test]
    fn volumes_and_centroids() {
        let b = geom(PartShape::Block, 2.0, 3.0, 4.0);
        assert_eq!(b.volume(), 24.0);
        let w = geom(PartShape::Wedge, 2.0, 3.0, 4.0);
        assert_eq!(w.volume(), 12.0);
        assert!((w.centroid() - Vec3::new(0.0, -0.5, 2.0 / 3.0)).length() < 1e-6);
        let c = geom(PartShape::CornerWedge, 2.0, 3.0, 4.0);
        assert!((c.volume() - 8.0).abs() < 1e-6);
        let ball = geom(PartShape::Ball, 2.0, 4.0, 4.0);
        assert!((ball.radius() - 1.0).abs() < 1e-6);
        let cyl = geom(PartShape::Cylinder, 4.0, 2.0, 3.0);
        assert!((cyl.volume() - std::f32::consts::PI * 4.0).abs() < 1e-4);
    }

    /// The centroid of each polyhedron matches a Monte Carlo estimate from its
    /// planes.
    #[test]
    fn centroids_match_sampling() {
        for shape in [PartShape::Wedge, PartShape::CornerWedge] {
            let g = geom(shape, 2.0, 3.0, 5.0);
            let mut sum = Vec3::ZERO;
            let mut n = 0u32;
            let mut seed = 12345u64;
            let mut rnd = || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 11) as f32 / (1u64 << 53) as f32
            };
            for _ in 0..200_000 {
                let p = (Vec3::new(rnd(), rnd(), rnd()) - 0.5) * g.size;
                if g.contains_local(p) {
                    sum += p;
                    n += 1;
                }
            }
            let est = sum / n as f32;
            let frac = n as f32 / 200_000.0;
            assert!(
                (est - g.centroid()).length() < 0.03,
                "{shape:?}: {est} vs {}",
                g.centroid()
            );
            assert!((frac * g.size.x * g.size.y * g.size.z - g.volume()).abs() < 0.3);
        }
    }

    #[test]
    fn ray_hits_box_faces_and_skips_from_inside() {
        let b = geom(PartShape::Block, 2.0, 2.0, 2.0);
        let (t, n) = b
            .ray_entry(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 100.0)
            .unwrap();
        assert!((t - 4.0).abs() < 1e-5);
        assert_eq!(n, -Vec3::X);
        assert!(b.ray_entry(Vec3::ZERO, Vec3::X, 100.0).is_none());
        assert!(
            b.ray_entry(Vec3::new(-5.0, 2.0, 0.0), Vec3::X, 100.0)
                .is_none()
        );
        assert!(
            b.ray_entry(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 3.0)
                .is_none()
        );
    }

    #[test]
    fn ray_hits_ball_cylinder_and_wedge_slope() {
        let ball = geom(PartShape::Ball, 2.0, 2.0, 2.0);
        let (t, n) = ball
            .ray_entry(Vec3::new(0.0, 5.0, 0.0), -Vec3::Y, 10.0)
            .unwrap();
        assert!((t - 4.0).abs() < 1e-5 && (n - Vec3::Y).length() < 1e-5);
        let cyl = geom(PartShape::Cylinder, 4.0, 2.0, 2.0);
        let (t, n) = cyl
            .ray_entry(Vec3::new(0.0, 5.0, 0.0), -Vec3::Y, 10.0)
            .unwrap();
        assert!((t - 4.0).abs() < 1e-5 && (n - Vec3::Y).length() < 1e-5);
        let (t, n) = cyl
            .ray_entry(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 10.0)
            .unwrap();
        assert!((t - 3.0).abs() < 1e-5 && (n + Vec3::X).length() < 1e-5);
        // A ray straight down onto the middle of a 2x2x2 wedge meets the slope at y = 0.
        let w = geom(PartShape::Wedge, 2.0, 2.0, 2.0);
        let (t, n) = w
            .ray_entry(Vec3::new(0.0, 5.0, 0.0), -Vec3::Y, 10.0)
            .unwrap();
        assert!((t - 5.0).abs() < 1e-5, "{t}");
        assert!(n.y > 0.0 && n.z < 0.0);
    }

    #[test]
    fn support_bounds_match_vertices() {
        let g = geom(PartShape::Wedge, 2.0, 3.0, 4.0);
        let pose = Pose::new(Vec3::new(1.0, 2.0, 3.0), Quat::from_rotation_y(0.7));
        let (min, max) = g.world_aabb(&pose);
        let verts: Vec<Vec3> = g.vertices().iter().map(|v| pose.point(*v)).collect();
        let vmin = verts.iter().fold(Vec3::splat(f32::MAX), |a, b| a.min(*b));
        let vmax = verts.iter().fold(Vec3::splat(f32::MIN), |a, b| a.max(*b));
        assert!((min - vmin).length() < 1e-4 && (max - vmax).length() < 1e-4);
    }

    #[test]
    fn gjk_distance_between_boxes() {
        let g = geom(PartShape::Block, 2.0, 2.0, 2.0);
        let a = Placed::new(g, &Pose::new(Vec3::ZERO, Quat::IDENTITY));
        let b = Placed::new(g, &Pose::new(Vec3::new(5.0, 0.0, 0.0), Quat::IDENTITY));
        let s = gjk_distance(&a, &b);
        assert!((s.distance - 3.0).abs() < 1e-6, "{}", s.distance);
        let c = Placed::new(
            g,
            &Pose::new(Vec3::new(1.5, 0.5, 0.0), Quat::from_rotation_z(0.3)),
        );
        assert!(overlaps(&a, &c));
        let ball = Placed::new(
            geom(PartShape::Ball, 2.0, 2.0, 2.0),
            &Pose::new(Vec3::new(0.0, 4.0, 0.0), Quat::IDENTITY),
        );
        assert!((gjk_distance(&a, &ball).distance - 2.0).abs() < 1e-4);
    }

    #[test]
    fn shape_cast_finds_first_contact() {
        let g = geom(PartShape::Block, 2.0, 2.0, 2.0);
        let target = Placed::new(g, &Pose::new(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY));
        let moving = Placed::new(g, &Pose::IDENTITY);
        let (t, point, normal) =
            shape_cast(&moving, &target, DVec3::new(20.0, 0.0, 0.0), 1e-5).unwrap();
        assert!((t * 20.0 - 8.0).abs() < 1e-3, "{}", t * 20.0);
        assert!((point.x - 9.0).abs() < 1e-3);
        assert!((normal - DVec3::new(-1.0, 0.0, 0.0)).length() < 1e-3);
        // Starting inside: ignored.
        let inside = Placed::new(g, &Pose::new(Vec3::new(10.5, 0.0, 0.0), Quat::IDENTITY));
        assert!(shape_cast(&inside, &target, DVec3::new(5.0, 0.0, 0.0), 1e-5).is_none());
    }

    #[test]
    fn obb_tests() {
        let a = Pose::IDENTITY;
        let h = Vec3::ONE;
        assert!(obb_overlap(
            &a,
            h,
            &Pose::new(Vec3::new(1.9, 0.0, 0.0), Quat::IDENTITY),
            h
        ));
        assert!(!obb_overlap(
            &a,
            h,
            &Pose::new(Vec3::new(2.1, 0.0, 0.0), Quat::IDENTITY),
            h
        ));
        let r = Pose::new(
            Vec3::new(2.3, 0.0, 0.0),
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_4),
        );
        assert!(obb_overlap(&a, h, &r, h));
        assert!(obb_sphere_overlap(&a, h, Vec3::new(1.5, 1.5, 0.0), 0.75));
        assert!(!obb_sphere_overlap(&a, h, Vec3::new(1.5, 1.5, 0.0), 0.7));
    }
}
