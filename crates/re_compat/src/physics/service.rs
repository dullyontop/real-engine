//! `PhysicsService` (collision groups), the workspace's `Terrain` and
//! `CurrentCamera`, and what a world needs before it is simulated.
//!
//! Collision groups belong to the running place: they are kept in the
//! simulation, not saved with the scene. Group 0 is `Default`; at most
//! [`MAX_GROUPS`] exist. Parts name their group in `CollisionGroup`; a name
//! that is not registered acts as `Default`.

use std::sync::Arc;

use mlua::{Lua, MultiValue, Value as LuaValue};
use re_reflect::Value;
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::component::Component;
use re_scene::{Entity, World};

use super::sim::{MAX_GROUPS, Sim};
use super::with_sim;
use crate::class::{Accessor, ClassMarker, ClassRegistry};
use crate::codes::{COLLISION_GROUP, PHYSICS_SETUP};
use crate::error::CompatError;
use crate::instance::methods_api::{args, ret};
use crate::instance::tree;
use crate::runtime::{Bindings, compat_error, with_world};

/// The camera `Workspace.CurrentCamera` names.
#[derive(Component, Clone, Copy, Debug)]
pub struct CurrentCamera(pub Entity);

fn workspace(w: &World) -> Option<Entity> {
    let game = w.entity(crate::GAME_ID).ok()?;
    crate::services::core::find_service(w, game, "Workspace")
}

fn child_of_class(w: &World, parent: Entity, class: &str) -> Option<Entity> {
    tree::children(w, parent).into_iter().find(|c| {
        w.ecs
            .get::<ClassMarker>(*c)
            .is_some_and(|m| m.class == class)
    })
}

/// Gives the workspace a `Terrain` and a `Camera` when it lacks them.
pub(crate) fn prepare_world(world: &mut World, classes: &ClassRegistry) -> Result<(), CompatError> {
    let Some(ws) = workspace(world) else {
        return Ok(());
    };
    for class in ["Terrain", "Camera"] {
        if child_of_class(world, ws, class).is_none() && classes.try_class(class).is_some() {
            crate::scene::spawn_instance(world, classes, class, None, class, Some(ws))?;
        }
    }
    Ok(())
}

fn setup_error(message: impl Into<String>) -> CompatError {
    CompatError::Runtime {
        code: PHYSICS_SETUP,
        message: message.into(),
    }
}

/// The accessors of `Workspace.Terrain` and `Workspace.CurrentCamera`.
pub(crate) fn accessors() -> Vec<(&'static str, &'static str, Accessor)> {
    vec![
        (
            "Workspace",
            "Terrain",
            Accessor {
                get: Arc::new(|w, e| {
                    child_of_class(w, e, "Terrain")
                        .map(|t| Value::Entity(Some(t)))
                        .ok_or_else(|| setup_error("the workspace has no Terrain"))
                }),
                set: None,
            },
        ),
        (
            "Workspace",
            "CurrentCamera",
            Accessor {
                get: Arc::new(|w, e| {
                    let named = w
                        .ecs
                        .get::<CurrentCamera>(e)
                        .map(|c| c.0)
                        .filter(|c| tree::alive(w, *c) && !tree::is_destroyed(w, *c));
                    Ok(Value::Entity(
                        named.or_else(|| child_of_class(w, e, "Camera")),
                    ))
                }),
                set: Some(Arc::new(|w, e, v| match v {
                    Value::Entity(Some(c)) => {
                        w.ecs.entity_mut(e).insert(CurrentCamera(c));
                        Ok(())
                    }
                    Value::Entity(None) => {
                        w.ecs.entity_mut(e).remove::<CurrentCamera>();
                        Ok(())
                    }
                    other => Err(CompatError::Runtime {
                        code: re_script::codes::BAD_ARGUMENT,
                        message: format!(
                            "CurrentCamera must be a Camera, got {}",
                            other.describe()
                        ),
                    }),
                })),
            },
        ),
    ]
}

