//! Physics sync (ADR-0006): `RigidBody`, `Collider` and `CharacterController`
//! become [`PhysicsCmd`]s ([`push`]); poses come back into `Transform` ([`pull`]).
//!
//! The mapping follows the notes under WP0's built-in component table
//! (docs/M1_PLAN.md):
//! - a `Collider` without a `RigidBody` is a static body;
//! - `Collider.enabled = false` removes the body, and `true` adds it again;
//! - `layer` and `groups` are range-checked into `Layer(u16)` and `GroupMask(u32)`
//!   (`RE0505`), never truncated;
//! - `Convex`, `Mesh` and `Heightfield` shapes read their data from the
//!   [`ShapeLibrary`] by the collider's `source` (`RE0506` when missing);
//! - a `CharacterController` wins over a `Collider` on the same entity.
//!
//! Shapes are in the entity's local space and scaled by its world scale. A body's
//! pose is the entity's world pose; a character's pose is its capsule's centre.
//! Any change to a body's description replaces the body (remove, then add at the
//! current pose); a `Transform` written by anything other than [`pull`] (a
//! script, an edit) teleports the body, or moves a kinematic body's target.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use glam::{Affine3A, Quat, Vec3};
use re_core::{Diagnostic, Location};
use re_scene::ecs::query::{Or, With};
use re_scene::{
    BodyKind as SceneBodyKind, CharacterController, Collider, ColliderShape, Entity,
    GlobalTransform, RigidBody, Transform, World, entity_id, hierarchy,
};
use re_types::{
    BodyDesc, BodyKind, BodyPose, CharacterDesc, EntityId, GroupMask, Layer, MeshData, PhysicsCmd,
    ShapeDesc,
};

use crate::codes;

/// A heightfield shape's data: what a heightfield `source` asset carries.
#[derive(Clone, Debug, PartialEq)]
pub struct HeightfieldData {
    /// Samples per side.
    pub n: u32,
    /// Distance between samples, in metres.
    pub spacing: f32,
    /// Position of sample (0, 0) in the entity's local space.
    pub offset: Vec3,
    /// `n × n` heights, `samples[z * n + x]`.
    pub samples: Vec<f32>,
}

/// The meshes and heightfields collider `source`s can name, by asset path
/// (`assets/rock.glb`) or derived key (`derived:<hash>`).
#[derive(Clone, Debug, Default)]
pub struct ShapeLibrary {
    meshes: HashMap<String, Arc<MeshData>>,
    heightfields: HashMap<String, Arc<HeightfieldData>>,
    generation: u64,
}

impl ShapeLibrary {
    /// Adds or replaces a mesh.
    pub fn insert_mesh(&mut self, source: impl Into<String>, mesh: Arc<MeshData>) {
        self.meshes.insert(source.into(), mesh);
        self.generation += 1;
    }

    /// Adds or replaces a heightfield.
    pub fn insert_heightfield(&mut self, source: impl Into<String>, data: Arc<HeightfieldData>) {
        self.heightfields.insert(source.into(), data);
        self.generation += 1;
    }

    /// Forgets a source.
    pub fn remove(&mut self, source: &str) {
        if self.meshes.remove(source).is_some() | self.heightfields.remove(source).is_some() {
            self.generation += 1;
        }
    }

    /// The mesh named `source`.
    pub fn mesh(&self, source: &str) -> Option<&Arc<MeshData>> {
        self.meshes.get(source)
    }

    /// The heightfield named `source`.
    pub fn heightfield(&self, source: &str) -> Option<&Arc<HeightfieldData>> {
        self.heightfields.get(source)
    }

    /// The number of meshes plus heightfields.
    pub fn len(&self) -> usize {
        self.meshes.len() + self.heightfields.len()
    }

    /// True when the library is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What one entity is in the physics world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// A body of this kind.
    Body(BodyKind),
    /// A character.
    Character,
}

#[derive(Clone, Debug)]
struct Link {
    entity: Entity,
    kind: LinkKind,
    fingerprint: blake3::Hash,
    /// The local translation and rotation last written by [`pull`] or seen by
    /// [`push`]; a different value means someone else moved the entity.
    written: (Vec3, Quat),
    /// World pose before and after the last step, for interpolation.
    prev: (Vec3, Quat),
    cur: (Vec3, Quat),
}

/// The physics side of the World: which entity has which body, and their poses.
#[derive(Clone, Debug, Default)]
pub struct PhysicsSync {
    links: BTreeMap<EntityId, Link>,
    /// (entity, fingerprint) pairs already reported as failing, so a broken
    /// collider is reported once, not every step.
    reported: BTreeSet<(EntityId, [u8; 32])>,
    shapes_generation: u64,
}

