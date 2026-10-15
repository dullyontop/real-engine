//! [`Media`]: the CPU side of a project's meshes, materials and clips, on top of
//! `re_assets::AssetServer`.
//!
//! `MeshRef.asset` names a glTF file (all of its meshes, placed by the default
//! scene's node transforms, merged into one), a built-in shape (`builtin:cube`,
//! `builtin:sphere`, `builtin:plane`, `builtin:capsule`), or a `derived:` mesh
//! registered by the procedural pipeline ([`Media::insert_derived_mesh`]).
//! `MaterialRef.asset` names a `.material.json` file. Results are cached by
//! asset string until the file changes ([`Media::apply_changes`]).

use std::collections::{HashMap, HashSet};
use std::f32::consts::{PI, TAU};
use std::path::Path;
use std::sync::Arc;

use glam::{Affine3A, Vec3};
use re_assets::{AssetChange, AssetServer, ReloadReport};
use re_core::{Diagnostic, Location};
use re_types::{ClipData, MaterialDesc, MeshData, TextureData, Vertex};

use crate::codes;

/// The prefix of built-in mesh names.
pub const BUILTIN: &str = "builtin:";

/// The prefix of procedurally derived asset names.
pub const DERIVED: &str = "derived:";

/// A material and the texture data it refers to, by texture slot.
#[derive(Clone, Debug)]
pub struct LoadedMaterial {
    /// The material, with textures as `TextureRef::Asset` ids.
    pub desc: MaterialDesc,
    /// `(slot, texture id, pixels)` for each texture the material uses.
    pub textures: Vec<(usize, String, TextureData)>,
}

/// A mesh asset: its geometry and, for a glTF file, the file's first material.
#[derive(Clone, Debug)]
pub struct LoadedMesh {
    /// The merged geometry.
    pub mesh: Arc<MeshData>,
    /// The material the file gives its first mesh, if any.
    pub material: Option<LoadedMaterial>,
}

/// Meshes, materials and clips by the asset strings scenes use.
pub struct Media {
    server: AssetServer,
    meshes: HashMap<String, LoadedMesh>,
    derived: HashMap<String, Arc<MeshData>>,
    materials: HashMap<String, LoadedMaterial>,
    clips: HashMap<String, Arc<ClipData>>,
    /// Loads that failed, so a broken file is not read again every frame; a
    /// change to any file forgets them.
    failed: HashMap<String, Diagnostic>,
    reported: HashSet<String>,
}

impl Media {
    /// Media for the project at `root` (asset cache in `<root>/.real/cache/assets`).
    pub fn new(root: &Path) -> Media {
        Media {
            server: AssetServer::new(root),
            meshes: HashMap::new(),
            derived: HashMap::new(),
            materials: HashMap::new(),
            clips: HashMap::new(),
            failed: HashMap::new(),
            reported: HashSet::new(),
        }
    }

    /// The asset server.
    pub fn server(&mut self) -> &mut AssetServer {
        &mut self.server
    }

    /// Registers a procedurally generated mesh under its `derived:` name.
    pub fn insert_derived_mesh(&mut self, name: impl Into<String>, mesh: Arc<MeshData>) {
        let name = name.into();
        self.meshes.remove(&name);
        self.derived.insert(name, mesh);
    }

    /// True when the `derived:` mesh `name` was given with
    /// [`Media::insert_derived_mesh`].
    pub fn has_derived_mesh(&self, name: &str) -> bool {
        self.derived.contains_key(name)
    }

    /// Forgets a `derived:` mesh.
    pub fn remove_derived_mesh(&mut self, name: &str) {
        self.derived.remove(name);
        self.meshes.remove(name);
    }

    /// Forgets the `derived:` meshes `keep` refuses.
    pub fn retain_derived_meshes(&mut self, mut keep: impl FnMut(&str) -> bool) {
        let gone: Vec<String> = self
            .derived
            .keys()
            .filter(|name| !keep(name))
            .cloned()
            .collect();
        for name in gone {
            self.remove_derived_mesh(&name);
        }
    }

