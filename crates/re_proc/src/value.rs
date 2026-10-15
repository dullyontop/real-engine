//! The values that flow along a graph's edges: [`Field`], [`MeshValue`],
//! [`Points`] and [`Fragment`], carried as an [`Output`].

use std::sync::Arc;

use glam::Vec3;
use re_geom::Heightfield;
use re_reflect::Value;
use re_scene::PrefabFile;
use re_types::mesh::MeshData;
use serde::{Deserialize, Serialize};

/// The type of a port.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PortType {
    /// A [`Field`]: a square grid of numbers in metres (heights) or weights.
    Field,
    /// A [`MeshValue`]: one mesh, optionally with a material.
    Mesh,
    /// [`Points`]: positions with normals, ids and tags.
    Points,
    /// A [`Fragment`]: entities to create.
    Fragment,
}

impl PortType {
    /// The name of a node's output port of this type (`field`, `mesh`,
    /// `points` or `fragment`).
    pub fn port_name(self) -> &'static str {
        match self {
            PortType::Field => "field",
            PortType::Mesh => "mesh",
            PortType::Points => "points",
            PortType::Fragment => "fragment",
        }
    }
}

impl std::fmt::Display for PortType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// A square grid of values centred on the origin in X and Z: a
/// [`Heightfield`] whose samples are heights in metres (terrain) or weights
/// (masks).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field(pub Heightfield);

impl Field {
    /// A centred field of `n × n` values, `spacing` metres apart.
    pub fn new(n: u32, spacing: f32, values: Vec<f32>) -> Result<Field, String> {
        Heightfield::centered(n, spacing, values)
            .map(Field)
            .map_err(|e| e.to_string())
    }

    /// Samples per side.
    pub fn n(&self) -> u32 {
        self.0.n
    }

    /// Half the side length, in metres.
    pub fn half_extent(&self) -> f32 {
        Heightfield::half_extent(self.0.n, self.0.spacing)
    }

    /// The value at normalised coordinates `u, v` in `[0, 1]` (clamped),
    /// bilinearly interpolated. Used to combine fields of different sizes.
    pub fn sample_uv(&self, u: f32, v: f32) -> f32 {
        let last = (self.0.n - 1) as f32;
        let fx = (u.clamp(0.0, 1.0)) * last;
        let fz = (v.clamp(0.0, 1.0)) * last;
        let ix = (fx as u32).min(self.0.n - 2);
        let iz = (fz as u32).min(self.0.n - 2);
        let (tx, tz) = (fx - ix as f32, fz - iz as f32);
        let h = |x, z| self.0.sample(x, z);
        let top = h(ix, iz) + (h(ix + 1, iz) - h(ix, iz)) * tx;
        let bottom = h(ix, iz + 1) + (h(ix + 1, iz + 1) - h(ix, iz + 1)) * tx;
        top + (bottom - top) * tz
    }

    /// This field's values on an `n × n` grid (itself when `n` matches).
    pub fn resampled(&self, n: u32) -> Vec<f32> {
        if n == self.0.n {
            return self.0.samples.clone();
        }
        let last = (n - 1) as f32;
        let mut out = Vec::with_capacity((n * n) as usize);
        for z in 0..n {
            for x in 0..n {
                out.push(self.sample_uv(x as f32 / last, z as f32 / last));
            }
        }
        out
    }
}

/// A mesh flowing between mesh nodes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshValue {
    /// The geometry.
    pub mesh: MeshData,
    /// A material file path (`materials/stone.material.json`), or empty.
    pub material: String,
    /// The key of the node that made the geometry: the mesh is the derived
    /// asset `derived:<hex of this key>`. Nodes that only change the
    /// material keep it, so the GPU mesh is shared.
    pub asset: [u8; 32],
}

/// One point of a [`Points`] set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Stable within the producing scatter node; becomes the local key of
    /// anything instanced at the point.
    pub id: u32,
    /// Position in the source's local space.
    pub position: Vec3,
    /// The surface normal at the point (unit length).
    pub normal: Vec3,
    /// A tag chosen by `points.pick` (empty until then).
    pub tag: String,
}

