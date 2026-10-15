//! The `world` library.

use mlua::{Lua, Result, Table, Value};
use re_reflect::{ComponentOrigin, ComponentSchema, FieldSchema, FieldType};
use re_scene::{Entity, Name, StableId, World, hierarchy};

use super::Lib;
use super::entity::{EntityRef, handle, insert_component};
use super::value::{RawValue, field_from_lua, fields_from_table};
use crate::codes;
use crate::host::with_host;

pub(crate) fn lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "get",
        "(id: string) -> Entity?",
        "The entity with this stable id, or nil.",
        |lua, id: String| {
            let e = with_host(lua, |h| Ok(h.world().ids().get(&id)))?;
            e.map(|e| handle(lua, e)).transpose()
        },
    )?;
    lib.func(
        lua,
        "find",
        "(path: string) -> Entity?",
        "The entity at a path of names such as \"Island/Crates/Crate_3\", or nil (a convenience, not an identity).",
        |lua, path: String| {
            let e = with_host(lua, |h| Ok(h.world().find(&path)))?;
            e.map(|e| handle(lua, e)).transpose()
        },
    )?;
    lib.func(
        lua,
        "query",
        "(...string) -> { Entity }",
        "Every entity that has all the named components, ordered by stable id.",
        |lua, names: mlua::Variadic<String>| {
            let found = with_host(lua, |h| query(h.world(), &names))?;
            super::entity::handle_list(lua, &found)
        },
    )?;
    lib.func(
        lua,
        "spawn",
        "(spec: { id: string?, name: string?, parent: Entity?, components: { [string]: { [string]: any } }? }?) -> Entity",
        "Spawns a runtime entity (with a fresh stable id unless one is given) and its components.",
        |lua, spec: Option<Table>| {
            let spec = SpawnSpec::read(spec)?;
            let e = with_host(lua, |h| spawn(h.world(), spec))?;
            handle(lua, e)
        },
    )?;
    lib.func(
        lua,
        "define_component",
        "(name: string, fields: { [string]: string | { type: string, default: any?, doc: string?, variants: { string }? } }) -> ()",
        "Declares a script component, e.g. define_component(\"Collectible\", { value = \"i32\" }). Defining the same shape again is allowed.",
        |lua, (name, fields): (String, Table)| {
            let schema = read_schema(&name, fields)?;
            with_host(lua, |h| define(h.world(), schema))
        },
    )?;
    lib.func(
        lua,
        "restart",
        "() -> ()",
        "Asks the runtime to reload the entry scene at the end of the frame.",
        |lua, ()| {
            with_host(lua, |h| {
                h.services().request_restart();
                Ok(())
            })
        },
    )?;
    Ok(())
}

/// Every entity with all of `names`, sorted by stable id (entities without one
/// after, by entity bits).
pub(crate) fn query(w: &World, names: &[String]) -> Result<Vec<Entity>> {
    let mut ids = Vec::with_capacity(names.len());
    for n in names {
        let key = w.registry.key(n).map_err(|e| super::error(e.code(), e))?;
        ids.push(w.registry.component_id(key));
    }
    let mut found: Vec<(Option<&str>, u64, Entity)> = Vec::new();
    for archetype in w.ecs.archetypes().iter() {
        if archetype.is_empty() || !ids.iter().all(|id| archetype.contains(*id)) {
            continue;
        }
        for e in archetype.entities() {
            let e = e.id();
            found.push((w.id_of(e), e.to_bits(), e));
        }
    }
    found.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(&b.1),
    });
    Ok(found.into_iter().map(|f| f.2).collect())
}

struct SpawnSpec {
    id: Option<String>,
    name: Option<String>,
    parent: Option<Entity>,
    components: Vec<(String, Vec<(String, RawValue)>)>,
}

impl SpawnSpec {
    fn read(spec: Option<Table>) -> Result<SpawnSpec> {
        let Some(t) = spec else {
            return Ok(SpawnSpec {
                id: None,
                name: None,
                parent: None,
                components: Vec::new(),
            });
        };
        for pair in t.pairs::<String, Value>() {
            let (k, _) = pair?;
            if !matches!(k.as_str(), "id" | "name" | "parent" | "components") {
                return Err(super::error(
                    codes::BAD_ARGUMENT,
                    format!(
                        "world.spawn: unknown key {k:?} (expected id, name, parent, components)"
                    ),
                ));
            }
        }
        let parent = match t.get::<Value>("parent")? {
            Value::Nil => None,
            Value::UserData(ud) => Some(
                ud.borrow::<EntityRef>()
                    .map_err(|_| {
                        super::error(codes::BAD_ARGUMENT, "world.spawn: parent must be an Entity")
                    })?
                    .0,
            ),
            _ => {
                return Err(super::error(
                    codes::BAD_ARGUMENT,
                    "world.spawn: parent must be an Entity",
                ));
            }
        };
        let mut components = Vec::new();
        if let Some(c) = t.get::<Option<Table>>("components")? {
            for pair in c.pairs::<String, Table>() {
                let (name, fields) = pair?;
                let fields = fields_from_table(&name, fields)?;
                components.push((name, fields));
            }
        }
        components.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(SpawnSpec {
            id: t.get("id")?,
            name: t.get("name")?,
            parent,
            components,
        })
    }
}

