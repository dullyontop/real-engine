//! `MemoryStoreService`, `MemoryStoreQueue`, `MemoryStoreSortedMap` and
//! `MemoryStoreHashMap`.

use mlua::{Function, IntoLuaMulti, Lua, MultiValue, RegistryKey, Result, Value as LuaValue};
use re_scene::Entity;
use re_script::sched;
use serde_json::Value as J;

use super::ds::plain;
use super::memory::{
    self, Item, Kind, MAX_EXPIRATION_SECS, MAX_KEY_BYTES, MAX_VALUE_BYTES, QueueItem, Refusal,
    SortKey,
};
use super::value::{self, to_stored};
use super::{
    Done, Handle, Op as AnyOp, add, data, handle_instance, handle_of, now_ms, pages_table,
    paginate, players, saved, submit, usable, warn_unreadable,
};
use crate::codes;
use crate::enums::EnumItem;
use crate::instance::methods_api::{args, ret};
use crate::runtime::{Bindings, lua_error};

/// How long a queue read holds its items when `GetQueue` names no
/// invisibility timeout, in seconds.
const DEFAULT_INVISIBILITY: f64 = 30.0;
/// The most items one read or range call returns (our limit).
const MAX_COUNT: usize = 100;
/// How often a waiting queue read looks again, in seconds; each look costs a
/// request unit.
const READ_POLL: f64 = 2.0;

/// A memory store request, kept until its runner completes it.
pub(super) enum Op {
    Add {
        name: String,
        value: J,
        expires_in: f64,
        priority: f64,
    },
    Read {
        name: String,
        count: usize,
        all_or_nothing: bool,
        wait: f64,
        hold: f64,
    },
    Remove {
        name: String,
        id: String,
    },
    QueueSize {
        name: String,
        exclude_invisible: bool,
    },
    Get {
        kind: Kind,
        name: String,
        key: String,
    },
    Set {
        kind: Kind,
        name: String,
        key: String,
        value: J,
        expires_in: f64,
        sort_key: Option<SortKey>,
    },
    Update {
        kind: Kind,
        name: String,
        key: String,
        transform: RegistryKey,
        expires_in: f64,
    },
    Delete {
        kind: Kind,
        name: String,
        key: String,
    },
    Range {
        name: String,
        ascending: bool,
        count: usize,
        lower: Option<(String, Option<SortKey>)>,
        upper: Option<(String, Option<SortKey>)>,
    },
    MapSize {
        kind: Kind,
        name: String,
    },
    List {
        name: String,
        page: usize,
    },
}

pub(super) fn register(b: &mut Bindings) {
    let plain_methods: &[(&str, &str, super::Imp)] = &[
        ("MemoryStoreService", "GetQueue", get_queue),
        ("MemoryStoreService", "GetSortedMap", get_sorted_map),
        ("MemoryStoreService", "GetHashMap", get_hash_map),
    ];
    for (class, name, f) in plain_methods {
        add(b, class, name, *f, false);
    }
    let yielding: &[(&str, &str, super::Imp)] = &[
        ("MemoryStoreQueue", "AddAsync", queue_add),
        ("MemoryStoreQueue", "ReadAsync", queue_read),
        ("MemoryStoreQueue", "RemoveAsync", queue_remove),
        ("MemoryStoreQueue", "GetSizeAsync", queue_size),
        ("MemoryStoreSortedMap", "GetAsync", map_get),
        ("MemoryStoreSortedMap", "SetAsync", map_set),
        ("MemoryStoreSortedMap", "UpdateAsync", map_update),
        ("MemoryStoreSortedMap", "RemoveAsync", map_remove),
        ("MemoryStoreSortedMap", "GetRangeAsync", map_range),
        ("MemoryStoreSortedMap", "GetSizeAsync", map_size),
        ("MemoryStoreHashMap", "GetAsync", map_get),
        ("MemoryStoreHashMap", "SetAsync", map_set),
        ("MemoryStoreHashMap", "UpdateAsync", map_update),
        ("MemoryStoreHashMap", "RemoveAsync", map_remove),
        ("MemoryStoreHashMap", "ListItemsAsync", map_list),
    ];
    for (class, name, f) in yielding {
        add(b, class, name, *f, true);
    }
}