fn group_error(message: impl Into<String>) -> mlua::Error {
    compat_error(CompatError::Runtime {
        code: COLLISION_GROUP,
        message: message.into(),
    })
}

/// Runs `f` on the collision groups of the bound world.
fn groups<R>(lua: &Lua, f: impl FnOnce(&mut Sim) -> mlua::Result<R>) -> mlua::Result<R> {
    with_world(lua, |w| with_sim(w, |sim, _| f(sim)))?
        .unwrap_or_else(|| Err(group_error("this world has no workspace to simulate")))
}

fn check_name(name: &str) -> mlua::Result<()> {
    if name.is_empty() || name.chars().count() > 100 {
        return Err(group_error(format!(
            "{name:?} is not a valid collision group name (1 to 100 characters)"
        )));
    }
    Ok(())
}

fn known(sim: &Sim, name: &str) -> mlua::Result<u8> {
    sim.groups
        .find(name)
        .ok_or_else(|| group_error(format!("collision group {name:?} is not registered")))
}

/// Registers group `name`; returns its id.
fn register_group(sim: &mut Sim, name: &str) -> mlua::Result<u8> {
    check_name(name)?;
    if let Some(id) = sim.groups.find(name) {
        return Ok(id);
    }
    let Some(slot) = sim.groups.names.iter().position(Option::is_none) else {
        return Err(group_error(format!(
            "cannot register {name:?}: all {MAX_GROUPS} collision groups are in use"
        )));
    };
    sim.groups.names[slot] = Some(name.to_string());
    sim.groups.table.reset(slot as u8);
    sim.groups_changed();
    Ok(slot as u8)
}

fn unregister_group(sim: &mut Sim, name: &str) -> mlua::Result<()> {
    if name == "Default" {
        return Err(group_error("the Default collision group cannot be removed"));
    }
    if let Some(id) = sim.groups.find(name) {
        sim.groups.names[id as usize] = None;
        sim.groups.table.reset(id);
        sim.groups_changed();
    }
    Ok(())
}

fn register(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name,): (String,) = args(lua, a, "RegisterCollisionGroup")?;
    groups(lua, |s| register_group(s, &name))?;
    ret(lua, ())
}

fn create(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name,): (String,) = args(lua, a, "CreateCollisionGroup")?;
    let id = groups(lua, |s| {
        if s.groups.find(&name).is_some() {
            return Err(group_error(format!(
                "collision group {name:?} already exists"
            )));
        }
        register_group(s, &name)
    })?;
    ret(lua, id)
}

fn unregister(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name,): (String,) = args(lua, a, "UnregisterCollisionGroup")?;
    groups(lua, |s| unregister_group(s, &name))?;
    ret(lua, ())
}

fn remove(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name,): (String,) = args(lua, a, "RemoveCollisionGroup")?;
    groups(lua, |s| unregister_group(s, &name))?;
    ret(lua, ())
}

fn rename(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (from, to): (String, String) = args(lua, a, "RenameCollisionGroup")?;
    groups(lua, |s| {
        if from == "Default" {
            return Err(group_error("the Default collision group cannot be renamed"));
        }
        check_name(&to)?;
        let id = known(s, &from)?;
        if s.groups.find(&to).is_some_and(|other| other != id) {
            return Err(group_error(format!(
                "collision group {to:?} already exists"
            )));
        }
        s.groups.names[id as usize] = Some(to.clone());
        s.groups_changed();
        Ok(())
    })?;
    ret(lua, ())
}

fn set_collidable(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (x, y, collides): (String, String, bool) = args(lua, a, "CollisionGroupSetCollidable")?;
    groups(lua, |s| {
        let (a, b) = (known(s, &x)?, known(s, &y)?);
        s.groups.table.set(a, b, collides);
        s.groups_changed();
        Ok(())
    })?;
    ret(lua, ())
}

fn are_collidable(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (x, y): (String, String) = args(lua, a, "CollisionGroupsAreCollidable")?;
    let c = groups(lua, |s| {
        let (a, b) = (known(s, &x)?, known(s, &y)?);
        Ok(s.groups.table.collides(a, b))
    })?;
    ret(lua, c)
}

