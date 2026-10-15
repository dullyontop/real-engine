//! [`Physics`]: the engine's physics world, keyed by [`EntityId`].
//!
//! Jolt names bodies by `BodyId` (an index plus a sequence number); the engine names
//! them by entity. `Physics` keeps both directions: entity → body in its body table,
//! and body → entity in a map that outlives a removal by one step, because Jolt can
//! report a contact of a body after the body is gone (ADR-0008).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use glam::{Quat, Vec3};
use oxijolt::{
    Activation, ActivationEvent, AnyConstraintId, BodyId, BodySettings, BroadPhaseLayer,
    CharacterContact, CharacterContactListener, CharacterId, CharacterSettings, CollisionLayers,
    ContactEvent as JContactEvent, DistanceConstraintSettings, DistanceRange,
    ExtendedUpdateSettings, FixedConstraintSettings, GroundState as JGroundState,
    HingeConstraintSettings, InnerBody, ObjectLayer, PhysicsWorld, QueryFilter as JQueryFilter,
    Shape, WorldSettings, WorldState,
};
use re_core::{Diagnostic, Location, Severity, TaskPool};
use re_types::EntityId;
use re_types::physics::{
    BodyDesc, BodyKind, BodyPose, CharacterDesc, CharacterState, ConstraintDesc, ConstraintKind,
    ContactEvent, GroundState, GroupMask, Layer, PhysicsCmd,
};

use crate::codes;
use crate::config::{LayerTable, PhysicsConfig};
use crate::convert::{gq, grv, gv, jq, jv, rv, unit_rotation};
use crate::jobs::JoltRayonJobs;
use crate::parts::{PartGate, PartListener, PartsState};
use crate::shapes;

/// The engine's physics world: Jolt behind a command-buffer API.
///
/// One frame of use, in order:
///
/// 1. [`apply`](Self::apply) the frame's [`PhysicsCmd`]s (they are sorted first, so the
///    result does not depend on the order scripts issued them);
/// 2. [`step`](Self::step) once per fixed step;
/// 3. read [`poses_into`](Self::poses_into), [`events`](Self::events),
///    [`character`](Self::character) and, for the few entities that need it,
///    [`velocity`](Self::velocity);
/// 4. collect [`take_diagnostics`](Self::take_diagnostics).
///
/// Jolt's jobs run on the shared [`TaskPool`]; physics starts no threads. Given the
/// same commands in the same order, two worlds on one machine produce identical
/// poses bit for bit, whatever the number of worker threads.
pub struct Physics {
    pub(crate) world: PhysicsWorld,
    jobs: Arc<JoltRayonJobs>,
    pub(crate) config: PhysicsConfig,
    pub(crate) bodies: BTreeMap<EntityId, BodyEntry>,
    /// Body → entity, including bodies removed since the last step (see the module docs)
    /// and characters' inner bodies.
    pub(crate) entity_of: HashMap<BodyId, EntityId>,
    pub(crate) characters: BTreeMap<EntityId, CharacterEntry>,
    constraints: BTreeMap<EntityId, ConstraintEntry>,
    gate: Arc<GroupGate>,
    state: StepState,
    removed: Vec<BodyId>,
    diagnostics: Vec<Diagnostic>,
    scratch_poses: Vec<oxijolt::BodyPose>,
    pub(crate) parts: PartsState,
}

/// What the world knows about one body.
#[derive(Clone, Debug)]
pub(crate) struct BodyEntry {
    pub(crate) id: BodyId,
    pub(crate) kind: BodyKind,
    pub(crate) mass: f32,
    pub(crate) groups: GroupMask,
    pub(crate) sensor: bool,
}

pub(crate) struct CharacterEntry {
    id: CharacterId,
    inner: Option<BodyId>,
    pub(crate) groups: GroupMask,
    step_height: f32,
    /// The object layers the character collides with, for its query filter.
    sees: Vec<ObjectLayer>,
}

struct ConstraintEntry {
    id: AnyConstraintId,
    a: EntityId,
    b: EntityId,
}

/// A touching pair of bodies, counted over their touching sub-shapes.
#[derive(Clone, Copy, Debug)]
struct Pair {
    sub_contacts: u32,
    a: EntityId,
    b: EntityId,
    sensor: bool,
}

/// The state that changes from step to step and goes into a [`PhysicsSnapshot`].
#[derive(Clone, Default)]
struct StepState {
    pairs: HashMap<(BodyId, BodyId), Pair>,
    events: Vec<ContactEvent>,
    /// Events produced between steps (removals), delivered by the next step.
    pending_events: Vec<ContactEvent>,
    /// Kinematic targets set since the last step.
    kin_targets: BTreeMap<EntityId, (Vec3, Quat)>,
    /// Kinematic bodies moved toward a target in the last step.
    kin_moved: BTreeSet<EntityId>,
    /// Character moves requested since the last step.
    moves: BTreeMap<EntityId, (Vec3, bool)>,
    /// Each character's actual velocity over the last step (its displacement / dt).
    character_velocity: BTreeMap<EntityId, Vec3>,
    /// Bodies that fell asleep in the last step: their final pose is reported once.
    slept: Vec<BodyId>,
}

/// A saved physics state, from [`Physics::save_state`].
///
/// It restores only into the world that saved it, and only while no body, character
/// or constraint was added or removed (and no shape replaced) in between. To carry
/// motion into another world (a fresh copy of the scene), use
/// [`Physics::motion_state`] and [`Physics::apply_motion_state`].
#[derive(Clone)]
pub struct PhysicsSnapshot {
    world: WorldState,
    state: StepState,
}

impl std::fmt::Debug for PhysicsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicsSnapshot")
            .field("world", &self.world)
            .finish_non_exhaustive()
    }
}

/// The motion of one dynamic or kinematic body, in a [`MotionState`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyMotion {
    /// The entity the body belongs to.
    pub id: EntityId,
    /// The body's position (its shape origin).
    pub pos: Vec3,
    /// The body's rotation.
    pub rot: Quat,
    /// Linear velocity, m/s.
    pub linear: Vec3,
    /// Angular velocity, rad/s.
    pub angular: Vec3,
    /// Whether the body was awake.
    pub awake: bool,
}

#[derive(Clone, Debug)]
struct CharacterMotion {
    id: EntityId,
    velocity: Vec3,
    /// Jolt's character state (pose, velocity, ground and contacts), which restores into
    /// a character of any world.
    jolt: oxijolt::CharacterState,
}

/// Entity-keyed motion, from [`Physics::motion_state`], that seeds another world with
/// [`Physics::apply_motion_state`].
#[derive(Clone, Debug, Default)]
pub struct MotionState {
    bodies: Vec<BodyMotion>,
    characters: Vec<CharacterMotion>,
}

