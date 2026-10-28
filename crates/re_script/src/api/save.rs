//! `save`: save slots (ADR-0059 §7). A slot holds a table of the game's data
//! as JSON, stamped with `[saves] version` and the run's `os.time()`; a read
//! passes older data through the steps the game registers with
//! `save.migrate`. The library reaches the store through
//! [`ScriptHost::game`](crate::ScriptHost::game) and raises `RE3015` where no
//! game runs.

use mlua::{Function, Lua, Result, Table, Value};
use serde_json::{Map, Number, Value as Json};

use super::Lib;
use super::entity::EntityRef;
use super::game::GameAccess;
use super::json;
use crate::codes;
use crate::diag::TIMEOUT_MESSAGE;
use crate::host::{ScriptHost, with_host};

/// The most a slot holds, as compact JSON.
pub const MAX_SAVE_BYTES: usize = 4 * 1024 * 1024;

/// How deeply a slot's tables may nest, the slot's own table included.
const MAX_DEPTH: usize = 64;

/// Registry key of the table of migration steps, by the version they migrate
/// from.
const STEPS: &str = "re_script.save_steps";

/// True if `name` is a slot name: 1 to 64 characters from `a`–`z`, `0`–`9`,
/// `_` and `-`. Lowercase only, so two slots never share a file on a
/// case-insensitive file system.
pub fn is_slot_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

/// `RE3102` unless `slot` is a slot name.
fn check_slot(slot: &str, what: &str) -> Result<()> {
    if is_slot_name(slot) {
        return Ok(());
    }
    Err(super::error(
        codes::SAVE_SLOT_INVALID,
        format!(
            "{what}: {slot:?} is not a slot name; use 1 to 64 characters from a-z, 0-9, _ and -"
        ),
    ))
}

/// The game, or `RE3015` naming `what` when this host runs none.
fn game<'h>(h: &'h mut dyn ScriptHost, what: &str) -> Result<&'h mut dyn GameAccess> {
    h.game().ok_or_else(|| {
        super::error(
            codes::SERVICE_UNAVAILABLE,
            format!("{what} is not available here: no game is running"),
        )
    })
}

/// `n` as JSON: integral values within ±2^53 as integers, others as they
/// are; `None` for NaN and the infinities.
fn number(n: f64) -> Option<Json> {
    if n.fract() == 0.0
        && n.abs() <= 9.007_199_254_740_992e15
        && !(n == 0.0 && n.is_sign_negative())
    {
        return Some(Json::from(n as i64));
    }
    Number::from_f64(n).map(Json::Number)
}

/// `at` followed by the key `key`: `data.coins`, `data["two words"]`.
fn field(at: &str, key: &str) -> String {
    let ident = key
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ident {
        format!("{at}.{key}")
    } else {
        format!("{at}[{key:?}]")
    }
}

/// `v`'s type as Luau names it (mlua tells integral numbers apart).
fn luau_type(v: &Value) -> &'static str {
    match v {
        Value::Integer(_) | Value::Number(_) => "number",
        other => other.type_name(),
    }
}

/// A number key as the message shows it.
fn show_key(k: &Value) -> String {
    match k {
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.type_name().to_string(),
    }
}

/// `v`, found at `at`, as JSON that reads back as written, or why it cannot
/// be saved (the end of an `RE3103` message).
fn to_json(v: &Value, at: &str, depth: usize) -> std::result::Result<Json, String> {
    match v {
        Value::Nil => Ok(Json::Null),
        Value::Boolean(b) => Ok(Json::Bool(*b)),
        Value::Integer(i) => Ok(Json::from(*i)),
        Value::Number(n) => {
            number(*n).ok_or_else(|| format!("{at} is {n}, which JSON cannot hold"))
        }
        Value::String(s) => match s.to_str() {
            Ok(s) => Ok(Json::String(s.to_string())),
            Err(_) => Err(format!("{at} is a string that is not UTF-8 text")),
        },
        Value::Vector(_) => Err(format!(
            "{at} is a vector, which a save cannot hold; store {{x, y, z}} instead"
        )),
        Value::Table(t) => table(t, at, depth),
        Value::UserData(ud) if ud.is::<EntityRef>() => Err(format!(
            "{at} is an entity, which a save cannot hold; store the entity's id instead"
        )),
        other => Err(format!(
            "{at} is a {}, which a save cannot hold",
            luau_type(other)
        )),
    }
}

