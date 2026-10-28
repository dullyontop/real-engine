//! Touches between characters and parts.
//!
//! A character's parts are left out of the part simulation while the
//! Humanoid places them, so the simulation reports no touches for them.
//! After the Humanoids have placed their parts, each part of a character
//! that can touch is checked against the simulated parts its box overlaps;
//! a new overlap fires `Touched` on both parts and one that ended fires
//! `TouchEnded`, as the part simulation does for its own parts. A new
//! overlap with a team-changing `SpawnLocation` changes the player's team
//! first (`players::teams::touched`).

use std::collections::BTreeSet;

use mlua::{Lua, MultiValue, Result};
use re_reflect::Value;
use re_scene::ecs as bevy_ecs;
use re_scene::{Entity, World};

use crate::class::ClassRegistry;
use crate::instance::tree;
use crate::physics::{CharacterDriven, world_pose};
use crate::runtime::with_world;
use crate::signal;

/// The most parts one character part reports touching.
const MAX_TOUCHING: usize = 64;

/// The (character part, other part) pairs touching after the last frame.
#[derive(bevy_ecs::resource::Resource, Default, Debug)]
pub struct CharacterTouches(pub BTreeSet<(Entity, Entity)>);

fn scan(
    w: &mut World,
    classes: &ClassRegistry,
    humanoids: &[Entity],
) -> Vec<(Entity, Entity, bool)> {
    let mut now = BTreeSet::new();
    for &h in humanoids {
        let Some(rig) = tree::parent(w, h) else {
            continue;
        };
        for p in tree::descendants(w, rig) {
            if w.ecs.get::<CharacterDriven>(p).is_none()
                || !matches!(
                    classes.get_property(w, p, "CanTouch"),
                    Ok(Value::Bool(true))
                )
            {
                continue;
            }
            let (Some(pose), Ok(Value::Vec3(size))) =
                (world_pose(w, p), classes.get_property(w, p, "Size"))
            else {
                continue;
            };
            for o in crate::physics::touchable_in_box(w, pose, size, MAX_TOUCHING) {
                now.insert((p, o));
            }
        }
    }
    let old = std::mem::take(
        &mut w
            .ecs
            .get_resource_or_insert_with(CharacterTouches::default)
            .0,
    );
    let mut out: Vec<(Entity, Entity, bool)> =
        now.difference(&old).map(|&(a, b)| (a, b, true)).collect();
    out.extend(old.difference(&now).map(|&(a, b)| (a, b, false)));
    w.ecs.resource_mut::<CharacterTouches>().0 = now;
    out
}

/// Fires the touches that began or ended between the characters' parts and
/// the simulated parts this frame. Call it with the VM bound, after the
/// Humanoids placed their parts.
pub(super) fn dispatch(lua: &Lua, classes: &ClassRegistry, humanoids: &[Entity]) -> Result<()> {
    let events = with_world(lua, |w| scan(w, classes, humanoids))?;
    let alive = |e: Entity| -> Result<bool> {
        with_world(lua, |w| tree::alive(w, e) && !tree::is_destroyed(w, e))
    };
    for (a, b, began) in events {
        let event = if began { "Touched" } else { "TouchEnded" };
        // A part an earlier handler destroyed touches nothing.
        if !(alive(a)? && alive(b)?) {
            continue;
        }
        if began {
            crate::players::teams::touched(lua, a, b)?;
        }
        let (pa, pb) = (
            crate::instance::push(lua, a)?,
            crate::instance::push(lua, b)?,
        );
        signal::fire(lua, a, event, MultiValue::from_vec(vec![pb]))?;
        if alive(b)? {
            signal::fire(lua, b, event, MultiValue::from_vec(vec![pa]))?;
        }
    }
    Ok(())
}
