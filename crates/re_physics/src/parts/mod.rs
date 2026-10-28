//! Parts and assemblies: rigid bodies built from primitive parts.
//!
//! A **part** is a primitive solid ([`PartGeom`]) with a material and flags,
//! named by the caller's [`EntityId`]. An **assembly** is one Jolt body made
//! of one or more parts, each at a fixed pose relative to the assembly's
//! origin: welded parts, or a static cell of unrelated anchored parts. Its
//! shape is a compound with one child per part; the child's user data is the
//! part's index, so every contact and query resolves to a part.
//!
//! What this layer adds over plain bodies:
//!
//! * **Mass from parts.** Each part weighs `density × volume`; massless parts
//!   add nothing (unless the assembly has no other mass). The assembly's mass
//!   and centre of mass are exact; its inertia is Jolt's for the compound at
//!   uniform density, scaled to the mass.
//! * **Collision groups per part** ([`GroupTable`], up to 32 groups), checked
//!   per touching pair of parts in a contact listener, so one assembly may mix
//!   groups.
//! * **Parts that touch without colliding** (`can_collide = false`): their
//!   contacts become sensor contacts, which push nothing but still report
//!   [`PartEvent`]s when both parts can touch.
//! * **Friction and elasticity per part**, combined per contact as the
//!   weight-averaged mean of the two parts' values.
//! * **Touch events per part pair** ([`PartEvent`]), kept across sleep: a
//!   pair resting asleep stays touching instead of ending and beginning again.
//! * **Part queries** ([`query`]): rays, shape casts and box, sphere and part
//!   overlaps over every part, with a per-part filter, answered from a
//!   bounding-volume tree and the exact primitive geometry of [`geom`], not
//!   from Jolt, so a filter can skip one part of an assembly.
//! * **Joints** between assemblies ([`JointDesc`]): ball sockets, hinges with
//!   motors and servos, ropes, rods and welds.
//!
//! Assemblies live in the same Jolt world as the bodies and characters of
//! [`PhysicsCmd`](re_types::physics::PhysicsCmd)s; a contact between a part
//! and a plain body or character is a [`PartEvent`] naming the body's entity.

pub mod geom;
mod joints;
mod listener;
pub mod query;
pub mod tree;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, PoisonError};

use glam::{Mat3, Quat, Vec3};
use oxijolt::{
    Activation, BodyId, BodySettings, CompoundChild, ContactEvent as JContactEvent, Shape,
    SubShapeIdPair,
};
use re_core::{Diagnostic, Location};
use re_types::EntityId;
use re_types::physics::BodyKind;

use crate::codes;
use crate::convert::{gq, grv, gv, jq, jv, rv, unit_rotation};
use crate::world::{Physics, object_layer};

pub use geom::{PartGeom, PartShape, Pose};
pub use joints::{HingeDrive, JointDesc, JointKind};
pub use query::{PartFilter, PartHit};

pub(crate) use listener::{GateBody, GatePart, PartGate, PartListener};

/// The most collision groups a [`GroupTable`] holds.
pub const MAX_GROUPS: usize = 32;

/// Which collision groups collide with which: a symmetric 32 × 32 table.
/// Group 0 is the default group. A new table lets everything collide.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GroupTable {
    rows: [u32; MAX_GROUPS],
}

impl Default for GroupTable {
    fn default() -> Self {
        GroupTable {
            rows: [u32::MAX; MAX_GROUPS],
        }
    }
}

impl GroupTable {
    /// Whether parts of groups `a` and `b` collide. Groups beyond the table
    /// collide with nothing.
    pub fn collides(&self, a: u8, b: u8) -> bool {
        let (a, b) = (usize::from(a), usize::from(b));
        a < MAX_GROUPS && b < MAX_GROUPS && self.rows[a] & (1 << b) != 0
    }

    /// Sets whether groups `a` and `b` collide (both ways). Groups beyond the
    /// table are ignored.
    pub fn set(&mut self, a: u8, b: u8, collides: bool) {
        let (a, b) = (usize::from(a), usize::from(b));
        if a >= MAX_GROUPS || b >= MAX_GROUPS {
            return;
        }
        if collides {
            self.rows[a] |= 1 << b;
            self.rows[b] |= 1 << a;
        } else {
            self.rows[a] &= !(1 << b);
            self.rows[b] &= !(1 << a);
        }
    }

    /// Makes group `g` collide with every group again, as a fresh group does.
    pub fn reset(&mut self, g: u8) {
        for other in 0..MAX_GROUPS as u8 {
            self.set(g, other, true);
        }
    }
}

/// A part's physical material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartMaterial {
    /// Mass per unit volume.
    pub density: f32,
    /// Friction coefficient.
    pub friction: f32,
    /// Bounciness, 0 to 1.
    pub elasticity: f32,
    /// How much this part's friction counts when two parts touch.
    pub friction_weight: f32,
    /// How much this part's elasticity counts when two parts touch.
    pub elasticity_weight: f32,
}

impl Default for PartMaterial {
    fn default() -> Self {
        PartMaterial {
            density: 0.7,
            friction: 0.3,
            elasticity: 0.5,
            friction_weight: 1.0,
            elasticity_weight: 1.0,
        }
    }
}

/// One part of an assembly.
#[derive(Clone, Debug, PartialEq)]
pub struct PartDesc {
    /// The part's entity.
    pub id: EntityId,
    /// Its shape and size.
    pub geom: PartGeom,
    /// Its pose relative to the assembly's origin.
    pub local: Pose,
    /// Its material.
    pub material: PartMaterial,
    /// A massless part adds nothing to the assembly's mass.
    pub massless: bool,
    /// Its collision group, below [`MAX_GROUPS`].
    pub group: u8,
    /// Whether it pushes and is pushed by other parts.
    pub can_collide: bool,
    /// Whether it reports touches.
    pub can_touch: bool,
    /// Whether queries see it while it does not collide (colliding parts are
    /// always seen).
    pub can_query: bool,
}

impl PartDesc {
    /// A colliding block part of `size` at the assembly's origin, in group 0.
    pub fn block(id: EntityId, size: Vec3) -> PartDesc {
        PartDesc {
            id,
            geom: PartGeom::new(PartShape::Block, size),
            local: Pose::IDENTITY,
            material: PartMaterial::default(),
            massless: false,
            group: 0,
            can_collide: true,
            can_touch: true,
            can_query: true,
        }
    }

    /// The part's mass: density × volume.
    pub fn mass(&self) -> f32 {
        self.material.density.max(0.0) * self.geom.volume()
    }

