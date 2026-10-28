//! Whole-mesh edits: transform, mirror, weld, merge, attributes and
//! materials, and conversion from triangle meshes.

use std::collections::HashMap;

use glam::{Quat, Vec3};
use re_types::mesh::MeshData;
use serde::{Deserialize, Serialize};

use super::attr::union_layout;
use super::{
    AttrDomain, AttrValue, Attributes, COLOR, Column, MATERIAL, NORMAL, PolyMesh, UV,
    check_attribute, length64, newell,
};
use crate::GeomError;
use crate::math::{Rotation, normalize_or};

/// A coordinate axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// The X axis.
    #[default]
    X,
    /// The Y axis.
    Y,
    /// The Z axis.
    Z,
}

impl Axis {
    /// The axis as an index into `[x, y, z]`.
    pub fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }
}

fn finite_vec(name: &str, v: Vec3) -> Result<(), GeomError> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(GeomError::Argument(format!(
            "{name} must be finite, got {v}"
        )))
    }
}

impl PolyMesh {
    /// `Err` (`RE4510`) unless `faces` has one entry per face.
    pub(crate) fn check_selection(&self, faces: &[bool]) -> Result<(), GeomError> {
        if faces.len() == self.face_count() {
            Ok(())
        } else {
            Err(GeomError::Argument(format!(
                "a selection of {} faces for a mesh of {}",
                faces.len(),
                self.face_count()
            )))
        }
    }

    /// Reverses face `f`'s winding, keeping its first corner first; its
    /// corners' values move with them.
    pub(crate) fn rewind_face(&mut self, f: usize) {
        let r = self.face(f);
        let tail = r.start + 1..r.end;
        self.corner_point[tail.clone()].reverse();
        self.corners.reverse(tail);
    }

    /// Sets `value` on the attribute `name` of `domain` for the faces in
    /// `faces` (their corners, or the points they use), making the column
    /// with its default first if absent. `Err` (`RE4510`) for an invalid
    /// name, a value of another type than the column's or the standard
    /// attribute's, a non-finite value, or [`MATERIAL`], which indexes the
    /// material table and is set by [`PolyMesh::set_material`].
    pub fn fill_attribute(
        &mut self,
        domain: AttrDomain,
        name: &str,
        value: AttrValue,
        faces: &[bool],
    ) -> Result<(), GeomError> {
        self.check_selection(faces)?;
        if !value.is_finite() {
            return Err(GeomError::Argument(format!(
                "attribute `{name}` must be finite, got {value:?}"
            )));
        }
        if name == MATERIAL {
            return Err(GeomError::Argument(
                "`material` indexes the material table; set materials with `set_material`".into(),
            ));
        }
        check_attribute(name, domain, value.ty())?;
        let len = match domain {
            AttrDomain::Point => self.point_count(),
            AttrDomain::Corner => self.corner_count(),
            AttrDomain::Face => self.face_count(),
        };
        let mut targets = vec![false; len];
        for f in (0..self.face_count()).filter(|&f| faces[f]) {
            match domain {
                AttrDomain::Face => targets[f] = true,
                AttrDomain::Corner => self.face(f).for_each(|c| targets[c] = true),
                AttrDomain::Point => {
                    for &p in self.face_points(f) {
                        targets[p as usize] = true;
                    }
                }
            }
        }
        let attrs = match domain {
            AttrDomain::Point => &mut self.points,
            AttrDomain::Corner => &mut self.corners,
            AttrDomain::Face => &mut self.faces,
        };
        let col = attrs.ensure(name, value.ty(), len)?;
        for (i, t) in targets.iter().enumerate() {
            if *t {
                col.set(i, value);
            }
        }
        Ok(())
    }

    /// Gives the faces in `faces` the material `path` (an empty path is the
    /// default material), adding it to the table if absent. Materials no
    /// face uses any more then leave the table, the others keeping their
    /// order, and a table left with only the default material is emptied
    /// (with the [`MATERIAL`] column): so a mesh painted with one material has
    /// a one-entry table, and one painted back to the default has none.
    pub fn set_material(&mut self, faces: &[bool], path: &str) -> Result<(), GeomError> {
        self.check_selection(faces)?;
        let index = self.material_index(path);
        let n = self.face_count();
        let col = self.faces.ensure(MATERIAL, super::ColumnType::Int, n)?;
        if let Column::Int(v) = col {
            for (m, s) in v.iter_mut().zip(faces) {
                if *s {
                    *m = index;
                }
            }
        }
        self.compact_materials();
        Ok(())
    }

