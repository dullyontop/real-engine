//! `config.get` and `config.set`: `real.toml` read and edited as a document
//! (ADR-0064 §5).
//!
//! * `config.get {key?}` reads the value at a dotted key
//!   (`render.post.exposure`; a number indexes an array), or the whole
//!   configuration, as JSON: from the open document when there is one, else
//!   from the file;
//! * `config.set {key, value?}` sets the value at the key, making missing
//!   tables, or removes it when `value` is left out.
//!
//! The document holds `real.toml`'s JSON form. Its validator converts the
//! JSON back to TOML and parses it as the App does when it starts, so an
//! unknown key, a wrong type or a value out of range is refused exactly as
//! at start (`RE2008`). Its writer merges the JSON into the file's last text
//! with `toml_edit`: comments, order and layout stay, and only the changed
//! values' text changes, so undoing every edit gives the file back byte for
//! byte.

use std::str::FromStr;

use re_command::{Call, CommandRegistry, Op, Tier, codes as command_codes};
use re_core::{Diagnostic, Location};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use super::{open_file, token, transact};
use crate::app::App;
use crate::config::{AppConfig, CONFIG_FILE};

/// Arguments of `config.get`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetArgs {
    /// A dotted key (`sim.fixed_hz`, `input.axes.move.1.dead_zone`); the
    /// whole configuration when left out.
    #[serde(default)]
    pub key: Option<String>,
}

/// Arguments of `config.set`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetArgs {
    /// A dotted key of bare TOML keys (`render.post.exposure`); a number
    /// indexes an array.
    pub key: String,
    /// The new value, any JSON but `null`; left out, the key is removed.
    #[serde(default, deserialize_with = "present")]
    pub value: Option<Json>,
}

/// A value that is present, `null` included, as `Some` (one left out is
/// `None` through `#[serde(default)]`), so a `null` is refused rather than
/// read as a removal.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Json>, D::Error> {
    Json::deserialize(d).map(Some)
}

/// `real.toml`'s text as the document's JSON content.
fn parse(text: &str) -> Result<Json, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.message().to_string())?;
    serde_json::to_value(table).map_err(|e| e.to_string())
}

/// `value` as TOML text, or why it cannot be.
fn to_toml(value: &Json) -> Result<String, String> {
    if let Some(at) = null_at(value, String::new()) {
        return Err(format!("TOML has no null (at `{at}`)"));
    }
    toml::to_string(value).map_err(|e| e.to_string())
}

/// Where the first `null` in `value` is, as a dotted key.
fn null_at(value: &Json, at: String) -> Option<String> {
    let join = |k: &str| {
        if at.is_empty() {
            k.to_string()
        } else {
            format!("{at}.{k}")
        }
    };
    match value {
        Json::Null => Some(at),
        Json::Array(a) => a
            .iter()
            .enumerate()
            .find_map(|(i, v)| null_at(v, join(&i.to_string()))),
        Json::Object(m) => m.iter().find_map(|(k, v)| null_at(v, join(k))),
        _ => None,
    }
}

/// The validator of the open `real.toml`: what [`AppConfig::parse`] refuses.
pub(super) fn validate(value: &Json) -> Result<(), String> {
    let text = to_toml(value)?;
    AppConfig::parse(&text).map(|_| ()).map_err(|d| d.message)
}

/// The writer of `real.toml`: `value` merged into `base`, the last text read
/// or written, keeping everything that did not change.
pub(super) fn write(value: &Json, base: Option<&str>) -> Result<String, String> {
    let Json::Object(want) = value else {
        return Err("real.toml holds a table".into());
    };
    let mut doc = match base {
        Some(text) => DocumentMut::from_str(text).map_err(|e| e.to_string())?,
        None => DocumentMut::new(),
    };
    merge(doc.as_table_mut(), want)?;
    Ok(doc.to_string())
}