    fn gate(&self) -> GatePart {
        GatePart {
            group: self.group,
            collide: self.can_collide,
            touch: self.can_touch,
            friction: self.material.friction,
            elasticity: self.material.elasticity,
            friction_weight: self.material.friction_weight,
            elasticity_weight: self.material.elasticity_weight,
        }
    }

    fn queryable(&self) -> bool {
        self.can_collide || self.can_query
    }
}

/// Everything needed to create an assembly.
#[derive(Clone, Debug, PartialEq)]
pub struct AssemblyDesc {
    /// Static (anchored), dynamic or kinematic.
    pub kind: BodyKind,
    /// The pose of the assembly's origin in the world.
    pub pose: Pose,
    /// Initial linear velocity of the centre of mass.
    pub linear: Vec3,
    /// Initial angular velocity.
    pub angular: Vec3,
    /// The parts; at least one, each id once.
    pub parts: Vec<PartDesc>,
}

/// A change in touch between two parts (or a part and a plain body), from the
/// steps since the last [`Physics::take_part_events`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PartEvent {
    /// The two started touching.
    Begin {
        /// The smaller entity.
        a: EntityId,
        /// The larger entity.
        b: EntityId,
        /// A contact point.
        point: Vec3,
    },
    /// The two stopped touching (or one was removed).
    End {
        /// The smaller entity.
        a: EntityId,
        /// The larger entity.
        b: EntityId,
    },
}

/// The state of an assembly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssemblyInfo {
    /// Its kind.
    pub kind: BodyKind,
    /// The pose of its origin.
    pub pose: Pose,
    /// Its mass (0 for a static assembly).
    pub mass: f32,
    /// Its centre of mass in the world.
    pub center_of_mass: Vec3,
    /// Its inertia tensor about the centre of mass, in world axes (each part
    /// counted as a solid of its shape at its own density; zero when static).
    pub inertia: Mat3,
    /// Linear velocity of the centre of mass.
    pub linear: Vec3,
    /// Angular velocity.
    pub angular: Vec3,
    /// Whether the body is awake.
    pub awake: bool,
}

/// One part as the queries and callers see it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartView {
    /// The part.
    pub id: EntityId,
    /// Its assembly.
    pub assembly: EntityId,
    /// Its shape and size.
    pub geom: PartGeom,
    /// Its pose in the world.
    pub world: Pose,
    /// Its collision group.
    pub group: u8,
    /// Whether it collides.
    pub can_collide: bool,
    /// Whether it reports touches.
    pub can_touch: bool,
    /// Whether queries see it while it does not collide.
    pub can_query: bool,
}

pub(crate) struct PartRec {
    pub(crate) desc: PartDesc,
    pub(crate) world: Pose,
    pub(crate) proxy: u32,
}

impl PartRec {
    pub(crate) fn view(&self, assembly: EntityId) -> PartView {
        PartView {
            id: self.desc.id,
            assembly,
            geom: self.desc.geom,
            world: self.world,
            group: self.desc.group,
            can_collide: self.desc.can_collide,
            can_touch: self.desc.can_touch,
            can_query: self.desc.queryable(),
        }
    }
}

pub(crate) struct Assembly {
    pub(crate) body: BodyId,
    pub(crate) kind: BodyKind,
    pub(crate) parts: Vec<PartRec>,
    gate: Arc<GateBody>,
    mass: f32,
    com_local: Vec3,
    inertia_local: Mat3,
}

/// A touching pair of sub-shapes, as Jolt reports it.
#[derive(Clone, Copy, Debug)]
struct SubPair {
    a: EntityId,
    b: EntityId,
    /// Asleep: kept touching until a body wakes; steps of grace left after that.
    dormant: Option<u8>,
}

#[derive(Clone, Copy, Debug)]
struct PairState {
    count: u32,
    reported: bool,
}

type SubKey = (u32, u32, u32, u32);

fn sub_key(p: &SubShapeIdPair) -> SubKey {
    (
        p.body1.to_raw(),
        p.sub_shape1.to_raw(),
        p.body2.to_raw(),
        p.sub_shape2.to_raw(),
    )
}

fn ordered(a: EntityId, b: EntityId) -> (EntityId, EntityId) {
    if a <= b { (a, b) } else { (b, a) }
}

/// The parts layer's state inside [`Physics`].
pub(crate) struct PartsState {
    pub(crate) assemblies: BTreeMap<EntityId, Assembly>,
    pub(crate) body_key: HashMap<BodyId, EntityId>,
    /// Part → (assembly, child index).
    pub(crate) slots: HashMap<EntityId, (EntityId, u32)>,
    pub(crate) tree: tree::Tree<EntityId>,
    pub(crate) gate: Arc<PartGate>,
    events: Vec<PartEvent>,
    sub_pairs: HashMap<SubKey, SubPair>,
    pair_states: BTreeMap<(EntityId, EntityId), PairState>,
    moved: BTreeSet<EntityId>,
    pub(crate) joints: joints::Joints,
    scratch: Vec<oxijolt::BodyPose>,
    /// Where the simulation's origin lies in the world: positions go to Jolt
    /// relative to it, so a map away from the world origin still fits Jolt's
    /// limits.
    pub(crate) origin: Vec3,
}

impl Default for PartsState {
    fn default() -> Self {
        PartsState {
            assemblies: BTreeMap::new(),
            body_key: HashMap::new(),
            slots: HashMap::new(),
            tree: tree::Tree::new(0.1),
            gate: Arc::new(PartGate::default()),
            events: Vec::new(),
            sub_pairs: HashMap::new(),
            pair_states: BTreeMap::new(),
            moved: BTreeSet::new(),
            joints: joints::Joints::default(),
            scratch: Vec::new(),
            origin: Vec3::ZERO,
        }
    }
}

impl PartsState {
    fn pair_delta(&mut self, a: EntityId, b: EntityId, touch: bool, delta: i32, point: Vec3) {
        let key = ordered(a, b);
        if delta > 0 {
            let s = self.pair_states.entry(key).or_insert(PairState {
                count: 0,
                reported: false,
            });
            s.count += 1;
            if s.count == 1 && touch {
                s.reported = true;
                self.events.push(PartEvent::Begin {
                    a: key.0,
                    b: key.1,
                    point,
                });
            }
        } else if let Some(s) = self.pair_states.get_mut(&key) {
            s.count = s.count.saturating_sub(1);
            if s.count == 0 {
                let reported = s.reported;
                self.pair_states.remove(&key);
                if reported {
                    self.events.push(PartEvent::End { a: key.0, b: key.1 });
                }
            }
        }
    }