fn registered(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "GetRegisteredCollisionGroups")?;
    let list = groups(lua, |s| {
        Ok(s.groups
            .names
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.clone().map(|n| (i as u8, n)))
            .map(|(i, n)| (i, n.clone(), s.group_bits(&n)))
            .collect::<Vec<_>>())
    })?;
    let t = lua.create_table()?;
    for (id, name, mask) in list {
        let g = lua.create_table()?;
        g.raw_set("id", id)?;
        g.raw_set("name", name)?;
        g.raw_set("mask", mask)?;
        t.raw_push(g)?;
    }
    ret(lua, t)
}

fn is_registered(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name,): (String,) = args(lua, a, "IsCollisionGroupRegistered")?;
    let r = groups(lua, |s| Ok(s.groups.find(&name).is_some()))?;
    ret(lua, r)
}

fn max_groups(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "GetMaxCollisionGroups")?;
    ret(lua, MAX_GROUPS as u32)
}

fn group_id(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name,): (String,) = args(lua, a, "GetCollisionGroupId")?;
    let id = groups(lua, |s| known(s, &name))?;
    ret(lua, id)
}

fn group_name(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (id,): (i64,) = args(lua, a, "GetCollisionGroupName")?;
    let name = groups(lua, |s| {
        usize::try_from(id)
            .ok()
            .and_then(|i| s.groups.names.get(i).cloned().flatten())
            .ok_or_else(|| group_error(format!("no collision group has id {id}")))
    })?;
    ret(lua, name)
}

fn set_part_group(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (part, name): (LuaValue, String) = args(lua, a, "SetPartCollisionGroup")?;
    let part = crate::instance::arg_instance(&part, "SetPartCollisionGroup")?;
    groups(lua, |s| known(s, &name).map(|_| ()))?;
    with_world(lua, |w| {
        let f = super::layout::Field::find(w, "BasePart", "CollisionGroup")
            .ok_or_else(|| setup_error("BasePart has no CollisionGroup"))?;
        if !w.registry.has(&w.ecs, part, f.key) {
            return Err(CompatError::Runtime {
                code: re_script::codes::BAD_ARGUMENT,
                message: "SetPartCollisionGroup: BasePart expected".into(),
            });
        }
        f.set(w, part, Value::Str(name))
    })?
    .map_err(compat_error)?;
    ret(lua, ())
}

type Imp = fn(&Lua, Entity, MultiValue) -> mlua::Result<MultiValue>;

/// Registers the `PhysicsService` methods, and reads `CustomPhysicalProperties`
/// of `nil` as `nil`.
pub(crate) fn register_methods(b: &mut Bindings) {
    let methods: &[(&str, Imp)] = &[
        ("RegisterCollisionGroup", register),
        ("UnregisterCollisionGroup", unregister),
        ("RenameCollisionGroup", rename),
        ("CollisionGroupSetCollidable", set_collidable),
        ("CollisionGroupsAreCollidable", are_collidable),
        ("GetRegisteredCollisionGroups", registered),
        ("IsCollisionGroupRegistered", is_registered),
        ("GetMaxCollisionGroups", max_groups),
        ("CreateCollisionGroup", create),
        ("RemoveCollisionGroup", remove),
        ("GetCollisionGroupId", group_id),
        ("GetCollisionGroupName", group_name),
        ("SetPartCollisionGroup", set_part_group),
    ];
    for (name, f) in methods {
        b.method("PhysicsService", name, Arc::new(*f));
    }
    // A part without custom properties reads `nil`.
    b.codec.set_to_lua(
        "PhysicalProperties",
        Arc::new(|lua: &Lua, v: &Value| match v {
            Value::Entity(None) => Ok(LuaValue::Nil),
            other => crate::datatypes::to_lua(lua, "PhysicalProperties", other.clone()),
        }),
    );
}
