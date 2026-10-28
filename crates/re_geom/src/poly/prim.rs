//! Modelling primitives: a box with rounded edges, an icosphere, cones and
//! cylinders, a torus, stairs and an arch (ADR-0062 §7).
//!
//! Each writes box-projected UVs in metres ([`box_uv`]), smooth normals on
//! its curved faces (and then on every corner), and face groups. Angles come
//! from [`sin_cos`], positions are computed in `f64` and rounded once.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_4, PI, TAU};

use glam::Vec3;

use super::{Builder, PolyMesh, box_uv, length64, newell};
use crate::GeomError;
use crate::math::{normalize_or, sin_cos};

/// The most segments, sides or steps a primitive may have.
pub const MAX_SEGMENTS: u32 = 256;

fn bad(shape: &str, message: String) -> GeomError {
    GeomError::Argument(format!("{shape}: {message}"))
}

fn positive(shape: &str, what: &str, v: f32) -> Result<(), GeomError> {
    if v.is_finite() && v > 0.0 {
        Ok(())
    } else {
        Err(bad(
            shape,
            format!("{what} must be finite and positive, got {v}"),
        ))
    }
}

fn count(shape: &str, what: &str, v: u32, min: u32, max: u32) -> Result<(), GeomError> {
    if (min..=max).contains(&v) {
        Ok(())
    } else {
        Err(bad(
            shape,
            format!("{what} must be {min} to {max}, got {v}"),
        ))
    }
}

fn v3(x: f64, y: f64, z: f64) -> Vec3 {
    Vec3::new(x as f32, y as f32, z as f32)
}

impl Builder {
    /// A face through `points` facing along `want` (reversed when its
    /// Newell vector points the other way), with box-projected UVs and,
    /// when given, a normal per corner.
    fn face_facing(&mut self, points: &[u32], want: Vec3, normals: Option<&[Vec3]>, group: u8) {
        let n = newell(&self.positions, points);
        let dot = n[0] * f64::from(want.x) + n[1] * f64::from(want.y) + n[2] * f64::from(want.z);
        let mut pts = points.to_vec();
        let mut ns = normals.map(<[Vec3]>::to_vec);
        if dot < 0.0 {
            pts[1..].reverse();
            if let Some(ns) = &mut ns {
                ns[1..].reverse();
            }
        }
        self.face_uv(&pts, ns.as_deref(), group);
    }

    /// A face through `points` as given, with box-projected UVs along its
    /// own normal.
    fn face_uv(&mut self, points: &[u32], normals: Option<&[Vec3]>, group: u8) {
        let n = newell(&self.positions, points);
        let len = length64(n);
        let unit = v3(n[0] / len, n[1] / len, n[2] / len);
        let uv: Vec<[f32; 2]> = points
            .iter()
            .map(|&p| box_uv(self.positions[p as usize], unit, 1.0))
            .collect();
        self.face(points, &uv, normals, group);
    }
}