    fn end_sub_pairs(&mut self, raw: u32) {
        let mut keys: Vec<SubKey> = self
            .sub_pairs
            .keys()
            .filter(|k| k.0 == raw || k.2 == raw)
            .copied()
            .collect();
        keys.sort_unstable();
        for k in keys {
            if let Some(p) = self.sub_pairs.remove(&k) {
                self.pair_delta(p.a, p.b, true, -1, Vec3::ZERO);
            }
        }
    }
}

fn at(id: EntityId) -> Location {
    Location::entity(id.to_string())
}

fn invalid(id: EntityId, why: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::error(codes::ASSEMBLY_INVALID, format!("assembly {id}: {why}"))
        .at(at(id))
        .with_hint(
            "give the assembly at least one part with a positive, finite size, unique part ids \
             and a group below 32",
        )
}

fn unknown_assembly(id: EntityId) -> Diagnostic {
    Diagnostic::error(
        codes::UNKNOWN_ENTITY,
        format!("entity {id} has no assembly"),
    )
    .at(at(id))
    .with_hint("add the assembly before sending commands to it")
}

/// Jolt's default convex radius, shrunk for thin parts.
fn convex_radius(geom: &PartGeom) -> f32 {
    (geom.size.min_element() * 0.25).min(0.05)
}

/// The Jolt shape of one part, in the part's frame, and the rotation that puts
/// it there (a Jolt cylinder lies along Y; a part cylinder along X).
fn part_shape(geom: &PartGeom) -> Result<(Shape, Quat), String> {
    let g = geom.sanitized();
    let h = g.half();
    let r = convex_radius(&g);
    let res = match g.shape {
        PartShape::Block => {
            Shape::new_box_with_convex_radius(jv(h), r).map(|s| (s, Quat::IDENTITY))
        }
        PartShape::Ball => Shape::new_sphere(g.radius()).map(|s| (s, Quat::IDENTITY)),
        PartShape::Cylinder => {
            let radius = g.radius();
            Shape::new_cylinder_with_convex_radius(h.x, radius, r.min(radius * 0.5).min(h.x * 0.5))
                .map(|s| (s, Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2)))
        }
        PartShape::Wedge | PartShape::CornerWedge => {
            let pts: Vec<oxijolt::Vec3> = g.vertices().into_iter().map(jv).collect();
            match Shape::new_convex_hull_with_convex_radius(&pts, r) {
                Ok(s) => Ok((s, Quat::IDENTITY)),
                // Too thin for Jolt's hull builder: collide as its box.
                Err(_) => Shape::new_box_with_convex_radius(jv(h), r).map(|s| (s, Quat::IDENTITY)),
            }
        }
    };
    res.map_err(|e| e.to_string())
}

/// The compound of an assembly's parts and, for a moving body whose centre of
/// mass is not the compound's, the decorated shape the body gets instead, plus
/// the mass and the local centre of mass.
fn build_shape(
    kind: BodyKind,
    parts: &[PartDesc],
) -> Result<(Shape, Option<Shape>, f32, Vec3), String> {
    let shapes: Vec<(Shape, Quat)> = parts
        .iter()
        .map(|p| part_shape(&p.geom))
        .collect::<Result<_, _>>()?;
    let children: Vec<CompoundChild<'_>> = parts
        .iter()
        .zip(&shapes)
        .enumerate()
        .map(|(i, (p, (s, fix)))| CompoundChild {
            shape: s,
            position: jv(p.local.pos),
            rotation: jq((p.local.rot * *fix).normalize()),
            user_data: i as u32,
        })
        .collect();
    let compound = Shape::new_compound(&children).map_err(|e| e.to_string())?;
    // Mass: massless parts count only when nothing else has mass.
    let mut mass = 0.0f32;
    let mut moment = Vec3::ZERO;
    let mut vol = 0.0f32;
    let mut vol_moment = Vec3::ZERO;
    for p in parts {
        let c = p.local.point(p.geom.sanitized().centroid());
        let v = p.geom.sanitized().volume();
        vol += v;
        vol_moment += c * v;
        if !p.massless {
            let m = p.mass();
            mass += m;
            moment += c * m;
        }
    }
    if mass <= 0.0 {
        for p in parts {
            let m = p.mass();
            mass += m;
            moment += p.local.point(p.geom.sanitized().centroid()) * m;
        }
    }
    let com = if mass > 0.0 {
        moment / mass
    } else {
        vol_moment / vol.max(1e-12)
    };
    let jolt_com = vol_moment / vol.max(1e-12);
    let offset = com - jolt_com;
    let body_shape = if kind != BodyKind::Static && offset.length() > 1e-6 {
        Some(Shape::new_offset_center_of_mass(&compound, jv(offset)).map_err(|e| e.to_string())?)
    } else {
        None
    };
    Ok((compound, body_shape, mass, com))
}

/// The inertia tensor of one solid of `mass` and `geom` about its own
/// centroid, in its own axes (wedges count as their bounding boxes).
fn part_inertia(geom: &PartGeom, mass: f32) -> Mat3 {
    let s = geom.size;
    match geom.shape {
        PartShape::Ball => Mat3::from_diagonal(Vec3::splat(0.4 * mass * geom.radius().powi(2))),
        PartShape::Cylinder => {
            let (r, l) = (geom.radius(), s.x);
            let side = mass * (3.0 * r * r + l * l) / 12.0;
            Mat3::from_diagonal(Vec3::new(0.5 * mass * r * r, side, side))
        }
        _ => Mat3::from_diagonal(
            Vec3::new(
                s.y * s.y + s.z * s.z,
                s.x * s.x + s.z * s.z,
                s.x * s.x + s.y * s.y,
            ) * (mass / 12.0),
        ),
    }
}

/// The inertia of the parts about `com`, in the assembly's axes.
fn assembly_inertia(parts: &[PartDesc], com: Vec3) -> Mat3 {
    let any_mass = parts.iter().any(|p| !p.massless && p.mass() > 0.0);
    let mut total = Mat3::ZERO;
    for p in parts {
        if any_mass && p.massless {
            continue;
        }
        let g = p.geom.sanitized();
        let m = p.mass();
        let r = Mat3::from_quat(p.local.rot);
        let own = r * part_inertia(&g, m) * r.transpose();
        // Parallel axis: m (|d|² I - d dᵀ).
        let d = p.local.point(g.centroid()) - com;
        let outer = Mat3::from_cols(d * d.x, d * d.y, d * d.z);
        total += own + (Mat3::IDENTITY * d.length_squared() - outer) * m;
    }
    total
}

fn clamp_mass(m: f32) -> f32 {
    #[allow(
        clippy::unnecessary_cast,
        reason = "`Real` is f64 when oxijolt's `double-precision` feature is on"
    )]
    let (lo, hi) = (
        oxijolt::limits::MIN_MASS as f32,
        oxijolt::limits::MAX_MASS as f32,
    );
    if m.is_finite() { m.clamp(lo, hi) } else { lo }
}