/// Makes `table` hold `want`, touching only what differs: an unchanged key
/// keeps its text, and a changed value keeps its key's spacing and comment.
fn merge(table: &mut Table, want: &Map<String, Json>) -> Result<(), String> {
    let gone: Vec<String> = table
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !want.contains_key(k))
        .collect();
    for k in gone {
        table.remove(&k);
    }
    for (k, v) in want {
        if table
            .get(k)
            .is_some_and(|item| item_json(item).as_ref() == Some(v))
        {
            continue;
        }
        match (table.get_mut(k), v) {
            (Some(Item::Table(t)), Json::Object(o)) => merge(t, o)?,
            (Some(Item::Value(old)), _) if !v.is_object() || old.is_inline_table() => {
                merge_value(old, v)?;
            }
            _ => {
                let item = match v {
                    Json::Object(o) => {
                        let mut t = Table::new();
                        merge(&mut t, o)?;
                        Item::Table(t)
                    }
                    other => Item::Value(value(other)?),
                };
                table.insert(k, item);
            }
        }
    }
    Ok(())
}

/// Makes `old` hold `want`: element by element for an array of the same
/// length, key by key for an inline table, and otherwise a new value with
/// `old`'s spacing and comment.
fn merge_value(old: &mut Value, want: &Json) -> Result<(), String> {
    if item_json(&Item::Value(old.clone())).as_ref() == Some(want) {
        return Ok(());
    }
    match (&mut *old, want) {
        (Value::Array(a), Json::Array(w)) if a.len() == w.len() => {
            for (i, wv) in w.iter().enumerate() {
                merge_value(a.get_mut(i).expect("the same length"), wv)?;
            }
        }
        (Value::InlineTable(t), Json::Object(o)) => {
            let gone: Vec<String> = t
                .iter()
                .map(|(k, _)| k.to_string())
                .filter(|k| !o.contains_key(k))
                .collect();
            for k in gone {
                t.remove(&k);
            }
            for (k, wv) in o {
                match t.get_mut(k) {
                    Some(v) => merge_value(v, wv)?,
                    None => {
                        t.insert(k, value(wv)?);
                    }
                }
            }
        }
        _ => {
            let decor = old.decor().clone();
            let mut new = value(want)?;
            *new.decor_mut() = decor;
            *old = new;
        }
    }
    Ok(())
}

/// A TOML value for a JSON value (objects inside values become inline
/// tables).
fn value(v: &Json) -> Result<Value, String> {
    Ok(match v {
        Json::Bool(b) => Value::from(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::from(i),
            None => Value::from(n.as_f64().ok_or("a number TOML cannot hold")?),
        },
        Json::String(s) => Value::from(s.as_str()),
        Json::Array(a) => Value::Array(a.iter().map(value).collect::<Result<_, _>>()?),
        Json::Object(o) => {
            let mut t = InlineTable::new();
            for (k, v) in o {
                t.insert(k, value(v)?);
            }
            Value::InlineTable(t)
        }
        Json::Null => return Err("TOML has no null".into()),
    })
}

/// What `item` holds, as JSON (`None` for what does not parse alone).
fn item_json(item: &Item) -> Option<Json> {
    let text = match item {
        Item::Table(t) => t.to_string(),
        Item::Value(v) => format!("x = {v}"),
        other => other.to_string(),
    };
    let parsed = parse(&text).ok()?;
    match item {
        Item::Value(_) => parsed.get("x").cloned(),
        _ => Some(parsed),
    }
}

/// The segments of a dotted key, each a bare TOML key, or `RE2057`.
fn segments(key: &str) -> Result<Vec<&str>, Diagnostic> {
    let bare = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    let segs: Vec<&str> = key.split('.').collect();
    if let Some(bad) = segs.iter().find(|s| !bare(s)) {
        return Err(Diagnostic::error(
            command_codes::CONFIG_NOT_TOML,
            format!(
                "`{key}` is not a configuration key: `{bad}` is not a bare TOML key (letters, \
                 digits, `_` and `-`)"
            ),
        )
        .at(Location::file(CONFIG_FILE))
        .with_hint("write the key as dotted bare keys, such as `render.post.exposure`"));
    }
    Ok(segs)
}