/// A box of full size `size`, centred on the origin, whose edges and
/// corners are rounded with `radius` (0 to half the smallest side) in
/// `segments` steps per quarter circle (an even number, 2 to 64). Radius 0
/// gives six quads. Groups: `top`, `bottom` and `side`, by the side of the
/// box a face is on.
pub fn rounded_box(size: Vec3, radius: f32, segments: u32) -> Result<PolyMesh, GeomError> {
    const SHAPE: &str = "rounded box";
    for (axis, v) in [("x", size.x), ("y", size.y), ("z", size.z)] {
        positive(SHAPE, &format!("size {axis}"), v)?;
    }
    let half = [
        f64::from(size.x) * 0.5,
        f64::from(size.y) * 0.5,
        f64::from(size.z) * 0.5,
    ];
    let max_r = half[0].min(half[1]).min(half[2]);
    let r = f64::from(radius);
    if !(radius.is_finite() && radius >= 0.0 && r <= max_r) {
        return Err(bad(
            SHAPE,
            format!("radius must be 0 to half the smallest side ({max_r}), got {radius}"),
        ));
    }
    if r > 0.0 && (!segments.is_multiple_of(2) || !(2..=64).contains(&segments)) {
        return Err(bad(
            SHAPE,
            format!("segments must be an even number from 2 to 64, got {segments}"),
        ));
    }
    // The coordinates along each axis: with rounding, the arc's points in
    // equal angles on each side of the flat middle (which is gone when the
    // radius is the whole half side).
    let m = if r > 0.0 { segments / 2 } else { 0 };
    let lines: [Vec<f64>; 3] = std::array::from_fn(|a| {
        let e = half[a] - r;
        let mut v = Vec::new();
        for k in (0..=m).rev() {
            let (s, c) = sin_cos(FRAC_PI_4 * f64::from(k) / f64::from(m.max(1)));
            v.push(-(e + r * (s / c)));
        }
        for k in 0..=m {
            let (s, c) = sin_cos(FRAC_PI_4 * f64::from(k) / f64::from(m.max(1)));
            let x = e + r * (s / c);
            if v.last() != Some(&x) {
                v.push(x);
            }
        }
        if r == 0.0 {
            v = vec![-half[a], half[a]];
        }
        v
    });
    let inner = [half[0] - r, half[1] - r, half[2] - r];
    let mut b = Builder::new();
    let mut ids: HashMap<[usize; 3], (u32, Vec3)> = HashMap::new();
    // (normal axis, sign, group)
    let sides = [
        (0usize, 1.0f64, 2u8),
        (0, -1.0, 2),
        (1, 1.0, 0),
        (1, -1.0, 1),
        (2, 1.0, 2),
        (2, -1.0, 2),
    ];
    for (a, sign, group) in sides {
        let (u, v) = ((a + 1) % 3, (a + 2) % 3);
        let fixed = if sign > 0.0 { lines[a].len() - 1 } else { 0 };
        let mut axis = [0.0f32; 3];
        axis[a] = sign as f32;
        let face_n = Vec3::from_array(axis);
        for i in 0..lines[u].len() - 1 {
            for j in 0..lines[v].len() - 1 {
                let mut cell = [[0usize; 3]; 4];
                for (k, (di, dj)) in [(0, 0), (1, 0), (1, 1), (0, 1)].into_iter().enumerate() {
                    cell[k][a] = fixed;
                    cell[k][u] = i + di;
                    cell[k][v] = j + dj;
                }
                if sign < 0.0 {
                    cell.swap(1, 3);
                }
                let mut pts = [0u32; 4];
                let mut ns = [Vec3::ZERO; 4];
                for (k, idx) in cell.iter().enumerate() {
                    let (id, n) = *ids.entry(*idx).or_insert_with(|| {
                        let q = [lines[0][idx[0]], lines[1][idx[1]], lines[2][idx[2]]];
                        let c: [f64; 3] = std::array::from_fn(|t| q[t].clamp(-inner[t], inner[t]));
                        let d = [q[0] - c[0], q[1] - c[1], q[2] - c[2]];
                        let len = length64(d);
                        let (p, n) = if r > 0.0 {
                            (
                                v3(
                                    c[0] + r * (d[0] / len),
                                    c[1] + r * (d[1] / len),
                                    c[2] + r * (d[2] / len),
                                ),
                                v3(d[0] / len, d[1] / len, d[2] / len),
                            )
                        } else {
                            (v3(q[0], q[1], q[2]), face_n)
                        };
                        (b.point(p), n)
                    });
                    pts[k] = id;
                    ns[k] = n;
                }
                let uv: Vec<[f32; 2]> = pts
                    .iter()
                    .map(|&p| box_uv(b.positions[p as usize], face_n, 1.0))
                    .collect();
                b.face(&pts, &uv, (r > 0.0).then_some(&ns[..]), group);
            }
        }
    }
    b.finish(&["top", "bottom", "side"])
}