/// A table at `at`: an array when its keys are exactly `1..n`, an object with
/// sorted keys when they are all strings.
fn table(t: &Table, at: &str, depth: usize) -> std::result::Result<Json, String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "data nests tables deeper than {MAX_DEPTH} levels (or a table holds itself)"
        ));
    }
    let mut entries: Vec<(Value, Value)> = Vec::new();
    for pair in t.pairs::<Value, Value>() {
        entries.push(pair.map_err(|e| format!("{at} cannot be read: {e}"))?);
    }
    let numbers = entries
        .iter()
        .filter(|(k, _)| matches!(k, Value::Integer(_) | Value::Number(_)))
        .count();
    if numbers > 0 {
        if numbers < entries.len() {
            return Err(format!(
                "{at} mixes number keys with other keys; a saved table is an array (keys 1 to n) or has string keys only"
            ));
        }
        let n = entries.len();
        let mut items: Vec<Option<Value>> = vec![None; n];
        for (k, v) in entries {
            let index = match &k {
                Value::Integer(i) => usize::try_from(*i).ok(),
                Value::Number(x) if x.fract() == 0.0 && *x >= 1.0 && *x <= n as f64 => {
                    Some(*x as usize)
                }
                _ => None,
            };
            match index {
                Some(i) if (1..=n).contains(&i) => items[i - 1] = Some(v),
                _ => {
                    return Err(format!(
                        "{at} has the number key {}, which is not part of a sequence 1 to n; use string keys for a table with gaps",
                        show_key(&k)
                    ));
                }
            }
        }
        // `n` distinct keys within 1..=n: every item is set.
        let mut out = Vec::with_capacity(n);
        for (i, item) in items.into_iter().enumerate() {
            let item = item.unwrap_or(Value::Nil);
            out.push(to_json(&item, &format!("{at}[{}]", i + 1), depth + 1)?);
        }
        return Ok(Json::Array(out));
    }
    let mut keyed: Vec<(String, Json)> = Vec::with_capacity(entries.len());
    for (k, v) in &entries {
        let key = match k {
            Value::String(s) => s
                .to_str()
                .map_err(|_| format!("{at} has a key that is not UTF-8 text"))?
                .to_string(),
            other => {
                return Err(format!(
                    "{at} has a {} key; a saved table is an array (keys 1 to n) or has string keys only",
                    luau_type(other)
                ));
            }
        };
        let value = to_json(v, &field(at, &key), depth + 1)?;
        keyed.push((key, value));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Json::Object(keyed.into_iter().collect::<Map<_, _>>()))
}

/// The run's `os.time()`: what the game context's own `os.time` returns, so
/// a slot's `saved_at` is the clock the game's scripts read (ADR-0057 §10).
fn os_time(lua: &Lua) -> Result<i64> {
    let os: Table = lua.globals().get("os")?;
    let time: Function = os.get("time")?;
    let t: f64 = time.call(())?;
    Ok(t.floor() as i64)
}

/// True for an error a migration step must not wrap: the budget's timeout
/// and running out of memory, which stop the calling script as they are.
fn stops_the_script(e: &mlua::Error) -> bool {
    match e {
        mlua::Error::MemoryError(_) => true,
        mlua::Error::CallbackError { cause, .. } => stops_the_script(cause),
        other => other.to_string().contains(TIMEOUT_MESSAGE),
    }
}