/// An error whose message starts with the vendor's error name (`RE9403`).
fn refused(r: Refusal, message: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(format!(
        "{}: {message} [{}]",
        r.name(),
        codes::MEMORY_REQUEST
    ))
}

/// A bad argument to a memory store call (`RE9403`).
fn invalid(message: impl std::fmt::Display) -> mlua::Error {
    lua_error(codes::MEMORY_REQUEST, message)
}

fn structure(lua: &Lua, a: MultiValue, what: &str, kind: Kind, class: &str) -> Result<MultiValue> {
    let (name, timeout): (String, Option<f64>) = args(lua, a, what)?;
    usable(lua, &format!("MemoryStoreService:{what}"))?;
    if name.is_empty() || name.len() > MAX_KEY_BYTES {
        return Err(invalid(format!(
            "{what}: the name must be 1 to {MAX_KEY_BYTES} bytes long"
        )));
    }
    let hold = timeout.unwrap_or(DEFAULT_INVISIBILITY);
    if !(hold.is_finite() && hold >= 0.0) {
        return Err(invalid(format!(
            "{what}: the invisibility timeout must be 0 or more"
        )));
    }
    let hold = if kind == Kind::Queue { hold } else { 0.0 };
    ret(
        lua,
        handle_instance(lua, class, &name, Handle::Memory(kind, name.clone(), hold))?,
    )
}

fn get_queue(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    structure(lua, a, "GetQueue", Kind::Queue, "MemoryStoreQueue")
}

fn get_sorted_map(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    structure(
        lua,
        a,
        "GetSortedMap",
        Kind::SortedMap,
        "MemoryStoreSortedMap",
    )
}

fn get_hash_map(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    structure(lua, a, "GetHashMap", Kind::HashMap, "MemoryStoreHashMap")
}

fn of(lua: &Lua, e: Entity, what: &str) -> Result<(Kind, String, f64)> {
    match handle_of(lua, e, what)? {
        Handle::Memory(k, n, h) => Ok((k, n, h)),
        Handle::Store(_) => Err(invalid(format!("{what}: not a memory store structure"))),
    }
}

fn charge(lua: &Lua, units: f64) -> Result<()> {
    let (now, players) = (sched::now(lua)?, players(lua)?);
    data(lua)
        .memory
        .charge(units, now, players)
        .map_err(|r| refused(r, "the place's request units for this minute are spent"))
}

fn expiration(v: Option<f64>) -> Result<f64> {
    let secs = v.unwrap_or(MAX_EXPIRATION_SECS);
    if !secs.is_finite() || !(0.0..=MAX_EXPIRATION_SECS).contains(&secs) {
        return Err(invalid(format!(
            "the expiration {secs} must be from 0 to {MAX_EXPIRATION_SECS} seconds"
        )));
    }
    Ok(secs)
}

fn item_value(v: &LuaValue) -> Result<J> {
    let j = to_stored(v).map_err(|e| invalid(format!("the value cannot be stored: {}", e.0)))?;
    let n = j.to_string().len();
    if n > MAX_VALUE_BYTES {
        return Err(refused(
            Refusal::ItemValueSizeTooLarge,
            format!("the value is {n} bytes once serialized; the limit is {MAX_VALUE_BYTES}"),
        ));
    }
    Ok(j)
}