impl MotionState {
    /// The dynamic and kinematic bodies, sorted by entity.
    pub fn bodies(&self) -> &[BodyMotion] {
        &self.bodies
    }

    /// The characters' entities and velocities (m/s, over their last step), sorted by
    /// entity.
    pub fn characters(&self) -> impl Iterator<Item = (EntityId, Vec3)> + '_ {
        self.characters.iter().map(|c| (c.id, c.velocity))
    }
}

/// Filters what characters collide with by group mask (Jolt asks it during
/// `update_character`; it never touches the world).
/// A character meets an assembly's part only when the part collides and the
/// character's group bits include the part's collision group.
struct GroupGate {
    /// Raw body id → group bits.
    masks: RwLock<HashMap<u32, u32>>,
    /// The group bits of the character being updated.
    current: AtomicU32,
    /// The assemblies' parts.
    parts: Arc<PartGate>,
}

impl GroupGate {
    fn set(&self, body: BodyId, groups: GroupMask) {
        self.masks
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(body.to_raw(), groups.0);
    }

    fn forget(&self, body: BodyId) {
        self.masks
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&body.to_raw());
    }
}

impl CharacterContactListener for GroupGate {
    fn contact_validate(&self, _character: CharacterId, contact: &CharacterContact) -> bool {
        let Some(body) = contact.body else {
            return true;
        };
        let current = self.current.load(Ordering::Relaxed);
        if let Some(asm) = self.parts.body(body) {
            let Some(part) = asm.child(contact.sub_shape_id).map(|i| asm.parts[i]) else {
                return true;
            };
            return part.collide && part.group < 32 && current & (1 << part.group) != 0;
        }
        self.masks
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&body.to_raw())
            .is_none_or(|bits| bits & current != 0)
    }
}

/// The Jolt object layer of `layer` for static bodies (`moving == false`) or for bodies
/// that move.
pub(crate) fn object_layer(layer: Layer, moving: bool) -> ObjectLayer {
    ObjectLayer::new(u32::from(layer.0) * 2 + u32::from(moving))
}

fn collision_layers(table: &LayerTable) -> CollisionLayers {
    let mut layers = CollisionLayers::new(2);
    for _ in 0..table.count() {
        layers.add_object_layer(BroadPhaseLayer::new(0));
        layers.add_object_layer(BroadPhaseLayer::new(1));
    }
    for a in 0..table.count() {
        for b in 0..table.count() {
            let (la, lb) = (Layer(a), Layer(b));
            if a <= b && table.moving_collides(la, lb) {
                layers.enable_collision(object_layer(la, true), object_layer(lb, true));
            }
            if table.static_collides(la, lb) {
                layers.enable_collision(object_layer(la, true), object_layer(lb, false));
            }
        }
    }
    layers
}

/// How far from the simulation origin, per axis, a body may be (Jolt's
/// limit for single precision), in studs.
#[allow(clippy::unnecessary_cast)]
pub const WORLD_LIMIT: f32 = oxijolt::limits::MAX_POSITION as f32;

fn max_position() -> f32 {
    WORLD_LIMIT
}

pub(crate) fn in_world(p: Vec3) -> bool {
    p.is_finite() && p.abs().max_element() <= max_position()
}

fn at(id: EntityId) -> Location {
    Location::entity(id.to_string())
}

pub(crate) fn outside_world(id: EntityId, what: &str, p: Vec3) -> Diagnostic {
    Diagnostic::error(
        codes::OUTSIDE_WORLD,
        format!(
            "{what} position {p} is outside the world limits (±{} m)",
            max_position()
        ),
    )
    .at(at(id))
    .with_hint("keep the world within 5 km of the origin, or move the origin with the world")
}

fn unknown(id: EntityId, what: &str) -> Diagnostic {
    Diagnostic::error(codes::UNKNOWN_ENTITY, format!("entity {id} has no {what}"))
        .at(at(id))
        .with_hint("add the body or character before sending commands to it")
}

fn map_ground(g: JGroundState) -> GroundState {
    match g {
        JGroundState::OnGround => GroundState::OnGround,
        JGroundState::OnSteepGround => GroundState::OnSteepGround,
        JGroundState::NotSupported => GroundState::NotSupported,
        JGroundState::InAir => GroundState::InAir,
    }
}

impl Physics {
    /// Creates an empty world whose jobs run on `pool`.
    ///
    /// The pool must live as long as the world; the engine's pool is
    /// [`TaskPool::global`]. Fails with `RE5001` if Jolt refuses the configuration.
    pub fn new(config: PhysicsConfig, pool: &'static TaskPool) -> Result<Physics, Diagnostic> {
        let jobs = Arc::new(JoltRayonJobs::new(pool));
        let mut world = PhysicsWorld::new(
            WorldSettings::default()
                .layers(collision_layers(&config.layers))
                .gravity(jv(config.gravity))
                .max_bodies(config.max_bodies)
                .max_body_pairs(config.max_body_pairs)
                .max_contact_constraints(config.max_contact_constraints)
                .job_system(jobs.clone()),
        )
        .map_err(|e| {
            Diagnostic::error(
                codes::WORLD_CREATE_FAILED,
                format!("the physics world could not be created: {e}"),
            )
            .with_hint("check the PhysicsConfig limits and gravity")
        })?;
        world.set_event_settings(
            oxijolt::EventSettings::default()
                .contacts(true)
                .body_activation(true),
        );
        let parts = PartsState::default();
        world.set_contact_listener(Some(Arc::new(PartListener(parts.gate.clone()))));
        let gate = Arc::new(GroupGate {
            masks: RwLock::default(),
            current: AtomicU32::new(0),
            parts: parts.gate.clone(),
        });
        world.set_character_contact_listener(Some(gate.clone()));
        Ok(Physics {
            world,
            jobs,
            config,
            bodies: BTreeMap::new(),
            entity_of: HashMap::new(),
            characters: BTreeMap::new(),
            constraints: BTreeMap::new(),
            gate,
            state: StepState::default(),
            removed: Vec::new(),
            diagnostics: Vec::new(),
            scratch_poses: Vec::new(),
            parts,
        })
    }

    /// The configuration the world was created with.
    pub fn config(&self) -> &PhysicsConfig {
        &self.config
    }

