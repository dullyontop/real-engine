//! The Physics phase of a fixed step, and what it needs besides
//! [`crate::sync::physics`]: the meshes colliders name, characters and
//! vehicles linked as they appear, and characters put back on ground that
//! changed.

use re_core::time::Instant;
use re_core::{Diagnostic, Location, Phase};
use re_scene::{World, entity_from_id};

use re_script::ContextKind;

use crate::codes;
use crate::media::Media;
use crate::procedural;
use crate::sync::physics::{self, ShapeLibrary};
use crate::sync::ragdoll;

use super::App;

impl App {
    /// Physics: the World's bodies, ragdolls and commands pushed to the
    /// simulation (characters put back on the ground when the terrain
    /// under them changed), one step with a compat game's parts and
    /// characters, then the poses, contacts and diagnostics read back.
    pub(super) fn physics_phase(&mut self) {
        let t = Instant::now();
        self.physics_sync.begin_step();
        self.load_collider_sources();
        physics::push(
            &mut self.world,
            &mut self.physics_sync,
            &self.shapes,
            &mut self.physics_cmds,
            &mut self.diagnostics,
        );
        ragdoll::push(
            &mut self.world,
            &self.physics,
            &self.skin_sync,
            &mut self.ragdoll_sync,
            &mut self.physics_cmds,
            &mut self.diagnostics,
        );
        self.physics.apply(&mut self.physics_cmds);
        let ground = procedural::ground(&mut self.world);
        if self.ground.as_ref() != Some(&ground) {
            if let Some(old) = self.ground.take()
                && !ground.is_empty()
            {
                // The terrain changed under the characters: put them on it.
                self.reground(&old, &ground);
                physics::push(
                    &mut self.world,
                    &mut self.physics_sync,
                    &self.shapes,
                    &mut self.physics_cmds,
                    &mut self.diagnostics,
                );
                self.physics.apply(&mut self.physics_cmds);
            }
            self.ground = Some(ground);
        }
        let step = Instant::now();
        if let Err(d) = self.physics.step(self.clock.dt_secs()) {
            self.diagnostics.push(d);
        }
        // The server's parts (WP13) and characters (WP14), with their touch
        // events.
        if self.compat.is_some() {
            let dt: f64 = self.clock.dt_secs().into();
            self.compat_call(ContextKind::Server, |vm, h| vm.physics(h, dt));
        }
        self.perf.physics_step(step.elapsed());
        self.physics.poses_into(&mut self.poses);
        self.physics.vehicle_poses_into(&mut self.wheel_poses);
        self.contacts.extend_from_slice(self.physics.events());
        self.diagnostics.extend(self.physics.take_diagnostics());
        self.perf.add(Phase::Physics, t.elapsed());
    }

    pub(super) fn load_collider_sources(&mut self) {
        load_collider_sources(&mut self.world, &mut self.shapes, &mut self.media);
    }

    /// Gives characters and vehicles that appeared since the last physics push
    /// (the first step, a spawn, a restart) their physics character or vehicle
    /// now, so scripts can move them in the step they appear.
    pub(super) fn link_new_characters(&mut self) {
        let mut unlinked: Vec<re_types::EntityId> = self
            .world
            .ecs
            .query_filtered::<re_scene::Entity, re_scene::ecs::query::With<re_scene::CharacterController>>()
            .iter(&self.world.ecs)
            .map(re_scene::entity_id)
            .filter(|&id| self.physics_sync.kind(id) != Some(physics::LinkKind::Character))
            .collect();
        unlinked.extend(
            self.world
                .ecs
                .query_filtered::<re_scene::Entity, re_scene::ecs::query::With<re_scene::Vehicle>>()
                .iter(&self.world.ecs)
                .map(re_scene::entity_id)
                .filter(|&id| !self.physics_sync.is_vehicle(id)),
        );
        // A character or vehicle that cannot be linked (reported by `push`) is tried
        // once here, not on every step.
        let fresh = unlinked.iter().any(|id| !self.link_tried.contains(id));
        self.link_tried = unlinked.into_iter().collect();
        if fresh {
            self.load_collider_sources();
            physics::push(
                &mut self.world,
                &mut self.physics_sync,
                &self.shapes,
                &mut self.physics_cmds,
                &mut self.diagnostics,
            );
            self.physics.apply(&mut self.physics_cmds);
        }
    }

