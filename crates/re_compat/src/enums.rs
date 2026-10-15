//! `Enum`: the Roblox-style enum tables, built from `enums/*.json`
//! (docs/M2_PLAN.md, WP10).
//!
//! * `Enum` is userdata of type `Enums`; `Enum.Material` is userdata of type
//!   `Enum`; `Enum.Material.Plastic` is an [`EnumItem`], userdata of type
//!   `EnumItem` with `Name`, `Value` and `EnumType`.
//! * Each VM creates every enum and item object once and hands out the same
//!   object afterwards, so items compare equal with `==` and `rawequal` and
//!   work as table keys.
//! * Writing an enum property accepts an item of the right type, its name or
//!   its number ([`EnumItem::coerce`]); the stored form is the item's name.

use std::collections::HashMap;
use std::sync::OnceLock;

use mlua::{
    AnyUserData, Lua, MetaMethod, Result, Table, UserData, UserDataFields, UserDataMethods, Value,
};

use crate::data::DataSet;
use crate::datatypes::{bad_argument, error, type_name, unsupported};

/// One enum type of the built-in data.
#[derive(Debug)]
struct EnumType {
    name: String,
    items: Vec<(String, i64)>,
    by_name: HashMap<String, u16>,
    by_value: HashMap<i64, u16>,
}

#[derive(Debug, Default)]
struct EnumTable {
    types: Vec<EnumType>,
    by_name: HashMap<String, u16>,
}

/// The enum types of the built-in data files, loaded once.
fn table() -> &'static EnumTable {
    static TABLE: OnceLock<EnumTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        // The built-in data is checked by the crate's tests: a broken file is
        // a build bug.
        let data = DataSet::builtin().unwrap_or_else(|e| panic!("built-in compat data: {e}"));
        let mut docs: Vec<_> = data.enums.into_iter().flat_map(|(_, f)| f.enums).collect();
        docs.sort_by(|a, b| a.name.cmp(&b.name));
        let mut t = EnumTable::default();
        for d in docs {
            let id = t.types.len() as u16;
            let items: Vec<(String, i64)> =
                d.items.into_iter().map(|i| (i.name, i.value)).collect();
            let by_name = items
                .iter()
                .enumerate()
                .map(|(i, (n, _))| (n.clone(), i as u16))
                .collect();
            let by_value = items
                .iter()
                .enumerate()
                .map(|(i, (_, v))| (*v, i as u16))
                .collect();
            t.by_name.insert(d.name.clone(), id);
            t.types.push(EnumType {
                name: d.name,
                items,
                by_name,
                by_value,
            });
        }
        t
    })
}

/// An enum item: a type and an item of the built-in enum data. Cheap to copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EnumItem {
    ty: u16,
    item: u16,
}

impl EnumItem {
    /// The item `item` of enum `ty`, if both exist.
    pub fn lookup(ty: &str, item: &str) -> Option<EnumItem> {
        let t = table();
        let ty_id = *t.by_name.get(ty)?;
        let item = *t.types[ty_id as usize].by_name.get(item)?;
        Some(EnumItem { ty: ty_id, item })
    }

    /// The item of enum `ty` with number `value`, if both exist.
    pub fn from_value(ty: &str, value: i64) -> Option<EnumItem> {
        let t = table();
        let ty_id = *t.by_name.get(ty)?;
        let item = *t.types[ty_id as usize].by_value.get(&value)?;
        Some(EnumItem { ty: ty_id, item })
    }

    /// The item `item` of enum `ty`, for names fixed in the engine's code.
    ///
    /// # Panics
    /// If the built-in data has no such item.
    pub fn named(ty: &str, item: &str) -> EnumItem {
        Self::lookup(ty, item).unwrap_or_else(|| panic!("no built-in enum item {ty}.{item}"))
    }

