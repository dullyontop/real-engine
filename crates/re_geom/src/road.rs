//! Roads: centreline splines, ribbons and junctions (ADR-0043 §5).
//!
//! A road starts as [`ControlPoint`]s. [`spline`] runs a centripetal
//! Catmull-Rom spline through them and samples it at a fixed arc spacing,
//! giving a [`Path`] whose samples carry the interpolated width and bank.
//! [`road_mesh`] turns paths into one surface: a ribbon per road, cut back
//! around junctions, plus a flat surface for each junction. [`footprint`]
//! gives the area the roads cover, for masks and booleans.
//!
//! Everything runs in `f64` with `+ - * /` and `sqrt` only, angles from
//! [`math::sin_cos`](crate::math::sin_cos), and no fused multiply-add, so the
//! output is the same on every platform. Coordinates are in the source's
//! local space, Y up; a road lies along X and Z.

use re_types::mesh::{MeshData, Vertex};
use serde::{Deserialize, Serialize};

use crate::GeomError;
use crate::polygon::{self, MAX_COORD, Shape, Shapes};

/// The most samples one path may have.
pub const MAX_SAMPLES: usize = 1_000_000;

/// One control point of a road's centreline.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlPoint {
    /// Position `[x, y, z]` in metres.
    pub position: [f64; 3],
    /// The road's full width here, in metres.
    pub width: f64,
    /// The road's bank here, in degrees: positive raises the left edge.
    pub bank: f64,
}

/// One sample of a [`Path`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathSample {
    /// Position `[x, y, z]` in metres.
    pub position: [f64; 3],
    /// The full width, in metres.
    pub width: f64,
    /// The bank, in degrees: positive raises the left edge.
    pub bank: f64,
    /// Distance along the path from its first sample, in metres.
    pub distance: f64,
}

/// A road centreline sampled at a fixed spacing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Path {
    /// The samples, in order.
    pub samples: Vec<PathSample>,
    /// True if the last sample joins back to the first.
    pub closed: bool,
}

/// How [`road_mesh`] handles junctions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoadStyle {
    /// Roads are cut back this far (metres, in X and Z) around a junction.
    pub junction_radius: f64,
    /// Road ends this close to another road (metres) make a junction.
    pub snap: f64,
    /// Over this distance (metres) before a junction, a road's bank fades
    /// to 0 and its height eases to the junction's.
    pub bank_fade: f64,
}

/// What [`road_mesh`] made.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Road {
    /// The road surface: ribbons and junctions, facing up.
    pub mesh: MeshData,
    /// Each junction's centre `[x, y, z]`, `y` being its surface height.
    pub junctions: Vec<[f64; 3]>,
}

fn bad(msg: String) -> GeomError {
    GeomError::Polygon(msg)
}

fn finite(v: f64, what: &str) -> Result<(), GeomError> {
    if !v.is_finite() {
        return Err(bad(format!("{what} is not a finite number")));
    }
    if v.abs() > MAX_COORD {
        return Err(GeomError::OutOfRange(format!(
            "{what} ({v}) is beyond ±{MAX_COORD} m"
        )));
    }
    Ok(())
}

