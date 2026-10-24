//! Polygons in the XZ plane: booleans, offsets, strokes and triangulation
//! (ADR-0043 §5).
//!
//! [`Shapes`] are polygons with holes, in metres, with points `[x, z]` in a
//! source's local space. Booleans, offsets and strokes run on i_overlay's
//! integer engine at a fixed 1 mm grid ([`GRID`]) with integer construction,
//! so their results are exact integers mapped back to floats and are the
//! same on every platform. Triangulation uses earcut. Neither crate appears
//! in this module's public types.
//!
//! Every result is *simplified*: shapes do not overlap, outer contours run
//! counter-clockwise in `(x, z)` and holes clockwise, so even-odd and
//! non-zero filling agree on them ([`Shapes::contains`]).

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::scale::{FixedScaleFloatOverlay, FixedScaleOverlayError};
use i_overlay::mesh::float::outline::offset::OutlineOffset;
use i_overlay::mesh::float::stroke::offset::StrokeOffset;
use i_overlay::mesh::float::style::{LineCap, LineJoin, OutlineStyle, StrokeStyle};
use re_types::mesh::{MeshData, Vertex};
use serde::{Deserialize, Serialize};

use crate::GeomError;

/// Grid points per metre: every boolean, offset and stroke snaps to 1 mm.
pub const GRID: f64 = 1000.0;

/// The largest coordinate accepted, in metres (about 1,000 km). Larger ones
/// do not fit i_overlay's 32-bit integer grid at 1 mm (`RE4503`).
pub const MAX_COORD: f64 = 1.0e6;

/// Join angle step of round joins and caps, in radians (segment length over
/// radius): about 6 segments per quarter turn.
const ROUND_STEP: f64 = 0.25;

/// A closed ring of points `[x, z]`, without the first point repeated.
pub type Contour = Vec<[f64; 2]>;

/// One polygon: an outer contour and the holes inside it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    /// The outer boundary.
    pub outer: Contour,
    /// Holes, each inside `outer`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<Contour>,
}

/// A set of polygons with holes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shapes {
    /// The polygons, in a deterministic order.
    pub shapes: Vec<Shape>,
}

/// A boolean operation on two [`Shapes`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoolOp {
    /// Area in either.
    #[default]
    Union,
    /// Area in both.
    Intersect,
    /// Area in the first and not the second.
    Difference,
    /// Area in exactly one.
    Xor,
}

/// How offsets and strokes turn corners.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Join {
    /// An arc.
    #[default]
    Round,
    /// A sharp corner (a bevel for very sharp turns).
    Miter,
    /// The corner cut off.
    Bevel,
}

/// How a stroke ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    /// Square, at the end point.
    #[default]
    Butt,
    /// A half disc.
    Round,
    /// Square, half the width past the end point.
    Square,
}

/// How [`stroke`] widens a path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    /// The full width, in metres.
    pub width: f64,
    /// Corners.
    pub join: Join,
    /// Both ends.
    pub cap: Cap,
}

type Raw = Vec<Vec<Vec<[f64; 2]>>>;

/// Twice the signed area of a ring in `(x, z)`: positive counter-clockwise.
fn signed_area2(c: &[[f64; 2]]) -> f64 {
    let mut a = 0.0;
    for i in 0..c.len() {
        let p = c[i];
        let q = c[(i + 1) % c.len()];
        a += p[0] * q[1] - q[0] * p[1];
    }
    a
}

fn range_error(e: FixedScaleOverlayError) -> GeomError {
    match e {
        FixedScaleOverlayError::ScaleTooLarge
        | FixedScaleOverlayError::InvalidRect(_)
        | FixedScaleOverlayError::ScaleTooSmall => GeomError::OutOfRange(format!(
            "coordinates do not fit a 1 mm grid (they must stay within ±{MAX_COORD} m)"
        )),
        other => GeomError::Polygon(format!("{other:?}")),
    }
}

