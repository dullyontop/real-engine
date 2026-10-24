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
//! Shapes are in the entity's local space and scaled by its world scale (the
//! product of its and its ancestors' `Transform` scales). A body's
//! pose is the entity's world pose; a character's pose is its capsule's centre.
//! Any change to a body's description replaces the body (remove, then add at the
//! current pose); a `Transform` written by anything other than [`pull`] (a
//! script, an edit) teleports the body, or moves a kinematic body's target.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use glam::{Affine3A, Quat, Vec3};
use re_core::{Diagnostic, Location};
use re_scene::ecs::change_detection::Tick;
use re_scene::ecs::query::{Or, With};
use re_scene::ecs::world::WorldId;
use re_scene::{
    BodyKind as SceneBodyKind, CharacterController, Collider, ColliderShape, Entity,
    GlobalTransform, RigidBody, Transform, Vehicle, VehicleKind as SceneVehicleKind, VehicleWheel,
    WheelTester as SceneWheelTester, World, entity_from_id, entity_id, hierarchy,
};
use re_types::{
    BodyDesc, BodyKind, BodyPose, CharacterDesc, EngineDesc, EntityId, GroupMask, Layer, MeshData,
    PhysicsCmd, ShapeDesc, TransmissionDesc, VehicleDesc, VehicleKind, WheelDesc, WheelPose,
    WheelTester,
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
    /// How finely the physics shape stores them (`None`: the backend's
    /// defaults).
    pub precision: Option<re_types::physics::HeightfieldPrecision>,
}

/// The meshes and heightfields collider `source`s can name, by asset path
/// (`assets/rock.glb`) or derived key (`derived:<hash>`).
#[derive(Clone, Debug, Default)]
pub struct ShapeLibrary {
    /// Each shape with a hash of what a collider reads from it, so [`push`]
    /// can tell an unchanged collider without copying the shape's data.
    meshes: HashMap<String, (Arc<MeshData>, blake3::Hash)>,
    heightfields: HashMap<String, (Arc<HeightfieldData>, blake3::Hash)>,
    generation: u64,
}

/// A hash of the mesh data a collider reads: positions and indices.
fn mesh_content(mesh: &MeshData) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    h.update(&(mesh.vertices.len() as u64).to_le_bytes());
    for v in &mesh.vertices {
        for c in v.pos {
            h.update(&c.to_le_bytes());
        }
    }
    for i in &mesh.indices {
        h.update(&i.to_le_bytes());
    }
    h.finalize()
}

/// A hash of a heightfield's data.
fn heightfield_content(data: &HeightfieldData) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    h.update(&data.n.to_le_bytes());
    h.update(&data.spacing.to_le_bytes());
    for c in data.offset.to_array() {
        h.update(&c.to_le_bytes());
    }
    for v in &data.samples {
        h.update(&v.to_le_bytes());
    }
    if let Some(p) = data.precision {
        h.update(&[p.bits_per_sample, p.block_size]);
    }
    h.finalize()
}

impl ShapeLibrary {
    /// Adds or replaces a mesh.
    pub fn insert_mesh(&mut self, source: impl Into<String>, mesh: Arc<MeshData>) {
        let content = mesh_content(&mesh);
        self.meshes.insert(source.into(), (mesh, content));
        self.generation += 1;
    }

    /// Adds or replaces a heightfield.
    pub fn insert_heightfield(&mut self, source: impl Into<String>, data: Arc<HeightfieldData>) {
        let content = heightfield_content(&data);
        self.heightfields.insert(source.into(), (data, content));
        self.generation += 1;
    }

    /// Forgets a source.
    pub fn remove(&mut self, source: &str) {
        if self.meshes.remove(source).is_some() | self.heightfields.remove(source).is_some() {
            self.generation += 1;
        }
    }