    /// Sorts `commands` by `(kind, entity)`, applies them and leaves the vector empty
    /// (its capacity is kept).
    ///
    /// A command that fails is skipped and reported through
    /// [`take_diagnostics`](Self::take_diagnostics); the others still apply.
    pub fn apply(&mut self, commands: &mut Vec<PhysicsCmd>) {
        PhysicsCmd::sort(commands);
        for cmd in commands.drain(..) {
            if let Err(d) = self.apply_one(cmd) {
                tracing::debug!(target: "re_physics", "{}", d.render_human());
                self.diagnostics.push(d);
            }
        }
    }

    fn apply_one(&mut self, cmd: PhysicsCmd) -> Result<(), Diagnostic> {
        match cmd {
            PhysicsCmd::AddBody { id, desc } => self.add_body(id, &desc),
            PhysicsCmd::RemoveBody { id } => self.remove_body(id),
            PhysicsCmd::SetTransform { id, pos, rot } => self.set_transform(id, pos, rot),
            PhysicsCmd::SetKinematicTarget { id, pos, rot } => {
                let entry = self.bodies.get(&id).ok_or_else(|| unknown(id, "body"))?;
                if entry.kind != BodyKind::Kinematic {
                    return Err(wrong_kind(id, "a kinematic target", entry.kind));
                }
                if !in_world(pos) {
                    return Err(outside_world(id, "target", pos));
                }
                let rot = unit_rotation(rot).ok_or_else(|| bad_rotation(id))?;
                self.state.kin_targets.insert(id, (pos, rot));
                Ok(())
            }
            PhysicsCmd::ApplyImpulse { id, impulse } => {
                let entry = self.bodies.get(&id).ok_or_else(|| unknown(id, "body"))?;
                if entry.kind != BodyKind::Dynamic {
                    return Err(wrong_kind(id, "an impulse", entry.kind));
                }
                let body = entry.id;
                self.world
                    .body_mut(body)
                    .and_then(|mut b| b.add_impulse(jv(impulse)))
                    .map_err(|e| body_invalid(id, e))
            }
            PhysicsCmd::SetVelocity {
                id,
                linear,
                angular,
            } => {
                // A character takes the linear part: the next step starts from it
                // (an upward speed above the ground's makes it leave the ground).
                if !self.bodies.contains_key(&id)
                    && let Some(ch) = self.characters.get(&id)
                {
                    if !linear.is_finite() {
                        return Err(character_invalid(
                            id,
                            format!("velocity {linear} is not finite"),
                        ));
                    }
                    let cid = ch.id;
                    return self
                        .world
                        .character_mut(cid)
                        .and_then(|mut c| c.set_linear_velocity(jv(linear)))
                        .map_err(|e| character_invalid(id, e.to_string()));
                }
                let entry = self.bodies.get(&id).ok_or_else(|| unknown(id, "body"))?;
                if entry.kind == BodyKind::Static {
                    return Err(wrong_kind(id, "a velocity", entry.kind));
                }
                let body = entry.id;
                self.world
                    .body_mut(body)
                    .and_then(|mut b| {
                        b.set_linear_velocity(jv(linear))?;
                        b.set_angular_velocity(jv(angular))
                    })
                    .map_err(|e| body_invalid(id, e))
            }
            PhysicsCmd::SetShape { id, shape } => self.set_shape(id, &shape),
            PhysicsCmd::AddCharacter { id, desc, pos, rot } => {
                self.add_character(id, &desc, pos, rot)
            }
            PhysicsCmd::RemoveCharacter { id } => self.remove_character(id),
            PhysicsCmd::MoveCharacter {
                id,
                desired_velocity,
                jump,
            } => {
                if !self.characters.contains_key(&id) {
                    return Err(unknown(id, "character"));
                }
                if !desired_velocity.is_finite() {
                    return Err(Diagnostic::error(
                        codes::CHARACTER_INVALID,
                        format!(
                            "character {id}: desired velocity {desired_velocity} is not finite"
                        ),
                    )
                    .at(at(id)));
                }
                self.state.moves.insert(id, (desired_velocity, jump));
                Ok(())
            }
            PhysicsCmd::AddConstraint { id, desc } => self.add_constraint(id, &desc),
            PhysicsCmd::RemoveConstraint { id } => {
                let entry = self
                    .constraints
                    .remove(&id)
                    .ok_or_else(|| unknown(id, "constraint"))?;
                self.world
                    .remove_constraint(entry.id)
                    .map_err(|e| constraint_invalid(id, e.to_string()))
            }
        }
    }

    fn check_layer(&self, id: EntityId, layer: Layer) -> Result<(), Diagnostic> {
        if self.config.layers.contains(layer) {
            Ok(())
        } else {
            Err(Diagnostic::error(
                codes::UNKNOWN_LAYER,
                format!(
                    "layer {} is not in the layer table (layers 0 to {})",
                    layer.0,
                    self.config.layers.count() - 1
                ),
            )
            .at(at(id))
            .with_hint(
                "use a layer below the table's count, or add layers to PhysicsConfig::layers",
            ))
        }
    }

    fn add_body(&mut self, id: EntityId, desc: &BodyDesc) -> Result<(), Diagnostic> {
        if self.bodies.contains_key(&id) || self.characters.contains_key(&id) {
            return Err(Diagnostic::error(
                codes::DUPLICATE_ENTITY,
                format!("entity {id} already has a body or character"),
            )
            .at(at(id))
            .with_hint("remove the existing body first, or send SetShape to change its shape"));
        }
        self.check_layer(id, desc.layer)?;
        if desc.kind != BodyKind::Static && shapes::is_static_only(&desc.shape) {
            return Err(static_only(id, &desc.shape, desc.kind));
        }
        if !in_world(desc.pos) {
            return Err(outside_world(id, "body", desc.pos));
        }
        let rot = unit_rotation(desc.rot).ok_or_else(|| bad_rotation(id))?;
        let built = shapes::build(&desc.shape).map_err(|d| d.at(at(id)))?;
        if let Some(w) = built.warning {
            self.diagnostics.push(w.at(at(id)));
        }
        let moving = desc.kind != BodyKind::Static;
        let settings = match desc.kind {
            BodyKind::Static => BodySettings::new_static(),
            BodyKind::Dynamic => BodySettings::new_dynamic().mass(desc.mass),
            BodyKind::Kinematic => BodySettings::new_kinematic(),
        }
        .object_layer(object_layer(desc.layer, moving))
        .position(rv(desc.pos))
        .rotation(jq(rot))
        .friction(desc.friction)
        .restitution(desc.restitution)
        .linear_damping(desc.linear_damping)
        .angular_damping(desc.angular_damping)
        .sensor(desc.sensor)
        .user_data(id.to_bits());
        let body = self
            .world
            .create_body(&built.shape, &settings)
            .map_err(|e| body_invalid(id, e))?;
        self.entity_of.insert(body, id);
        self.gate.set(body, desc.groups);
        self.bodies.insert(
            id,
            BodyEntry {
                id: body,
                kind: desc.kind,
                mass: desc.mass,
                groups: desc.groups,
                sensor: desc.sensor,
            },
        );
        Ok(())
    }

