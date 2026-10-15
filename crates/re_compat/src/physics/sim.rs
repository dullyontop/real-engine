//! The simulation of a compat world: which parts exist, how they are grouped
//! into assemblies and static cells, and how the world and `re_physics` are
//! kept in step.
//!
//! [`Sim`] lives in the [`CompatPhysics`] world resource. Each frame,
//! [`Sim::sync`] brings `re_physics` up to date with the world:
//!
//! 1. **Membership.** Tree changes (an observer on `ChildOf`) and, on the
//!    first frame, the whole workspace are walked: a part joins the
//!    simulation when it is a descendant of the workspace, and leaves when it
//!    is not (moved out, destroyed). Rigid joints (`Weld`, `Motor6D`,
//!    `WeldConstraint`, ...) and attachment constraints are tracked the same
//!    way.
//! 2. **Changes.** Bevy's change ticks tell which tracked parts, joints and
//!    constraints scripts changed since the last frame. A part whose physical
//!    properties changed is rebuilt; a part a script moved teleports its
//!    assembly (physics writes `Transform` without tripping change
//!    detection, so only script writes are seen).
//! 3. **Assemblies.** The parts joined by enabled rigid joints form one
//!    assembly, one `re_physics` body. Its root is an anchored part if it has
//!    one, then the highest `RootPriority`, then a part with mass, then the
//!    largest, then the lowest entity. Joints place their second part
//!    (`Part0.CFrame * C0 * Transform * C1:Inverse()`); weld constraints keep
//!    the offset the parts have when the assembly is built.
//! 4. **Static cells** (ADR-0031). Anchored parts without joints that exist
//!    when the simulation starts are grouped into cells of
//!    [`CELL_SIZE`] studs, one static body per cell. A script that moves or
//!    changes a member evicts it: it becomes its own assembly and its cell is
//!    rebuilt without it.
//!
//! After the physics steps, [`Sim::write_back`] puts every moved part at its
//! assembly's pose: `Transform` is written without change detection (no
//! `Changed` signals, as in Roblox) and `GlobalTransform` normally.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;

use glam::{Affine3A, Quat, Vec3};
use re_core::Diagnostic;
use re_physics::{
    AssemblyDesc, GroupTable, HingeDrive, JointDesc, JointKind, PartDesc, PartEvent, PartMaterial,
    Physics, PhysicsConfig, Pose,
};
use re_reflect::Value;
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::change_detection::{DetectChanges, DetectChangesMut, Tick};
use re_scene::ecs::component::{Component, ComponentId};
use re_scene::ecs::hierarchy::ChildOf;
use re_scene::ecs::lifecycle::{Discard, Insert};
use re_scene::ecs::observer::On;
use re_scene::ecs::prelude::{QueryBuilder, QueryState, Ref};
use re_scene::ecs::prelude::{ResMut, Resource};
use re_scene::ecs::world::FilteredEntityRef;
use re_scene::{Entity, GlobalTransform, Transform, World};
use re_types::EntityId;
use re_types::physics::BodyKind;

use super::layout::{Field, Layout};
use super::pose::world_pose;
use crate::class::ClassMarker;
use crate::datatypes::PhysicalProperties;

/// The side of a static cell, in studs.
pub const CELL_SIZE: f32 = 128.0;

/// Assembly keys of static cells have this bit set (entity bits never do).
const CELL_BIT: u64 = 1 << 63;

/// The fastest an assembly is set moving by an impulse, studs/s.
const MAX_SPEED: f32 = 490.0;

/// The fastest an assembly is set turning by an impulse, rad/s (the
/// simulation caps turning at about 47).
const MAX_SPIN: f32 = 46.0;

/// The most collision groups (Default included).
pub const MAX_GROUPS: usize = 32;

/// Marks a part that something else moves (a character controller): the
/// simulation leaves it, and every part welded to it, out.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct CharacterDriven;

/// A part's custom physical properties (`BasePart.CustomPhysicalProperties`),
/// kept while the place runs.
#[derive(Component, Clone, Copy, Debug)]
pub struct CustomPhysics(pub PhysicalProperties);

/// What the observers and setters tell the simulation between frames.
#[derive(Resource, Default, Debug)]
pub struct PhysicsFeed {
    /// Entities whose parent changed (their subtrees are re-examined).
    pub(crate) tree: Vec<Entity>,
    /// Parts whose physical properties changed outside their own component.
    pub(crate) dirty: Vec<Entity>,
    /// Parts a script moved (`CFrame`, `PivotTo`, ...).
    pub(crate) moved: Vec<Entity>,
}

fn on_parented(ev: On<Insert<ChildOf>>, feed: Option<ResMut<PhysicsFeed>>) {
    if let Some(mut f) = feed {
        f.tree.push(ev.event().entity);
    }
}

fn on_unparented(ev: On<Discard<ChildOf>>, feed: Option<ResMut<PhysicsFeed>>) {
    if let Some(mut f) = feed {
        f.tree.push(ev.event().entity);
    }
}

/// The identity of a static cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellId(pub u64);

/// One static cell: anchored parts that do not move, simulated as one body.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    /// The parts, sorted.
    pub members: Vec<Entity>,
    /// Bumped whenever the members change.
    pub version: u64,
}

/// The static cells of a world, for the renderer: it draws one merged mesh
/// per cell and rebuilds the cells [`take_changed`](Self::take_changed)
/// names.
#[derive(Resource, Debug, Default)]
pub struct StaticCells {
    cells: BTreeMap<CellId, Cell>,
    changed: BTreeSet<CellId>,
}

impl StaticCells {
    /// Every cell.
    pub fn cells(&self) -> &BTreeMap<CellId, Cell> {
        &self.cells
    }

    /// The cells created, changed or removed since the last call.
    pub fn take_changed(&mut self) -> Vec<CellId> {
        std::mem::take(&mut self.changed).into_iter().collect()
    }

    /// The cell a part belongs to.
    pub fn cell_of(&self, part: Entity) -> Option<CellId> {
        self.cells
            .iter()
            .find(|(_, c)| c.members.binary_search(&part).is_ok())
            .map(|(id, _)| *id)
    }
}

/// The physical properties of a part that the simulation uses.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PartSnap {
    pub(crate) geom: re_physics::PartGeom,
    pub(crate) anchored: bool,
    pub(crate) can_collide: bool,
    pub(crate) can_query: bool,
    pub(crate) can_touch: bool,
    pub(crate) massless: bool,
    pub(crate) material: PhysicalProperties,
    pub(crate) group: String,
    pub(crate) root_priority: i64,
}

