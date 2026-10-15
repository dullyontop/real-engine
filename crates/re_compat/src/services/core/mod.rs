//! The core services: `game:GetService` and `FindService`, the `game` and
//! `workspace` globals, and `CollectionService`.
//!
//! A service is the one instance of a service class under `game`, with a
//! `ServiceTag`, named and identified by its class name. `GetService` makes a
//! declared service that the place lacks (as Roblox does); a name that is not
//! a declared service class raises `RE9001`. Making `StarterPlayer` also makes
//! its `StarterPlayerScripts` and `StarterCharacterScripts`.

use std::sync::Arc;

use mlua::{Lua, MultiValue, Result, Value as LuaValue};
use re_scene::{Entity, World, hierarchy};
use re_script::codes::UNSUPPORTED_MEMBER;

use crate::class::{ClassMarker, ClassRegistry};
use crate::error::CompatError;
use crate::instance::methods_api::{args, ret};
use crate::instance::{arg_instance, attributes, push, push_list, tree};
use crate::runtime::{Bindings, compat_error, lua_error, state, with_world};
use crate::scene::{ServiceTag, spawn_instance};
use crate::signal::{self, events};

/// The children a service is made with; each is named after its class and,
/// when free, has its class name as stable id.
fn service_children(class: &str) -> &'static [&'static str] {
    match class {
        "StarterPlayer" => &["StarterPlayerScripts", "StarterCharacterScripts"],
        _ => &[],
    }
}

/// The existing service of class `class` under `game`.
pub fn find_service(w: &World, game: Entity, class: &str) -> Option<Entity> {
    hierarchy::children(&w.ecs, game).iter().copied().find(|c| {
        w.ecs.get::<ServiceTag>(*c).is_some()
            && w.ecs
                .get::<ClassMarker>(*c)
                .is_some_and(|m| m.class == class)
    })
}

/// The service of class `class` under `game`, made when the place lacks it.
/// Fails with `RE9001` when `class` is not a declared service class.
pub fn ensure_service(
    w: &mut World,
    classes: &ClassRegistry,
    game: Entity,
    class: &str,
) -> std::result::Result<Entity, CompatError> {
    let def = classes.class(class)?;
    if !def.service {
        return Err(CompatError::UnknownMember {
            class: "ServiceProvider".into(),
            member: format!("service {class}"),
            suggestion: None,
        });
    }
    if let Some(s) = find_service(w, game, class) {
        return Ok(s);
    }
    // The id is the class name, unless something else already has it.
    let id = w.entity(class).is_err().then_some(class);
    let s = spawn_instance(w, classes, class, id, class, Some(game))?;
    for child in service_children(class) {
        let id = w.entity(child).is_err().then_some(*child);
        spawn_instance(w, classes, child, id, child, Some(s))?;
    }
    Ok(s)
}

/// `game.<Service>` for a service the place does not have yet: when `class`
/// is a service provider and `key` names a declared service class, the
/// service (made if missing); otherwise `None`.
pub(crate) fn service_by_index(
    lua: &Lua,
    e: Entity,
    class: &str,
    key: &str,
) -> Result<Option<LuaValue>> {
    let st = state(lua)?;
    let classes = &st.setup.classes;
    if !classes.is_a(class, "ServiceProvider") || !classes.try_class(key).is_some_and(|c| c.service)
    {
        return Ok(None);
    }
    let s = with_world(lua, |w| ensure_service(w, classes, e, key))?.map_err(compat_error)?;
    events::after_write(lua)?;
    Ok(Some(push(lua, s)?))
}

fn get_service(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "GetService")?;
    let st = state(lua)?;
    let classes = &st.setup.classes;
    if !classes.try_class(&name).is_some_and(|c| c.service) {
        return Err(lua_error(
            UNSUPPORTED_MEMBER,
            format!("unsupported API member: '{name}' is not a valid Service name"),
        ));
    }
    let s = with_world(lua, |w| ensure_service(w, classes, e, &name))?.map_err(compat_error)?;
    events::after_write(lua)?;
    ret(lua, push(lua, s)?)
}

fn find_service_method(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "FindService")?;
    let s = with_world(lua, |w| find_service(w, e, &name))?;
    match s {
        Some(s) => ret(lua, push(lua, s)?),
        None => ret(lua, LuaValue::Nil),
    }
}