fn dist3(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (x, y, z) = (b[0] - a[0], b[1] - a[1], b[2] - a[2]);
    (x * x + y * y + z * z).sqrt()
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A point on the centripetal Catmull-Rom segment from `p[1]` to `p[2]`
/// (Barry and Goldman's form), at `t` between knots `k[1]` and `k[2]`.
fn catmull_rom(p: [[f64; 3]; 4], k: [f64; 4], t: f64) -> [f64; 3] {
    let mix = |a: [f64; 3], b: [f64; 3], ka: f64, kb: f64| {
        let d = kb - ka;
        if d <= 0.0 {
            return a;
        }
        lerp3(a, b, (t - ka) / d)
    };
    let a1 = mix(p[0], p[1], k[0], k[1]);
    let a2 = mix(p[1], p[2], k[1], k[2]);
    let a3 = mix(p[2], p[3], k[2], k[3]);
    let b1 = mix(a1, a2, k[0], k[2]);
    let b2 = mix(a2, a3, k[1], k[3]);
    mix(b1, b2, k[1], k[2])
}

/// A dense point of a spline: position, width, bank, length so far.
type Dense = ([f64; 3], f64, f64, f64);

/// The centreline through `points`, sampled every `spacing` metres along it
/// (the last sample sits on the last point). `closed` joins the last point
/// back to the first. Width and bank are interpolated linearly between
/// control points.
pub fn spline(points: &[ControlPoint], spacing: f64, closed: bool) -> Result<Path, GeomError> {
    let min = if closed { 3 } else { 2 };
    if points.len() < min {
        return Err(bad(format!(
            "a{} path needs at least {min} points, got {}",
            if closed { " closed" } else { "n open" },
            points.len()
        )));
    }
    if !spacing.is_finite() || spacing <= 0.0 {
        return Err(bad(format!("spacing must be positive, got {spacing}")));
    }
    for (i, c) in points.iter().enumerate() {
        for v in c.position {
            finite(v, &format!("point {i}"))?;
        }
        finite(c.bank, &format!("point {i}'s bank"))?;
        if !c.width.is_finite() || c.width <= 0.0 {
            return Err(bad(format!(
                "point {i}'s width must be positive, got {}",
                c.width
            )));
        }
    }
    let n = points.len();
    let segments = if closed { n } else { n - 1 };
    let at = |i: isize| -> [f64; 3] {
        if closed {
            return points[i.rem_euclid(n as isize) as usize].position;
        }
        // Open ends: reflect the neighbour so the end tangents follow the
        // first and last segments.
        if i < 0 {
            let (a, b) = (points[0].position, points[1].position);
            return [2.0 * a[0] - b[0], 2.0 * a[1] - b[1], 2.0 * a[2] - b[2]];
        }
        if i as usize >= n {
            let (a, b) = (points[n - 1].position, points[n - 2].position);
            return [2.0 * a[0] - b[0], 2.0 * a[1] - b[1], 2.0 * a[2] - b[2]];
        }
        points[i as usize].position
    };
    // A dense polyline first, then even samples along it.
    let mut dense: Vec<Dense> = Vec::new();
    for s in 0..segments {
        let i = s as isize;
        let p = [at(i - 1), at(i), at(i + 1), at(i + 2)];
        let chord = dist3(p[1], p[2]);
        if chord <= 1e-9 {
            return Err(bad(format!(
                "points {s} and {} are at the same place",
                (s + 1) % n
            )));
        }
        let steps = ((chord / spacing).ceil() * 8.0).clamp(8.0, 4096.0) as usize;
        if dense.len() + steps > MAX_SAMPLES * 8 {
            return Err(bad(format!(
                "the path is too long for a spacing of {spacing} m"
            )));
        }
        let mut k = [0.0; 4];
        for j in 1..4 {
            // Centripetal: knot gaps are the square roots of the distances.
            k[j] = k[j - 1] + dist3(p[j - 1], p[j]).sqrt().max(1e-12);
        }
        let (c0, c1) = (points[s], points[(s + 1) % n]);
        let first = if dense.is_empty() { 0 } else { 1 };
        for j in first..=steps {
            let u = j as f64 / steps as f64;
            let pos = catmull_rom(p, k, lerp(k[1], k[2], u));
            let length = match dense.last() {
                Some(last) => last.3 + dist3(last.0, pos),
                None => 0.0,
            };
            dense.push((
                pos,
                lerp(c0.width, c1.width, u),
                lerp(c0.bank, c1.bank, u),
                length,
            ));
        }
    }
    let total = dense.last().map_or(0.0, |d| d.3);
    let count = (total / spacing).floor() + 1.0;
    if count > MAX_SAMPLES as f64 {
        return Err(bad(format!(
            "the path would have {count} samples (at most {MAX_SAMPLES}); use a larger spacing"
        )));
    }
    let count = count as usize;
    let mut samples = Vec::with_capacity(count + 1);
    let mut j = 0;
    for i in 0..count {
        let d = i as f64 * spacing;
        if closed && i > 0 && d >= total - spacing * 1e-6 {
            break;
        }
        while j + 2 < dense.len() && dense[j + 1].3 < d {
            j += 1;
        }
        samples.push(sample_between(
            &dense[j],
            &dense[(j + 1).min(dense.len() - 1)],
            d,
        ));
    }
    if !closed {
        let end = dense[dense.len() - 1];
        let last = samples.last().map_or(-1.0, |s| s.distance);
        // A last regular sample within a thousandth of the spacing of the
        // end is replaced by the end itself.
        if total - last < spacing * 1e-3 {
            samples.pop();
        }
        samples.push(PathSample {
            position: end.0,
            width: end.1,
            bank: end.2,
            distance: total,
        });
    }
    Ok(Path { samples, closed })
}

fn sample_between(a: &Dense, b: &Dense, d: f64) -> PathSample {
    let span = b.3 - a.3;
    let t = if span > 0.0 {
        ((d - a.3) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    PathSample {
        position: lerp3(a.0, b.0, t),
        width: lerp(a.1, b.1, t),
        bank: lerp(a.2, b.2, t),
        distance: d,
    }
}

impl Path {
    /// Checks the path: 2 samples or more (3 if closed), finite and within
    /// range, with positive widths (`RE4504`, `RE4503`).
    pub fn validate(&self) -> Result<(), GeomError> {
        let min = if self.closed { 3 } else { 2 };
        if self.samples.len() < min {
            return Err(bad(format!(
                "a path needs at least {min} samples, got {}",
                self.samples.len()
            )));
        }
        if self.samples.len() > MAX_SAMPLES {
            return Err(bad(format!(
                "a path may have at most {MAX_SAMPLES} samples, got {}",
                self.samples.len()
            )));
        }
        for (i, s) in self.samples.iter().enumerate() {
            for v in s.position {
                finite(v, &format!("sample {i}"))?;
            }
            finite(s.bank, &format!("sample {i}'s bank"))?;
            if !s.distance.is_finite() {
                return Err(bad(format!("sample {i}'s distance is not finite")));
            }
            if !s.width.is_finite() || s.width <= 0.0 {
                return Err(bad(format!(
                    "sample {i}'s width must be positive, got {}",
                    s.width
                )));
            }
        }
        Ok(())
    }

    /// The length along the samples, in metres.
    pub fn length(&self) -> f64 {
        self.segments()
            .map(|(a, b)| dist3(a.position, b.position))
            .sum()
    }

    /// The segments `(a, b)` between consecutive samples (and back to the
    /// first, when closed).
    pub fn segments(&self) -> impl Iterator<Item = (&PathSample, &PathSample)> {
        let n = self.samples.len();
        let count = if self.closed { n } else { n.saturating_sub(1) };
        (0..count).map(move |i| (&self.samples[i], &self.samples[(i + 1) % n]))
    }

    /// The area the road covers (its ribbon, banking ignored), simplified.
    pub fn footprint(&self) -> Result<Shapes, GeomError> {
        self.validate()?;
        let tangents = tangents(self);
        let mut st: Vec<Station> = self
            .samples
            .iter()
            .zip(&tangents)
            .map(|(s, t)| Station {
                sample: *s,
                tangent: *t,
            })
            .collect();
        if self.closed {
            st.push(st[0]);
        }
        swept(&st)
    }
}

/// The union of the paths' footprints, grown by `margin` metres (0 for none).
pub fn footprint(paths: &[Path], margin: f64) -> Result<Shapes, GeomError> {
    let prints = paths
        .iter()
        .map(Path::footprint)
        .collect::<Result<Vec<_>, _>>()?;
    let all = polygon::union_all(&prints)?;
    if margin == 0.0 {
        return Ok(all);
    }
    all.offset(margin, polygon::Join::Round)
}

/// Unit tangents in X and Z at each sample, from its neighbours.
fn tangents(path: &Path) -> Vec<[f64; 2]> {
    let s = &path.samples;
    let n = s.len();
    (0..n)
        .map(|i| {
            let (a, b) = if path.closed {
                (s[(i + n - 1) % n].position, s[(i + 1) % n].position)
            } else {
                (
                    s[i.saturating_sub(1)].position,
                    s[(i + 1).min(n - 1)].position,
                )
            };
            unit([b[0] - a[0], b[2] - a[2]])
        })
        .collect()
}

fn unit(v: [f64; 2]) -> [f64; 2] {
    let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if len > 0.0 {
        [v[0] / len, v[1] / len]
    } else {
        [1.0, 0.0]
    }
}

/// The left and right edge of a sample whose direction in X and Z is `t`.
fn edges(s: &PathSample, t: [f64; 2]) -> ([f64; 3], [f64; 3]) {
    // Right of the direction of travel, Y up: (-tz, tx).
    let r = [-t[1], t[0]];
    let (sin, cos) = crate::math::sin_cos(s.bank * (std::f64::consts::PI / 180.0));
    let half = s.width * 0.5;
    let (dx, dy, dz) = (r[0] * half * cos, half * sin, r[1] * half * cos);
    let p = s.position;
    (
        [p[0] - dx, p[1] + dy, p[2] - dz],
        [p[0] + dx, p[1] - dy, p[2] + dz],
    )
}

/// A sample on a road with its direction in X and Z.
#[derive(Clone, Copy, Debug)]
struct Station {
    sample: PathSample,
    tangent: [f64; 2],
}

/// The closest point to `p` on segment `a`–`b`: `(t, distance²)`.
fn closest_on_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> (f64, f64) {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dz * dz;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (x, z) = (a[0] + dx * t - p[0], a[1] + dz * t - p[1]);
    (t, x * x + z * z)
}

/// Where segments `a0`–`a1` and `b0`–`b1` cross, if they do.
fn crossing(a0: [f64; 2], a1: [f64; 2], b0: [f64; 2], b1: [f64; 2]) -> Option<[f64; 2]> {
    let (rx, rz) = (a1[0] - a0[0], a1[1] - a0[1]);
    let (sx, sz) = (b1[0] - b0[0], b1[1] - b0[1]);
    let den = rx * sz - rz * sx;
    if den == 0.0 {
        return None;
    }
    let (qx, qz) = (b0[0] - a0[0], b0[1] - a0[1]);
    let t = (qx * sz - qz * sx) / den;
    let u = (qx * rz - qz * rx) / den;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then(|| [a0[0] + rx * t, a0[1] + rz * t])
}

fn xz(p: [f64; 3]) -> [f64; 2] {
    [p[0], p[2]]
}

fn bbox(a: [f64; 2], b: [f64; 2], pad: f64) -> [f64; 4] {
    [
        a[0].min(b[0]) - pad,
        a[1].min(b[1]) - pad,
        a[0].max(b[0]) + pad,
        a[1].max(b[1]) + pad,
    ]
}

/// A segment in X and Z with its bounding box.
type Segment = ([f64; 2], [f64; 2], [f64; 4]);

fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// Junction centres in X and Z: crossings between different paths, and
/// open ends within `snap` of another path. Points closer than twice the
/// radius are merged into their mean, so junction discs never overlap.
fn junction_centres(paths: &[Path], style: &RoadStyle) -> Vec<[f64; 2]> {
    let mut points: Vec<[f64; 2]> = Vec::new();
    let segs: Vec<Vec<Segment>> = paths
        .iter()
        .map(|p| {
            p.segments()
                .map(|(a, b)| {
                    let (a, b) = (xz(a.position), xz(b.position));
                    (a, b, bbox(a, b, 0.0))
                })
                .collect()
        })
        .collect();
    for i in 0..paths.len() {
        for j in i + 1..paths.len() {
            for (a0, a1, ba) in &segs[i] {
                for (b0, b1, bb) in &segs[j] {
                    if overlaps(*ba, *bb)
                        && let Some(c) = crossing(*a0, *a1, *b0, *b1)
                    {
                        points.push(c);
                    }
                }
            }
        }
    }
    let snap2 = style.snap * style.snap;
    for (i, path) in paths.iter().enumerate() {
        if path.closed || style.snap <= 0.0 {
            continue;
        }
        let ends = [path.samples[0], path.samples[path.samples.len() - 1]];
        for end in ends {
            let e = xz(end.position);
            let reach = bbox(e, e, style.snap);
            for (j, other) in segs.iter().enumerate() {
                if j == i {
                    continue;
                }
                let mut best: Option<(f64, [f64; 2])> = None;
                for (a, b, bb) in other {
                    if !overlaps(reach, *bb) {
                        continue;
                    }
                    let (t, d2) = closest_on_segment(e, *a, *b);
                    if d2 <= snap2 && best.is_none_or(|(bd, _)| d2 < bd) {
                        best = Some((d2, [lerp(a[0], b[0], t), lerp(a[1], b[1], t)]));
                    }
                }
                if let Some((_, c)) = best {
                    points.push([(e[0] + c[0]) * 0.5, (e[1] + c[1]) * 0.5]);
                }
            }
        }
    }
    // Merge, in order, until no two centres are within twice the radius.
    let reach = 2.0 * style.junction_radius;
    let mut clusters: Vec<(f64, f64, f64)> = Vec::new();
    for p in points {
        merge_point(&mut clusters, p, 1.0, reach);
    }
    loop {
        let before = clusters.len();
        let old = std::mem::take(&mut clusters);
        for (x, z, w) in old {
            merge_point(&mut clusters, [x / w, z / w], w, reach);
        }
        if clusters.len() == before {
            break;
        }
    }
    clusters
        .into_iter()
        .map(|(x, z, w)| [x / w, z / w])
        .collect()
}

/// Adds `p` (with weight `w`) to the first cluster whose mean is within
/// `reach`, or starts a new one. Clusters are `(Σx, Σz, Σw)`.
fn merge_point(clusters: &mut Vec<(f64, f64, f64)>, p: [f64; 2], w: f64, reach: f64) {
    for c in clusters.iter_mut() {
        let (dx, dz) = (c.0 / c.2 - p[0], c.1 / c.2 - p[1]);
        if dx * dx + dz * dz < reach * reach {
            c.0 += p[0] * w;
            c.1 += p[1] * w;
            c.2 += w;
            return;
        }
    }
    clusters.push((p[0] * w, p[1] * w, w));
}

/// The parameter range `[t0, t1]` of segment `a`–`b` inside the disc.
fn disc_interval(a: [f64; 2], b: [f64; 2], c: [f64; 2], r: f64) -> Option<(f64, f64)> {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let (fx, fz) = (a[0] - c[0], a[1] - c[1]);
    let qa = dx * dx + dz * dz;
    let qb = 2.0 * (fx * dx + fz * dz);
    let qc = fx * fx + fz * fz - r * r;
    if qa == 0.0 {
        return (qc < 0.0).then_some((0.0, 1.0));
    }
    let disc = qb * qb - 4.0 * qa * qc;
    if disc <= 0.0 {
        return None;
    }
    let root = disc.sqrt();
    let t0 = ((-qb - root) / (2.0 * qa)).max(0.0);
    let t1 = ((-qb + root) / (2.0 * qa)).min(1.0);
    (t0 < t1).then_some((t0, t1))
}

/// A stretch of a path inside a junction.
struct Run {
    stations: Vec<Station>,
    junction: usize,
}

/// A stretch of a path outside every junction, with the junction (if any)
/// at each end.
struct Piece {
    stations: Vec<Station>,
    ends: [Option<usize>; 2],
}

/// The station at parameter `s` (segment index plus fraction).
fn station_at(path: &Path, tangents: &[[f64; 2]], s: f64) -> Station {
    let n = path.samples.len();
    let last = if path.closed { n - 1 } else { n - 2 };
    let i = (s.floor().max(0.0) as usize).min(last);
    let t = s - i as f64;
    let (a, b) = (&path.samples[i], &path.samples[(i + 1) % n]);
    if t <= 0.0 {
        return Station {
            sample: *a,
            tangent: tangents[i],
        };
    }
    if t >= 1.0 {
        let mut sample = *b;
        if i + 1 == n {
            sample.distance = path.length();
        }
        return Station {
            sample,
            tangent: tangents[(i + 1) % n],
        };
    }
    Station {
        sample: PathSample {
            position: lerp3(a.position, b.position, t),
            width: lerp(a.width, b.width, t),
            bank: lerp(a.bank, b.bank, t),
            distance: a.distance + dist3(a.position, b.position) * t,
        },
        tangent: unit([b.position[0] - a.position[0], b.position[2] - a.position[2]]),
    }
}

/// The stations of `path` from parameter `s0` to `s1`. On a closed path,
/// `s1 <= s0` wraps past the end, and distances keep growing.
fn stations(path: &Path, tangents: &[[f64; 2]], s0: f64, s1: f64) -> Vec<Station> {
    let n = path.samples.len();
    let m = if path.closed { n } else { n - 1 } as f64;
    let end = if path.closed && s1 <= s0 { s1 + m } else { s1 };
    let length = path.length();
    let mut out = vec![station_at(path, tangents, s0)];
    let mut k = s0.floor() + 1.0;
    while k < end {
        let lap = if k >= m { length } else { 0.0 };
        let idx = (k as usize) % n;
        let mut sample = path.samples[idx];
        sample.distance += lap;
        push_station(
            &mut out,
            Station {
                sample,
                tangent: tangents[idx],
            },
        );
        k += 1.0;
    }
    let mut last = if end > m {
        station_at(path, tangents, end - m)
    } else {
        station_at(path, tangents, end)
    };
    if end > m {
        last.sample.distance += length;
    }
    if out.len() > 1 && last.sample.distance - out[out.len() - 1].sample.distance < SAME {
        out.pop();
    }
    push_station(&mut out, last);
    out
}

/// Stations closer than this along a path (metres) are one station.
const SAME: f64 = 1e-6;

/// Appends `st` unless it sits on the last station.
fn push_station(out: &mut Vec<Station>, st: Station) {
    if out
        .last()
        .is_none_or(|l| st.sample.distance - l.sample.distance >= SAME)
    {
        out.push(st);
    }
}

/// The road surface for `paths`: each road's ribbon (UVs across, 0 to 1,
/// and along, in metres), cut back by `junction_radius` around every
/// junction, and each junction's surface (the roads' footprints inside its
/// radius, flat at the mean height of the roads where they were cut, UVs
/// in metres).
///
/// A curve tighter than half the road's width folds the ribbon's inner edge
/// back on itself; [`footprint`] still covers the right area, but the mesh
/// has overlapping triangles there, so control points should keep curves
/// wider than that.
pub fn road_mesh(paths: &[Path], style: &RoadStyle) -> Result<Road, GeomError> {
    for (name, v) in [
        ("junction_radius", style.junction_radius),
        ("snap", style.snap),
        ("bank_fade", style.bank_fade),
    ] {
        if !v.is_finite() || v < 0.0 {
            return Err(bad(format!("{name} must be 0 or more, got {v}")));
        }
    }
    for p in paths {
        p.validate()?;
    }
    let centres = if style.junction_radius > 0.0 {
        junction_centres(paths, style)
    } else {
        Vec::new()
    };
    let r = style.junction_radius;
    let mut runs: Vec<Run> = Vec::new();
    let mut pieces: Vec<Piece> = Vec::new();
    for path in paths {
        let tangents = tangents(path);
        let n = path.samples.len();
        let m = if path.closed { n } else { n - 1 } as f64;
        // Inside intervals along the path parameter, with their junction.
        let mut inside: Vec<(f64, f64, usize)> = Vec::new();
        for (i, (a, b)) in path.segments().enumerate() {
            let (a, b) = (xz(a.position), xz(b.position));
            for (j, c) in centres.iter().enumerate() {
                if let Some((t0, t1)) = disc_interval(a, b, *c, r) {
                    let (s0, s1) = (i as f64 + t0, i as f64 + t1);
                    match inside.last_mut() {
                        Some(last) if last.2 == j && last.1 >= s0 => last.1 = last.1.max(s1),
                        _ => inside.push((s0, s1, j)),
                    }
                }
            }
        }
        if path.closed && inside.len() > 1 {
            let (first, last) = (inside[0], inside[inside.len() - 1]);
            if first.0 <= 0.0 && last.1 >= m && first.2 == last.2 {
                inside.pop();
                inside[0].0 = last.0;
            }
        }
        for &(s0, s1, j) in &inside {
            runs.push(Run {
                stations: stations(path, &tangents, s0, if s1 >= m { m } else { s1 }),
                junction: j,
            });
        }
        if inside.is_empty() {
            pieces.push(Piece {
                stations: stations(path, &tangents, 0.0, m),
                ends: [None, None],
            });
        } else if path.closed {
            for k in 0..inside.len() {
                let (_, e, j0) = inside[k];
                let (s, _, j1) = inside[(k + 1) % inside.len()];
                let e = if e >= m { e - m } else { e };
                pieces.push(Piece {
                    stations: stations(path, &tangents, e, s),
                    ends: [Some(j0), Some(j1)],
                });
            }
        } else {
            let mut from = (0.0, None);
            for &(s0, s1, j) in &inside {
                if s0 > from.0 {
                    pieces.push(Piece {
                        stations: stations(path, &tangents, from.0, s0),
                        ends: [from.1, Some(j)],
                    });
                }
                from = (s1, Some(j));
            }
            if from.0 < m {
                pieces.push(Piece {
                    stations: stations(path, &tangents, from.0, m),
                    ends: [from.1, None],
                });
            }
        }
    }
    // Junction heights: the mean height where roads meet them.
    let mut sums = vec![(0.0, 0usize); centres.len()];
    for run in &runs {
        for st in [run.stations[0], run.stations[run.stations.len() - 1]] {
            sums[run.junction].0 += st.sample.position[1];
            sums[run.junction].1 += 1;
        }
    }
    let heights: Vec<f64> = sums
        .iter()
        .map(|&(sum, n)| if n > 0 { sum / n as f64 } else { 0.0 })
        .collect();
    let mut meshes: Vec<MeshData> = pieces
        .iter()
        .map(|p| ribbon_mesh(p, &heights, style.bank_fade))
        .collect();
    let mut junctions = Vec::new();
    for (j, c) in centres.iter().enumerate() {
        let prints = runs
            .iter()
            .filter(|r| r.junction == j)
            .map(|r| swept(&r.stations))
            .collect::<Result<Vec<_>, _>>()?;
        if prints.is_empty() {
            continue;
        }
        let area = polygon::union_all(&prints)?;
        meshes.push(polygon::triangulate(&area, heights[j] as f32)?);
        junctions.push([c[0], heights[j], c[1]]);
    }
    let mesh = crate::merge(meshes.iter())?;
    Ok(Road { mesh, junctions })
}

/// The flat area swept by a run of stations: the union of the triangles
/// between consecutive edges. Triangles never cross themselves, so a curve
/// tighter than half the road's width still gives the area it covers.
fn swept(stations: &[Station]) -> Result<Shapes, GeomError> {
    let flat: Vec<([f64; 2], [f64; 2])> = stations
        .iter()
        .map(|st| {
            let mut s = st.sample;
            s.bank = 0.0;
            let (l, r) = edges(&s, st.tangent);
            ([l[0], l[2]], [r[0], r[2]])
        })
        .collect();
    let area2 = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])
    };
    let mut tris = Shapes::new();
    for w in flat.windows(2) {
        let ((l0, r0), (l1, r1)) = (w[0], w[1]);
        for t in [[l0, r0, l1], [r0, r1, l1]] {
            if area2(t[0], t[1], t[2]) != 0.0 {
                tris.shapes.push(Shape {
                    outer: t.to_vec(),
                    holes: Vec::new(),
                });
            }
        }
    }
    tris.simplified()
}

