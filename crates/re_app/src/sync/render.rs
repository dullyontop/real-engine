//! Render sync (ADR-0006): `GlobalTransform`, `MeshRef`, `MaterialRef`,
//! `Visible`, `Camera` and `Light` become a [`RenderSnapshot`].
//!
//! Mesh and material asset strings are resolved to renderer handles through
//! [`RenderAssets`]; an entity whose mesh is not uploaded (yet) is not drawn.
//! Moving bodies and characters are drawn at their interpolated pose between
//! the last two physics steps.

use std::collections::HashMap;

use glam::{Affine3A, Mat4, Vec3};
use re_scene::ecs::entity::Entity;
use re_scene::{
    Camera as CameraC, GlobalTransform, Light as LightC, LightKind as LightKindC, MaterialRef,
    MeshRef, Visible, World, entity_id,
};
use re_types::{
    Camera, DrawFlags, DrawItem, Light, LightKind, MaterialHandle, MeshHandle, RenderSnapshot,
};

use super::physics::PhysicsSync;

/// Renderer handles for the asset strings scenes use (`MeshRef.asset`,
/// `MaterialRef.asset`), filled as meshes and materials are uploaded.
#[derive(Clone, Debug, Default)]
pub struct RenderAssets {
    meshes: HashMap<String, MeshHandle>,
    materials: HashMap<String, MaterialHandle>,
    /// The material for entities without a `MaterialRef` (or whose material is
    /// not loaded).
    pub default_material: Option<MaterialHandle>,
}

impl RenderAssets {
    /// Records the handle of an uploaded mesh.
    pub fn insert_mesh(&mut self, asset: impl Into<String>, handle: MeshHandle) {
        self.meshes.insert(asset.into(), handle);
    }

    /// Records the handle of an uploaded material.
    pub fn insert_material(&mut self, asset: impl Into<String>, handle: MaterialHandle) {
        self.materials.insert(asset.into(), handle);
    }

    /// Forgets a mesh, returning its handle (to release it).
    pub fn remove_mesh(&mut self, asset: &str) -> Option<MeshHandle> {
        self.meshes.remove(asset)
    }

    /// Forgets a material, returning its handle (to release it).
    pub fn remove_material(&mut self, asset: &str) -> Option<MaterialHandle> {
        self.materials.remove(asset)
    }

    /// The handle of a mesh asset.
    pub fn mesh(&self, asset: &str) -> Option<MeshHandle> {
        self.meshes.get(asset).copied()
    }

    /// The handle of a material asset.
    pub fn material(&self, asset: &str) -> Option<MaterialHandle> {
        self.materials.get(asset).copied()
    }

    /// Every mesh asset with a handle.
    pub fn mesh_assets(&self) -> impl Iterator<Item = &str> {
        self.meshes.keys().map(String::as_str)
    }

    /// Every material asset with a handle.
    pub fn material_assets(&self) -> impl Iterator<Item = &str> {
        self.materials.keys().map(String::as_str)
    }

    /// The number of meshes with a handle.
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }
}

/// What [`build`] needs besides the world.
pub struct RenderInputs<'a> {
    /// Asset handles.
    pub assets: &'a RenderAssets,
    /// Physics poses, for interpolation.
    pub physics: &'a PhysicsSync,
    /// Interpolation factor between the last two physics steps (0 to 1).
    pub alpha: f32,
    /// The clear colour.
    pub clear: [f32; 4],
}

/// Mesh and material assets referenced by the world's drawable entities, so the
/// app can upload what is missing.
pub fn referenced_assets(world: &mut World) -> (Vec<String>, Vec<String>) {
    let mut meshes: Vec<String> = world
        .ecs
        .query::<&MeshRef>()
        .iter(&world.ecs)
        .map(|m| m.asset.clone())
        .filter(|a| !a.is_empty())
        .collect();
    meshes.sort();
    meshes.dedup();
    let mut materials: Vec<String> = world
        .ecs
        .query::<&MaterialRef>()
        .iter(&world.ecs)
        .map(|m| m.asset.clone())
        .filter(|a| !a.is_empty())
        .collect();
    materials.sort();
    materials.dedup();
    (meshes, materials)
}

