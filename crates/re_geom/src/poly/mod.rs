//! The modelling kernel (ADR-0062): [`PolyMesh`], an indexed polygon mesh
//! with attribute columns, and the operations on it.
//!
//! Faces are polygons with any number of corners, counter-clockwise seen
//! from the front, stored as runs of one corner array; each corner names its
//! point. No adjacency is stored: [`PolyMesh::topology`] computes it when an
//! operation needs it. Attributes are columns of values per point, corner or
//! face ([`Attributes`]); a `bool` face column is a group.
//!
//! Every operation returns a new mesh and follows ADR-0036 as the rest of the
//! crate does: scalar arithmetic in a fixed order, [`crate::math`] for
//! cross products, normalisation and angles, `f64` where precision matters,
//! no fused multiply-add and no hash-map iteration reaching an output.

mod attr;
mod edit;
mod prim;
mod topo;
mod tri;

use glam::Vec3;
use re_types::mesh::Aabb;
use serde::{Deserialize, Serialize};

pub use attr::{
    AttrDomain, AttrValue, Attributes, COLOR, Column, ColumnType, MATERIAL, MAX_NAME, NORMAL, UV,
    check_attribute, check_name,
};
pub use edit::{Axis, from_mesh, merge, mirror, transform, weld};
pub use prim::{MAX_SEGMENTS, arch, cone, icosphere, rounded_box, stairs, torus};
pub use topo::{NO_TWIN, Topology};

use crate::GeomError;

/// The most points or corners a mesh an operation makes may have: 2^24,
/// which bounds a node's output at a few hundred megabytes (`RE4515`).
pub const MAX_ELEMENTS: usize = 1 << 24;

/// The collider a mesh asks for where it becomes entities (ADR-0062 §9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColliderKind {
    /// No collider.
    #[default]
    None,
    /// A triangle-mesh collider, for static bodies.
    Mesh,
    /// A convex hull collider.
    Convex,
}

/// An indexed polygon mesh with attribute columns.
///
/// Face `f` is the corners `face_start[f]..face_start[f + 1]`; corner `c`
/// uses point `corner_point[c]`. [`PolyMesh::validate`] states the rules a
/// mesh keeps; every operation keeps them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolyMesh {
    /// Per point, in metres.
    pub positions: Vec<Vec3>,
    /// One offset per face and one more: the faces' corner runs.
    pub face_start: Vec<u32>,
    /// Per corner, its point.
    pub corner_point: Vec<u32>,
    /// Columns as long as `positions`.
    pub points: Attributes,
    /// Columns as long as `corner_point`.
    pub corners: Attributes,
    /// Columns as long as the faces.
    pub faces: Attributes,
    /// Material paths, indexed by the face column [`MATERIAL`]; an empty
    /// path draws with the default material. An empty table is one empty
    /// path.
    pub materials: Vec<String>,
    /// The collider the mesh asks for.
    pub collider: ColliderKind,
    /// False for a mesh that is only a collider.
    pub visible: bool,
}

impl Default for PolyMesh {
    fn default() -> Self {
        PolyMesh::new()
    }
}

/// The Newell normal of the polygon through `positions[pts[i]]` in `f64`:
/// its direction is the polygon's front and its length twice its area.
pub(crate) fn newell(positions: &[Vec3], pts: &[u32]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    for (i, &a) in pts.iter().enumerate() {
        let b = pts[if i + 1 == pts.len() { 0 } else { i + 1 }];
        let (p, q) = (positions[a as usize], positions[b as usize]);
        let (px, py, pz) = (f64::from(p.x), f64::from(p.y), f64::from(p.z));
        let (qx, qy, qz) = (f64::from(q.x), f64::from(q.y), f64::from(q.z));
        out[0] += (py - qy) * (pz + qz);
        out[1] += (pz - qz) * (px + qx);
        out[2] += (px - qx) * (py + qy);
    }
    out
}