fn spawn(w: &mut World, spec: SpawnSpec) -> Result<Entity> {
    // Validate everything before spawning, so a bad spec leaves no entity behind.
    if let Some(p) = spec.parent {
        super::entity::alive(w, p)?;
    }
    for (name, fields) in &spec.components {
        let key = w
            .registry
            .key(name)
            .map_err(|e| super::error(e.code(), e))?;
        let schema = w.registry.schema(key);
        for (f, raw) in fields {
            let index = schema
                .field_index(f)
                .map_err(|e| super::error(e.code(), e))?;
            field_from_lua(raw.clone(), &schema.fields[index].ty)
                .map_err(|m| super::entity::type_error(name, f, m))?;
        }
    }
    let id = match spec.id {
        Some(id) => {
            re_scene::validate_id(&id).map_err(|e| super::error(e.code(), e))?;
            if w.ids().get(&id).is_some() {
                return Err(super::error(
                    codes::BAD_ARGUMENT,
                    format!("world.spawn: an entity with id {id:?} already exists"),
                ));
            }
            id
        }
        None => w.new_id(),
    };
    let e = w
        .ecs
        .spawn((StableId(id), Name(spec.name.unwrap_or_default())))
        .id();
    if let Some(p) = spec.parent {
        hierarchy::add_child(&mut w.ecs, p, e);
    }
    for (name, fields) in spec.components {
        if let Err(err) = insert_component(w, e, &name, fields) {
            hierarchy::despawn_recursive(&mut w.ecs, e);
            return Err(err);
        }
    }
    Ok(e)
}

fn schema_error(name: &str, msg: impl std::fmt::Display) -> mlua::Error {
    super::error(
        codes::COMPONENT_DEFINITION,
        format!("world.define_component({name:?}): {msg}"),
    )
}

fn read_schema(name: &str, fields: Table) -> Result<ComponentSchema> {
    let mut out = Vec::new();
    for pair in fields.pairs::<String, Value>() {
        let (field, spec) = pair?;
        let (kind, default, doc, variants) = match spec {
            Value::String(s) => (
                s.to_str()?.to_string(),
                Value::Nil,
                String::new(),
                Vec::new(),
            ),
            Value::Table(t) => {
                let kind: Option<String> = t.get("type")?;
                let kind = kind
                    .ok_or_else(|| schema_error(name, format!("field {field:?} needs a `type`")))?;
                let variants: Vec<String> = t
                    .get::<Option<Vec<String>>>("variants")?
                    .unwrap_or_default();
                (
                    kind,
                    t.get::<Value>("default")?,
                    t.get::<Option<String>>("doc")?.unwrap_or_default(),
                    variants,
                )
            }
            _ => {
                return Err(schema_error(
                    name,
                    format!("field {field:?} must be a type name or a table with `type`"),
                ));
            }
        };
        let ty = FieldType::from_name(&kind, &variants).ok_or_else(|| {
            schema_error(
                name,
                format!(
                    "field {field:?} has unknown type {kind:?} (types: bool, i32, i64, f32, f64, vec3, quat, color, string, entity, enum with `variants`)"
                ),
            )
        })?;
        let default = match default {
            Value::Nil => default_of(&ty),
            v => field_from_lua(RawValue::from_lua(v)?, &ty)
                .map_err(|m| schema_error(name, format!("field {field:?}: {m}")))?,
        };
        out.push(FieldSchema {
            name: field,
            ty,
            doc,
            default,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(ComponentSchema {
        name: name.to_string(),
        doc: String::new(),
        fields: out,
        origin: ComponentOrigin::Script,
    })
}

fn default_of(ty: &FieldType) -> re_reflect::Value {
    use re_reflect::Value as V;
    match ty {
        FieldType::Bool => V::Bool(false),
        FieldType::I32 | FieldType::I64 => V::Int(0),
        FieldType::F32 | FieldType::F64 => V::Float(0.0),
        FieldType::Vec3 => V::Vec3(glam::Vec3::ZERO),
        FieldType::Quat => V::Quat(glam::Quat::IDENTITY),
        FieldType::Color => V::Color(re_reflect::Color::WHITE),
        FieldType::String => V::Str(String::new()),
        FieldType::Entity => V::Entity(None),
        FieldType::Enum(v) => V::Enum(v.first().copied().unwrap_or_default().to_string()),
    }
}

fn define(w: &mut World, schema: ComponentSchema) -> Result<()> {
    if let Some(key) = w.registry.try_key(&schema.name) {
        let existing = w.registry.schema(key);
        if existing.origin == ComponentOrigin::Script && existing.same_shape(&schema) {
            return Ok(()); // re-running a script (hot reload, restart) is fine
        }
        return Err(schema_error(
            &schema.name,
            if existing.origin == ComponentOrigin::Native {
                "a native component already has this name".to_string()
            } else {
                "a script component with this name already exists with different fields".to_string()
            },
        ));
    }
    let World { ecs, registry, .. } = w;
    registry
        .register_script(ecs, schema)
        .map(|_| ())
        .map_err(|e| super::error(e.code(), e))
}