fn check_parts(key: EntityId, parts: &[PartDesc]) -> Result<(), Diagnostic> {
    if parts.is_empty() {
        return Err(invalid(key, "it has no parts"));
    }
    let mut seen = BTreeSet::new();
    for p in parts {
        if !seen.insert(p.id) {
            return Err(invalid(key, format!("part {} appears twice", p.id)));
        }
        if usize::from(p.group) >= MAX_GROUPS {
            return Err(invalid(
                key,
                format!("part {} is in group {}", p.id, p.group),
            ));
        }
        if !(p.local.pos.is_finite() && p.local.rot.is_finite()) {
            return Err(invalid(key, format!("part {} has a non-finite pose", p.id)));
        }
        if !p.geom.size.is_finite() {
            return Err(invalid(key, format!("part {} has a non-finite size", p.id)));
        }
    }
    Ok(())
}

impl Physics {
    /// Replaces the collision group table. Contacts already cached are
    /// re-checked at the next step.
    pub fn set_collision_groups(&mut self, table: GroupTable) {
        let changed = {
            let mut g = self
                .parts
                .gate
                .groups
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let changed = *g != table;
            *g = table;
            changed
        };
        if changed {
            self.invalidate_all_contacts();
        }
    }

    /// The collision group table.
    pub fn collision_groups(&self) -> GroupTable {
        self.parts
            .gate
            .groups
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn invalidate_all_contacts(&mut self) {
        let bodies: Vec<BodyId> = self.parts.assemblies.values().map(|a| a.body).collect();
        for b in bodies {
            if let Ok(mut body) = self.world.body_mut(b) {
                body.invalidate_contact_cache();
            }
        }
    }

    /// Creates an assembly named `key` (usually its root part's entity).
    ///
    /// Fails with `RE5017` for an empty or malformed part list, `RE5005` when
    /// `key` or one of its parts already belongs to an assembly, `RE5006` for a
    /// pose outside the world, and `RE5002` when Jolt refuses the shape.
    pub fn add_assembly(&mut self, key: EntityId, desc: &AssemblyDesc) -> Result<(), Diagnostic> {
        if self.parts.assemblies.contains_key(&key) {
            return Err(Diagnostic::error(
                codes::DUPLICATE_ENTITY,
                format!("entity {key} already has an assembly"),
            )
            .at(at(key)));
        }
        check_parts(key, &desc.parts)?;
        if let Some(p) = desc
            .parts
            .iter()
            .find(|p| self.parts.slots.contains_key(&p.id))
        {
            return Err(Diagnostic::error(
                codes::DUPLICATE_ENTITY,
                format!("part {} already belongs to an assembly", p.id),
            )
            .at(at(p.id)));
        }
        if !self.in_reach(desc.pose.pos) {
            return Err(self.out_of_reach(key, "assembly", desc.pose.pos));
        }
        let rot = unit_rotation(desc.pose.rot)
            .ok_or_else(|| invalid(key, "its rotation is zero or not finite"))?;
        let (compound, shape, mass, com) =
            build_shape(desc.kind, &desc.parts).map_err(|e| invalid(key, e))?;
        let moving = desc.kind != BodyKind::Static;
        let mut settings = match desc.kind {
            BodyKind::Static => BodySettings::new_static(),
            BodyKind::Dynamic => BodySettings::new_dynamic().mass(clamp_mass(mass)),
            BodyKind::Kinematic => BodySettings::new_kinematic(),
        }
        .object_layer(object_layer(re_types::physics::Layer(0), moving))
        .position(self.jolt_point(desc.pose.pos))
        .rotation(jq(rot))
        .linear_damping(0.0)
        .angular_damping(0.0)
        .user_data(key.to_bits());
        if desc.kind == BodyKind::Dynamic {
            settings = settings
                .linear_velocity(jv(desc.linear))
                .angular_velocity(jv(desc.angular));
        }
        let body = self
            .world
            .create_body(shape.as_ref().unwrap_or(&compound), &settings)
            .map_err(|e| invalid(key, e))?;
        let pose = Pose::new(desc.pose.pos, rot);
        let gate = Arc::new(GateBody {
            compound: Arc::new(compound),
            parts: desc.parts.iter().map(PartDesc::gate).collect(),
        });
        self.parts.gate.set_body(body, gate.clone());
        let mut recs = Vec::with_capacity(desc.parts.len());
        for (i, p) in desc.parts.iter().enumerate() {
            let world = pose.mul(&p.local);
            let (min, max) = p.geom.sanitized().box_aabb(&world);
            let proxy = self.parts.tree.insert(tree::Aabb::new(min, max), p.id);
            self.parts.slots.insert(p.id, (key, i as u32));
            recs.push(PartRec {
                desc: p.clone(),
                world,
                proxy,
            });
        }
        self.parts.body_key.insert(body, key);
        self.parts.assemblies.insert(
            key,
            Assembly {
                body,
                kind: desc.kind,
                parts: recs,
                gate,
                inertia_local: assembly_inertia(&desc.parts, com),
                mass: if desc.kind == BodyKind::Dynamic {
                    clamp_mass(mass)
                } else {
                    mass
                },
                com_local: com,
            },
        );
        Ok(())
    }

    /// Removes an assembly. Its touches end (the `End` events come with the
    /// next [`take_part_events`](Self::take_part_events)) and the joints
    /// attached to it are removed and reported by
    /// [`take_dropped_joints`](Self::take_dropped_joints).
    pub fn remove_assembly(&mut self, key: EntityId) -> Result<(), Diagnostic> {
        let body = self
            .parts
            .assemblies
            .get(&key)
            .map(|a| a.body)
            .ok_or_else(|| unknown_assembly(key))?;
        self.drop_joints_of(key);
        self.world.remove_body(body).map_err(|e| invalid(key, e))?;
        let asm = self.parts.assemblies.remove(&key).expect("checked above");
        self.parts.body_key.remove(&body);
        self.parts.gate.forget(body);
        for p in &asm.parts {
            self.parts.tree.remove(p.proxy);
            self.parts.slots.remove(&p.desc.id);
        }
        self.parts.end_sub_pairs(body.to_raw());
        self.parts.moved.remove(&key);
        Ok(())
    }

    /// Gives an assembly a new part list, keeping its body, pose and motion (the
    /// body's kind stays; change kind by removing and adding). Parts that stay
    /// keep their touches; joints on the assembly are put back with their frames
    /// relative to the assembly's origin.
    pub fn set_assembly_parts(
        &mut self,
        key: EntityId,
        parts: &[PartDesc],
    ) -> Result<(), Diagnostic> {
        let (body, kind) = self
            .parts
            .assemblies
            .get(&key)
            .map(|a| (a.body, a.kind))
            .ok_or_else(|| unknown_assembly(key))?;
        check_parts(key, parts)?;
        if let Some(p) = parts.iter().find(|p| {
            self.parts
                .slots
                .get(&p.id)
                .is_some_and(|(owner, _)| *owner != key)
        }) {
            return Err(Diagnostic::error(
                codes::DUPLICATE_ENTITY,
                format!("part {} already belongs to another assembly", p.id),
            )
            .at(at(p.id)));
        }
        let (compound, shape, mass, com) = build_shape(kind, parts).map_err(|e| invalid(key, e))?;
        let joints = self.suspend_joints_of(key);
        let activation = if kind == BodyKind::Static {
            Activation::DontActivate
        } else {
            Activation::Activate
        };
        let jmass = (kind == BodyKind::Dynamic).then(|| clamp_mass(mass));
        let reshaped = self
            .world
            .body_mut(body)
            .and_then(|mut b| b.set_shape(shape.as_ref().unwrap_or(&compound), jmass, activation))
            .map_err(|e| invalid(key, e));
        if let Err(d) = reshaped {
            self.resume_joints(joints);
            return Err(d);
        }
        let pose = self.assembly_pose_raw(body);
        let gate = Arc::new(GateBody {
            compound: Arc::new(compound),
            parts: parts.iter().map(PartDesc::gate).collect(),
        });
        self.parts.gate.set_body(body, gate.clone());
        let asm = self.parts.assemblies.get_mut(&key).expect("checked above");
        for p in &asm.parts {
            self.parts.tree.remove(p.proxy);
            self.parts.slots.remove(&p.desc.id);
        }
        let mut recs = Vec::with_capacity(parts.len());
        for (i, p) in parts.iter().enumerate() {
            let world = pose.mul(&p.local);
            let (min, max) = p.geom.sanitized().box_aabb(&world);
            let proxy = self.parts.tree.insert(tree::Aabb::new(min, max), p.id);
            self.parts.slots.insert(p.id, (key, i as u32));
            recs.push(PartRec {
                desc: p.clone(),
                world,
                proxy,
            });
        }
        asm.parts = recs;
        asm.gate = gate;
        asm.mass = jmass.unwrap_or(mass);
        asm.com_local = com;
        asm.inertia_local = assembly_inertia(parts, com);
        // Touches of parts that left the assembly end; the rest carry on through
        // Jolt's own removed and added reports.
        let keep: BTreeSet<EntityId> = parts.iter().map(|p| p.id).collect();
        let raw = body.to_raw();
        let mut gone: Vec<SubKey> = self
            .parts
            .sub_pairs
            .iter()
            .filter(|(k, p)| {
                (k.0 == raw && !keep.contains(&p.a) && !keep.contains(&p.b))
                    || (k.2 == raw && !keep.contains(&p.a) && !keep.contains(&p.b))
            })
            .map(|(k, _)| *k)
            .collect();
        gone.sort_unstable();
        for k in gone {
            if let Some(p) = self.parts.sub_pairs.remove(&k) {
                self.parts.pair_delta(p.a, p.b, true, -1, Vec3::ZERO);
            }
        }
        if let Ok(mut b) = self.world.body_mut(body) {
            b.invalidate_contact_cache();
        }
        self.resume_joints(joints);
        Ok(())
    }

    /// Changes one part's group, flags and material without rebuilding its
    /// assembly. Fails with `RE5004` for an unknown part.
    pub fn set_part_flags(
        &mut self,
        part: EntityId,
        group: u8,
        can_collide: bool,
        can_touch: bool,
        can_query: bool,
        material: PartMaterial,
    ) -> Result<(), Diagnostic> {
        let (key, child) = *self.parts.slots.get(&part).ok_or_else(|| {
            Diagnostic::error(
                codes::UNKNOWN_ENTITY,
                format!("entity {part} is not a part"),
            )
            .at(at(part))
        })?;
        if usize::from(group) >= MAX_GROUPS {
            return Err(invalid(key, format!("part {part} is in group {group}")));
        }
        let asm = self
            .parts
            .assemblies
            .get_mut(&key)
            .expect("slot names a live assembly");
        let rec = &mut asm.parts[child as usize];
        let changed_contacts = rec.desc.group != group
            || rec.desc.can_collide != can_collide
            || rec.desc.can_touch != can_touch
            || rec.desc.material.friction != material.friction
            || rec.desc.material.elasticity != material.elasticity;
        let mass_changed = rec.desc.material.density != material.density;
        rec.desc.group = group;
        rec.desc.can_collide = can_collide;
        rec.desc.can_touch = can_touch;
        rec.desc.can_query = can_query;
        rec.desc.material = material;
        let body = asm.body;
        let mut gate = GateBody {
            compound: Arc::clone(&asm.gate.compound),
            parts: asm.gate.parts.clone(),
        };
        gate.parts[child as usize] = rec.desc.gate();
        let gate = Arc::new(gate);
        asm.gate = gate.clone();
        self.parts.gate.set_body(body, gate);
        if mass_changed && asm.kind == BodyKind::Dynamic {
            let parts: Vec<PartDesc> = asm.parts.iter().map(|p| p.desc.clone()).collect();
            return self.set_assembly_parts(key, &parts);
        }
        if changed_contacts && let Ok(mut b) = self.world.body_mut(body) {
            b.invalidate_contact_cache();
            if asm.kind != BodyKind::Static {
                b.activate();
            }
        }
        Ok(())
    }

    /// Where the simulation's origin lies in the world (the origin, unless
    /// [`set_origin`](Self::set_origin) moved it).
    pub fn origin(&self) -> Vec3 {
        self.parts.origin
    }

    /// Moves the simulation's origin to world point `origin`, so that bodies,
    /// characters and parts within Jolt's limits of it
    /// ([`WORLD_LIMIT`](crate::WORLD_LIMIT) per axis) can be simulated wherever
    /// the world puts them (ADR-0031, ADR-0042's floating origin). Everything
    /// keeps its world pose and velocity: the whole Jolt world moves by one
    /// translation, so contacts, sleep and joints carry on. Positions in and
    /// out of [`Physics`] stay world positions. Call it between steps, before
    /// the tick's commands. Bodies and characters the move leaves outside
    /// the limits stay where they are until a later move brings them back; a
    /// new position for one is kept (reported once as an `RE5006` warning)
    /// and applied then. Bodies added out of reach while the origin is moved
    /// wait, with their vehicles and constraints, and join Jolt once in
    /// reach. Ragdolls move with the world too; one whose target the move
    /// leaves out of reach stays where it is, undriven, until its target is
    /// back in reach. Fails with `RE5016`, leaving the world unchanged, for a
    /// point that is not finite or a move larger than twice the limits. A
    /// [`PhysicsSnapshot`](crate::PhysicsSnapshot) saved before the move no
    /// longer restores (`RE5012`).
    pub fn set_origin(&mut self, origin: Vec3) -> Result<(), Diagnostic> {
        let refuse = |why: &str| {
            Diagnostic::error(
                codes::INVALID_ARGUMENT,
                format!("the simulation origin cannot move to {origin}: {why}"),
            )
        };
        if !origin.is_finite() {
            return Err(refuse("it is not finite"));
        }
        if origin == self.parts.origin {
            return Ok(());
        }
        let shift = self.parts.origin - origin;
        if !crate::world::in_world(shift * 0.5) {
            return Err(refuse("the move is larger than twice the world limits"));
        }
        // Every body of the Jolt world, in a stable order: plain bodies by
        // entity, then assemblies, then characters' inner bodies, then ragdoll
        // parts by entity and part. Bodies the move leaves out of reach stay
        // where they are, far from every focus, until a later move brings
        // them back.
        let mut bodies: Vec<BodyId> = self.bodies.values().map(|b| b.id).collect();
        bodies.extend(self.parts.assemblies.values().map(|a| a.body));
        bodies.extend(self.characters.values().filter_map(|c| c.inner));
        bodies.extend(
            self.ragdolls
                .values()
                .flat_map(|r| r.bodies.iter().copied()),
        );
        self.world
            .rebase(&bodies, jq(Quat::IDENTITY), rv(shift))
            .map_err(|e| refuse(&e.to_string()))?;
        self.parts.origin = origin;
        // Bodies added while out of reach join now if they are in reach.
        let back: Vec<EntityId> = self
            .waiting
            .iter()
            .filter(|(_, d)| self.in_reach(d.pos))
            .map(|(e, _)| *e)
            .collect();
        for id in back {
            if let Err(d) = self.add_waiting(id) {
                self.diagnostics.push(d);
            }
        }
        self.catch_up();
        Ok(())
    }

    /// The smallest box holding every assembly's origin, in the world.
    pub fn assembly_extent(&self) -> Option<(Vec3, Vec3)> {
        let mut out: Option<(Vec3, Vec3)> = None;
        for a in self.parts.assemblies.values() {
            let p = self.assembly_pose_raw(a.body).pos + self.parts.origin;
            out = Some(out.map_or((p, p), |(lo, hi)| (lo.min(p), hi.max(p))));
        }
        out
    }

    /// True if world point `p` is within Jolt's limits of the simulation
    /// origin ([`WORLD_LIMIT`](crate::WORLD_LIMIT) per axis), where physics
    /// can simulate it.
    pub fn in_reach(&self, p: Vec3) -> bool {
        crate::world::in_world(p - self.parts.origin)
    }

    /// `RE5006`: `what` at world point `p` is out of reach, naming the
    /// origin when it moved.
    pub(crate) fn out_of_reach(&self, key: EntityId, what: &str, p: Vec3) -> Diagnostic {
        let mut d = crate::world::outside_world(key, what, p);
        if self.parts.origin != Vec3::ZERO {
            d.message = format!(
                "{} of the simulation origin {}",
                d.message, self.parts.origin
            );
        }
        d
    }

    pub(crate) fn jolt_point(&self, p: Vec3) -> oxijolt::RVec3 {
        rv(p - self.parts.origin)
    }

    pub(crate) fn world_point(&self, p: oxijolt::RVec3) -> Vec3 {
        grv(p) + self.parts.origin
    }

    /// An assembly body's pose in Jolt's frame (relative to the origin).
    fn assembly_pose_raw(&self, body: BodyId) -> Pose {
        self.world
            .body(body)
            .map(|b| Pose::new(grv(b.position()), gq(b.rotation())))
            .unwrap_or_default()
    }

    /// Whether `key` names an assembly.
    pub fn has_assembly(&self, key: EntityId) -> bool {
        self.parts.assemblies.contains_key(&key)
    }

    /// The assembly a part belongs to.
    pub fn assembly_of(&self, part: EntityId) -> Option<EntityId> {
        self.parts.slots.get(&part).map(|(k, _)| *k)
    }

    /// The number of assemblies.
    pub fn assembly_count(&self) -> usize {
        self.parts.assemblies.len()
    }

    /// The number of parts in all assemblies.
    pub fn part_count(&self) -> usize {
        self.parts.slots.len()
    }

    /// A part as it is now.
    pub fn part(&self, part: EntityId) -> Option<PartView> {
        let (key, child) = *self.parts.slots.get(&part)?;
        let asm = self.parts.assemblies.get(&key)?;
        Some(asm.parts[child as usize].view(key))
    }

    /// The parts of an assembly, in the order they were given.
    pub fn assembly_parts(&self, key: EntityId) -> Option<impl Iterator<Item = PartView> + '_> {
        let asm = self.parts.assemblies.get(&key)?;
        Some(asm.parts.iter().map(move |p| p.view(key)))
    }