/// A sphere of `radius` from an icosahedron whose triangles are split in
/// four `subdivisions` times (0 to 6), the new points pushed out to the
/// sphere: 20 × 4^subdivisions triangles of nearly equal size.
pub fn icosphere(radius: f32, subdivisions: u32) -> Result<PolyMesh, GeomError> {
    const SHAPE: &str = "icosphere";
    positive(SHAPE, "radius", radius)?;
    count(SHAPE, "subdivisions", subdivisions, 0, 6)?;
    let t = (1.0 + 5.0f64.sqrt()) / 2.0;
    let unit = |p: [f64; 3]| {
        let l = length64(p);
        [p[0] / l, p[1] / l, p[2] / l]
    };
    let mut pts: Vec<[f64; 3]> = [
        [-1.0, t, 0.0],
        [1.0, t, 0.0],
        [-1.0, -t, 0.0],
        [1.0, -t, 0.0],
        [0.0, -1.0, t],
        [0.0, 1.0, t],
        [0.0, -1.0, -t],
        [0.0, 1.0, -t],
        [t, 0.0, -1.0],
        [t, 0.0, 1.0],
        [-t, 0.0, -1.0],
        [-t, 0.0, 1.0],
    ]
    .map(unit)
    .to_vec();
    let mut faces: Vec<[u32; 3]> = vec![
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    for _ in 0..subdivisions {
        let mut mid: HashMap<(u32, u32), u32> = HashMap::new();
        let mut next = Vec::with_capacity(faces.len() * 4);
        for f in &faces {
            let mut m = [0u32; 3];
            for k in 0..3 {
                let (a, c) = (f[k], f[(k + 1) % 3]);
                m[k] = *mid.entry((a.min(c), a.max(c))).or_insert_with(|| {
                    let (p, q) = (pts[a as usize], pts[c as usize]);
                    pts.push(unit([p[0] + q[0], p[1] + q[1], p[2] + q[2]]));
                    pts.len() as u32 - 1
                });
            }
            next.push([f[0], m[0], m[2]]);
            next.push([f[1], m[1], m[0]]);
            next.push([f[2], m[2], m[1]]);
            next.push(m);
        }
        faces = next;
    }
    let r = f64::from(radius);
    let mut b = Builder::new();
    for p in &pts {
        b.point(v3(p[0] * r, p[1] * r, p[2] * r));
    }
    for f in &faces {
        let ns: Vec<Vec3> = f
            .iter()
            .map(|&i| {
                let p = pts[i as usize];
                v3(p[0], p[1], p[2])
            })
            .collect();
        b.face_uv(f, Some(&ns), 0);
    }
    b.finish(&[])
}

/// A cone, cylinder or frustum about +Y, centred on the origin: `radius` at
/// the bottom, `top_radius` at the top (either may be 0, for a point, but
/// not both), `height` tall, with `segments` sides (3 to 256) and, with
/// `caps`, flat ends. Groups: `side`, `top` and `bottom`.
pub fn cone(
    radius: f32,
    top_radius: f32,
    height: f32,
    segments: u32,
    caps: bool,
) -> Result<PolyMesh, GeomError> {
    const SHAPE: &str = "cone";
    for (what, v) in [("radius", radius), ("top_radius", top_radius)] {
        if !(v.is_finite() && v >= 0.0) {
            return Err(bad(
                SHAPE,
                format!("{what} must be finite and not negative, got {v}"),
            ));
        }
    }
    if radius == 0.0 && top_radius == 0.0 {
        return Err(bad(SHAPE, "radius and top_radius cannot both be 0".into()));
    }
    positive(SHAPE, "height", height)?;
    count(SHAPE, "segments", segments, 3, MAX_SEGMENTS)?;
    let (r0, r1, h) = (f64::from(radius), f64::from(top_radius), f64::from(height));
    let (y0, y1) = (-h * 0.5, h * 0.5);
    let angle = |j: u32| sin_cos(TAU * f64::from(j % segments) / f64::from(segments));
    // The side's normal at an angle: radial by the height, up by the taper.
    let side_n = |(s, c): (f64, f64)| {
        let n = [s * h, r0 - r1, c * h];
        let l = length64(n);
        v3(n[0] / l, n[1] / l, n[2] / l)
    };
    let mut b = Builder::new();
    let ring = |b: &mut Builder, r: f64, y: f64| -> Vec<u32> {
        if r == 0.0 {
            vec![b.point(v3(0.0, y, 0.0))]
        } else {
            (0..segments)
                .map(|j| {
                    let (s, c) = angle(j);
                    b.point(v3(s * r, y, c * r))
                })
                .collect()
        }
    };
    let bottom = ring(&mut b, r0, y0);
    let top = ring(&mut b, r1, y1);
    let at = |ring: &[u32], j: u32| {
        ring[if ring.len() == 1 {
            0
        } else {
            (j % segments) as usize
        }]
    };
    for j in 0..segments {
        let mid = sin_cos(TAU * (f64::from(j) + 0.5) / f64::from(segments));
        let out = side_n(mid);
        let (n0, n1) = (side_n(angle(j)), side_n(angle(j + 1)));
        let (b0, b1, t1, t0) = (
            at(&bottom, j),
            at(&bottom, j + 1),
            at(&top, j + 1),
            at(&top, j),
        );
        if bottom.len() == 1 {
            b.face_facing(&[b0, t1, t0], out, Some(&[out, n1, n0]), 0);
        } else if top.len() == 1 {
            b.face_facing(&[b0, b1, t0], out, Some(&[n0, n1, out]), 0);
        } else {
            b.face_facing(&[b0, b1, t1, t0], out, Some(&[n0, n1, n1, n0]), 0);
        }
    }
    if caps {
        if top.len() > 1 {
            b.face_facing(&top, Vec3::Y, Some(&vec![Vec3::Y; top.len()]), 1);
        }
        if bottom.len() > 1 {
            b.face_facing(
                &bottom,
                Vec3::NEG_Y,
                Some(&vec![Vec3::NEG_Y; bottom.len()]),
                2,
            );
        }
    }
    b.finish(&["side", "top", "bottom"])
}

/// A torus about +Y, centred on the origin: a tube of radius `tube` around a
/// circle of `radius` (larger than `tube`), with `segments` steps around the
/// circle and `sides` around the tube (each 3 to 256).
pub fn torus(radius: f32, tube: f32, segments: u32, sides: u32) -> Result<PolyMesh, GeomError> {
    const SHAPE: &str = "torus";
    positive(SHAPE, "radius", radius)?;
    positive(SHAPE, "tube", tube)?;
    if tube >= radius {
        return Err(bad(
            SHAPE,
            format!("tube ({tube}) must be smaller than radius ({radius})"),
        ));
    }
    count(SHAPE, "segments", segments, 3, MAX_SEGMENTS)?;
    count(SHAPE, "sides", sides, 3, MAX_SEGMENTS)?;
    let (big, small) = (f64::from(radius), f64::from(tube));
    let mut b = Builder::new();
    let mut normals = Vec::new();
    for i in 0..segments {
        let (st, ct) = sin_cos(TAU * f64::from(i) / f64::from(segments));
        for j in 0..sides {
            let (sp, cp) = sin_cos(TAU * f64::from(j) / f64::from(sides));
            let n = [cp * st, sp, cp * ct];
            b.point(v3(
                st * big + small * n[0],
                small * n[1],
                ct * big + small * n[2],
            ));
            normals.push(v3(n[0], n[1], n[2]));
        }
    }
    let id = |i: u32, j: u32| (i % segments) * sides + (j % sides);
    for i in 0..segments {
        for j in 0..sides {
            let pts = [id(i, j), id(i + 1, j), id(i + 1, j + 1), id(i, j + 1)];
            let ns = pts.map(|p| normals[p as usize]);
            b.face_uv(&pts, Some(&ns), 0);
        }
    }
    b.finish(&[])
}

/// A closed flight of `steps` stairs (1 to 256), `width` wide along X, each
/// `rise` high and `run` deep: the first riser stands on the origin facing
/// +Z and the flight climbs towards -Z. Groups: `tread`, `riser`, `side`,
/// `back` and `bottom`.
pub fn stairs(steps: u32, width: f32, rise: f32, run: f32) -> Result<PolyMesh, GeomError> {
    const SHAPE: &str = "stairs";
    count(SHAPE, "steps", steps, 1, MAX_SEGMENTS)?;
    positive(SHAPE, "width", width)?;
    positive(SHAPE, "rise", rise)?;
    positive(SHAPE, "run", run)?;
    let (w, r, d) = (f64::from(width) * 0.5, f64::from(rise), f64::from(run));
    // The profile in (z, y), counter-clockwise seen from -X.
    let mut profile = vec![(0.0, 0.0)];
    for i in 0..steps {
        let i = f64::from(i);
        profile.push((-i * d, (i + 1.0) * r));
        profile.push((-(i + 1.0) * d, (i + 1.0) * r));
    }
    let n = f64::from(steps);
    profile.push((-n * d, 0.0));
    let mut b = Builder::new();
    let left: Vec<u32> = profile
        .iter()
        .map(|&(z, y)| b.point(v3(-w, y, z)))
        .collect();
    let right: Vec<u32> = profile.iter().map(|&(z, y)| b.point(v3(w, y, z))).collect();
    let k = profile.len();
    for e in 0..k {
        let f = (e + 1) % k;
        let (dz, dy) = (profile[f].0 - profile[e].0, profile[f].1 - profile[e].1);
        // Outward is the profile edge turned clockwise (in z, y).
        let want = v3(0.0, -dz, dy);
        let group = if e == k - 1 {
            4
        } else if e == k - 2 {
            3
        } else if e % 2 == 0 {
            1
        } else {
            0
        };
        b.face_facing(&[left[e], right[e], right[f], left[f]], want, None, group);
    }
    b.face_facing(&left, Vec3::NEG_X, None, 2);
    b.face_facing(&right, Vec3::X, None, 2);
    b.finish(&["tread", "riser", "side", "back", "bottom"])
}

/// A round arch in the XY plane, `depth` deep along Z and centred on it:
/// its inner curve is a circular arc `span` wide at its feet (on y = 0) and
/// `rise` high (up to half the span, a semicircle), the ring `thickness`
/// thick, in `segments` pieces (1 to 256). Groups: `front`, `back`, `inner`,
/// `outer` and `bottom` (the feet).
pub fn arch(
    span: f32,
    rise: f32,
    thickness: f32,
    depth: f32,
    segments: u32,
) -> Result<PolyMesh, GeomError> {
    const SHAPE: &str = "arch";
    positive(SHAPE, "span", span)?;
    positive(SHAPE, "rise", rise)?;
    positive(SHAPE, "thickness", thickness)?;
    positive(SHAPE, "depth", depth)?;
    count(SHAPE, "segments", segments, 1, MAX_SEGMENTS)?;
    if rise > span * 0.5 {
        return Err(bad(
            SHAPE,
            format!("rise ({rise}) must be at most half the span ({span})"),
        ));
    }
    let (half, h, t, dz) = (
        f64::from(span) * 0.5,
        f64::from(rise),
        f64::from(thickness),
        f64::from(depth) * 0.5,
    );
    let radius = (h * h + half * half) / (2.0 * h);
    let cy = h - radius;
    let start = libm::atan2(-cy, half);
    let sweep = PI - 2.0 * start;
    let mut b = Builder::new();
    // Per piece boundary: inner front, inner back, outer front, outer back.
    let mut ring = Vec::new();
    let mut radial = Vec::new();
    for k in 0..=segments {
        let (s, c) = sin_cos(start + sweep * f64::from(k) / f64::from(segments));
        let (ix, iy) = (c * radius, cy + s * radius);
        let (ox, oy) = (c * (radius + t), cy + s * (radius + t));
        ring.push([
            b.point(v3(ix, iy, dz)),
            b.point(v3(ix, iy, -dz)),
            b.point(v3(ox, oy, dz)),
            b.point(v3(ox, oy, -dz)),
        ]);
        radial.push(v3(c, s, 0.0));
    }
    for k in 0..segments as usize {
        let (p, q) = (ring[k], ring[k + 1]);
        let (np, nq) = (radial[k], radial[k + 1]);
        let mid = normalize_or(np + nq, Vec3::Y);
        let front = [Vec3::Z; 4];
        b.face_facing(&[p[0], q[0], q[2], p[2]], Vec3::Z, Some(&front), 0);
        b.face_facing(
            &[p[1], p[3], q[3], q[1]],
            Vec3::NEG_Z,
            Some(&[Vec3::NEG_Z; 4]),
            1,
        );
        b.face_facing(
            &[p[0], p[1], q[1], q[0]],
            -mid,
            Some(&[-np, -np, -nq, -nq]),
            2,
        );
        b.face_facing(&[p[2], q[2], q[3], p[3]], mid, Some(&[np, nq, nq, np]), 3);
    }
    // The feet face away from the arch, along its curve.
    let tangent = |n: Vec3| Vec3::new(-n.y, n.x, 0.0);
    let (first, last) = (ring[0], ring[segments as usize]);
    let out0 = -tangent(radial[0]);
    b.face_facing(
        &[first[0], first[2], first[3], first[1]],
        out0,
        Some(&[out0; 4]),
        4,
    );
    let out1 = tangent(radial[segments as usize]);
    b.face_facing(
        &[last[0], last[1], last[3], last[2]],
        out1,
        Some(&[out1; 4]),
        4,
    );
    b.finish(&["front", "back", "inner", "outer", "bottom"])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed_and_outward(m: &PolyMesh) -> f64 {
        m.validate().unwrap();
        assert!(m.is_closed(), "closed");
        assert!(m.is_manifold(), "manifold");
        let v = m.volume().unwrap();
        assert!(v > 0.0, "faces point outwards: volume {v}");
        v
    }

    #[test]
    fn a_box_with_no_radius_is_six_quads() {
        let m = rounded_box(Vec3::new(2.0, 1.0, 4.0), 0.0, 2).unwrap();
        assert_eq!((m.point_count(), m.face_count()), (8, 6));
        assert_eq!(closed_and_outward(&m), 8.0);
        assert_eq!(
            m.faces.bools("top").unwrap().iter().filter(|x| **x).count(),
            1
        );
        assert!(m.corners.get(super::super::NORMAL).is_none());
    }

    #[test]
    fn a_rounded_box_approaches_its_volume() {
        let m = rounded_box(Vec3::new(2.0, 1.0, 1.0), 0.25, 8).unwrap();
        let v = closed_and_outward(&m);
        // The inner box grown by a ball of the radius: the box, slabs on its
        // faces, quarter cylinders on its edges and eighth balls at its
        // corners. The polygons lie inside the curved surface.
        let (a, b, c, r) = (1.5, 0.5, 0.5, 0.25);
        let want = a * b * c
            + 2.0 * r * (a * b + b * c + a * c)
            + PI * r * r * (a + b + c)
            + 4.0 / 3.0 * PI * r * r * r;
        assert!(v < want && want - v < 0.01, "{v} vs {want}");
        assert_eq!(m.face_count(), 6 * 9 * 9);
        // Every point lies within the box.
        let b = m.bounds().unwrap();
        assert_eq!(b.max, Vec3::new(1.0, 0.5, 0.5));
        // A radius of half the smallest side leaves no flat strip there.
        let s = rounded_box(Vec3::ONE, 0.5, 4).unwrap();
        let v = closed_and_outward(&s);
        let sphere = 4.0 / 3.0 * PI * 0.125;
        assert!((v - sphere).abs() < 0.06, "{v} vs {sphere}");
        assert!(rounded_box(Vec3::ONE, 0.25, 3).is_err());
        assert!(rounded_box(Vec3::ONE, 0.6, 2).is_err());
    }

    #[test]
    fn an_icosphere_has_twenty_times_four_to_the_n_faces() {
        for n in 0..3 {
            let m = icosphere(1.0, n).unwrap();
            assert_eq!(m.face_count(), 20 * 4usize.pow(n));
            assert_eq!(m.point_count(), 10 * 4usize.pow(n) + 2);
            closed_and_outward(&m);
        }
        let v = closed_and_outward(&icosphere(1.0, 4).unwrap());
        assert!((v - 4.0 / 3.0 * PI).abs() < 0.03, "{v}");
    }

    #[test]
    fn cones_cylinders_and_frustums_are_closed() {
        let cyl = cone(1.0, 1.0, 2.0, 64, true).unwrap();
        let v = closed_and_outward(&cyl);
        assert!((v - 2.0 * PI).abs() < 0.02, "{v}");
        assert_eq!(cyl.face_count(), 66);
        let c = cone(1.0, 0.0, 3.0, 32, true).unwrap();
        let v = closed_and_outward(&c);
        assert!((v - PI).abs() < 0.03, "{v}");
        closed_and_outward(&cone(0.5, 1.0, 1.0, 5, true).unwrap());
        closed_and_outward(&cone(0.0, 1.0, 1.0, 5, true).unwrap());
        let open = cone(1.0, 1.0, 1.0, 8, false).unwrap();
        open.validate().unwrap();
        assert!(!open.is_closed());
        assert!(cone(0.0, 0.0, 1.0, 8, true).is_err());
    }

    #[test]
    fn a_torus_has_the_volume_of_its_tube() {
        let m = torus(2.0, 0.5, 64, 32).unwrap();
        let v = closed_and_outward(&m);
        let want = 2.0 * PI * PI * 2.0 * 0.25;
        assert!((v - want).abs() / want < 0.01, "{v} vs {want}");
        assert!(torus(1.0, 1.0, 8, 8).is_err());
    }

    #[test]
    fn stairs_are_a_closed_solid() {
        let m = stairs(4, 1.0, 0.2, 0.3).unwrap();
        let v = closed_and_outward(&m);
        // Ten 0.2 × 0.3 blocks under the treads: 4 + 3 + 2 + 1.
        assert!((v - 10.0 * 0.2 * 0.3).abs() < 1e-6, "{v}");
        assert_eq!(
            m.faces
                .bools("tread")
                .unwrap()
                .iter()
                .filter(|x| **x)
                .count(),
            4
        );
        assert_eq!(
            m.faces
                .bools("riser")
                .unwrap()
                .iter()
                .filter(|x| **x)
                .count(),
            4
        );
        let mesh = m.to_mesh().unwrap();
        assert!(mesh.is_well_formed());
    }

    #[test]
    fn an_arch_is_a_closed_ring_piece() {
        let m = arch(2.0, 1.0, 0.3, 0.5, 16).unwrap();
        let v = closed_and_outward(&m);
        // Half an annulus, 0.5 deep.
        let want = 0.5 * PI * (1.3f64 * 1.3 - 1.0) * 0.5;
        assert!((v - want).abs() / want < 0.01, "{v} vs {want}");
        let b = m.bounds().unwrap();
        assert!(b.min.y.abs() < 1e-6 && (b.max.y - 1.3).abs() < 1e-6);
        // A segmental arch: lower than a semicircle.
        closed_and_outward(&arch(4.0, 1.0, 0.3, 0.5, 8).unwrap());
        assert!(arch(2.0, 1.5, 0.3, 0.5, 8).is_err());
    }
}