    /// Drops the materials no face uses, keeping the others' order; a table
    /// left with only the empty path is emptied, with the [`MATERIAL`] column.
    fn compact_materials(&mut self) {
        let mut used = vec![false; self.part_count()];
        let Some(Column::Int(v)) = self.faces.get_mut(MATERIAL) else {
            return;
        };
        for &m in v.iter() {
            used[m as usize] = true;
        }
        let mut remap = vec![0i32; used.len()];
        let mut table = Vec::new();
        for (i, u) in used.iter().enumerate() {
            if *u {
                remap[i] = table.len() as i32;
                table.push(self.materials.get(i).cloned().unwrap_or_default());
            }
        }
        for m in v.iter_mut() {
            *m = remap[*m as usize];
        }
        if table.iter().all(String::is_empty) {
            self.materials.clear();
            self.faces.remove(MATERIAL);
        } else {
            self.materials = table;
        }
    }
}

/// `m` scaled, then rotated, then moved. The standard [`NORMAL`]s follow by
/// the inverse transpose, renormalised (a zero normal stays zero); other
/// columns are kept as they are. A scale with an odd number of negative axes
/// mirrors the mesh, so every face is rewound to keep its front.
/// `Err` (`RE4510`) for a non-finite argument, a zero scale, or points moved
/// out of the finite range.
pub fn transform(
    m: &PolyMesh,
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
) -> Result<PolyMesh, GeomError> {
    finite_vec("translation", translation)?;
    finite_vec("scale", scale)?;
    if !rotation.is_finite() {
        return Err(GeomError::Argument("rotation must be finite".into()));
    }
    if scale.x == 0.0 || scale.y == 0.0 || scale.z == 0.0 {
        return Err(GeomError::Argument(format!(
            "scale must not be zero on any axis, got {scale}"
        )));
    }
    let r = Rotation::from_quat(rotation);
    let mut out = m.clone();
    for p in &mut out.positions {
        let s = Vec3::new(p.x * scale.x, p.y * scale.y, p.z * scale.z);
        let q = r.apply(s);
        *p = Vec3::new(
            q.x + translation.x,
            q.y + translation.y,
            q.z + translation.z,
        );
    }
    if out.positions.iter().any(|p| !p.is_finite()) {
        return Err(GeomError::Argument(
            "the transform moves points beyond the finite range".into(),
        ));
    }
    if let Some(Column::Vec3(ns)) = out.corners.get_mut(NORMAL) {
        let inv = Vec3::new(1.0 / scale.x, 1.0 / scale.y, 1.0 / scale.z);
        for n in ns {
            let s = Vec3::new(n.x * inv.x, n.y * inv.y, n.z * inv.z);
            *n = normalize_or(r.apply(s), Vec3::ZERO);
        }
    }
    let negative = [scale.x, scale.y, scale.z]
        .iter()
        .filter(|s| **s < 0.0)
        .count();
    if negative % 2 == 1 {
        for f in 0..out.face_count() {
            out.rewind_face(f);
        }
    }
    Ok(out)
}