    /// The mesh named by a `MeshRef.asset`.
    ///
    /// # Errors
    ///
    /// `RE0520` for an unknown built-in or derived name, and the asset
    /// server's errors for files.
    pub fn mesh(&mut self, asset: &str) -> Result<LoadedMesh, Diagnostic> {
        if let Some(m) = self.meshes.get(asset) {
            return Ok(m.clone());
        }
        if let Some(d) = self.failed.get(asset) {
            return Err(d.clone());
        }
        let loaded = self.load_mesh(asset);
        match loaded {
            Ok(loaded) => {
                self.meshes.insert(asset.to_string(), loaded.clone());
                Ok(loaded)
            }
            Err(d) => {
                // Derived meshes appear when generated; only files are remembered.
                if !asset.starts_with(DERIVED) {
                    self.failed.insert(asset.to_string(), d.clone());
                }
                Err(d)
            }
        }
    }

    fn load_mesh(&mut self, asset: &str) -> Result<LoadedMesh, Diagnostic> {
        Ok(if let Some(name) = asset.strip_prefix(BUILTIN) {
            LoadedMesh {
                mesh: Arc::new(builtin_mesh(name).ok_or_else(|| {
                    unknown_asset(asset, "no such built-in mesh")
                        .with_hint("built-in meshes are builtin:cube, builtin:sphere, builtin:plane and builtin:capsule")
                })?),
                material: None,
            }
        } else if asset.starts_with(DERIVED) {
            LoadedMesh {
                mesh: self.derived.get(asset).cloned().ok_or_else(|| {
                    unknown_asset(asset, "no procedural source has generated this mesh")
                })?,
                material: None,
            }
        } else {
            self.load_gltf_mesh(asset)?
        })
    }

    fn load_gltf_mesh(&mut self, asset: &str) -> Result<LoadedMesh, Diagnostic> {
        let h = self.server.load_gltf(asset)?;
        let gltf = self.server.gltf(h).expect("just loaded");
        let mut out = MeshData::default();
        let mut placed = HashSet::new();
        // A malformed file can make a node its own ancestor; visit each once.
        let mut visited = HashSet::new();
        let mut stack: Vec<(usize, Affine3A)> = gltf
            .roots
            .iter()
            .map(|&r| (r, Affine3A::IDENTITY))
            .collect();
        while let Some((index, parent)) = stack.pop() {
            if !visited.insert(index) {
                continue;
            }
            let Some(node) = gltf.nodes.get(index) else {
                continue;
            };
            let world = parent
                * Affine3A::from_scale_rotation_translation(
                    node.scale,
                    node.rotation,
                    node.translation,
                );
            for &m in &node.meshes {
                if let Some(mesh) = gltf.meshes.get(m) {
                    append(&mut out, &mesh.mesh, &world);
                    placed.insert(m);
                }
            }
            stack.extend(node.children.iter().rev().map(|&c| (c, world)));
        }
        // A file without a scene tree: every mesh at the origin.
        if placed.is_empty() {
            for mesh in &gltf.meshes {
                append(&mut out, &mesh.mesh, &Affine3A::IDENTITY);
            }
        }
        let material_index = gltf.meshes.first().and_then(|m| m.material);
        let material = match material_index.and_then(|i| gltf.materials.get(i)) {
            Some(m) => {
                let desc = m.desc.clone();
                let textures = self.textures_of(&desc)?;
                Some(LoadedMaterial { desc, textures })
            }
            None => None,
        };
        Ok(LoadedMesh {
            mesh: Arc::new(out),
            material,
        })
    }

    fn textures_of(
        &mut self,
        desc: &MaterialDesc,
    ) -> Result<Vec<(usize, String, TextureData)>, Diagnostic> {
        let mut out = Vec::new();
        for (slot, t) in desc.textures().into_iter().enumerate() {
            if let Some(re_types::TextureRef::Asset(id)) = t {
                out.push((slot, id.clone(), self.server.texture_by_id(id)?.clone()));
            }
        }
        Ok(out)
    }

    /// The material named by a `MaterialRef.asset`.
    ///
    /// # Errors
    ///
    /// The asset server's errors.
    pub fn material(&mut self, asset: &str) -> Result<LoadedMaterial, Diagnostic> {
        if let Some(m) = self.materials.get(asset) {
            return Ok(m.clone());
        }
        if let Some(d) = self.failed.get(asset) {
            return Err(d.clone());
        }
        let loaded = self.server.load_material(asset).and_then(|h| {
            let desc = self.server.material(h).cloned().unwrap_or_default();
            let textures = self.server.material_textures(h)?;
            Ok(LoadedMaterial { desc, textures })
        });
        match &loaded {
            Ok(l) => {
                self.materials.insert(asset.to_string(), l.clone());
            }
            Err(d) => {
                self.failed.insert(asset.to_string(), d.clone());
            }
        }
        loaded
    }