    /// Forgets the sources `keep` refuses.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        let before = self.len();
        self.meshes.retain(|source, _| keep(source));
        self.heightfields.retain(|source, _| keep(source));
        if self.len() != before {
            self.generation += 1;
        }
    }

    /// The mesh named `source`.
    pub fn mesh(&self, source: &str) -> Option<&Arc<MeshData>> {
        self.meshes.get(source).map(|(m, _)| m)
    }

    /// The heightfield named `source`.
    pub fn heightfield(&self, source: &str) -> Option<&Arc<HeightfieldData>> {
        self.heightfields.get(source).map(|(h, _)| h)
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
    /// The parent's world transform when `written` was recorded; a different
    /// value means the parent moved and the body moves with it.
    parent: Affine3A,
    /// World pose before and after the last step, for interpolation.
    prev: (Vec3, Quat),
    cur: (Vec3, Quat),
    /// The entity had a `RigidBody` (its removal changes no other component).
    rigid_body: bool,
    /// The shape comes from the [`ShapeLibrary`].
    library: bool,
}

/// A vehicle on a linked body.
#[derive(Clone, Debug)]
struct VehicleLink {
    fingerprint: blake3::Hash,
    /// The wheel entities, in wheel order.
    wheels: Vec<Entity>,
}

/// Where a `VehicleWheel` entity is attached. Kept for as long as the entity has
/// the component, whatever happens to its vehicle, because [`pull_wheels`]
/// overwrites the wheel's `Transform` with the simulated pose.
#[derive(Clone, Copy, Debug)]
struct WheelMount {
    /// The attachment point, in the chassis entity's local space.
    attach: Vec3,
    /// The local translation and rotation last written by [`pull_wheels`] or
    /// seen by [`push`]; a different value moves the attachment point.
    written: (Vec3, Quat),
}

/// The physics side of the World: which entity has which body, and their poses.
#[derive(Clone, Debug, Default)]
pub struct PhysicsSync {
    links: BTreeMap<EntityId, Link>,
    vehicles: BTreeMap<EntityId, VehicleLink>,
    mounts: BTreeMap<EntityId, WheelMount>,
    /// (entity, fingerprint) pairs already reported as failing, so a broken
    /// collider is reported once, not every step.
    reported: BTreeSet<(EntityId, [u8; 32])>,
    shapes_generation: u64,
    /// The world and change tick of the last [`push`]: a static body none of
    /// whose components changed since is not looked at again.
    last_push: Option<(WorldId, Tick)>,
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

    /// True when `id` has a vehicle.
    pub fn is_vehicle(&self, id: EntityId) -> bool {
        self.vehicles.contains_key(&id)
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
pub(crate) fn world_affine(world: &World, entity: Entity) -> Affine3A {
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

/// The entity's world pose. The scale is the product of the `Transform` scales
/// (see [`world_scale`]) whenever that product is the matrix's scale up to
/// rounding: the matrix's column lengths round differently as the body turns,
/// which would change its shape's fingerprint and replace the body every few
/// steps. Under a non-uniformly scaled parent with a rotation in between, the
/// product is on the wrong axes, and the matrix's scale is used.
fn world_pose(world: &World, entity: Entity) -> (Vec3, Quat, Vec3) {
    let (s, r, t) = world_affine(world, entity).to_scale_rotation_translation();
    let product = world_scale(world, entity);
    let close = (s.abs() - product.abs())
        .abs()
        .cmple(product.abs() * 1e-4 + Vec3::splat(1e-6))
        .all();
    (t, r.normalize(), if close { product } else { s })
}

/// The product of the `Transform` scales from the root down to `entity`.
pub(crate) fn world_scale(world: &World, entity: Entity) -> Vec3 {
    let mut scale = Vec3::ONE;
    let mut at = Some(entity);
    while let Some(e) = at {
        if let Some(t) = world.ecs.get::<Transform>(e) {
            scale *= t.scale;
        }
        at = hierarchy::parent(&world.ecs, e);
    }
    scale
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
                precision: h.precision,
            }
        }
    })
}