/// `m` reflected across the plane where coordinate `axis` is `offset`,
/// rewound so faces keep their fronts, its [`NORMAL`]s reflected. With
/// `keep`, the result is `m` and its reflection together: points lying
/// exactly on the plane are shared, so the two halves join there, and faces
/// lying wholly on the plane are left out of both.
pub fn mirror(m: &PolyMesh, axis: Axis, offset: f32, keep: bool) -> Result<PolyMesh, GeomError> {
    if !offset.is_finite() {
        return Err(GeomError::Argument(format!(
            "the mirror offset must be finite, got {offset}"
        )));
    }
    let a = axis.index();
    let reflect = |p: Vec3| {
        let mut v = p.to_array();
        v[a] = (offset + offset) - v[a];
        Vec3::from_array(v)
    };
    let on_plane = |p: Vec3| p.to_array()[a] == offset;
    // Faces wholly on the plane go from both halves.
    let keep_face: Vec<bool> = (0..m.face_count())
        .map(|f| !m.face_positions(f).all(on_plane))
        .collect();
    if keep {
        let shared = m.positions.iter().filter(|p| on_plane(**p)).count();
        let corners: usize = (0..m.face_count())
            .filter(|&f| keep_face[f])
            .map(|f| m.face(f).len())
            .sum();
        PolyMesh::check_size(2 * m.point_count() - shared, 2 * corners)?;
    }
    let mut reflected = m.clone();
    for p in &mut reflected.positions {
        *p = reflect(*p);
    }
    if reflected.positions.iter().any(|p| !p.is_finite()) {
        return Err(GeomError::Argument(
            "the mirror moves points beyond the finite range".into(),
        ));
    }
    if let Some(Column::Vec3(ns)) = reflected.corners.get_mut(NORMAL) {
        for n in ns {
            let mut v = n.to_array();
            v[a] = -v[a];
            *n = Vec3::from_array(v);
        }
    }
    for f in 0..reflected.face_count() {
        reflected.rewind_face(f);
    }
    if !keep {
        return Ok(reflected);
    }
    let base = drop_faces(m, &keep_face);
    let half = drop_faces(&reflected, &keep_face);
    // The reflected half's points, but those on the plane are the original's.
    let n = base.point_count();
    let mut map = vec![0u32; n];
    let mut new_points = Vec::new();
    for (p, slot) in map.iter_mut().enumerate() {
        if on_plane(base.positions[p]) {
            *slot = p as u32;
        } else {
            *slot = (n + new_points.len()) as u32;
            new_points.push(p as u32);
        }
    }
    let mut out = base;
    out.positions
        .extend(new_points.iter().map(|&p| half.positions[p as usize]));
    let extra = half.points.gather(&new_points);
    for (name, col) in out.points.columns_mut() {
        if let Some(src) = extra.get(name) {
            col.extend_from(src);
        }
    }
    let corner_base = out.corner_point.len() as u32;
    out.corner_point
        .extend(half.corner_point.iter().map(|&p| map[p as usize]));
    out.face_start
        .extend(half.face_start[1..].iter().map(|&s| s + corner_base));
    for (name, col) in out.corners.columns_mut() {
        if let Some(src) = half.corners.get(name) {
            col.extend_from(src);
        }
    }
    for (name, col) in out.faces.columns_mut() {
        if let Some(src) = half.faces.get(name) {
            col.extend_from(src);
        }
    }
    Ok(out)
}

/// `m` with only the faces whose `keep` entry is true (points unchanged).
pub(crate) fn drop_faces(m: &PolyMesh, keep: &[bool]) -> PolyMesh {
    if keep.iter().all(|k| *k) {
        return m.clone();
    }
    let mut out = m.clone();
    let mut keep_corner = Vec::with_capacity(m.corner_count());
    out.face_start = vec![0];
    out.corner_point.clear();
    for (f, &k) in keep.iter().enumerate() {
        let r = m.face(f);
        keep_corner.extend(std::iter::repeat_n(k, r.len()));
        if k {
            out.corner_point.extend_from_slice(&m.corner_point[r]);
            out.face_start.push(out.corner_point.len() as u32);
        }
    }
    out.corners.retain(&keep_corner);
    out.faces.retain(keep);
    out
}

