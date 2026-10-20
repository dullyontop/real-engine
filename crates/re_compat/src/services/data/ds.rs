//! `DataStoreService`, `GlobalDataStore`, `DataStore`, `OrderedDataStore`
//! and `DataStoreSetOptions`.

use mlua::{Function, Lua, MultiValue, RegistryKey, Result, Table, Value as LuaValue};
use re_scene::Entity;
use re_script::sched;
use serde_json::Value as J;

use super::budget::Kind;
use super::store::{StoreId, StoreKind};
use super::value::{self, as_integer, stored_len, to_stored};
use super::{
    Done, Handle, Op as AnyOp, add, bad, data, handle_instance, handle_of, now_ms, pages_table,
    paginate, players, queue_full, saved, submit, usable, warn_unreadable,
};
use crate::enums::EnumItem;
use crate::instance::methods_api::{args, ret};
use crate::runtime::Bindings;

/// The longest store name, scope, key or metadata key, in characters.
const MAX_NAME: usize = 50;
/// The longest stored value, in characters of its JSON text.
const MAX_VALUE: usize = 4_194_304;
/// The longest metadata value, in characters.
const MAX_META_VALUE: usize = 250;
/// The most characters all metadata keys and values may have together.
const MAX_META_TOTAL: usize = 300;
/// The largest page.
const MAX_PAGE: usize = 100;
/// The scope `GetDataStore` uses when none is given.
const DEFAULT_SCOPE: &str = "global";

/// A data store request, kept until its runner completes it.
pub(super) enum Op {
    Get {
        id: StoreId,
        key: String,
    },
    Set {
        id: StoreId,
        key: String,
        value: J,
        user_ids: Vec<i64>,
        metadata: serde_json::Map<String, J>,
    },
    Increment {
        id: StoreId,
        key: String,
        delta: i64,
        user_ids: Vec<i64>,
        metadata: serde_json::Map<String, J>,
    },
    Update {
        id: StoreId,
        key: String,
        transform: RegistryKey,
    },
    Remove {
        id: StoreId,
        key: String,
    },
    ListKeys {
        id: StoreId,
        prefix: String,
        page: usize,
    },
    ListStores {
        prefix: String,
        page: usize,
    },
    Sorted {
        id: StoreId,
        ascending: bool,
        page: usize,
        min: Option<i64>,
        max: Option<i64>,
    },
    Version {
        id: StoreId,
        key: String,
        version: String,
    },
    Versions {
        id: StoreId,
        key: String,
    },
}

pub(super) fn register(b: &mut Bindings) {
    let plain: &[(&str, &str, super::Imp)] = &[
        ("DataStoreService", "GetDataStore", get_data_store),
        (
            "DataStoreService",
            "GetGlobalDataStore",
            get_global_data_store,
        ),
        (
            "DataStoreService",
            "GetOrderedDataStore",
            get_ordered_data_store,
        ),
        (
            "DataStoreService",
            "GetRequestBudgetForRequestType",
            get_budget,
        ),
        ("DataStoreSetOptions", "SetMetadata", set_metadata),
        ("DataStoreSetOptions", "GetMetadata", get_metadata),
    ];
    for (class, name, f) in plain {
        add(b, class, name, *f, false);
    }
    let yielding: &[(&str, &str, super::Imp)] = &[
        ("DataStoreService", "ListDataStoresAsync", list_data_stores),
        ("GlobalDataStore", "GetAsync", get_async),
        ("GlobalDataStore", "SetAsync", set_async),
        ("GlobalDataStore", "IncrementAsync", increment_async),
        ("GlobalDataStore", "UpdateAsync", update_async),
        ("GlobalDataStore", "RemoveAsync", remove_async),
        ("DataStore", "ListKeysAsync", list_keys),
        ("DataStore", "GetVersionAsync", get_version),
        ("DataStore", "ListVersionsAsync", list_versions),
        ("OrderedDataStore", "GetSortedAsync", get_sorted),
    ];
    for (class, name, f) in yielding {
        add(b, class, name, *f, true);
    }
}