/// What identifies a library-backed shape (convex, mesh, heightfield) without
/// its data: the kind, the content hash of its source and the scale.
#[derive(Clone, Copy, Debug)]
struct LibraryShape {
    kind: u8,
    content: blake3::Hash,
    scale: Vec3,
}

/// The library shape a collider names, or `None` for a primitive shape.
fn library_shape(
    c: &Collider,
    scale: Vec3,
    shapes: &ShapeLibrary,
    id: &str,
) -> Result<Option<LibraryShape>, Diagnostic> {
    let (kind, content) = match c.shape {
        ColliderShape::Box | ColliderShape::Sphere | ColliderShape::Capsule => return Ok(None),
        ColliderShape::Convex => (
            0,
            shapes
                .meshes
                .get(&c.source)
                .ok_or_else(|| missing_source(id, &c.source, "convex"))?
                .1,
        ),
        ColliderShape::Mesh => (
            1,
            shapes
                .meshes
                .get(&c.source)
                .ok_or_else(|| missing_source(id, &c.source, "mesh"))?
                .1,
        ),
        ColliderShape::Heightfield => (
            2,
            shapes
                .heightfields
                .get(&c.source)
                .ok_or_else(|| missing_source(id, &c.source, "heightfield"))?
                .1,
        ),
    };
    Ok(Some(LibraryShape {
        kind,
        content,
        scale: scale.abs(),
    }))
}

/// Stands in for a library shape in a [`Wanted`] body until the body is added.
const PENDING_SHAPE: ShapeDesc = ShapeDesc::Sphere { radius: 0.0 };

fn body_kind(rb: Option<&RigidBody>) -> BodyKind {
    match rb.map(|r| r.kind) {
        None | Some(SceneBodyKind::Static) => BodyKind::Static,
        Some(SceneBodyKind::Dynamic) => BodyKind::Dynamic,
        Some(SceneBodyKind::Kinematic) => BodyKind::Kinematic,
    }
}

enum Wanted {
    /// A body. A library shape is left as [`PENDING_SHAPE`] and named by the
    /// second field; [`Wanted::complete`] fills it in when the body is added.
    Body(BodyDesc, Option<LibraryShape>),
    Character(CharacterDesc),
}

impl Wanted {
    fn kind(&self) -> LinkKind {
        match self {
            Wanted::Body(d, _) => LinkKind::Body(d.kind),
            Wanted::Character(_) => LinkKind::Character,
        }
    }
}

impl Wanted {
    /// Builds a library shape's data into the description: done only when a
    /// body is added or replaced, not on every push.
    fn complete(
        self,
        world: &World,
        entity: Entity,
        shapes: &ShapeLibrary,
    ) -> Result<Wanted, Diagnostic> {
        match self {
            Wanted::Body(mut desc, Some(lib)) => {
                let id = world.id_of(entity).unwrap_or("?");
                let c = world
                    .ecs
                    .get::<Collider>(entity)
                    .expect("a library shape comes from a collider");
                desc.shape = shape_of(c, lib.scale, shapes, id)?;
                Ok(Wanted::Body(desc, None))
            }
            other => Ok(other),
        }
    }
}

/// The fingerprint of a description, ignoring its pose. A library shape
/// counts by its content hash and scale, so its data is neither copied nor
/// hashed on each push.
fn fingerprint(w: &Wanted) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    match w {
        Wanted::Body(d, lib) => {
            let mut d = d.clone();
            d.pos = Vec3::ZERO;
            d.rot = Quat::IDENTITY;
            let _ = serde_json::to_writer(&mut h, &d);
            if let Some(lib) = lib {
                h.update(&[lib.kind]);
                h.update(lib.content.as_bytes());
                for c in lib.scale.to_array() {
                    h.update(&c.to_le_bytes());
                }
            }
        }
        Wanted::Character(d) => {
            let _ = serde_json::to_writer(&mut h, d);
        }
    }
    h.finalize()
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
    let lib = library_shape(c, scale, shapes, id)?;
    let shape = match lib {
        Some(_) => PENDING_SHAPE,
        None => shape_of(c, scale, shapes, id)?,
    };
    Ok(Some(Wanted::Body(
        BodyDesc {
            kind: body_kind(rb),
            shape,
            pos,
            rot,
            mass: r.mass,
            friction: r.friction,
            restitution: r.restitution,
            linear_damping: r.linear_damping,
            angular_damping: r.angular_damping,
            center_of_mass_offset: r.center_of_mass_offset * scale,
            layer: layer_of(c.layer, id, "Collider")?,
            groups: groups_of(c.groups, id, "Collider")?,
            sensor: c.sensor,
        },
        lib,
    )))
}