    /// An assembly's pose, mass, centre of mass and motion.
    pub fn assembly(&self, key: EntityId) -> Option<AssemblyInfo> {
        let asm = self.parts.assemblies.get(&key)?;
        let b = self.world.body(asm.body).ok()?;
        let pose = Pose::new(self.world_point(b.position()), gq(b.rotation()));
        Some(AssemblyInfo {
            kind: asm.kind,
            pose,
            mass: if asm.kind == BodyKind::Static {
                0.0
            } else {
                asm.mass
            },
            center_of_mass: pose.point(asm.com_local),
            inertia: if asm.kind == BodyKind::Static {
                Mat3::ZERO
            } else {
                let r = Mat3::from_quat(pose.rot);
                r * asm.inertia_local * r.transpose()
            },
            linear: gv(b.linear_velocity()),
            angular: gv(b.angular_velocity()),
            awake: b.is_active(),
        })
    }

    /// Moves an assembly's origin to `pose` at once (a teleport). A moving
    /// assembly wakes; its velocity is kept.
    pub fn set_assembly_pose(&mut self, key: EntityId, pose: Pose) -> Result<(), Diagnostic> {
        let asm = self
            .parts
            .assemblies
            .get(&key)
            .ok_or_else(|| unknown_assembly(key))?;
        if !self.in_reach(pose.pos) {
            return Err(self.out_of_reach(key, "assembly", pose.pos));
        }
        let rot = unit_rotation(pose.rot)
            .ok_or_else(|| invalid(key, "its rotation is zero or not finite"))?;
        let activation = if asm.kind == BodyKind::Static {
            Activation::DontActivate
        } else {
            Activation::Activate
        };
        let body = asm.body;
        let at = self.jolt_point(pose.pos);
        self.world
            .body_mut(body)
            .and_then(|mut b| b.set_position_and_rotation(at, jq(rot), activation))
            .map_err(|e| invalid(key, e))?;
        self.refresh_assembly(key, Pose::new(pose.pos, rot));
        Ok(())
    }