fn check_name(num: u16, what: &str, s: &str) -> Result<()> {
    if s.is_empty() {
        return Err(bad(101, format!("{what} is empty")));
    }
    let n = s.chars().count();
    if n > MAX_NAME {
        return Err(bad(
            num,
            format!("{what} is {n} characters long; the limit is {MAX_NAME}"),
        ));
    }
    Ok(())
}

fn store_handle(lua: &Lua, a: MultiValue, what: &str, kind: StoreKind) -> Result<MultiValue> {
    let (name, scope, _options): (String, Option<String>, LuaValue) = args(lua, a, what)?;
    usable(lua, &format!("DataStoreService:{what}"))?;
    check_name(106, "the data store name", &name)?;
    let scope = scope.unwrap_or_else(|| DEFAULT_SCOPE.to_string());
    check_name(106, "the scope", &scope)?;
    let class = match kind {
        StoreKind::Standard => "DataStore",
        StoreKind::Ordered => "OrderedDataStore",
    };
    ret(
        lua,
        handle_instance(
            lua,
            class,
            &name,
            Handle::Store((kind, name.clone(), scope)),
        )?,
    )
}

fn get_data_store(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    store_handle(lua, a, "GetDataStore", StoreKind::Standard)
}

fn get_ordered_data_store(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    store_handle(lua, a, "GetOrderedDataStore", StoreKind::Ordered)
}

fn get_global_data_store(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    usable(lua, "DataStoreService:GetGlobalDataStore")?;
    // The global store has no name of its own; it is kept as the store with
    // an empty name.
    let id = (
        StoreKind::Standard,
        String::new(),
        DEFAULT_SCOPE.to_string(),
    );
    ret(
        lua,
        handle_instance(lua, "GlobalDataStore", "GlobalDataStore", Handle::Store(id))?,
    )
}

fn get_budget(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let ty: LuaValue = args(lua, a, "GetRequestBudgetForRequestType")?;
    let item = EnumItem::coerce(&ty, "DataStoreRequestType")?;
    usable(lua, "DataStoreService:GetRequestBudgetForRequestType")?;
    let (now, players) = (sched::now(lua)?, players(lua)?);
    let mut d = data(lua);
    let mut avail = |k: Kind| d.budgets.available(k, now, players);
    let n = match item.name() {
        "GetAsync" | "GetVersionAsync" | "StandardRead" => avail(Kind::StandardRead),
        "SetIncrementAsync" | "StandardWrite" => avail(Kind::StandardWrite),
        "UpdateAsync" => avail(Kind::StandardRead).min(avail(Kind::StandardWrite)),
        "GetSortedAsync" | "OrderedList" => avail(Kind::OrderedList),
        "SetIncrementSortedAsync" | "OrderedWrite" => avail(Kind::OrderedWrite),
        "ListAsync" | "StandardList" => avail(Kind::StandardList),
        "RemoveVersionAsync" | "StandardRemove" => avail(Kind::StandardRemove),
        "OrderedRead" => avail(Kind::OrderedRead),
        "OrderedRemove" => avail(Kind::OrderedRemove),
        // OnUpdate is deprecated and has no budget here.
        _ => 0.0,
    };
    drop(d);
    ret(lua, n)
}

fn options_entity(v: &LuaValue) -> Option<Entity> {
    match v {
        LuaValue::Nil => None,
        v => crate::instance::arg_instance(v, "options").ok(),
    }
}

fn set_metadata(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let t: Table = args(lua, a, "SetMetadata")?;
    let j = to_stored(&LuaValue::Table(t)).map_err(|e| bad(511, e.0))?;
    let J::Object(map) = j else {
        return Err(bad(511, "metadata must be a table with string keys"));
    };
    check_metadata(&map)?;
    data(lua).set_options.insert(e, map);
    ret(lua, ())
}

fn get_metadata(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let map = data(lua).set_options.get(&e).cloned().unwrap_or_default();
    ret(lua, value::to_lua(lua, &J::Object(map))?)
}