fn add_cmd(id: EntityId, w: Wanted, pos: Vec3, rot: Quat) -> PhysicsCmd {
    match w {
        Wanted::Body(desc, _) => PhysicsCmd::AddBody { id, desc },
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
    let shapes_changed = sync.shapes_generation != shapes.generation;
    if shapes_changed {
        sync.shapes_generation = shapes.generation;
        sync.reported.clear();
    }
    // Changes since the last push in this world (a copied sync state in a
    // forked world looks at everything once).
    let now = world.ecs.change_tick();
    let since = sync
        .last_push
        .filter(|(w, _)| *w == world.ecs.id())
        .map(|(_, t)| t);
    let mut candidates: Vec<Entity> = world
        .ecs
        .query_filtered::<Entity, Or<(With<CharacterController>, With<Collider>)>>()
        .iter(&world.ecs)
        .collect();
    candidates.sort_by_key(|e| e.to_bits());
    let world_mut = world;
    let world = &*world_mut;

    let mut seen: BTreeSet<EntityId> = BTreeSet::new();
    let mut added: BTreeSet<EntityId> = BTreeSet::new();
    for entity in candidates {
        let id = entity_id(entity);
        if let Some(since) = since
            && let Some(link) = sync.links.get(&id)
            && unchanged_static(world, entity, link, since, now, shapes_changed)
        {
            seen.insert(id);
            continue;
        }
        let world_pose = world_pose(world, entity);
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
        let library = matches!(w, Wanted::Body(_, Some(_)));
        let rigid_body = world.ecs.get::<RigidBody>(entity).is_some();
        seen.insert(id);
        let (pos, rot, _) = world_pose;
        let fp = fingerprint(&w);
        let local = local_tr(world, entity);
        let parent = parent_affine(world, entity);
        match sync.links.get_mut(&id) {
            Some(link) if link.fingerprint == fp && link.entity == entity => {
                if link.written != local || link.parent != parent {
                    link.written = local;
                    link.parent = parent;
                    // A teleport, not motion: nothing to interpolate across.
                    // (A kinematic target is motion the next step performs.)
                    if link.kind != LinkKind::Body(BodyKind::Kinematic) {
                        link.prev = (pos, rot);
                        link.cur = (pos, rot);
                    }
                    cmds.push(match link.kind {
                        LinkKind::Body(BodyKind::Kinematic) => {
                            PhysicsCmd::SetKinematicTarget { id, pos, rot }
                        }
                        _ => PhysicsCmd::SetTransform { id, pos, rot },
                    });
                }
            }
            existing => {
                // Only now is a library shape's data copied into the body.
                let w = match w.complete(world, entity, shapes) {
                    Ok(w) => w,
                    Err(d) => {
                        let key = (id, *blake3::hash(d.message.as_bytes()).as_bytes());
                        if sync.reported.insert(key) {
                            diagnostics.push(d);
                        }
                        seen.remove(&id);
                        continue;
                    }
                };
                if let Some(old) = existing {
                    cmds.push(remove_cmd(id, old.kind));
                }
                let kind = w.kind();
                added.insert(id);
                cmds.push(add_cmd(id, w, pos, rot));
                sync.links.insert(
                    id,
                    Link {
                        entity,
                        kind,
                        fingerprint: fp,
                        written: local,
                        parent,
                        prev: (pos, rot),
                        cur: (pos, rot),
                        rigid_body,
                        library,
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
    push_vehicles(world, sync, &added, cmds);
    // Changes from here on carry a later tick than the one recorded.
    sync.last_push = Some((world.ecs.id(), now));
    world_mut.ecs.increment_change_tick();
}

/// True when `entity` is a static body whose `Transform`, `Collider` and
/// `RigidBody` have not changed since `since`, under a parent that has not
/// moved: [`push`] would find its fingerprint and pose as they were.
fn unchanged_static(
    world: &World,
    entity: Entity,
    link: &Link,
    since: Tick,
    now: Tick,
    shapes_changed: bool,
) -> bool {
    if link.entity != entity
        || link.kind != LinkKind::Body(BodyKind::Static)
        || (shapes_changed && link.library)
    {
        return false;
    }
    let Ok(e) = world.ecs.get_entity(entity) else {
        return false;
    };
    let changed = |ticks: Option<re_scene::ecs::change_detection::ComponentTicks>| {
        ticks.is_some_and(|t| t.is_changed(since, now))
    };
    e.contains::<RigidBody>() == link.rigid_body
        && !e.contains::<CharacterController>()
        && !changed(e.get_change_ticks::<Collider>())
        && !changed(e.get_change_ticks::<RigidBody>())
        && !changed(e.get_change_ticks::<Transform>())
        && parent_affine(world, entity) == link.parent
}

fn vehicle_kind(kind: SceneVehicleKind) -> VehicleKind {
    match kind {
        SceneVehicleKind::Wheeled => VehicleKind::Wheeled,
        SceneVehicleKind::Motorcycle => VehicleKind::Motorcycle,
    }
}

fn wheel_tester(v: &Vehicle) -> WheelTester {
    match v.tester {
        SceneWheelTester::Ray => WheelTester::Ray,
        SceneWheelTester::CastSphere => WheelTester::CastSphere {
            radius: v.tester_radius,
        },
        SceneWheelTester::CastCylinder => WheelTester::CastCylinder,
    }
}

fn vehicle_desc(v: &Vehicle, wheels: &[(&VehicleWheel, Vec3)], scale: Vec3) -> VehicleDesc {
    VehicleDesc {
        kind: vehicle_kind(v.kind),
        wheels: wheels
            .iter()
            .map(|(w, attach)| WheelDesc {
                pos: *attach * scale,
                radius: w.radius,
                width: w.width,
                suspension_min: w.suspension_min,
                suspension_max: w.suspension_max,
                suspension_frequency: w.suspension_frequency,
                suspension_damping: w.suspension_damping,
                max_steer_deg: w.max_steer_deg,
                max_brake_torque: w.max_brake_torque,
                max_hand_brake_torque: w.max_hand_brake_torque,
                driven: w.driven,
            })
            .collect(),
        engine: EngineDesc {
            max_torque: v.max_torque,
            min_rpm: v.min_rpm,
            max_rpm: v.max_rpm,
        },
        transmission: TransmissionDesc {
            gears: v.gears.clone(),
            reverse_gears: v.reverse_gears.clone(),
            shift_up_rpm: v.shift_up_rpm,
            shift_down_rpm: v.shift_down_rpm,
            differential_ratio: v.differential_ratio,
        },
        anti_roll_stiffness: v.anti_roll_stiffness,
        max_pitch_roll_deg: v.max_pitch_roll_deg,
        tester: wheel_tester(v),
    }
}

/// Adds, replaces and removes vehicles on linked bodies. `added` holds the
/// bodies this push added or replaced: replacing a body drops its vehicle, so
/// those get theirs added again.
fn push_vehicles(
    world: &World,
    sync: &mut PhysicsSync,
    added: &BTreeSet<EntityId>,
    cmds: &mut Vec<PhysicsCmd>,
) {
    sync.mounts.retain(|&wheel, _| {
        entity_from_id(wheel).is_some_and(|e| world.ecs.get::<VehicleWheel>(e).is_some())
    });
    let mut chassis: Vec<Entity> = world
        .ecs
        .iter_entities()
        .filter(|e| e.contains::<Vehicle>())
        .map(|e| e.id())
        .collect();
    chassis.sort_by_key(|e| e.to_bits());
    let mut seen: BTreeSet<EntityId> = BTreeSet::new();
    for entity in chassis {
        let id = entity_id(entity);
        // A vehicle needs a body.
        let Some(kind) = sync
            .links
            .get(&id)
            .filter(|l| l.entity == entity)
            .map(|l| l.kind)
        else {
            continue;
        };
        let Some(v) = world.ecs.get::<Vehicle>(entity) else {
            continue;
        };
        let mut wheel_entities = Vec::new();
        let mut wheels = Vec::new();
        for &child in hierarchy::children(&world.ecs, entity) {
            let Some(w) = world.ecs.get::<VehicleWheel>(child) else {
                continue;
            };
            let local = local_tr(world, child);
            let mount = sync
                .mounts
                .entry(entity_id(child))
                .and_modify(|m| {
                    if m.written != local {
                        m.attach = local.0;
                        m.written = local;
                    }
                })
                .or_insert(WheelMount {
                    attach: local.0,
                    written: local,
                });
            wheel_entities.push(child);
            wheels.push((w, mount.attach));
        }
        let desc = vehicle_desc(v, &wheels, world_scale(world, entity).abs());
        let fp = blake3::hash(serde_json::to_string(&desc).unwrap_or_default().as_bytes());
        if kind != LinkKind::Body(BodyKind::Dynamic) {
            // `re_physics` refuses it (RE5018): send it once per description so the
            // problem is reported, and record no vehicle.
            if sync.reported.insert((id, *fp.as_bytes())) {
                cmds.push(PhysicsCmd::AddVehicle { id, desc });
            }
            continue;
        }
        seen.insert(id);
        let replaced = added.contains(&id);
        let old = sync.vehicles.get(&id);
        let unchanged = old.is_some_and(|o| o.fingerprint == fp && o.wheels == wheel_entities);
        if unchanged && !replaced {
            continue;
        }
        if old.is_some() && !replaced {
            cmds.push(PhysicsCmd::RemoveVehicle { id });
        }
        cmds.push(PhysicsCmd::AddVehicle { id, desc });
        sync.vehicles.insert(
            id,
            VehicleLink {
                fingerprint: fp,
                wheels: wheel_entities,
            },
        );
    }
    let gone: Vec<EntityId> = sync
        .vehicles
        .keys()
        .filter(|id| !seen.contains(id))
        .copied()
        .collect();
    for id in gone {
        sync.vehicles.remove(&id);
        // A RemoveVehicle after its body went is a no-op in `re_physics`.
        cmds.push(PhysicsCmd::RemoveVehicle { id });
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
        link.parent = parent;
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

/// Writes wheel poses from physics into the wheel entities' `Transform`s (as
/// local transforms under the chassis entity, after [`pull`] moved it; scale is
/// kept). Poses of vehicles or wheels that are no longer linked are ignored.
pub fn pull_wheels(world: &mut World, sync: &mut PhysicsSync, poses: &[WheelPose]) {
    for pose in poses {
        let Some(chassis) = sync.links.get(&pose.id).map(|l| l.entity) else {
            continue;
        };
        let Some(link) = sync.vehicles.get(&pose.id) else {
            continue;
        };
        let Some(&entity) = link.wheels.get(pose.wheel as usize) else {
            continue;
        };
        let parent = world_affine(world, chassis);
        let local = parent.inverse() * Affine3A::from_rotation_translation(pose.rot, pose.pos);
        let (_, rot, t) = local.to_scale_rotation_translation();
        let rot = rot.normalize();
        if let Some(m) = sync.mounts.get_mut(&entity_id(entity)) {
            m.written = (t, rot);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A rotated child of a stretched parent keeps the shape that is drawn: the
    /// parent's (4, 1, 1) lies along the child's z after a quarter turn about y.
    #[test]
    fn a_rotated_child_of_a_stretched_parent_is_stretched_along_world_x() {
        let mut world = World::new();
        let parent = world
            .ecs
            .spawn(Transform {
                scale: Vec3::new(4.0, 1.0, 1.0),
                ..Default::default()
            })
            .id();
        world.ecs.spawn((
            Transform {
                rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                ..Default::default()
            },
            Collider::default(),
            re_scene::ecs::hierarchy::ChildOf(parent),
        ));
        world.update();
        let mut sync = PhysicsSync::default();
        let (mut cmds, mut diagnostics) = (Vec::new(), Vec::new());
        push(
            &mut world,
            &mut sync,
            &ShapeLibrary::default(),
            &mut cmds,
            &mut diagnostics,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let [PhysicsCmd::AddBody { desc, .. }] = &cmds[..] else {
            panic!("{cmds:?}");
        };
        let ShapeDesc::Box { half_extents } = desc.shape else {
            panic!("{desc:?}");
        };
        assert!(
            half_extents.abs_diff_eq(Vec3::new(0.5, 0.5, 2.0), 1e-5),
            "{half_extents}"
        );
    }

    /// A turning body keeps its shape: the scale comes from the `Transform`s, not
    /// from the decomposed world matrix, whose column lengths round differently at
    /// each rotation (0.3 becomes 0.29999998) and would replace the body.
    #[test]
    fn a_rotation_moves_a_scaled_body_without_replacing_it() {
        let mut world = World::new();
        let e = world
            .ecs
            .spawn((
                Transform {
                    scale: Vec3::new(0.3, 0.7, 1.1),
                    ..Default::default()
                },
                RigidBody {
                    kind: SceneBodyKind::Dynamic,
                    ..Default::default()
                },
                Collider::default(),
            ))
            .id();
        let mut sync = PhysicsSync::default();
        let shapes = ShapeLibrary::default();
        let (mut cmds, mut diagnostics) = (Vec::new(), Vec::new());
        push(&mut world, &mut sync, &shapes, &mut cmds, &mut diagnostics);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(cmds.len(), 1, "{cmds:?}");
        for i in 1..=64 {
            let angle = i as f32 * 0.37;
            world.ecs.get_mut::<Transform>(e).unwrap().rotation =
                Quat::from_euler(glam::EulerRot::XYZ, angle, angle * 0.5, angle * 0.25);
            cmds.clear();
            push(&mut world, &mut sync, &shapes, &mut cmds, &mut diagnostics);
            assert!(
                matches!(cmds[..], [PhysicsCmd::SetTransform { .. }]),
                "step {i}: {cmds:?}"
            );
        }
    }

    fn field(height: f32) -> Arc<HeightfieldData> {
        Arc::new(HeightfieldData {
            n: 3,
            spacing: 1.0,
            offset: Vec3::ZERO,
            samples: vec![height; 9],
            precision: None,
        })
    }

    fn adds(cmds: &[PhysicsCmd]) -> usize {
        cmds.iter()
            .filter(|c| matches!(c, PhysicsCmd::AddBody { .. }))
            .count()
    }

    /// An unchanged heightfield is not rebuilt on each push; new data, an
    /// equal copy and a new scale behave as they did when its samples were
    /// fingerprinted.
    #[test]
    fn heightfields_are_fingerprinted_by_content_and_scale() {
        let mut world = World::new();
        let mut shapes = ShapeLibrary::default();
        shapes.insert_heightfield("derived:t", field(1.0));
        let e = world
            .ecs
            .spawn((
                Transform::default(),
                Collider {
                    shape: ColliderShape::Heightfield,
                    source: "derived:t".into(),
                    ..Collider::default()
                },
            ))
            .id();
        let mut sync = PhysicsSync::new();
        let mut diagnostics = Vec::new();
        let mut push_once = |world: &mut World, shapes: &ShapeLibrary| {
            let mut cmds = Vec::new();
            push(world, &mut sync, shapes, &mut cmds, &mut diagnostics);
            cmds
        };
        let first = push_once(&mut world, &shapes);
        assert_eq!(adds(&first), 1);
        let PhysicsCmd::AddBody { desc, .. } = &first[0] else {
            panic!("{first:?}");
        };
        assert!(
            matches!(&desc.shape, ShapeDesc::Heightfield { samples, .. } if samples == &vec![1.0; 9]),
            "{desc:?}"
        );
        assert!(push_once(&mut world, &shapes).is_empty());
        // An equal copy under the same name changes nothing.
        shapes.insert_heightfield("derived:t", field(1.0));
        assert!(push_once(&mut world, &shapes).is_empty());
        // New data replaces the body.
        shapes.insert_heightfield("derived:t", field(2.0));
        assert_eq!(adds(&push_once(&mut world, &shapes)), 1);
        // So does a new scale.
        world.ecs.get_mut::<Transform>(e).unwrap().scale = Vec3::new(1.0, 2.0, 1.0);
        let scaled = push_once(&mut world, &shapes);
        assert_eq!(adds(&scaled), 1);
        let Some(PhysicsCmd::AddBody { desc, .. }) = scaled
            .iter()
            .find(|c| matches!(c, PhysicsCmd::AddBody { .. }))
        else {
            panic!("{scaled:?}");
        };
        assert!(
            matches!(&desc.shape, ShapeDesc::Heightfield { samples, .. } if samples == &vec![4.0; 9]),
            "{desc:?}"
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
}

#[cfg(test)]
mod skip_tests {
    use super::*;

    fn replaced(cmds: &[PhysicsCmd]) -> bool {
        cmds.iter().any(|c| matches!(c, PhysicsCmd::AddBody { .. }))
    }

    /// A static body is skipped while nothing about it changes, and every
    /// kind of change is still seen.
    #[test]
    fn unchanged_static_bodies_are_skipped_and_changes_still_seen() {
        let mut world = World::new();
        let shapes = ShapeLibrary::default();
        let e = world
            .ecs
            .spawn((Transform::default(), Collider::default()))
            .id();
        let mut sync = PhysicsSync::new();
        let mut diagnostics = Vec::new();
        let mut push_once = |world: &mut World| {
            let mut cmds = Vec::new();
            push(world, &mut sync, &shapes, &mut cmds, &mut diagnostics);
            cmds
        };
        assert!(replaced(&push_once(&mut world)));
        assert!(push_once(&mut world).is_empty());
        assert!(push_once(&mut world).is_empty());
        // A collider edit.
        world.ecs.get_mut::<Collider>(e).unwrap().half_extents = Vec3::ONE;
        assert!(replaced(&push_once(&mut world)));
        assert!(push_once(&mut world).is_empty());
        // A move is a teleport.
        world.ecs.get_mut::<Transform>(e).unwrap().translation = Vec3::X;
        let moved = push_once(&mut world);
        assert!(
            matches!(moved.as_slice(), [PhysicsCmd::SetTransform { .. }]),
            "{moved:?}"
        );
        // A static RigidBody added, then removed: friction and the rest
        // come and go with it.
        world.ecs.entity_mut(e).insert(RigidBody {
            kind: SceneBodyKind::Static,
            friction: 0.1,
            ..RigidBody::default()
        });
        assert!(replaced(&push_once(&mut world)));
        world.ecs.entity_mut(e).remove::<RigidBody>();
        assert!(replaced(&push_once(&mut world)));
        assert!(push_once(&mut world).is_empty());
        // Disabled: the body goes.
        world.ecs.get_mut::<Collider>(e).unwrap().enabled = false;
        let gone = push_once(&mut world);
        assert!(
            matches!(gone.as_slice(), [PhysicsCmd::RemoveBody { .. }]),
            "{gone:?}"
        );
    }
}