/// The value at `segs` in `root`: a segment indexes an array by number.
fn get<'a>(root: &'a Json, segs: &[&str]) -> Option<&'a Json> {
    segs.iter().try_fold(root, |v, s| match v {
        Json::Object(m) => m.get(*s),
        Json::Array(a) => s.parse::<usize>().ok().and_then(|i| a.get(i)),
        _ => None,
    })
}

/// The JSON pointer of `segs`.
fn pointer(segs: &[&str]) -> String {
    segs.iter().map(|s| format!("/{}", token(s))).collect()
}

/// The operation that sets (`Some`) or removes (`None`) the value at `segs`
/// in `root`: `Ok(None)` when nothing changes.
fn op_for(root: &Json, segs: &[&str], value: Option<&Json>) -> Result<Option<Op>, String> {
    let set = |pointer: String, value: Option<Json>| Op::SetDoc {
        doc: CONFIG_FILE.into(),
        pointer,
        value,
    };
    // The deepest existing parent, as the number of segments to it.
    let mut depth = 0;
    let mut parent = root;
    while depth + 1 < segs.len() {
        match get(parent, &segs[depth..=depth]) {
            Some(child) if child.is_object() || child.is_array() => {
                parent = child;
                depth += 1;
            }
            Some(_) => {
                return Err(format!(
                    "`{}` is a value, not a table",
                    segs[..=depth].join(".")
                ));
            }
            None => break,
        }
    }
    let rest = &segs[depth..];
    let Some(value) = value else {
        // Removing: only what exists.
        if rest.len() > 1 || get(parent, rest).is_none() {
            return Ok(None);
        }
        return Ok(Some(match parent {
            Json::Array(a) => {
                let i: usize = rest[0].parse().expect("found by index");
                let mut a = a.clone();
                a.remove(i);
                set(pointer(&segs[..depth]), Some(Json::Array(a)))
            }
            _ => set(pointer(segs), None),
        }));
    };
    if rest.len() == 1 && get(parent, rest) == Some(value) {
        return Ok(None);
    }
    // The missing tables below the parent, holding the value.
    let built = rest[1..]
        .iter()
        .rev()
        .fold(value.clone(), |v, k| json!({ *k: v }));
    match parent {
        Json::Array(a) => {
            let i: usize = rest[0]
                .parse()
                .ok()
                .filter(|i| *i <= a.len())
                .ok_or_else(|| {
                    format!(
                        "`{}` is a list of {} items: `{}` is not one of them, or the next",
                        segs[..depth].join("."),
                        a.len(),
                        rest[0]
                    )
                })?;
            // An element is replaced, or appended, as part of its whole list.
            let mut a = a.clone();
            if i == a.len() {
                a.push(built);
            } else {
                a[i] = built;
            }
            Ok(Some(set(pointer(&segs[..depth]), Some(Json::Array(a)))))
        }
        _ => Ok(Some(set(pointer(&segs[..=depth]), Some(built)))),
    }
}

/// The configuration as a document: the open one, else the file's.
fn current(app: &App) -> Result<Json, Diagnostic> {
    if let Some(doc) = app.world().doc(CONFIG_FILE) {
        return Ok(doc.value.clone());
    }
    let text = app.files().read_str(CONFIG_FILE).map_err(|e| {
        Diagnostic::error(
            crate::codes::CONFIG_INVALID,
            format!("cannot read {CONFIG_FILE}: {e}"),
        )
        .at(Location::file(CONFIG_FILE))
    })?;
    parse(&text).map_err(|e| super::invalid(CONFIG_FILE, &e))
}

fn get_cmd(app: &App, a: GetArgs) -> Result<Json, Diagnostic> {
    let root = current(app)?;
    let Some(key) = a.key else {
        return Ok(json!({ "value": root }));
    };
    let segs = segments(&key)?;
    let value = get(&root, &segs).cloned();
    Ok(json!({ "key": key, "set": value.is_some(), "value": value }))
}