fn key_arg(v: &LuaValue) -> Result<String> {
    let k = match v {
        LuaValue::String(s) => s.to_str()?.to_string(),
        LuaValue::Integer(n) => n.to_string(),
        LuaValue::Number(n) => n.to_string(),
        other => {
            return Err(invalid(format!(
                "the key must be a string, not a {}",
                other.type_name()
            )));
        }
    };
    if k.is_empty() || k.len() > MAX_KEY_BYTES {
        return Err(invalid(format!(
            "the key must be 1 to {MAX_KEY_BYTES} bytes long"
        )));
    }
    Ok(k)
}

fn sort_key(v: &LuaValue) -> Result<Option<SortKey>> {
    match v {
        LuaValue::Nil => Ok(None),
        LuaValue::Integer(n) => Ok(Some(SortKey::Num(*n as f64))),
        LuaValue::Number(n) if n.is_finite() => Ok(Some(SortKey::Num(*n))),
        LuaValue::String(s) => Ok(Some(SortKey::Str(s.to_str()?.to_string()))),
        other => Err(invalid(format!(
            "a sort key must be a number or a string, not a {}",
            other.type_name()
        ))),
    }
}

fn sort_key_lua(lua: &Lua, s: &Option<SortKey>) -> Result<LuaValue> {
    Ok(match s {
        None => LuaValue::Nil,
        Some(SortKey::Num(n)) => LuaValue::Number(*n),
        Some(SortKey::Str(s)) => LuaValue::String(lua.create_string(s)?),
    })
}

fn label(lua: &Lua, e: Entity, method: &str) -> Result<String> {
    let name = super::with_world(lua, |w| crate::instance::tree::name(w, e))?;
    Ok(format!("{name}:{method}"))
}

fn queue_add(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (v, exp, priority): (LuaValue, Option<f64>, Option<f64>) = args(lua, a, "AddAsync")?;
    usable(lua, "AddAsync")?;
    let (_, name, _) = of(lua, e, "AddAsync")?;
    let value = item_value(&v)?;
    let expires_in = expiration(exp)?;
    let priority = priority.unwrap_or(0.0);
    if !priority.is_finite() {
        return Err(invalid("the priority must be a finite number"));
    }
    charge(lua, 1.0)?;
    let op = Op::Add {
        name,
        value,
        expires_in,
        priority,
    };
    submit(lua, &label(lua, e, "AddAsync")?, AnyOp::Ms(op), 0.0)
}

fn count_arg(n: f64, what: &str) -> Result<usize> {
    if n.fract() != 0.0 || !(1.0..=MAX_COUNT as f64).contains(&n) {
        return Err(invalid(format!(
            "{what}: the count {n} must be a whole number from 1 to {MAX_COUNT}"
        )));
    }
    Ok(n as usize)
}

fn queue_read(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (count, all_or_nothing, wait): (f64, Option<bool>, Option<f64>) =
        args(lua, a, "ReadAsync")?;
    usable(lua, "ReadAsync")?;
    let (_, name, hold) = of(lua, e, "ReadAsync")?;
    let count = count_arg(count, "ReadAsync")?;
    let wait = wait.unwrap_or(-1.0);
    charge(lua, 1.0)?;
    let op = Op::Read {
        name,
        count,
        all_or_nothing: all_or_nothing.unwrap_or(false),
        wait: if wait < 0.0 { f64::INFINITY } else { wait },
        hold,
    };
    submit(lua, &label(lua, e, "ReadAsync")?, AnyOp::Ms(op), 0.0)
}

fn queue_remove(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let id: String = args(lua, a, "RemoveAsync")?;
    usable(lua, "RemoveAsync")?;
    let (_, name, _) = of(lua, e, "RemoveAsync")?;
    charge(lua, 1.0)?;
    submit(
        lua,
        &label(lua, e, "RemoveAsync")?,
        AnyOp::Ms(Op::Remove { name, id }),
        0.0,
    )
}

