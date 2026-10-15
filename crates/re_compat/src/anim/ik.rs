//! `IKControl`: bends a rig's joints after the Animator's pose.
//!
//! An IKControl under a rig's Humanoid or AnimationController names a chain:
//! `ChainRoot` (an upper arm) down to `EndEffector` (a hand, or an attachment
//! on one). Each step, after the Animator writes its pose, the enabled
//! controls apply in `Priority` order:
//! - `Position` bends the first two joints of the chain with a two-bone solve
//!   so the end effector reaches the target, in the plane of `Pole` if set;
//! - `Rotation` turns the last joint so the end effector matches the target's
//!   rotation;
//! - `Transform` does both;
//! - `LookAt` turns the chain root's joint so the end effector's front faces
//!   the target.
//!
//! `Weight` blends from the animated pose (0) to the solved one (1), and
//! `Offset` is applied to the target's frame.

use std::collections::BTreeMap;
use std::sync::Arc;

use glam::{Quat, Vec3};
use mlua::{Lua, Result};
use re_reflect::Value;
use re_scene::{Entity, World};

use super::pose::{self, Joint};
use super::{cframe_value, rig_of};
use crate::class::{ClassMarker, ClassRegistry};
use crate::datatypes::CFrame;
use crate::instance::tree;
use crate::runtime::with_world;

/// Applies the IK controls of every rig that has an Animator.
pub fn apply_all(lua: &Lua, classes: &Arc<ClassRegistry>) -> Result<()> {
    with_world(lua, |w| {
        let mut animators: Vec<Entity> = w
            .ecs
            .query::<(Entity, &ClassMarker)>()
            .iter(&w.ecs)
            .filter(|(_, m)| m.class == "Animator")
            .map(|(e, _)| e)
            .collect();
        animators.sort();
        for a in animators {
            if tree::is_destroyed(w, a) {
                continue;
            }
            let (Some(holder), Some(rig)) = (tree::parent(w, a), rig_of(w, a)) else {
                continue;
            };
            let mut controls: Vec<(i64, Entity)> = tree::children(w, holder)
                .into_iter()
                .filter(|c| tree::is_a(w, classes, *c, "IKControl"))
                .filter(|c| {
                    matches!(
                        classes.get_property(w, *c, "Enabled"),
                        Ok(Value::Bool(true))
                    )
                })
                .map(|c| (int_prop(w, classes, c, "Priority"), c))
                .collect();
            controls.sort_by_key(|(p, _)| *p);
            for (_, c) in controls {
                apply_one(w, classes, a, rig, c);
            }
        }
    })
}

fn int_prop(w: &World, classes: &ClassRegistry, e: Entity, prop: &str) -> i64 {
    match classes.get_property(w, e, prop) {
        Ok(Value::Int(i)) => i,
        Ok(Value::Float(x)) => x as i64,
        _ => 0,
    }
}

fn num_prop(w: &World, classes: &ClassRegistry, e: Entity, prop: &str) -> f32 {
    match classes.get_property(w, e, prop) {
        Ok(Value::Float(x)) => x as f32,
        Ok(Value::Int(i)) => i as f32,
        _ => 0.0,
    }
}

fn entity_prop(w: &World, classes: &ClassRegistry, e: Entity, prop: &str) -> Option<Entity> {
    match classes.get_property(w, e, prop) {
        Ok(Value::Entity(x)) => x,
        _ => None,
    }
}

fn v(a: [f32; 3]) -> Vec3 {
    Vec3::from_array(a)
}

fn quat(c: &CFrame) -> Quat {
    let [x, y, z, w] = c.quaternion();
    Quat::from_xyzw(x, y, z, w).normalize()
}

/// The joints from `root` (the part the first joint moves) down to `end`
/// (the part the last one moves), in that order.
fn chain(joints: &[Joint], root: Entity, end: Entity) -> Option<Vec<Joint>> {
    let mut out = Vec::new();
    let mut at = end;
    loop {
        let j = *joints.iter().find(|j| j.part1 == at)?;
        out.push(j);
        if at == root {
            break;
        }
        at = j.part0;
        if out.len() > joints.len() {
            return None;
        }
    }
    out.reverse();
    Some(out)
}