fn check_metadata(map: &serde_json::Map<String, J>) -> Result<()> {
    let mut total = 0;
    for (k, v) in map {
        let kl = k.chars().count();
        if kl > MAX_NAME {
            return Err(bad(
                511,
                format!("the metadata key {k:?} is {kl} characters long; the limit is {MAX_NAME}"),
            ));
        }
        let vl = match v {
            J::String(s) => s.chars().count(),
            other => other.to_string().chars().count(),
        };
        if vl > MAX_META_VALUE {
            return Err(bad(
                511,
                format!(
                    "the metadata value of {k:?} is {vl} characters; the limit is {MAX_META_VALUE}"
                ),
            ));
        }
        total += kl + vl;
    }
    if total > MAX_META_TOTAL {
        return Err(bad(
            511,
            format!("the metadata is {total} characters in all; the limit is {MAX_META_TOTAL}"),
        ));
    }
    Ok(())
}

fn user_ids(v: &LuaValue) -> Result<Vec<i64>> {
    match v {
        LuaValue::Nil => Ok(Vec::new()),
        LuaValue::Table(t) => {
            let mut out = Vec::new();
            for i in 1..=t.raw_len() {
                let x: LuaValue = t.raw_get(i)?;
                match x {
                    LuaValue::Integer(n) => out.push(re_script::lua_int(n)),
                    LuaValue::Number(n) if n.fract() == 0.0 => out.push(n as i64),
                    _ => return Err(bad(513, "user ids must be whole numbers")),
                }
            }
            if out.len() < t.raw_len()
                || out.len() != t.clone().pairs::<LuaValue, LuaValue>().count()
            {
                return Err(bad(513, "user ids must be an array of numbers"));
            }
            if out.len() > 4 {
                return Err(bad(512, "at most 4 user ids may be given with a key"));
            }
            Ok(out)
        }
        _ => Err(bad(513, "user ids must be an array of numbers")),
    }
}

fn store_of(lua: &Lua, e: Entity, what: &str) -> Result<StoreId> {
    match handle_of(lua, e, what)? {
        Handle::Store(id) => Ok(id),
        Handle::Memory(..) => Err(bad(106, format!("{what}: not a data store"))),
    }
}

fn key_arg(key: &LuaValue) -> Result<String> {
    let key = match key {
        LuaValue::String(s) => s.to_str()?.to_string(),
        LuaValue::Integer(n) => n.to_string(),
        LuaValue::Number(n) => n.to_string(),
        LuaValue::Nil => return Err(bad(101, "the key name is empty")),
        other => {
            return Err(bad(
                106,
                format!("the key must be a string, not a {}", other.type_name()),
            ));
        }
    };
    check_name(102, "the key name", &key)?;
    Ok(key)
}

fn reserve(lua: &Lua, kinds: &[Kind], num: u16, what: &str) -> Result<f64> {
    let (now, players) = (sched::now(lua)?, players(lua)?);
    data(lua)
        .budgets
        .reserve(kinds, now, players)
        .map_err(|_| queue_full(num, what))
}

fn read_kind(id: &StoreId) -> Kind {
    match id.0 {
        StoreKind::Standard => Kind::StandardRead,
        StoreKind::Ordered => Kind::OrderedRead,
    }
}

fn write_kind(id: &StoreId) -> Kind {
    match id.0 {
        StoreKind::Standard => Kind::StandardWrite,
        StoreKind::Ordered => Kind::OrderedWrite,
    }
}

fn remove_kind(id: &StoreId) -> Kind {
    match id.0 {
        StoreKind::Standard => Kind::StandardRemove,
        StoreKind::Ordered => Kind::OrderedRemove,
    }
}

/// Checks a value for a write to store `id`.
fn storable(id: &StoreId, v: &LuaValue) -> Result<J> {
    let j = to_stored(v).map_err(|e| bad(104, e.0))?;
    if id.0 == StoreKind::Ordered && as_integer(&j).is_none() {
        return Err(bad(104, "an ordered data store holds whole numbers only"));
    }
    let n = stored_len(&j);
    if n > MAX_VALUE {
        return Err(bad(
            105,
            format!("the value is {n} characters once serialized; the limit is {MAX_VALUE}"),
        ));
    }
    Ok(j)
}

fn label(lua: &Lua, e: Entity, method: &str) -> Result<String> {
    let name = super::with_world(lua, |w| crate::instance::tree::name(w, e))?;
    Ok(format!("{name}:{method}"))
}

