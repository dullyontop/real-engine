//! [`Topology`]: a mesh's adjacency, computed when an operation needs it.

use super::PolyMesh;

/// [`Topology::twin`] of a corner whose edge has no single opposite corner.
pub const NO_TWIN: u32 = u32::MAX;

/// The adjacency of a [`PolyMesh`]'s faces, in linear passes: per corner its
/// face, next and previous corner and twin; per point the corners leaving
/// it, in corner order.
///
/// Corner `c`'s edge runs from its point to the next corner's point. Its
/// twin is the one corner whose edge runs the other way, when there is
/// exactly one such corner and `c` is the only corner running its way; an
/// edge used by more than two corners, or twice in one direction, has no
/// twins, and operations treat it as a boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topology {
    /// Per corner, its face.
    pub corner_face: Vec<u32>,
    /// Per corner, the next corner of its face.
    pub next: Vec<u32>,
    /// Per corner, the previous corner of its face.
    pub prev: Vec<u32>,
    /// Per corner, its twin, or [`NO_TWIN`].
    pub twin: Vec<u32>,
    /// One offset per point and one more into `point_corners`.
    pub point_start: Vec<u32>,
    /// The corners at each point, grouped by point, each group in corner
    /// order.
    pub point_corners: Vec<u32>,
}

impl Topology {
    /// The adjacency of `m`, which must be valid.
    pub fn new(m: &PolyMesh) -> Topology {
        let (n_points, n_corners) = (m.point_count(), m.corner_count());
        let mut corner_face = Vec::with_capacity(n_corners);
        let mut next = Vec::with_capacity(n_corners);
        let mut prev = Vec::with_capacity(n_corners);
        for f in 0..m.face_count() {
            let r = m.face(f);
            let (start, end) = (r.start as u32, r.end as u32);
            for c in start..end {
                corner_face.push(f as u32);
                next.push(if c + 1 == end { start } else { c + 1 });
                prev.push(if c == start { end - 1 } else { c - 1 });
            }
        }
        // Corners by point: a counting sort, so each point's corners stay in
        // corner order.
        let mut point_start = vec![0u32; n_points + 1];
        for &p in &m.corner_point {
            point_start[p as usize + 1] += 1;
        }
        for p in 0..n_points {
            point_start[p + 1] += point_start[p];
        }
        let mut fill = point_start.clone();
        let mut point_corners = vec![0u32; n_corners];
        for (c, &p) in m.corner_point.iter().enumerate() {
            point_corners[fill[p as usize] as usize] = c as u32;
            fill[p as usize] += 1;
        }
        let mut t = Topology {
            corner_face,
            next,
            prev,
            twin: vec![NO_TWIN; n_corners],
            point_start,
            point_corners,
        };
        for c in 0..n_corners {
            let (a, b) = t.edge(m, c as u32);
            let (back, found) = t.count_edges(m, b, a);
            if back == 1 && t.count_edges(m, a, b).0 == 1 {
                t.twin[c] = found;
            }
        }
        t
    }

    /// The corners at point `p`, in corner order.
    pub fn corners_at(&self, p: u32) -> &[u32] {
        &self.point_corners
            [self.point_start[p as usize] as usize..self.point_start[p as usize + 1] as usize]
    }

    /// Corner `c`'s edge: its point and the next corner's.
    pub fn edge(&self, m: &PolyMesh, c: u32) -> (u32, u32) {
        (
            m.corner_point[c as usize],
            m.corner_point[self.next[c as usize] as usize],
        )
    }

    /// How many corners have the edge `a -> b`, and the last of them.
    fn count_edges(&self, m: &PolyMesh, a: u32, b: u32) -> (u32, u32) {
        let mut count = 0;
        let mut last = NO_TWIN;
        for &d in self.corners_at(a) {
            if m.corner_point[self.next[d as usize] as usize] == b {
                count += 1;
                last = d;
            }
        }
        (count, last)
    }

    /// The number of corners on an edge used by more than two corners or
    /// twice in one direction.
    pub fn non_manifold_edges(&self, m: &PolyMesh) -> usize {
        (0..m.corner_count() as u32)
            .filter(|&c| self.twin[c as usize] == NO_TWIN)
            .filter(|&c| {
                let (a, b) = self.edge(m, c);
                self.count_edges(m, a, b).0 + self.count_edges(m, b, a).0 > 1
            })
            .count()
    }