    fn remove_body(&mut self, id: EntityId) -> Result<(), Diagnostic> {
        let entry = self.bodies.get(&id).ok_or_else(|| unknown(id, "body"))?;
        let body = entry.id;
        // Jolt refuses to remove a body a constraint holds: remove those constraints first.
        let attached: Vec<EntityId> = self
            .constraints
            .iter()
            .filter(|(_, c)| c.a == id || c.b == id)
            .map(|(cid, _)| *cid)
            .collect();
        for cid in attached {
            if let Some(c) = self.constraints.remove(&cid) {
                // The constraint exists (it is in the table), so this cannot fail.
                let _ = self.world.remove_constraint(c.id);
                self.diagnostics.push(
                    Diagnostic::warning(
                        codes::CONSTRAINT_REMOVED_WITH_BODY,
                        format!("constraint {cid} was removed because its body {id} was removed"),
                    )
                    .at(at(cid))
                    .with_hint("remove constraints before the bodies they join"),
                );
            }
        }
        self.world
            .remove_body(body)
            .map_err(|e| body_invalid(id, e))?;
        self.bodies.remove(&id);
        self.end_pairs_of(body);
        self.end_part_pairs_of_body(body);
        self.gate.forget(body);
        self.state.kin_targets.remove(&id);
        self.state.kin_moved.remove(&id);
        self.removed.push(body);
        Ok(())
    }

    /// Ends every contact of `body` now (it is being removed): the `End` events go out
    /// with the next step, and Jolt's own late `Removed` events for it are dropped.
    fn end_pairs_of(&mut self, body: BodyId) {
        let mut ended: Vec<((BodyId, BodyId), Pair)> = self
            .state
            .pairs
            .iter()
            .filter(|(k, _)| k.0 == body || k.1 == body)
            .map(|(k, p)| (*k, *p))
            .collect();
        ended.sort_by_key(|(_, p)| (p.a, p.b));
        for (key, pair) in ended {
            self.state.pairs.remove(&key);
            self.state.pending_events.push(ContactEvent::End {
                a: pair.a,
                b: pair.b,
                sensor: pair.sensor,
            });
        }
    }

    fn set_transform(&mut self, id: EntityId, pos: Vec3, rot: Quat) -> Result<(), Diagnostic> {
        if !in_world(pos) {
            return Err(outside_world(id, "new", pos));
        }
        let rot = unit_rotation(rot).ok_or_else(|| bad_rotation(id))?;
        if let Some(entry) = self.bodies.get(&id) {
            let activation = if entry.kind == BodyKind::Static {
                Activation::DontActivate
            } else {
                Activation::Activate
            };
            let body = entry.id;
            return self
                .world
                .body_mut(body)
                .and_then(|mut b| b.set_position_and_rotation(rv(pos), jq(rot), activation))
                .map_err(|e| body_invalid(id, e));
        }
        if let Some(ch) = self.characters.get(&id) {
            let cid = ch.id;
            let filter_layers = ch.sees.clone();
            self.gate.current.store(ch.groups.0, Ordering::Relaxed);
            let result = self.world.character_mut(cid).and_then(|mut c| {
                c.set_position(rv(pos))?;
                c.set_rotation(jq(rot))
            });
            return result
                .and_then(|()| {
                    self.world.refresh_character_contacts(
                        cid,
                        &JQueryFilter::new().object_layers(&filter_layers),
                    )
                })
                .map_err(|e| character_invalid(id, e.to_string()));
        }
        Err(unknown(id, "body or character"))
    }

    fn set_shape(
        &mut self,
        id: EntityId,
        shape: &re_types::physics::ShapeDesc,
    ) -> Result<(), Diagnostic> {
        let entry = self.bodies.get(&id).ok_or_else(|| unknown(id, "body"))?;
        if entry.kind != BodyKind::Static && shapes::is_static_only(shape) {
            return Err(static_only(id, shape, entry.kind));
        }
        let (body, kind, mass) = (entry.id, entry.kind, entry.mass);
        let built = shapes::build(shape).map_err(|d| d.at(at(id)))?;
        if let Some(w) = built.warning {
            self.diagnostics.push(w.at(at(id)));
        }
        let (mass, activation) = match kind {
            BodyKind::Dynamic => (Some(mass), Activation::Activate),
            BodyKind::Kinematic => (None, Activation::Activate),
            BodyKind::Static => (None, Activation::DontActivate),
        };
        self.world
            .body_mut(body)
            .and_then(|mut b| b.set_shape(&built.shape, mass, activation))
            .map_err(|e| body_invalid(id, e))
    }

    fn add_character(
        &mut self,
        id: EntityId,
        desc: &CharacterDesc,
        pos: Vec3,
        rot: Quat,
    ) -> Result<(), Diagnostic> {
        if self.bodies.contains_key(&id) || self.characters.contains_key(&id) {
            return Err(Diagnostic::error(
                codes::DUPLICATE_ENTITY,
                format!("entity {id} already has a body or character"),
            )
            .at(at(id)));
        }
        self.check_layer(id, desc.layer)?;
        let half = desc.height * 0.5 - desc.radius;
        let numbers_ok = desc.radius.is_finite()
            && desc.radius > 0.0
            && half.is_finite()
            && half > 0.0
            && desc.max_slope_deg.is_finite()
            && desc.max_slope_deg > 0.0
            && desc.max_slope_deg < 90.0
            && desc.step_height.is_finite()
            && desc.step_height >= 0.0
            && desc.mass.is_finite()
            && desc.mass > 0.0;
        if !numbers_ok {
            return Err(character_invalid(
                id,
                format!(
                    "height {} must exceed twice the radius {}, the radius, mass and step height \
                     must be positive and finite, and the slope between 0 and 90 degrees",
                    desc.height, desc.radius
                ),
            ));
        }
        if !in_world(pos) {
            return Err(outside_world(id, "character", pos));
        }
        let rot = unit_rotation(rot).ok_or_else(|| bad_rotation(id))?;
        let capsule = Shape::new_capsule(half, desc.radius)
            .map_err(|e| character_invalid(id, e.to_string()))?;
        let inner_shape = if desc.inner_body {
            Some(
                Shape::new_capsule(half, desc.radius)
                    .map_err(|e| character_invalid(id, e.to_string()))?,
            )
        } else {
            None
        };
        let settings = CharacterSettings::new(&capsule)
            .max_slope_angle(desc.max_slope_deg.to_radians())
            .mass(desc.mass)
            .max_strength(desc.mass * self.config.character_push_accel)
            .user_data(id.to_bits())
            .inner_body(inner_shape.as_ref().map(|shape| InnerBody {
                shape,
                object_layer: object_layer(desc.layer, true),
            }));
        let cid = self
            .world
            .create_character(&settings, rv(pos), jq(rot))
            .map_err(|e| character_invalid(id, e.to_string()))?;
        let sees = self.layers_seen_by(desc.layer);
        self.gate.current.store(desc.groups.0, Ordering::Relaxed);
        // A new character knows no contacts until refreshed, and stick-to-floor only
        // acts on a character that was supported before its update.
        let _ = self
            .world
            .refresh_character_contacts(cid, &JQueryFilter::new().object_layers(&sees));
        let inner = self.world.character(cid).ok().and_then(|c| c.inner_body());
        if let Some(inner) = inner {
            self.entity_of.insert(inner, id);
            self.gate.set(inner, desc.groups);
        }
        self.characters.insert(
            id,
            CharacterEntry {
                id: cid,
                inner,
                groups: desc.groups,
                step_height: desc.step_height,
                sees,
            },
        );
        Ok(())
    }

