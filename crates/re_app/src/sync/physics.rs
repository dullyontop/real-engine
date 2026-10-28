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
//! A change to a body's shape alone (its collider's size or `source`, or the
//! entity's scale) changes the shape in place, keeping the body's motion and
//! contacts, except on a vehicle's chassis; any other change to its description
//! replaces the body (remove, then add at the current pose, a dynamic body keeping
//! its velocity). A `Transform` written by anything other than [`pull`] (a
//! script, an edit) teleports the body, or moves a kinematic body's target.
//!
//! [`push`] looks only at what may have changed since the last push: entities
//! whose `Collider`, `CharacterController`, `RigidBody`, `Transform` or parent
//! changed or went (by their change ticks and the World's removal messages),
//! static bodies under a parent whose `GlobalTransform` changed, every moving
//! body and character, and, when the [`ShapeLibrary`] changes, the bodies whose
//! shape it holds and the entities that failed. The first push in a World, and
//! one after removal messages were missed, looks at every entity.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use glam::{Affine3A, Quat, Vec3};
use re_core::{Diagnostic, Location};
use re_scene::ecs::change_detection::{DetectChanges, Ref, Tick};
use re_scene::ecs::hierarchy::{ChildOf, Children};
use re_scene::ecs::lifecycle::RemovedComponentEntity;
use re_scene::ecs::message::{MessageCursor, Messages};
use re_scene::ecs::query::{Or, QueryState, With};
use re_scene::ecs::world::{World as EcsWorld, WorldId};
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
    /// The fingerprint of the description without its pose and shape.
    fingerprint: blake3::Hash,
    /// The fingerprint of the shape.
    shape: blake3::Hash,
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
    /// The links that are not static bodies: [`push`] looks at each on every
    /// push, and [`PhysicsSync::begin_step`] moves their poses on.
    moving: BTreeSet<EntityId>,
    /// The links whose shape comes from the [`ShapeLibrary`].
    library: BTreeSet<EntityId>,
    /// Entities with a `Collider` or a `CharacterController` and no body because
    /// their description failed: looked at again when the library changes.
    failed: BTreeSet<EntityId>,
    vehicles: BTreeMap<EntityId, VehicleLink>,
    mounts: BTreeMap<EntityId, WheelMount>,
    /// (entity, fingerprint) pairs already reported as failing, so a broken
    /// collider is reported once, not every step.
    reported: BTreeSet<(EntityId, [u8; 32])>,
    shapes_generation: u64,
    /// The world and change tick of the last [`push`]: a static body none of
    /// whose components changed since is not looked at again.
    last_push: Option<(WorldId, Tick)>,
    /// How [`push`] finds what changed in its World.
    watch: Watch,
    /// The links pushes added or replaced since the last
    /// [`PhysicsSync::end_step`].
    added: BTreeSet<EntityId>,
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

    /// Starts a step: the pose after the last step becomes the pose before this
    /// one (static bodies are not interpolated).
    pub fn begin_step(&mut self) {
        for id in &self.moving {
            if let Some(link) = self.links.get_mut(id) {
                link.prev = link.cur;
            }
        }
    }

    /// The bodies and characters that pushes added, or replaced, since the
    /// last [`PhysicsSync::end_step`], in id order: what a check after the
    /// step's commands are applied looks at (`stream.ground_ready`, ADR-0057
    /// §2). A body waiting out of reach of a moved origin is among them before
    /// it is in Jolt.
    pub fn added(&self) -> &BTreeSet<EntityId> {
        &self.added
    }

    /// Ends a step's pushes: forgets what they added.
    pub fn end_step(&mut self) {
        self.added.clear();
    }

    /// Links `id`, keeping the sets of moving and library-shaped links.
    fn insert_link(&mut self, id: EntityId, link: Link) {
        if link.kind == LinkKind::Body(BodyKind::Static) {
            self.moving.remove(&id);
        } else {
            self.moving.insert(id);
        }
        if link.library {
            self.library.insert(id);
        } else {
            self.library.remove(&id);
        }
        self.links.insert(id, link);
    }

    /// Unlinks `id`, queueing the removal of its body or character.
    fn remove_link(&mut self, id: EntityId, cmds: &mut Vec<PhysicsCmd>) {
        if let Some(link) = self.links.remove(&id) {
            self.moving.remove(&id);
            self.library.remove(&id);
            self.added.remove(&id);
            cmds.push(remove_cmd(id, link.kind));
        }
    }
}

/// The queries and removal cursors [`push`] uses to find what changed in one
/// World. A copy starts without them, so its first push looks at every entity.
#[derive(Default)]
struct Watch(Option<Box<WatchState>>);

impl Clone for Watch {
    fn clone(&self) -> Self {
        Watch(None)
    }
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Watch")
            .field(&self.0.as_ref().map(|w| w.world))
            .finish()
    }
}

/// An entity with physics: a `Collider` or a `CharacterController`.
type Candidate = Or<(With<Collider>, With<CharacterController>)>;

/// The change ticks of what a candidate's description and pose read.
type CandidateTicks = (
    Entity,
    Option<Ref<'static, Collider>>,
    Option<Ref<'static, CharacterController>>,
    Option<Ref<'static, RigidBody>>,
    Option<Ref<'static, Transform>>,
    Option<Ref<'static, ChildOf>>,
);

struct WatchState {
    /// The World the queries belong to.
    world: WorldId,
    /// Every entity with physics.
    candidates: QueryState<Entity, Candidate>,
    /// What every entity with physics reads, with its change ticks.
    ticks: QueryState<CandidateTicks, Candidate>,
    /// Every parent's `GlobalTransform`, with its children.
    parents: QueryState<(Ref<'static, GlobalTransform>, &'static Children)>,
    /// Every entity with a `Vehicle`.
    vehicles: QueryState<Entity, With<Vehicle>>,
    /// Where the last push stopped reading the removals of `Collider`,
    /// `CharacterController`, `RigidBody`, `ChildOf` and `GlobalTransform`.
    removed: [MessageCursor<RemovedComponentEntity>; 5],
    /// The cursors stand at the end of the removals as of the last push.
    current: bool,
}

impl WatchState {
    fn new(world: &mut EcsWorld) -> WatchState {
        WatchState {
            world: world.id(),
            candidates: world.query_filtered(),
            ticks: world.query_filtered(),
            parents: world.query(),
            vehicles: world.query_filtered(),
            removed: Default::default(),
            current: false,
        }
    }

    /// The removal messages of the five components the cursors follow, where
    /// any were sent.
    fn removals(world: &EcsWorld) -> [Option<&Messages<RemovedComponentEntity>>; 5] {
        let removed = world.removed_components();
        [
            world.component_id::<Collider>(),
            world.component_id::<CharacterController>(),
            world.component_id::<RigidBody>(),
            world.component_id::<ChildOf>(),
            world.component_id::<GlobalTransform>(),
        ]
        .map(|id| id.and_then(|id| removed.get(id)))
    }

    /// Moves the cursors past every removal so far.
    fn catch_up(&mut self, world: &EcsWorld) {
        for (cursor, messages) in self.removed.iter_mut().zip(Self::removals(world)) {
            if let Some(messages) = messages {
                cursor.clear(messages);
            }
        }
        self.current = true;
    }

