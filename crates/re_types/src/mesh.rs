//! Mesh data: what `re_assets` imports and `re_geom` builds, and what
//! `re_render` uploads.
//!
//! These types live in the contract layer (not in `re_geom`) so that `re_render`
//! and `re_assets` depend only on it.

use glam::Vec3;
use serde::{Deserialize, Serialize};

/// One vertex: position, normal, texture coordinate and colour.
///
/// The layout is `#[repr(C)]` with no padding: 12 `f32`s, 48 bytes, in the order
/// `pos`, `normal`, `uv`, `color`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Vertex {
    /// Position in model space.
    pub pos: [f32; 3],
    /// Unit normal in model space.
    pub normal: [f32; 3],
    /// Texture coordinate (glTF convention: origin at the top left).
    pub uv: [f32; 2],
    /// Linear RGBA vertex colour (white when the source has none).
    pub color: [f32; 4],
}

/// An indexed triangle list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MeshData {
    /// The vertices.
    pub vertices: Vec<Vertex>,
    /// Three indices per triangle, counter-clockwise when seen from the front.
    pub indices: Vec<u32>,
}

impl MeshData {
    /// The number of whole triangles.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// True when the index count is a multiple of three and every index names a vertex.
    pub fn is_well_formed(&self) -> bool {
        self.indices.len().is_multiple_of(3)
            && self
                .indices
                .iter()
                .all(|&i| (i as usize) < self.vertices.len())
    }

    /// The bounding box of the vertex positions, or `None` for an empty mesh.
    pub fn aabb(&self) -> Option<Aabb> {
        Aabb::from_points(self.vertices.iter().map(|v| Vec3::from_array(v.pos)))
    }
}

/// Skin weights for a [`MeshData`]: up to four joints per vertex (ADR-0039).
///
/// `joints` and `weights` have one entry per vertex. Joint indices refer to a
/// [`SkinData`](crate::anim::SkinData)'s joints. Weights are unorm16 (65,535 is
/// 1.0) and each vertex's weights sum to 65,535.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkinWeights {
    /// Four joint indices per vertex.
    pub joints: Vec<[u16; 4]>,
    /// Four weights per vertex, matching `joints`.
    pub weights: Vec<[u16; 4]>,
}

impl SkinWeights {
    /// True when there is one entry per vertex of `mesh` in both lists, each
    /// vertex's weights sum to 65,535 and every joint index with a weight is below
    /// `joint_count`.
    pub fn is_well_formed(&self, mesh: &MeshData, joint_count: usize) -> bool {
        self.joints.len() == mesh.vertices.len()
            && self.weights.len() == mesh.vertices.len()
            && self.joints.iter().zip(&self.weights).all(|(j, w)| {
                w.iter().map(|&x| u32::from(x)).sum::<u32>() == u32::from(u16::MAX)
                    && j.iter()
                        .zip(w)
                        .all(|(&j, &w)| w == 0 || (j as usize) < joint_count)
            })
    }
}

/// An axis-aligned bounding box.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    /// The minimum corner.
    pub min: Vec3,
    /// The maximum corner.
    pub max: Vec3,
}

impl Aabb {
    /// The box from `min` to `max`.
    pub const fn new(min: Vec3, max: Vec3) -> Aabb {
        Aabb { min, max }
    }

    /// The smallest box containing every point, or `None` when there are none.
    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Option<Aabb> {
        let mut points = points.into_iter();
        let first = points.next()?;
        Some(points.fold(Aabb::new(first, first), |b, p| Aabb {
            min: b.min.min(p),
            max: b.max.max(p),
        }))
    }

    /// The centre.
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Half the size along each axis.
    pub fn half_extents(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    /// The smallest box containing both.
    #[must_use]
    pub fn union(&self, other: &Aabb) -> Aabb {
        Aabb {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    /// The smallest box containing this box's eight corners moved by `m`.
    #[must_use]
    pub fn transformed(&self, m: &glam::Mat4) -> Aabb {
        let corners = (0..8).map(|i| {
            m.transform_point3(Vec3::new(
                if i & 1 == 0 { self.min.x } else { self.max.x },
                if i & 2 == 0 { self.min.y } else { self.max.y },
                if i & 4 == 0 { self.min.z } else { self.max.z },
            ))
        });
        Aabb::from_points(corners).unwrap_or(*self)
    }

    /// True when `p` is inside or on the boundary.
    pub fn contains(&self, p: Vec3) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }
}
