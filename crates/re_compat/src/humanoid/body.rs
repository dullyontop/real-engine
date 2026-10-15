//! The physics side of a character: a `re_physics` character controller
//! (Jolt `CharacterVirtual`) that carries the `HumanoidRootPart`.
//!
//! Characters live in the compat world's part simulation
//! ([`crate::physics::with_physics`]). While a character has a controller,
//! the parts of its model are marked [`CharacterDriven`]: the part
//! simulation leaves them out, so the capsule does not collide with its own
//! limbs, and the Humanoid places them after each step.
//!
//! The part simulation splits a frame into [`SUBSTEPS`] steps, and a
//! `re_physics` move request lasts one step, so the walk velocity a Humanoid
//! asks for is kept here ([`Walks`]) and sent before every substep
//! ([`before_substep`]).

use std::collections::BTreeMap;

use glam::{Quat, Vec3};
use re_scene::ecs as bevy_ecs;
use re_scene::{Entity, World, entity_id};
use re_types::EntityId;
use re_types::physics::{CharacterDesc, CharacterState, PhysicsCmd};

use re_reflect::Value;

use crate::class::ClassRegistry;
use crate::instance::tree;
use crate::physics::{CharacterDriven, CharacterLoose, SUBSTEPS, group_bits, with_physics};

/// The walk velocity of each character this frame, sent before every
/// substep.
#[derive(bevy_ecs::resource::Resource, Default, Debug)]
pub struct Walks(pub BTreeMap<EntityId, Vec3>);

/// True when the world has a part simulation for characters to move in.
pub fn available(w: &mut World) -> bool {
    with_physics(w, |_| ()).is_some()
}

fn apply(w: &mut World, cmd: PhysicsCmd) -> bool {
    with_physics(w, |p| {
        let mut cmds = vec![cmd];
        p.apply(&mut cmds);
    })
    .is_some()
}

/// Marks (or unmarks) the parts of `model` as moved by the character, and
/// has the part simulation look at them again.
fn mark_parts(w: &mut World, classes: &ClassRegistry, model: Entity, driven: bool) {
    let parts: Vec<Entity> = tree::descendants(w, model)
        .into_iter()
        .filter(|d| tree::is_a(w, classes, *d, "BasePart"))
        .collect();
    for p in parts {
        let mut e = w.ecs.entity_mut(p);
        if driven {
            e.insert(CharacterDriven);
        } else {
            e.remove::<CharacterDriven>();
        }
    }
    crate::physics::reexamine(w, model);
}

/// Creates the character for a root part: a capsule `height` tall (caps
/// included) and `radius` wide, centred at `pos`. It collides with what the
/// root part's collision group collides with.
#[allow(clippy::too_many_arguments)]
pub fn add(
    w: &mut World,
    classes: &ClassRegistry,
    root: Entity,
    height: f32,
    radius: f32,
    pos: Vec3,
    rot: Quat,
    max_slope_deg: f32,
) -> bool {
    let group = match classes.get_property(w, root, "CollisionGroup") {
        Ok(Value::Str(s)) => s,
        _ => "Default".to_string(),
    };
    let groups = re_types::physics::GroupMask(group_bits(w, &group));
    let desc = CharacterDesc {
        height,
        radius,
        // Roughly a quarter of the rig's height, like a stair step.
        step_height: (height * 0.2).clamp(0.3, 2.0),
        max_slope_deg: max_slope_deg.clamp(0.0, 89.0),
        mass: 70.0,
        inner_body: true,
        groups,
        ..CharacterDesc::default()
    };
    let added = apply(
        w,
        PhysicsCmd::AddCharacter {
            id: entity_id(root),
            desc,
            pos,
            rot,
        },
    );
    if added && let Some(model) = tree::parent(w, root) {
        mark_parts(w, classes, model, true);
    }
    added
}

/// Removes a root part's character; its model's parts go back to the part
/// simulation.
pub fn remove(w: &mut World, classes: &ClassRegistry, root: Entity) {
    apply(
        w,
        PhysicsCmd::RemoveCharacter {
            id: entity_id(root),
        },
    );
    if let Some(mut walks) = w.ecs.get_resource_mut::<Walks>() {
        walks.0.remove(&entity_id(root));
    }
    if tree::alive(w, root) {
        match tree::parent(w, root) {
            Some(model) => mark_parts(w, classes, model, false),
            None => {
                w.ecs.entity_mut(root).remove::<CharacterDriven>();
            }
        }
    }
}

/// Removes a root part's character but keeps its model's parts out of the
/// part simulation (the character sat down: the Humanoid places them).
pub fn release(w: &mut World, root: Entity) {
    apply(
        w,
        PhysicsCmd::RemoveCharacter {
            id: entity_id(root),
        },
    );
    if let Some(mut walks) = w.ecs.get_resource_mut::<Walks>() {
        walks.0.remove(&entity_id(root));
    }
}

/// Hands a dead character's parts to the part simulation as ordinary parts,
/// so the body falls (apart, when its joints broke).
pub fn loosen(w: &mut World, classes: &ClassRegistry, model: Entity) {
    let parts: Vec<Entity> = tree::descendants(w, model)
        .into_iter()
        .filter(|d| tree::is_a(w, classes, *d, "BasePart"))
        .collect();
    for p in parts {
        w.ecs
            .entity_mut(p)
            .insert(CharacterLoose)
            .remove::<CharacterDriven>();
    }
    crate::physics::reexamine(w, model);
}

/// Asks the character to walk at `velocity` (horizontal) for the next frame.
pub fn walk(w: &mut World, root: Entity, velocity: Vec3) {
    w.ecs
        .get_resource_or_insert_with(Walks::default)
        .0
        .insert(entity_id(root), velocity);
}

/// Sends the frame's walk requests; the part simulation calls it before each
/// substep.
pub(crate) fn before_substep(w: &World, physics: &mut re_physics::Physics) {
    let Some(walks) = w.ecs.get_resource::<Walks>() else {
        return;
    };
    let mut cmds: Vec<PhysicsCmd> = walks
        .0
        .iter()
        .map(|(id, v)| PhysicsCmd::MoveCharacter {
            id: *id,
            desired_velocity: *v,
            jump: false,
        })
        .collect();
    if !cmds.is_empty() {
        physics.apply(&mut cmds);
    }
}

/// The length of one simulation substep of a frame of `dt` seconds.
pub fn substep(dt: f32) -> f32 {
    dt / SUBSTEPS as f32
}

/// Sets the character's velocity now (a jump sets its upward speed).
pub fn set_velocity(w: &mut World, root: Entity, velocity: Vec3) {
    apply(
        w,
        PhysicsCmd::SetVelocity {
            id: entity_id(root),
            linear: velocity,
            angular: Vec3::ZERO,
        },
    );
}

/// Moves the character to a new place at once (a script moved the root part).
pub fn teleport(w: &mut World, root: Entity, pos: Vec3, rot: Quat) {
    apply(
        w,
        PhysicsCmd::SetTransform {
            id: entity_id(root),
            pos,
            rot,
        },
    );
}

/// The character's state after the last step.
pub fn state(w: &World, root: Entity) -> Option<CharacterState> {
    crate::physics::read_sim(w, |s| s.physics.character(entity_id(root))).flatten()
}

/// The gravity the characters fall with (studs/s², pointing down), once the
/// simulation exists.
pub fn gravity(w: &World) -> Option<f32> {
    crate::physics::read_sim(w, |s| -s.physics.config().gravity.y)
}