    /// Every object layer a moving body (or character) on `layer` collides with.
    fn layers_seen_by(&self, layer: Layer) -> Vec<ObjectLayer> {
        let table = &self.config.layers;
        let mut out = Vec::new();
        for other in 0..table.count() {
            let other = Layer(other);
            if table.static_collides(layer, other) {
                out.push(object_layer(other, false));
            }
            if table.moving_collides(layer, other) {
                out.push(object_layer(other, true));
            }
        }
        out
    }

    fn remove_character(&mut self, id: EntityId) -> Result<(), Diagnostic> {
        let entry = self
            .characters
            .remove(&id)
            .ok_or_else(|| unknown(id, "character"))?;
        if let Some(inner) = entry.inner {
            self.end_pairs_of(inner);
            self.gate.forget(inner);
            self.removed.push(inner);
        }
        self.state.moves.remove(&id);
        self.state.character_velocity.remove(&id);
        self.world
            .remove_character(entry.id)
            .map_err(|e| character_invalid(id, e.to_string()))
    }

    fn add_constraint(&mut self, id: EntityId, desc: &ConstraintDesc) -> Result<(), Diagnostic> {
        if self.constraints.contains_key(&id) {
            return Err(Diagnostic::error(
                codes::DUPLICATE_ENTITY,
                format!("entity {id} already has a constraint"),
            )
            .at(at(id)));
        }
        if !desc.is_well_formed() {
            return Err(constraint_invalid(
                id,
                "it must join two different bodies, with finite numbers, a non-zero hinge axis \
                 and ordered limits"
                    .into(),
            ));
        }
        let body = |e: EntityId| {
            self.bodies.get(&e).map(|b| b.id).ok_or_else(|| {
                constraint_invalid(id, format!("entity {e} has no body to attach to"))
            })
        };
        let (a, b) = (body(desc.a)?, body(desc.b)?);
        let created: Result<AnyConstraintId, oxijolt::ConstraintError> = match desc.kind {
            ConstraintKind::Fixed => self
                .world
                .create_constraint(
                    a,
                    b,
                    &FixedConstraintSettings::default().auto_detect_point(true),
                )
                .map(Into::into),
            ConstraintKind::Hinge {
                pivot,
                axis,
                limits,
            } => {
                let axis = axis.normalize();
                let normal = axis.any_orthonormal_vector();
                let mut settings = HingeConstraintSettings::new(rv(pivot), jv(axis), jv(normal));
                if let Some([lo, hi]) = limits {
                    settings = settings.limits(lo, hi);
                }
                self.world
                    .create_constraint(a, b, &settings)
                    .map(Into::into)
            }
            ConstraintKind::Distance {
                point_a,
                point_b,
                min,
                max,
            } => self
                .world
                .create_constraint(
                    a,
                    b,
                    &DistanceConstraintSettings::new(rv(point_a), rv(point_b))
                        .range(DistanceRange::Range { min, max }),
                )
                .map(Into::into),
        };
        let cid = created.map_err(|e| constraint_invalid(id, e.to_string()))?;
        self.constraints.insert(
            id,
            ConstraintEntry {
                id: cid,
                a: desc.a,
                b: desc.b,
            },
        );
        Ok(())
    }

    /// Advances the world by `dt` seconds: moves characters, drives kinematic bodies
    /// toward their targets, steps Jolt and gathers the step's contact events.
    ///
    /// Fails with `RE5016` (and does nothing) when `dt` is not a valid step length.
    /// A step that dropped contacts because a buffer was full still advances and
    /// reports `RE5011` through [`take_diagnostics`](Self::take_diagnostics).
    pub fn step(&mut self, dt: f32) -> Result<(), Diagnostic> {
        if !(dt.is_finite()
            && (PhysicsWorld::MIN_DELTA_TIME..=PhysicsWorld::MAX_DELTA_TIME).contains(&dt))
        {
            return Err(Diagnostic::error(
                codes::INVALID_ARGUMENT,
                format!(
                    "step length {dt} s is outside {}..={} s",
                    PhysicsWorld::MIN_DELTA_TIME,
                    PhysicsWorld::MAX_DELTA_TIME
                ),
            )
            .with_hint("step with the fixed time step (1/60 s by default)"));
        }
        // Bodies removed before the last step: their late events have been seen.
        for body in self.removed.drain(..) {
            self.entity_of.remove(&body);
        }
        self.state.events.clear();
        self.state.events.append(&mut self.state.pending_events);
        self.state.slept.clear();

        self.drive_kinematics(dt);
        self.drive_joints(dt);
        self.update_characters(dt);

        let report = self.world.step(dt).map_err(|e| {
            Diagnostic::error(codes::INVALID_ARGUMENT, format!("the step failed: {e}"))
        })?;
        if !report.is_complete() {
            self.diagnostics.push(
                Diagnostic::warning(
                    codes::STEP_INCOMPLETE,
                    format!(
                        "the step dropped contacts (manifold cache full: {}, body pair cache \
                         full: {}, contact constraints full: {})",
                        report.manifold_cache_full,
                        report.body_pair_cache_full,
                        report.contact_constraints_full
                    ),
                )
                .with_hint("raise PhysicsConfig::max_body_pairs and max_contact_constraints"),
            );
        }
        self.gather_events();
        let slept = std::mem::take(&mut self.state.slept);
        self.refresh_moved_assemblies(&slept);
        self.state.slept = slept;
        Ok(())
    }