/// The length of a `f64` vector.
pub(crate) fn length64(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

impl PolyMesh {
    /// A mesh with nothing in it.
    pub fn new() -> PolyMesh {
        PolyMesh {
            positions: Vec::new(),
            face_start: vec![0],
            corner_point: Vec::new(),
            points: Attributes::new(),
            corners: Attributes::new(),
            faces: Attributes::new(),
            materials: Vec::new(),
            collider: ColliderKind::None,
            visible: true,
        }
    }

    /// The number of points.
    pub fn point_count(&self) -> usize {
        self.positions.len()
    }

    /// The number of faces.
    pub fn face_count(&self) -> usize {
        self.face_start.len().saturating_sub(1)
    }

    /// The number of corners.
    pub fn corner_count(&self) -> usize {
        self.corner_point.len()
    }

    /// True with no faces.
    pub fn is_empty(&self) -> bool {
        self.face_count() == 0
    }

    /// The corners of face `f`.
    pub fn face(&self, f: usize) -> std::ops::Range<usize> {
        self.face_start[f] as usize..self.face_start[f + 1] as usize
    }

    /// The points of face `f`, in corner order.
    pub fn face_points(&self, f: usize) -> &[u32] {
        &self.corner_point[self.face(f)]
    }

    /// The positions of face `f`'s corners, in order.
    pub fn face_positions(&self, f: usize) -> impl ExactSizeIterator<Item = Vec3> + Clone + '_ {
        self.face_points(f)
            .iter()
            .map(|&p| self.positions[p as usize])
    }

    /// Face `f`'s Newell vector in `f64`: twice its area along its front.
    pub fn face_area_vector(&self, f: usize) -> [f64; 3] {
        newell(&self.positions, self.face_points(f))
    }

    /// Face `f`'s unit normal (+Y for a face with no area).
    pub fn face_normal(&self, f: usize) -> Vec3 {
        let n = self.face_area_vector(f);
        let len = length64(n);
        if len > 0.0 && len.is_finite() {
            Vec3::new(
                (n[0] / len) as f32,
                (n[1] / len) as f32,
                (n[2] / len) as f32,
            )
        } else {
            Vec3::Y
        }
    }

    /// The average of face `f`'s corner positions, in `f64` and rounded once.
    pub fn face_center(&self, f: usize) -> Vec3 {
        let mut s = [0.0f64; 3];
        let ps = self.face_positions(f);
        let n = ps.len() as f64;
        for p in ps {
            s[0] += f64::from(p.x);
            s[1] += f64::from(p.y);
            s[2] += f64::from(p.z);
        }
        Vec3::new((s[0] / n) as f32, (s[1] / n) as f32, (s[2] / n) as f32)
    }

    /// The face of every corner.
    pub fn corner_faces(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.corner_count());
        for f in 0..self.face_count() {
            let r = self.face(f);
            out.extend(std::iter::repeat_n(f as u32, r.len()));
        }
        out
    }

    /// The face group `name` (`Err`, `RE4510`, if there is no `bool` face
    /// column of that name); every face when `name` is empty.
    pub fn group(&self, name: &str) -> Result<Vec<bool>, GeomError> {
        if name.is_empty() {
            return Ok(vec![true; self.face_count()]);
        }
        match self.faces.get(name) {
            Some(Column::Bool(v)) => Ok(v.clone()),
            Some(c) => Err(GeomError::Argument(format!(
                "face attribute `{name}` is a `{}`, not a group (`bool`)",
                c.ty().name()
            ))),
            None => Err(GeomError::Argument(format!(
                "there is no face group `{name}`"
            ))),
        }
    }

    /// Sets the face group `name` to `faces` (one entry per face), replacing
    /// a group of that name. `Err` (`RE4510`) for an invalid name, a standard
    /// attribute's name, or a face column of that name that is not a group.
    pub fn set_group(&mut self, name: &str, faces: Vec<bool>) -> Result<(), GeomError> {
        check_attribute(name, AttrDomain::Face, ColumnType::Bool)?;
        if faces.len() != self.face_count() {
            return Err(GeomError::Argument(format!(
                "group `{name}` has {} entries for {} faces",
                faces.len(),
                self.face_count()
            )));
        }
        if let Some(c) = self.faces.get(name)
            && c.ty() != ColumnType::Bool
        {
            return Err(GeomError::Argument(format!(
                "face attribute `{name}` is a `{}`, not a group (`bool`)",
                c.ty().name()
            )));
        }
        self.faces.insert(name, Column::Bool(faces))?;
        Ok(())
    }

    /// The material index of every face (0 without a [`MATERIAL`] column).
    pub fn face_materials(&self) -> Vec<i32> {
        match self.faces.ints(MATERIAL) {
            Some(m) => m.to_vec(),
            None => vec![0; self.face_count()],
        }
    }

    /// The index of material `path` in [`PolyMesh::materials`], added at the
    /// end if absent. An empty table first gets the empty path, which every
    /// face without a material uses.
    pub fn material_index(&mut self, path: &str) -> i32 {
        if self.materials.is_empty() {
            if path.is_empty() {
                return 0;
            }
            self.materials.push(String::new());
        }
        match self.materials.iter().position(|m| m == path) {
            Some(i) => i as i32,
            None => {
                self.materials.push(path.to_string());
                self.materials.len() as i32 - 1
            }
        }
    }

    /// The number of material parts [`PolyMesh::to_meshes`] gives: one per
    /// material, or one with an empty table.
    pub fn part_count(&self) -> usize {
        self.materials.len().max(1)
    }

    /// The material path of part `i` (empty for the default material).
    pub fn part_material(&self, i: usize) -> &str {
        self.materials.get(i).map_or("", String::as_str)
    }

    /// Checks the rules every mesh keeps (`RE4511` with the first one
    /// broken): `face_start` starts at 0, rises by at least 3 per face and
    /// ends at the corner count; every index is in range; no face uses a
    /// point twice; positions are finite; every face has a non-zero area
    /// vector; each column is as long as its domain, finite, validly named
    /// and sorted, with the standard attributes' types; every `material`
    /// indexes the table (0 with an empty one), whose paths are distinct; and
    /// faces, corners and points each number less than `u32::MAX`.
    pub fn validate(&self) -> Result<(), GeomError> {
        let bad = |m: String| Err(GeomError::InvalidMesh(m));
        let (n_points, n_corners) = (self.positions.len(), self.corner_point.len());
        if self.face_start.is_empty() || self.face_start[0] != 0 {
            return bad("`face_start` must start at 0".into());
        }
        if n_points >= u32::MAX as usize
            || n_corners >= u32::MAX as usize
            || self.face_start.len() >= u32::MAX as usize
        {
            return bad("more elements than a u32 index can name".into());
        }
        for (f, w) in self.face_start.windows(2).enumerate() {
            if w[1] < w[0].saturating_add(3) {
                return bad(format!("face {f} has fewer than 3 corners"));
            }
        }
        if *self.face_start.last().unwrap_or(&0) as usize != n_corners {
            return bad(format!(
                "`face_start` ends at {} but there are {n_corners} corners",
                self.face_start.last().unwrap_or(&0)
            ));
        }
        if let Some(c) = self
            .corner_point
            .iter()
            .position(|&p| p as usize >= n_points)
        {
            return bad(format!(
                "corner {c} names point {} of {n_points}",
                self.corner_point[c]
            ));
        }
        if let Some(p) = self.positions.iter().position(|p| !p.is_finite()) {
            return bad(format!("point {p} is not finite"));
        }
        let mut seen = vec![u32::MAX; n_points];
        for f in 0..self.face_count() {
            for &p in self.face_points(f) {
                if seen[p as usize] == f as u32 {
                    return bad(format!("face {f} uses point {p} twice"));
                }
                seen[p as usize] = f as u32;
            }
            if length64(self.face_area_vector(f)) <= 0.0 {
                return bad(format!("face {f} has no area"));
            }
        }
        self.points
            .check(AttrDomain::Point, n_points)
            .map_err(GeomError::InvalidMesh)?;
        self.corners
            .check(AttrDomain::Corner, n_corners)
            .map_err(GeomError::InvalidMesh)?;
        self.faces
            .check(AttrDomain::Face, self.face_count())
            .map_err(GeomError::InvalidMesh)?;
        for (i, m) in self.materials.iter().enumerate() {
            if self.materials[..i].contains(m) {
                return bad(format!("material `{m}` is listed twice"));
            }
        }
        if let Some(ms) = self.faces.ints(MATERIAL) {
            let limit = self.part_count() as i32;
            if let Some(f) = ms.iter().position(|&m| m < 0 || m >= limit) {
                return bad(format!(
                    "face {f} has material {} but the table has {} entries",
                    ms[f],
                    self.materials.len()
                ));
            }
        }
        Ok(())
    }

    /// `Err` (`RE4515`) if a mesh of `points` points and `corners` corners
    /// would be over [`MAX_ELEMENTS`].
    pub fn check_size(points: usize, corners: usize) -> Result<(), GeomError> {
        if points > MAX_ELEMENTS || corners > MAX_ELEMENTS {
            Err(GeomError::PolyTooLarge {
                points: points as u64,
                corners: corners as u64,
            })
        } else {
            Ok(())
        }
    }

    /// The adjacency of the faces (see [`Topology`]).
    pub fn topology(&self) -> Topology {
        Topology::new(self)
    }

    /// True if every edge has a twin: no boundary and no edge shared by more
    /// than two faces or twice in one direction.
    pub fn is_closed(&self) -> bool {
        self.topology().twin.iter().all(|&t| t != NO_TWIN)
    }

    /// True if no edge is used by more than two faces or twice in one
    /// direction, and each point's corners form one fan (a closed one, or
    /// one open fan on a boundary). Open meshes can be manifold.
    pub fn is_manifold(&self) -> bool {
        let t = self.topology();
        t.non_manifold_edges(self) == 0 && t.fans_are_single(self)
    }

    /// The box around every point, or `None` without points.
    pub fn bounds(&self) -> Option<Aabb> {
        Aabb::from_points(self.positions.iter().copied())
    }

    /// The surface area in square metres: half the length of each face's
    /// Newell vector, summed in `f64` in face order.
    pub fn area(&self) -> f64 {
        (0..self.face_count())
            .map(|f| length64(self.face_area_vector(f)) * 0.5)
            .sum()
    }

    /// The enclosed volume in cubic metres, for a closed mesh (`None` for an
    /// open one): each face fanned from its first corner, signed tetrahedra
    /// against the centre of the bounds, in `f64`. Negative when the faces
    /// point inwards.
    pub fn volume(&self) -> Option<f64> {
        if !self.is_closed() {
            return None;
        }
        let c = self.bounds()?.center();
        let rel = |p: Vec3| {
            [
                f64::from(p.x) - f64::from(c.x),
                f64::from(p.y) - f64::from(c.y),
                f64::from(p.z) - f64::from(c.z),
            ]
        };
        let mut v = 0.0;
        for f in 0..self.face_count() {
            let pts = self.face_points(f);
            let a = rel(self.positions[pts[0] as usize]);
            for w in pts[1..].windows(2) {
                let b = rel(self.positions[w[0] as usize]);
                let d = rel(self.positions[w[1] as usize]);
                v += a[0] * (b[1] * d[2] - b[2] * d[1]) - a[1] * (b[0] * d[2] - b[2] * d[0])
                    + a[2] * (b[0] * d[1] - b[1] * d[0]);
            }
        }
        Some(v / 6.0)
    }

    /// The area-weighted centre of the surface, or `None` with no area: each
    /// face's corner average weighted by its area, in `f64`.
    pub fn centroid(&self) -> Option<Vec3> {
        let (mut s, mut total) = ([0.0f64; 3], 0.0f64);
        for f in 0..self.face_count() {
            let a = length64(self.face_area_vector(f));
            let c = self.face_center(f);
            s[0] += f64::from(c.x) * a;
            s[1] += f64::from(c.y) * a;
            s[2] += f64::from(c.z) * a;
            total += a;
        }
        (total > 0.0).then(|| {
            Vec3::new(
                (s[0] / total) as f32,
                (s[1] / total) as f32,
                (s[2] / total) as f32,
            )
        })
    }

    /// Reverses face `f`'s winding, keeping its first corner first; its
    /// corners' values move with them and their [`NORMAL`]s are negated.
    pub(crate) fn flip_face(&mut self, f: usize) {
        let r = self.face(f);
        let tail = r.start + 1..r.end;
        self.corner_point[tail.clone()].reverse();
        self.corners.reverse(tail);
        if let Some(Column::Vec3(n)) = self.corners.get_mut(NORMAL) {
            for v in &mut n[r] {
                *v = -*v;
            }
        }
    }

    /// Reverses the winding of the faces in group `group` (every face when it
    /// is empty).
    pub fn flip(&self, group: &str) -> Result<PolyMesh, GeomError> {
        let sel = self.group(group)?;
        let mut out = self.clone();
        for (f, s) in sel.iter().enumerate() {
            if *s {
                out.flip_face(f);
            }
        }
        Ok(out)
    }

    /// One `MeshData` per material part (ADR-0062 §5), in table order; a part
    /// no face uses is empty. Faces whose fan from their first corner faces
    /// their way are fanned; others are ear-clipped (`RE4505` when one
    /// cannot be).
    pub fn to_meshes(&self) -> Result<Vec<re_types::mesh::MeshData>, GeomError> {
        tri::to_meshes(self)
    }

    /// Every face in one `MeshData`, the parts merged in table order.
    pub fn to_mesh(&self) -> Result<re_types::mesh::MeshData, GeomError> {
        let parts = self.to_meshes()?;
        crate::merge(parts.iter())
    }
}