/// `m` with points within `distance` of each other merged. Each point, in
/// index order, joins the lowest-indexed earlier point within `distance`
/// that has not joined another, keeping that point's position and values;
/// with 0, only points at the same position merge. A face then drops the
/// corners that repeat the point before them; one left with fewer than three
/// corners, a repeated point or no area goes. Merged points go, the others
/// keep their order. `Err` (`RE4505`) when a face whose points moved would
/// cross itself, which a smaller distance avoids.
pub fn weld(m: &PolyMesh, distance: f32) -> Result<PolyMesh, GeomError> {
    if !(distance.is_finite() && distance >= 0.0) {
        return Err(GeomError::Argument(format!(
            "the weld distance must be finite and not negative, got {distance}"
        )));
    }
    let n = m.point_count();
    let d = f64::from(distance);
    let cell = |p: Vec3| -> [i64; 3] {
        if distance > 0.0 {
            [
                (f64::from(p.x) / d).floor() as i64,
                (f64::from(p.y) / d).floor() as i64,
                (f64::from(p.z) / d).floor() as i64,
            ]
        } else {
            // Adding 0 turns -0 into 0, the same position.
            [
                i64::from((p.x + 0.0).to_bits()),
                i64::from((p.y + 0.0).to_bits()),
                i64::from((p.z + 0.0).to_bits()),
            ]
        }
    };
    let close = |a: Vec3, b: Vec3| {
        let (dx, dy, dz) = (
            f64::from(a.x) - f64::from(b.x),
            f64::from(a.y) - f64::from(b.y),
            f64::from(a.z) - f64::from(b.z),
        );
        dx * dx + dy * dy + dz * dz <= d * d
    };
    // Grid cells of kept points, for lookups only.
    let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    let mut map = vec![0u32; n];
    let mut kept: Vec<u32> = Vec::new();
    for (i, &p) in m.positions.iter().enumerate() {
        let c = cell(p);
        let mut best: Option<u32> = None;
        let reach: i64 = if distance > 0.0 { 1 } else { 0 };
        for dx in -reach..=reach {
            for dy in -reach..=reach {
                for dz in -reach..=reach {
                    let key = [c[0] + dx, c[1] + dy, c[2] + dz];
                    for &k in grid.get(&key).into_iter().flatten() {
                        if close(p, m.positions[k as usize]) && best.is_none_or(|b| k < b) {
                            best = Some(k);
                        }
                    }
                }
            }
        }
        match best {
            Some(k) => map[i] = map[k as usize],
            None => {
                map[i] = kept.len() as u32;
                kept.push(i as u32);
                grid.entry(c).or_default().push(i as u32);
            }
        }
    }
    let mut out = PolyMesh {
        positions: kept.iter().map(|&p| m.positions[p as usize]).collect(),
        face_start: vec![0],
        corner_point: Vec::with_capacity(m.corner_count()),
        points: m.points.gather(&kept),
        corners: m.corners.empty_like(m.corner_count()),
        faces: m.faces.empty_like(m.face_count()),
        materials: m.materials.clone(),
        collider: m.collider,
        visible: m.visible,
    };
    let mut pts: Vec<u32> = Vec::new();
    let mut corners: Vec<usize> = Vec::new();
    let mut seen = vec![u32::MAX; out.positions.len()];
    let mut moved: Vec<usize> = Vec::new();
    for f in 0..m.face_count() {
        pts.clear();
        corners.clear();
        let r = m.face(f);
        for c in r.clone() {
            let p = map[m.corner_point[c] as usize];
            if pts.last() != Some(&p) {
                pts.push(p);
                corners.push(c);
            }
        }
        while pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
            corners.pop();
        }
        if pts.len() < 3 {
            continue;
        }
        let repeats = pts.iter().any(|&p| {
            let again = seen[p as usize] == f as u32;
            seen[p as usize] = f as u32;
            again
        });
        if repeats || length64(newell(&out.positions, &pts)) <= 0.0 {
            continue;
        }
        let merged = |c: usize| kept[map[m.corner_point[c] as usize] as usize] != m.corner_point[c];
        if pts.len() > 3 && r.clone().any(merged) {
            moved.push(out.face_count());
        }
        out.corner_point.extend_from_slice(&pts);
        out.face_start.push(out.corner_point.len() as u32);
        for &c in &corners {
            out.corners.push_from(&m.corners, c);
        }
        out.faces.push_from(&m.faces, f);
    }
    // A face that kept three corners cannot cross itself; a larger one whose
    // points moved might.
    let mut tris = Vec::new();
    for f in moved {
        let n = out.face_area_vector(f);
        if super::tri::triangulate(&out, f, n, &mut tris).is_none() {
            return Err(GeomError::Triangulation(format!(
                "welding at {distance} m folds face {f} so that its boundary crosses itself; \
                 use a smaller distance"
            )));
        }
    }
    Ok(out)
}