fn check_point(p: [f64; 2], what: &str) -> Result<(), GeomError> {
    if !p[0].is_finite() || !p[1].is_finite() {
        return Err(GeomError::Polygon(format!(
            "{what} has a non-finite point {p:?}"
        )));
    }
    if p[0].abs() > MAX_COORD || p[1].abs() > MAX_COORD {
        return Err(GeomError::OutOfRange(format!(
            "{what} has the point {p:?}, beyond ±{MAX_COORD} m"
        )));
    }
    Ok(())
}

fn check_contour(c: &[[f64; 2]], what: &str) -> Result<(), GeomError> {
    if c.len() < 3 {
        return Err(GeomError::Polygon(format!(
            "{what} has {} points; a contour needs at least 3",
            c.len()
        )));
    }
    c.iter().try_for_each(|p| check_point(*p, what))
}

impl Shapes {
    /// No shapes.
    pub fn new() -> Shapes {
        Shapes::default()
    }

    /// One shape without holes.
    pub fn polygon(outer: Contour) -> Shapes {
        Shapes {
            shapes: vec![Shape {
                outer,
                holes: Vec::new(),
            }],
        }
    }

    /// True if there are no shapes.
    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// The number of points in every contour.
    pub fn point_count(&self) -> usize {
        self.contours().map(|c| c.len()).sum()
    }

    /// Every contour (outer, then its holes, shape by shape).
    pub fn contours(&self) -> impl Iterator<Item = &Contour> {
        self.shapes
            .iter()
            .flat_map(|s| std::iter::once(&s.outer).chain(&s.holes))
    }

    /// Checks every contour has 3 points or more, finite and within
    /// [`MAX_COORD`] (`RE4504`, `RE4503`).
    pub fn validate(&self) -> Result<(), GeomError> {
        for (i, s) in self.shapes.iter().enumerate() {
            check_contour(&s.outer, &format!("shape {i}"))?;
            for (j, h) in s.holes.iter().enumerate() {
                check_contour(h, &format!("shape {i} hole {j}"))?;
            }
        }
        Ok(())
    }

    /// The area covered, in square metres (holes subtracted). Exact only for
    /// simplified shapes.
    pub fn area(&self) -> f64 {
        let mut a = 0.0;
        for s in &self.shapes {
            a += signed_area2(&s.outer).abs();
            for h in &s.holes {
                a -= signed_area2(h).abs();
            }
        }
        a * 0.5
    }

    /// True if `p` is inside, by the even-odd rule over every contour (which
    /// for simplified shapes is "inside an outer contour and not in a hole").
    pub fn contains(&self, p: [f64; 2]) -> bool {
        let mut inside = false;
        for c in self.contours() {
            let mut j = c.len() - 1;
            for i in 0..c.len() {
                let (a, b) = (c[i], c[j]);
                if (a[1] > p[1]) != (b[1] > p[1]) {
                    let x = a[0] + (p[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1]);
                    if p[0] < x {
                        inside = !inside;
                    }
                }
                j = i;
            }
        }
        inside
    }

    /// The input in i_overlay's layout, outer contours counter-clockwise and
    /// holes clockwise, so the non-zero rule reads them as written.
    fn raw(&self) -> Result<Raw, GeomError> {
        self.validate()?;
        Ok(self
            .shapes
            .iter()
            .map(|s| {
                let mut out = Vec::with_capacity(1 + s.holes.len());
                let mut outer = s.outer.clone();
                if signed_area2(&outer) < 0.0 {
                    outer.reverse();
                }
                out.push(outer);
                for h in &s.holes {
                    let mut h = h.clone();
                    if signed_area2(&h) > 0.0 {
                        h.reverse();
                    }
                    out.push(h);
                }
                out
            })
            .collect())
    }

    fn from_raw(raw: Raw) -> Shapes {
        Shapes {
            shapes: raw
                .into_iter()
                .filter_map(|mut s| {
                    if s.is_empty() {
                        return None;
                    }
                    let outer = s.remove(0);
                    Some(Shape { outer, holes: s })
                })
                .collect(),
        }
    }