    pub(crate) fn push_diagnostic(&mut self, d: Diagnostic) {
        self.diagnostics.push(d);
    }

    fn drive_kinematics(&mut self, dt: f32) {
        let targets = std::mem::take(&mut self.state.kin_targets);
        // A kinematic body keeps the velocity of its last move: stop the ones that were
        // driven last step and have no target now, or they drift on forever.
        for id in &self.state.kin_moved {
            if targets.contains_key(id) {
                continue;
            }
            if let Some(entry) = self.bodies.get(id)
                && let Ok(mut b) = self.world.body_mut(entry.id)
            {
                let _ = b.set_linear_velocity(jv(Vec3::ZERO));
                let _ = b.set_angular_velocity(jv(Vec3::ZERO));
            }
        }
        self.state.kin_moved.clear();
        for (id, (pos, rot)) in targets {
            let Some(entry) = self.bodies.get(&id) else {
                continue;
            };
            let moved = self
                .world
                .body_mut(entry.id)
                .and_then(|mut b| b.move_kinematic(rv(pos), jq(rot), dt));
            match moved {
                Ok(()) => {
                    self.state.kin_moved.insert(id);
                }
                Err(e) => self.diagnostics.push(body_invalid(id, e)),
            }
        }
    }

    fn update_characters(&mut self, dt: f32) {
        let gravity = self.config.gravity;
        let up = Vec3::Y;
        let moves = std::mem::take(&mut self.state.moves);
        for (&id, ch) in &self.characters {
            let (desired, jump) = moves.get(&id).copied().unwrap_or((Vec3::ZERO, false));
            let Ok(c) = self.world.character(ch.id) else {
                continue;
            };
            let before = grv(c.position());
            let velocity = gv(c.linear_velocity());
            let ground_velocity = gv(c.ground_velocity());
            let on_ground = c.ground_state() == JGroundState::OnGround;
            // Jolt's CharacterVirtual sample: on the ground the character takes the
            // ground's velocity (moving platforms) unless it is moving away from it; in
            // the air it keeps its vertical speed. Gravity always applies.
            let mut new_velocity = if on_ground && velocity.dot(up) - ground_velocity.dot(up) < 0.1
            {
                let mut v = ground_velocity;
                if jump {
                    v += up * self.config.character_jump_speed;
                }
                v
            } else {
                up * velocity.dot(up)
            };
            new_velocity += gravity * dt;
            new_velocity += desired - up * desired.dot(up);
            // Jolt refuses a faster character; a long fall stops speeding up
            // there (a terminal velocity).
            new_velocity = new_velocity.clamp_length_max(oxijolt::limits::MAX_LINEAR_VELOCITY);
            let step_up = jv(up * ch.step_height);
            let extended = ExtendedUpdateSettings::default().walk_stairs_step_up(step_up);
            self.gate.current.store(ch.groups.0, Ordering::Relaxed);
            let updated = self
                .world
                .character_mut(ch.id)
                .and_then(|mut c| c.set_linear_velocity(jv(new_velocity)))
                .and_then(|()| {
                    self.world.update_character(
                        ch.id,
                        dt,
                        jv(gravity),
                        &extended,
                        &JQueryFilter::new().object_layers(&ch.sees),
                    )
                });
            if let Err(e) = updated {
                self.diagnostics.push(character_invalid(id, e.to_string()));
                continue;
            }
            if let Ok(c) = self.world.character(ch.id) {
                let moved = grv(c.position()) - before;
                self.state.character_velocity.insert(id, moved / dt);
            }
            // Jolt moves the inner body without waking it; a static sensor sees only
            // awake bodies.
            if let Some(inner) = ch.inner
                && let Ok(mut b) = self.world.body_mut(inner)
            {
                b.activate();
            }
        }
    }

    fn gather_events(&mut self) {
        let mut events = self.world.take_events();
        let contacts = std::mem::take(&mut events.contacts);
        events.contacts = self.gather_part_contacts(contacts);
        for a in &events.activations {
            match a {
                ActivationEvent::Deactivated(body) => self.state.slept.push(*body),
                ActivationEvent::Activated(body) => self.state.slept.retain(|b| b != body),
            }
        }
        // Per body pair: how many sub-shape contacts appeared and went this step.
        struct Delta {
            added: u32,
            removed: u32,
            first: Option<(Vec3, Vec3, bool)>,
        }
        let mut deltas: BTreeMap<(BodyId, BodyId), Delta> = BTreeMap::new();
        for c in &events.contacts {
            let pair = c.pair();
            let d = deltas.entry((pair.body1, pair.body2)).or_insert(Delta {
                added: 0,
                removed: 0,
                first: None,
            });
            match c {
                JContactEvent::Added {
                    manifold, settings, ..
                } => {
                    d.added += 1;
                    if d.first.is_none() {
                        let point = manifold
                            .points
                            .first()
                            .map_or(Vec3::ZERO, |p| grv(p.point_on1));
                        d.first = Some((point, gv(manifold.normal), settings.is_sensor()));
                    }
                }
                JContactEvent::Removed(_) => d.removed += 1,
                JContactEvent::Persisted { .. } => {}
            }
        }
        for ((b1, b2), d) in deltas {
            let before = self
                .state
                .pairs
                .get(&(b1, b2))
                .map_or(0, |p| p.sub_contacts);
            let after = (before + d.added).saturating_sub(d.removed);
            if before == 0 {
                // A Removed for a pair we no longer track names a body that is gone.
                let (Some((point, normal, sensor)), true) = (d.first, after > 0) else {
                    continue;
                };
                let (Some(&e1), Some(&e2)) = (self.entity_of.get(&b1), self.entity_of.get(&b2))
                else {
                    continue;
                };
                let sensor = sensor || self.is_sensor(e1) || self.is_sensor(e2);
                // Jolt's normal points from body1 to body2; ours from the smaller entity.
                let (a, b, normal) = if e1 <= e2 {
                    (e1, e2, normal)
                } else {
                    (e2, e1, -normal)
                };
                self.state.pairs.insert(
                    (b1, b2),
                    Pair {
                        sub_contacts: after,
                        a,
                        b,
                        sensor,
                    },
                );
                self.state.events.push(ContactEvent::Begin {
                    a,
                    b,
                    point,
                    normal,
                    sensor,
                });
            } else if after == 0 {
                if let Some(pair) = self.state.pairs.remove(&(b1, b2)) {
                    self.state.events.push(ContactEvent::End {
                        a: pair.a,
                        b: pair.b,
                        sensor: pair.sensor,
                    });
                }
            } else if let Some(pair) = self.state.pairs.get_mut(&(b1, b2)) {
                pair.sub_contacts = after;
            }
        }
    }