/// A piece's ribbon: two vertices per station. Within `fade` metres of a
/// junction end, the bank fades to 0 and the height eases to the junction's.
fn ribbon_mesh(piece: &Piece, heights: &[f64], fade: f64) -> MeshData {
    let st = &piece.stations;
    let (d0, d1) = (st[0].sample.distance, st[st.len() - 1].sample.distance);
    let mut vertices = Vec::with_capacity(st.len() * 2);
    for s in st {
        let mut sample = s.sample;
        for (end, at) in [(piece.ends[0], d0), (piece.ends[1], d1)] {
            let Some(j) = end else { continue };
            let w = if fade > 0.0 {
                smoothstep((sample.distance - at).abs() / fade)
            } else {
                1.0
            };
            sample.bank *= w;
            sample.position[1] = lerp(heights[j], sample.position[1], w);
        }
        let (l, r) = edges(&sample, s.tangent);
        let v = sample.distance as f32;
        for (p, u) in [(l, 0.0), (r, 1.0)] {
            vertices.push(Vertex {
                pos: [p[0] as f32, p[1] as f32, p[2] as f32],
                normal: [0.0, 1.0, 0.0],
                uv: [u, v],
                color: [1.0; 4],
            });
        }
    }
    let mut indices = Vec::with_capacity(st.len().saturating_sub(1) * 6);
    for i in 0..st.len().saturating_sub(1) as u32 {
        let (l0, r0, l1, r1) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
        indices.extend([l0, r0, l1, r0, r1, l1]);
    }
    let mut mesh = MeshData { vertices, indices };
    crate::compute_normals(&mut mesh);
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(points: &[[f64; 3]], width: f64) -> Path {
        let cps: Vec<ControlPoint> = points
            .iter()
            .map(|p| ControlPoint {
                position: *p,
                width,
                bank: 0.0,
            })
            .collect();
        spline(&cps, 1.0, false).unwrap()
    }

    fn upward_area(mesh: &MeshData) -> f64 {
        assert!(mesh.is_well_formed());
        let mut area = 0.0;
        for t in mesh.indices.as_chunks::<3>().0 {
            let p = t.map(|i| mesh.vertices[i as usize].pos);
            let up = (p[1][2] - p[0][2]) * (p[2][0] - p[0][0])
                - (p[1][0] - p[0][0]) * (p[2][2] - p[0][2]);
            assert!(up > 0.0, "{p:?} faces down");
            area += f64::from(up) * 0.5;
        }
        area
    }

    #[test]
    fn a_straight_spline_is_evenly_sampled() {
        let p = line(&[[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [20.0, 0.0, 0.0]], 4.0);
        assert_eq!(p.samples.len(), 21);
        for (i, s) in p.samples.iter().enumerate() {
            assert!((s.position[0] - i as f64).abs() < 1e-9, "{s:?}");
            assert!(s.position[2].abs() < 1e-9);
            assert!((s.distance - i as f64).abs() < 1e-9);
        }
        assert!((p.footprint().unwrap().area() - 80.0).abs() < 1e-3);
    }

    #[test]
    fn a_closed_spline_loops() {
        // Eight points on a circle of radius 20.
        let cps: Vec<ControlPoint> = (0..8)
            .map(|i| {
                let (s, c) = crate::math::sin_cos(f64::from(i) * std::f64::consts::FRAC_PI_4);
                [20.0 * c, 20.0 * s]
            })
            .map(|p| ControlPoint {
                position: [p[0], 0.0, p[1]],
                width: 2.0,
                bank: 0.0,
            })
            .collect();
        let p = spline(&cps, 1.0, true).unwrap();
        assert!(p.closed);
        let print = p.footprint().unwrap();
        assert_eq!(print.shapes.len(), 1, "{print:?}");
        assert_eq!(print.shapes[0].holes.len(), 1);
        let area = print.area();
        assert!((area - 2.0 * p.length()).abs() < 0.05 * area, "{area}");
        let road = road_mesh(
            &[p],
            &RoadStyle {
                junction_radius: 5.0,
                snap: 2.0,
                bank_fade: 4.0,
            },
        )
        .unwrap();
        assert!(road.junctions.is_empty());
        assert!((upward_area(&road.mesh) - area).abs() < 0.05 * area);
    }

    #[test]
    fn crossing_roads_make_one_junction_with_no_gap_or_overlap() {
        let a = line(&[[-30.0, 0.0, 0.0], [30.0, 0.0, 0.0]], 6.0);
        let b = line(&[[0.0, 2.0, -30.0], [0.0, 2.0, 30.0]], 6.0);
        let style = RoadStyle {
            junction_radius: 8.0,
            snap: 2.0,
            bank_fade: 6.0,
        };
        let road = road_mesh(&[a.clone(), b.clone()], &style).unwrap();
        assert_eq!(road.junctions.len(), 1);
        let j = road.junctions[0];
        assert!(j[0].abs() < 1e-9 && j[2].abs() < 1e-9, "{j:?}");
        assert!((j[1] - 1.0).abs() < 1e-9, "{j:?}");
        let area = upward_area(&road.mesh);
        let print = footprint(&[a, b], 0.0).unwrap().area();
        assert!((area - print).abs() < 0.01 * print, "{area} vs {print}");
    }

    #[test]
    fn a_road_ending_near_another_makes_a_t_junction() {
        let a = line(&[[-30.0, 0.0, 0.0], [30.0, 0.0, 0.0]], 6.0);
        let b = line(&[[0.0, 0.0, 4.5], [0.0, 0.0, 40.0]], 6.0);
        let style = RoadStyle {
            junction_radius: 7.0,
            snap: 5.0,
            bank_fade: 6.0,
        };
        let road = road_mesh(&[a.clone(), b.clone()], &style).unwrap();
        assert_eq!(road.junctions.len(), 1);
        upward_area(&road.mesh);
        let none = RoadStyle { snap: 0.0, ..style };
        assert!(road_mesh(&[a, b], &none).unwrap().junctions.is_empty());
    }

    #[test]
    fn bank_raises_the_left_edge() {
        let cp = |x: f64| ControlPoint {
            position: [x, 0.0, 0.0],
            width: 4.0,
            bank: 10.0,
        };
        let p = spline(&[cp(0.0), cp(10.0)], 1.0, false).unwrap();
        let style = RoadStyle {
            junction_radius: 0.0,
            snap: 0.0,
            bank_fade: 0.0,
        };
        let road = road_mesh(&[p], &style).unwrap();
        let v = &road.mesh.vertices;
        // Left of travel along +X is -Z.
        assert!(v[0].pos[2] < 0.0 && v[0].pos[1] > 0.3, "{:?}", v[0]);
        assert!(v[1].pos[2] > 0.0 && v[1].pos[1] < -0.3, "{:?}", v[1]);
        assert_eq!((v[0].uv[0], v[1].uv[0]), (0.0, 1.0));
    }

    #[test]
    fn bad_paths_are_refused() {
        let cp = |x: f64| ControlPoint {
            position: [x, 0.0, 0.0],
            width: 1.0,
            bank: 0.0,
        };
        let code = |r: Result<Path, GeomError>| r.unwrap_err().code();
        let invalid = crate::codes::INVALID_POLYGON;
        assert_eq!(code(spline(&[cp(0.0)], 1.0, false)), invalid);
        assert_eq!(code(spline(&[cp(0.0), cp(0.0)], 1.0, false)), invalid);
        assert_eq!(code(spline(&[cp(0.0), cp(1.0)], 0.0, false)), invalid);
        assert_eq!(code(spline(&[cp(0.0), cp(1.0e5)], 1.0e-4, false)), invalid);
        assert_eq!(
            code(spline(&[cp(0.0), cp(3.0e6)], 1.0, false)),
            crate::codes::OUT_OF_RANGE
        );
    }
}