    /// Sets an assembly's velocities (linear at its centre of mass). Fails with
    /// `RE5010` for a static assembly.
    pub fn set_assembly_velocity(
        &mut self,
        key: EntityId,
        linear: Vec3,
        angular: Vec3,
    ) -> Result<(), Diagnostic> {
        let asm = self
            .parts
            .assemblies
            .get(&key)
            .ok_or_else(|| unknown_assembly(key))?;
        if asm.kind == BodyKind::Static {
            return Err(Diagnostic::error(
                codes::WRONG_BODY_KIND,
                format!("assembly {key} is anchored and takes no velocity"),
            )
            .at(at(key)));
        }
        let body = asm.body;
        self.world
            .body_mut(body)
            .and_then(|mut b| {
                b.set_linear_velocity(jv(linear))?;
                b.set_angular_velocity(jv(angular))?;
                b.activate();
                Ok(())
            })
            .map_err(|e| invalid(key, e))
    }

    fn dynamic_body(&self, key: EntityId, what: &str) -> Result<BodyId, Diagnostic> {
        let asm = self
            .parts
            .assemblies
            .get(&key)
            .ok_or_else(|| unknown_assembly(key))?;
        if asm.kind != BodyKind::Dynamic {
            return Err(Diagnostic::error(
                codes::WRONG_BODY_KIND,
                format!("assembly {key} is not simulated and takes no {what}"),
            )
            .at(at(key)));
        }
        Ok(asm.body)
    }