    fn ty(self) -> &'static EnumType {
        &table().types[self.ty as usize]
    }

    /// The enum type's name (`Material`).
    pub fn enum_name(self) -> &'static str {
        &self.ty().name
    }

    /// The item's name (`Plastic`).
    pub fn name(self) -> &'static str {
        &self.ty().items[self.item as usize].0
    }

    /// The item's number (`EnumItem.Value`).
    pub fn value(self) -> i64 {
        self.ty().items[self.item as usize].1
    }

    /// Reads the value a script wrote to an enum property of type `ty`: an
    /// item of that type, an item name or an item number. Anything else
    /// raises `RE3006`.
    pub fn coerce(v: &Value, ty: &str) -> Result<EnumItem> {
        let bad =
            |what: String| bad_argument(3, ty, format!("invalid value for Enum.{ty}: {what}"));
        match v {
            Value::UserData(ud) if ud.is::<EnumItem>() => {
                let item = *ud.borrow::<EnumItem>()?;
                if item.enum_name() == ty {
                    Ok(item)
                } else {
                    Err(bad(format!("Enum.{}.{}", item.enum_name(), item.name())))
                }
            }
            Value::String(s) => {
                let s = s.to_str()?;
                Self::lookup(ty, &s).ok_or_else(|| bad(format!("{:?}", &*s)))
            }
            Value::Integer(i) => Self::from_value(ty, *i).ok_or_else(|| bad(i.to_string())),
            Value::Number(n) if n.fract() == 0.0 => {
                Self::from_value(ty, *n as i64).ok_or_else(|| bad(n.to_string()))
            }
            other => Err(bad(format!("a {}", type_name(other)))),
        }
    }

    /// The VM's object for this item (the same object every time).
    pub fn to_lua(self, lua: &Lua) -> Result<Value> {
        let cache = cache(lua)?;
        let items: Table = match cache.items.raw_get::<Option<Table>>(self.ty as i64 + 1)? {
            Some(t) => t,
            None => {
                let t = lua.create_table()?;
                cache.items.raw_set(self.ty as i64 + 1, &t)?;
                t
            }
        };
        if let Some(ud) = items.raw_get::<Option<AnyUserData>>(self.item as i64 + 1)? {
            return Ok(Value::UserData(ud));
        }
        let ud = lua.create_userdata(self)?;
        items.raw_set(self.item as i64 + 1, &ud)?;
        Ok(Value::UserData(ud))
    }
}

/// The per-VM objects: enum types and items, created on first use.
struct Cache {
    types: Table,
    items: Table,
}

const CACHE_KEY: &str = "re_compat.enums";

fn cache(lua: &Lua) -> Result<Cache> {
    let t: Table = lua.named_registry_value(CACHE_KEY)?;
    Ok(Cache {
        types: t.raw_get("types")?,
        items: t.raw_get("items")?,
    })
}

/// The `Enum` object for type `ty` (the same object every time).
fn enum_object(lua: &Lua, ty: u16) -> Result<Value> {
    let cache = cache(lua)?;
    if let Some(ud) = cache.types.raw_get::<Option<AnyUserData>>(ty as i64 + 1)? {
        return Ok(Value::UserData(ud));
    }
    let ud = lua.create_userdata(EnumObject(ty))?;
    cache.types.raw_set(ty as i64 + 1, &ud)?;
    Ok(Value::UserData(ud))
}

impl UserData for EnumItem {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "EnumItem");
        fields.add_field_method_get("Name", |_, i| Ok(i.name()));
        fields.add_field_method_get("Value", |_, i| Ok(i.value()));
        fields.add_field_method_get("EnumType", |lua, i| enum_object(lua, i.ty));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("IsA", |_, i, name: String| Ok(i.enum_name() == name));
        methods.add_meta_method(MetaMethod::Eq, |_, i, o: Value| {
            Ok(matches!(&o, Value::UserData(u) if u.borrow::<EnumItem>().is_ok_and(|o| *i == *o)))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, i, ()| {
            Ok(format!("Enum.{}.{}", i.enum_name(), i.name()))
        });
        crate::datatypes::strict(methods, "EnumItem");
    }
}

/// One enum type as a Luau object (`Enum.Material`).
#[derive(Clone, Copy)]
struct EnumObject(u16);

impl EnumObject {
    fn ty(self) -> &'static EnumType {
        &table().types[self.0 as usize]
    }

    fn item(self, lua: &Lua, item: u16) -> Result<Value> {
        EnumItem { ty: self.0, item }.to_lua(lua)
    }
}