fn queue_size(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let exclude: Option<bool> = args(lua, a, "GetSizeAsync")?;
    usable(lua, "GetSizeAsync")?;
    let (_, name, _) = of(lua, e, "GetSizeAsync")?;
    charge(lua, 1.0)?;
    let op = Op::QueueSize {
        name,
        exclude_invisible: exclude.unwrap_or(false),
    };
    submit(lua, &label(lua, e, "GetSizeAsync")?, AnyOp::Ms(op), 0.0)
}

fn map_get(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let key: LuaValue = args(lua, a, "GetAsync")?;
    usable(lua, "GetAsync")?;
    let (kind, name, _) = of(lua, e, "GetAsync")?;
    let key = key_arg(&key)?;
    charge(lua, 1.0)?;
    submit(
        lua,
        &label(lua, e, "GetAsync")?,
        AnyOp::Ms(Op::Get { kind, name, key }),
        0.0,
    )
}

fn map_set(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (key, v, exp, sk): (LuaValue, LuaValue, Option<f64>, LuaValue) = args(lua, a, "SetAsync")?;
    usable(lua, "SetAsync")?;
    let (kind, name, _) = of(lua, e, "SetAsync")?;
    let key = key_arg(&key)?;
    let value = item_value(&v)?;
    let expires_in = expiration(exp)?;
    let sort_key = if kind == Kind::SortedMap {
        sort_key(&sk)?
    } else {
        None
    };
    charge(lua, 1.0)?;
    let op = Op::Set {
        kind,
        name,
        key,
        value,
        expires_in,
        sort_key,
    };
    submit(lua, &label(lua, e, "SetAsync")?, AnyOp::Ms(op), 0.0)
}

fn map_update(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (key, f, exp): (LuaValue, Function, Option<f64>) = args(lua, a, "UpdateAsync")?;
    usable(lua, "UpdateAsync")?;
    let (kind, name, _) = of(lua, e, "UpdateAsync")?;
    let key = key_arg(&key)?;
    let expires_in = expiration(exp)?;
    charge(lua, if kind == Kind::HashMap { 2.0 } else { 1.0 })?;
    let op = Op::Update {
        kind,
        name,
        key,
        transform: lua.create_registry_value(f)?,
        expires_in,
    };
    submit(lua, &label(lua, e, "UpdateAsync")?, AnyOp::Ms(op), 0.0)
}

fn map_remove(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let key: LuaValue = args(lua, a, "RemoveAsync")?;
    usable(lua, "RemoveAsync")?;
    let (kind, name, _) = of(lua, e, "RemoveAsync")?;
    let key = key_arg(&key)?;
    charge(lua, 1.0)?;
    submit(
        lua,
        &label(lua, e, "RemoveAsync")?,
        AnyOp::Ms(Op::Delete { kind, name, key }),
        0.0,
    )
}

fn bound(v: &LuaValue) -> Result<Option<(String, Option<SortKey>)>> {
    match v {
        LuaValue::Nil => Ok(None),
        LuaValue::Table(t) => {
            let key: LuaValue = t.get("key")?;
            let sk: LuaValue = t.get("sortKey")?;
            let key = match key {
                LuaValue::Nil => String::new(),
                k => key_arg(&k)?,
            };
            Ok(Some((key, sort_key(&sk)?)))
        }
        other => Err(invalid(format!(
            "a range bound must be a table with key and sortKey, not a {}",
            other.type_name()
        ))),
    }
}

fn map_range(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (dir, count, lower, upper): (LuaValue, f64, LuaValue, LuaValue) =
        args(lua, a, "GetRangeAsync")?;
    usable(lua, "GetRangeAsync")?;
    let (_, name, _) = of(lua, e, "GetRangeAsync")?;
    let dir = EnumItem::coerce(&dir, "SortDirection")?;
    let count = count_arg(count, "GetRangeAsync")?;
    let (lower, upper) = (bound(&lower)?, bound(&upper)?);
    // Units for the returned items are charged when the call completes.
    charge(lua, 1.0)?;
    let op = Op::Range {
        name,
        ascending: dir.name() == "Ascending",
        count,
        lower,
        upper,
    };
    submit(lua, &label(lua, e, "GetRangeAsync")?, AnyOp::Ms(op), 0.0)
}