impl PartSnap {
    pub(crate) fn read(w: &World, l: &Layout, e: Entity) -> PartSnap {
        let class = w
            .ecs
            .get::<ClassMarker>(e)
            .map(|m| m.class.as_str())
            .unwrap_or("Part");
        let material = match w.ecs.get::<CustomPhysics>(e) {
            Some(c) => c.0,
            None => PhysicalProperties::of_material(
                &l.material.str(w, e).unwrap_or_else(|| "Plastic".into()),
            ),
        };
        PartSnap {
            geom: l.geom(w, e, class),
            anchored: l.anchored.bool(w, e, false),
            can_collide: l.can_collide.bool(w, e, true),
            can_query: l.can_query.bool(w, e, true),
            can_touch: l.can_touch.bool(w, e, true),
            massless: l.massless.bool(w, e, false),
            material,
            group: l.group.str(w, e).unwrap_or_else(|| "Default".into()),
            root_priority: match l.root_priority.get(w, e) {
                Some(Value::Int(i)) => i,
                Some(Value::Float(f)) => f as i64,
                _ => 0,
            },
        }
    }

    pub(crate) fn mass(&self) -> f32 {
        self.material.density.max(0.0) * self.geom.volume()
    }
}

/// A tracked part.
#[derive(Clone, Debug)]
pub(crate) struct PartRec {
    pub(crate) snap: PartSnap,
    /// Its assembly's key.
    pub(crate) asm: Option<EntityId>,
    /// Its local pose in the assembly.
    pub(crate) local: Pose,
}

/// A tracked assembly.
#[derive(Clone, Debug)]
pub(crate) struct AsmRec {
    pub(crate) members: Vec<Entity>,
    pub(crate) root: Entity,
    pub(crate) kind: BodyKind,
    pub(crate) cell: Option<CellId>,
}

/// The kinds of rigid joints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RigidKind {
    /// Weld, Motor6D, Snap, ...: `Part0 * C0 * Transform * C1⁻¹`.
    Joint,
    /// WeldConstraint: the offset at build time.
    WeldConstraint,
}

/// A tracked rigid joint, as of the last sync.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RigidRec {
    pub(crate) kind: RigidKind,
    pub(crate) part0: Option<Entity>,
    pub(crate) part1: Option<Entity>,
    pub(crate) enabled: bool,
    /// `C0 * Transform * C1⁻¹` (joints only).
    pub(crate) offset: Pose,
}

/// The kinds of attachment constraints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConKind {
    BallSocket,
    Hinge,
    Rope,
    Rod,
    LinearVelocity,
    AngularVelocity,
    AlignPosition,
    AlignOrientation,
    VectorForce,
}

impl ConKind {
    fn of(class: &str) -> Option<ConKind> {
        Some(match class {
            "BallSocketConstraint" => ConKind::BallSocket,
            "HingeConstraint" => ConKind::Hinge,
            "RopeConstraint" => ConKind::Rope,
            "RodConstraint" => ConKind::Rod,
            "LinearVelocity" => ConKind::LinearVelocity,
            "AngularVelocity" => ConKind::AngularVelocity,
            "AlignPosition" => ConKind::AlignPosition,
            "AlignOrientation" => ConKind::AlignOrientation,
            "VectorForce" => ConKind::VectorForce,
            _ => return None,
        })
    }

    pub(crate) fn is_joint(self) -> bool {
        matches!(
            self,
            ConKind::BallSocket | ConKind::Hinge | ConKind::Rope | ConKind::Rod
        )
    }
}

/// A tracked constraint.
#[derive(Clone, Debug)]
pub(crate) struct ConRec {
    pub(crate) kind: ConKind,
    /// The joint last given to `re_physics`.
    pub(crate) joint: Option<JointDesc>,
}

/// Collision groups by name.
#[derive(Clone, Debug)]
pub(crate) struct Groups {
    pub(crate) names: Vec<Option<String>>,
    pub(crate) table: GroupTable,
}

impl Default for Groups {
    fn default() -> Self {
        let mut names = vec![None; MAX_GROUPS];
        names[0] = Some("Default".to_string());
        Groups {
            names,
            table: GroupTable::default(),
        }
    }
}

impl Groups {
    /// The id of group `name` (Default for an unknown name).
    pub(crate) fn id(&self, name: &str) -> u8 {
        self.find(name).unwrap_or(0)
    }

    pub(crate) fn find(&self, name: &str) -> Option<u8> {
        self.names
            .iter()
            .position(|n| n.as_deref() == Some(name))
            .map(|i| i as u8)
    }
}

/// Velocity writes and impulses scripts made since the last frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct Pending {
    pub(crate) linear: HashMap<Entity, Vec3>,
    pub(crate) angular: HashMap<Entity, Vec3>,
    pub(crate) impulses: Vec<(Entity, Impulse)>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Impulse {
    Linear(Vec3),
    AtPoint(Vec3, Vec3),
    Angular(Vec3),
}

/// A touch between two parts, for `Touched` and `TouchEnded`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Touch {
    pub(crate) a: Entity,
    pub(crate) b: Entity,
    pub(crate) began: bool,
}

/// The simulation of one world.
pub(crate) struct Sim {
    pub(crate) physics: Physics,
    pub(crate) layout: Layout,
    pub(crate) workspace: Entity,
    last_tick: Option<Tick>,
    started: bool,
    pub(crate) parts: HashMap<Entity, PartRec>,
    pub(crate) rigid: HashMap<Entity, RigidRec>,
    pub(crate) constraints: BTreeMap<Entity, ConRec>,
    pub(crate) assemblies: HashMap<EntityId, AsmRec>,
    pub(crate) cells: HashMap<(i32, i32, i32), CellId>,
    next_cell: u64,
    pub(crate) groups: Groups,
    groups_applied: bool,
    pub(crate) pending: Pending,
    pub(crate) touches: Vec<Touch>,
    /// The parts each part touches now.
    pub(crate) touching: HashMap<Entity, BTreeSet<Entity>>,
    pub(crate) fallen: Vec<Entity>,
    /// What the simulation refused since the last step (a part outside the
    /// world limits, say): reported by `step`.
    pub(crate) refused: Vec<Diagnostic>,
    /// Parts whose assembly must be rebuilt at the next sync.
    dirty: HashSet<Entity>,
    /// Parts a script moved since the last sync.
    moved: HashSet<Entity>,
    /// Scratch for moved assemblies.
    moved_asm: Vec<(EntityId, Pose)>,
    ids: Ids,
    scans: Scans,
    pub(crate) cell_table: StaticCells,
}

/// The ECS ids of the components whose changes the sync watches.
#[derive(Clone, Copy, Debug)]
struct Ids {
    base_part: ComponentId,
    part: Option<ComponentId>,
    joint: Option<ComponentId>,
    motor6d: Option<ComponentId>,
    weld_constraint: Option<ComponentId>,
}

type OwnScan = QueryState<(Entity, FilteredEntityRef<'static, 'static>)>;

