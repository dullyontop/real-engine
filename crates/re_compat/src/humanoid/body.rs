//! The physics side of a character: a `re_physics` character controller
//! (Jolt `CharacterVirtual`) that carries the `HumanoidRootPart`.
//!
//! Characters share the compat world's `re_physics::Physics`. Until the part
//! physics (WP13) owns that world, a [`CharacterPhysics`] resource holds it;
//! this module is the one place that looks it up.

use glam::{Quat, Vec3};
use re_physics::Physics;
use re_scene::ecs as bevy_ecs;
use re_scene::{Entity, World, entity_id};
use re_types::physics::{CharacterDesc, CharacterState, PhysicsCmd};

/// The physics world characters move in. The host steps it between
/// [`super::pre_simulation`] and [`super::post_simulation`].
#[derive(bevy_ecs::resource::Resource)]
pub struct CharacterPhysics(pub Physics);

fn physics(w: &mut World) -> Option<bevy_ecs::world::Mut<'_, CharacterPhysics>> {
    w.ecs.get_resource_mut::<CharacterPhysics>()
}

/// True when the world has a physics world for characters.
pub fn available(w: &World) -> bool {
    w.ecs.contains_resource::<CharacterPhysics>()
}

fn apply(w: &mut World, cmd: PhysicsCmd) -> bool {
    let Some(mut p) = physics(w) else {
        return false;
    };
    let mut cmds = vec![cmd];
    p.0.apply(&mut cmds);
    true
}

/// Creates the character for a root part: a capsule `height` tall (caps
/// included) and `radius` wide, centred at `pos`.
pub fn add(w: &mut World, root: Entity, height: f32, radius: f32, pos: Vec3, rot: Quat) -> bool {
    let desc = CharacterDesc {
        height,
        radius,
        // Roughly a quarter of the rig's height, like a stair step.
        step_height: (height * 0.2).clamp(0.3, 2.0),
        mass: 70.0,
        inner_body: true,
        ..CharacterDesc::default()
    };
    apply(
        w,
        PhysicsCmd::AddCharacter {
            id: entity_id(root),
            desc,
            pos,
            rot,
        },
    )
}

/// Removes a root part's character.
pub fn remove(w: &mut World, root: Entity) {
    apply(
        w,
        PhysicsCmd::RemoveCharacter {
            id: entity_id(root),
        },
    );
}

/// Asks the character to walk at `velocity` (horizontal) this step.
pub fn walk(w: &mut World, root: Entity, velocity: Vec3) {
    apply(
        w,
        PhysicsCmd::MoveCharacter {
            id: entity_id(root),
            desired_velocity: velocity,
            jump: false,
        },
    );
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
    w.ecs
        .get_resource::<CharacterPhysics>()?
        .0
        .character(entity_id(root))
}

/// The gravity the characters fall with (studs/s², pointing down).
pub fn gravity(w: &World) -> Option<f32> {
    let g = w.ecs.get_resource::<CharacterPhysics>()?.0.config().gravity;
    Some(-g.y)
}