fn get_async(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (key, _options): (LuaValue, LuaValue) = args(lua, a, "GetAsync")?;
    usable(lua, "GetAsync")?;
    let id = store_of(lua, e, "GetAsync")?;
    let key = key_arg(&key)?;
    let wait = reserve(lua, &[read_kind(&id)], 301, "GetAsync")?;
    submit(
        lua,
        &label(lua, e, "GetAsync")?,
        AnyOp::Ds(Op::Get { id, key }),
        wait,
    )
}

fn set_async(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (key, v, ids, options): (LuaValue, LuaValue, LuaValue, LuaValue) =
        args(lua, a, "SetAsync")?;
    usable(lua, "SetAsync")?;
    let id = store_of(lua, e, "SetAsync")?;
    let key = key_arg(&key)?;
    let value = storable(&id, &v)?;
    let user_ids = user_ids(&ids)?;
    let metadata = options_entity(&options)
        .and_then(|o| data(lua).set_options.get(&o).cloned())
        .unwrap_or_default();
    let wait = reserve(lua, &[write_kind(&id)], 302, "SetAsync")?;
    let op = Op::Set {
        id,
        key,
        value,
        user_ids,
        metadata,
    };
    submit(lua, &label(lua, e, "SetAsync")?, AnyOp::Ds(op), wait)
}

fn increment_async(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (key, delta, ids, options): (LuaValue, Option<f64>, LuaValue, LuaValue) =
        args(lua, a, "IncrementAsync")?;
    usable(lua, "IncrementAsync")?;
    let id = store_of(lua, e, "IncrementAsync")?;
    let key = key_arg(&key)?;
    let delta = delta.unwrap_or(1.0);
    if delta.fract() != 0.0 || !delta.is_finite() {
        return Err(bad(
            106,
            format!("the increment {delta} is not a whole number"),
        ));
    }
    let user_ids = user_ids(&ids)?;
    let metadata = options_entity(&options)
        .and_then(|o| data(lua).set_options.get(&o).cloned())
        .unwrap_or_default();
    let wait = reserve(lua, &[write_kind(&id)], 303, "IncrementAsync")?;
    let op = Op::Increment {
        id,
        key,
        delta: delta as i64,
        user_ids,
        metadata,
    };
    submit(lua, &label(lua, e, "IncrementAsync")?, AnyOp::Ds(op), wait)
}

fn update_async(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (key, f): (LuaValue, Function) = args(lua, a, "UpdateAsync")?;
    usable(lua, "UpdateAsync")?;
    let id = store_of(lua, e, "UpdateAsync")?;
    let key = key_arg(&key)?;
    let wait = reserve(lua, &[read_kind(&id), write_kind(&id)], 304, "UpdateAsync")?;
    let transform = lua.create_registry_value(f)?;
    let op = Op::Update { id, key, transform };
    submit(lua, &label(lua, e, "UpdateAsync")?, AnyOp::Ds(op), wait)
}

fn remove_async(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let key: LuaValue = args(lua, a, "RemoveAsync")?;
    usable(lua, "RemoveAsync")?;
    let id = store_of(lua, e, "RemoveAsync")?;
    let key = key_arg(&key)?;
    let wait = reserve(lua, &[remove_kind(&id)], 306, "RemoveAsync")?;
    submit(
        lua,
        &label(lua, e, "RemoveAsync")?,
        AnyOp::Ds(Op::Remove { id, key }),
        wait,
    )
}

fn page_size(v: Option<f64>, default: usize) -> Result<usize> {
    match v {
        None => Ok(default),
        Some(n) if n.fract() == 0.0 && (1.0..=MAX_PAGE as f64).contains(&n) => Ok(n as usize),
        Some(n) => Err(bad(
            106,
            format!("the page size {n} must be a whole number from 1 to {MAX_PAGE}"),
        )),
    }
}

fn list_keys(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (prefix, size, _cursor, _exclude): (Option<String>, Option<f64>, LuaValue, LuaValue) =
        args(lua, a, "ListKeysAsync")?;
    usable(lua, "ListKeysAsync")?;
    let id = store_of(lua, e, "ListKeysAsync")?;
    let page = page_size(size, 50)?;
    let wait = reserve(lua, &[Kind::StandardList], 305, "ListKeysAsync")?;
    let op = Op::ListKeys {
        id,
        prefix: prefix.unwrap_or_default(),
        page,
    };
    submit(lua, &label(lua, e, "ListKeysAsync")?, AnyOp::Ds(op), wait)
}