/// Queries over the components the sync watches, so a change scan walks
/// tables rather than looking entities up one by one.
struct Scans {
    transform: QueryState<(Entity, Ref<'static, Transform>)>,
    own: Vec<(ComponentId, OwnScan)>,
}

impl Scans {
    fn new(w: &mut World, own: &[ComponentId]) -> Scans {
        Scans {
            transform: w.ecs.query::<(Entity, Ref<Transform>)>(),
            own: own
                .iter()
                .map(|&id| {
                    let q = QueryBuilder::<(Entity, FilteredEntityRef)>::new(&mut w.ecs)
                        .ref_id(id)
                        .build();
                    (id, q)
                })
                .collect(),
        }
    }
}

/// The world resource holding the simulation.
#[derive(Resource)]
pub struct CompatPhysics(pub(crate) Mutex<Sim>);

/// The key of the assembly rooted at `e`.
pub(crate) fn key_of(e: Entity) -> EntityId {
    EntityId(e.to_bits())
}

/// The entity of a part key (not a cell key).
pub(crate) fn entity_of(id: EntityId) -> Option<Entity> {
    if id.0 & CELL_BIT != 0 {
        return None;
    }
    Entity::try_from_bits(id.0)
}

fn cell_key(id: CellId) -> EntityId {
    EntityId(CELL_BIT | id.0)
}

fn class_of(w: &World, e: Entity) -> Option<&str> {
    w.ecs.get::<ClassMarker>(e).map(|m| m.class.as_str())
}

fn is_descendant(w: &World, e: Entity, ancestor: Entity) -> bool {
    let mut cur = e;
    while let Some(c) = w.ecs.get::<ChildOf>(cur) {
        cur = c.parent();
        if cur == ancestor {
            return true;
        }
    }
    false
}

fn rigid_kind(class: &str) -> Option<RigidKind> {
    match class {
        "Weld" | "ManualWeld" | "Snap" | "Motor" | "Motor6D" | "Glue" | "ManualGlue" => {
            Some(RigidKind::Joint)
        }
        "WeldConstraint" => Some(RigidKind::WeldConstraint),
        _ => None,
    }
}

/// Prepares `w` for a simulation: the tree observers and the feed.
pub(crate) fn prepare(w: &mut World) {
    if !w.ecs.contains_resource::<PhysicsFeed>() {
        w.ecs.insert_resource(PhysicsFeed::default());
        w.ecs.add_observer(on_parented);
        w.ecs.add_observer(on_unparented);
    }
}

impl Sim {
    /// A simulation for `w`, if it is a compat world with a workspace and the
    /// part classes.
    pub(crate) fn new(w: &mut World) -> Option<Sim> {
        let layout = Layout::find(w)?;
        let game = w.entity(crate::GAME_ID).ok()?;
        let workspace = crate::services::core::find_service(w, game, "Workspace")?;
        prepare(w);
        let comp = |class: &str| {
            w.registry
                .try_key(&crate::class::own_component_name(class))
                .map(|k| w.registry.component_id(k))
        };
        let ids = Ids {
            base_part: layout.size.component(w),
            part: comp("Part"),
            joint: comp("JointInstance"),
            motor6d: comp("Motor6D"),
            weld_constraint: comp("WeldConstraint"),
        };
        let own: Vec<ComponentId> = [Some(ids.base_part), ids.part]
            .into_iter()
            .flatten()
            .collect();
        let scans = Scans::new(w, &own);
        let config = PhysicsConfig {
            gravity: Vec3::new(0.0, -196.2, 0.0),
            max_bodies: 262_144,
            max_body_pairs: 131_072,
            max_contact_constraints: 131_072,
            ..PhysicsConfig::default()
        };
        let physics = Physics::new(config, re_core::TaskPool::global()).ok()?;
        Some(Sim {
            physics,
            layout,
            workspace,
            last_tick: None,
            started: false,
            parts: HashMap::new(),
            rigid: HashMap::new(),
            constraints: BTreeMap::new(),
            assemblies: HashMap::new(),
            cells: HashMap::new(),
            next_cell: 0,
            groups: Groups::default(),
            groups_applied: false,
            pending: Pending::default(),
            touches: Vec::new(),
            touching: HashMap::new(),
            fallen: Vec::new(),
            refused: Vec::new(),
            dirty: HashSet::new(),
            moved: HashSet::new(),
            moved_asm: Vec::new(),
            ids,
            scans,
            cell_table: StaticCells::default(),
        })
    }

    /// Brings the physics world up to date with `w`.
    ///
    /// A full sync also scans every tracked part, joint and constraint for
    /// changes (once per frame, before stepping). Without it, only what the
    /// feed names is seen (parts added, removed, moved through the part
    /// members, or given custom physical properties): enough for the queries
    /// scripts make between frames.
    pub(crate) fn sync(&mut self, w: &mut World, full: bool) {
        let this = w.ecs.change_tick();
        let (tree, dirty, moved) = w
            .ecs
            .get_resource_mut::<PhysicsFeed>()
            .map(|mut f| {
                (
                    std::mem::take(&mut f.tree),
                    std::mem::take(&mut f.dirty),
                    std::mem::take(&mut f.moved),
                )
            })
            .unwrap_or_default();
        let initial = !self.started;
        if initial {
            self.started = true;
            let all = crate::instance::tree::descendants(w, self.workspace);
            self.examine(w, &all, true);
        } else {
            let mut seen = HashSet::new();
            let mut list = Vec::new();
            for e in tree {
                if !seen.insert(e) {
                    continue;
                }
                list.push(e);
                if w.ecs.get_entity(e).is_ok() {
                    for d in crate::instance::tree::descendants(w, e) {
                        if seen.insert(d) {
                            list.push(d);
                        }
                    }
                }
            }
            self.examine(w, &list, false);
        }
        for e in dirty {
            if self.parts.contains_key(&e) {
                self.dirty.insert(e);
            }
        }
        // On the first sync every part is placed where it is; earlier moves
        // do not count (they would take anchored parts out of their cells).
        for e in moved {
            if !initial && self.parts.contains_key(&e) {
                self.moved.insert(e);
            }
        }
        if full {
            if let Some(last) = self.last_tick {
                self.detect_changes(w, last, this);
            }
            self.last_tick = Some(this);
        }
        if !self.groups_applied {
            self.physics.set_collision_groups(self.groups.table.clone());
            self.groups_applied = true;
        }
        self.rebuild(w);
        self.teleport(w);
        self.sync_constraints(w);
        self.apply_pending();
        if let Some(g) = self.layout.gravity {
            let g = g.f32(w, self.workspace, 196.2);
            if g.is_finite() && (self.physics.gravity().y + g).abs() > 1e-6 {
                let _ = self.physics.set_gravity(Vec3::new(0.0, -g, 0.0));
            }
        }
        // Writes after this sync carry a later tick than `this`.
        if full {
            w.ecs.increment_change_tick();
        }
    }

