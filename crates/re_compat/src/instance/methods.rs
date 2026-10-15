//! The methods of `Instance` and the `Instance` global.

use std::sync::Arc;

use mlua::{FromLuaMulti, IntoLuaMulti, Lua, MultiValue, Result, Value as LuaValue};
use re_scene::Entity;
use re_script::codes::{BAD_ARGUMENT, UNSUPPORTED_MEMBER};
use re_script::sched;

use super::{Tombstone, attributes, clone, handle, push, push_list, tree};
use crate::error::CompatError;
use crate::runtime::{Bindings, compat_error, lua_error, state, with_world};
use crate::scene::spawn_instance;
use crate::signal::{self, Guard, SignalKey, events};

/// Reads a method's arguments, naming the method in errors.
pub fn args<T: FromLuaMulti>(lua: &Lua, a: MultiValue, what: &str) -> Result<T> {
    T::from_lua_multi(a, lua)
        .map_err(|e| lua_error(BAD_ARGUMENT, format!("bad argument to {what}: {e}")))
}

/// Packs a method's results.
pub fn ret(lua: &Lua, v: impl IntoLuaMulti) -> Result<MultiValue> {
    v.into_lua_multi(lua)
}

fn opt_push(lua: &Lua, e: Option<Entity>) -> Result<MultiValue> {
    match e {
        Some(e) => ret(lua, push(lua, e)?),
        None => ret(lua, LuaValue::Nil),
    }
}

type Imp = fn(&Lua, Entity, MultiValue) -> Result<MultiValue>;

/// Registers every `Instance` method and the `Instance` global.
pub(crate) fn register(b: &mut Bindings) {
    let plain: &[(&str, Imp)] = &[
        ("IsA", is_a),
        ("isA", is_a),
        ("FindFirstChild", find_first_child),
        ("findFirstChild", find_first_child),
        ("FindFirstChildOfClass", find_first_child_of_class),
        ("FindFirstChildWhichIsA", find_first_child_which_is_a),
        ("FindFirstAncestor", find_first_ancestor),
        ("FindFirstAncestorOfClass", find_first_ancestor_of_class),
        ("FindFirstAncestorWhichIsA", find_first_ancestor_which_is_a),
        ("FindFirstDescendant", find_first_descendant),
        ("GetChildren", get_children),
        ("getChildren", get_children),
        ("children", get_children),
        ("GetDescendants", get_descendants),
        ("IsDescendantOf", is_descendant_of),
        ("IsAncestorOf", is_ancestor_of),
        ("GetFullName", get_full_name),
        ("Destroy", destroy_method),
        ("destroy", destroy_method),
        ("Remove", remove),
        ("remove", remove),
        ("ClearAllChildren", clear_all_children),
        ("Clone", clone_method),
        ("clone", clone_method),
        ("GetAttribute", get_attribute),
        ("SetAttribute", set_attribute),
        ("GetAttributes", get_attributes),
        ("GetAttributeChangedSignal", get_attribute_changed_signal),
        ("AddTag", add_tag),
        ("RemoveTag", remove_tag),
        ("HasTag", has_tag),
        ("GetTags", get_tags),
        ("GetPropertyChangedSignal", get_property_changed_signal),
    ];
    for (name, f) in plain {
        b.method("Instance", name, Arc::new(*f));
    }
    b.yielding_method("Instance", "WaitForChild", Arc::new(wait_for_child));
    b.global("Instance", Arc::new(|lua, _| instance_global(lua)));
}

fn is_a(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let class: String = args(lua, a, "IsA")?;
    let st = state(lua)?;
    let live = with_world(lua, |w| tree::class(w, e).map(str::to_string))?;
    let own = match live {
        Some(c) => Some(c),
        None => tomb_of(lua, e)?.map(|t| t.class),
    };
    ret(lua, own.is_some_and(|c| st.setup.classes.is_a(&c, &class)))
}

fn tomb_of(lua: &Lua, e: Entity) -> Result<Option<Tombstone>> {
    let ud = handle(lua, e)?;
    let r = ud.borrow::<super::InstanceRef>()?;
    Ok(r.tomb.borrow().as_deref().cloned())
}