    /// Applies an impulse to a dynamic assembly, at its centre of mass or at a
    /// world point.
    pub fn apply_assembly_impulse(
        &mut self,
        key: EntityId,
        impulse: Vec3,
        at_point: Option<Vec3>,
    ) -> Result<(), Diagnostic> {
        let body = self.dynamic_body(key, "impulse")?;
        let origin = self.parts.origin;
        self.world
            .body_mut(body)
            .and_then(|mut b| match at_point {
                Some(p) => b.add_impulse_at_point(jv(impulse), rv(p - origin)),
                None => b.add_impulse(jv(impulse)),
            })
            .map_err(|e| invalid(key, e))
    }

    /// Applies an angular impulse to a dynamic assembly.
    pub fn apply_assembly_angular_impulse(
        &mut self,
        key: EntityId,
        impulse: Vec3,
    ) -> Result<(), Diagnostic> {
        let body = self.dynamic_body(key, "angular impulse")?;
        self.world
            .body_mut(body)
            .and_then(|mut b| b.add_angular_impulse(jv(impulse)))
            .map_err(|e| invalid(key, e))
    }

    /// Adds a force (for the next step only) to a dynamic assembly, at its
    /// centre of mass or at a world point.
    pub fn add_assembly_force(
        &mut self,
        key: EntityId,
        force: Vec3,
        at_point: Option<Vec3>,
    ) -> Result<(), Diagnostic> {
        let body = self.dynamic_body(key, "force")?;
        let origin = self.parts.origin;
        self.world
            .body_mut(body)
            .and_then(|mut b| {
                match at_point {
                    Some(p) => b.add_force_at_point(jv(force), rv(p - origin))?,
                    None => b.add_force(jv(force))?,
                }
                b.activate();
                Ok(())
            })
            .map_err(|e| invalid(key, e))
    }

    /// Adds a torque (for the next step only) to a dynamic assembly.
    pub fn add_assembly_torque(&mut self, key: EntityId, torque: Vec3) -> Result<(), Diagnostic> {
        let body = self.dynamic_body(key, "torque")?;
        self.world
            .body_mut(body)
            .and_then(|mut b| {
                b.add_torque(jv(torque))?;
                b.activate();
                Ok(())
            })
            .map_err(|e| invalid(key, e))
    }

    /// Moves a kinematic assembly to `pose` over the next step.
    pub fn set_assembly_target(
        &mut self,
        key: EntityId,
        pose: Pose,
        dt: f32,
    ) -> Result<(), Diagnostic> {
        let asm = self
            .parts
            .assemblies
            .get(&key)
            .ok_or_else(|| unknown_assembly(key))?;
        if asm.kind != BodyKind::Kinematic {
            return Err(Diagnostic::error(
                codes::WRONG_BODY_KIND,
                format!("assembly {key} is not kinematic"),
            )
            .at(at(key)));
        }
        let body = asm.body;
        if !self.in_reach(pose.pos) {
            return Err(self.out_of_reach(key, "assembly", pose.pos));
        }
        let origin = self.parts.origin;
        let rot = unit_rotation(pose.rot)
            .ok_or_else(|| invalid(key, "its rotation is zero or not finite"))?;
        self.world
            .body_mut(body)
            .and_then(|mut b| b.move_kinematic(rv(pose.pos - origin), jq(rot), dt))
            .map_err(|e| invalid(key, e))
    }

    /// Sets the world's gravity.
    pub fn set_gravity(&mut self, gravity: Vec3) -> Result<(), Diagnostic> {
        self.world.set_gravity(jv(gravity)).map_err(|e| {
            Diagnostic::error(codes::INVALID_ARGUMENT, format!("gravity {gravity}: {e}"))
        })
    }

    /// The world's gravity.
    pub fn gravity(&self) -> Vec3 {
        gv(self.world.gravity())
    }

    /// Takes the touch events of the steps since the last call, in order.
    pub fn take_part_events(&mut self) -> Vec<PartEvent> {
        std::mem::take(&mut self.parts.events)
    }

    /// Writes the assemblies that moved in the steps since the last call (by the
    /// simulation, not by [`set_assembly_pose`](Self::set_assembly_pose)) into
    /// `out`, sorted by key, with their current poses.
    pub fn take_moved_assemblies(&mut self, out: &mut Vec<(EntityId, Pose)>) {
        out.clear();
        let moved = std::mem::take(&mut self.parts.moved);
        for key in moved {
            if let Some(asm) = self.parts.assemblies.get(&key) {
                out.push((key, self.assembly_pose_raw(asm.body)));
            }
        }
    }

