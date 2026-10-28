//! Drawn poses (ADR-0057 §7, the audit's B2): where each entity is drawn this
//! frame, for render sync and for scripts (`Entity:drawn_transform`).
//!
//! - A moving physics body (dynamic, kinematic or a character) is drawn between
//!   the poses before and after the last step, at the frame's alpha
//!   ([`PhysicsSync::interpolated`]).
//! - Everything under it in the hierarchy is drawn with the same correction:
//!   the body's drawn pose times the inverse of its simulation pose, applied to
//!   the entity's world transform. A moving body under it has its own pose, and
//!   carries what is under it. The chain ends, as propagation's does, at an
//!   entity without a `Transform`.
//! - Anything else is drawn where the world has it, and has no entry.
//!
//! [`DrawnPoses::compute`] fills the table at the start of `LateFrame`, after
//! a propagation, for the late-frame callbacks to read, and again in Sync
//! after its propagation, so what those callbacks moved on a moving body is
//! drawn in the same frame. That table then answers until the next frame's
//! `LateFrame`: in `PreRender`, scripts read what is on screen.

use glam::Affine3A;
use re_scene::ecs::entity::EntityHashMap;
use re_scene::ecs::hierarchy::Children;
use re_scene::{Entity, GlobalTransform, Transform, World, entity_from_id, entity_id};

use super::physics::PhysicsSync;

/// The drawn pose of every moving body and of every entity under one.
#[derive(Clone, Debug, Default)]
pub struct DrawnPoses {
    poses: EntityHashMap<Affine3A>,
}

impl DrawnPoses {
    /// An empty table: everything is drawn where the world has it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fills the table for `alpha` (0 is the pose before the last step, 1 the
    /// pose after it) from the bodies in `physics` and the world's
    /// `GlobalTransform`s, which must be current (propagated after the last
    /// step).
    pub fn compute(&mut self, world: &World, physics: &PhysicsSync, alpha: f32) {
        self.poses.clear();
        let moving = |e: Entity| physics.interpolated(entity_id(e), alpha);
        let mut stack = Vec::new();
        for (id, _) in physics.linked() {
            let Some((translation, rotation)) = physics.interpolated(id, alpha) else {
                continue;
            };
            let Some(body) = entity_from_id(id) else {
                continue;
            };
            let Some(sim) = world.ecs.get::<GlobalTransform>(body).map(|g| g.0) else {
                continue;
            };
            let (scale, _, _) = sim.to_scale_rotation_translation();
            let drawn = Affine3A::from_scale_rotation_translation(scale, rotation, translation);
            self.poses.insert(body, drawn);
            // A body scaled to nothing has no inverse: what it carries is drawn
            // where the world has it.
            let correction = drawn * sim.inverse();
            if !correction.is_finite() {
                continue;
            }
            stack.clear();
            stack.extend(children(world, body));
            while let Some(e) = stack.pop() {
                if moving(e).is_some() || world.ecs.get::<Transform>(e).is_none() {
                    continue;
                }
                if let Some(g) = world.ecs.get::<GlobalTransform>(e) {
                    self.poses.insert(e, correction * g.0);
                }
                stack.extend(children(world, e));
            }
        }
    }

    /// Where `entity` is drawn, or `None` when it is drawn where the world has
    /// it.
    pub fn get(&self, entity: Entity) -> Option<Affine3A> {
        self.poses.get(&entity).copied()
    }

    /// Forgets every pose (a new world, or a stopped game).
    pub fn clear(&mut self) {
        self.poses.clear();
    }

    /// The number of entities with a pose.
    pub fn len(&self) -> usize {
        self.poses.len()
    }

    /// True when every entity is drawn where the world has it.
    pub fn is_empty(&self) -> bool {
        self.poses.is_empty()
    }
}

fn children(world: &World, e: Entity) -> impl Iterator<Item = Entity> + '_ {
    world
        .ecs
        .get::<Children>(e)
        .into_iter()
        .flat_map(|c| c.iter().copied())
}

#[cfg(test)]
mod tests {
    use glam::{Quat, Vec3};
    use re_scene::ecs::hierarchy::ChildOf;
    use re_scene::{BodyKind, Collider, RigidBody};
    use re_types::BodyPose;

    use super::super::physics::{ShapeLibrary, pull, push};
    use super::*;

    fn at(t: Vec3) -> Transform {
        Transform {
            translation: t,
            ..Default::default()
        }
    }

    fn body(world: &mut World, kind: BodyKind, t: Transform, parent: Option<Entity>) -> Entity {
        let mut e = world.ecs.spawn((
            t,
            Collider::default(),
            RigidBody {
                kind,
                ..Default::default()
            },
        ));
        if let Some(p) = parent {
            e.insert(ChildOf(p));
        }
        e.id()
    }

    fn child(world: &mut World, parent: Entity, t: Transform) -> Entity {
        world.ecs.spawn((t, ChildOf(parent))).id()
    }