fn tag_args(lua: &Lua, a: MultiValue, what: &str) -> Result<(Entity, String)> {
    let (inst, tag): (LuaValue, String) = args(lua, a, what)?;
    Ok((arg_instance(&inst, what)?, tag))
}

fn cs_add_tag(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let (e, tag) = tag_args(lua, a, "CollectionService:AddTag")?;
    with_world(lua, |w| {
        if tree::alive(w, e) {
            attributes::add_tag(w, e, &tag)
        }
    })?;
    events::after_write(lua)?;
    ret(lua, ())
}

fn cs_remove_tag(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let (e, tag) = tag_args(lua, a, "CollectionService:RemoveTag")?;
    with_world(lua, |w| {
        if tree::alive(w, e) {
            attributes::remove_tag(w, e, &tag)
        }
    })?;
    events::after_write(lua)?;
    ret(lua, ())
}

fn cs_has_tag(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let (e, tag) = tag_args(lua, a, "CollectionService:HasTag")?;
    ret(lua, with_world(lua, |w| attributes::has_tag(w, e, &tag))?)
}

fn cs_get_tags(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let inst: LuaValue = args(lua, a, "CollectionService:GetTags")?;
    let e = arg_instance(&inst, "CollectionService:GetTags")?;
    let tags = with_world(lua, |w| attributes::tags(w, e))?;
    ret(lua, lua.create_sequence_from(tags)?)
}

/// The instances inside the place under `game` with `tag`, in stable-id
/// order (so the order survives a reload).
pub fn tagged(w: &World, game: Entity, tag: &str) -> Vec<Entity> {
    let mut found: Vec<(String, Entity)> = w
        .tag_index()
        .tagged(tag)
        .filter(|e| {
            tree::is_instance(w, *e) && !tree::is_destroyed(w, *e) && tree::in_game(w, game, *e)
        })
        .map(|e| (w.id_of(e).unwrap_or_default().to_string(), e))
        .collect();
    found.sort();
    found.into_iter().map(|(_, e)| e).collect()
}

fn cs_get_tagged(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let tag: String = args(lua, a, "CollectionService:GetTagged")?;
    let game = state(lua)?.game;
    let list = with_world(lua, |w| tagged(w, game, &tag))?;
    ret(lua, push_list(lua, &list)?)
}

fn cs_get_all_tags(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    let game = state(lua)?.game;
    let tags: Vec<String> = with_world(lua, |w| {
        w.tag_index()
            .tags()
            .into_iter()
            .filter(|t| !tagged(w, game, t).is_empty())
            .map(str::to_string)
            .collect()
    })?;
    ret(lua, lua.create_sequence_from(tags)?)
}

fn cs_added_signal(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let tag: String = args(lua, a, "CollectionService:GetInstanceAddedSignal")?;
    ret(lua, signal::push(lua, e, &format!("InstanceAdded:{tag}"))?)
}

fn cs_removed_signal(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let tag: String = args(lua, a, "CollectionService:GetInstanceRemovedSignal")?;
    ret(
        lua,
        signal::push(lua, e, &format!("InstanceRemoved:{tag}"))?,
    )
}

type Imp = fn(&Lua, Entity, MultiValue) -> Result<MultiValue>;

/// Registers `GetService`, `FindService`, the `CollectionService` methods and
/// the `game` and `workspace` globals.
pub fn register(b: &mut Bindings) {
    b.method("ServiceProvider", "GetService", Arc::new(get_service));
    b.method(
        "ServiceProvider",
        "FindService",
        Arc::new(find_service_method),
    );
    let cs: &[(&str, Imp)] = &[
        ("AddTag", cs_add_tag),
        ("RemoveTag", cs_remove_tag),
        ("HasTag", cs_has_tag),
        ("GetTags", cs_get_tags),
        ("GetTagged", cs_get_tagged),
        ("GetAllTags", cs_get_all_tags),
        ("GetInstanceAddedSignal", cs_added_signal),
        ("GetInstanceRemovedSignal", cs_removed_signal),
    ];
    for (name, f) in cs {
        b.method("CollectionService", name, Arc::new(*f));
    }
    b.global("game", Arc::new(|lua, env| push(lua, env.game)));
    b.global("workspace", Arc::new(|lua, env| push(lua, env.workspace)));
    b.global("Game", Arc::new(|lua, env| push(lua, env.game)));
    b.global("Workspace", Arc::new(|lua, env| push(lua, env.workspace)));
}