impl Point {
    /// The slope at the point as rise over run (0 is flat, 1 is 45°).
    pub fn slope(&self) -> f32 {
        let ny = self.normal.y.clamp(-1.0, 1.0);
        if ny <= 0.0 {
            return f32::INFINITY;
        }
        (1.0 - ny * ny).max(0.0).sqrt() / ny
    }
}

/// A set of points.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Points {
    /// The points, in a deterministic order.
    pub points: Vec<Point>,
}

/// A derived asset made by a graph: a mesh (`MeshRef.asset`) or a heightfield
/// (`Collider.source`), both named `derived:<64 hex digits>`.
#[derive(Clone, Debug, PartialEq)]
pub enum DerivedAsset {
    /// A render mesh.
    Mesh(Arc<MeshData>),
    /// A heightfield collider.
    Heightfield(Arc<Heightfield>),
}

/// A prefab used by a `points.instance` node.
#[derive(Clone, Debug, PartialEq)]
pub struct PrefabUse {
    /// The prefab file's path, as written in the node's parameters.
    pub path: String,
    /// The parsed file.
    pub file: PrefabFile,
}

/// One entity of a [`Fragment`].
#[derive(Clone, Debug, PartialEq)]
pub struct FragItem {
    /// The StableKey: `<node id>/<local key>`.
    pub key: String,
    /// The display name.
    pub name: String,
    /// The key of an earlier item, or `None` for a child of the source.
    pub parent_key: Option<String>,
    /// Components by name, with the fields to set.
    pub components: Vec<(String, Vec<(String, Value)>)>,
    /// When set, the prefab's entities are instantiated under this item with
    /// keys `<key>/<prefab entity id>`.
    pub prefab: Option<Arc<PrefabUse>>,
}

/// Entities to create, plus the derived assets they reference.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fragment {
    /// The entities, parents before children.
    pub items: Vec<FragItem>,
    /// The derived assets the items reference, by `derived:<hex>` name.
    pub assets: Vec<(String, DerivedAsset)>,
}

/// A node's output.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// A field.
    Field(Arc<Field>),
    /// A mesh.
    Mesh(Arc<MeshValue>),
    /// Points.
    Points(Arc<Points>),
    /// A fragment.
    Fragment(Arc<Fragment>),
}

impl Output {
    /// The output's port type.
    pub fn port_type(&self) -> PortType {
        match self {
            Output::Field(_) => PortType::Field,
            Output::Mesh(_) => PortType::Mesh,
            Output::Points(_) => PortType::Points,
            Output::Fragment(_) => PortType::Fragment,
        }
    }

    /// `(items, bytes)`: samples, vertices, points or entities, and an
    /// estimate of the memory held.
    pub fn size(&self) -> (u64, u64) {
        match self {
            Output::Field(f) => (f.0.samples.len() as u64, f.0.samples.len() as u64 * 4),
            Output::Mesh(m) => (
                m.mesh.vertices.len() as u64,
                m.mesh.vertices.len() as u64 * 48 + m.mesh.indices.len() as u64 * 4,
            ),
            Output::Points(p) => (p.points.len() as u64, p.points.len() as u64 * 48),
            Output::Fragment(f) => {
                let assets: u64 = f
                    .assets
                    .iter()
                    .map(|(_, a)| match a {
                        DerivedAsset::Mesh(m) => {
                            m.vertices.len() as u64 * 48 + m.indices.len() as u64 * 4
                        }
                        DerivedAsset::Heightfield(h) => h.samples.len() as u64 * 4,
                    })
                    .sum();
                (f.items.len() as u64, f.items.len() as u64 * 256 + assets)
            }
        }
    }
}

/// `derived:<64 hex digits>` for a 32-byte key.
pub fn derived_name(key: &[u8; 32]) -> String {
    format!("derived:{}", blake3::Hash::from_bytes(*key).to_hex())
}