impl UserData for EnumObject {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "Enum");
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("GetEnumItems", |lua, e, ()| {
            let t = lua.create_table_with_capacity(e.ty().items.len(), 0)?;
            for i in 0..e.ty().items.len() {
                t.raw_push(e.item(lua, i as u16)?)?;
            }
            Ok(t)
        });
        methods.add_method("FromName", |lua, e, name: String| {
            match e.ty().by_name.get(&name) {
                Some(&i) => e.item(lua, i),
                None => Ok(Value::Nil),
            }
        });
        methods.add_method("FromValue", |lua, e, v: Value| {
            let n = match v {
                Value::Integer(i) => Some(i),
                Value::Number(n) if n.fract() == 0.0 => Some(n as i64),
                _ => None,
            };
            match n.and_then(|n| e.ty().by_value.get(&n)) {
                Some(&i) => e.item(lua, i),
                None => Ok(Value::Nil),
            }
        });
        methods.add_meta_method(MetaMethod::Index, |lua, e, key: Value| {
            let name = key.to_string().unwrap_or_else(|_| "?".into());
            match e.ty().by_name.get(&name) {
                Some(&i) => e.item(lua, i),
                None => Err(unsupported(&format!("Enum.{}", e.ty().name), &name)),
            }
        });
        methods.add_meta_function(
            MetaMethod::NewIndex,
            |_, (_e, k): (AnyUserData, Value)| -> Result<()> {
                Err(error(
                    re_script::codes::UNSUPPORTED_MEMBER,
                    format!(
                        "{} cannot be assigned to: enums are read-only",
                        k.to_string().unwrap_or_default()
                    ),
                ))
            },
        );
        methods.add_meta_method(MetaMethod::ToString, |_, e, ()| Ok(e.ty().name.clone()));
    }
}

/// The `Enum` global.
struct Enums;

impl UserData for Enums {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "Enums");
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("GetEnums", |lua, _, ()| {
            let t = lua.create_table_with_capacity(table().types.len(), 0)?;
            for i in 0..table().types.len() {
                t.raw_push(enum_object(lua, i as u16)?)?;
            }
            Ok(t)
        });
        methods.add_meta_method(MetaMethod::Index, |lua, _, key: Value| {
            let name = key.to_string().unwrap_or_else(|_| "?".into());
            match table().by_name.get(&name) {
                Some(&ty) => enum_object(lua, ty),
                None => Err(unsupported("Enum", &name)),
            }
        });
        methods.add_meta_function(
            MetaMethod::NewIndex,
            |_, (_e, k): (AnyUserData, Value)| -> Result<()> {
                Err(error(
                    re_script::codes::UNSUPPORTED_MEMBER,
                    format!(
                        "{} cannot be assigned to: enums are read-only",
                        k.to_string().unwrap_or_default()
                    ),
                ))
            },
        );
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("Enums"));
    }
}

/// Installs `Enum` into `globals`.
pub(crate) fn install(lua: &Lua, globals: &Table) -> Result<()> {
    let c = lua.create_table()?;
    c.raw_set("types", lua.create_table()?)?;
    c.raw_set("items", lua.create_table()?)?;
    lua.set_named_registry_value(CACHE_KEY, c)?;
    globals.raw_set("Enum", lua.create_userdata(Enums)?)
}

/// Every built-in enum type name, sorted.
pub fn enum_names() -> impl Iterator<Item = &'static str> {
    table().types.iter().map(|t| t.name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_coerce() {
        let i = EnumItem::named("Material", "Plastic");
        assert_eq!(i.enum_name(), "Material");
        assert_eq!(i.name(), "Plastic");
        assert_eq!(EnumItem::from_value("Material", i.value()), Some(i));
        assert_eq!(EnumItem::lookup("Material", "Nope"), None);
        assert_eq!(EnumItem::named("RunContext", "Client").value(), 2);
        let lua = Lua::new();
        let s = Value::String(lua.create_string("Neon").unwrap());
        assert_eq!(EnumItem::coerce(&s, "Material").unwrap().name(), "Neon");
        let n = Value::Integer(i.value());
        assert_eq!(EnumItem::coerce(&n, "Material").unwrap(), i);
        assert!(EnumItem::coerce(&Value::Boolean(true), "Material").is_err());
        assert!(enum_names().any(|n| n == "KeyCode"));
    }
}