fn find_first_child(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (name, recursive): (String, Option<bool>) = args(lua, a, "FindFirstChild")?;
    let found = with_world(lua, |w| {
        tree::alive(w, e)
            .then(|| tree::find_child(w, e, &name, recursive.unwrap_or(false)))
            .flatten()
    })?;
    opt_push(lua, found)
}

fn find_first_child_of_class(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let class: String = args(lua, a, "FindFirstChildOfClass")?;
    let found = with_world(lua, |w| {
        tree::children(w, e)
            .into_iter()
            .find(|c| tree::class(w, *c) == Some(class.as_str()))
    })?;
    opt_push(lua, found)
}

fn find_first_child_which_is_a(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (class, recursive): (String, Option<bool>) = args(lua, a, "FindFirstChildWhichIsA")?;
    let st = state(lua)?;
    let found = with_world(lua, |w| {
        let list = if recursive.unwrap_or(false) {
            tree::descendants(w, e)
        } else {
            tree::children(w, e)
        };
        list.into_iter()
            .find(|c| tree::is_a(w, &st.setup.classes, *c, &class))
    })?;
    opt_push(lua, found)
}

fn find_first_ancestor(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "FindFirstAncestor")?;
    let found = with_world(lua, |w| {
        tree::ancestors(w, e)
            .into_iter()
            .find(|x| tree::is_instance(w, *x) && w.name_of(*x) == name)
    })?;
    opt_push(lua, found)
}

fn find_first_ancestor_of_class(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let class: String = args(lua, a, "FindFirstAncestorOfClass")?;
    let found = with_world(lua, |w| {
        tree::ancestors(w, e)
            .into_iter()
            .find(|x| tree::class(w, *x) == Some(class.as_str()))
    })?;
    opt_push(lua, found)
}

fn find_first_ancestor_which_is_a(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let class: String = args(lua, a, "FindFirstAncestorWhichIsA")?;
    let st = state(lua)?;
    let found = with_world(lua, |w| {
        tree::ancestors(w, e)
            .into_iter()
            .find(|x| tree::is_a(w, &st.setup.classes, *x, &class))
    })?;
    opt_push(lua, found)
}

fn find_first_descendant(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "FindFirstDescendant")?;
    let found = with_world(lua, |w| {
        tree::alive(w, e)
            .then(|| tree::find_child(w, e, &name, true))
            .flatten()
    })?;
    opt_push(lua, found)
}

fn get_children(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let list = with_world(lua, |w| tree::children(w, e))?;
    ret(lua, push_list(lua, &list)?)
}

fn get_descendants(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let list = with_world(lua, |w| tree::descendants(w, e))?;
    ret(lua, push_list(lua, &list)?)
}

fn other_instance(lua: &Lua, a: MultiValue, what: &str) -> Result<Option<Entity>> {
    let v: LuaValue = args(lua, a, what)?;
    match v {
        LuaValue::Nil => Ok(None),
        v => super::arg_instance(&v, what).map(Some),
    }
}

fn is_descendant_of(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let Some(other) = other_instance(lua, a, "IsDescendantOf")? else {
        return ret(lua, false);
    };
    let r = with_world(lua, |w| tree::ancestors(w, e).contains(&other))?;
    ret(lua, r)
}

fn is_ancestor_of(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let Some(other) = other_instance(lua, a, "IsAncestorOf")? else {
        return ret(lua, false);
    };
    let r = with_world(lua, |w| tree::ancestors(w, other).contains(&e))?;
    ret(lua, r)
}

fn get_full_name(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let live = with_world(lua, |w| tree::alive(w, e).then(|| tree::full_name(w, e)))?;
    let name = match live {
        Some(n) => n,
        None => tomb_of(lua, e)?.map(|t| t.name).unwrap_or_default(),
    };
    ret(lua, name)
}