/// Builds a mesh face by face, with optional corner UVs and normals and
/// numbered face groups; used by the primitives.
#[derive(Default)]
pub(crate) struct Builder {
    pub positions: Vec<Vec3>,
    pub face_start: Vec<u32>,
    pub corner_point: Vec<u32>,
    pub uv: Vec<[f32; 2]>,
    pub normal: Vec<Vec3>,
    pub face_group: Vec<u8>,
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            face_start: vec![0],
            ..Default::default()
        }
    }

    pub fn point(&mut self, p: Vec3) -> u32 {
        self.positions.push(p);
        self.positions.len() as u32 - 1
    }

    /// A face of `points` in group `group`, with a UV per corner and, when
    /// `normals` is given, a normal per corner.
    pub fn face(&mut self, points: &[u32], uv: &[[f32; 2]], normals: Option<&[Vec3]>, group: u8) {
        self.corner_point.extend_from_slice(points);
        self.uv.extend_from_slice(uv);
        if let Some(n) = normals {
            self.normal.extend_from_slice(n);
        }
        self.face_start.push(self.corner_point.len() as u32);
        self.face_group.push(group);
    }

    /// The mesh, its groups named by `groups` (group `i` is `groups[i]`; an
    /// empty name makes no column). Normals are kept only if every corner
    /// has one.
    pub fn finish(self, groups: &[&str]) -> Result<PolyMesh, GeomError> {
        PolyMesh::check_size(self.positions.len(), self.corner_point.len())?;
        let mut m = PolyMesh::new();
        let n_corners = self.corner_point.len();
        m.positions = self.positions;
        m.face_start = self.face_start;
        m.corner_point = self.corner_point;
        m.corners.insert(UV, Column::Vec2(self.uv))?;
        if self.normal.len() == n_corners && n_corners > 0 {
            m.corners.insert(NORMAL, Column::Vec3(self.normal))?;
        }
        for (i, name) in groups.iter().enumerate() {
            if !name.is_empty() {
                let sel = self.face_group.iter().map(|&g| g as usize == i).collect();
                m.faces.insert(*name, Column::Bool(sel))?;
            }
        }
        Ok(m)
    }
}

