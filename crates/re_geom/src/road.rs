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

use std::collections::HashMap;
use std::ops::Range;

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
        // end is replaced by the end itself (unless it is the start).
        if samples.len() > 1 && total - last < spacing * 1e-3 {
            samples.pop();
        }
        samples.push(PathSample {
            position: end.0,
            width: end.1,
            bank: end.2,
            distance: total,
        });
    }
    let path = Path { samples, closed };
    if path.validate().is_err() {
        return Err(bad(format!(
            "a spacing of {spacing} leaves too few samples on a path {total} long; use a smaller spacing"
        )));
    }
    Ok(path)
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

/// Boxes in X and Z on a uniform grid, to find the boxes that may overlap
/// another without testing them all.
struct Grid {
    size: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
    /// Boxes over more than [`Grid::MAX_CELLS`] cells, in every answer.
    big: Vec<usize>,
}

impl Grid {
    /// A box over more cells than this is kept apart.
    const MAX_CELLS: i64 = 64;

    /// An empty grid of cells `size` metres wide (at least 1 mm).
    fn new(size: f64) -> Grid {
        Grid {
            size: if size.is_finite() {
                size.max(1e-3)
            } else {
                1e-3
            },
            cells: HashMap::new(),
            big: Vec::new(),
        }
    }

    /// The cells `[x0, z0, x1, z1]` (inclusive) under box `b`, and how many.
    fn span(&self, b: [f64; 4]) -> ([i64; 4], i64) {
        let c = |v: f64| (v / self.size).floor() as i64;
        let s = [c(b[0]), c(b[1]), c(b[2]), c(b[3])];
        let side = |lo: i64, hi: i64| hi.saturating_sub(lo).saturating_add(1);
        (s, side(s[0], s[2]).saturating_mul(side(s[1], s[3])))
    }

    /// Adds box `b` as item `id` (ids in increasing order).
    fn insert(&mut self, id: usize, b: [f64; 4]) {
        let ([x0, z0, x1, z1], count) = self.span(b);
        if count > Self::MAX_CELLS {
            self.big.push(id);
            return;
        }
        for z in z0..=z1 {
            for x in x0..=x1 {
                self.cells.entry((x, z)).or_default().push(id);
            }
        }
    }