    /// The character's actual velocity over the last step; before its first step, the
    /// velocity it was asked to move with.
    fn character_velocity(&self, id: EntityId, c: &oxijolt::CharacterRef<'_>) -> Vec3 {
        self.state
            .character_velocity
            .get(&id)
            .copied()
            .unwrap_or_else(|| gv(c.linear_velocity()))
    }

    fn is_sensor(&self, id: EntityId) -> bool {
        self.bodies.get(&id).is_some_and(|b| b.sensor)
    }

    /// Writes the pose of every awake body, plus every body that fell asleep during the
    /// last step and every character, into `out` (cleared first), sorted by entity.
    ///
    /// A sleeping body is not reported again until it wakes: its `Transform` keeps the
    /// value it had when it fell asleep, which is exact because the step it fell asleep
    /// in reports its final pose. A character's pose is its capsule's centre. Character
    /// inner bodies are not reported (the character is). Poses come from one batched
    /// read of Jolt's active bodies.
    pub fn poses_into(&mut self, out: &mut Vec<BodyPose>) {
        out.clear();
        self.world.active_body_poses_into(&mut self.scratch_poses);
        for p in &self.scratch_poses {
            if self.world.is_inner_body(p.id) {
                continue;
            }
            if let Some(&id) = self.entity_of.get(&p.id) {
                out.push(BodyPose {
                    id,
                    pos: grv(p.position),
                    rot: gq(p.rotation),
                });
            }
        }
        for body in &self.state.slept {
            let (Some(&id), Ok(b)) = (self.entity_of.get(body), self.world.body(*body)) else {
                continue;
            };
            if b.is_active() || self.world.is_inner_body(*body) {
                continue;
            }
            out.push(BodyPose {
                id,
                pos: grv(b.position()),
                rot: gq(b.rotation()),
            });
        }
        for (&id, ch) in &self.characters {
            if let Ok(c) = self.world.character(ch.id) {
                out.push(BodyPose {
                    id,
                    pos: grv(c.position()),
                    rot: gq(c.rotation()),
                });
            }
        }
        out.sort_by_key(|p| p.id);
    }

    /// The linear and angular velocity of an entity's body (or character, whose angular
    /// velocity is zero), or `None` if it has neither.
    pub fn velocity(&self, id: EntityId) -> Option<(Vec3, Vec3)> {
        if let Some(entry) = self.bodies.get(&id) {
            let b = self.world.body(entry.id).ok()?;
            return Some((gv(b.linear_velocity()), gv(b.angular_velocity())));
        }
        let ch = self.characters.get(&id)?;
        let c = self.world.character(ch.id).ok()?;
        Some((self.character_velocity(id, &c), Vec3::ZERO))
    }

    /// The position and rotation of an entity's body or character right now, awake or
    /// asleep. A body's position is its shape origin, not its centre of mass.
    pub fn pose(&self, id: EntityId) -> Option<(Vec3, Quat)> {
        if let Some(entry) = self.bodies.get(&id) {
            let b = self.world.body(entry.id).ok()?;
            return Some((grv(b.position()), gq(b.rotation())));
        }
        let ch = self.characters.get(&id)?;
        let c = self.world.character(ch.id).ok()?;
        Some((grv(c.position()), gq(c.rotation())))
    }

    /// Whether an entity's body is awake. `None` if it has no body.
    pub fn is_awake(&self, id: EntityId) -> Option<bool> {
        let entry = self.bodies.get(&id)?;
        self.world.body(entry.id).ok().map(|b| b.is_active())
    }

    /// A character's state after the last step, or `None` if the entity has none.
    pub fn character(&self, id: EntityId) -> Option<CharacterState> {
        let ch = self.characters.get(&id)?;
        let c = self.world.character(ch.id).ok()?;
        let ground = map_ground(c.ground_state());
        Some(CharacterState {
            pos: grv(c.position()),
            rot: gq(c.rotation()),
            velocity: self.character_velocity(id, &c),
            ground,
            ground_normal: gv(c.ground_normal()),
            ground_entity: c
                .ground_body()
                .and_then(|b| self.entity_of.get(&b).copied()),
        })
    }

    /// The contact events of the last step: `Begin` when two bodies start touching (or a
    /// body enters a sensor) and `End` when they stop.
    ///
    /// Events are per body pair, not per touching sub-shape, in a deterministic order.
    /// Removing a body ends its contacts: the `End` events arrive with the next step and
    /// name the removed entity. Jolt's own late reports about removed bodies are dropped.
    ///
    /// Jolt also ends a contact when the pair falls asleep, and begins it again when one
    /// of the bodies wakes; gameplay that reacts to `End` must treat sleep as the end of
    /// a touch (or check [`is_awake`](Self::is_awake)). A static sensor only sees awake
    /// bodies for the same reason.
    pub fn events(&self) -> &[ContactEvent] {
        &self.state.events
    }

    /// Takes the diagnostics produced since the last call (failed commands, steps that
    /// dropped contacts, characters that could not move).
    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// The number of bodies, character inner bodies included.
    pub fn body_count(&self) -> u32 {
        self.world.body_count()
    }

    /// The number of characters.
    pub fn character_count(&self) -> usize {
        self.characters.len()
    }

    /// How many Jolt jobs ran on the worker pool so far (a diagnostic counter).
    pub fn jobs_queued(&self) -> u64 {
        self.jobs.queued()
    }

    /// Saves the whole simulation state: bodies, characters, constraints' state, the
    /// contact cache and the contact pairs, so that a replay from it is bit-identical.
    pub fn save_state(&self) -> PhysicsSnapshot {
        PhysicsSnapshot {
            world: self.world.save_state(),
            state: self.state.clone(),
        }
    }

    /// Returns the world to `snapshot`. Fails with `RE5012` (leaving the world as it
    /// was) when the snapshot belongs to another world or bodies, characters or
    /// constraints were added or removed since it was saved.
    pub fn restore_state(&mut self, snapshot: &PhysicsSnapshot) -> Result<(), Diagnostic> {
        self.world.restore_state(&snapshot.world).map_err(|e| {
            Diagnostic::error(
                codes::RESTORE_FAILED,
                format!("the physics state could not be restored: {e}"),
            )
            .with_hint(
                "a snapshot restores only into the world that saved it, before any body, \
                 character or constraint is added or removed; to seed another world, use \
                 motion_state and apply_motion_state",
            )
        })?;
        self.state = snapshot.state.clone();
        Ok(())
    }