/// A texture coordinate in metres for `p` on a face whose normal is `n`,
/// projected along the axis nearest `n` (ties to x, then y, then z), with u
/// to the face's right and v down seen from the front (glTF's convention),
/// so textures stand upright on walls. `size` metres is one repeat.
pub fn box_uv(p: Vec3, n: Vec3, size: f32) -> [f32; 2] {
    let (ax, ay, az) = (n.x.abs(), n.y.abs(), n.z.abs());
    let (u, v) = if ax >= ay && ax >= az {
        // Facing ±X: right is -Z for +X, +Z for -X; up is +Y.
        (if n.x >= 0.0 { -p.z } else { p.z }, -p.y)
    } else if ay >= az {
        // Facing ±Y: right is +X; "up" on the page is -Z for +Y, +Z for -Y.
        (p.x, if n.y >= 0.0 { p.z } else { -p.z })
    } else {
        // Facing ±Z: right is +X for +Z, -X for -Z; up is +Y.
        (if n.z >= 0.0 { p.x } else { -p.x }, -p.y)
    };
    [u / size, v / size]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit cube of six quads.
    pub(crate) fn cube() -> PolyMesh {
        let mut m = PolyMesh::new();
        for i in 0..8u32 {
            m.positions.push(Vec3::new(
                (i & 1) as f32,
                ((i >> 1) & 1) as f32,
                ((i >> 2) & 1) as f32,
            ));
        }
        let faces: [[u32; 4]; 6] = [
            [0, 4, 6, 2], // -X
            [1, 3, 7, 5], // +X
            [0, 1, 5, 4], // -Y
            [2, 6, 7, 3], // +Y
            [0, 2, 3, 1], // -Z
            [4, 5, 7, 6], // +Z
        ];
        for f in faces {
            m.corner_point.extend_from_slice(&f);
            m.face_start.push(m.corner_point.len() as u32);
        }
        m
    }

    #[test]
    fn a_cube_is_valid_closed_and_manifold() {
        let m = cube();
        m.validate().unwrap();
        assert!(m.is_closed());
        assert!(m.is_manifold());
        assert_eq!(m.area(), 6.0);
        assert_eq!(m.volume(), Some(1.0));
        assert_eq!(m.centroid(), Some(Vec3::splat(0.5)));
        assert_eq!(m.face_normal(3), Vec3::Y);
        let b = m.bounds().unwrap();
        assert_eq!((b.min, b.max), (Vec3::ZERO, Vec3::ONE));
    }

    #[test]
    fn validation_names_the_broken_rule() {
        let mut m = cube();
        m.corner_point[0] = 9;
        assert!(
            matches!(m.validate(), Err(GeomError::InvalidMesh(s)) if s.contains("names point"))
        );
        let mut m = cube();
        m.corner_point[1] = 0;
        assert!(matches!(m.validate(), Err(GeomError::InvalidMesh(s)) if s.contains("twice")));
        let mut m = cube();
        m.positions[7] = Vec3::splat(f32::NAN);
        assert!(m.validate().is_err());
        let mut m = cube();
        m.faces
            .insert(MATERIAL, Column::Int(vec![0, 0, 0, 0, 0, 1]))
            .unwrap();
        assert!(m.validate().is_err(), "material 1 with an empty table");
        m.materials = vec![String::new(), "a".into()];
        m.validate().unwrap();
        let mut m = cube();
        m.face_start = vec![0, 2];
        assert!(m.validate().is_err());
        let mut m = cube();
        // Collapse the -X face to a line.
        m.positions[4] = Vec3::new(0.0, 0.0, 0.0);
        m.positions[6] = Vec3::new(0.0, 0.5, 0.0);
        m.positions[2] = Vec3::new(0.0, 1.0, 0.0);
        assert!(matches!(m.validate(), Err(GeomError::InvalidMesh(s)) if s.contains("no area")));
    }

    #[test]
    fn an_open_box_is_manifold_but_not_closed() {
        let mut m = cube();
        m.face_start.pop();
        m.corner_point.truncate(20);
        m.validate().unwrap();
        assert!(!m.is_closed());
        assert!(m.is_manifold());
        assert_eq!(m.volume(), None);
    }

    #[test]
    fn flipping_turns_the_volume_inside_out() {
        let m = cube().flip("").unwrap();
        m.validate().unwrap();
        assert_eq!(m.volume(), Some(-1.0));
        assert_eq!(m.face_points(0), &[0, 2, 6, 4]);
    }

    #[test]
    fn materials_are_added_once() {
        let mut m = cube();
        assert_eq!(m.material_index(""), 0);
        assert!(m.materials.is_empty());
        assert_eq!(m.material_index("stone"), 1);
        assert_eq!(m.material_index("stone"), 1);
        assert_eq!(m.materials, ["", "stone"]);
        assert_eq!(m.part_count(), 2);
    }

    #[test]
    fn box_uvs_stand_upright() {
        // On a +Z wall, u grows to the right (+X) and v down (-Y).
        assert_eq!(box_uv(Vec3::new(2.0, 3.0, 0.0), Vec3::Z, 1.0), [2.0, -3.0]);
        // On a -Z wall, right is -X.
        assert_eq!(
            box_uv(Vec3::new(2.0, 3.0, 0.0), Vec3::NEG_Z, 2.0),
            [-1.0, -1.5]
        );
        assert_eq!(box_uv(Vec3::new(1.0, 0.0, 2.0), Vec3::Y, 1.0), [1.0, 2.0]);
    }
}