    /// The clip named by an `AnimPlayer.clip` (`<glTF path>#<clip name>`).
    ///
    /// # Errors
    ///
    /// The asset server's errors.
    pub fn clip(&mut self, reference: &str) -> Result<Arc<ClipData>, Diagnostic> {
        if let Some(c) = self.clips.get(reference) {
            return Ok(c.clone());
        }
        if let Some(d) = self.failed.get(reference) {
            return Err(d.clone());
        }
        match self.server.clip(reference) {
            Ok(c) => {
                let clip = Arc::new(c.clone());
                self.clips.insert(reference.to_string(), clip.clone());
                Ok(clip)
            }
            Err(d) => {
                self.failed.insert(reference.to_string(), d.clone());
                Err(d)
            }
        }
    }

    /// True the first time `key` is passed, so each broken asset is reported once.
    pub fn first_report(&mut self, key: &str) -> bool {
        self.reported.insert(key.to_string())
    }

    /// Applies file changes from an `AssetWatcher`: re-imports what changed and
    /// forgets cached meshes, materials and clips built from changed files.
    /// Returns the server's report and the asset strings to re-upload.
    pub fn apply_changes(&mut self, changes: &[AssetChange]) -> (ReloadReport, Vec<String>) {
        let report = self.server.apply_changes(changes);
        let mut changed: HashSet<String> = changes.iter().map(|c| c.path().to_string()).collect();
        for asset in &report.reloaded {
            if let Some(info) = self.server.info(*asset) {
                changed.insert(info.path.clone());
            }
        }
        let file_of = |asset: &str| asset.split('#').next().unwrap_or(asset).to_string();
        // A changed texture can belong to any material, so materials (and glTF
        // meshes, which carry theirs) are rebuilt when any texture changed.
        let textures_changed = !report.changed_ids.is_empty();
        let mut stale: Vec<String> = self
            .meshes
            .keys()
            .filter(|k| {
                changed.contains(&file_of(k)) || textures_changed && !k.starts_with(BUILTIN)
            })
            .chain(
                self.materials
                    .keys()
                    .filter(|k| textures_changed || changed.contains(*k)),
            )
            .chain(self.clips.keys().filter(|k| changed.contains(&file_of(k))))
            .cloned()
            .collect();
        stale.retain(|k| !k.starts_with(DERIVED));
        // Failed loads get another try (and another report) after any change.
        stale.extend(self.failed.drain().map(|(k, _)| k));
        stale.sort();
        stale.dedup();
        for key in &stale {
            self.meshes.remove(key);
            self.materials.remove(key);
            self.clips.remove(key);
            self.reported.remove(key);
        }
        (report, stale)
    }
}

fn unknown_asset(asset: &str, why: &str) -> Diagnostic {
    Diagnostic::error(codes::ASSET_UNAVAILABLE, format!("`{asset}`: {why}"))
        .at(Location::file(asset))
}

fn append(out: &mut MeshData, mesh: &MeshData, world: &Affine3A) {
    let base = out.vertices.len() as u32;
    let normal_m = glam::Mat3::from(world.matrix3).inverse().transpose();
    let flip = world.matrix3.determinant() < 0.0;
    out.vertices.extend(mesh.vertices.iter().map(|v| {
        Vertex {
            pos: world.transform_point3(Vec3::from(v.pos)).to_array(),
            normal: (normal_m * Vec3::from(v.normal))
                .normalize_or_zero()
                .to_array(),
            ..*v
        }
    }));
    for t in mesh.indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| i + base);
        if flip {
            out.indices.extend_from_slice(&[a, c, b]);
        } else {
            out.indices.extend_from_slice(&[a, b, c]);
        }
    }
}