    /// Puts characters back on the ground after the terrain changed: a ray
    /// down from above every terrain at the character's x and z finds the
    /// first terrain (a derived heightfield in `new`, passing through anything
    /// else on the way); when that terrain is a new one, the character's centre
    /// goes half its height (plus a skin) above the hit. Characters over a
    /// terrain that did not change stay where they are.
    pub(super) fn reground(
        &mut self,
        old: &std::collections::BTreeSet<String>,
        new: &std::collections::BTreeSet<String>,
    ) {
        const SKIN: f32 = 0.02;
        const MARGIN: f32 = 10.0;
        // Scripts and earlier steps this frame moved things since Sync.
        self.world.propagate();
        let Some((bottom, top)) = procedural::ground_span(&mut self.world) else {
            return;
        };
        let characters: Vec<(re_scene::Entity, f32)> = self
            .world
            .ecs
            .query::<(re_scene::Entity, &re_scene::CharacterController)>()
            .iter(&self.world.ecs)
            .map(|(e, c)| (e, c.height))
            .collect();
        for (entity, height) in characters {
            let Some(gt) = self.world.ecs.get::<re_scene::GlobalTransform>(entity) else {
                continue;
            };
            let pos = glam::Vec3::from(gt.0.translation);
            match self.terrain_below(entity, pos, bottom - MARGIN, top + MARGIN, new) {
                Some((source, _)) if old.contains(&source) => {}
                Some((_, ground)) => {
                    let target = glam::Vec3::new(pos.x, ground + height * 0.5 + SKIN, pos.z);
                    let local = match re_scene::hierarchy::parent(&self.world.ecs, entity)
                        .and_then(|p| self.world.ecs.get::<re_scene::GlobalTransform>(p))
                    {
                        Some(parent) => parent.0.inverse().transform_point3(target),
                        None => target,
                    };
                    if let Some(mut tr) = self.world.ecs.get_mut::<re_scene::Transform>(entity) {
                        tr.translation = local;
                    }
                }
                None => {
                    let name = self.world.id_of(entity).unwrap_or("?").to_string();
                    self.diagnostics.push(
                        Diagnostic::warning(
                            codes::REGROUND_FAILED,
                            format!("no ground below character {name:?} after the terrain changed"),
                        )
                        .at(Location::entity(name)),
                    );
                }
            }
        }
        self.world.propagate();
    }

    /// The first terrain (a derived heightfield collider in `ground`) straight
    /// below `pos`, between the heights `top` and `bottom`: its source and the
    /// height of the hit.
    fn terrain_below(
        &self,
        character: re_scene::Entity,
        pos: glam::Vec3,
        bottom: f32,
        top: f32,
        ground: &std::collections::BTreeSet<String>,
    ) -> Option<(String, f32)> {
        const MAX_SKIPS: usize = 64;
        let mut origin = glam::Vec3::new(pos.x, top.max(pos.y), pos.z);
        let mut exclude = re_scene::entity_id(character);
        for _ in 0..MAX_SKIPS {
            let length = origin.y - bottom;
            if length <= 0.0 {
                return None;
            }
            let filter = re_physics::QueryFilter {
                exclude: Some(exclude),
                sensors: false,
                ..re_physics::QueryFilter::default()
            };
            let hit = self
                .physics
                .raycast(origin, glam::Vec3::NEG_Y, length, &filter)
                .ok()??;
            let terrain = entity_from_id(hit.entity)
                .and_then(|e| self.world.ecs.get::<re_scene::Collider>(e))
                .filter(|c| {
                    c.shape == re_scene::ColliderShape::Heightfield && ground.contains(&c.source)
                })
                .map(|c| c.source.clone());
            if let Some(source) = terrain {
                return Some((source, hit.point.y));
            }
            // Something else (a crate, a roof, another character): look past it.
            exclude = hit.entity;
            origin = glam::Vec3::new(pos.x, hit.point.y - 1e-3, pos.z);
        }
        None
    }
}

/// Puts the meshes that `Convex` and `Mesh` colliders name (asset paths and
/// built-ins) into the shape library, loading each once.
pub(super) fn load_collider_sources(
    world: &mut World,
    shapes: &mut ShapeLibrary,
    media: &mut Media,
) {
    use re_scene::{Collider, ColliderShape};
    procedural::sync_derived(world, media, shapes);
    let wanted: Vec<String> = world
        .ecs
        .query::<&Collider>()
        .iter(&world.ecs)
        .filter(|c| matches!(c.shape, ColliderShape::Convex | ColliderShape::Mesh))
        .map(|c| c.source.clone())
        .filter(|s| !s.is_empty() && !s.starts_with(crate::media::DERIVED))
        .collect();
    for source in wanted {
        if shapes.mesh(&source).is_some() {
            continue;
        }
        // A source that cannot be loaded stays missing; push reports it.
        if let Ok(loaded) = media.mesh(&source) {
            shapes.insert_mesh(source, loaded.mesh);
        }
    }
}