impl PhysicsSync {
    /// An empty sync state (no bodies yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// What `id` is in the physics world, if anything.
    pub fn kind(&self, id: EntityId) -> Option<LinkKind> {
        self.links.get(&id).map(|l| l.kind)
    }

    /// The number of linked bodies and characters.
    pub fn len(&self) -> usize {
        self.links.len()
    }

    /// True when nothing is linked.
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Every linked entity, in id order, with its kind.
    pub fn linked(&self) -> impl Iterator<Item = (EntityId, LinkKind)> + '_ {
        self.links.iter().map(|(id, l)| (*id, l.kind))
    }

    /// The interpolated world pose of a moving body or character: `alpha` 0 is the
    /// pose before the last step, 1 the pose after it. `None` for static bodies
    /// and entities without physics.
    pub fn interpolated(&self, id: EntityId, alpha: f32) -> Option<(Vec3, Quat)> {
        let link = self.links.get(&id)?;
        if link.kind == LinkKind::Body(BodyKind::Static) {
            return None;
        }
        let a = alpha.clamp(0.0, 1.0);
        Some((
            link.prev.0.lerp(link.cur.0, a),
            link.prev.1.slerp(link.cur.1, a),
        ))
    }

    /// Starts a step: the pose after the last step becomes the pose before this one.
    pub fn begin_step(&mut self) {
        for link in self.links.values_mut() {
            link.prev = link.cur;
        }
    }
}

/// The world pose of `entity`: its parent's global transform times its local
/// `Transform` (read now, not from the last propagation, so a script's write
/// in this frame counts).
fn world_affine(world: &World, entity: Entity) -> Affine3A {
    let local = world
        .ecs
        .get::<Transform>(entity)
        .map_or(Affine3A::IDENTITY, Transform::affine);
    parent_affine(world, entity) * local
}

fn parent_affine(world: &World, entity: Entity) -> Affine3A {
    hierarchy::parent(&world.ecs, entity)
        .and_then(|p| world.ecs.get::<GlobalTransform>(p))
        .map_or(Affine3A::IDENTITY, |g| g.0)
}

fn decompose(a: &Affine3A) -> (Vec3, Quat, Vec3) {
    let (s, r, t) = a.to_scale_rotation_translation();
    (t, r.normalize(), s)
}

fn layer_of(value: i32, id: &str, component: &str) -> Result<Layer, Diagnostic> {
    u16::try_from(value).map(Layer).map_err(|_| {
        Diagnostic::error(
            codes::PHYSICS_FIELD_OUT_OF_RANGE,
            format!("{component}.layer = {value} is out of range (0 to 65535)"),
        )
        .at(Location::entity(id))
    })
}

fn groups_of(value: i64, id: &str, component: &str) -> Result<GroupMask, Diagnostic> {
    u32::try_from(value).map(GroupMask).map_err(|_| {
        Diagnostic::error(
            codes::PHYSICS_FIELD_OUT_OF_RANGE,
            format!("{component}.groups = {value} is out of range (0 to 4294967295)"),
        )
        .at(Location::entity(id))
        .with_hint("groups is a bit mask: bit g is group g, and the default 1 is group 0")
    })
}

fn missing_source(id: &str, source: &str, what: &str) -> Diagnostic {
    let msg = if source.is_empty() {
        format!("this {what} collider has no `source`")
    } else {
        format!("the {what} {source:?} named by this collider's `source` is not loaded")
    };
    Diagnostic::error(codes::SHAPE_SOURCE_MISSING, msg)
        .at(Location::entity(id))
        .with_hint(
            "set `Collider.source` to a mesh asset (`assets/x.glb`) or a derived key \
             produced by a procedural node (`derived:<hash>`)",
        )
}