/// A built-in mesh by name: `cube` (1 m, centred), `sphere` (radius 0.5),
/// `plane` (1 m × 1 m in XZ, facing +Y) and `capsule` (radius 0.5, 2 m tall).
pub fn builtin_mesh(name: &str) -> Option<MeshData> {
    match name {
        "cube" => Some(cube()),
        "sphere" => Some(capsule(0.5, 0.0, 24, 16)),
        "plane" => {
            let mut m = MeshData::default();
            quad(&mut m, Vec3::ZERO, Vec3::X * 0.5, Vec3::NEG_Z * 0.5);
            Some(m)
        }
        "capsule" => Some(capsule(0.5, 1.0, 24, 16)),
        _ => None,
    }
}

fn vertex(pos: Vec3, normal: Vec3, uv: [f32; 2]) -> Vertex {
    Vertex {
        pos: pos.to_array(),
        normal: normal.to_array(),
        uv,
        color: [1.0; 4],
    }
}

fn quad(mesh: &mut MeshData, c: Vec3, u: Vec3, v: Vec3) {
    let n = u.cross(v).normalize();
    let base = mesh.vertices.len() as u32;
    for (p, uv) in [
        (c - u - v, [0.0, 1.0]),
        (c + u - v, [1.0, 1.0]),
        (c + u + v, [1.0, 0.0]),
        (c - u + v, [0.0, 0.0]),
    ] {
        mesh.vertices.push(vertex(p, n, uv));
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn cube() -> MeshData {
    let mut mesh = MeshData::default();
    for (n, u, v) in [
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
    ] {
        quad(&mut mesh, n * 0.5, u * 0.5, v * 0.5);
    }
    mesh
}

/// A capsule along Y: hemispheres of `radius` joined by a cylinder of
/// `cylinder` length (0 gives a sphere).
fn capsule(radius: f32, cylinder: f32, segments: u32, rings: u32) -> MeshData {
    let mut mesh = MeshData::default();
    let half = cylinder * 0.5;
    // Rings from the top pole to the bottom pole; the equator ring is doubled
    // so the cylinder part has its own vertices.
    let mut rows: Vec<(f32, f32, f32)> = Vec::new(); // (y offset, polar angle, v)
    for r in 0..=rings {
        let phi = PI * r as f32 / rings as f32;
        let offset = if phi <= PI / 2.0 { half } else { -half };
        rows.push((offset, phi, r as f32 / rings as f32));
        if cylinder > 0.0 && r == rings / 2 && rings.is_multiple_of(2) {
            rows.push((-half, phi, r as f32 / rings as f32));
        }
    }
    for &(offset, phi, v) in &rows {
        for s in 0..=segments {
            let theta = TAU * s as f32 / segments as f32;
            let n = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            let p = n * radius + Vec3::Y * offset;
            mesh.vertices
                .push(vertex(p, n, [s as f32 / segments as f32, v]));
        }
    }
    let stride = segments + 1;
    for row in 0..rows.len() as u32 - 1 {
        for s in 0..segments {
            let a = row * stride + s;
            let b = a + stride;
            mesh.indices
                .extend_from_slice(&[a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_well_formed_and_face_outwards() {
        for name in ["cube", "sphere", "plane", "capsule"] {
            let m = builtin_mesh(name).unwrap();
            assert!(m.is_well_formed(), "{name}");
            for t in m.indices.as_chunks::<3>().0 {
                let [a, b, c] = t.map(|i| Vec3::from(m.vertices[i as usize].pos));
                let face = (b - a).cross(c - a);
                if face.length() < 1e-6 {
                    continue;
                }
                let n = Vec3::from(m.vertices[t[0] as usize].normal);
                assert!(face.dot(n) > 0.0, "{name}: triangle {t:?} faces inwards");
            }
        }
        let aabb = builtin_mesh("capsule").unwrap().aabb().unwrap();
        assert!((aabb.half_extents() - Vec3::new(0.5, 1.0, 0.5)).length() < 1e-3);
    }

    #[test]
    fn unknown_names_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut media = Media::new(dir.path());
        assert_eq!(
            media.mesh("builtin:teapot").unwrap_err().code,
            codes::ASSET_UNAVAILABLE
        );
        assert_eq!(
            media.mesh("derived:00").unwrap_err().code,
            codes::ASSET_UNAVAILABLE
        );
        media.insert_derived_mesh("derived:00", Arc::new(cube()));
        assert_eq!(media.mesh("derived:00").unwrap().mesh.triangle_count(), 12);
    }
}