    /// Every item whose box overlaps box `b` (and some that do not), in
    /// increasing order, into `out`.
    fn query(&self, b: [f64; 4], out: &mut Vec<usize>) {
        out.clear();
        out.extend_from_slice(&self.big);
        let ([x0, z0, x1, z1], count) = self.span(b);
        if count > self.cells.len() as i64 {
            for ((x, z), ids) in &self.cells {
                if (x0..=x1).contains(x) && (z0..=z1).contains(z) {
                    out.extend_from_slice(ids);
                }
            }
        } else {
            for z in z0..=z1 {
                for x in x0..=x1 {
                    if let Some(ids) = self.cells.get(&(x, z)) {
                        out.extend_from_slice(ids);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
    }
}

/// Junction centres in X and Z: crossings between different paths, and
/// open ends within `snap` of another path. Points closer than twice the
/// radius are merged into their mean, so junction discs never overlap.
///
/// Segments are found on a grid, and the points are taken in the order a
/// search of every pair of segments finds them, so the centres are that
/// search's.
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
    // Every segment, numbered path by path, on a grid as wide as an
    // average segment.
    let index: Vec<(usize, usize)> = segs
        .iter()
        .enumerate()
        .flat_map(|(i, s)| (0..s.len()).map(move |k| (i, k)))
        .collect();
    let extent = segs
        .iter()
        .flatten()
        .map(|(_, _, b)| (b[2] - b[0]).max(b[3] - b[1]))
        .sum::<f64>()
        / index.len().max(1) as f64;
    let mut grid = Grid::new(extent);
    for (id, &(i, k)) in index.iter().enumerate() {
        grid.insert(id, segs[i][k].2);
    }
    let mut near = Vec::new();
    // Crossings, in (path, later path, segment of each) order.
    let mut crossings: Vec<([usize; 4], [f64; 2])> = Vec::new();
    for (i, path_segs) in segs.iter().enumerate() {
        for (a, (a0, a1, ba)) in path_segs.iter().enumerate() {
            grid.query(*ba, &mut near);
            for &id in &near {
                let (j, b) = index[id];
                let (b0, b1, bb) = &segs[j][b];
                if j > i
                    && overlaps(*ba, *bb)
                    && let Some(c) = crossing(*a0, *a1, *b0, *b1)
                {
                    crossings.push(([i, j, a, b], c));
                }
            }
        }
    }
    crossings.sort_by_key(|(order, _)| *order);
    points.extend(crossings.into_iter().map(|(_, c)| c));
    let snap2 = style.snap * style.snap;
    let mid = |e: [f64; 2], c: [f64; 2]| [(e[0] + c[0]) * 0.5, (e[1] + c[1]) * 0.5];
    for (i, path) in paths.iter().enumerate() {
        if path.closed || style.snap <= 0.0 {
            continue;
        }
        let ends = [path.samples[0], path.samples[path.samples.len() - 1]];
        for end in ends {
            let e = xz(end.position);
            let reach = bbox(e, e, style.snap);
            grid.query(reach, &mut near);
            // Each other path's nearest point (on the first of equally near
            // segments), path by path: `near` runs in path order.
            let mut best: Option<(usize, f64, [f64; 2])> = None;
            for &id in &near {
                let (j, k) = index[id];
                if j == i {
                    continue;
                }
                if let Some((bj, _, c)) = best
                    && bj != j
                {
                    points.push(mid(e, c));
                    best = None;
                }
                let (a, b, bb) = &segs[j][k];
                if !overlaps(reach, *bb) {
                    continue;
                }
                let (t, d2) = closest_on_segment(e, *a, *b);
                if d2 <= snap2 && best.is_none_or(|(_, bd, _)| d2 < bd) {
                    best = Some((j, d2, [lerp(a[0], b[0], t), lerp(a[1], b[1], t)]));
                }
            }
            if let Some((_, _, c)) = best {
                points.push(mid(e, c));
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
#[derive(Clone, Debug)]
struct Piece {
    stations: Vec<Station>,
    ends: [Option<usize>; 2],
    /// How far its ribbon's edges reach from its centreline in X and Z (half
    /// its widest width, with room for rounding).
    pad: f64,
    /// A box `[min x, min z, max x, max z]` holding its ribbon.
    bbox: [f64; 4],
}

impl Piece {
    fn new(stations: Vec<Station>, ends: [Option<usize>; 2]) -> Piece {
        let half = stations
            .iter()
            .map(|s| s.sample.width * 0.5)
            .fold(0.0, f64::max);
        let pad = half * (1.0 + 1e-9) + 1e-9;
        let mut b = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for s in &stations {
            let [x, _, z] = s.sample.position;
            b = [
                b[0].min(x - pad),
                b[1].min(z - pad),
                b[2].max(x + pad),
                b[3].max(z + pad),
            ];
        }
        Piece {
            stations,
            ends,
            pad,
            bbox: b,
        }
    }
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
/// in metres). It is [`network`]'s [`Network::mesh`].
///
/// A curve tighter than half the road's width folds the ribbon's inner edge
/// back on itself; [`footprint`] still covers the right area, but the mesh
/// has overlapping triangles there, so control points should keep curves
/// wider than that.
pub fn road_mesh(paths: &[Path], style: &RoadStyle) -> Result<Road, GeomError> {
    let network = network(paths, style)?;
    Ok(Road {
        mesh: network.mesh()?,
        junctions: network.junctions(),
    })
}

/// Roads joined into one network: the stretches between junctions and the
/// junction surfaces. [`road_mesh`] gives its whole [`Network::mesh`]; a
/// streamed world joins the roads once and builds each cell's part with
/// [`Network::mesh_in`].
#[derive(Clone, Debug, Default)]
pub struct Network {
    pieces: Vec<Piece>,
    /// Each junction centre's height, which the ribbons ease to.
    heights: Vec<f64>,
    /// The junctions with a surface, in centre order.
    junctions: Vec<Junction>,
    fade: f64,
}

/// A junction with a surface: its centre `[x, y, z]` (`y` the surface's
/// height), the surface's area and that area's box (`None` when empty).
#[derive(Clone, Debug)]
struct Junction {
    centre: [f64; 3],
    area: Shapes,
    bbox: Option<[f64; 4]>,
}

/// Joins `paths` into a [`Network`] (see [`road_mesh`]).
pub fn network(paths: &[Path], style: &RoadStyle) -> Result<Network, GeomError> {
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
    // The centres on a grid. A centre farther than `reach` from a segment's
    // box is out of its disc's reach, with room for rounding.
    let mut grid = Grid::new(2.0 * r);
    for (j, c) in centres.iter().enumerate() {
        grid.insert(j, [c[0], c[1], c[0], c[1]]);
    }
    let reach = r + 1e-3 + r * 1e-3;
    let mut near = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut pieces: Vec<Piece> = Vec::new();
    for path in paths {
        let tangents = tangents(path);
        let n = path.samples.len();
        let m = if path.closed { n } else { n - 1 } as f64;
        // Inside intervals along the path parameter, with their junction.
        let mut inside: Vec<(f64, f64, usize)> = Vec::new();
        let mut hits: Vec<(f64, f64, usize)> = Vec::new();
        for (i, (a, b)) in path.segments().enumerate() {
            let (a, b) = (xz(a.position), xz(b.position));
            // A segment can cross several discs: take them in path order.
            hits.clear();
            grid.query(bbox(a, b, reach), &mut near);
            for &j in &near {
                if let Some((t0, t1)) = disc_interval(a, b, centres[j], r) {
                    hits.push((i as f64 + t0, i as f64 + t1, j));
                }
            }
            hits.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.2.cmp(&y.2)));
            for &(s0, s1, j) in &hits {
                match inside.last_mut() {
                    Some(last) if last.2 == j && last.1 >= s0 => last.1 = last.1.max(s1),
                    _ => inside.push((s0, s1, j)),
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
        // A loop wholly inside one junction is all junction.
        let whole = path.closed && inside.len() == 1 && inside[0].0 <= 0.0 && inside[0].1 >= m;
        if whole {
            // No pieces: the junction's surface covers the loop.
        } else if inside.is_empty() {
            pieces.push(Piece::new(stations(path, &tangents, 0.0, m), [None, None]));
        } else if path.closed {
            for k in 0..inside.len() {
                let (_, e, j0) = inside[k];
                let (s, _, j1) = inside[(k + 1) % inside.len()];
                let e = if e >= m { e - m } else { e };
                pieces.push(Piece::new(
                    stations(path, &tangents, e, s),
                    [Some(j0), Some(j1)],
                ));
            }
        } else {
            let mut from = (0.0, None);
            for &(s0, s1, j) in &inside {
                if s0 > from.0 {
                    pieces.push(Piece::new(
                        stations(path, &tangents, from.0, s0),
                        [from.1, Some(j)],
                    ));
                }
                from = (s1, Some(j));
            }
            if from.0 < m {
                pieces.push(Piece::new(
                    stations(path, &tangents, from.0, m),
                    [from.1, None],
                ));
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
        let bbox = area
            .bbox()
            .map(|b| [b[0] - 1e-9, b[1] - 1e-9, b[2] + 1e-9, b[3] + 1e-9]);
        junctions.push(Junction {
            centre: [c[0], heights[j], c[1]],
            area,
            bbox,
        });
    }
    Ok(Network {
        pieces,
        heights,
        junctions,
        fade: style.bank_fade,
    })
}

impl Network {
    /// About how many bytes it holds, for a cache's budget.
    pub fn bytes(&self) -> u64 {
        let stations: usize = self.pieces.iter().map(|p| p.stations.len()).sum();
        let points: usize = self.junctions.iter().map(|j| j.area.point_count()).sum();
        (size_of::<Network>()
            + self.pieces.len() * size_of::<Piece>()
            + stations * size_of::<Station>()
            + self.heights.len() * size_of::<f64>()
            + self.junctions.len() * size_of::<Junction>()
            + points * size_of::<[f64; 2]>()) as u64
    }

    /// Each junction's centre `[x, y, z]`, `y` being its surface height.
    pub fn junctions(&self) -> Vec<[f64; 3]> {
        self.junctions.iter().map(|j| j.centre).collect()
    }

    /// The whole road surface (see [`road_mesh`]).
    pub fn mesh(&self) -> Result<MeshData, GeomError> {
        let mut meshes: Vec<MeshData> = self
            .pieces
            .iter()
            .map(|p| ribbon(p, 0..p.stations.len(), &self.heights, self.fade).0)
            .collect();
        for j in &self.junctions {
            meshes.push(polygon::triangulate(&j.area, j.centre[1] as f32)?);
        }
        crate::merge(meshes.iter())
    }

    /// The part of [`Network::mesh`] in the square from `min` to `max` (in
    /// X and Z): the triangles whose centroid lies in it, `max` excluded, so
    /// the squares of a grid share out every triangle exactly once. Positions
    /// are relative to `min`, so a part far from the origin keeps its
    /// precision; normals and UVs are the whole mesh's.
    pub fn mesh_in(&self, min: [f64; 2], max: [f64; 2]) -> Result<MeshData, GeomError> {
        let touches =
            |b: [f64; 4]| b[0] <= max[0] && b[2] >= min[0] && b[1] <= max[1] && b[3] >= min[1];
        let mut out = MeshData::default();
        for piece in &self.pieces {
            if !touches(piece.bbox) {
                continue;
            }
            // The quads (station k to k + 1) that can hold a triangle here,
            // with a neighbour on each side so the normals are the whole
            // ribbon's.
            let st = &piece.stations;
            let quads = (0..st.len().saturating_sub(1)).filter(|&k| {
                let (a, b) = (xz(st[k].sample.position), xz(st[k + 1].sample.position));
                touches(bbox(a, b, piece.pad))
            });
            let Some((first, last)) = quads.fold(None, |r: Option<(usize, usize)>, k| {
                Some(r.map_or((k, k), |(f, _)| (f, k)))
            }) else {
                continue;
            };
            let range = first.saturating_sub(1)..(last + 3).min(st.len());
            let (mesh, at) = ribbon(piece, range, &self.heights, self.fade);
            keep_owned(&mut out, &mesh, &at, min, max)?;
        }
        for j in &self.junctions {
            if !j.bbox.is_some_and(touches) {
                continue;
            }
            let (mesh, at) = polygon::triangulated(&j.area, j.centre[1] as f32, [0.0, 0.0])?;
            keep_owned(&mut out, &mesh, &at, min, max)?;
        }
        Ok(out)
    }
}

/// Appends the triangles of `mesh` whose centroid lies in `[min, max)`,
/// from `at` (each vertex's `[x, z]` in metres), with positions relative to
/// `min`.
fn keep_owned(
    out: &mut MeshData,
    mesh: &MeshData,
    at: &[[f64; 2]],
    min: [f64; 2],
    max: [f64; 2],
) -> Result<(), GeomError> {
    let mut remap = vec![u32::MAX; mesh.vertices.len()];
    for t in mesh.indices.as_chunks::<3>().0 {
        let p = t.map(|i| at[i as usize]);
        let c = [
            (p[0][0] + p[1][0] + p[2][0]) / 3.0,
            (p[0][1] + p[1][1] + p[2][1]) / 3.0,
        ];
        if !(c[0] >= min[0] && c[0] < max[0] && c[1] >= min[1] && c[1] < max[1]) {
            continue;
        }
        for &i in t {
            let i = i as usize;
            if remap[i] == u32::MAX {
                if out.vertices.len() >= u32::MAX as usize {
                    return Err(GeomError::TooLarge(out.vertices.len() as u64 + 1));
                }
                remap[i] = out.vertices.len() as u32;
                let mut v = mesh.vertices[i];
                v.pos[0] = (at[i][0] - min[0]) as f32;
                v.pos[2] = (at[i][1] - min[1]) as f32;
                out.vertices.push(v);
            }
            out.indices.push(remap[i]);
        }
    }
    Ok(())
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

/// A piece's ribbon over its stations `range`: two vertices per station,
/// and each vertex's `[x, z]` in metres. Within `fade` metres of a junction
/// end, the bank fades to 0 and the height eases to the junction's. A
/// station whose neighbours are in `range` has the whole ribbon's normals.
fn ribbon(
    piece: &Piece,
    range: Range<usize>,
    heights: &[f64],
    fade: f64,
) -> (MeshData, Vec<[f64; 2]>) {
    let st = &piece.stations;
    let (d0, d1) = (st[0].sample.distance, st[st.len() - 1].sample.distance);
    let count = range.len();
    let mut vertices = Vec::with_capacity(count * 2);
    let mut xzs = Vec::with_capacity(count * 2);
    for s in &st[range] {
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
            xzs.push([p[0], p[2]]);
        }
    }
    let mut indices = Vec::with_capacity(count.saturating_sub(1) * 6);
    for i in 0..count.saturating_sub(1) as u32 {
        let (l0, r0, l1, r1) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
        indices.extend([l0, r0, l1, r0, r1, l1]);
    }
    let mut mesh = MeshData { vertices, indices };
    crate::compute_normals(&mut mesh);
    (mesh, xzs)
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
    fn a_segment_through_two_junctions_meets_them_in_order() {
        // One 100 m segment, crossed at x = 70 and x = 30: the junctions are
        // listed out of path order, and the result is the same either way.
        let cp = |x: f64| ControlPoint {
            position: [x, 0.0, 0.0],
            width: 6.0,
            bank: 0.0,
        };
        let a = spline(&[cp(0.0), cp(100.0)], 100.0, false).unwrap();
        assert_eq!(a.samples.len(), 2);
        let b = line(&[[70.0, 0.0, -20.0], [70.0, 0.0, 20.0]], 6.0);
        let c = line(&[[30.0, 0.0, -20.0], [30.0, 0.0, 20.0]], 6.0);
        let style = RoadStyle {
            junction_radius: 5.0,
            snap: 0.0,
            bank_fade: 4.0,
        };
        let print = footprint(&[a.clone(), b.clone(), c.clone()], 0.0)
            .unwrap()
            .area();
        for paths in [[&a, &b, &c], [&a, &c, &b]] {
            let paths: Vec<Path> = paths.into_iter().cloned().collect();
            let road = road_mesh(&paths, &style).unwrap();
            assert_eq!(road.junctions.len(), 2);
            let area = upward_area(&road.mesh);
            assert!((area - print).abs() < 0.01 * print, "{area} vs {print}");
        }
    }

    #[test]
    fn a_loop_inside_a_junction_is_all_junction() {
        let ring: Vec<ControlPoint> = (0..8)
            .map(|i| {
                let (s, c) = crate::math::sin_cos(f64::from(i) * std::f64::consts::FRAC_PI_4);
                ControlPoint {
                    position: [3.0 * c, 0.0, 3.0 * s],
                    width: 1.0,
                    bank: 0.0,
                }
            })
            .collect();
        let ring = spline(&ring, 0.5, true).unwrap();
        let cross = line(&[[-20.0, 0.0, 0.0], [20.0, 0.0, 0.0]], 4.0);
        let style = RoadStyle {
            junction_radius: 8.0,
            snap: 0.0,
            bank_fade: 4.0,
        };
        let road = road_mesh(&[ring.clone(), cross.clone()], &style).unwrap();
        let area = upward_area(&road.mesh);
        let print = footprint(&[ring, cross], 0.0).unwrap().area();
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

    /// The junction search before the grid: every pair of segments.
    fn every_pair_centres(paths: &[Path], style: &RoadStyle) -> Vec<[f64; 2]> {
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

    /// Test numbers in `[0, 1)`, the same on every run.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// `count` random roads within a square `size` metres wide around the
    /// origin, every fifth one a loop.
    fn random_roads(rng: &mut Lcg, count: usize, size: f64, spacing: f64) -> Vec<Path> {
        (0..count)
            .map(|k| {
                let closed = k % 5 == 4;
                let n = if closed {
                    4
                } else {
                    2 + (rng.next() * 4.0) as usize
                };
                let cps: Vec<ControlPoint> = (0..n)
                    .map(|_| ControlPoint {
                        position: [
                            (rng.next() - 0.5) * size,
                            rng.next() * 4.0,
                            (rng.next() - 0.5) * size,
                        ],
                        width: 2.0 + rng.next() * 6.0,
                        bank: rng.next() * 10.0 - 5.0,
                    })
                    .collect();
                spline(&cps, spacing, closed).unwrap()
            })
            .collect()
    }

    fn bits(points: &[[f64; 2]]) -> Vec<[u64; 2]> {
        points
            .iter()
            .map(|p| [p[0].to_bits(), p[1].to_bits()])
            .collect()
    }

    #[test]
    fn the_junction_grid_finds_what_every_pair_finds() {
        let mut rng = Lcg(42);
        let mut found = 0;
        for round in 0..24 {
            let spacing = [0.5, 1.0, 3.0, 40.0][round % 4];
            let roads = random_roads(&mut rng, 4 + round % 6, 300.0, spacing);
            let style = RoadStyle {
                junction_radius: 1.0 + rng.next() * 10.0,
                snap: rng.next() * 30.0,
                bank_fade: 4.0,
            };
            let grid = junction_centres(&roads, &style);
            assert_eq!(bits(&grid), bits(&every_pair_centres(&roads, &style)));
            found += grid.len();
        }
        assert!(found > 50, "{found}");
    }

    #[test]
    fn the_disc_grid_misses_no_disc() {
        // Every centre whose disc a segment enters is among the grid's
        // answers, so junction intervals are those of a search over every
        // centre.
        let mut rng = Lcg(7);
        let mut entered = 0;
        for round in 0..40 {
            let r = [0.01, 0.5, 4.0, 25.0][round % 4] * (0.5 + rng.next());
            let centres: Vec<[f64; 2]> = (0..300)
                .map(|_| [(rng.next() - 0.5) * 400.0, (rng.next() - 0.5) * 400.0])
                .collect();
            let mut grid = Grid::new(2.0 * r);
            for (j, c) in centres.iter().enumerate() {
                grid.insert(j, [c[0], c[1], c[0], c[1]]);
            }
            let reach = r + 1e-3 + r * 1e-3;
            let mut near = Vec::new();
            for k in 0..200 {
                let a = [(rng.next() - 0.5) * 400.0, (rng.next() - 0.5) * 400.0];
                let len = [0.0, 1.0, 50.0, 2000.0][k % 4] * rng.next();
                let (s, c) = crate::math::sin_cos(rng.next() * std::f64::consts::TAU);
                let b = [a[0] + len * c, a[1] + len * s];
                grid.query(bbox(a, b, reach), &mut near);
                for (j, centre) in centres.iter().enumerate() {
                    if disc_interval(a, b, *centre, r).is_some() {
                        assert!(near.contains(&j), "{a:?} {b:?} {centre:?} {r}");
                        entered += 1;
                    }
                }
            }
        }
        assert!(entered > 100, "{entered}");
    }

    #[test]
    fn cells_share_out_the_road_mesh() {
        let mut rng = Lcg(3);
        let roads = random_roads(&mut rng, 7, 200.0, 1.0);
        let style = RoadStyle {
            junction_radius: 7.0,
            snap: 3.0,
            bank_fade: 5.0,
        };
        let net = network(&roads, &style).unwrap();
        assert!(net.junctions().len() > 2, "{:?}", net.junctions());
        let whole = net.mesh().unwrap();
        // The whole mesh's triangles by their exact normals, UVs and heights,
        // with their positions in X and Z.
        let key = |v: [&Vertex; 3]| -> Vec<u32> {
            v.iter()
                .flat_map(|v| {
                    [
                        v.normal[0].to_bits(),
                        v.normal[1].to_bits(),
                        v.normal[2].to_bits(),
                        v.uv[0].to_bits(),
                        v.uv[1].to_bits(),
                        v.pos[1].to_bits(),
                    ]
                })
                .collect()
        };
        let mut left: HashMap<Vec<u32>, Vec<[[f64; 2]; 3]>> = HashMap::new();
        for t in whole.indices.as_chunks::<3>().0 {
            let v = t.map(|i| &whole.vertices[i as usize]);
            left.entry(key(v))
                .or_default()
                .push(v.map(|v| [f64::from(v.pos[0]), f64::from(v.pos[2])]));
        }
        let cell = 16.0;
        let mut shared = 0;
        for cz in -10..10 {
            for cx in -10..10 {
                let min = [f64::from(cx) * cell, f64::from(cz) * cell];
                let max = [min[0] + cell, min[1] + cell];
                let part = net.mesh_in(min, max).unwrap();
                assert!(part.is_well_formed());
                for t in part.indices.as_chunks::<3>().0 {
                    let v = t.map(|i| &part.vertices[i as usize]);
                    let world =
                        v.map(|v| [f64::from(v.pos[0]) + min[0], f64::from(v.pos[2]) + min[1]]);
                    let c = [
                        (world[0][0] + world[1][0] + world[2][0]) / 3.0,
                        (world[0][1] + world[1][1] + world[2][1]) / 3.0,
                    ];
                    let eps = 1e-3;
                    assert!(
                        c[0] > min[0] - eps
                            && c[0] < max[0] + eps
                            && c[1] > min[1] - eps
                            && c[1] < max[1] + eps,
                        "{c:?} outside {min:?}"
                    );
                    let same = left.get_mut(&key(v)).expect("a triangle of the whole mesh");
                    let k = same
                        .iter()
                        .position(|w| {
                            (0..3).all(|i| {
                                (w[i][0] - world[i][0]).abs() < eps
                                    && (w[i][1] - world[i][1]).abs() < eps
                            })
                        })
                        .expect("the same triangle");
                    same.swap_remove(k);
                    shared += 1;
                }
            }
        }
        assert_eq!(shared, whole.indices.len() / 3);
        assert!(left.values().all(Vec::is_empty));
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
        // A spacing longer than a loop leaves too few samples.
        let tri = [
            cp(0.0),
            cp(10.0),
            ControlPoint {
                position: [0.0, 0.0, 10.0],
                ..cp(0.0)
            },
        ];
        assert_eq!(code(spline(&tri, 1000.0, true)), invalid);
        // An open path keeps both ends whatever the spacing.
        assert_eq!(
            spline(&[cp(0.0), cp(5.0)], 1.0e4, false)
                .unwrap()
                .samples
                .len(),
            2
        );
        assert_eq!(code(spline(&[cp(0.0), cp(1.0e5)], 1.0e-4, false)), invalid);
        assert_eq!(
            code(spline(&[cp(0.0), cp(3.0e6)], 1.0, false)),
            crate::codes::OUT_OF_RANGE
        );
    }
}