/// Destroys an instance: fires `Destroying` on it and its descendants,
/// disconnects their signals, takes the subtree out of the tree (so
/// `ChildRemoved`, `DescendantRemoving` and `CollectionService` see it leave)
/// and locks its `Parent`. The subtree stays readable until the resumption
/// cycle settles, so deferred handlers see its properties and attributes;
/// then it is despawned and its handles keep only its name and class. A
/// destroyed instance is left alone; `game` and services refuse (`RE9008`).
pub fn destroy(lua: &Lua, e: Entity) -> Result<()> {
    let info = with_world(lua, |w| {
        if !tree::is_instance(w, e) || tree::is_destroyed(w, e) {
            return Ok(None);
        }
        if tree::is_locked(w, e) {
            return Err(CompatError::Runtime {
                code: crate::codes::INVALID_PARENT,
                message: format!("The Parent property of {} is locked", tree::name(w, e)),
            });
        }
        let mut sub = vec![e];
        sub.extend(tree::descendants(w, e));
        Ok(Some(sub))
    })?
    .map_err(compat_error)?;
    let Some(sub) = info else { return Ok(()) };
    for x in &sub {
        signal::fire_with(
            lua,
            &SignalKey::new(*x, "Destroying"),
            MultiValue::new(),
            Guard::None,
        )?;
    }
    signal::disconnect_entities(lua, &sub)?;
    for x in &sub {
        crate::runtime::scripts::stop(lua, *x)?;
        super::drop_callbacks(lua, *x)?;
    }
    let detached = with_world(lua, |w| {
        if tree::is_destroyed(w, e) {
            return false; // a Destroying handler destroyed it already
        }
        if tree::parent(w, e).is_some() {
            re_scene::hierarchy::remove_parent(&mut w.ecs, e);
        }
        for x in tree::descendants(w, e).into_iter().chain([e]) {
            w.ecs.entity_mut(x).insert(tree::Destroyed);
        }
        true
    })?;
    if detached {
        events::despawn_later(lua, e);
    }
    events::after_write(lua)
}

fn destroy_method(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    destroy(lua, e)?;
    ret(lua, ())
}

fn remove(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    with_world(lua, |w| tree::set_parent(w, e, None))?.map_err(compat_error)?;
    events::after_write(lua)?;
    ret(lua, ())
}

fn clear_all_children(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let kids = with_world(lua, |w| tree::children(w, e))?;
    for c in kids {
        destroy(lua, c)?;
    }
    ret(lua, ())
}

fn clone_method(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let copy = with_world(lua, |w| {
        if tree::is_instance(w, e) {
            clone::clone_subtree(w, e)
        } else {
            None
        }
    })?;
    events::after_write(lua)?;
    opt_push(lua, copy)
}

fn get_attribute(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "GetAttribute")?;
    let v = with_world(lua, |w| attributes::get(w, e, &name))?;
    match v {
        None => ret(lua, LuaValue::Nil),
        Some(v) => ret(lua, state(lua)?.setup.codec.dyn_to_lua(lua, v)?),
    }
}

fn set_attribute(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (name, value): (String, LuaValue) = args(lua, a, "SetAttribute")?;
    attributes::check_name(&name).map_err(compat_error)?;
    let st = state(lua)?;
    let v = st.setup.codec.attribute_from_lua(lua, value).map_err(|m| {
        lua_error(
            crate::codes::INVALID_ATTRIBUTE,
            format!("SetAttribute: {m}"),
        )
    })?;
    let alive = with_world(lua, |w| {
        let alive = tree::alive(w, e);
        if alive {
            attributes::set(w, e, &name, v);
        }
        alive
    })?;
    if alive {
        events::after_write(lua)?;
    }
    ret(lua, ())
}

fn get_attributes(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let all = with_world(lua, |w| attributes::all(w, e))?;
    let codec = &state(lua)?.setup.codec;
    let t = lua.create_table_with_capacity(0, all.len())?;
    for (k, v) in all {
        t.raw_set(k, codec.dyn_to_lua(lua, v)?)?;
    }
    ret(lua, t)
}

fn get_attribute_changed_signal(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "GetAttributeChangedSignal")?;
    ret(
        lua,
        signal::push(lua, e, &format!("AttributeChanged:{name}"))?,
    )
}

fn tag_arg(lua: &Lua, a: MultiValue, what: &str) -> Result<String> {
    args(lua, a, what)
}

fn add_tag(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let tag = tag_arg(lua, a, "AddTag")?;
    with_world(lua, |w| {
        if tree::alive(w, e) {
            attributes::add_tag(w, e, &tag)
        }
    })?;
    events::after_write(lua)?;
    ret(lua, ())
}

fn remove_tag(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let tag = tag_arg(lua, a, "RemoveTag")?;
    with_world(lua, |w| {
        if tree::alive(w, e) {
            attributes::remove_tag(w, e, &tag)
        }
    })?;
    events::after_write(lua)?;
    ret(lua, ())
}