    /// The entities whose push may differ from the last one, which ran at
    /// `since`: a candidate whose collider, character, rigid body, transform
    /// or parent changed, a link whose parent's `GlobalTransform` changed,
    /// and an entity that lost one of those components or was despawned
    /// (a link under a parent that lost its `GlobalTransform` too). `None`
    /// before the first look at every entity in this World, or when removals
    /// were missed: only a look at every entity can tell then.
    fn changed(
        &mut self,
        world: &EcsWorld,
        links: &BTreeMap<EntityId, Link>,
        since: Tick,
        now: Tick,
    ) -> Option<Vec<EntityId>> {
        if !self.current {
            return None;
        }
        let new = |tick: Option<Tick>| tick.is_some_and(|t| t.is_newer_than(since, now));
        let mut changed = Vec::new();
        for (entity, collider, character, body, transform, child_of) in self.ticks.iter(world) {
            let ticks = [
                collider.map(|r| r.last_changed()),
                character.map(|r| r.last_changed()),
                body.map(|r| r.last_changed()),
                transform.map(|r| r.last_changed()),
                child_of.map(|r| r.last_changed()),
            ];
            if ticks.into_iter().any(new) {
                changed.push(entity_id(entity));
            }
        }
        for (global, children) in self.parents.iter(world) {
            if new(Some(global.last_changed())) {
                changed.extend(
                    children
                        .iter()
                        .map(|&c| entity_id(c))
                        .filter(|id| links.contains_key(id)),
                );
            }
        }
        let [collider, character, body, child_of, global] = &mut self.removed;
        let messages = Self::removals(world);
        for (cursor, messages) in [collider, character, body, child_of]
            .into_iter()
            .zip(messages)
        {
            let Some(messages) = messages else {
                continue;
            };
            if cursor.missed_messages(messages) > 0 {
                return None;
            }
            changed.extend(
                cursor
                    .read(messages)
                    .map(|e| entity_id(Entity::from(e.clone()))),
            );
        }
        if let Some(messages) = messages[4] {
            if global.missed_messages(messages) > 0 {
                return None;
            }
            for e in global.read(messages) {
                changed.extend(
                    hierarchy::children(world, Entity::from(e.clone()))
                        .iter()
                        .map(|&c| entity_id(c))
                        .filter(|id| links.contains_key(id)),
                );
            }
        }
        Some(changed)
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

/// A fingerprint written field by field. Equal fingerprints mean equal
/// fields, where every number that is not finite counts as one value (as in
/// the JSON that fingerprints were once made of, which wrote each as `null`).
struct Fingerprint(blake3::Hasher);

impl Fingerprint {
    /// A fingerprint of the kind of value `tag` names.
    fn new(tag: u8) -> Fingerprint {
        let mut h = blake3::Hasher::new();
        h.update(&[tag]);
        Fingerprint(h)
    }

    fn tag(&mut self, v: u8) -> &mut Self {
        self.0.update(&[v]);
        self
    }

    fn bool(&mut self, v: bool) -> &mut Self {
        self.tag(u8::from(v))
    }

    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.update(&v.to_le_bytes());
        self
    }

    fn len(&mut self, n: usize) -> &mut Self {
        self.0.update(&(n as u64).to_le_bytes());
        self
    }

    fn f32(&mut self, v: f32) -> &mut Self {
        if v.is_finite() {
            self.0.update(&[1]);
            self.0.update(&v.to_le_bytes());
        } else {
            self.0.update(&[0]);
        }
        self
    }

    fn vec3(&mut self, v: Vec3) -> &mut Self {
        self.f32(v.x).f32(v.y).f32(v.z)
    }

    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.update(b);
        self
    }

    fn finish(&self) -> blake3::Hash {
        self.0.finalize()
    }
}

/// The fingerprint of a body's description without its pose and its shape,
/// which [`push`] compares apart.
fn body_fingerprint(d: &BodyDesc) -> blake3::Hash {
    let BodyDesc {
        kind,
        shape: _,
        pos: _,
        rot: _,
        mass,
        friction,
        restitution,
        linear_damping,
        angular_damping,
        layer,
        groups,
        sensor,
        center_of_mass_offset,
    } = d;
    let mut f = Fingerprint::new(b'b');
    f.tag(match kind {
        BodyKind::Static => 0,
        BodyKind::Dynamic => 1,
        BodyKind::Kinematic => 2,
    })
    .f32(*mass)
    .f32(*friction)
    .f32(*restitution)
    .f32(*linear_damping)
    .f32(*angular_damping)
    .u32(u32::from(layer.0))
    .u32(groups.0)
    .bool(*sensor);
    // A zero offset is the same as none, as in the description's serialized form.
    if *center_of_mass_offset == Vec3::ZERO {
        f.tag(0);
    } else {
        f.tag(1).vec3(*center_of_mass_offset);
    }
    f.finish()
}

/// The fingerprint of a character's description.
fn character_fingerprint(d: &CharacterDesc) -> blake3::Hash {
    let CharacterDesc {
        height,
        radius,
        max_slope_deg,
        step_height,
        mass,
        inner_body,
        layer,
        groups,
    } = d;
    Fingerprint::new(b'c')
        .f32(*height)
        .f32(*radius)
        .f32(*max_slope_deg)
        .f32(*step_height)
        .f32(*mass)
        .bool(*inner_body)
        .u32(u32::from(layer.0))
        .u32(groups.0)
        .finish()
}

/// The fingerprint of a body's shape. A library shape counts by its content
/// hash and scale, so its data is neither copied nor hashed on each push.
fn shape_fingerprint(shape: &ShapeDesc, lib: Option<&LibraryShape>) -> blake3::Hash {
    let mut f = Fingerprint::new(b's');
    match shape {
        ShapeDesc::Box { half_extents } => {
            f.tag(0).vec3(*half_extents);
        }
        ShapeDesc::Sphere { radius } => {
            f.tag(1).f32(*radius);
        }
        ShapeDesc::Capsule {
            half_height,
            radius,
        } => {
            f.tag(2).f32(*half_height).f32(*radius);
        }
        ShapeDesc::ConvexHull { points } => {
            f.tag(3).len(points.len());
            for p in points {
                f.vec3(*p);
            }
        }
        ShapeDesc::TriangleMesh {
            vertices,
            triangles,
        } => {
            f.tag(4).len(vertices.len());
            for v in vertices {
                f.vec3(*v);
            }
            f.len(triangles.len());
            for t in triangles.as_flattened() {
                f.u32(*t);
            }
        }
        ShapeDesc::Heightfield {
            n,
            samples,
            spacing,
            offset,
            precision,
        } => {
            f.tag(5).u32(*n).len(samples.len());
            for s in samples {
                f.f32(*s);
            }
            f.f32(*spacing).vec3(*offset);
            if let Some(re_types::physics::HeightfieldPrecision {
                bits_per_sample,
                block_size,
            }) = precision
            {
                f.tag(1).tag(*bits_per_sample).tag(*block_size);
            } else {
                f.tag(0);
            }
        }
    }
    if let Some(LibraryShape {
        kind,
        content,
        scale,
    }) = lib
    {
        f.tag(1).tag(*kind).bytes(content.as_bytes());
        for c in scale.to_array() {
            f.bytes(&c.to_le_bytes());
        }
    } else {
        f.tag(0);
    }
    f.finish()
}

/// The fingerprints of a description, ignoring its pose: of everything but the
/// shape, and of the shape.
fn fingerprints(w: &Wanted) -> (blake3::Hash, blake3::Hash) {
    match w {
        Wanted::Body(d, lib) => (
            body_fingerprint(d),
            shape_fingerprint(&d.shape, lib.as_ref()),
        ),
        Wanted::Character(d) => (character_fingerprint(d), Fingerprint::new(b'n').finish()),
    }
}