/// One mesh holding every input's points and faces, in input order.
/// Columns are unioned, and a value a mesh lacks takes the column's default;
/// material tables are unioned in order. The collider and visibility are
/// the first mesh's. `Err` (`RE4510`) when one attribute name has two types,
/// `RE4515` when the result is too large.
pub fn merge<'a>(meshes: impl IntoIterator<Item = &'a PolyMesh>) -> Result<PolyMesh, GeomError> {
    let meshes: Vec<&PolyMesh> = meshes.into_iter().collect();
    let Some(first) = meshes.first() else {
        return Ok(PolyMesh::new());
    };
    let points: usize = meshes.iter().map(|m| m.point_count()).sum();
    let corners: usize = meshes.iter().map(|m| m.corner_count()).sum();
    let faces: usize = meshes.iter().map(|m| m.face_count()).sum();
    PolyMesh::check_size(points, corners)?;
    let p_layout = union_layout(meshes.iter().map(|m| &m.points), AttrDomain::Point)?;
    let c_layout = union_layout(meshes.iter().map(|m| &m.corners), AttrDomain::Corner)?;
    let mut f_layout = union_layout(meshes.iter().map(|m| &m.faces), AttrDomain::Face)?;
    // Material tables, an empty one counting as the empty path.
    let mut materials: Vec<String> = Vec::new();
    let mut remap: Vec<Vec<i32>> = Vec::with_capacity(meshes.len());
    for m in &meshes {
        let table: Vec<&str> = if m.materials.is_empty() {
            vec![""]
        } else {
            m.materials.iter().map(String::as_str).collect()
        };
        remap.push(
            table
                .iter()
                .map(|t| match materials.iter().position(|x| x == t) {
                    Some(i) => i as i32,
                    None => {
                        materials.push((*t).to_string());
                        materials.len() as i32 - 1
                    }
                })
                .collect(),
        );
    }
    if materials == [""] {
        materials.clear();
    }
    let any_material = !materials.is_empty();
    if any_material && f_layout.iter().all(|(n, _)| n != MATERIAL) {
        let i = f_layout
            .binary_search_by(|(n, _)| n.as_str().cmp(MATERIAL))
            .unwrap_or_else(|i| i);
        f_layout.insert(i, (MATERIAL.to_string(), super::ColumnType::Int));
    }
    let mut out = PolyMesh {
        positions: Vec::with_capacity(points),
        face_start: Vec::with_capacity(faces + 1),
        corner_point: Vec::with_capacity(corners),
        points: Attributes::from_layout(&p_layout, points),
        corners: Attributes::from_layout(&c_layout, corners),
        faces: Attributes::from_layout(&f_layout, faces),
        materials,
        collider: first.collider,
        visible: first.visible,
    };
    out.face_start.push(0);
    for (m, remap) in meshes.iter().zip(&remap) {
        let p_base = out.positions.len() as u32;
        let c_base = out.corner_point.len() as u32;
        out.positions.extend_from_slice(&m.positions);
        out.corner_point
            .extend(m.corner_point.iter().map(|&p| p + p_base));
        out.face_start
            .extend(m.face_start[1..].iter().map(|&s| s + c_base));
        out.points.extend_partial(&m.points, m.point_count());
        out.corners.extend_partial(&m.corners, m.corner_count());
        let before = out.face_count() - m.face_count();
        out.faces.extend_partial(&m.faces, m.face_count());
        if any_material && let Some(Column::Int(v)) = out.faces.get_mut(MATERIAL) {
            let old = m.face_materials();
            for (slot, o) in v[before..].iter_mut().zip(old) {
                *slot = remap[o as usize];
            }
        }
    }
    Ok(out)
}