fn has_tag(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let tag = tag_arg(lua, a, "HasTag")?;
    ret(lua, with_world(lua, |w| attributes::has_tag(w, e, &tag))?)
}

fn get_tags(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let tags = with_world(lua, |w| attributes::tags(w, e))?;
    ret(lua, lua.create_sequence_from(tags)?)
}

fn get_property_changed_signal(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let prop: String = args(lua, a, "GetPropertyChangedSignal")?;
    let st = state(lua)?;
    let class = with_world(lua, |w| tree::class(w, e).map(str::to_string))?
        .or(tomb_of(lua, e)?.map(|t| t.class))
        .unwrap_or_else(|| "Instance".into());
    let m = st
        .setup
        .classes
        .member(&class, &prop)
        .map_err(compat_error)?;
    if !matches!(m.member, crate::class::Member::Property(_)) {
        return Err(compat_error(CompatError::NotAProperty {
            class,
            member: prop,
            kind: m.member.kind(),
        }));
    }
    m.require_implemented_by_script().map_err(compat_error)?;
    ret(
        lua,
        signal::push(lua, e, &format!("PropertyChanged:{prop}"))?,
    )
}

fn wait_for_child(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (name, timeout): (String, Option<f64>) = args(lua, a, "WaitForChild")?;
    let (found, path) = with_world(lua, |w| {
        let found = tree::alive(w, e)
            .then(|| tree::find_child(w, e, &name, false))
            .flatten();
        (found, tree::full_name(w, e))
    })?;
    if let Some(c) = found {
        return ret(lua, push(lua, c)?);
    }
    let label = format!("{path}:WaitForChild({name:?})");
    let (values, token) = sched::park(
        lua,
        &label,
        timeout.map(|t| (t, MultiValue::from_iter([LuaValue::Nil]))),
    )?;
    events::add_child_waiter(lua, e, name, token, timeout.is_none())?;
    Ok(values)
}

fn instance_new(lua: &Lua, (class, parent): (String, LuaValue)) -> Result<LuaValue> {
    let st = state(lua)?;
    let classes = &st.setup.classes;
    let def = match classes.class(&class) {
        Ok(d) => d,
        Err(e) => {
            return Err(crate::runtime::unsupported(
                lua,
                format!("Instance.new({class})"),
                e,
            ));
        }
    };
    if !def.creatable || def.service {
        return Err(lua_error(
            BAD_ARGUMENT,
            format!("Unable to create an Instance of type \"{class}\""),
        ));
    }
    if !matches!(
        def.status,
        crate::Status::Implemented | crate::Status::Partial
    ) {
        return Err(crate::runtime::unsupported(
            lua,
            format!("Instance.new({class})"),
            format!(
                "unsupported API member: class {class} is not implemented yet ({})",
                def.status.as_str()
            ),
        ));
    }
    let parent = match parent {
        LuaValue::Nil => None,
        v => Some(super::arg_instance(&v, "Instance.new")?),
    };
    let e = with_world(lua, |w| -> std::result::Result<Entity, CompatError> {
        let e = spawn_instance(w, classes, &class, None, &class, None)?;
        if let Some(p) = parent
            && let Err(err) = tree::set_parent(w, e, Some(p))
        {
            events::suppressed(w, |w| {
                let _ = w.despawn_runtime(e);
            });
            return Err(err);
        }
        Ok(e)
    })?
    .map_err(compat_error)?;
    events::after_write(lua)?;
    push(lua, e)
}

fn instance_global(lua: &Lua) -> Result<LuaValue> {
    let t = lua.create_table()?;
    t.raw_set("new", lua.create_function(instance_new)?)?;
    let mt = lua.create_table()?;
    mt.raw_set(
        "__index",
        lua.create_function(|_, (_t, key): (LuaValue, LuaValue)| -> Result<LuaValue> {
            let key = key.to_string().unwrap_or_else(|_| "?".into());
            Err(lua_error(
                UNSUPPORTED_MEMBER,
                format!("unsupported API member: Instance.{key}"),
            ))
        })?,
    )?;
    mt.raw_set("__metatable", false)?;
    t.set_metatable(Some(mt))?;
    t.set_readonly(true);
    Ok(LuaValue::Table(t))
}