    /// The next corner around corner `c`'s point, across the edge into `c`
    /// (`None` at a boundary).
    pub fn around(&self, c: u32) -> Option<u32> {
        let t = self.twin[self.prev[c as usize] as usize];
        (t != NO_TWIN).then_some(t)
    }

    /// The previous corner around corner `c`'s point, across `c`'s edge
    /// (`None` at a boundary).
    pub fn around_back(&self, c: u32) -> Option<u32> {
        let t = self.twin[c as usize];
        (t != NO_TWIN).then(|| self.next[t as usize])
    }

    /// True if the corners at every point form one fan: walking around the
    /// point across edges reaches all of them.
    pub fn fans_are_single(&self, m: &PolyMesh) -> bool {
        (0..m.point_count() as u32).all(|p| {
            let at = self.corners_at(p);
            let Some(&first) = at.first() else {
                return true;
            };
            let mut seen = 1;
            let mut c = first;
            let mut closed = false;
            while let Some(d) = self.around(c) {
                if d == first {
                    closed = true;
                    break;
                }
                seen += 1;
                if seen > at.len() {
                    return false;
                }
                c = d;
            }
            if !closed {
                c = first;
                while let Some(d) = self.around_back(c) {
                    seen += 1;
                    if seen > at.len() {
                        return false;
                    }
                    c = d;
                }
            }
            seen == at.len()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn quad_strip(n: u32) -> PolyMesh {
        let mut m = PolyMesh::new();
        for i in 0..=n {
            m.positions.push(Vec3::new(i as f32, 0.0, 0.0));
            m.positions.push(Vec3::new(i as f32, 0.0, -1.0));
        }
        for i in 0..n {
            m.corner_point
                .extend_from_slice(&[2 * i, 2 * i + 2, 2 * i + 3, 2 * i + 1]);
            m.face_start.push(m.corner_point.len() as u32);
        }
        m
    }

    #[test]
    fn a_strip_pairs_its_inner_edges() {
        let m = quad_strip(3);
        m.validate().unwrap();
        let t = m.topology();
        // Face 0's second edge (2 -> 3) is face 1's fourth (3 -> 2).
        assert_eq!(t.twin[1], 7);
        assert_eq!(t.twin[7], 1);
        assert_eq!(t.twin.iter().filter(|&&x| x == NO_TWIN).count(), 8);
        assert_eq!(t.corners_at(2), &[1, 4]);
        assert_eq!(t.non_manifold_edges(&m), 0);
        assert!(t.fans_are_single(&m));
    }

    #[test]
    fn an_edge_with_three_faces_has_no_twins() {
        // Three quads on the edge 0 -> 1, like walls meeting on one edge.
        let mut m = PolyMesh::new();
        for p in [
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [-1.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 1.0],
            [0.0, 0.0, 1.0],
        ] {
            m.positions.push(Vec3::from_array(p));
        }
        for f in [[0u32, 1, 2, 3], [1, 0, 5, 4], [0, 1, 6, 7]] {
            m.corner_point.extend_from_slice(&f);
            m.face_start.push(m.corner_point.len() as u32);
        }
        m.validate().unwrap();
        let t = m.topology();
        assert_eq!(t.twin[0], NO_TWIN);
        assert_eq!(t.twin[4], NO_TWIN);
        assert_eq!(t.non_manifold_edges(&m), 3);
        assert!(!m.is_manifold());
    }

    #[test]
    fn two_fans_at_one_point_are_not_manifold() {
        // Two triangles touching only at point 0.
        let mut m = PolyMesh::new();
        for p in [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, -1.0],
            [-1.0, 0.0, 0.0],
            [-1.0, 0.0, 1.0],
        ] {
            m.positions.push(Vec3::from_array(p));
        }
        for f in [[0u32, 1, 2], [0, 3, 4]] {
            m.corner_point.extend_from_slice(&f);
            m.face_start.push(m.corner_point.len() as u32);
        }
        m.validate().unwrap();
        let t = m.topology();
        assert_eq!(t.non_manifold_edges(&m), 0);
        assert!(!t.fans_are_single(&m));
    }
}