fn list_data_stores(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let (prefix, size, _cursor): (Option<String>, Option<f64>, LuaValue) =
        args(lua, a, "ListDataStoresAsync")?;
    usable(lua, "ListDataStoresAsync")?;
    let page = page_size(size, 32)?;
    let wait = reserve(lua, &[Kind::StandardList], 305, "ListDataStoresAsync")?;
    let op = Op::ListStores {
        prefix: prefix.unwrap_or_default(),
        page,
    };
    submit(
        lua,
        "DataStoreService:ListDataStoresAsync",
        AnyOp::Ds(op),
        wait,
    )
}

fn get_sorted(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (ascending, size, min, max): (bool, f64, Option<f64>, Option<f64>) =
        args(lua, a, "GetSortedAsync")?;
    usable(lua, "GetSortedAsync")?;
    let id = store_of(lua, e, "GetSortedAsync")?;
    let page = page_size(Some(size), 50)?;
    let whole = |v: Option<f64>| -> Result<Option<i64>> {
        match v {
            None => Ok(None),
            Some(n) if n.fract() == 0.0 && n.is_finite() => Ok(Some(n as i64)),
            Some(n) => Err(bad(106, format!("the bound {n} is not a whole number"))),
        }
    };
    let (min, max) = (whole(min)?, whole(max)?);
    if let (Some(lo), Some(hi)) = (min, max)
        && hi < lo
    {
        return Err(bad(
            107,
            format!("MaxValue {hi} is less than MinValue {lo}"),
        ));
    }
    let wait = reserve(lua, &[Kind::OrderedList], 305, "GetSortedAsync")?;
    let op = Op::Sorted {
        id,
        ascending,
        page,
        min,
        max,
    };
    submit(lua, &label(lua, e, "GetSortedAsync")?, AnyOp::Ds(op), wait)
}

fn get_version(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (key, version): (LuaValue, String) = args(lua, a, "GetVersionAsync")?;
    usable(lua, "GetVersionAsync")?;
    let id = store_of(lua, e, "GetVersionAsync")?;
    let key = key_arg(&key)?;
    let wait = reserve(lua, &[Kind::StandardRead], 301, "GetVersionAsync")?;
    let op = Op::Version { id, key, version };
    submit(lua, &label(lua, e, "GetVersionAsync")?, AnyOp::Ds(op), wait)
}

fn list_versions(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (key, _rest): (LuaValue, MultiValue) = args(lua, a, "ListVersionsAsync")?;
    usable(lua, "ListVersionsAsync")?;
    let id = store_of(lua, e, "ListVersionsAsync")?;
    let key = key_arg(&key)?;
    let wait = reserve(lua, &[Kind::StandardList], 305, "ListVersionsAsync")?;
    submit(
        lua,
        &label(lua, e, "ListVersionsAsync")?,
        AnyOp::Ds(Op::Versions { id, key }),
        wait,
    )
}

/// A `DataStoreKeyInfo`: `Version`, `CreatedTime`, `UpdatedTime`,
/// `GetUserIds()` and `GetMetadata()`.
fn key_info(lua: &Lua, r: &super::store::Record) -> Result<LuaValue> {
    let t = lua.create_table()?;
    t.raw_set("Version", r.version.to_string())?;
    t.raw_set("CreatedTime", r.created)?;
    t.raw_set("UpdatedTime", r.updated)?;
    let ids = r.user_ids.clone();
    t.raw_set(
        "GetUserIds",
        lua.create_function(move |lua, _this: LuaValue| {
            let out = lua.create_table()?;
            for (i, id) in ids.iter().enumerate() {
                out.raw_set(i + 1, *id)?;
            }
            Ok(out)
        })?,
    )?;
    let meta = J::Object(r.metadata.clone());
    t.raw_set(
        "GetMetadata",
        lua.create_function(move |lua, _this: LuaValue| value::to_lua(lua, &meta))?,
    )?;
    Ok(LuaValue::Table(t))
}