fn set_cmd(app: &mut App, a: SetArgs, call: &mut Call) -> Result<Json, Diagnostic> {
    let not_toml = |why: String| {
        Diagnostic::error(command_codes::CONFIG_NOT_TOML, why).at(Location::file(CONFIG_FILE))
    };
    let segs = segments(&a.key)?;
    if let Some(v) = &a.value
        && let Some(at) = null_at(v, String::new())
    {
        let at = if at.is_empty() {
            a.key.clone()
        } else {
            format!("{}.{at}", a.key)
        };
        return Err(not_toml(format!(
            "{CONFIG_FILE} cannot hold `null` at `{at}`: TOML has no null"
        ))
        .with_hint("leave `value` out to remove the key"));
    }
    // No argument names real.toml, so the policy has not seen it yet.
    call.check_write(CONFIG_FILE)?;
    let opened = open_file(app, CONFIG_FILE, parse)?;
    let root = app
        .world()
        .doc(CONFIG_FILE)
        .map_or(Json::Null, |d| d.value.clone());
    let op = match op_for(&root, &segs, a.value.as_ref()) {
        Ok(op) => op,
        Err(why) => {
            if opened {
                app.world_mut().close_doc(CONFIG_FILE);
            }
            return Err(not_toml(format!("cannot set `{}`: {why}", a.key)));
        }
    };
    let what = if a.value.is_some() { "set" } else { "remove" };
    let changed = transact(
        app,
        call,
        CONFIG_FILE,
        opened,
        format!("config.set {} ({what})", a.key),
        op.into_iter().collect(),
    )?;
    Ok(json!({ "key": a.key, "changed": changed, "value": a.value }))
}