fn map_size(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    usable(lua, "GetSizeAsync")?;
    let (kind, name, _) = of(lua, e, "GetSizeAsync")?;
    charge(lua, 1.0)?;
    submit(
        lua,
        &label(lua, e, "GetSizeAsync")?,
        AnyOp::Ms(Op::MapSize { kind, name }),
        0.0,
    )
}

fn map_list(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let count: f64 = args(lua, a, "ListItemsAsync")?;
    usable(lua, "ListItemsAsync")?;
    let (_, name, _) = of(lua, e, "ListItemsAsync")?;
    let page = count_arg(count, "ListItemsAsync")?;
    charge(lua, 1.0)?;
    submit(
        lua,
        &label(lua, e, "ListItemsAsync")?,
        AnyOp::Ms(Op::List { name, page }),
        0.0,
    )
}

/// Runs `f` on structure `name` of `kind`, warning once if its file did not
/// read.
fn with<T>(
    lua: &Lua,
    kind: Kind,
    name: &str,
    f: impl FnOnce(&mut memory::Structure) -> T,
) -> Result<T> {
    let now = now_ms(lua)?;
    let (out, problem) = {
        let mut d = data(lua);
        let super::Data {
            memory, settings, ..
        } = &mut *d;
        let (s, problem) = memory.get(settings.backend.as_mut(), kind, name, now);
        (f(s), problem)
    };
    if let Some(p) = problem {
        warn_unreadable(lua, &p, "it did not parse")?;
    }
    Ok(out)
}

fn save(lua: &Lua, kind: Kind, name: &str) -> Result<()> {
    let r = {
        let mut d = data(lua);
        let super::Data {
            memory, settings, ..
        } = &mut *d;
        memory.save(settings.backend.as_mut(), kind, name)
    };
    saved(lua, r, &format!("the memory store structure {name:?}"))
}

/// Checks that growing structure `name` by `grow` bytes and `items` items
/// stays inside the limits.
fn fits(lua: &Lua, kind: Kind, name: &str, grow: isize, items: isize) -> Result<()> {
    let (now, players) = (now_ms(lua)?, players(lua)?);
    data(lua)
        .memory
        .fits(kind, name, grow, items, now, players)
        .map_err(|r| {
            let what = match r {
                Refusal::TotalMemoryOverLimit => "the place's memory quota is used up",
                Refusal::DataStructureMemoryOverLimit => "the structure holds 100 MB already",
                Refusal::DataStructureItemsOverLimit => {
                    "the structure holds 1,000,000 items already"
                }
                _ => "the request is over a limit",
            };
            refused(r, what)
        })
}

fn expires_at(now: i64, secs: f64) -> i64 {
    now.saturating_add((secs * 1000.0) as i64)
}

fn done(lua: &Lua, v: impl IntoLuaMulti) -> Result<Done> {
    Ok(Done::Values(v.into_lua_multi(lua)?))
}

fn item_bytes(key: &str, v: &J) -> isize {
    (key.len() + v.to_string().len()) as isize
}