/// Reads store `id`, warning once if its file did not read.
fn with_store<T>(
    lua: &Lua,
    id: &StoreId,
    f: impl FnOnce(&mut super::store::Store) -> T,
) -> Result<T> {
    let (out, problem) = {
        let mut d = data(lua);
        let super::Data {
            stores, settings, ..
        } = &mut *d;
        let (s, problem) = stores.get(settings.backend.as_mut(), id);
        (f(s), problem)
    };
    if let Some(p) = problem {
        warn_unreadable(lua, &p.path, &p.reason)?;
    }
    Ok(out)
}

fn save(lua: &Lua, id: &StoreId) -> Result<()> {
    let r = {
        let mut d = data(lua);
        let super::Data {
            stores, settings, ..
        } = &mut *d;
        stores.save(settings.backend.as_mut(), id)
    };
    saved(lua, r, &super::store::path(id))
}

fn values(lua: &Lua, v: impl mlua::IntoLuaMulti) -> Result<Done> {
    Ok(Done::Values(v.into_lua_multi(lua)?))
}

pub(super) fn complete(lua: &Lua, op: &Op) -> Result<Done> {
    let now = now_ms(lua)?;
    match op {
        Op::Get { id, key } => {
            let r = with_store(lua, id, |s| s.keys.get(key).cloned())?;
            match r {
                Some(r) => values(lua, (value::to_lua(lua, &r.value)?, key_info(lua, &r)?)),
                None => values(lua, (LuaValue::Nil, LuaValue::Nil)),
            }
        }
        Op::Set {
            id,
            key,
            value,
            user_ids,
            metadata,
        } => {
            let r = with_store(lua, id, |s| {
                s.write(key, value.clone(), user_ids.clone(), metadata.clone(), now)
            })?;
            save(lua, id)?;
            values(lua, r.version.to_string())
        }
        Op::Increment {
            id,
            key,
            delta,
            user_ids,
            metadata,
        } => {
            let old = with_store(lua, id, |s| s.keys.get(key).map(|r| r.value.clone()))?;
            let base = match &old {
                None => 0,
                Some(v) => as_integer(v).ok_or_else(|| {
                    bad(
                        103,
                        format!("IncrementAsync: the value of {key:?} is not a whole number"),
                    )
                })?,
            };
            let n = base
                .checked_add(*delta)
                .ok_or_else(|| bad(103, format!("IncrementAsync: {base} + {delta} overflows")))?;
            let r = with_store(lua, id, |s| {
                s.write(key, J::from(n), user_ids.clone(), metadata.clone(), now)
            })?;
            save(lua, id)?;
            values(lua, (n, key_info(lua, &r)?))
        }
        Op::Update { id, key, transform } => {
            let f: Function = lua.registry_value(transform)?;
            let old = with_store(lua, id, |s| s.keys.get(key).cloned())?;
            let (cur, info) = match &old {
                Some(r) => (value::to_lua(lua, &r.value)?, key_info(lua, r)?),
                None => (LuaValue::Nil, LuaValue::Nil),
            };
            // The transform runs with no store borrowed, so it may read
            // other stores' cached handles; it may not yield.
            let out: MultiValue = f.call((cur, info)).map_err(|e| {
                bad(
                    103,
                    format!("UpdateAsync: the transform function failed: {}", plain(&e)),
                )
            })?;
            let mut it = out.into_iter();
            let new = it.next().unwrap_or(LuaValue::Nil);
            if new.is_nil() {
                // The transform cancelled the update.
                return values(lua, LuaValue::Nil);
            }
            let value = storable(id, &new).map_err(|e| {
                bad(
                    103,
                    format!(
                        "UpdateAsync: the transform returned a value that {}",
                        plain(&e)
                    ),
                )
            })?;
            let ids = user_ids(&it.next().unwrap_or(LuaValue::Nil))?;
            let metadata = match it.next().unwrap_or(LuaValue::Nil) {
                LuaValue::Nil => old.as_ref().map(|r| r.metadata.clone()).unwrap_or_default(),
                v => match to_stored(&v).map_err(|e| bad(511, e.0))? {
                    J::Object(m) => {
                        check_metadata(&m)?;
                        m
                    }
                    _ => return Err(bad(511, "metadata must be a table with string keys")),
                },
            };
            let r = with_store(lua, id, |s| s.write(key, value.clone(), ids, metadata, now))?;
            save(lua, id)?;
            values(lua, (value::to_lua(lua, &r.value)?, key_info(lua, &r)?))
        }
        Op::Remove { id, key } => {
            let old = with_store(lua, id, |s| {
                let r = s.keys.remove(key);
                if r.is_some() {
                    s.updated = now;
                }
                r
            })?;
            match old {
                Some(r) => {
                    save(lua, id)?;
                    values(lua, (value::to_lua(lua, &r.value)?, key_info(lua, &r)?))
                }
                None => values(lua, (LuaValue::Nil, LuaValue::Nil)),
            }
        }
        Op::ListKeys { id, prefix, page } => {
            let keys = with_store(lua, id, |s| {
                s.keys
                    .keys()
                    .filter(|k| k.starts_with(prefix.as_str()))
                    .map(|k| serde_json::json!({ "KeyName": k }))
                    .collect::<Vec<J>>()
            })?;
            Ok(Done::Values(
                pages_table(lua, paginate(keys, *page), Kind::StandardList)?.into_lua_multi(lua)?,
            ))
        }
        Op::ListStores { prefix, page } => {
            let names: Vec<String> = {
                let d = data(lua);
                d.settings
                    .backend
                    .list(StoreKind::Standard.dir())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|c| super::decode(c))
                    .filter(|n| !n.is_empty() && n.starts_with(prefix.as_str()))
                    .collect()
            };
            let mut items = Vec::new();
            for n in names {
                let id = (StoreKind::Standard, n.clone(), DEFAULT_SCOPE.to_string());
                let (c, u) = with_store(lua, &id, |s| (s.created, s.updated))?;
                items.push(serde_json::json!({
                    "DataStoreName": n, "CreatedTime": c, "UpdatedTime": u,
                }));
            }
            Ok(Done::Values(
                pages_table(lua, paginate(items, *page), Kind::StandardList)?
                    .into_lua_multi(lua)?,
            ))
        }
        Op::Sorted {
            id,
            ascending,
            page,
            min,
            max,
        } => {
            let mut items: Vec<(i64, String)> = with_store(lua, id, |s| {
                s.keys
                    .iter()
                    .filter_map(|(k, r)| as_integer(&r.value).map(|v| (v, k.clone())))
                    .filter(|(v, _)| min.is_none_or(|lo| *v >= lo) && max.is_none_or(|hi| *v <= hi))
                    .collect()
            })?;
            items.sort();
            if !ascending {
                items.reverse();
            }
            let items = items
                .into_iter()
                .map(|(v, k)| serde_json::json!({ "key": k, "value": v }))
                .collect();
            Ok(Done::Values(
                pages_table(lua, paginate(items, *page), Kind::OrderedList)?.into_lua_multi(lua)?,
            ))
        }
        Op::Version { id, key, version } => {
            let r = with_store(lua, id, |s| s.keys.get(key).cloned())?;
            match r {
                Some(r) if r.version.to_string() == *version => {
                    values(lua, (value::to_lua(lua, &r.value)?, key_info(lua, &r)?))
                }
                _ => values(lua, (LuaValue::Nil, LuaValue::Nil)),
            }
        }
        Op::Versions { id, key } => {
            let r = with_store(lua, id, |s| s.keys.get(key).cloned())?;
            let items = r
                .map(|r| {
                    vec![serde_json::json!({
                        "Version": r.version.to_string(),
                        "CreatedTime": r.updated,
                        "IsDeleted": false,
                    })]
                })
                .unwrap_or_default();
            Ok(Done::Values(
                pages_table(lua, paginate(items, MAX_PAGE), Kind::StandardList)?
                    .into_lua_multi(lua)?,
            ))
        }
    }
}

/// An error's message without mlua's traceback.
pub(super) fn plain(e: &mlua::Error) -> String {
    match e {
        mlua::Error::RuntimeError(m) => m.clone(),
        mlua::Error::CallbackError { cause, .. } => plain(cause),
        other => other.to_string(),
    }
}

use mlua::IntoLuaMulti;