    /// Looks at `list` (tree changes, or the whole workspace at the start):
    /// tracks what is now a simulated part, joint or constraint, and forgets
    /// what no longer is.
    /// Before the first assembly: when the parts reach past what the
    /// simulation holds around the world origin, its origin moves to the
    /// middle of them, so a map centred away from the origin (or a little
    /// wider than the limits allow) still simulates whole.
    fn place_origin(&mut self, w: &World) {
        // Room kept for parts that move outward from the map.
        const MARGIN: f32 = 500.0;
        let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for e in self.parts.keys() {
            if let Some(p) = world_pose(w, *e).filter(|p| p.pos.is_finite()) {
                lo = lo.min(p.pos);
                hi = hi.max(p.pos);
            }
        }
        if !lo.is_finite() {
            return;
        }
        let reach = re_physics::WORLD_LIMIT - MARGIN;
        let fits_now = lo.min_element() >= -reach && hi.max_element() <= reach;
        // A map wider than the limits stays where it is: its far parts are
        // reported (RE5006) wherever the origin goes.
        let too_wide = (hi - lo).max_element() > 2.0 * re_physics::WORLD_LIMIT;
        if fits_now || too_wide {
            return;
        }
        // On a coarse grid, so the origin is exact in f32 and easy to read.
        let origin = ((lo + hi) * 0.5 / 64.0).round() * 64.0;
        let _ = self.physics.set_origin(origin);
    }

    /// Moves the simulation origin to the middle of every assembly and `pos`,
    /// when they all fit around one origin; whether it moved.
    fn recentre(&mut self, pos: Vec3) -> bool {
        let (lo, hi) = self
            .physics
            .assembly_extent()
            .map_or((pos, pos), |(lo, hi)| (lo.min(pos), hi.max(pos)));
        if (hi - lo).max_element() > 2.0 * (re_physics::WORLD_LIMIT - 64.0) {
            return false;
        }
        let origin = ((lo + hi) * 0.5 / 64.0).round() * 64.0;
        self.physics.set_origin(origin).is_ok()
    }

    fn examine(&mut self, w: &World, list: &[Entity], initial: bool) {
        let mut statics = Vec::new();
        for &e in list {
            let alive = w.ecs.get_entity(e).is_ok();
            let in_ws = alive && is_descendant(w, e, self.workspace);
            let class = if alive { class_of(w, e) } else { None };
            // Parts.
            let is_part = alive
                && self.layout.is_part(w, e)
                && class != Some("Terrain")
                && w.ecs.get::<CharacterDriven>(e).is_none();
            if in_ws && is_part {
                if !self.parts.contains_key(&e) {
                    let snap = PartSnap::read(w, &self.layout, e);
                    let anchored = snap.anchored;
                    self.parts.insert(
                        e,
                        PartRec {
                            snap,
                            asm: None,
                            local: Pose::IDENTITY,
                        },
                    );
                    if initial && anchored {
                        statics.push(e);
                    } else {
                        self.dirty.insert(e);
                    }
                }
            } else if self.parts.contains_key(&e) {
                self.untrack_part(e);
            }
            // Rigid joints.
            match class.and_then(rigid_kind) {
                Some(kind) if in_ws => {
                    let rec = self.read_rigid(w, e, kind);
                    self.set_rigid(e, rec);
                }
                _ => {
                    if let Some(old) = self.rigid.remove(&e) {
                        self.touch_joint_parts(&old);
                    }
                }
            }
            // Attachment constraints.
            match class.and_then(ConKind::of) {
                Some(kind) if in_ws => {
                    self.constraints
                        .entry(e)
                        .or_insert(ConRec { kind, joint: None });
                }
                _ => {
                    if self.constraints.remove(&e).is_some() {
                        self.physics.remove_joint(key_of(e));
                    }
                }
            }
        }
        if initial {
            self.place_origin(w);
        }
        if !statics.is_empty() {
            // Anchored parts with a joint are assemblies, not cell members.
            let jointed: HashSet<Entity> = self
                .rigid
                .values()
                .filter(|r| r.enabled)
                .flat_map(|r| [r.part0, r.part1])
                .flatten()
                .collect();
            let (cellable, rest): (Vec<Entity>, Vec<Entity>) =
                statics.into_iter().partition(|e| !jointed.contains(e));
            self.dirty.extend(rest);
            self.add_to_cells(w, &cellable);
        }
    }

    fn read_rigid(&self, w: &World, e: Entity, kind: RigidKind) -> RigidRec {
        match kind {
            RigidKind::Joint => {
                let j = &self.layout.joint;
                let c0 = j.c0.map(|f| f.pose(w, e)).unwrap_or(Pose::IDENTITY);
                let c1 = j.c1.map(|f| f.pose(w, e)).unwrap_or(Pose::IDENTITY);
                let t = match (class_of(w, e), j.transform) {
                    (Some("Motor6D"), Some(f)) => f.pose(w, e),
                    _ => Pose::IDENTITY,
                };
                RigidRec {
                    kind,
                    part0: j.part0.and_then(|f| f.entity(w, e)),
                    part1: j.part1.and_then(|f| f.entity(w, e)),
                    enabled: j.enabled.is_none_or(|f| f.bool(w, e, true)),
                    offset: c0.mul(&t).mul(&c1.inverse()),
                }
            }
            RigidKind::WeldConstraint => {
                let j = &self.layout.weld_constraint;
                RigidRec {
                    kind,
                    part0: j.part0.and_then(|f| f.entity(w, e)),
                    part1: j.part1.and_then(|f| f.entity(w, e)),
                    enabled: j.enabled.is_none_or(|f| f.bool(w, e, true)),
                    offset: Pose::IDENTITY,
                }
            }
        }
    }

    fn set_rigid(&mut self, e: Entity, rec: RigidRec) {
        match self.rigid.get(&e) {
            Some(old) if *old == rec => {}
            Some(old) => {
                let old = old.clone();
                self.touch_joint_parts(&old);
                self.touch_joint_parts(&rec);
                self.rigid.insert(e, rec);
            }
            None => {
                self.touch_joint_parts(&rec);
                self.rigid.insert(e, rec);
            }
        }
    }

    fn touch_joint_parts(&mut self, r: &RigidRec) {
        for p in [r.part0, r.part1].into_iter().flatten() {
            if self.parts.contains_key(&p) {
                self.dirty.insert(p);
            }
        }
    }

    /// Takes a part out of the simulation; its assembly's other parts are
    /// rebuilt.
    fn untrack_part(&mut self, e: Entity) {
        let Some(rec) = self.parts.remove(&e) else {
            return;
        };
        self.dirty.remove(&e);
        self.moved.remove(&e);
        self.pending.linear.remove(&e);
        self.pending.angular.remove(&e);
        if let Some(key) = rec.asm {
            self.detach_from(key, e);
        }
    }

    /// Removes `e` from assembly `key`: a cell is rebuilt without it, any
    /// other assembly is dissolved and its other members rebuilt.
    fn detach_from(&mut self, key: EntityId, e: Entity) {
        let Some(asm) = self.assemblies.get(&key).cloned() else {
            return;
        };
        if let Some(cell) = asm.cell {
            let members: Vec<Entity> = asm.members.iter().copied().filter(|m| *m != e).collect();
            self.set_cell(cell, members);
            if let Some(p) = self.parts.get_mut(&e) {
                p.asm = None;
            }
            return;
        }
        let _ = self.physics.remove_assembly(key);
        self.assemblies.remove(&key);
        for m in asm.members {
            if let Some(p) = self.parts.get_mut(&m) {
                p.asm = None;
                self.dirty.insert(m);
            }
        }
        self.dirty.remove(&e);
        if !self.parts.contains_key(&e) {
            return;
        }
        self.dirty.insert(e);
    }