/// Turns joint `j` so its world rotation is pre-multiplied by `delta`, given
/// its `part0`'s current frame, and writes the new `Transform`.
fn turn(
    w: &mut World,
    classes: &ClassRegistry,
    animator: Entity,
    j: &mut Joint,
    part0: &CFrame,
    delta: Quat,
) {
    let base = quat(&(*part0 * j.c0));
    let t = quat(&j.transform);
    let r = (base.inverse() * delta * base * t).normalize();
    let before = classes.get_property(w, j.motor, "Transform");
    j.transform = CFrame::from_quaternion(j.transform.p, [r.x, r.y, r.z, r.w]);
    if classes
        .set_property(w, j.motor, "Transform", cframe_value(&j.transform))
        .is_ok()
        && let (Ok(before), Ok(after)) = (before, classes.get_property(w, j.motor, "Transform"))
    {
        super::note_ik(w, animator, j.motor, before, after);
    }
}

fn apply_one(w: &mut World, classes: &ClassRegistry, a: Entity, rig: Entity, ctl: Entity) {
    let weight = num_prop(w, classes, ctl, "Weight").clamp(0.0, 1.0);
    if weight <= 0.0 {
        return;
    }
    let (Some(root), Some(effector), Some(target)) = (
        entity_prop(w, classes, ctl, "ChainRoot"),
        entity_prop(w, classes, ctl, "EndEffector"),
        entity_prop(w, classes, ctl, "Target"),
    ) else {
        return;
    };
    let kind = match classes.get_property(w, ctl, "Type") {
        Ok(Value::Enum(s) | Value::Str(s)) => s,
        _ => "Transform".to_string(),
    };
    let end_part = if tree::is_a(w, classes, effector, "Attachment") {
        match tree::parent(w, effector) {
            Some(p) => p,
            None => return,
        }
    } else {
        effector
    };
    let joints = pose::joints(w, classes, rig);
    let Some(mut chain) = chain(&joints, root, end_part) else {
        return;
    };
    let mut frames = pose::solve(w, classes, rig);
    let offset = pose::cframe_prop(w, classes, ctl, "Offset").unwrap_or_default();
    let Some(goal) = pose::world_of(w, classes, &frames, target).map(|t| t * offset) else {
        return;
    };
    let pole = entity_prop(w, classes, ctl, "Pole")
        .and_then(|p| pose::world_of(w, classes, &frames, p))
        .map(|c| v(c.p));
    let blend = |d: Quat| Quat::IDENTITY.slerp(d, weight);

    // Re-poses the chain from its first joint after a change.
    let refresh = |frames: &mut BTreeMap<Entity, crate::datatypes::CFrame>, chain: &[Joint]| {
        let p0 = chain[0].part0;
        for j in chain {
            frames.remove(&j.part1);
        }
        pose::solve_from(chain, frames, p0);
    };

    if kind == "LookAt" {
        let Some(eff) = pose::world_of(w, classes, &frames, effector) else {
            return;
        };
        let Some(base) = frames.get(&chain[0].part0).copied() else {
            return;
        };
        let d = re_anim::ik::look_at_delta(v(eff.look_vector()), v(eff.p), v(goal.p));
        turn(w, classes, a, &mut chain[0], &base, blend(d));
        return;
    }

    if (kind == "Position" || kind == "Transform") && chain.len() >= 2 {
        let (Some(b0), Some(b1), Some(eff)) = (
            frames.get(&chain[0].part0).copied(),
            frames.get(&chain[1].part0).copied(),
            pose::world_of(w, classes, &frames, effector),
        ) else {
            return;
        };
        let root_pt = v(chain[0].world(&b0).p);
        let mid_pt = v(chain[1].world(&b1).p);
        let (dr, dm) = re_anim::ik::two_bone(root_pt, mid_pt, v(eff.p), v(goal.p), pole);
        turn(w, classes, a, &mut chain[0], &b0, blend(dr));
        refresh(&mut frames, &chain);
        let Some(b1) = frames.get(&chain[1].part0).copied() else {
            return;
        };
        let mut second = chain[1];
        turn(w, classes, a, &mut second, &b1, blend(dm));
        chain[1] = second;
        refresh(&mut frames, &chain);
    }

    if kind == "Rotation" || kind == "Transform" {
        let last = chain.len() - 1;
        let (Some(base), Some(eff)) = (
            frames.get(&chain[last].part0).copied(),
            pose::world_of(w, classes, &frames, effector),
        ) else {
            return;
        };
        let d = (quat(&goal) * quat(&eff).inverse()).normalize();
        turn(w, classes, a, &mut chain[last], &base, blend(d));
    }
}