/// The first line of a step's error, without mlua's prefixes, and with the
/// step's position moved to the end (`no coins (scripts/progress.luau:4)`), so
/// the diagnostic stays at the `save.read` call that ran the step.
fn short(e: &mlua::Error) -> String {
    let text = match e {
        mlua::Error::CallbackError { cause, .. } => return short(cause),
        mlua::Error::RuntimeError(m) => m.clone(),
        other => other.to_string(),
    };
    let first = text.lines().next().unwrap_or_default();
    let first = first.trim_start_matches("runtime error: ").trim();
    match crate::diag::split_location(first) {
        (Some(chunk), Some(line), message) => format!("{message} ({chunk}:{line})"),
        _ => first.to_string(),
    }
}

fn write(lua: &Lua, slot: &str, data: &Value) -> Result<()> {
    check_slot(slot, "save.write")?;
    let invalid =
        |why: String| super::error(codes::SAVE_VALUE_INVALID, format!("save.write: {why}"));
    let Value::Table(t) = data else {
        return Err(invalid(format!(
            "the data must be a table, not a {}",
            luau_type(data)
        )));
    };
    let json = table(t, "data", 1).map_err(invalid)?;
    let size = serde_json::to_vec(&json).map_or(usize::MAX, |b| b.len());
    if size > MAX_SAVE_BYTES {
        return Err(invalid(format!(
            "slot {slot:?} would hold {:.1} MiB as JSON, more than the 4 MiB a slot holds",
            size as f64 / (1024.0 * 1024.0)
        )));
    }
    let saved_at = os_time(lua)?;
    with_host(lua, |h| {
        game(h, "save.write")?.save_write(slot, saved_at, json);
        Ok(())
    })
}

fn read(lua: &Lua, slot: &str) -> Result<Value> {
    check_slot(slot, "save.read")?;
    let (version, found) = with_host(lua, |h| {
        let game = game(h, "save.read")?;
        Ok((game.save_version(), game.save_read(slot)))
    })?;
    let Some(found) = found else {
        return Ok(Value::Nil);
    };
    let from = found.data_version;
    if from > version {
        return Err(super::error(
            codes::SAVE_TOO_NEW,
            format!(
                "save.read: slot {slot:?} holds version {from} data, newer than this build's [saves] version {version}"
            ),
        ));
    }
    // Every step is found before any runs, so a missing one changes nothing.
    let registered: Table = lua.named_registry_value(STEPS)?;
    let mut steps = Vec::new();
    for v in from..version {
        match registered.raw_get::<Option<Function>>(v)? {
            Some(step) => steps.push((v, step)),
            None => {
                return Err(super::error(
                    codes::SAVE_MIGRATION_MISSING,
                    format!(
                        "save.read: slot {slot:?} holds version {from} data, and no step migrates version {v} to {}; add save.migrate({v}, step)",
                        v + 1
                    ),
                ));
            }
        }
    }
    let mut data = json::from_json(lua, &found.data)?;
    for (v, step) in steps {
        data = match step.call::<Value>(data) {
            Ok(Value::Table(t)) => Value::Table(t),
            Ok(other) => {
                return Err(super::error(
                    codes::SAVE_MIGRATION_FAILED,
                    format!(
                        "save.read: the step from version {v} returned a {}, not a table (slot {slot:?})",
                        luau_type(&other)
                    ),
                ));
            }
            Err(e) if stops_the_script(&e) => return Err(e),
            Err(e) => {
                return Err(super::error(
                    codes::SAVE_MIGRATION_FAILED,
                    format!(
                        "save.read: the step from version {v} failed on slot {slot:?}: {}",
                        short(&e)
                    ),
                ));
            }
        };
    }
    Ok(data)
}