    /// Finds what scripts changed since `last`: part properties, part poses,
    /// joints and constraints.
    fn detect_changes(&mut self, w: &World, last: Tick, this: Tick) {
        let ids = self.ids;
        let changed = |e: Entity, id: ComponentId| -> bool {
            w.ecs
                .get_entity(e)
                .ok()
                .and_then(|er| er.get_change_ticks_by_id(id))
                .is_some_and(|t| t.is_changed(last, this))
        };
        // Walk the components' tables: most parts changed nothing.
        let mut props = Vec::new();
        for (e, t) in self.scans.transform.iter(&w.ecs) {
            if t.last_changed().is_newer_than(last, this) && self.parts.contains_key(&e) {
                self.moved.insert(e);
            }
        }
        for (id, q) in &mut self.scans.own {
            for (e, r) in q.iter(&w.ecs) {
                if r.get_change_ticks_by_id(*id)
                    .is_some_and(|t| t.is_changed(last, this))
                    && self.parts.contains_key(&e)
                {
                    props.push(e);
                }
            }
        }
        props.sort_unstable();
        props.dedup();
        for e in props {
            let snap = PartSnap::read(w, &self.layout, e);
            let rec = self.parts.get_mut(&e).expect("tracked");
            if rec.snap != snap {
                rec.snap = snap;
                self.dirty.insert(e);
            }
        }
        let rigid: Vec<Entity> = self.rigid.keys().copied().collect();
        for e in rigid {
            let touched = [ids.joint, ids.motor6d, ids.weld_constraint]
                .into_iter()
                .flatten()
                .any(|id| changed(e, id));
            if touched && let Some(kind) = self.rigid.get(&e).map(|r| r.kind) {
                let rec = self.read_rigid(w, e, kind);
                self.set_rigid(e, rec);
            }
        }
    }

    /// Rebuilds the assemblies of the dirty parts.
    fn rebuild(&mut self, w: &mut World) {
        if self.dirty.is_empty() {
            return;
        }
        // Everything in the dirty parts' assemblies is rebuilt with them; a
        // dirty cell member leaves its cell.
        let mut work: HashSet<Entity> = HashSet::new();
        let dirty: Vec<Entity> = std::mem::take(&mut self.dirty).into_iter().collect();
        for e in dirty {
            if !self.parts.contains_key(&e) {
                continue;
            }
            work.insert(e);
            if let Some(key) = self.parts[&e].asm
                && let Some(asm) = self.assemblies.get(&key)
            {
                if asm.cell.is_some() {
                    self.detach_from(key, e);
                } else {
                    work.extend(asm.members.iter().copied());
                }
            }
        }
        // Rigid adjacency among tracked parts.
        let mut adj: HashMap<Entity, Vec<(Entity, Entity)>> = HashMap::new();
        for (&j, r) in &self.rigid {
            let (Some(a), Some(b)) = (r.part0, r.part1) else {
                continue;
            };
            if !r.enabled || a == b || !self.parts.contains_key(&a) || !self.parts.contains_key(&b)
            {
                continue;
            }
            adj.entry(a).or_default().push((b, j));
            adj.entry(b).or_default().push((a, j));
        }
        // Grow the work set to whole components (a new weld can reach parts
        // that were not dirty).
        let mut stack: Vec<Entity> = work.iter().copied().collect();
        while let Some(e) = stack.pop() {
            for &(n, _) in adj.get(&e).map(Vec::as_slice).unwrap_or(&[]) {
                if work.insert(n) {
                    stack.push(n);
                    if let Some(key) = self.parts[&n].asm
                        && let Some(asm) = self.assemblies.get(&key)
                        && asm.cell.is_some()
                    {
                        self.detach_from(key, n);
                    }
                }
            }
        }
        let mut work: Vec<Entity> = work.into_iter().collect();
        work.sort_unstable();
        // The old assemblies of the work set go; their velocities carry over.
        let mut carried: HashMap<Entity, (Vec3, Vec3)> = HashMap::new();
        let mut old_keys = BTreeSet::new();
        for e in &work {
            if let Some(key) = self.parts[e].asm {
                old_keys.insert(key);
            }
        }
        for key in old_keys {
            let Some(asm) = self.assemblies.get(&key) else {
                continue;
            };
            if asm.cell.is_some() {
                continue;
            }
            if let Some(info) = self.physics.assembly(key) {
                for m in &asm.members {
                    carried.insert(*m, (info.linear, info.angular));
                }
            }
            let _ = self.physics.remove_assembly(key);
            for m in self
                .assemblies
                .remove(&key)
                .map(|a| a.members)
                .unwrap_or_default()
            {
                if let Some(p) = self.parts.get_mut(&m) {
                    p.asm = None;
                }
            }
        }
        // Components.
        let mut done: HashSet<Entity> = HashSet::new();
        for &start in &work {
            if done.contains(&start) {
                continue;
            }
            let mut comp = vec![start];
            done.insert(start);
            let mut i = 0;
            while i < comp.len() {
                let e = comp[i];
                i += 1;
                for &(n, _) in adj.get(&e).map(Vec::as_slice).unwrap_or(&[]) {
                    if done.insert(n) {
                        comp.push(n);
                    }
                }
            }
            comp.sort_unstable();
            self.build_assembly(w, &comp, &adj, &carried);
        }
    }

    /// Chooses the root of a set of parts.
    fn choose_root(&self, comp: &[Entity]) -> Entity {
        *comp
            .iter()
            .max_by(|a, b| {
                let (pa, pb) = (&self.parts[a].snap, &self.parts[b].snap);
                pa.anchored
                    .cmp(&pb.anchored)
                    .then(pa.root_priority.cmp(&pb.root_priority))
                    .then((!pa.massless).cmp(&!pb.massless))
                    .then(pa.geom.volume().total_cmp(&pb.geom.volume()))
                    .then(b.cmp(a))
            })
            .expect("a component has a part")
    }