    /// Updates the parts of `key` to the assembly pose `pose`.
    fn refresh_assembly(&mut self, key: EntityId, pose: Pose) {
        let Some(asm) = self.parts.assemblies.get_mut(&key) else {
            return;
        };
        for p in &mut asm.parts {
            p.world = pose.mul(&p.desc.local);
            let (min, max) = p.desc.geom.sanitized().box_aabb(&p.world);
            self.parts.tree.update(p.proxy, tree::Aabb::new(min, max));
        }
    }

    /// After a step: the parts of every assembly that moved follow their body.
    pub(crate) fn refresh_moved_assemblies(&mut self, slept: &[BodyId]) {
        if self.parts.assemblies.is_empty() {
            return;
        }
        let mut poses = std::mem::take(&mut self.parts.scratch);
        self.world.active_body_poses_into(&mut poses);
        let origin = self.parts.origin;
        let mut moved: Vec<(EntityId, Pose)> = poses
            .iter()
            .filter_map(|p| {
                self.parts
                    .body_key
                    .get(&p.id)
                    .map(|k| (*k, Pose::new(grv(p.position) + origin, gq(p.rotation))))
            })
            .collect();
        self.parts.scratch = poses;
        for body in slept {
            if let Some(k) = self.parts.body_key.get(body) {
                let raw = self.assembly_pose_raw(*body);
                moved.push((*k, Pose::new(raw.pos + self.parts.origin, raw.rot)));
            }
        }
        for (key, pose) in moved {
            self.refresh_assembly(key, pose);
            self.parts.moved.insert(key);
        }
    }

    /// The part on one side of a contact: an assembly's part or a plain body's
    /// entity, with its touch flag.
    fn contact_side(&self, body: BodyId, sub: oxijolt::SubShapeId) -> Option<(EntityId, bool)> {
        if let Some(key) = self.parts.body_key.get(&body) {
            let asm = self.parts.assemblies.get(key)?;
            let i = asm.gate.child(sub)?;
            let p = &asm.parts[i];
            return Some((p.desc.id, p.desc.can_touch));
        }
        self.entity_of.get(&body).map(|e| (*e, true))
    }

    fn body_active(&self, body: BodyId) -> bool {
        self.world.body(body).is_ok_and(|b| b.is_active())
    }

    /// Turns the step's contacts that involve an assembly into part events;
    /// returns the others for the plain-body path.
    pub(crate) fn gather_part_contacts(
        &mut self,
        contacts: Vec<JContactEvent>,
    ) -> Vec<JContactEvent> {
        if self.parts.assemblies.is_empty() && self.parts.sub_pairs.is_empty() {
            return contacts;
        }
        let mut rest = Vec::with_capacity(contacts.len());
        let mut readded: BTreeSet<SubKey> = BTreeSet::new();
        for c in contacts {
            let pair = c.pair();
            let involved = self.parts.body_key.contains_key(&pair.body1)
                || self.parts.body_key.contains_key(&pair.body2);
            let key = sub_key(&pair);
            if !involved && !self.parts.sub_pairs.contains_key(&key) {
                rest.push(c);
                continue;
            }
            match &c {
                JContactEvent::Added { manifold, .. } => {
                    if let Some(sp) = self.parts.sub_pairs.get_mut(&key) {
                        sp.dormant = None;
                        readded.insert(key);
                        continue;
                    }
                    let (Some((a, ta)), Some((b, tb))) = (
                        self.contact_side(pair.body1, pair.sub_shape1),
                        self.contact_side(pair.body2, pair.sub_shape2),
                    ) else {
                        continue;
                    };
                    let point = manifold
                        .points
                        .first()
                        .map_or(Vec3::ZERO, |p| grv(p.point_on1) + self.parts.origin);
                    self.parts.sub_pairs.insert(
                        key,
                        SubPair {
                            a,
                            b,
                            dormant: None,
                        },
                    );
                    self.parts.pair_delta(a, b, ta && tb, 1, point);
                }
                JContactEvent::Persisted { .. } => {}
                JContactEvent::Removed(_) => {
                    let Some(sp) = self.parts.sub_pairs.get(&key).copied() else {
                        continue;
                    };
                    let asleep = !self.body_active(pair.body1)
                        && !self.body_active(pair.body2)
                        && self.world.body(pair.body1).is_ok()
                        && self.world.body(pair.body2).is_ok();
                    if asleep {
                        if let Some(p) = self.parts.sub_pairs.get_mut(&key) {
                            p.dormant = Some(3);
                        }
                    } else {
                        self.parts.sub_pairs.remove(&key);
                        self.parts.pair_delta(sp.a, sp.b, true, -1, Vec3::ZERO);
                    }
                }
            }
        }
        // Dormant pairs whose bodies woke and did not touch again end.
        let mut ended: Vec<SubKey> = Vec::new();
        let dormant: Vec<SubKey> = self
            .parts
            .sub_pairs
            .iter()
            .filter(|(k, p)| p.dormant.is_some() && !readded.contains(k))
            .map(|(k, _)| *k)
            .collect();
        for k in dormant {
            let b1 = self.body_from_raw(k.0);
            let b2 = self.body_from_raw(k.2);
            let gone = b1.is_none() || b2.is_none();
            let awake =
                b1.is_some_and(|b| self.body_active(b)) || b2.is_some_and(|b| self.body_active(b));
            if gone {
                ended.push(k);
            } else if awake && let Some(p) = self.parts.sub_pairs.get_mut(&k) {
                let left = p.dormant.unwrap_or(1).saturating_sub(1);
                if left == 0 {
                    ended.push(k);
                } else {
                    p.dormant = Some(left);
                }
            }
        }
        ended.sort_unstable();
        for k in ended {
            if let Some(p) = self.parts.sub_pairs.remove(&k) {
                self.parts.pair_delta(p.a, p.b, true, -1, Vec3::ZERO);
            }
        }
        rest
    }

    fn body_from_raw(&self, raw: u32) -> Option<BodyId> {
        self.parts
            .body_key
            .keys()
            .chain(self.entity_of.keys())
            .find(|b| b.to_raw() == raw)
            .copied()
    }

    /// Ends the part touches of a plain body that is being removed.
    pub(crate) fn end_part_pairs_of_body(&mut self, body: BodyId) {
        if !self.parts.sub_pairs.is_empty() {
            self.parts.end_sub_pairs(body.to_raw());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_table_is_symmetric() {
        let mut t = GroupTable::default();
        assert!(t.collides(3, 4));
        t.set(3, 4, false);
        assert!(!t.collides(3, 4) && !t.collides(4, 3));
        assert!(t.collides(3, 3));
        t.reset(3);
        assert!(t.collides(4, 3));
        assert!(!t.collides(40, 0));
    }
}