    /// The same area, simplified: overlaps merged, self-intersections
    /// resolved (non-zero rule), and points snapped to the 1 mm grid.
    pub fn simplified(&self) -> Result<Shapes, GeomError> {
        let raw = self.raw()?;
        if raw.is_empty() {
            return Ok(Shapes::new());
        }
        let empty: Raw = Vec::new();
        raw.overlay_with_fixed_scale(&empty, OverlayRule::Subject, FillRule::NonZero, GRID)
            .map(Shapes::from_raw)
            .map_err(range_error)
    }

    /// `self op other`, simplified.
    pub fn boolean(&self, other: &Shapes, op: BoolOp) -> Result<Shapes, GeomError> {
        let (a, b) = (self.raw()?, other.raw()?);
        match (a.is_empty(), b.is_empty()) {
            (true, true) => return Ok(Shapes::new()),
            (false, true) => {
                return match op {
                    BoolOp::Intersect => Ok(Shapes::new()),
                    _ => self.simplified(),
                };
            }
            (true, false) => {
                return match op {
                    BoolOp::Union | BoolOp::Xor => other.simplified(),
                    _ => Ok(Shapes::new()),
                };
            }
            (false, false) => {}
        }
        let rule = match op {
            BoolOp::Union => OverlayRule::Union,
            BoolOp::Intersect => OverlayRule::Intersect,
            BoolOp::Difference => OverlayRule::Difference,
            BoolOp::Xor => OverlayRule::Xor,
        };
        a.overlay_with_fixed_scale(&b, rule, FillRule::NonZero, GRID)
            .map(Shapes::from_raw)
            .map_err(range_error)
    }

    /// Grows (positive `distance`) or shrinks (negative) every shape by
    /// `distance` metres. Holes shrink as the shape grows.
    pub fn offset(&self, distance: f64, join: Join) -> Result<Shapes, GeomError> {
        if !distance.is_finite() || distance.abs() > MAX_COORD {
            return Err(GeomError::Polygon(format!(
                "offset distance {distance} is not usable"
            )));
        }
        let raw = self.raw()?;
        if raw.is_empty() {
            return Ok(Shapes::new());
        }
        if distance == 0.0 {
            return self.simplified();
        }
        let style = OutlineStyle::new(distance).line_join(line_join(join));
        raw.outline_fixed_scale(&style, GRID)
            .map(Shapes::from_raw)
            .map_err(range_error)
    }
}

fn line_join(join: Join) -> LineJoin<f64> {
    match join {
        Join::Round => LineJoin::Round(ROUND_STEP),
        // Turns sharper than about 30° from straight back are bevelled.
        Join::Miter => LineJoin::Miter(std::f64::consts::PI / 6.0),
        Join::Bevel => LineJoin::Bevel,
    }
}

/// The union of every shape of every set, simplified.
pub fn union_all<'a>(sets: impl IntoIterator<Item = &'a Shapes>) -> Result<Shapes, GeomError> {
    let mut all = Shapes::new();
    for s in sets {
        all.shapes.extend(s.shapes.iter().cloned());
    }
    all.simplified()
}

/// The area within `style.width / 2` of an open polyline (`closed` joins the
/// last point to the first), simplified.
pub fn stroke(path: &[[f64; 2]], style: Stroke, closed: bool) -> Result<Shapes, GeomError> {
    if path.len() < 2 {
        return Err(GeomError::Polygon(format!(
            "a path to stroke needs 2 points or more, got {}",
            path.len()
        )));
    }
    path.iter().try_for_each(|p| check_point(*p, "the path"))?;
    if !style.width.is_finite() || style.width <= 0.0 || style.width > MAX_COORD {
        return Err(GeomError::Polygon(format!(
            "stroke width {} must be positive",
            style.width
        )));
    }
    let cap = || match style.cap {
        Cap::Butt => LineCap::Butt,
        Cap::Round => LineCap::Round(ROUND_STEP),
        Cap::Square => LineCap::Square,
    };
    let s = StrokeStyle::new(style.width)
        .line_join(line_join(style.join))
        .start_cap(cap())
        .end_cap(cap());
    let path: Vec<[f64; 2]> = path.to_vec();
    path.stroke_fixed_scale(s, closed, GRID)
        .map(Shapes::from_raw)
        .map_err(range_error)
}