    /// Creates the assembly of one connected set of parts.
    fn build_assembly(
        &mut self,
        w: &mut World,
        comp: &[Entity],
        adj: &HashMap<Entity, Vec<(Entity, Entity)>>,
        carried: &HashMap<Entity, (Vec3, Vec3)>,
    ) {
        let root = self.choose_root(comp);
        let root_pose = world_pose(w, root).unwrap_or(Pose::IDENTITY);
        // Place every part from the root through the joints.
        let mut placed: HashMap<Entity, Pose> = HashMap::new();
        placed.insert(root, root_pose);
        let mut queue = vec![root];
        let mut i = 0;
        while i < queue.len() {
            let e = queue[i];
            i += 1;
            let pe = placed[&e];
            for &(n, j) in adj.get(&e).map(Vec::as_slice).unwrap_or(&[]) {
                if placed.contains_key(&n) {
                    continue;
                }
                let r = &self.rigid[&j];
                let pn = match r.kind {
                    RigidKind::Joint => {
                        if r.part0 == Some(e) {
                            pe.mul(&r.offset)
                        } else {
                            pe.mul(&r.offset.inverse())
                        }
                    }
                    RigidKind::WeldConstraint => {
                        // Keep the offset the two parts have now.
                        let (we, wn) = (
                            world_pose(w, e).unwrap_or(pe),
                            world_pose(w, n).unwrap_or(Pose::IDENTITY),
                        );
                        pe.mul(&we.inverse().mul(&wn))
                    }
                };
                placed.insert(n, pn);
                queue.push(n);
            }
        }
        let anchored = comp.iter().any(|e| self.parts[e].snap.anchored);
        let kind = if anchored {
            BodyKind::Static
        } else {
            BodyKind::Dynamic
        };
        let inv_root = root_pose.inverse();
        let mut descs = Vec::with_capacity(comp.len());
        let mut joint_moved = Vec::new();
        for &e in comp {
            let local = inv_root.mul(&placed[&e]);
            let local = Pose::new(local.pos, local.rot.normalize());
            descs.push(self.part_desc(e, local, e == root));
            if let Some(p) = self.parts.get_mut(&e) {
                p.local = local;
            }
            // A joint moved this part: write its new pose.
            if e != root {
                let now = world_pose(w, e).unwrap_or(Pose::IDENTITY);
                let want = placed[&e];
                if (now.pos - want.pos).length_squared() > 1e-10
                    || now.rot.angle_between(want.rot) > 1e-5
                {
                    joint_moved.push((e, want));
                }
            }
        }
        write_world_poses(w, &joint_moved);
        let (linear, angular) = carried.get(&root).copied().unwrap_or_default();
        let key = key_of(root);
        let desc = AssemblyDesc {
            kind,
            pose: root_pose,
            linear,
            angular,
            parts: descs,
        };
        let mut added = self.physics.add_assembly(key, &desc);
        if added
            .as_ref()
            .is_err_and(|d| d.code == re_physics::codes::OUTSIDE_WORLD)
            && self.recentre(root_pose.pos)
        {
            added = self.physics.add_assembly(key, &desc);
        }
        if let Err(d) = added {
            // The parts stay where they are, unsimulated, until they change.
            self.refused.push(d);
            return;
        }
        for &e in comp {
            if let Some(p) = self.parts.get_mut(&e) {
                p.asm = Some(key);
            }
        }
        self.assemblies.insert(
            key,
            AsmRec {
                members: comp.to_vec(),
                root,
                kind,
                cell: None,
            },
        );
        // Joints on these parts are re-made against the new assembly.
        for c in self.constraints.values_mut() {
            c.joint = None;
        }
    }

    fn part_desc(&self, e: Entity, local: Pose, is_root: bool) -> PartDesc {
        let s = &self.parts[&e].snap;
        PartDesc {
            id: key_of(e),
            geom: s.geom,
            local,
            material: PartMaterial {
                density: s.material.density,
                friction: s.material.friction,
                elasticity: s.material.elasticity,
                friction_weight: s.material.friction_weight,
                elasticity_weight: s.material.elasticity_weight,
            },
            massless: s.massless && !is_root,
            group: self.groups.id(&s.group),
            can_collide: s.can_collide,
            can_touch: s.can_touch,
            can_query: s.can_query,
        }
    }

    /// Puts anchored parts into the cells their positions fall in.
    fn add_to_cells(&mut self, w: &World, parts: &[Entity]) {
        let mut by_cell: BTreeMap<(i32, i32, i32), Vec<Entity>> = BTreeMap::new();
        for &e in parts {
            let p = world_pose(w, e).unwrap_or(Pose::IDENTITY).pos;
            let c = (p / CELL_SIZE).floor();
            by_cell
                .entry((c.x as i32, c.y as i32, c.z as i32))
                .or_default()
                .push(e);
        }
        for (coord, mut members) in by_cell {
            let id = match self.cells.get(&coord) {
                Some(id) => *id,
                None => {
                    let id = CellId(self.next_cell);
                    self.next_cell += 1;
                    self.cells.insert(coord, id);
                    id
                }
            };
            if let Some(asm) = self.assemblies.get(&cell_key(id)) {
                members.extend(asm.members.iter().copied());
            }
            // Poses relative to the cell's corner.
            let origin = Vec3::new(coord.0 as f32, coord.1 as f32, coord.2 as f32) * CELL_SIZE;
            for &e in &members {
                let p = world_pose(w, e).unwrap_or(Pose::IDENTITY);
                if let Some(rec) = self.parts.get_mut(&e) {
                    rec.local = Pose::new(p.pos - origin, p.rot.normalize());
                }
            }
            self.set_cell(id, members);
        }
    }

    /// Gives cell `id` exactly `members` (removing it when empty).
    fn set_cell(&mut self, id: CellId, mut members: Vec<Entity>) {
        members.sort_unstable();
        members.dedup();
        let key = cell_key(id);
        let origin = self
            .cells
            .iter()
            .find(|(_, c)| **c == id)
            .map(|(k, _)| Vec3::new(k.0 as f32, k.1 as f32, k.2 as f32) * CELL_SIZE)
            .unwrap_or(Vec3::ZERO);
        if members.is_empty() {
            let _ = self.physics.remove_assembly(key);
            self.assemblies.remove(&key);
            if self.cell_table.cells.remove(&id).is_some() {
                self.cell_table.changed.insert(id);
            }
            return;
        }
        let descs: Vec<PartDesc> = members
            .iter()
            .map(|e| self.part_desc(*e, self.parts[e].local, false))
            .collect();
        let ok = if self.assemblies.contains_key(&key) {
            self.physics.set_assembly_parts(key, &descs).is_ok()
        } else {
            self.physics
                .add_assembly(
                    key,
                    &AssemblyDesc {
                        kind: BodyKind::Static,
                        pose: Pose::new(origin, Quat::IDENTITY),
                        linear: Vec3::ZERO,
                        angular: Vec3::ZERO,
                        parts: descs,
                    },
                )
                .is_ok()
        };
        if !ok {
            return;
        }
        for e in &members {
            if let Some(p) = self.parts.get_mut(e) {
                p.asm = Some(key);
            }
        }
        let version = self.cell_table.cells.get(&id).map_or(0, |c| c.version + 1);
        self.cell_table.cells.insert(
            id,
            Cell {
                members: members.clone(),
                version,
            },
        );
        self.cell_table.changed.insert(id);
        self.assemblies.insert(
            key,
            AsmRec {
                root: members[0],
                members,
                kind: BodyKind::Static,
                cell: Some(id),
            },
        );
    }