/// The `save` library.
pub(crate) fn lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lua.set_named_registry_value(STEPS, lua.create_table()?)?;
    lib.func(
        lua,
        "write",
        "(slot: string, data: {[any]: any}) -> ()",
        "Saves a table of the game's data in `slot`, stamped with [saves] version and os.time(); reads see it at once, and the file is written at the end of the frame.",
        |lua, (slot, data): (String, Value)| write(lua, &slot, &data),
    )?;
    lib.func(
        lua,
        "read",
        "(slot: string) -> {[any]: any}?",
        "The data in `slot`, migrated to [saves] version by the save.migrate steps; nil for an empty slot.",
        |lua, slot: String| read(lua, &slot),
    )?;
    lib.func(
        lua,
        "exists",
        "(slot: string) -> boolean",
        "True if `slot` holds data.",
        |lua, slot: String| {
            check_slot(&slot, "save.exists")?;
            with_host(lua, |h| Ok(game(h, "save.exists")?.save_exists(&slot)))
        },
    )?;
    lib.func(
        lua,
        "list",
        "() -> { { slot: string, version: number, saved_at: number } }",
        "Every slot that holds data, sorted by name, with the data version and os.time() it was written with.",
        |lua, ()| {
            let slots = with_host(lua, |h| Ok(game(h, "save.list")?.save_list()))?;
            let out = lua.create_table_with_capacity(slots.len(), 0)?;
            for s in slots {
                let row = lua.create_table_with_capacity(0, 3)?;
                row.raw_set("slot", s.slot)?;
                row.raw_set("version", s.data_version)?;
                row.raw_set("saved_at", s.saved_at)?;
                out.raw_push(row)?;
            }
            Ok(out)
        },
    )?;
    lib.func(
        lua,
        "delete",
        "(slot: string) -> ()",
        "Empties `slot`; its file goes at the end of the frame.",
        |lua, slot: String| {
            check_slot(&slot, "save.delete")?;
            with_host(lua, |h| {
                game(h, "save.delete")?.save_delete(&slot);
                Ok(())
            })
        },
    )?;
    lib.func(
        lua,
        "version",
        "() -> number",
        "[saves] version: the data version this build writes.",
        |lua, ()| with_host(lua, |h| Ok(game(h, "save.version")?.save_version())),
    )?;
    lib.func(
        lua,
        "migrate",
        "(from: number, step: (data: {[any]: any}) -> {[any]: any}) -> ()",
        "Registers the step that turns version `from` data into version `from + 1` data; save.read runs the steps a slot needs, in turn.",
        |lua, (from, step): (f64, Function)| {
            let version = with_host(lua, |h| Ok(game(h, "save.migrate")?.save_version()))?;
            if from.fract() != 0.0 || from < 1.0 || from >= f64::from(version) {
                let why = if version > 1 {
                    format!("a whole number from 1 to {}", version - 1)
                } else {
                    "below [saves] version, which is 1: raise it in real.toml when the data changes".to_string()
                };
                return Err(super::error(
                    codes::BAD_ARGUMENT,
                    format!("save.migrate: from is {from}; it must be {why}"),
                ));
            }
            // A later step for the same version (a reloaded script) replaces it.
            let steps: Table = lua.named_registry_value(STEPS)?;
            steps.raw_set(from as u32, step)
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{field, is_slot_name};

    #[test]
    fn slot_names_are_short_lowercase_words() {
        for ok in ["a", "slot1", "auto-save", "quick_save", &"x".repeat(64)] {
            assert!(is_slot_name(ok), "{ok}");
        }
        for bad in ["", "Slot1", "a b", "a.b", "a/b", "é", &"x".repeat(65)] {
            assert!(!is_slot_name(bad), "{bad}");
        }
    }

    #[test]
    fn paths_name_fields_as_luau_would() {
        assert_eq!(field("data", "coins"), "data.coins");
        assert_eq!(field("data", "two words"), r#"data["two words"]"#);
        assert_eq!(field("data", "1st"), r#"data["1st"]"#);
    }
}