/// The fingerprint of a vehicle's description.
fn vehicle_fingerprint(d: &VehicleDesc) -> blake3::Hash {
    let VehicleDesc {
        kind,
        wheels,
        engine,
        transmission,
        anti_roll_stiffness,
        max_pitch_roll_deg,
        tester,
    } = d;
    let mut f = Fingerprint::new(b'v');
    f.tag(match kind {
        VehicleKind::Wheeled => 0,
        VehicleKind::Motorcycle => 1,
    })
    .len(wheels.len());
    for w in wheels {
        let WheelDesc {
            pos,
            radius,
            width,
            suspension_min,
            suspension_max,
            suspension_frequency,
            suspension_damping,
            max_steer_deg,
            max_brake_torque,
            max_hand_brake_torque,
            driven,
        } = w;
        f.vec3(*pos)
            .f32(*radius)
            .f32(*width)
            .f32(*suspension_min)
            .f32(*suspension_max)
            .f32(*suspension_frequency)
            .f32(*suspension_damping)
            .f32(*max_steer_deg)
            .f32(*max_brake_torque)
            .f32(*max_hand_brake_torque)
            .bool(*driven);
    }
    let EngineDesc {
        max_torque,
        min_rpm,
        max_rpm,
    } = engine;
    f.f32(*max_torque).f32(*min_rpm).f32(*max_rpm);
    let TransmissionDesc {
        gears,
        reverse_gears,
        shift_up_rpm,
        shift_down_rpm,
        differential_ratio,
    } = transmission;
    f.len(gears.len());
    for g in gears {
        f.f32(*g);
    }
    f.len(reverse_gears.len());
    for g in reverse_gears {
        f.f32(*g);
    }
    f.f32(*shift_up_rpm)
        .f32(*shift_down_rpm)
        .f32(*differential_ratio)
        .f32(*anti_roll_stiffness)
        .f32(*max_pitch_roll_deg);
    match tester {
        WheelTester::Ray => f.tag(0),
        WheelTester::CastSphere { radius } => f.tag(1).f32(*radius),
        WheelTester::CastCylinder => f.tag(2),
    }
    .finish()
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

/// The shape physics simulates for `entity`, with the world pose the next
/// [`push`] gives it: a collider's shape, scaled as its body's is, or a
/// character's capsule about its centre (ADR-0057 §9's `debug.physics`
/// outlines). `None` for an entity without physics, with a disabled collider,
/// or with a description `push` refuses (its diagnostic says why). A mesh's or
/// a heightfield's data is copied.
pub fn shape_and_pose(
    world: &World,
    entity: Entity,
    shapes: &ShapeLibrary,
) -> Option<(ShapeDesc, Vec3, Quat)> {
    let pose = world_pose(world, entity);
    let (pos, rot, _) = pose;
    let shape = match wanted(world, entity, shapes, pose)
        .ok()??
        .complete(world, entity, shapes)
        .ok()?
    {
        Wanted::Body(desc, _) => desc.shape,
        Wanted::Character(d) => ShapeDesc::Capsule {
            half_height: d.height * 0.5 - d.radius,
            radius: d.radius,
        },
    };
    Some((shape, pos, rot))
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

/// Drops the commands scripts queued (through the host) for entities despawned
/// since: an impulse, a velocity, a character move, or a vehicle's or a
/// ragdoll's input for an entity destroyed in the same frame has nothing left to
/// move, and is not an error. Commands for live entities stay.
fn drop_despawned(world: &World, cmds: &mut Vec<PhysicsCmd>) {
    cmds.retain(|cmd| match cmd {
        PhysicsCmd::ApplyImpulse { id, .. }
        | PhysicsCmd::SetVelocity { id, .. }
        | PhysicsCmd::MoveCharacter { id, .. }
        | PhysicsCmd::SetVehicleInput { id, .. }
        | PhysicsCmd::SetRagdollMode { id, .. }
        | PhysicsCmd::ApplyRagdollImpulse { id, .. } => {
            entity_from_id(*id).is_some_and(|e| world.ecs.get_entity(e).is_ok())
        }
        _ => true,
    });
}

/// Turns World changes since the last call into physics commands, appended to
/// `cmds` (unsorted; `Physics::apply` sorts by kind and entity).
///
/// Adds bodies and characters for new entities, replaces the ones whose
/// description changed, removes the ones whose entity or component is gone or
/// whose collider is disabled, and teleports (or, for kinematic bodies,
/// retargets) entities whose `Transform` was written by someone other than
/// [`pull`]. Problems become diagnostics in `diagnostics`, each reported once;
/// the entity then gets no body until it is fixed. Commands already in `cmds`
/// for entities despawned since they were queued are dropped.
pub fn push(
    world: &mut World,
    sync: &mut PhysicsSync,
    shapes: &ShapeLibrary,
    cmds: &mut Vec<PhysicsCmd>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    drop_despawned(world, cmds);
    // A shape library change may fix a missing source: report again if still broken.
    let shapes_changed = sync.shapes_generation != shapes.generation;
    if shapes_changed {
        sync.shapes_generation = shapes.generation;
        sync.reported.clear();
    }
    // Changes since the last push in this world (a copied sync state in a
    // forked world looks at everything once).
    let now = world.ecs.change_tick();
    let world_id = world.ecs.id();
    let since = sync
        .last_push
        .filter(|(w, _)| *w == world_id)
        .map(|(_, t)| t);
    if sync.watch.0.as_ref().is_none_or(|w| w.world != world_id) {
        sync.watch.0 = Some(Box::new(WatchState::new(&mut world.ecs)));
    }
    let world_mut = world;
    let world = &*world_mut;
    let mut watch = sync.watch.0.take().expect("made above");
    let changed = since.and_then(|since| watch.changed(&world.ecs, &sync.links, since, now));
    let mut push = Push {
        world,
        shapes,
        since,
        now,
        shapes_changed,
        cmds,
        diagnostics,
        added: BTreeSet::new(),
    };
    match changed {
        Some(mut changed) => {
            // What may have changed, and what moves.
            if shapes_changed {
                changed.extend(sync.library.iter().chain(&sync.failed));
            }
            changed.extend(&sync.moving);
            changed.sort_unstable();
            changed.dedup();
            for id in changed {
                let candidate = entity_from_id(id).filter(|&e| {
                    world.ecs.get_entity(e).is_ok_and(|e| {
                        e.contains::<Collider>() || e.contains::<CharacterController>()
                    })
                });
                let kept = match candidate {
                    Some(entity) => push.visit(sync, entity),
                    None => {
                        sync.failed.remove(&id);
                        false
                    }
                };
                if !kept {
                    sync.remove_link(id, push.cmds);
                }
            }
        }
        None => {
            // Every entity: the first push in this World, or one that cannot
            // tell what changed.
            let mut candidates: Vec<Entity> = watch.candidates.iter(&world.ecs).collect();
            candidates.sort_by_key(|e| e.to_bits());
            sync.failed.clear();
            let mut seen: BTreeSet<EntityId> = BTreeSet::new();
            for entity in candidates {
                if push.visit(sync, entity) {
                    seen.insert(entity_id(entity));
                }
            }
            let gone: Vec<EntityId> = sync
                .links
                .keys()
                .filter(|id| !seen.contains(id))
                .copied()
                .collect();
            for id in gone {
                sync.remove_link(id, push.cmds);
            }
        }
    }
    watch.catch_up(&world.ecs);
    let mut chassis: Vec<Entity> = watch.vehicles.iter(&world.ecs).collect();
    chassis.sort_by_key(|e| e.to_bits());
    sync.watch.0 = Some(watch);
    let Push { cmds, added, .. } = push;
    push_vehicles(world, sync, &chassis, &added, cmds);
    sync.added.extend(added);
    // Changes from here on carry a later tick than the one recorded.
    sync.last_push = Some((world_id, now));
    world_mut.ecs.increment_change_tick();
}

/// One [`push`]: what it reads, and where its commands and diagnostics go.
struct Push<'a> {
    world: &'a World,
    shapes: &'a ShapeLibrary,
    /// The tick of the last push in this World, if any.
    since: Option<Tick>,
    now: Tick,
    shapes_changed: bool,
    cmds: &'a mut Vec<PhysicsCmd>,
    diagnostics: &'a mut Vec<Diagnostic>,
    /// The bodies this push added or replaced.
    added: BTreeSet<EntityId>,
}

impl Push<'_> {
    /// Reports `d` for `id`, once for each problem, and remembers that the
    /// entity failed.
    fn fail(&mut self, sync: &mut PhysicsSync, id: EntityId, d: Diagnostic) {
        let key = (id, *blake3::hash(d.message.as_bytes()).as_bytes());
        if sync.reported.insert(key) {
            self.diagnostics.push(d);
        }
        sync.failed.insert(id);
    }

    /// Compares an entity with a `Collider` or a `CharacterController` with
    /// its link, queueing what changed. Returns whether it keeps (or now
    /// has) a link; the caller removes one it does not keep.
    fn visit(&mut self, sync: &mut PhysicsSync, entity: Entity) -> bool {
        let world = self.world;
        let id = entity_id(entity);
        if let Some(since) = self.since
            && let Some(link) = sync.links.get(&id)
            && unchanged_static(world, entity, link, since, self.now, self.shapes_changed)
        {
            return true;
        }
        let world_pose = world_pose(world, entity);
        let w = match wanted(world, entity, self.shapes, world_pose) {
            Ok(Some(w)) => w,
            // A disabled collider has no body.
            Ok(None) => {
                sync.failed.remove(&id);
                return false;
            }
            // A broken description removes an existing body rather than
            // leaving a stale one.
            Err(d) => {
                self.fail(sync, id, d);
                return false;
            }
        };
        let library = matches!(w, Wanted::Body(_, Some(_)));
        let rigid_body = world.ecs.get::<RigidBody>(entity).is_some();
        let (pos, rot, _) = world_pose;
        let (fp, shape_fp) = fingerprints(&w);
        let local = local_tr(world, entity);
        let parent = parent_affine(world, entity);
        // Jolt cannot change the shape of a body that carries a vehicle.
        let reshapes = !sync.vehicles.contains_key(&id);
        match sync.links.get_mut(&id) {
            Some(link)
                if link.fingerprint == fp
                    && link.entity == entity
                    && (link.shape == shape_fp || reshapes) =>
            {
                if link.shape != shape_fp {
                    // Only the shape changed: the body keeps its motion and contacts.
                    match w.complete(world, entity, self.shapes) {
                        Ok(Wanted::Body(desc, _)) => {
                            link.shape = shape_fp;
                            link.library = library;
                            self.cmds.push(PhysicsCmd::SetShape {
                                id,
                                shape: desc.shape,
                            });
                        }
                        Ok(Wanted::Character(_)) => {}
                        Err(d) => {
                            self.fail(sync, id, d);
                            return false;
                        }
                    }
                }
                if link.written != local || link.parent != parent {
                    // A character only turned (root motion's yaw, ADR-0039,
                    // or any rotation-only write): it keeps interpolating,
                    // so the turn shows over one fixed step.
                    let turned = link.kind == LinkKind::Character
                        && link.parent == parent
                        && link.written.0 == local.0;
                    link.written = local;
                    link.parent = parent;
                    if turned {
                        link.cur.1 = rot;
                    } else if link.kind != LinkKind::Body(BodyKind::Kinematic) {
                        // A teleport, not motion: nothing to interpolate
                        // across. (A kinematic target is motion the next step
                        // performs.)
                        link.prev = (pos, rot);
                        link.cur = (pos, rot);
                    }
                    self.cmds.push(match link.kind {
                        LinkKind::Body(BodyKind::Kinematic) => {
                            PhysicsCmd::SetKinematicTarget { id, pos, rot }
                        }
                        _ => PhysicsCmd::SetTransform { id, pos, rot },
                    });
                }
                if library {
                    sync.library.insert(id);
                } else {
                    sync.library.remove(&id);
                }
            }
            existing => {
                let old = existing.map(|l| l.kind);
                // Only now is a library shape's data copied into the body.
                let w = match w.complete(world, entity, self.shapes) {
                    Ok(w) => w,
                    Err(d) => {
                        self.fail(sync, id, d);
                        return false;
                    }
                };
                if let Some(old) = old {
                    self.cmds.push(remove_cmd(id, old));
                }
                let kind = w.kind();
                self.added.insert(id);
                self.cmds.push(add_cmd(id, w, pos, rot));
                sync.insert_link(
                    id,
                    Link {
                        entity,
                        kind,
                        fingerprint: fp,
                        shape: shape_fp,
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
        sync.failed.remove(&id);
        true
    }
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

/// Adds, replaces and removes vehicles on linked bodies: `chassis` holds the
/// entities with a `Vehicle`, in id order. `added` holds the bodies this push
/// added or replaced: replacing a body drops its vehicle, so those get theirs
/// added again.
fn push_vehicles(
    world: &World,
    sync: &mut PhysicsSync,
    chassis: &[Entity],
    added: &BTreeSet<EntityId>,
    cmds: &mut Vec<PhysicsCmd>,
) {
    sync.mounts.retain(|&wheel, _| {
        entity_from_id(wheel).is_some_and(|e| world.ecs.get::<VehicleWheel>(e).is_some())
    });
    let mut seen: BTreeSet<EntityId> = BTreeSet::new();
    for &entity in chassis {
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
        let fp = vehicle_fingerprint(&desc);
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

    /// The samples of the one heightfield `cmds` sets on a body in place.
    fn reshaped_samples(cmds: &[PhysicsCmd]) -> Option<&[f32]> {
        match cmds {
            [
                PhysicsCmd::SetShape {
                    shape: ShapeDesc::Heightfield { samples, .. },
                    ..
                },
            ] => Some(samples),
            _ => None,
        }
    }

    /// An unchanged heightfield is not rebuilt on each push; new data, an
    /// equal copy and a new scale behave as they did when its samples were
    /// fingerprinted (new data and a new scale change the shape in place).
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
        // New data changes the shape.
        shapes.insert_heightfield("derived:t", field(2.0));
        let changed = push_once(&mut world, &shapes);
        assert_eq!(
            reshaped_samples(&changed),
            Some(&[2.0; 9][..]),
            "{changed:?}"
        );
        // So does a new scale.
        world.ecs.get_mut::<Transform>(e).unwrap().scale = Vec3::new(1.0, 2.0, 1.0);
        let scaled = push_once(&mut world, &shapes);
        assert_eq!(reshaped_samples(&scaled), Some(&[4.0; 9][..]), "{scaled:?}");
        assert!(push_once(&mut world, &shapes).is_empty());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// A body whose shape alone changes keeps its body, and so its motion and
    /// contacts: the push changes the shape in place, and moves the body too when
    /// its `Transform` moved. Any other change replaces the body, and so does a
    /// change of shape on a vehicle's chassis, which Jolt cannot reshape.
    #[test]
    fn a_change_of_shape_alone_keeps_the_body() {
        let mut world = World::new();
        let shapes = ShapeLibrary::default();
        let e = world
            .ecs
            .spawn((
                Transform::default(),
                RigidBody {
                    kind: SceneBodyKind::Dynamic,
                    ..Default::default()
                },
                Collider::default(),
            ))
            .id();
        let mut sync = PhysicsSync::new();
        let mut diagnostics = Vec::new();
        let mut push_once = |world: &mut World| {
            let mut cmds = Vec::new();
            push(world, &mut sync, &shapes, &mut cmds, &mut diagnostics);
            cmds
        };
        assert_eq!(adds(&push_once(&mut world)), 1);
        let boxed = |cmd: &PhysicsCmd| match cmd {
            PhysicsCmd::SetShape {
                shape: ShapeDesc::Box { half_extents },
                ..
            } => Some(*half_extents),
            _ => None,
        };
        // The collider's size.
        world.ecs.get_mut::<Collider>(e).unwrap().half_extents = Vec3::splat(0.25);
        let cmds = push_once(&mut world);
        assert_eq!(cmds.len(), 1, "{cmds:?}");
        assert_eq!(boxed(&cmds[0]), Some(Vec3::splat(0.25)), "{cmds:?}");
        // The entity's scale, and a move: the new shape at the new pose.
        {
            let mut t = world.ecs.get_mut::<Transform>(e).unwrap();
            t.scale = Vec3::splat(2.0);
            t.translation = Vec3::Y;
        }
        let cmds = push_once(&mut world);
        assert_eq!(cmds.len(), 2, "{cmds:?}");
        assert_eq!(boxed(&cmds[0]), Some(Vec3::splat(0.5)), "{cmds:?}");
        assert!(
            matches!(cmds[1], PhysicsCmd::SetTransform { pos, .. } if pos == Vec3::Y),
            "{cmds:?}"
        );
        assert!(push_once(&mut world).is_empty());
        // Anything else about the body replaces it.
        world.ecs.get_mut::<RigidBody>(e).unwrap().friction = 0.9;
        let cmds = push_once(&mut world);
        assert!(
            matches!(
                cmds.as_slice(),
                [PhysicsCmd::RemoveBody { .. }, PhysicsCmd::AddBody { .. }]
            ),
            "{cmds:?}"
        );
        // A chassis is replaced, and its vehicle added again.
        world.ecs.entity_mut(e).insert(Vehicle::default());
        let cmds = push_once(&mut world);
        assert!(
            matches!(cmds.as_slice(), [PhysicsCmd::AddVehicle { .. }]),
            "{cmds:?}"
        );
        world.ecs.get_mut::<Collider>(e).unwrap().half_extents = Vec3::splat(0.4);
        let cmds = push_once(&mut world);
        assert!(
            matches!(
                cmds.as_slice(),
                [
                    PhysicsCmd::RemoveBody { .. },
                    PhysicsCmd::AddBody { .. },
                    PhysicsCmd::AddVehicle { .. }
                ]
            ),
            "{cmds:?}"
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
        // A collider edit changes the shape in place.
        world.ecs.get_mut::<Collider>(e).unwrap().half_extents = Vec3::ONE;
        let edited = push_once(&mut world);
        assert!(
            matches!(edited.as_slice(), [PhysicsCmd::SetShape { .. }]),
            "{edited:?}"
        );
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

#[cfg(test)]
mod change_tests {
    //! The change-driven push against one that looks at every entity, and the
    //! structural fingerprints against the JSON ones they replaced.
    use super::*;

    /// A small deterministic random number generator (xorshift64*).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn chance(&mut self, percent: u64) -> bool {
            self.next() % 100 < percent
        }

        fn pick<T: Clone>(&mut self, items: &[T]) -> T {
            items[self.below(items.len())].clone()
        }

        fn unit(&mut self) -> f32 {
            (self.next() >> 40) as f32 / (1u64 << 24) as f32
        }

        fn vec3(&mut self, scale: f32) -> Vec3 {
            Vec3::new(self.unit() - 0.5, self.unit() - 0.5, self.unit() - 0.5) * 2.0 * scale
        }

        fn quat(&mut self) -> Quat {
            let axis = self.vec3(1.0).try_normalize().unwrap_or(Vec3::Y);
            Quat::from_axis_angle(axis, self.unit() * 6.0)
        }
    }

    /// Numbers whose JSON forms are equal or differ in every way that
    /// matters: signed zeros, two NaNs and the infinities (all `null`).
    const NUMBERS: [f32; 7] = [
        0.0,
        -0.0,
        1.5,
        f32::NAN,
        f32::from_bits(0x7fc0_0001),
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];

    /// The fingerprints `push` used to make, from each description's JSON.
    fn json_fingerprints(w: &Wanted) -> (blake3::Hash, blake3::Hash) {
        let (mut rest, mut shape) = (blake3::Hasher::new(), blake3::Hasher::new());
        match w {
            Wanted::Body(d, lib) => {
                let _ = serde_json::to_writer(&mut shape, &d.shape);
                if let Some(lib) = lib {
                    shape.update(&[lib.kind]);
                    shape.update(lib.content.as_bytes());
                    for c in lib.scale.to_array() {
                        shape.update(&c.to_le_bytes());
                    }
                }
                let d = BodyDesc {
                    shape: PENDING_SHAPE,
                    pos: Vec3::ZERO,
                    rot: Quat::IDENTITY,
                    ..d.clone()
                };
                let _ = serde_json::to_writer(&mut rest, &d);
            }
            Wanted::Character(d) => {
                let _ = serde_json::to_writer(&mut rest, d);
            }
        }
        (rest.finalize(), shape.finalize())
    }

    fn random_wanted(rng: &mut Rng) -> Wanted {
        let n = |rng: &mut Rng| rng.pick(&NUMBERS);
        if rng.chance(25) {
            let mut d = CharacterDesc::default();
            for _ in 0..rng.below(3) {
                match rng.below(8) {
                    0 => d.height = n(rng),
                    1 => d.radius = n(rng),
                    2 => d.max_slope_deg = n(rng),
                    3 => d.step_height = n(rng),
                    4 => d.mass = n(rng),
                    5 => d.inner_body = !d.inner_body,
                    6 => d.layer = Layer(rng.pick(&[0, 1, 256])),
                    _ => d.groups = GroupMask(rng.pick(&[1, 2, 257])),
                }
            }
            return Wanted::Character(d);
        }
        let mut d = BodyDesc::default();
        for _ in 0..rng.below(3) {
            match rng.below(11) {
                0 => d.kind = rng.pick(&[BodyKind::Static, BodyKind::Dynamic, BodyKind::Kinematic]),
                1 => d.mass = n(rng),
                2 => d.friction = n(rng),
                3 => d.restitution = n(rng),
                4 => d.linear_damping = n(rng),
                5 => d.angular_damping = n(rng),
                6 => d.layer = Layer(rng.pick(&[0, 1, 256])),
                7 => d.groups = GroupMask(rng.pick(&[1, 2, 257])),
                8 => d.sensor = !d.sensor,
                9 => d.center_of_mass_offset = Vec3::new(n(rng), n(rng), 0.0),
                _ => d.pos = Vec3::new(n(rng), 1.0, 2.0),
            }
        }
        let lib = rng.chance(30).then(|| LibraryShape {
            kind: rng.pick(&[0, 1, 2]),
            content: blake3::hash(&[rng.pick(&[0u8, 1])]),
            scale: Vec3::new(n(rng), 1.0, 1.0),
        });
        d.shape = match (lib, rng.below(3)) {
            (Some(_), _) => PENDING_SHAPE,
            (None, 0) => ShapeDesc::Box {
                half_extents: Vec3::new(n(rng), n(rng), 0.5),
            },
            (None, 1) => ShapeDesc::Sphere { radius: n(rng) },
            (None, _) => ShapeDesc::Capsule {
                half_height: n(rng),
                radius: 0.25,
            },
        };
        Wanted::Body(d, lib)
    }

    fn random_vehicle(rng: &mut Rng) -> VehicleDesc {
        let n = |rng: &mut Rng| rng.pick(&NUMBERS);
        let mut d = VehicleDesc {
            wheels: vec![WheelDesc::default(); rng.below(3)],
            ..VehicleDesc::default()
        };
        for _ in 0..rng.below(3) {
            match rng.below(9) {
                0 => d.kind = VehicleKind::Motorcycle,
                1 => {
                    if let Some(w) = d.wheels.first_mut() {
                        w.radius = n(rng);
                        w.driven = rng.chance(50);
                    }
                }
                2 => {
                    if let Some(w) = d.wheels.last_mut() {
                        w.pos = Vec3::new(n(rng), 0.0, 1.0);
                    }
                }
                3 => d.engine.max_torque = n(rng),
                4 => d.transmission.gears.push(n(rng)),
                5 => d.transmission.reverse_gears.clear(),
                6 => d.anti_roll_stiffness = n(rng),
                7 => d.tester = WheelTester::CastSphere { radius: n(rng) },
                _ => d.tester = WheelTester::CastCylinder,
            }
        }
        d
    }

    /// Two descriptions get equal fingerprints exactly when their JSON did.
    #[test]
    fn fingerprints_tell_apart_what_the_json_ones_did() {
        let mut rng = Rng(0x5eed);
        let wanted: Vec<_> = (0..600).map(|_| random_wanted(&mut rng)).collect();
        let prints: Vec<_> = wanted
            .iter()
            .map(|w| (json_fingerprints(w), fingerprints(w)))
            .collect();
        let mut equal = 0;
        for (i, (json_a, new_a)) in prints.iter().enumerate() {
            for (j, (json_b, new_b)) in prints.iter().enumerate() {
                assert_eq!(
                    json_a.0 == json_b.0,
                    new_a.0 == new_b.0,
                    "{:?} and {:?}",
                    wanted[i],
                    wanted[j]
                );
                assert_eq!(
                    json_a.1 == json_b.1,
                    new_a.1 == new_b.1,
                    "the shapes of {:?} and {:?}",
                    wanted[i],
                    wanted[j]
                );
                equal += usize::from(i != j && new_a == new_b);
            }
        }
        assert!(equal > 1000, "only {equal} equal pairs");
        let vehicles: Vec<_> = (0..300).map(|_| random_vehicle(&mut rng)).collect();
        let prints: Vec<_> = vehicles
            .iter()
            .map(|v| {
                let json = blake3::hash(serde_json::to_string(v).unwrap().as_bytes());
                (json, vehicle_fingerprint(v))
            })
            .collect();
        let mut equal = 0;
        for (i, a) in prints.iter().enumerate() {
            for (j, b) in prints.iter().enumerate() {
                assert_eq!(
                    a.0 == b.0,
                    a.1 == b.1,
                    "{:?} and {:?}",
                    vehicles[i],
                    vehicles[j]
                );
                equal += usize::from(i != j && a.1 == b.1);
            }
        }
        assert!(equal > 500, "only {equal} equal vehicle pairs");
    }

    impl std::fmt::Debug for Wanted {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Wanted::Body(d, lib) => write!(f, "{d:?} {lib:?}"),
                Wanted::Character(d) => write!(f, "{d:?}"),
            }
        }
    }

    /// One World with its sync state, edited at random.
    struct Side {
        world: World,
        shapes: ShapeLibrary,
        sync: PhysicsSync,
        entities: Vec<Entity>,
        rng: Rng,
        /// Look at every entity on each push, as `push` did before it
        /// followed changes.
        everything: bool,
    }

    fn mesh(points: &[[f32; 3]]) -> Arc<MeshData> {
        Arc::new(MeshData {
            vertices: points
                .iter()
                .map(|&pos| re_types::Vertex {
                    pos,
                    ..Default::default()
                })
                .collect(),
            indices: vec![0, 1, 2, 0, 2, 3],
        })
    }

    fn heightfield(h: f32) -> Arc<HeightfieldData> {
        Arc::new(HeightfieldData {
            n: 3,
            spacing: 1.0,
            offset: Vec3::ZERO,
            samples: vec![h; 9],
            precision: None,
        })
    }

    impl Side {
        fn new(seed: u64, everything: bool) -> Side {
            let mut shapes = ShapeLibrary::default();
            shapes.insert_mesh(
                "m0",
                mesh(&[[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
            );
            shapes.insert_heightfield("h0", heightfield(0.0));
            Side {
                world: World::new(),
                shapes,
                sync: PhysicsSync::new(),
                entities: Vec::new(),
                rng: Rng(seed),
                everything,
            }
        }

        fn random_collider(&mut self) -> Collider {
            let rng = &mut self.rng;
            Collider {
                shape: rng.pick(&[
                    ColliderShape::Box,
                    ColliderShape::Box,
                    ColliderShape::Sphere,
                    ColliderShape::Capsule,
                    ColliderShape::Convex,
                    ColliderShape::Mesh,
                    ColliderShape::Heightfield,
                ]),
                half_extents: Vec3::splat(rng.pick(&[0.5, 1.0])),
                source: rng.pick(&["m0", "m1", "h0", "", "gone"]).to_string(),
                layer: rng.pick(&[0, 0, 0, 2, -1]),
                sensor: rng.chance(10),
                enabled: !rng.chance(10),
                ..Collider::default()
            }
        }

        fn random_body(&mut self) -> RigidBody {
            RigidBody {
                kind: self.rng.pick(&[
                    SceneBodyKind::Static,
                    SceneBodyKind::Dynamic,
                    SceneBodyKind::Kinematic,
                ]),
                friction: self.rng.pick(&[0.5, 0.9]),
                ..RigidBody::default()
            }
        }

        fn random_entity(&mut self) -> Option<Entity> {
            (!self.entities.is_empty()).then(|| self.rng.pick(&self.entities))
        }

        fn spawn(&mut self) {
            let transform = Transform {
                translation: self.rng.vec3(5.0),
                rotation: self.rng.quat(),
                scale: self.rng.pick(&[
                    Vec3::ONE,
                    Vec3::ONE,
                    Vec3::splat(2.0),
                    Vec3::new(1.0, 2.0, 0.5),
                ]),
            };
            let mut e = self.world.ecs.spawn(transform);
            let id = e.id();
            if self.rng.chance(70) {
                let c = self.random_collider();
                self.world.ecs.entity_mut(id).insert(c);
            }
            if self.rng.chance(40) {
                let b = self.random_body();
                self.world.ecs.entity_mut(id).insert(b);
            }
            if self.rng.chance(10) {
                self.world
                    .ecs
                    .entity_mut(id)
                    .insert(CharacterController::default());
            }
            if self.rng.chance(40)
                && let Some(parent) = self.random_entity()
            {
                self.world.ecs.entity_mut(id).insert(ChildOf(parent));
            }
            if self.rng.chance(5) {
                e = self.world.ecs.entity_mut(id);
                e.insert((
                    RigidBody {
                        kind: SceneBodyKind::Dynamic,
                        ..RigidBody::default()
                    },
                    Vehicle::default(),
                ));
                for x in [-1.0, 1.0] {
                    let wheel = self
                        .world
                        .ecs
                        .spawn((
                            Transform::from_translation(Vec3::new(x, -0.5, 1.0)),
                            VehicleWheel::default(),
                            ChildOf(id),
                        ))
                        .id();
                    self.entities.push(wheel);
                }
            }
            self.entities.push(id);
        }

        /// True when `ancestor` is `e` or above it.
        fn is_above(&self, ancestor: Entity, mut e: Entity) -> bool {
            loop {
                if e == ancestor {
                    return true;
                }
                match hierarchy::parent(&self.world.ecs, e) {
                    Some(p) => e = p,
                    None => return false,
                }
            }
        }

        /// A few random edits, as scripts, the editor, streaming and the
        /// library make them.
        fn edit(&mut self) {
            for _ in 0..1 + self.rng.below(4) {
                let roll = self.rng.below(100);
                if roll < 25 || self.entities.is_empty() {
                    if self.entities.len() < 80 {
                        self.spawn();
                    }
                    continue;
                }
                let e = self.random_entity().expect("not empty");
                let world = &mut self.world.ecs;
                match roll {
                    25..40 => {
                        if let Some(mut c) = world.get_mut::<Collider>(e) {
                            match self.rng.below(7) {
                                0 => c.half_extents *= 1.5,
                                1 => c.enabled = !c.enabled,
                                2 => c.layer = self.rng.pick(&[0, 3, -1, 70_000]),
                                3 => c.groups = self.rng.pick(&[1, 6, -5]),
                                4 => c.sensor = !c.sensor,
                                5 => c.source = self.rng.pick(&["m0", "m1", "h0", "gone"]).into(),
                                _ => {
                                    c.shape = self.rng.pick(&[
                                        ColliderShape::Box,
                                        ColliderShape::Sphere,
                                        ColliderShape::Convex,
                                        ColliderShape::Heightfield,
                                    ])
                                }
                            }
                        }
                    }
                    40..50 => match self.rng.below(3) {
                        0 => {
                            let b = self.random_body();
                            self.world.ecs.entity_mut(e).insert(b);
                        }
                        1 => {
                            world.entity_mut(e).remove::<RigidBody>();
                        }
                        _ => {
                            if let Some(mut b) = world.get_mut::<RigidBody>(e) {
                                b.mass += 1.0;
                            }
                        }
                    },
                    50..54 => {
                        if world.get::<CharacterController>(e).is_some() {
                            world.entity_mut(e).remove::<CharacterController>();
                        } else {
                            world.entity_mut(e).insert(CharacterController::default());
                        }
                    }
                    54..59 => {
                        if world.get::<Collider>(e).is_some() {
                            world.entity_mut(e).remove::<Collider>();
                        } else {
                            let c = self.random_collider();
                            self.world.ecs.entity_mut(e).insert(c);
                        }
                    }
                    59..77 => {
                        let (v, q) = (self.rng.vec3(1.0), self.rng.quat());
                        let which = self.rng.below(4);
                        if let Some(mut t) = world.get_mut::<Transform>(e) {
                            match which {
                                0 | 1 => t.translation += v,
                                2 => t.rotation = q,
                                _ => t.scale = Vec3::splat(1.0 + v.x.abs()),
                            }
                        }
                    }
                    77..83 => {
                        let parent = self.random_entity().expect("not empty");
                        if self.rng.chance(30) {
                            self.world.ecs.entity_mut(e).remove::<ChildOf>();
                        } else if !self.is_above(e, parent) {
                            self.world.ecs.entity_mut(e).insert(ChildOf(parent));
                        }
                    }
                    83..89 => {
                        world.despawn(e);
                        let world = &self.world.ecs;
                        self.entities.retain(|&e| world.get_entity(e).is_ok());
                    }
                    89..92 => {
                        if world.get::<GlobalTransform>(e).is_some() {
                            world.entity_mut(e).remove::<GlobalTransform>();
                        } else {
                            world.entity_mut(e).insert(GlobalTransform::default());
                        }
                    }
                    92..93 => {
                        world.entity_mut(e).remove::<Transform>();
                    }
                    93..97 => match self.rng.below(3) {
                        0 => self.shapes.insert_mesh(
                            "m1",
                            mesh(&[[0.0; 3], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0]]),
                        ),
                        1 => self.shapes.remove("h0"),
                        _ => self
                            .shapes
                            .insert_heightfield("h0", heightfield(self.rng.unit())),
                    },
                    _ => {
                        // Twice drops every removal message sent before.
                        world.clear_trackers();
                        if self.rng.chance(50) {
                            world.clear_trackers();
                        }
                    }
                }
            }
            if self.rng.chance(50) {
                self.world.propagate();
            }
        }

        /// One push, with what it queued and reported.
        fn push(&mut self) -> (Vec<String>, Vec<String>) {
            if self.everything {
                self.sync.watch = Watch(None);
            }
            let (mut cmds, mut diagnostics) = (Vec::new(), Vec::new());
            push(
                &mut self.world,
                &mut self.sync,
                &self.shapes,
                &mut cmds,
                &mut diagnostics,
            );
            PhysicsCmd::sort(&mut cmds);
            (
                cmds.iter().map(|c| format!("{c:?}")).collect(),
                diagnostics.iter().map(|d| format!("{d:?}")).collect(),
            )
        }

        /// Physics moving every moving body a little.
        fn step(&mut self) {
            self.sync.begin_step();
            let poses: Vec<BodyPose> = self
                .sync
                .linked()
                .filter(|(_, kind)| *kind != LinkKind::Body(BodyKind::Static))
                .map(|(id, _)| BodyPose {
                    id,
                    pos: self.rng.vec3(3.0),
                    rot: self.rng.quat(),
                })
                .collect();
            if self.rng.chance(70) {
                pull(&mut self.world, &mut self.sync, &poses);
            }
        }

        fn state(&self) -> Vec<String> {
            self.sync
                .linked()
                .map(|(id, kind)| format!("{id:?} {kind:?} {:?}", self.sync.interpolated(id, 0.5)))
                .collect()
        }
    }

    fn push_once(world: &mut World, sync: &mut PhysicsSync) -> Vec<PhysicsCmd> {
        let (mut cmds, mut diagnostics) = (Vec::new(), Vec::new());
        push(
            world,
            sync,
            &ShapeLibrary::default(),
            &mut cmds,
            &mut diagnostics,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        cmds
    }

    /// A static body under a parent follows the parent once the World is
    /// propagated: the push sees the parent's `GlobalTransform` change, not the
    /// child's own components.
    #[test]
    fn a_static_body_follows_its_parent_once_propagated() {
        let mut world = World::new();
        let parent = world.ecs.spawn(Transform::default()).id();
        world.ecs.spawn((
            Transform::from_translation(Vec3::X),
            Collider::default(),
            ChildOf(parent),
        ));
        world.propagate();
        let mut sync = PhysicsSync::new();
        assert!(matches!(
            push_once(&mut world, &mut sync)[..],
            [PhysicsCmd::AddBody { .. }]
        ));
        assert!(push_once(&mut world, &mut sync).is_empty());
        world.ecs.get_mut::<Transform>(parent).unwrap().translation = Vec3::Y;
        // Until propagation, the parent's world pose is what it was.
        assert!(push_once(&mut world, &mut sync).is_empty());
        world.propagate();
        let cmds = push_once(&mut world, &mut sync);
        assert!(
            matches!(cmds[..], [PhysicsCmd::SetTransform { pos, .. }] if pos == Vec3::new(1.0, 1.0, 0.0)),
            "{cmds:?}"
        );
        assert!(push_once(&mut world, &mut sync).is_empty());
    }

    /// Removals the push did not read (the World's trackers cleared twice
    /// since) make it look at every entity, so a despawned body still goes.
    #[test]
    fn missed_removals_make_the_push_look_at_everything() {
        let mut world = World::new();
        let e = world
            .ecs
            .spawn((Transform::default(), Collider::default()))
            .id();
        let kept = world
            .ecs
            .spawn((Transform::default(), Collider::default()))
            .id();
        let mut sync = PhysicsSync::new();
        assert_eq!(push_once(&mut world, &mut sync).len(), 2);
        world.ecs.despawn(e);
        world.ecs.clear_trackers();
        world.ecs.clear_trackers();
        let cmds = push_once(&mut world, &mut sync);
        assert!(
            matches!(cmds[..], [PhysicsCmd::RemoveBody { id }] if id == entity_id(e)),
            "{cmds:?}"
        );
        assert_eq!(
            sync.kind(entity_id(kept)),
            Some(LinkKind::Body(BodyKind::Static))
        );
        assert!(push_once(&mut world, &mut sync).is_empty());
    }

    /// The same when a parent's `GlobalTransform` went: its static child is
    /// put where its parent's pose now puts it.
    #[test]
    fn a_missed_parent_pose_removal_makes_the_push_look_at_everything() {
        let mut world = World::new();
        let parent = world.ecs.spawn(Transform::from_translation(Vec3::Y)).id();
        let child = world
            .ecs
            .spawn((Transform::default(), Collider::default(), ChildOf(parent)))
            .id();
        world.propagate();
        let mut sync = PhysicsSync::new();
        assert_eq!(push_once(&mut world, &mut sync).len(), 1);
        world.ecs.entity_mut(parent).remove::<GlobalTransform>();
        world.ecs.clear_trackers();
        world.ecs.clear_trackers();
        let cmds = push_once(&mut world, &mut sync);
        assert!(
            matches!(cmds[..], [PhysicsCmd::SetTransform { id, pos, .. }]
                if id == entity_id(child) && pos == Vec3::ZERO),
            "{cmds:?}"
        );
        assert!(push_once(&mut world, &mut sync).is_empty());
    }

    /// A moving body reparented where it stands (`e:set_parent(p)` keeps the
    /// world pose by default, ADR-0057 §3) stays there and keeps its velocity:
    /// the push sees the new parent and local transform and teleports the body
    /// to the pose it has, which keeps its motion.
    #[test]
    fn a_body_reparented_in_place_keeps_its_pose_and_velocity() {
        let mut world = World::new();
        let a = world
            .ecs
            .spawn(Transform::from_translation(Vec3::new(1.0, 0.0, 0.0)))
            .id();
        let b = world
            .ecs
            .spawn(Transform {
                translation: Vec3::new(-3.0, 2.0, 0.5),
                rotation: Quat::from_rotation_y(0.7),
                ..Transform::default()
            })
            .id();
        let e = world
            .ecs
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 5.0, 0.0)),
                RigidBody {
                    kind: SceneBodyKind::Dynamic,
                    ..RigidBody::default()
                },
                Collider::default(),
                ChildOf(a),
            ))
            .id();
        world.propagate();
        let config = re_physics::PhysicsConfig {
            gravity: Vec3::ZERO,
            ..re_physics::PhysicsConfig::default()
        };
        let mut physics = re_physics::Physics::new(config, re_core::TaskPool::global()).unwrap();
        let (mut sync, shapes) = (PhysicsSync::new(), ShapeLibrary::default());
        let (mut cmds, mut diagnostics, mut poses) = (Vec::new(), Vec::new(), Vec::new());
        let id = entity_id(e);
        for step in 0..4 {
            sync.begin_step();
            push(&mut world, &mut sync, &shapes, &mut cmds, &mut diagnostics);
            if step == 0 {
                cmds.push(PhysicsCmd::SetVelocity {
                    id,
                    linear: Vec3::new(2.0, 0.0, 1.0),
                    angular: Vec3::new(0.0, 0.5, 0.0),
                });
            }
            physics.apply(&mut cmds);
            physics.step(1.0 / 60.0).unwrap();
            physics.poses_into(&mut poses);
            pull(&mut world, &mut sync, &poses);
            world.propagate();
        }
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let (pos, rot) = physics.pose(id).unwrap();
        let velocity = physics.velocity(id).unwrap();
        assert!(velocity.0.length() > 1.0, "{velocity:?}");
        // What `set_parent` does with the world pose kept.
        let local =
            world.ecs.get::<GlobalTransform>(b).unwrap().0.inverse() * world_affine(&world, e);
        let (_, r, t) = local.to_scale_rotation_translation();
        {
            let mut tr = world.ecs.get_mut::<Transform>(e).unwrap();
            tr.translation = t;
            tr.rotation = r;
        }
        world.ecs.entity_mut(e).insert(ChildOf(b));
        push(&mut world, &mut sync, &shapes, &mut cmds, &mut diagnostics);
        let [
            PhysicsCmd::SetTransform {
                pos: to,
                rot: turned,
                ..
            },
        ] = cmds[..]
        else {
            panic!("{cmds:?}");
        };
        assert!(to.abs_diff_eq(pos, 1e-5), "{to} {pos}");
        assert!(turned.abs_diff_eq(rot, 1e-5), "{turned} {rot}");
        physics.apply(&mut cmds);
        assert_eq!(physics.velocity(id), Some(velocity));
        let (now, _) = physics.pose(id).unwrap();
        assert!(now.abs_diff_eq(pos, 1e-5), "{now} {pos}");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// The shape and pose an outline draws are the ones physics gets: a scaled
    /// collider at the entity's world pose, and a character's capsule.
    #[test]
    fn shapes_and_poses_are_what_physics_gets() {
        let mut world = World::new();
        let parent = world
            .ecs
            .spawn(Transform::from_translation(Vec3::new(0.0, 2.0, 0.0)))
            .id();
        let body = world
            .ecs
            .spawn((
                Transform {
                    translation: Vec3::X,
                    scale: Vec3::new(2.0, 1.0, 1.0),
                    ..Transform::default()
                },
                Collider::default(),
                ChildOf(parent),
            ))
            .id();
        let character = world
            .ecs
            .spawn((
                Transform::from_translation(Vec3::Z),
                CharacterController::default(),
            ))
            .id();
        let none = world.ecs.spawn(Transform::default()).id();
        world.propagate();
        let shapes = ShapeLibrary::default();
        let (shape, pos, _) = shape_and_pose(&world, body, &shapes).unwrap();
        assert_eq!(
            shape,
            ShapeDesc::Box {
                half_extents: Vec3::new(1.0, 0.5, 0.5)
            }
        );
        assert_eq!(pos, Vec3::new(1.0, 2.0, 0.0));
        let (shape, pos, _) = shape_and_pose(&world, character, &shapes).unwrap();
        let c = CharacterController::default();
        assert_eq!(
            shape,
            ShapeDesc::Capsule {
                half_height: c.height * 0.5 - c.radius,
                radius: c.radius
            }
        );
        assert_eq!(pos, Vec3::Z);
        assert_eq!(shape_and_pose(&world, none, &shapes), None);
        // The same shape the push adds.
        let mut sync = PhysicsSync::new();
        let cmds = push_once(&mut world, &mut sync);
        assert!(
            cmds.iter().any(|c| matches!(c, PhysicsCmd::AddBody { desc, .. }
                if Some((desc.shape.clone(), desc.pos, desc.rot)) == shape_and_pose(&world, body, &shapes))),
            "{cmds:?}"
        );
    }

    /// `added` gathers what a step's pushes added, until the step ends.
    #[test]
    fn added_holds_a_steps_new_bodies() {
        let mut world = World::new();
        let a = world
            .ecs
            .spawn((Transform::default(), Collider::default()))
            .id();
        let mut sync = PhysicsSync::new();
        push_once(&mut world, &mut sync);
        let b = world
            .ecs
            .spawn((Transform::default(), Collider::default()))
            .id();
        push_once(&mut world, &mut sync);
        assert_eq!(sync.added(), &BTreeSet::from([entity_id(a), entity_id(b)]));
        // A body removed again within the step is not among them.
        world.ecs.despawn(b);
        push_once(&mut world, &mut sync);
        assert_eq!(sync.added(), &BTreeSet::from([entity_id(a)]));
        sync.end_step();
        assert!(sync.added().is_empty());
        push_once(&mut world, &mut sync);
        assert!(sync.added().is_empty());
    }

    /// Whatever changes, the push that follows changes queues and reports what
    /// a push looking at every entity does.
    #[test]
    fn the_push_follows_changes_as_a_look_at_everything_does() {
        let mut commands = 0;
        for seed in 1..=12u64 {
            let mut changes = Side::new(seed, false);
            let mut everything = Side::new(seed, true);
            for round in 0..250 {
                changes.edit();
                everything.edit();
                let a = changes.push();
                let b = everything.push();
                assert_eq!(a, b, "seed {seed}, round {round}");
                commands += a.0.len();
                changes.step();
                everything.step();
                assert_eq!(
                    changes.state(),
                    everything.state(),
                    "seed {seed}, round {round}"
                );
            }
        }
        assert!(commands > 2000, "only {commands} commands");
    }
}