    /// Moves the assemblies of the parts scripts moved.
    fn teleport(&mut self, w: &mut World) {
        if self.moved.is_empty() {
            return;
        }
        let mut moved: Vec<Entity> = std::mem::take(&mut self.moved).into_iter().collect();
        moved.sort_unstable();
        // Cell members leave their cells first.
        let mut evicted = false;
        for &e in &moved {
            if let Some(key) = self.parts.get(&e).and_then(|p| p.asm)
                && self.assemblies.get(&key).is_some_and(|a| a.cell.is_some())
            {
                self.detach_from(key, e);
                self.dirty.insert(e);
                evicted = true;
            }
        }
        if evicted {
            self.rebuild(w);
        }
        let mut done: BTreeSet<EntityId> = BTreeSet::new();
        for &e in &moved {
            let Some(rec) = self.parts.get(&e) else {
                continue;
            };
            let Some(key) = rec.asm else { continue };
            if !done.insert(key) {
                continue;
            }
            let Some(asm) = self.assemblies.get(&key) else {
                continue;
            };
            let (root, members) = (asm.root, asm.members.clone());
            // The root's new pose wins; otherwise the first moved member's.
            let mover = if moved.binary_search(&root).is_ok() {
                root
            } else {
                e
            };
            let Some(now) = world_pose(w, mover) else {
                continue;
            };
            let local = self.parts[&mover].local;
            let pose = now.mul(&local.inverse());
            let pose = Pose::new(pose.pos, pose.rot.normalize());
            let mut placed = self.physics.set_assembly_pose(key, pose);
            if placed
                .as_ref()
                .is_err_and(|d| d.code == re_physics::codes::OUTSIDE_WORLD)
                && self.recentre(pose.pos)
            {
                placed = self.physics.set_assembly_pose(key, pose);
            }
            if placed.is_err() {
                // Rebuilt at the next sync, which reports why it cannot be.
                self.dirty.extend(members);
                continue;
            }
            // The other members follow.
            let follow: Vec<(Entity, Pose)> = members
                .iter()
                .filter(|m| **m != mover)
                .map(|m| (*m, pose.mul(&self.parts[m].local)))
                .collect();
            write_world_poses(w, &follow);
        }
    }

    /// Gives `re_physics` the joints of the attachment constraints.
    fn sync_constraints(&mut self, w: &World) {
        let dropped = self.physics.take_dropped_joints();
        for id in dropped {
            if let Some(e) = entity_of(id)
                && let Some(c) = self.constraints.get_mut(&e)
            {
                c.joint = None;
            }
        }
        let keys: Vec<Entity> = self.constraints.keys().copied().collect();
        for e in keys {
            let kind = self.constraints[&e].kind;
            if !kind.is_joint() {
                continue;
            }
            let want = super::constraints::joint_desc(self, w, e, kind);
            let have = self.constraints[&e].joint;
            if want == have {
                continue;
            }
            let id = key_of(e);
            match want {
                None => self.physics.remove_joint(id),
                Some(d) => {
                    let only_drive = matches!(
                        (have, d.kind),
                        (
                            Some(JointDesc { kind: JointKind::Hinge { limits: l0, .. }, a, b, frame_a, frame_b }),
                            JointKind::Hinge { limits: l1, .. },
                        ) if l0 == l1 && a == d.a && b == d.b && frame_a == d.frame_a && frame_b == d.frame_b
                    );
                    let res = match (only_drive, d.kind) {
                        (true, JointKind::Hinge { drive, .. }) => {
                            self.physics.set_hinge_drive(id, drive)
                        }
                        _ => self.physics.set_joint(id, &d),
                    };
                    if res.is_err() {
                        self.constraints.get_mut(&e).expect("listed").joint = None;
                        continue;
                    }
                }
            }
            self.constraints.get_mut(&e).expect("listed").joint = want;
        }
        let _ = HingeDrive::Free;
    }

    /// Applies the velocities and impulses scripts asked for.
    fn apply_pending(&mut self) {
        let p = std::mem::take(&mut self.pending);
        let mut lin: Vec<(Entity, Vec3)> = p.linear.into_iter().collect();
        lin.sort_by_key(|(e, _)| *e);
        let mut ang: Vec<(Entity, Vec3)> = p.angular.into_iter().collect();
        ang.sort_by_key(|(e, _)| *e);
        for (e, v) in lin {
            if let Some(key) = self.parts.get(&e).and_then(|r| r.asm)
                && let Some(info) = self.physics.assembly(key)
            {
                let v = v.clamp_length_max(MAX_SPEED);
                let _ = self.physics.set_assembly_velocity(key, v, info.angular);
            }
        }
        for (e, v) in ang {
            if let Some(key) = self.parts.get(&e).and_then(|r| r.asm)
                && let Some(info) = self.physics.assembly(key)
            {
                let v = v.clamp_length_max(MAX_SPIN);
                let _ = self.physics.set_assembly_velocity(key, info.linear, v);
            }
        }
        for (e, imp) in p.impulses {
            let Some(key) = self.parts.get(&e).and_then(|r| r.asm) else {
                continue;
            };
            let res = match imp {
                Impulse::Linear(i) => self.physics.apply_assembly_impulse(key, i, None),
                Impulse::AtPoint(i, at) => self.physics.apply_assembly_impulse(key, i, Some(at)),
                Impulse::Angular(i) => self.physics.apply_assembly_angular_impulse(key, i),
            };
            if res.is_err() {
                // Too big a change for one step: apply it as a velocity
                // change, capped at the fastest the simulation allows.
                self.impulse_as_velocity(key, imp);
            }
        }
    }

    fn impulse_as_velocity(&mut self, key: EntityId, imp: Impulse) {
        let Some(info) = self.physics.assembly(key) else {
            return;
        };
        if info.kind != BodyKind::Dynamic || info.mass <= 0.0 {
            return;
        }
        let inv_i = if info.inertia.determinant().abs() > 1e-12 {
            info.inertia.inverse()
        } else {
            glam::Mat3::ZERO
        };
        let (dv, dw) = match imp {
            Impulse::Linear(i) => (i / info.mass, Vec3::ZERO),
            Impulse::AtPoint(i, at) => (i / info.mass, inv_i * (at - info.center_of_mass).cross(i)),
            Impulse::Angular(i) => (Vec3::ZERO, inv_i * i),
        };
        let cap = |v: Vec3, max: f32| {
            let l = v.length();
            if l > max && l.is_finite() {
                v * (max / l)
            } else {
                v
            }
        };
        let lin = cap(info.linear + dv, MAX_SPEED);
        let ang = cap(info.angular + dw, MAX_SPIN);
        if lin.is_finite() && ang.is_finite() {
            let _ = self.physics.set_assembly_velocity(key, lin, ang);
        }
    }