    /// Links the bodies, then runs one "step" that moves each body to its
    /// pose in `after`, and propagates.
    fn step(world: &mut World, sync: &mut PhysicsSync, after: &[(Entity, Vec3, Quat)]) {
        world.update();
        let (mut cmds, mut diagnostics) = (Vec::new(), Vec::new());
        push(
            world,
            sync,
            &ShapeLibrary::default(),
            &mut cmds,
            &mut diagnostics,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
        sync.begin_step();
        let poses: Vec<BodyPose> = after
            .iter()
            .map(|&(e, pos, rot)| BodyPose {
                id: entity_id(e),
                pos,
                rot,
            })
            .collect();
        pull(world, sync, &poses);
        world.propagate();
    }

    fn translation(p: Option<Affine3A>) -> Vec3 {
        Vec3::from(p.expect("a drawn pose").translation)
    }

    fn assert_near(a: Vec3, b: Vec3) {
        assert!((a - b).length() < 1e-5, "{a} != {b}");
    }

    #[test]
    fn what_a_moving_body_carries_is_drawn_with_its_correction() {
        let mut world = World::new();
        let car = body(&mut world, BodyKind::Dynamic, at(Vec3::ZERO), None);
        let seat = child(&mut world, car, at(Vec3::Y));
        let hero = child(&mut world, seat, at(Vec3::X));
        let lamp = world.ecs.spawn(at(Vec3::new(0.0, 5.0, 0.0))).id();
        let mut sync = PhysicsSync::new();
        step(
            &mut world,
            &mut sync,
            &[(car, Vec3::new(2.0, 0.0, 0.0), Quat::IDENTITY)],
        );

        let mut drawn = DrawnPoses::new();
        // Alpha 0: the pose before the step, for the body and what it carries.
        drawn.compute(&world, &sync, 0.0);
        assert_near(translation(drawn.get(car)), Vec3::ZERO);
        assert_near(translation(drawn.get(seat)), Vec3::Y);
        assert_near(translation(drawn.get(hero)), Vec3::new(1.0, 1.0, 0.0));
        assert_eq!(drawn.get(lamp), None, "nothing carries the lamp");
        assert_eq!(drawn.len(), 3);

        drawn.compute(&world, &sync, 0.5);
        assert_near(translation(drawn.get(seat)), Vec3::new(1.0, 1.0, 0.0));
        drawn.compute(&world, &sync, 1.0);
        assert_near(translation(drawn.get(hero)), Vec3::new(3.0, 1.0, 0.0));
    }

    #[test]
    fn a_turning_body_turns_what_it_carries() {
        let mut world = World::new();
        let turret = body(&mut world, BodyKind::Kinematic, at(Vec3::ZERO), None);
        let barrel = child(&mut world, turret, at(Vec3::X));
        let mut sync = PhysicsSync::new();
        let quarter = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        step(&mut world, &mut sync, &[(turret, Vec3::ZERO, quarter)]);
        let mut drawn = DrawnPoses::new();
        drawn.compute(&world, &sync, 0.0);
        // Before the turn, the barrel is where it started.
        assert_near(translation(drawn.get(barrel)), Vec3::X);
        drawn.compute(&world, &sync, 1.0);
        assert_near(translation(drawn.get(barrel)), quarter * Vec3::X);
    }

    #[test]
    fn a_moving_body_under_another_carries_its_own_subtree() {
        let mut world = World::new();
        let truck = body(&mut world, BodyKind::Kinematic, at(Vec3::ZERO), None);
        let trailer = body(&mut world, BodyKind::Kinematic, at(Vec3::Z), Some(truck));
        let crate_ = child(&mut world, trailer, at(Vec3::Y));
        let mut sync = PhysicsSync::new();
        // The truck moves 2 along x; the trailer stays where it was in the world.
        step(
            &mut world,
            &mut sync,
            &[
                (truck, Vec3::new(2.0, 0.0, 0.0), Quat::IDENTITY),
                (trailer, Vec3::Z, Quat::IDENTITY),
            ],
        );
        let mut drawn = DrawnPoses::new();
        drawn.compute(&world, &sync, 0.0);
        assert_near(translation(drawn.get(truck)), Vec3::ZERO);
        assert_near(translation(drawn.get(trailer)), Vec3::Z);
        assert_near(translation(drawn.get(crate_)), Vec3::new(0.0, 1.0, 1.0));
    }

    #[test]
    fn static_bodies_and_breaks_in_the_chain_are_drawn_where_the_world_has_them() {
        let mut world = World::new();
        let wall = body(&mut world, BodyKind::Static, at(Vec3::ZERO), None);
        let sign = child(&mut world, wall, at(Vec3::Y));
        let cart = body(&mut world, BodyKind::Dynamic, at(Vec3::ZERO), None);
        let bare = world.ecs.spawn(ChildOf(cart)).id();
        let under_bare = child(&mut world, bare, at(Vec3::Y));
        let mut sync = PhysicsSync::new();
        step(
            &mut world,
            &mut sync,
            &[(cart, Vec3::new(1.0, 0.0, 0.0), Quat::IDENTITY)],
        );
        let mut drawn = DrawnPoses::new();
        drawn.compute(&world, &sync, 0.0);
        assert_eq!(drawn.get(wall), None);
        assert_eq!(drawn.get(sign), None);
        assert!(drawn.get(cart).is_some());
        assert_eq!(drawn.get(bare), None);
        assert_eq!(drawn.get(under_bare), None);
        drawn.clear();
        assert!(drawn.is_empty());
    }
}