/// A polygon mesh of a triangle mesh: points at distinct positions (in
/// first use order; -0 and 0 are one), one face per triangle with a repeated point or no area
/// left out, and the vertices' UVs, normals and (unless all white) colours
/// as corner attributes. `Err` (`RE4510`) for an index out of range or a
/// non-finite position, `RE4515` when too large.
pub fn from_mesh(mesh: &MeshData) -> Result<PolyMesh, GeomError> {
    if !mesh.is_well_formed() {
        return Err(GeomError::Argument(
            "the mesh has an index out of range or a partial triangle".into(),
        ));
    }
    if let Some(i) = mesh
        .vertices
        .iter()
        .position(|v| !v.pos.iter().all(|x| x.is_finite()))
    {
        return Err(GeomError::Argument(format!("vertex {i} is not finite")));
    }
    PolyMesh::check_size(mesh.vertices.len(), mesh.indices.len())?;
    let mut ids: HashMap<[u32; 3], u32> = HashMap::new();
    let mut point_of = Vec::with_capacity(mesh.vertices.len());
    let mut out = PolyMesh::new();
    for v in &mesh.vertices {
        // Adding 0 turns -0 into 0, the same position.
        let p = Vec3::from_array(v.pos) + Vec3::ZERO;
        let key = [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        let next = out.positions.len() as u32;
        let id = *ids.entry(key).or_insert(next);
        if id == next {
            out.positions.push(p);
        }
        point_of.push(id);
    }
    // Points are numbered by first use in the vertex list; renumber them by
    // first use in the triangles so the order follows the faces.
    let mut uv = Vec::with_capacity(mesh.indices.len());
    let mut normal = Vec::with_capacity(mesh.indices.len());
    let mut color = Vec::with_capacity(mesh.indices.len());
    for t in mesh.indices.chunks_exact(3) {
        let pts = [
            point_of[t[0] as usize],
            point_of[t[1] as usize],
            point_of[t[2] as usize],
        ];
        if pts[0] == pts[1] || pts[1] == pts[2] || pts[0] == pts[2] {
            continue;
        }
        if length64(newell(&out.positions, &pts)) <= 0.0 {
            continue;
        }
        out.corner_point.extend_from_slice(&pts);
        out.face_start.push(out.corner_point.len() as u32);
        for &i in t {
            let v = &mesh.vertices[i as usize];
            uv.push(v.uv);
            normal.push(Vec3::from_array(v.normal));
            color.push(v.color);
        }
    }
    // Keep only the points the faces use, in first use order.
    let mut renumber = vec![u32::MAX; out.positions.len()];
    let mut used = Vec::new();
    for p in &mut out.corner_point {
        if renumber[*p as usize] == u32::MAX {
            renumber[*p as usize] = used.len() as u32;
            used.push(*p);
        }
        *p = renumber[*p as usize];
    }
    out.positions = used.iter().map(|&p| out.positions[p as usize]).collect();
    if normal.iter().any(|n| !n.is_finite()) || uv.iter().flatten().any(|x| !x.is_finite()) {
        return Err(GeomError::Argument(
            "a vertex normal or UV is not finite".into(),
        ));
    }
    out.corners.insert(UV, Column::Vec2(uv))?;
    out.corners.insert(NORMAL, Column::Vec3(normal))?;
    if color.iter().any(|c| *c != [1.0; 4]) {
        if color.iter().flatten().any(|x| !x.is_finite()) {
            return Err(GeomError::Argument("a vertex colour is not finite".into()));
        }
        out.corners.insert(COLOR, Column::Vec4(color))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poly::tests::cube;

    #[test]
    fn a_negative_scale_rewinds_and_keeps_the_volume_positive() {
        let m = cube();
        let t = transform(
            &m,
            Vec3::new(1.0, 2.0, 3.0),
            Quat::IDENTITY,
            Vec3::new(-2.0, 1.0, 1.0),
        )
        .unwrap();
        t.validate().unwrap();
        assert_eq!(t.volume(), Some(2.0));
        let b = t.bounds().unwrap();
        assert_eq!(
            (b.min, b.max),
            (Vec3::new(-1.0, 2.0, 3.0), Vec3::new(1.0, 3.0, 4.0))
        );
        assert!(transform(&m, Vec3::ZERO, Quat::IDENTITY, Vec3::new(0.0, 1.0, 1.0)).is_err());
    }

    #[test]
    fn mirroring_an_open_half_joins_it_on_the_plane() {
        // The cube without its -X face, mirrored across x = 0 with `keep`:
        // a closed box from -1 to 1 with no face on the plane.
        let m = drop_faces(&cube(), &[false, true, true, true, true, true]);
        let both = mirror(&m, Axis::X, 0.0, true).unwrap();
        both.validate().unwrap();
        assert_eq!(both.point_count(), 12);
        assert_eq!(both.face_count(), 10);
        assert!(both.is_closed());
        assert_eq!(both.volume(), Some(2.0));
        // Without `keep`, only the reflection.
        let r = mirror(&cube(), Axis::Y, 2.0, false).unwrap();
        r.validate().unwrap();
        assert_eq!(r.volume(), Some(1.0));
        assert_eq!(r.bounds().unwrap().min, Vec3::new(0.0, 3.0, 0.0));
    }

    #[test]
    fn faces_on_the_mirror_plane_go_from_both_halves() {
        let both = mirror(&cube(), Axis::X, 0.0, true).unwrap();
        both.validate().unwrap();
        assert_eq!(both.face_count(), 10);
        assert!(both.is_closed());
    }

    #[test]
    fn welding_merges_close_points_and_drops_collapsed_faces() {
        // Two cubes side by side, the second's touching face 1 mm away.
        let a = cube();
        let b = transform(&a, Vec3::new(1.001, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE).unwrap();
        let m = merge([&a, &b]).unwrap();
        assert_eq!(m.point_count(), 16);
        let w = weld(&m, 0.01).unwrap();
        w.validate().unwrap();
        assert_eq!(w.point_count(), 12);
        assert_eq!(w.face_count(), 12);
        // A weld wider than the cube collapses everything.
        let all = weld(&cube(), 2.0).unwrap();
        all.validate().unwrap();
        assert_eq!(all.point_count(), 1);
        assert_eq!(all.face_count(), 0);
        // Zero merges exact duplicates only.
        let dup = merge([&a, &a]).unwrap();
        assert_eq!(weld(&dup, 0.0).unwrap().point_count(), 8);
    }

    #[test]
    fn merging_unions_columns_and_materials() {
        let mut a = cube();
        a.set_material(&[true; 6], "stone").unwrap();
        a.set_group("top", vec![false, false, false, true, false, false])
            .unwrap();
        let mut b = cube();
        b.fill_attribute(AttrDomain::Point, "heat", AttrValue::Float(2.0), &[true; 6])
            .unwrap();
        assert_eq!(a.materials, ["stone"], "painted all over: one entry");
        let m = merge([&a, &b, &a]).unwrap();
        m.validate().unwrap();
        assert_eq!(m.materials, ["stone", ""]);
        assert_eq!(
            m.face_materials(),
            [[0; 6], [1; 6], [0; 6]].concat(),
            "b's faces keep the default material"
        );
        assert_eq!(
            m.faces.bools("top").unwrap().iter().filter(|x| **x).count(),
            2
        );
        assert_eq!(m.points.floats("heat").unwrap()[8], 2.0);
        assert_eq!(m.points.floats("heat").unwrap()[0], 0.0);
        let mut c = cube();
        c.fill_attribute(AttrDomain::Point, "heat", AttrValue::Int(1), &[true; 6])
            .unwrap();
        assert!(matches!(merge([&b, &c]), Err(GeomError::Argument(_))));
    }

    #[test]
    fn unused_materials_leave_the_table() {
        let mut m = cube();
        let top = [false, false, false, true, false, false];
        m.set_material(&top, "roof").unwrap();
        assert_eq!(m.materials, ["", "roof"]);
        m.set_material(&top.map(|t| !t), "wall").unwrap();
        assert_eq!(
            m.materials,
            ["roof", "wall"],
            "no face is left on the default"
        );
        assert_eq!(m.face_materials(), [1, 1, 1, 0, 1, 1]);
        m.set_material(&[true; 6], "").unwrap();
        m.validate().unwrap();
        assert_eq!(m, cube(), "painted back to the default, as it was");
    }

    #[test]
    fn a_triangle_mesh_converts_with_shared_points() {
        let mesh = crate::cuboid(Vec3::splat(0.5)).unwrap();
        let m = from_mesh(&mesh).unwrap();
        m.validate().unwrap();
        assert_eq!(m.point_count(), 8);
        assert_eq!(m.face_count(), 12);
        assert!(m.is_closed());
        assert!((m.volume().unwrap() - 1.0).abs() < 1e-12);
        assert!(m.corners.get(COLOR).is_none());
        // And back: flat-shaded, as the cuboid was.
        let back = m.to_mesh().unwrap();
        assert_eq!(back.triangle_count(), 12);
        assert_eq!(back.vertices.len(), 24);
    }

    #[test]
    fn attributes_fill_only_the_selected_faces() {
        let mut m = cube();
        let sel = [true, false, false, false, false, false];
        m.fill_attribute(AttrDomain::Face, COLOR, AttrValue::Vec3(Vec3::X), &sel)
            .unwrap();
        let c = m.faces.vec3s(COLOR).unwrap();
        assert_eq!(c[0], Vec3::X);
        assert_eq!(c[1], Vec3::ONE, "colour defaults to white");
        assert!(
            m.fill_attribute(AttrDomain::Point, UV, AttrValue::Vec2([0.0; 2]), &sel)
                .is_err()
        );
        assert!(
            m.fill_attribute(AttrDomain::Face, COLOR, AttrValue::Float(1.0), &sel)
                .is_err()
        );
    }
}