    /// After the steps: moved parts follow their assemblies; touches and
    /// fallen parts are collected.
    pub(crate) fn write_back(&mut self, w: &mut World) {
        let mut moved = std::mem::take(&mut self.moved_asm);
        self.physics.take_moved_assemblies(&mut moved);
        let fallen_height = self
            .layout
            .fallen_height
            .map(|f| f.f32(w, self.workspace, -500.0))
            .unwrap_or(-500.0);
        let mut targets = Vec::new();
        for (key, pose) in &moved {
            let Some(asm) = self.assemblies.get(key) else {
                continue;
            };
            if asm.cell.is_some() {
                continue;
            }
            for &m in &asm.members {
                let Some(rec) = self.parts.get(&m) else {
                    continue;
                };
                let world = pose.mul(&rec.local);
                targets.push((m, world));
                if asm.kind == BodyKind::Dynamic && world.pos.y < fallen_height {
                    self.fallen.push(m);
                }
            }
        }
        write_world_poses(w, &targets);
        self.moved_asm = moved;
        for ev in self.physics.take_part_events() {
            let (a, b, began) = match ev {
                PartEvent::Begin { a, b, .. } => (a, b, true),
                PartEvent::End { a, b } => (a, b, false),
            };
            if let (Some(a), Some(b)) = (entity_of(a), entity_of(b)) {
                if began {
                    self.touching.entry(a).or_default().insert(b);
                    self.touching.entry(b).or_default().insert(a);
                } else {
                    for (x, y) in [(a, b), (b, a)] {
                        if let Some(t) = self.touching.get_mut(&x) {
                            t.remove(&y);
                            if t.is_empty() {
                                self.touching.remove(&x);
                            }
                        }
                    }
                }
                self.touches.push(Touch { a, b, began });
            }
        }
    }

    /// Copies the cells that changed into the world's [`StaticCells`].
    pub(crate) fn publish_cells(&mut self, w: &mut World) {
        let changed = std::mem::take(&mut self.cell_table.changed);
        if changed.is_empty() {
            return;
        }
        if !w.ecs.contains_resource::<StaticCells>() {
            w.ecs.insert_resource(StaticCells::default());
        }
        let mut out = w.ecs.resource_mut::<StaticCells>();
        for id in changed {
            match self.cell_table.cells.get(&id) {
                Some(c) => {
                    out.cells.insert(id, c.clone());
                }
                None => {
                    out.cells.remove(&id);
                }
            }
            out.changed.insert(id);
        }
    }

    /// The group bits of group `name`: which groups it collides with.
    pub(crate) fn group_bits(&self, name: &str) -> u32 {
        let id = self.groups.id(name);
        (0..MAX_GROUPS as u8)
            .filter(|g| self.groups.table.collides(id, *g))
            .fold(0u32, |m, g| m | (1 << g))
    }

    /// Marks the group table changed.
    pub(crate) fn groups_changed(&mut self) {
        self.groups_applied = false;
        // Parts' group ids are resolved when assemblies are built.
        let named: Vec<Entity> = self
            .parts
            .iter()
            .filter(|(_, p)| p.snap.group != "Default")
            .map(|(e, _)| *e)
            .collect();
        self.dirty.extend(named);
    }

    /// The assembly key of a part, if it is simulated.
    pub(crate) fn assembly_key(&self, part: Entity) -> Option<EntityId> {
        self.parts.get(&part).and_then(|p| p.asm)
    }

    /// The members of the assembly `part` belongs to (just the part when it
    /// is in a cell or not simulated).
    pub(crate) fn assembly_members(&self, part: Entity) -> Vec<Entity> {
        match self.parts.get(&part).and_then(|p| p.asm) {
            Some(key) => match self.assemblies.get(&key) {
                Some(a) if a.cell.is_none() => a.members.clone(),
                _ => vec![part],
            },
            None => vec![part],
        }
    }

    /// The root of `part`'s assembly.
    pub(crate) fn root_of(&self, part: Entity) -> Entity {
        match self.parts.get(&part).and_then(|p| p.asm) {
            Some(key) => match self.assemblies.get(&key) {
                Some(a) if a.cell.is_none() => a.root,
                _ => part,
            },
            None => part,
        }
    }

    /// The rigid joints currently tracked, as `(joint, part0, part1)`.
    pub(crate) fn rigid_links(&self) -> Vec<(Entity, Entity, Entity)> {
        let mut v: Vec<(Entity, Entity, Entity)> = self
            .rigid
            .iter()
            .filter(|(_, r)| r.enabled)
            .filter_map(|(j, r)| Some((*j, r.part0?, r.part1?)))
            .collect();
        v.sort_unstable();
        v
    }

    /// Whether the fields this module reads exist (for tests).
    #[allow(dead_code)]
    pub(crate) fn field(&self) -> Field {
        self.layout.size
    }
}

/// Puts each entity of `targets` at its world pose, without change detection
/// (so the next sync does not take it for a script move), and updates
/// `GlobalTransform`. A part's `CFrame` is in the world whatever its parent,
/// so the other children with a `Transform` of a moved entity keep their
/// world poses. Parents are written before children.
pub(crate) fn write_world_poses(w: &mut World, targets: &[(Entity, Pose)]) {
    if targets.is_empty() {
        return;
    }
    let mut want: HashMap<Entity, Pose> = targets.iter().copied().collect();
    for &(e, _) in targets {
        for k in re_scene::hierarchy::children(&w.ecs, e).to_vec() {
            if w.ecs.get::<Transform>(k).is_some()
                && !want.contains_key(&k)
                && let Some(p) = world_pose(w, k)
            {
                want.insert(k, p);
            }
        }
    }
    let depth = |e: Entity| {
        let mut d = 0usize;
        let mut cur = e;
        while let Some(c) = w.ecs.get::<ChildOf>(cur) {
            cur = c.parent();
            d += 1;
        }
        d
    };
    let mut order: Vec<(usize, Entity, Pose)> =
        want.iter().map(|(e, p)| (depth(*e), *e, *p)).collect();
    order.sort_unstable_by_key(|(d, e, _)| (*d, *e));
    for (_, e, world) in order {
        // The nearest ancestor with a Transform, at its new pose if it moved.
        let mut cur = e;
        let mut parent = Pose::IDENTITY;
        while let Some(c) = w.ecs.get::<ChildOf>(cur) {
            cur = c.parent();
            if w.ecs.get::<Transform>(cur).is_some() {
                parent = want
                    .get(&cur)
                    .copied()
                    .or_else(|| world_pose(w, cur))
                    .unwrap_or(Pose::IDENTITY);
                break;
            }
        }
        set_pose_raw(w, e, world, parent);
    }
}

fn set_pose_raw(w: &mut World, e: Entity, world: Pose, parent: Pose) {
    let local = parent.inverse().mul(&world);
    let scale = if let Some(mut t) = w.ecs.get_mut::<Transform>(e) {
        let t = t.bypass_change_detection();
        t.translation = local.pos;
        t.rotation = local.rot.normalize();
        t.scale
    } else {
        return;
    };
    let g = Affine3A::from_scale_rotation_translation(scale, world.rot.normalize(), world.pos);
    if let Some(mut gt) = w.ecs.get_mut::<GlobalTransform>(e) {
        gt.set_if_neq(GlobalTransform(g));
    }
}