fn shape_of(
    c: &Collider,
    scale: Vec3,
    shapes: &ShapeLibrary,
    id: &str,
) -> Result<ShapeDesc, Diagnostic> {
    let s = scale.abs();
    Ok(match c.shape {
        ColliderShape::Box => ShapeDesc::Box {
            half_extents: c.half_extents * s,
        },
        ColliderShape::Sphere => ShapeDesc::Sphere {
            radius: c.radius * s.max_element(),
        },
        ColliderShape::Capsule => ShapeDesc::Capsule {
            half_height: c.half_height * s.y,
            radius: c.radius * s.x.max(s.z),
        },
        ColliderShape::Convex => {
            let mesh = shapes
                .mesh(&c.source)
                .ok_or_else(|| missing_source(id, &c.source, "convex"))?;
            ShapeDesc::ConvexHull {
                points: mesh
                    .vertices
                    .iter()
                    .map(|v| Vec3::from(v.pos) * s)
                    .collect(),
            }
        }
        ColliderShape::Mesh => {
            let mesh = shapes
                .mesh(&c.source)
                .ok_or_else(|| missing_source(id, &c.source, "mesh"))?;
            ShapeDesc::TriangleMesh {
                vertices: mesh
                    .vertices
                    .iter()
                    .map(|v| Vec3::from(v.pos) * s)
                    .collect(),
                triangles: mesh.indices.as_chunks::<3>().0.to_vec(),
            }
        }
        ColliderShape::Heightfield => {
            let h = shapes
                .heightfield(&c.source)
                .ok_or_else(|| missing_source(id, &c.source, "heightfield"))?;
            // Jolt heightfields have one horizontal spacing; a non-uniform x/z
            // scale uses x.
            ShapeDesc::Heightfield {
                n: h.n,
                samples: h.samples.iter().map(|v| v * s.y).collect(),
                spacing: h.spacing * s.x,
                offset: h.offset * s,
            }
        }
    })
}

fn body_kind(rb: Option<&RigidBody>) -> BodyKind {
    match rb.map(|r| r.kind) {
        None | Some(SceneBodyKind::Static) => BodyKind::Static,
        Some(SceneBodyKind::Dynamic) => BodyKind::Dynamic,
        Some(SceneBodyKind::Kinematic) => BodyKind::Kinematic,
    }
}

enum Wanted {
    Body(BodyDesc),
    Character(CharacterDesc),
}

impl Wanted {
    fn kind(&self) -> LinkKind {
        match self {
            Wanted::Body(d) => LinkKind::Body(d.kind),
            Wanted::Character(_) => LinkKind::Character,
        }
    }
}

/// The fingerprint of a description, ignoring its pose.
fn fingerprint(w: &Wanted) -> blake3::Hash {
    let text = match w {
        Wanted::Body(d) => {
            let mut d = d.clone();
            d.pos = Vec3::ZERO;
            d.rot = Quat::IDENTITY;
            serde_json::to_string(&d)
        }
        Wanted::Character(d) => serde_json::to_string(d),
    };
    blake3::hash(text.unwrap_or_default().as_bytes())
}

fn wanted(
    world: &World,
    entity: Entity,
    shapes: &ShapeLibrary,
    pose: (Vec3, Quat, Vec3),
) -> Result<Option<Wanted>, Diagnostic> {
    let id = world.id_of(entity).unwrap_or("?");
    let (pos, rot, scale) = pose;
    if let Some(cc) = world.ecs.get::<CharacterController>(entity) {
        return Ok(Some(Wanted::Character(CharacterDesc {
            height: cc.height,
            radius: cc.radius,
            max_slope_deg: cc.max_slope_deg,
            step_height: cc.step_height,
            mass: cc.mass,
            inner_body: cc.inner_body,
            layer: layer_of(cc.layer, id, "CharacterController")?,
            groups: groups_of(cc.groups, id, "CharacterController")?,
        })));
    }
    let Some(c) = world.ecs.get::<Collider>(entity) else {
        return Ok(None);
    };
    if !c.enabled {
        return Ok(None);
    }
    let rb = world.ecs.get::<RigidBody>(entity);
    let defaults = RigidBody::default();
    let r = rb.unwrap_or(&defaults);
    Ok(Some(Wanted::Body(BodyDesc {
        kind: body_kind(rb),
        shape: shape_of(c, scale, shapes, id)?,
        pos,
        rot,
        mass: r.mass,
        friction: r.friction,
        restitution: r.restitution,
        linear_damping: r.linear_damping,
        angular_damping: r.angular_damping,
        layer: layer_of(c.layer, id, "Collider")?,
        groups: groups_of(c.groups, id, "Collider")?,
        sensor: c.sensor,
    })))
}

fn add_cmd(id: EntityId, w: Wanted, pos: Vec3, rot: Quat) -> PhysicsCmd {
    match w {
        Wanted::Body(desc) => PhysicsCmd::AddBody { id, desc },
        Wanted::Character(desc) => PhysicsCmd::AddCharacter { id, desc, pos, rot },
    }
}

fn remove_cmd(id: EntityId, kind: LinkKind) -> PhysicsCmd {
    match kind {
        LinkKind::Body(_) => PhysicsCmd::RemoveBody { id },
        LinkKind::Character => PhysicsCmd::RemoveCharacter { id },
    }
}