/// Registers `config.get` and `config.set`.
pub fn register(registry: &mut CommandRegistry<App>) {
    registry.register_typed::<GetArgs, _>(
        "config.get",
        "Read real.toml: the value at a dotted key (`render.post.exposure`; a number indexes a list), or the whole configuration, as JSON, from the open document when there is one.",
        Tier::Read,
        true,
        |app, args, _call| get_cmd(app, args),
    );
    registry.register_typed::<SetArgs, _>(
        "config.set",
        "Set the value at a dotted key of real.toml (making missing tables), or remove it when `value` is left out; checked as the App reads real.toml when it starts. One undo step; the project save writes the file, keeping its comments and layout.",
        Tier::Edit,
        false,
        set_cmd,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `real new`'s template, with comments.
    const TEMPLATE: &str = r#"[project]
name = "spike"
entry_scene = "scenes/main.scene.json"

# Simulation rate.
[sim]
fixed_hz = 60   # steps per second
max_steps_per_frame = 5

[render]
backends = ["vulkan", "gl"]

[scripts]
cpu_budget_ms = 4
memory_limit_mb = 256

[input.buttons]
jump = ["key:Space", "pad:South"]

[input.axes]
move = [{ composite = ["key:KeyW", "key:KeyS", "key:KeyA", "key:KeyD"] }, { stick = "left", dead_zone = 0.15 }]
"#;

    fn json_of(text: &str) -> Json {
        parse(text).unwrap()
    }

    /// The document after `op` (or unchanged).
    fn apply(root: &Json, op: Option<Op>) -> Json {
        let mut out = root.clone();
        if let Some(Op::SetDoc { pointer, value, .. }) = op {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            let parent = out.pointer_mut(parent).unwrap().as_object_mut().unwrap();
            match value {
                Some(v) => {
                    parent.insert(key.to_string(), v);
                }
                None => {
                    parent.shift_remove(key);
                }
            }
        }
        out
    }

    fn changed_lines(a: &str, b: &str) -> Vec<String> {
        let (la, lb): (Vec<_>, Vec<_>) = (a.lines().collect(), b.lines().collect());
        (0..la.len().max(lb.len()))
            .filter_map(|i| {
                let (x, y) = (la.get(i).copied(), lb.get(i).copied());
                (x != y).then(|| format!("{x:?} -> {y:?}"))
            })
            .collect()
    }

    #[test]
    fn the_template_validates_and_prints_back_unchanged() {
        let root = json_of(TEMPLATE);
        validate(&root).unwrap();
        assert_eq!(write(&root, Some(TEMPLATE)).unwrap(), TEMPLATE);
    }

    #[test]
    fn a_changed_value_changes_its_line_only_and_comes_back() {
        let root = json_of(TEMPLATE);
        let op = op_for(&root, &["sim", "fixed_hz"], Some(&json!(30))).unwrap();
        let want = apply(&root, op);
        validate(&want).unwrap();
        let text = write(&want, Some(TEMPLATE)).unwrap();
        assert_eq!(
            changed_lines(TEMPLATE, &text),
            [
                r#"Some("fixed_hz = 60   # steps per second") -> Some("fixed_hz = 30   # steps per second")"#
            ]
        );
        assert_eq!(json_of(&text), want);
        assert_eq!(write(&root, Some(&text)).unwrap(), TEMPLATE);
    }

    #[test]
    fn tables_and_keys_are_added_and_removed_again_exactly() {
        let root = json_of(TEMPLATE);
        let op = op_for(&root, &["proc", "luau_budget"], Some(&json!(1000)))
            .unwrap()
            .unwrap();
        let Op::SetDoc { pointer, value, .. } = &op else {
            unreachable!()
        };
        assert_eq!(pointer, "/proc");
        assert_eq!(value.as_ref().unwrap(), &json!({"luau_budget": 1000}));
        let mut want = apply(&root, Some(op));
        want = apply(
            &want,
            op_for(&want, &["render", "width"], Some(&json!(800))).unwrap(),
        );
        validate(&want).unwrap();
        let text = write(&want, Some(TEMPLATE)).unwrap();
        assert!(text.contains("[proc]\nluau_budget = 1000"), "{text}");
        assert!(text.contains("width = 800"), "{text}");
        assert_eq!(write(&root, Some(&text)).unwrap(), TEMPLATE);
    }

    #[test]
    fn a_value_inside_a_list_changes_in_place() {
        let root = json_of(TEMPLATE);
        let segs = ["input", "axes", "move", "1", "dead_zone"];
        let op = op_for(&root, &segs, Some(&json!(0.2))).unwrap();
        let want = apply(&root, op);
        assert_eq!(get(&want, &segs), Some(&json!(0.2)));
        let text = write(&want, Some(TEMPLATE)).unwrap();
        let lines = changed_lines(TEMPLATE, &text);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("dead_zone = 0.2 }]"), "{lines:?}");
        assert_eq!(write(&root, Some(&text)).unwrap(), TEMPLATE);

        // A list's element is replaced, or appended, through its whole list.
        let op = op_for(&root, &["render", "backends", "2"], Some(&json!("dx12")))
            .unwrap()
            .unwrap();
        let Op::SetDoc { pointer, value, .. } = &op else {
            unreachable!()
        };
        assert_eq!(pointer, "/render/backends");
        assert_eq!(value.as_ref().unwrap(), &json!(["vulkan", "gl", "dx12"]));
        assert!(op_for(&root, &["render", "backends", "5"], Some(&json!("gl"))).is_err());
    }

    #[test]
    fn keys_values_and_types_toml_cannot_hold_are_refused() {
        assert!(segments("render.post.exposure").is_ok());
        for key in ["", "render..post", "render.\"post\"", "a b", "render."] {
            let d = segments(key).unwrap_err();
            assert_eq!(d.code, command_codes::CONFIG_NOT_TOML, "{key}");
        }
        assert_eq!(
            null_at(&json!({"a": [1, null]}), String::new()).as_deref(),
            Some("a.1")
        );
        let root = json_of(TEMPLATE);
        assert!(op_for(&root, &["sim", "fixed_hz", "x"], Some(&json!(1))).is_err());
        // A wrong type, an unknown key and a value out of range are refused
        // as at start.
        let mut bad = root.clone();
        bad["sim"]["fixed_hz"] = json!("fast");
        assert!(validate(&bad).is_err());
        let mut bad = root.clone();
        bad["sim"]["speed"] = json!(2);
        assert!(validate(&bad).is_err());
        // Removing what is not there changes nothing.
        assert!(op_for(&root, &["render", "vsync"], None).unwrap().is_none());
        assert!(op_for(&root, &["nothing", "here"], None).unwrap().is_none());
    }
}
