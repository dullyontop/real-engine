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

    /// True when `p` is inside or on the boundary.
    pub fn contains(&self, p: Vec3) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }
}