    /// The motion of every dynamic and kinematic body and every character, keyed by
    /// entity, to seed another world with [`apply_motion_state`](Self::apply_motion_state).
    ///
    /// Unlike [`save_state`](Self::save_state) it carries over to any world that has
    /// bodies and characters for the same entities, such as a fresh `Physics` built from
    /// a copy of the scene.
    pub fn motion_state(&self) -> MotionState {
        let mut bodies = Vec::new();
        for (&id, entry) in &self.bodies {
            if entry.kind == BodyKind::Static {
                continue;
            }
            let Ok(b) = self.world.body(entry.id) else {
                continue;
            };
            bodies.push(BodyMotion {
                id,
                pos: grv(b.position()),
                rot: gq(b.rotation()),
                linear: gv(b.linear_velocity()),
                angular: gv(b.angular_velocity()),
                awake: b.is_active(),
            });
        }
        let mut characters = Vec::new();
        for (&id, ch) in &self.characters {
            let Ok(c) = self.world.character(ch.id) else {
                continue;
            };
            characters.push(CharacterMotion {
                id,
                velocity: self.character_velocity(id, &c),
                jolt: c.save_state(),
            });
        }
        MotionState { bodies, characters }
    }

    /// Gives each entity of `motion` that this world also has the pose, velocity and
    /// sleep state it had in the world that took it, so a copy of the scene continues
    /// moving instead of starting from rest. A character also takes its ground and
    /// contact state.
    ///
    /// Entities this world lacks, or holds as a static body, are skipped with a warning
    /// (`RE5004` or `RE5009`) in [`take_diagnostics`](Self::take_diagnostics). Contact
    /// pairs are not carried over: the next step reports `Begin` for bodies already
    /// touching. Characters' ground and contacts are recomputed against this world, so
    /// it may hold fewer or other bodies than the source. Commands applied to the source
    /// but not yet stepped (kinematic targets, character moves) are not carried; take
    /// the state between frames. The continuation is close to the original but not
    /// bit-identical, since Jolt's contact cache starts empty;
    /// [`restore_state`](Self::restore_state) is the exact replay within one world.
    pub fn apply_motion_state(&mut self, motion: &MotionState) {
        for m in &motion.bodies {
            let Some(entry) = self.bodies.get(&m.id) else {
                self.diagnostics.push(as_warning(unknown(m.id, "body")));
                continue;
            };
            if entry.kind == BodyKind::Static {
                self.diagnostics
                    .push(as_warning(wrong_kind(m.id, "motion", entry.kind)));
                continue;
            }
            let kinematic = entry.kind == BodyKind::Kinematic;
            let body = entry.id;
            let result = self.world.body_mut(body).and_then(|mut b| {
                b.set_position_and_rotation(rv(m.pos), jq(m.rot), Activation::DontActivate)?;
                if m.awake {
                    b.set_linear_velocity(jv(m.linear))?;
                    b.set_angular_velocity(jv(m.angular))?;
                    b.activate();
                    Ok(())
                } else {
                    b.deactivate()
                }
            });
            match result {
                Ok(()) => {
                    // A moving kinematic body stops next step unless it gets a target,
                    // as it would have in the original world.
                    if kinematic && m.awake && (m.linear != Vec3::ZERO || m.angular != Vec3::ZERO) {
                        self.state.kin_moved.insert(m.id);
                    }
                }
                Err(e) => self.diagnostics.push(body_invalid(m.id, e)),
            }
        }
        for m in &motion.characters {
            let Some(ch) = self.characters.get(&m.id) else {
                self.diagnostics
                    .push(as_warning(unknown(m.id, "character")));
                continue;
            };
            let (cid, inner) = (ch.id, ch.inner);
            let filter_layers = ch.sees.clone();
            self.gate.current.store(ch.groups.0, Ordering::Relaxed);
            // The saved ground and contacts name bodies by Jolt id, which differ when the
            // worlds hold different bodies: recompute them against this world.
            let restored = self
                .world
                .character_mut(cid)
                .and_then(|mut c| c.restore_state(&m.jolt))
                .and_then(|()| {
                    self.world.refresh_character_contacts(
                        cid,
                        &JQueryFilter::new().object_layers(&filter_layers),
                    )
                });
            match restored {
                Ok(()) => {
                    self.state.character_velocity.insert(m.id, m.velocity);
                    if let Some(inner) = inner
                        && let Ok(mut b) = self.world.body_mut(inner)
                    {
                        b.activate();
                    }
                }
                Err(e) => self
                    .diagnostics
                    .push(character_invalid(m.id, e.to_string())),
            }
        }
    }
}

fn as_warning(mut d: Diagnostic) -> Diagnostic {
    d.severity = Severity::Warning;
    d
}

fn wrong_kind(id: EntityId, what: &str, kind: BodyKind) -> Diagnostic {
    Diagnostic::error(
        codes::WRONG_BODY_KIND,
        format!("entity {id} is a {kind:?} body, which does not take {what}"),
    )
    .at(at(id))
    .with_hint(
        "impulses move dynamic bodies, kinematic targets move kinematic bodies, and static \
         bodies only move by SetTransform",
    )
}

fn static_only(id: EntityId, shape: &re_types::physics::ShapeDesc, kind: BodyKind) -> Diagnostic {
    Diagnostic::error(
        codes::STATIC_ONLY_SHAPE,
        format!(
            "a {} collider can only be used on a static body, not a {kind:?} one",
            shapes::kind_name(shape)
        ),
    )
    .at(at(id))
    .with_hint("make the body static, or use a box, sphere, capsule or convex collider")
}

fn bad_rotation(id: EntityId) -> Diagnostic {
    Diagnostic::error(
        codes::BODY_INVALID,
        format!("entity {id}: the rotation is zero or not finite"),
    )
    .at(at(id))
}

fn body_invalid(id: EntityId, e: oxijolt::BodyError) -> Diagnostic {
    Diagnostic::error(codes::BODY_INVALID, format!("body {id}: {e}"))
        .at(at(id))
        .with_hint(
            "mass must be positive, friction and damping non-negative, restitution in [0, 1]",
        )
}

fn constraint_invalid(id: EntityId, why: String) -> Diagnostic {
    Diagnostic::error(
        codes::CONSTRAINT_INVALID,
        format!("constraint {id} cannot be created: {why}"),
    )
    .at(at(id))
}

fn character_invalid(id: EntityId, why: String) -> Diagnostic {
    Diagnostic::error(codes::CHARACTER_INVALID, format!("character {id}: {why}")).at(at(id))
}