fn local_tr(world: &World, entity: Entity) -> (Vec3, Quat) {
    world
        .ecs
        .get::<Transform>(entity)
        .map_or((Vec3::ZERO, Quat::IDENTITY), |t| {
            (t.translation, t.rotation)
        })
}

/// Turns World changes since the last call into physics commands, appended to
/// `cmds` (unsorted; `Physics::apply` sorts by kind and entity).
///
/// Adds bodies and characters for new entities, replaces the ones whose
/// description changed, removes the ones whose entity or component is gone or
/// whose collider is disabled, and teleports (or, for kinematic bodies,
/// retargets) entities whose `Transform` was written by someone other than
/// [`pull`]. Problems become diagnostics in `diagnostics`, each reported once;
/// the entity then gets no body until it is fixed.
pub fn push(
    world: &mut World,
    sync: &mut PhysicsSync,
    shapes: &ShapeLibrary,
    cmds: &mut Vec<PhysicsCmd>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // A shape library change may fix a missing source: report again if still broken.
    if sync.shapes_generation != shapes.generation {
        sync.shapes_generation = shapes.generation;
        sync.reported.clear();
    }
    let mut candidates: Vec<Entity> = world
        .ecs
        .query_filtered::<Entity, Or<(With<CharacterController>, With<Collider>)>>()
        .iter(&world.ecs)
        .collect();
    candidates.sort_by_key(|e| e.to_bits());
    let world = &*world;

    let mut seen: BTreeSet<EntityId> = BTreeSet::new();
    for entity in candidates {
        let id = entity_id(entity);
        let world_pose = decompose(&world_affine(world, entity));
        let wanted = match wanted(world, entity, shapes, world_pose) {
            Ok(w) => w,
            Err(d) => {
                let key = (id, *blake3::hash(d.message.as_bytes()).as_bytes());
                if sync.reported.insert(key) {
                    diagnostics.push(d);
                }
                // A broken description removes an existing body rather than
                // leaving a stale one.
                None
            }
        };
        let Some(w) = wanted else {
            continue;
        };
        seen.insert(id);
        let (pos, rot, _) = world_pose;
        let fp = fingerprint(&w);
        let local = local_tr(world, entity);
        match sync.links.get_mut(&id) {
            Some(link) if link.fingerprint == fp && link.entity == entity => {
                if link.written != local {
                    link.written = local;
                    link.cur = (pos, rot);
                    cmds.push(match link.kind {
                        LinkKind::Body(BodyKind::Kinematic) => {
                            PhysicsCmd::SetKinematicTarget { id, pos, rot }
                        }
                        _ => PhysicsCmd::SetTransform { id, pos, rot },
                    });
                }
            }
            existing => {
                if let Some(old) = existing {
                    cmds.push(remove_cmd(id, old.kind));
                }
                let kind = w.kind();
                cmds.push(add_cmd(id, w, pos, rot));
                sync.links.insert(
                    id,
                    Link {
                        entity,
                        kind,
                        fingerprint: fp,
                        written: local,
                        prev: (pos, rot),
                        cur: (pos, rot),
                    },
                );
            }
        }
    }
    let gone: Vec<EntityId> = sync
        .links
        .keys()
        .filter(|id| !seen.contains(id))
        .copied()
        .collect();
    for id in gone {
        if let Some(link) = sync.links.remove(&id) {
            cmds.push(remove_cmd(id, link.kind));
        }
    }
}

/// Writes poses from physics back into `Transform`s (as local transforms under
/// each entity's parent; scale is kept) and records them for interpolation.
/// Poses of entities that are no longer linked are ignored.
pub fn pull(world: &mut World, sync: &mut PhysicsSync, poses: &[BodyPose]) {
    for pose in poses {
        let Some(link) = sync.links.get_mut(&pose.id) else {
            continue;
        };
        let entity = link.entity;
        let parent = parent_affine(world, entity);
        let world_pose = Affine3A::from_rotation_translation(pose.rot, pose.pos);
        let local = parent.inverse() * world_pose;
        let (_, rot, t) = local.to_scale_rotation_translation();
        let rot = rot.normalize();
        link.cur = (pose.pos, pose.rot);
        link.written = (t, rot);
        if let Some(mut tr) = world.ecs.get_mut::<Transform>(entity) {
            if tr.translation != t || tr.rotation != rot {
                tr.translation = t;
                tr.rotation = rot;
            }
        } else if let Ok(mut e) = world.ecs.get_entity_mut(entity) {
            e.insert(Transform {
                translation: t,
                rotation: rot,
                ..Default::default()
            });
        }
    }
}