pub(super) fn complete(lua: &Lua, op: &Op, started: f64) -> Result<Done> {
    let now = now_ms(lua)?;
    match op {
        Op::Add {
            name,
            value,
            expires_in,
            priority,
        } => {
            with(lua, Kind::Queue, name, |_| ())?;
            fits(lua, Kind::Queue, name, value.to_string().len() as isize, 1)?;
            with(lua, Kind::Queue, name, |s| {
                let seq = s.next;
                s.next += 1;
                s.queue.push(QueueItem {
                    seq,
                    value: value.clone(),
                    priority: *priority,
                    expires: expires_at(now, *expires_in),
                    held: None,
                });
            })?;
            save(lua, Kind::Queue, name)?;
            done(lua, ())
        }
        Op::Read {
            name,
            count,
            all_or_nothing,
            wait,
            hold,
        } => {
            let got = with(lua, Kind::Queue, name, |s| {
                let visible = s.visible();
                if visible.is_empty() || (*all_or_nothing && visible.len() < *count) {
                    return None;
                }
                let id = format!("{}", s.next);
                s.next += 1;
                let until = expires_at(now, *hold);
                let mut values = Vec::new();
                for &i in visible.iter().take(*count) {
                    s.queue[i].held = Some((id.clone(), until));
                    values.push(s.queue[i].value.clone());
                }
                Some((id, values))
            })?;
            match got {
                Some((id, values)) => {
                    save(lua, Kind::Queue, name)?;
                    let extra = values.len().saturating_sub(1) as f64;
                    // Each returned item past the first costs a unit; a
                    // place over its units still gets what it read.
                    let _ = charge(lua, extra);
                    let t = lua.create_table()?;
                    for (i, v) in values.iter().enumerate() {
                        t.raw_set(i + 1, value::to_lua(lua, v)?)?;
                    }
                    done(lua, (t, id))
                }
                None => {
                    let waited = sched::now(lua)? - started;
                    if waited + 1e-9 < *wait {
                        // Looking again costs a unit; with none left the
                        // read ends empty.
                        if charge(lua, 1.0).is_ok() {
                            return Ok(Done::Again(READ_POLL.min(*wait - waited)));
                        }
                    }
                    done(lua, (lua.create_table()?, LuaValue::Nil))
                }
            }
        }
        Op::Remove { name, id } => {
            let changed = with(lua, Kind::Queue, name, |s| {
                let before = s.queue.len();
                s.queue
                    .retain(|q| q.held.as_ref().is_none_or(|(h, _)| h != id));
                before != s.queue.len()
            })?;
            if changed {
                save(lua, Kind::Queue, name)?;
            }
            done(lua, ())
        }
        Op::QueueSize {
            name,
            exclude_invisible,
        } => {
            let n = with(lua, Kind::Queue, name, |s| {
                if *exclude_invisible {
                    s.visible().len()
                } else {
                    s.queue.len()
                }
            })?;
            done(lua, n)
        }
        Op::Get { kind, name, key } => {
            let item = with(lua, *kind, name, |s| s.items.get(key).cloned())?;
            match (item, kind) {
                (None, _) => done(lua, LuaValue::Nil),
                (Some(i), Kind::SortedMap) => done(
                    lua,
                    (
                        value::to_lua(lua, &i.value)?,
                        sort_key_lua(lua, &i.sort_key)?,
                    ),
                ),
                (Some(i), _) => done(lua, value::to_lua(lua, &i.value)?),
            }
        }
        Op::Set {
            kind,
            name,
            key,
            value,
            expires_in,
            sort_key,
        } => {
            let old = with(lua, *kind, name, |s| s.items.get(key).cloned())?;
            let grow = item_bytes(key, value)
                - old.as_ref().map(|o| item_bytes(key, &o.value)).unwrap_or(0);
            fits(lua, *kind, name, grow, if old.is_none() { 1 } else { 0 })?;
            with(lua, *kind, name, |s| {
                s.items.insert(
                    key.clone(),
                    Item {
                        value: value.clone(),
                        sort_key: sort_key.clone(),
                        expires: expires_at(now, *expires_in),
                    },
                )
            })?;
            save(lua, *kind, name)?;
            done(lua, old.is_none())
        }
        Op::Update {
            kind,
            name,
            key,
            transform,
            expires_in,
        } => {
            let f: Function = lua.registry_value(transform)?;
            let old = with(lua, *kind, name, |s| s.items.get(key).cloned())?;
            let (cur, sk) = match &old {
                Some(i) => (
                    value::to_lua(lua, &i.value)?,
                    sort_key_lua(lua, &i.sort_key)?,
                ),
                None => (LuaValue::Nil, LuaValue::Nil),
            };
            let args = if *kind == Kind::SortedMap {
                MultiValue::from_iter([cur, sk])
            } else {
                MultiValue::from_iter([cur])
            };
            let out: MultiValue = f.call(args).map_err(|e| {
                invalid(format!(
                    "UpdateAsync: the transform function failed: {}",
                    plain(&e)
                ))
            })?;
            let mut it = out.into_iter();
            let new = it.next().unwrap_or(LuaValue::Nil);
            if new.is_nil() {
                return done(lua, LuaValue::Nil);
            }
            let value = item_value(&new)?;
            let new_sk = if *kind == Kind::SortedMap {
                sort_key(&it.next().unwrap_or(LuaValue::Nil))?
            } else {
                None
            };
            let grow = item_bytes(key, &value)
                - old.as_ref().map(|o| item_bytes(key, &o.value)).unwrap_or(0);
            fits(lua, *kind, name, grow, if old.is_none() { 1 } else { 0 })?;
            with(lua, *kind, name, |s| {
                s.items.insert(
                    key.clone(),
                    Item {
                        value: value.clone(),
                        sort_key: new_sk.clone(),
                        expires: expires_at(now, *expires_in),
                    },
                )
            })?;
            save(lua, *kind, name)?;
            if *kind == Kind::SortedMap {
                done(
                    lua,
                    (value::to_lua(lua, &value)?, sort_key_lua(lua, &new_sk)?),
                )
            } else {
                done(lua, value::to_lua(lua, &value)?)
            }
        }
        Op::Delete { kind, name, key } => {
            let removed = with(lua, *kind, name, |s| s.items.remove(key).is_some())?;
            if removed {
                save(lua, *kind, name)?;
            }
            done(lua, ())
        }
        Op::Range {
            name,
            ascending,
            count,
            lower,
            upper,
        } => {
            let items: Vec<(String, Item)> = with(lua, Kind::SortedMap, name, |s| {
                let mut v: Vec<(String, Item)> = s
                    .sorted()
                    .into_iter()
                    .filter(|(k, i)| {
                        let after = lower.as_ref().is_none_or(|(lk, ls)| {
                            memory::item_order((&i.sort_key, k), (ls, lk)).is_gt()
                        });
                        let before = upper.as_ref().is_none_or(|(uk, us)| {
                            memory::item_order((&i.sort_key, k), (us, uk)).is_lt()
                        });
                        after && before
                    })
                    .map(|(k, i)| (k.clone(), i.clone()))
                    .collect();
                if !ascending {
                    v.reverse();
                }
                v.truncate(*count);
                v
            })?;
            let _ = charge(lua, items.len().saturating_sub(1) as f64);
            let t = lua.create_table()?;
            for (n, (k, i)) in items.iter().enumerate() {
                let row = lua.create_table()?;
                row.raw_set("key", k.as_str())?;
                row.raw_set("value", value::to_lua(lua, &i.value)?)?;
                row.raw_set("sortKey", sort_key_lua(lua, &i.sort_key)?)?;
                t.raw_set(n + 1, row)?;
            }
            done(lua, t)
        }
        Op::MapSize { kind, name } => {
            let n = with(lua, *kind, name, |s| s.items.len())?;
            done(lua, n)
        }
        Op::List { name, page } => {
            let items: Vec<J> = with(lua, Kind::HashMap, name, |s| {
                s.items
                    .iter()
                    .map(|(k, i)| serde_json::json!({ "key": k, "value": i.value }))
                    .collect()
            })?;
            let _ = charge(lua, items.len().min(*page) as f64);
            Ok(Done::Values(
                pages_table(
                    lua,
                    paginate(items, *page),
                    super::budget::Kind::StandardList,
                )?
                .into_lua_multi(lua)?,
            ))
        }
    }
}