/// A flat mesh of `shapes` at height `y`, facing +Y, with UVs in metres
/// (`u = x`, `v = z`).
pub fn triangulate(shapes: &Shapes, y: f32) -> Result<MeshData, GeomError> {
    shapes.validate()?;
    let mut mesh = MeshData::default();
    let mut ear = earcut::Earcut::<f64>::new();
    let mut tris: Vec<u32> = Vec::new();
    for (i, s) in shapes.shapes.iter().enumerate() {
        let mut points: Vec<[f64; 2]> = s.outer.clone();
        let mut holes: Vec<u32> = Vec::with_capacity(s.holes.len());
        for h in &s.holes {
            holes.push(points.len() as u32);
            points.extend_from_slice(h);
        }
        let base = mesh.vertices.len() as u64;
        if base + points.len() as u64 > u64::from(u32::MAX) {
            return Err(GeomError::TooLarge(base + points.len() as u64));
        }
        ear.earcut(points.iter().copied(), &holes, &mut tris);
        let area = signed_area2(&s.outer).abs()
            - s.holes.iter().map(|h| signed_area2(h).abs()).sum::<f64>();
        if tris.is_empty() && area > 1e-6 {
            return Err(GeomError::Triangulation(format!(
                "shape {i} ({} points, {:.3} m²) gave no triangles",
                points.len(),
                area * 0.5
            )));
        }
        for p in &points {
            mesh.vertices.push(Vertex {
                pos: [p[0] as f32, y, p[1] as f32],
                normal: [0.0, 1.0, 0.0],
                uv: [p[0] as f32, p[1] as f32],
                color: [1.0; 4],
            });
        }
        let base = base as u32;
        for t in tris.as_chunks::<3>().0 {
            let [a, b, c] = [
                points[t[0] as usize],
                points[t[1] as usize],
                points[t[2] as usize],
            ];
            // Facing +Y means clockwise in (x, z).
            let ccw = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            if ccw > 0.0 {
                mesh.indices.extend([base + t[0], base + t[2], base + t[1]]);
            } else {
                mesh.indices.extend([base + t[0], base + t[1], base + t[2]]);
            }
        }
    }
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, z: f64, w: f64) -> Contour {
        vec![[x, z], [x + w, z], [x + w, z + w], [x, z + w]]
    }

    #[test]
    fn difference_makes_a_hole_and_triangulates_to_its_area() {
        let a = Shapes::polygon(square(0.0, 0.0, 10.0));
        let b = Shapes::polygon(square(2.0, 2.0, 6.0));
        let d = a.boolean(&b, BoolOp::Difference).unwrap();
        assert_eq!(d.shapes.len(), 1);
        assert_eq!(d.shapes[0].holes.len(), 1);
        assert!((d.area() - 64.0).abs() < 1e-9);
        assert!(d.contains([1.0, 1.0]) && !d.contains([5.0, 5.0]));
        let mesh = triangulate(&d, 2.0).unwrap();
        assert!(mesh.is_well_formed());
        let mut area = 0.0;
        for t in mesh.indices.as_chunks::<3>().0 {
            let p = t.map(|i| mesh.vertices[i as usize].pos);
            // Facing +Y.
            let up = (p[1][2] - p[0][2]) * (p[2][0] - p[0][0])
                - (p[1][0] - p[0][0]) * (p[2][2] - p[0][2]);
            assert!(up > 0.0);
            area += f64::from(up) * 0.5;
        }
        assert!((area - 64.0).abs() < 1e-3);
    }

    #[test]
    fn every_op_and_empty_inputs() {
        let a = Shapes::polygon(square(0.0, 0.0, 2.0));
        let b = Shapes::polygon(square(1.0, 0.0, 2.0));
        let area = |op| a.boolean(&b, op).unwrap().area();
        assert!((area(BoolOp::Union) - 6.0).abs() < 1e-9);
        assert!((area(BoolOp::Intersect) - 2.0).abs() < 1e-9);
        assert!((area(BoolOp::Difference) - 2.0).abs() < 1e-9);
        assert!((area(BoolOp::Xor) - 4.0).abs() < 1e-9);
        let e = Shapes::new();
        assert!((a.boolean(&e, BoolOp::Union).unwrap().area() - 4.0).abs() < 1e-9);
        assert!(a.boolean(&e, BoolOp::Intersect).unwrap().is_empty());
        assert!(e.boolean(&a, BoolOp::Difference).unwrap().is_empty());
        assert!((e.boolean(&a, BoolOp::Xor).unwrap().area() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn clockwise_input_and_holes_mean_the_same() {
        let mut cw = square(0.0, 0.0, 4.0);
        cw.reverse();
        let s = Shapes {
            shapes: vec![Shape {
                outer: cw,
                holes: vec![square(1.0, 1.0, 1.0)],
            }],
        };
        assert!((s.simplified().unwrap().area() - 15.0).abs() < 1e-9);
    }

    #[test]
    fn offsets_grow_and_shrink() {
        let s = Shapes::polygon(square(0.0, 0.0, 10.0));
        let grown = s.offset(1.0, Join::Miter).unwrap();
        assert!((grown.area() - 144.0).abs() < 1e-3, "{}", grown.area());
        let shrunk = s.offset(-1.0, Join::Round).unwrap();
        assert!((shrunk.area() - 64.0).abs() < 1e-3, "{}", shrunk.area());
        let round = s.offset(1.0, Join::Round).unwrap();
        let disc = 140.0 + std::f64::consts::PI;
        assert!((round.area() - disc).abs() < 0.05, "{}", round.area());
    }

    #[test]
    fn stroke_is_width_times_length() {
        let s = stroke(
            &[[0.0, 0.0], [10.0, 0.0]],
            Stroke {
                width: 2.0,
                join: Join::Round,
                cap: Cap::Butt,
            },
            false,
        )
        .unwrap();
        assert!((s.area() - 20.0).abs() < 1e-6);
    }

    #[test]
    fn bad_input_is_refused_with_its_code() {
        let far = Shapes::polygon(square(0.0, 0.0, 2.0e6));
        assert_eq!(
            far.simplified().unwrap_err().code(),
            crate::codes::OUT_OF_RANGE
        );
        let thin = Shapes::polygon(vec![[0.0, 0.0], [1.0, 0.0]]);
        assert_eq!(
            thin.simplified().unwrap_err().code(),
            crate::codes::INVALID_POLYGON
        );
        let nan = Shapes::polygon(vec![[0.0, 0.0], [1.0, f64::NAN], [0.0, 1.0]]);
        assert_eq!(
            nan.area_checked().unwrap_err().code(),
            crate::codes::INVALID_POLYGON
        );
    }

    impl Shapes {
        fn area_checked(&self) -> Result<f64, GeomError> {
            self.validate().map(|_| self.area())
        }
    }

    #[test]
    fn a_16_km_world_fits() {
        let big = Shapes::polygon(square(-8000.0, -8000.0, 16000.0));
        let hole = Shapes::polygon(square(0.0, 0.0, 1.0));
        let d = big.boolean(&hole, BoolOp::Difference).unwrap();
        assert!((d.area() - (16000.0f64 * 16000.0 - 1.0)).abs() < 1.0);
    }
}