fn global(world: &World, inputs: &RenderInputs<'_>, entity: Entity) -> Affine3A {
    let id = entity_id(entity);
    if let Some((pos, rot)) = inputs.physics.interpolated(id, inputs.alpha) {
        let scale = world
            .ecs
            .get::<GlobalTransform>(entity)
            .map_or(Vec3::ONE, |g| g.0.to_scale_rotation_translation().0);
        return Affine3A::from_scale_rotation_translation(scale, rot, pos);
    }
    world
        .ecs
        .get::<GlobalTransform>(entity)
        .map_or(Affine3A::IDENTITY, |g| g.0)
}

/// A camera-to-world matrix at `eye` looking at `target` with +Y up.
fn looking_at(eye: Vec3, target: Vec3) -> Mat4 {
    let f = (target - eye).normalize();
    let r = f.cross(Vec3::Y).normalize();
    let u = r.cross(f);
    Mat4::from_cols(
        r.extend(0.0),
        u.extend(0.0),
        (-f).extend(0.0),
        eye.extend(1.0),
    )
}

/// Builds the frame's [`RenderSnapshot`] from the world (after `propagate`).
///
/// - Items: every entity with a `MeshRef` whose mesh has a handle, unless
///   `Visible.visible` is false; sorted by entity for a stable order.
/// - Camera: the first active `Camera` in entity order; without one, a camera
///   at (0, 10, 20) looking at the origin.
/// - Lights: every `Light`, in entity order. A directional light shines along
///   its entity's −Z axis.
pub fn build(world: &mut World, inputs: &RenderInputs<'_>) -> RenderSnapshot {
    let mut items = Vec::new();
    let mut drawn: Vec<(Entity, String, Option<String>)> = world
        .ecs
        .query::<(Entity, &MeshRef, Option<&MaterialRef>, Option<&Visible>)>()
        .iter(&world.ecs)
        .filter(|(_, m, _, v)| !m.asset.is_empty() && v.is_none_or(|v| v.visible))
        .map(|(e, m, mat, _)| (e, m.asset.clone(), mat.map(|m| m.asset.clone())))
        .collect();
    drawn.sort_by_key(|(e, _, _)| e.to_bits());
    for (entity, mesh, material) in drawn {
        let Some(mesh) = inputs.assets.mesh(&mesh) else {
            continue;
        };
        let material = material
            .and_then(|m| inputs.assets.material(&m))
            .or(inputs.assets.default_material);
        let Some(material) = material else {
            continue;
        };
        items.push(DrawItem {
            entity: entity_id(entity),
            mesh,
            material,
            world: Mat4::from(global(world, inputs, entity)),
            flags: DrawFlags::CAST_SHADOWS | DrawFlags::RECEIVE_SHADOWS,
        });
    }

    let mut cams: Vec<(Entity, CameraC)> = world
        .ecs
        .query::<(Entity, &CameraC)>()
        .iter(&world.ecs)
        .filter(|(_, c)| c.active)
        .map(|(e, c)| (e, c.clone()))
        .collect();
    cams.sort_by_key(|(e, _)| e.to_bits());
    let camera = match cams.first() {
        Some((e, c)) => {
            let g = global(world, inputs, *e);
            // A camera has no scale: keep rotation and translation only.
            let (_, r, t) = g.to_scale_rotation_translation();
            Camera {
                world: Mat4::from_rotation_translation(r, t),
                fov_y_radians: c.fov_y_deg.to_radians(),
                near: c.near,
                far: c.far,
            }
        }
        None => Camera {
            world: looking_at(Vec3::new(0.0, 10.0, 20.0), Vec3::ZERO),
            ..Camera::default()
        },
    };

    let mut lights: Vec<(Entity, LightC)> = world
        .ecs
        .query::<(Entity, &LightC)>()
        .iter(&world.ecs)
        .map(|(e, l)| (e, l.clone()))
        .collect();
    lights.sort_by_key(|(e, _)| e.to_bits());
    let lights = lights
        .into_iter()
        .map(|(e, l)| {
            let g = global(world, inputs, e);
            let direction = g.transform_vector3(Vec3::NEG_Z).normalize_or(Vec3::NEG_Y);
            Light {
                kind: match l.kind {
                    LightKindC::Directional => LightKind::Directional,
                    LightKindC::Point => LightKind::Point,
                },
                position: g.translation.into(),
                direction,
                color: [l.color.r, l.color.g, l.color.b],
                intensity: l.intensity,
                range: l.range,
                shadows: l.shadows,
            }
        })
        .collect();

    RenderSnapshot {
        camera,
        lights,
        items,
        debug_lines: Vec::new(),
        clear: inputs.clear,
    }
}
