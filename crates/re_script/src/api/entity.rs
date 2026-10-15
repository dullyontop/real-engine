//! Entity handles: `Copy` userdata holding a runtime entity's bits (index plus
//! generation), interned in a weak-valued table so that the same entity is the
//! same Luau value (`==`, `rawequal` and table keys all work).
//!
//! A handle owns nothing. Each method looks the entity up in the bound
//! [`World`]; a despawned entity gives a clear "stale handle"
//! error. Component fields are read and written by name through the component
//! registry, so Luau numbers are coerced exactly as ADR-0032 says.

use std::ffi::c_void;

use mlua::{
    AnyUserData, LightUserData, Lua, MetaMethod, Result, Table, UserData, UserDataFields,
    UserDataMethods, Value,
};
use re_scene::{Entity, World, hierarchy};

use super::value::{field_from_lua, field_to_lua, fields_from_table};
use crate::codes;
use crate::host::with_host;

/// Registry key of the weak-valued handle table.
pub(crate) const HANDLE_CACHE: &str = "re_script.entity_handles";

/// The userdata scripts hold for an entity (`Entity` in Luau).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntityRef(pub Entity);

/// The declaration of `Entity` for `engine.d.luau`.
pub(crate) const DECL: &str = "-- A handle to an entity. The same entity is always the same value.
declare extern type Entity with
    -- The entity's stable id, or nil for an entity without one.
    read id: string?
    -- The display name (empty when it has none).
    read name: string
    -- False once the entity was despawned.
    read alive: boolean
    -- Reads a component field by name, e.g. e:get(\"Transform\", \"translation\").
    function get(self, component: string, field: string): any
    -- Writes a component field by name; numbers are coerced to the field type.
    function set(self, component: string, field: string, value: any): ()
    -- True when the entity has the component.
    function has(self, component: string): boolean
    -- Adds (or replaces) a component; unspecified fields take their defaults.
    function add(self, component: string, fields: { [string]: any }?): ()
    -- Removes a component (nothing happens when it is absent).
    function remove(self, component: string): ()
    -- The children, in order.
    function children(self): { Entity }
    -- The parent, or nil for a root.
    function parent(self): Entity?
    -- Despawns the entity and its subtree. A generated entity stays gone after a regeneration.
    function destroy(self): ()
    function __eq(self, other: Entity): boolean
    function __tostring(self): string
end";

/// Returns the one userdata for `entity`, creating it on first use.
pub fn handle(lua: &Lua, entity: Entity) -> Result<AnyUserData> {
    let cache: Table = lua.named_registry_value(HANDLE_CACHE)?;
    cached_handle(lua, &cache, entity)
}

/// A Luau array of the handles of `entities`, in order.
pub(crate) fn handle_list(lua: &Lua, entities: &[Entity]) -> Result<Table> {
    let cache: Table = lua.named_registry_value(HANDLE_CACHE)?;
    let out = lua.create_table_with_capacity(entities.len(), 0)?;
    for &e in entities {
        out.raw_push(cached_handle(lua, &cache, e)?)?;
    }
    Ok(out)
}

fn cached_handle(lua: &Lua, cache: &Table, entity: Entity) -> Result<AnyUserData> {
    let key = LightUserData(entity.to_bits() as usize as *mut c_void);
    if let Value::UserData(ud) = cache.raw_get::<Value>(key)? {
        return Ok(ud);
    }
    let ud = lua.create_userdata(EntityRef(entity))?;
    cache.raw_set(key, &ud)?;
    Ok(ud)
}

/// Creates the handle table. Called once per VM, before the sandbox is enabled.
pub(crate) fn init(lua: &Lua) -> Result<()> {
    let cache = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.raw_set("__mode", "v")?;
    cache.set_metatable(Some(mt))?;
    lua.set_named_registry_value(HANDLE_CACHE, cache)
}

/// The number of live handles in the cache (for tests and diagnostics).
pub fn cached_handles(lua: &Lua) -> Result<usize> {
    let cache: Table = lua.named_registry_value(HANDLE_CACHE)?;
    let mut n = 0;
    cache.for_each(|_: Value, _: Value| {
        n += 1;
        Ok(())
    })?;
    Ok(n)
}

fn stale(e: Entity) -> mlua::Error {
    super::error(
        codes::STALE_HANDLE,
        format!("Entity({e}) is no longer alive (stale handle)"),
    )
}

/// Checks that the entity exists.
pub(crate) fn alive(world: &World, e: Entity) -> Result<()> {
    if world.ecs.get_entity(e).is_ok() {
        Ok(())
    } else {
        Err(stale(e))
    }
}

fn reflect_err(e: re_reflect::ReflectError) -> mlua::Error {
    super::error(e.code(), e)
}

impl UserData for EntityRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |lua, this| {
            with_host(lua, |h| Ok(h.world().id_of(this.0).map(str::to_string)))
        });
        fields.add_field_method_get("name", |lua, this| {
            with_host(lua, |h| {
                let w = h.world();
                alive(w, this.0)?;
                Ok(w.name_of(this.0).to_string())
            })
        });
        fields.add_field_method_get("alive", |lua, this| {
            with_host(lua, |h| Ok(h.world().ecs.get_entity(this.0).is_ok()))
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("get", |lua, this, (component, field): (String, String)| {
            let value = with_host(lua, |h| {
                let w = h.world();
                alive(w, this.0)?;
                let key = w.registry.key(&component).map_err(reflect_err)?;
                let index = w
                    .registry
                    .schema(key)
                    .field_index(&field)
                    .map_err(reflect_err)?;
                w.registry
                    .get_field(&w.ecs, this.0, key, index)
                    .ok_or_else(|| {
                        reflect_err(re_reflect::ReflectError::MissingComponent { component })
                    })
            })?;
            field_to_lua(lua, value)
        });
        methods.add_method(
            "set",
            |lua, this, (component, field, value): (String, String, Value)| {
                // Converting a handle argument needs no world access, so it
                // happens before the world is borrowed.
                let raw = super::value::RawValue::from_lua(value)?;
                with_host(lua, |h| {
                    let w = h.world();
                    alive(w, this.0)?;
                    let key = w.registry.key(&component).map_err(reflect_err)?;
                    let schema = w.registry.schema(key);
                    let index = schema.field_index(&field).map_err(reflect_err)?;
                    let v = field_from_lua(raw, &schema.fields[index].ty)
                        .map_err(|m| type_error(&component, &field, m))?;
                    w.registry
                        .set_field(&mut w.ecs, this.0, key, index, v)
                        .map_err(reflect_err)
                })
            },
        );
        methods.add_method("has", |lua, this, component: String| {
            with_host(lua, |h| {
                let w = h.world();
                alive(w, this.0)?;
                let key = w.registry.key(&component).map_err(reflect_err)?;
                Ok(w.registry.has(&w.ecs, this.0, key))
            })
        });
        methods.add_method(
            "add",
            |lua, this, (component, fields): (String, Option<Table>)| {
                let given = match fields {
                    Some(t) => fields_from_table(&component, t)?,
                    None => Vec::new(),
                };
                with_host(lua, |h| {
                    insert_component(h.world(), this.0, &component, given)
                })
            },
        );
        methods.add_method("remove", |lua, this, component: String| {
            with_host(lua, |h| {
                let w = h.world();
                alive(w, this.0)?;
                let key = w.registry.key(&component).map_err(reflect_err)?;
                w.registry.remove(&mut w.ecs, this.0, key);
                Ok(())
            })
        });
        methods.add_method("children", |lua, this, ()| {
            let kids = with_host(lua, |h| {
                let w = h.world();
                alive(w, this.0)?;
                Ok(hierarchy::children(&w.ecs, this.0).to_vec())
            })?;
            let out = lua.create_table_with_capacity(kids.len(), 0)?;
            for k in kids {
                out.raw_push(handle(lua, k)?)?;
            }
            Ok(out)
        });
        methods.add_method("parent", |lua, this, ()| {
            let parent = with_host(lua, |h| {
                let w = h.world();
                alive(w, this.0)?;
                Ok(hierarchy::parent(&w.ecs, this.0))
            })?;
            parent.map(|p| handle(lua, p)).transpose()
        });
        methods.add_method("destroy", |lua, this, ()| {
            with_host(lua, |h| {
                h.world()
                    .despawn_runtime(this.0)
                    .map_err(|e| super::error(e.code(), e))
            })
        });
        methods.add_meta_method(MetaMethod::Eq, |_, this, other: Value| {
            Ok(match other {
                Value::UserData(ud) => ud.borrow::<EntityRef>().is_ok_and(|o| o.0 == this.0),
                _ => false,
            })
        });
        methods.add_meta_method(MetaMethod::ToString, |lua, this, ()| {
            let label = with_host(lua, |h| {
                let w = h.world();
                Ok(if w.ecs.get_entity(this.0).is_err() {
                    "<dead>".to_string()
                } else {
                    w.id_of(this.0)
                        .map(|id| format!("{id:?}"))
                        .unwrap_or_else(|| format!("{:?}", w.name_of(this.0)))
                })
            })
            .unwrap_or_else(|_| "?".into());
            Ok(format!("Entity({}, {label})", this.0))
        });
        super::strict_userdata(methods, "Entity");
    }
}

pub(crate) fn type_error(component: &str, field: &str, message: String) -> mlua::Error {
    super::error(
        codes::BAD_ARGUMENT,
        format!("{component}.{field}: {message}"),
    )
}

/// Inserts `component` on `e` with its defaults overridden by `given`.
pub(crate) fn insert_component(
    w: &mut World,
    e: Entity,
    component: &str,
    given: Vec<(String, super::value::RawValue)>,
) -> Result<()> {
    alive(w, e)?;
    let key = w.registry.key(component).map_err(reflect_err)?;
    let schema = w.registry.schema(key);
    let mut values = schema.defaults();
    for (name, raw) in given {
        let index = schema.field_index(&name).map_err(reflect_err)?;
        values[index] = field_from_lua(raw, &schema.fields[index].ty)
            .map_err(|m| type_error(component, &name, m))?;
    }
    w.registry
        .insert(&mut w.ecs, e, key, values)
        .map_err(reflect_err)
}
