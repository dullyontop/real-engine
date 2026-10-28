//! Widgets built from JSON Schema (ADR-0044's amendment on previews, §1): the
//! node canvas and the Procedural panel edit a node kind's parameters with a
//! widget per property, chosen from the property's schema.
//!
//! [`Shape::of`] reads a property, after resolving its `$ref`, into the
//! widget it gets, and [`edit`] draws that widget for a value. A widget keeps
//! a value being dragged or typed in egui's memory under its own id, reports
//! it as [`Change::Live`] every frame the drag goes on and as
//! [`Change::Done`] once, when the widget is released. [`to_send`] turns those
//! into what a command sends: every finished edit, and while a drag goes on
//! the value under the pointer at most every [`LIVE_INTERVAL`] seconds, under
//! one coalesce key so the history makes the whole drag one undo step. The
//! command checks every value against the kind, so a widget is never the only
//! guard.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use egui::{Id, RichText};
use re_core::Diagnostic;
use serde_json::{Map, Value as Json};

use crate::codes;

/// The shortest time between two live edits of one drag, in seconds: each one
/// regenerates the procedural source.
pub(crate) const LIVE_INTERVAL: f64 = 0.2;

/// How long a picker's list of asset files is reused before the project is
/// listed again, in seconds.
const ASSET_LIST_AGE: f64 = 2.0;

/// How deep `$ref`s and nested shapes are followed.
const DEPTH: usize = 8;

/// A list longer than this starts folded.
const OPEN_ITEMS: usize = 4;

/// The widget a property gets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Shape {
    /// `boolean`: a checkbox.
    Bool,
    /// `integer`: a drag within `range` (from the bounds and the `format`),
    /// or a slider when the schema bounds both ends.
    Int { range: (i64, i64), slider: bool },
    /// `number`: a drag within `range`, or a slider when the schema bounds
    /// both ends. `single` (`format: float`) sends the shortest decimal that
    /// reads back as the same `f32`.
    Number {
        range: (f64, f64),
        slider: bool,
        single: bool,
    },
    /// A `oneOf` (or `enum`) of string constants: a combo box of the
    /// variants, each with its description.
    Choice(Vec<(String, Option<String>)>),
    /// A value or `null` (`["number", "null"]`): a "set" checkbox beside the
    /// value's widget.
    Nullable(Box<Shape>),
    /// An array of 2 or 3 numbers: drags in a row.
    Vector { len: usize, single: bool },
    /// An array of 3 or 4 numbers with `"x-real-widget": "colour"`: a colour
    /// button with egui's picker, in linear colour (alpha when 4).
    Colour { alpha: bool },
    /// A string with `"x-real-asset": "<kind>"`: an asset picker listing the
    /// project's files of that kind.
    Asset(String),
    /// Any other string: a text field.
    Text,
    /// An object whose values have a widget (`points.instance`'s `prefabs`):
    /// a key and a value per row, with add and remove.
    Map(Box<Shape>),
    /// An array whose items have a widget: a widget per item, with add,
    /// remove and reorder.
    List(Box<Shape>),
    /// Anything else: its JSON as text, parsed when committed and checked
    /// against the property's `type` (`types`, empty for any).
    Raw { types: Vec<String> },
}

impl Shape {
    /// The widget of property `prop` of `root`, the schema that holds the
    /// `$defs` its `$ref`s name.
    pub(crate) fn of(prop: &Json, root: &Json) -> Shape {
        shape(&resolve(prop, root, 0), root, 0)
    }

    /// True when the widget can show `v`: a value of the property's type
    /// (the kind still checks ranges and the rest).
    pub(crate) fn fits(&self, v: &Json) -> bool {
        match self {
            Shape::Bool => v.is_boolean(),
            Shape::Int { .. } => v.is_i64() || v.is_u64(),
            Shape::Number { .. } => v.is_number(),
            Shape::Choice(variants) => v
                .as_str()
                .is_some_and(|s| variants.iter().any(|(c, _)| c == s)),
            Shape::Nullable(inner) => v.is_null() || inner.fits(v),
            Shape::Vector { len, .. } => v
                .as_array()
                .is_some_and(|a| a.len() == *len && a.iter().all(Json::is_number)),
            Shape::Colour { alpha } => v.as_array().is_some_and(|a| {
                a.len() == 3 + usize::from(*alpha) && a.iter().all(Json::is_number)
            }),
            Shape::Asset(_) | Shape::Text => v.is_string(),
            Shape::Map(values) => v
                .as_object()
                .is_some_and(|m| m.values().all(|x| values.fits(x))),
            Shape::List(item) => v.as_array().is_some_and(|a| a.iter().all(|x| item.fits(x))),
            Shape::Raw { types } => fits(types, v),
        }
    }

    /// The same property edited as JSON text, for a value its widget cannot
    /// show.
    pub(crate) fn as_raw(&self) -> Shape {
        let types: &[&str] = match self {
            Shape::Bool => &["boolean"],
            Shape::Int { .. } => &["integer"],
            Shape::Number { .. } => &["number"],
            Shape::Choice(_) | Shape::Asset(_) | Shape::Text => &["string"],
            Shape::Vector { .. } | Shape::Colour { .. } | Shape::List(_) => &["array"],
            Shape::Map(_) => &["object"],
            Shape::Nullable(inner) => {
                let Shape::Raw { mut types } = inner.as_raw() else {
                    unreachable!("as_raw gives Raw")
                };
                types.push("null".into());
                return Shape::Raw { types };
            }
            Shape::Raw { .. } => return self.clone(),
        };
        Shape::Raw {
            types: types.iter().map(|t| t.to_string()).collect(),
        }
    }

    /// About how many lines the widget takes for `value`.
    pub(crate) fn lines(&self, value: &Json) -> usize {
        match self {
            Shape::List(item) => match value.as_array() {
                Some(items) if items.len() > OPEN_ITEMS => 1,
                Some(items) => items.iter().map(|v| item.lines(v)).sum::<usize>() + 1,
                None => 1,
            },
            Shape::Map(values) => {
                value
                    .as_object()
                    .map_or(0, |m| m.values().map(|v| values.lines(v)).sum::<usize>())
                    + 1
            }
            _ => 1,
        }
    }

    /// About how wide the widget is, in points.
    pub(crate) fn width(&self) -> f32 {
        match self {
            Shape::Bool | Shape::Colour { .. } => 24.0,
            Shape::Int { slider: true, .. } | Shape::Number { slider: true, .. } => 170.0,
            Shape::Int { .. } | Shape::Number { .. } => 64.0,
            Shape::Choice(_) => 130.0,
            Shape::Nullable(inner) => 28.0 + inner.width(),
            Shape::Vector { len, .. } => 64.0 * *len as f32,
            Shape::Asset(_) => 170.0,
            Shape::Text => 140.0,
            Shape::Map(values) => 100.0 + values.width(),
            Shape::List(item) => 70.0 + item.width(),
            Shape::Raw { .. } => 180.0,
        }
    }

    /// A value to start from: what a list or map editor adds, and what a
    /// nullable value is set to.
    pub(crate) fn zero(&self) -> Json {
        match self {
            Shape::Bool => Json::Bool(false),
            Shape::Int { range, .. } => Json::from(0i64.clamp(range.0, range.1)),
            Shape::Number { range, .. } => Json::from(0f64.clamp(range.0, range.1)),
            Shape::Choice(variants) => variants
                .first()
                .map_or(Json::Null, |(v, _)| Json::String(v.clone())),
            Shape::Nullable(_) | Shape::Raw { .. } => Json::Null,
            Shape::Vector { len, .. } => Json::Array(vec![Json::from(0.0); *len]),
            Shape::Colour { alpha } => Json::Array(vec![Json::from(1.0); 3 + usize::from(*alpha)]),
            Shape::Asset(_) | Shape::Text => Json::String(String::new()),
            Shape::Map(_) => Json::Object(Map::new()),
            Shape::List(_) => Json::Array(Vec::new()),
        }
    }
}

/// The description of property `prop` of `root`: its own, or that of the
/// definition it refers to.
pub(crate) fn doc(prop: &Json, root: &Json) -> String {
    resolve(prop, root, 0)
        .get("description")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string()
}

/// The default of property `prop` of `root`, if its schema gives one.
pub(crate) fn default(prop: &Json, root: &Json) -> Option<Json> {
    resolve(prop, root, 0).get("default").cloned()
}

/// `prop` with a local `$ref` (or an `allOf` of one, as older generators
/// wrap it) replaced by what it names, `prop`'s own keys (its description and
/// default) kept on top. A `$ref` that names nothing is left as it is.
fn resolve<'a>(prop: &'a Json, root: &Json, depth: usize) -> Cow<'a, Json> {
    let Some(obj) = prop.as_object() else {
        return Cow::Borrowed(prop);
    };
    let target = obj.get("$ref").and_then(Json::as_str).or_else(|| {
        match obj.get("allOf").and_then(Json::as_array).map(Vec::as_slice) {
            Some([one]) => one.get("$ref").and_then(Json::as_str),
            _ => None,
        }
    });
    let Some(def) = target
        .and_then(|r| r.strip_prefix('#'))
        .and_then(|pointer| root.pointer(pointer))
    else {
        return Cow::Borrowed(prop);
    };
    if depth >= DEPTH {
        return Cow::Borrowed(prop);
    }
    let mut merged = resolve(def, root, depth + 1).into_owned();
    if let Json::Object(m) = &mut merged {
        for (k, v) in obj {
            if k != "$ref" && k != "allOf" {
                m.insert(k.clone(), v.clone());
            }
        }
    }
    Cow::Owned(merged)
}

/// The names in `p`'s `type` (one string or a list).
fn types(p: &Json) -> Vec<&str> {
    match p.get("type") {
        Some(Json::String(t)) => vec![t.as_str()],
        Some(Json::Array(ts)) => ts.iter().filter_map(Json::as_str).collect(),
        _ => Vec::new(),
    }
}

fn shape(p: &Json, root: &Json, depth: usize) -> Shape {
    let raw = || Shape::Raw {
        types: types(p).into_iter().map(str::to_string).collect(),
    };
    if depth >= DEPTH {
        return raw();
    }
    if let Some(variants) = choices(p, root, depth) {
        return Shape::Choice(variants);
    }
    if let Some(inner) = nullable(p, root, depth) {
        return Shape::Nullable(Box::new(inner));
    }
    match types(p).as_slice() {
        ["boolean"] => Shape::Bool,
        ["integer"] => {
            let (range, slider) = int_range(p);
            Shape::Int { range, slider }
        }
        ["number"] => {
            let (range, slider) = number_range(p);
            Shape::Number {
                range,
                slider,
                single: single(p),
            }
        }
        ["string"] => match p.get("x-real-asset").and_then(Json::as_str) {
            Some(kind) => Shape::Asset(kind.to_string()),
            None => Shape::Text,
        },
        ["array"] => array(p, root, depth).unwrap_or_else(raw),
        ["object"] => match p.get("additionalProperties") {
            Some(values @ Json::Object(_)) if p.get("properties").is_none() => {
                match shape(&resolve(values, root, depth + 1), root, depth + 1) {
                    Shape::Raw { .. } => raw(),
                    s => Shape::Map(Box::new(s)),
                }
            }
            _ => raw(),
        },
        _ => raw(),
    }
}

/// The variants of a `oneOf`/`anyOf` whose every alternative is a string
/// constant, or of an `enum` of strings.
fn choices(p: &Json, root: &Json, depth: usize) -> Option<Vec<(String, Option<String>)>> {
    if let Some(values) = p.get("enum").and_then(Json::as_array) {
        return values
            .iter()
            .map(|v| v.as_str().map(|s| (s.to_string(), None)))
            .collect();
    }
    let alternatives = p
        .get("oneOf")
        .or_else(|| p.get("anyOf"))
        .and_then(Json::as_array)?;
    if alternatives.is_empty() {
        return None;
    }
    alternatives
        .iter()
        .map(|a| {
            let a = resolve(a, root, depth + 1);
            let value = a.get("const")?.as_str()?.to_string();
            let doc = a
                .get("description")
                .and_then(Json::as_str)
                .map(str::to_string);
            Some((value, doc))
        })
        .collect()
}

/// The shape of what a nullable property holds when it is not `null`:
/// `type: [T, "null"]`, or an `anyOf`/`oneOf` of a schema and `null`.
fn nullable(p: &Json, root: &Json, depth: usize) -> Option<Shape> {
    let ts = types(p);
    let inner = if ts.len() == 2 && ts.contains(&"null") {
        let t = ts.iter().find(|t| **t != "null")?;
        let mut q = p.clone();
        q.as_object_mut()?.insert("type".into(), Json::from(*t));
        shape(&q, root, depth + 1)
    } else {
        let alternatives = p
            .get("anyOf")
            .or_else(|| p.get("oneOf"))
            .and_then(Json::as_array)?;
        let [a, b] = alternatives.as_slice() else {
            return None;
        };
        let is_null = |s: &Json| types(s) == ["null"];
        let other = match (is_null(a), is_null(b)) {
            (true, false) => b,
            (false, true) => a,
            _ => return None,
        };
        shape(&resolve(other, root, depth + 1), root, depth + 1)
    };
    (!matches!(inner, Shape::Raw { .. })).then_some(inner)
}

fn array(p: &Json, root: &Json, depth: usize) -> Option<Shape> {
    let items = resolve(p.get("items")?, root, depth + 1);
    let fixed = match (
        p.get("minItems").and_then(Json::as_u64),
        p.get("maxItems").and_then(Json::as_u64),
    ) {
        (Some(a), Some(b)) if a == b => Some(a as usize),
        _ => None,
    };
    let numbers = types(&items) == ["number"];
    let colour = p.get("x-real-widget").and_then(Json::as_str) == Some("colour");
    match fixed {
        Some(n @ (3 | 4)) if numbers && colour => Some(Shape::Colour { alpha: n == 4 }),
        Some(len @ (2 | 3)) if numbers => Some(Shape::Vector {
            len,
            single: single(&items),
        }),
        _ => match shape(&items, root, depth + 1) {
            Shape::Raw { .. } => None,
            s => Some(Shape::List(Box::new(s))),
        },
    }
}

fn single(p: &Json) -> bool {
    p.get("format").and_then(Json::as_str) == Some("float")
}

/// The range an integer property's drag keeps to (its `format` and bounds),
/// and whether the schema bounds both ends, which makes it a slider.
fn int_range(p: &Json) -> ((i64, i64), bool) {
    let (mut lo, mut hi) = match p.get("format").and_then(Json::as_str) {
        Some("uint8") => (0, i64::from(u8::MAX)),
        Some("uint16") => (0, i64::from(u16::MAX)),
        Some("uint32") => (0, i64::from(u32::MAX)),
        Some("uint" | "uint64" | "uint128") => (0, i64::MAX),
        Some("int8") => (i64::from(i8::MIN), i64::from(i8::MAX)),
        Some("int16") => (i64::from(i16::MIN), i64::from(i16::MAX)),
        Some("int32") => (i64::from(i32::MIN), i64::from(i32::MAX)),
        _ => (i64::MIN, i64::MAX),
    };
    let bound = |k: &str| p.get(k).and_then(Json::as_f64);
    let min = bound("minimum").or_else(|| bound("exclusiveMinimum").map(|m| m.floor() + 1.0));
    let max = bound("maximum").or_else(|| bound("exclusiveMaximum").map(|m| m.ceil() - 1.0));
    if let Some(m) = min {
        lo = lo.max(m.ceil() as i64);
    }
    if let Some(m) = max {
        hi = hi.min(m.floor() as i64);
    }
    ((lo, hi.max(lo)), min.is_some() && max.is_some())
}

/// The range a number property's drag keeps to, and whether the schema
/// bounds both ends, which makes it a slider.
fn number_range(p: &Json) -> ((f64, f64), bool) {
    let bound = |k: &str, exclusive: &str| {
        p.get(k)
            .and_then(Json::as_f64)
            .or_else(|| p.get(exclusive).and_then(Json::as_f64))
    };
    let min = bound("minimum", "exclusiveMinimum");
    let max = bound("maximum", "exclusiveMaximum");
    let lo = min.unwrap_or(f64::NEG_INFINITY);
    let hi = max.unwrap_or(f64::INFINITY).max(lo);
    ((lo, hi), min.is_some() && max.is_some())
}

/// `x` as JSON. For an `f32` parameter (`single`), the shortest decimal that
/// reads back as the same `f32`: `0.3`, never `0.30000001192092896`.
pub(crate) fn number(x: f64, single: bool) -> Json {
    let x = if single {
        (x as f32).to_string().parse::<f64>().unwrap_or(x)
    } else {
        x
    };
    serde_json::Number::from_f64(x).map_or(Json::Null, Json::Number)
}

/// Equal as JSON values, numbers compared by value (`4` and `4.0` are the
/// same parameter value).
pub(crate) fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => x.as_f64() == y.as_f64(),
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

/// True when `v` is one of `types` (JSON Schema type names; empty is any).
fn fits(types: &[String], v: &Json) -> bool {
    types.is_empty()
        || types.iter().any(|t| match t.as_str() {
            "null" => v.is_null(),
            "boolean" => v.is_boolean(),
            "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
            "number" => v.is_number(),
            "string" => v.is_string(),
            "array" => v.is_array(),
            "object" => v.is_object(),
            _ => true,
        })
}

/// What a widget did this frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Change {
    /// The value under the pointer while a drag goes on (or a picker is
    /// open), when it differs from the value shown.
    Live(Json),
    /// The widget was released, or clicked: the value to commit, once.
    Done(Json),
    /// Text that is not a value of the property: why.
    Invalid(String),
}

/// What a widget reads beside its value, and where it records its parts.
pub(crate) struct Env<'a> {
    /// The project's files of an asset kind, for asset pickers.
    assets: &'a dyn Fn(&str) -> Vec<String>,
    /// Where each part of the widget was drawn this frame, in screen points:
    /// `""` the widget itself; `"0"`, `"1"`… a vector's components, or a
    /// list's items and map's values (each with its own parts after a `/`);
    /// `"set"` a nullable value's checkbox; `"=<value>"` an item of a choice
    /// or asset picker's list; `"…"` an asset picker's list button; `"+"` a
    /// list's or map's add button; `"×<i>"` its remove buttons.
    pub(crate) parts: Vec<(String, egui::Rect)>,
    prefix: String,
}

impl<'a> Env<'a> {
    pub(crate) fn new(assets: &'a dyn Fn(&str) -> Vec<String>) -> Env<'a> {
        Env {
            assets,
            parts: Vec::new(),
            prefix: String::new(),
        }
    }

    fn part(&mut self, ui: &egui::Ui, name: &str, rect: egui::Rect) {
        let to_screen = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .unwrap_or_default();
        self.parts
            .push((format!("{}{name}", self.prefix), to_screen * rect));
    }

    /// Runs `f` with part names under `name/`.
    fn nested<R>(&mut self, name: &str, f: impl FnOnce(&mut Self) -> R) -> R {
        let outer = self.prefix.len();
        self.prefix.push_str(name);
        self.prefix.push('/');
        let r = f(self);
        self.prefix.truncate(outer);
        r
    }
}

/// A value being dragged or typed, kept under its widget's id.
#[derive(Clone, Default)]
struct Pending(Json);

/// Text being typed into a field, kept under its widget's id.
#[derive(Clone, Default)]
struct Typed(String);

fn pending(ui: &egui::Ui, id: Id) -> Option<Json> {
    ui.data(|d| d.get_temp::<Pending>(id)).map(|p| p.0)
}

/// Keeps a dragged or typed value pending while its widget is in use,
/// reporting it as live while it differs from `current`, and reports it once
/// as done when the widget is released.
fn settle(
    ui: &egui::Ui,
    id: Id,
    rs: &[egui::Response],
    value: Json,
    current: &Json,
) -> Option<Change> {
    if rs.iter().any(egui::Response::changed) {
        ui.data_mut(|d| d.insert_temp(id, Pending(value)));
    }
    // Each widget is checked on its own (a union of responses keeps only the
    // first one's id); an open popup, such as the colour picker, counts as
    // in use.
    let in_use = rs.iter().any(|r| r.dragged() || r.has_focus())
        || ui.ctx().dragged_id().is_some()
        || ui.ctx().any_popup_open()
        || ui.input(|i| i.pointer.any_down());
    if in_use {
        return pending(ui, id)
            .filter(|v| !same(v, current))
            .map(Change::Live);
    }
    let Pending(v) = ui.data_mut(|d| d.remove_temp::<Pending>(id))?;
    Some(Change::Done(v))
}

/// A drag for an integer, `speed` per point, with [`edit`]'s rules: the value
/// is pending while the drag or typing goes on, and done once, when the
/// widget is released.
pub(crate) fn drag_int(
    ui: &mut egui::Ui,
    id: Id,
    current: i64,
    speed: f64,
) -> (egui::Response, Option<Change>) {
    let mut i = pending(ui, id).and_then(|v| v.as_i64()).unwrap_or(current);
    let r = ui.add(
        egui::DragValue::new(&mut i)
            .speed(speed)
            .update_while_editing(false),
    );
    let change = settle(
        ui,
        id,
        std::slice::from_ref(&r),
        Json::from(i),
        &Json::from(current),
    );
    (r, change)
}

/// A single-line text field that returns its text when it is committed
/// (Enter or focus lost, not Escape) and differs from `current`.
fn text_field(
    ui: &mut egui::Ui,
    id: Id,
    current: &str,
    width: f32,
    env: &mut Env<'_>,
    part: &str,
) -> Option<String> {
    let mut text = ui
        .data(|d| d.get_temp::<Typed>(id))
        .map_or_else(|| current.to_string(), |t| t.0);
    let r = ui.add(
        egui::TextEdit::singleline(&mut text)
            .id(id)
            .desired_width(width),
    );
    env.part(ui, part, r.rect);
    if r.lost_focus() {
        ui.data_mut(|d| d.remove::<Typed>(id));
        let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
        return (!cancelled && text != current).then_some(text);
    }
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, Typed(text)));
    } else {
        ui.data_mut(|d| d.remove::<Typed>(id));
    }
    None
}

/// How far a drag moves a number per point: a 500th of a bounded range,
/// else a hundredth of the value's size.
fn speed(x: f64, (lo, hi): (f64, f64)) -> f64 {
    if lo.is_finite() && hi.is_finite() && hi > lo {
        return (hi - lo) / 500.0;
    }
    (x.abs() * 0.01).max(0.001)
}

fn int_speed(i: i64) -> f64 {
    (i.unsigned_abs() as f64 * 0.01).max(0.1)
}

/// The first `n` numbers of `v`, `fill` for any it lacks.
fn numbers(v: &Json, n: usize, fill: f64) -> Vec<f64> {
    let items = v.as_array().map(Vec::as_slice).unwrap_or_default();
    (0..n)
        .map(|i| items.get(i).and_then(Json::as_f64).unwrap_or(fill))
        .collect()
}

/// Draws `shape`'s widget for `value` and says what it did. `id` keeps the
/// widget's state across frames, so each property needs its own, the same
/// every frame.
pub(crate) fn edit(
    ui: &mut egui::Ui,
    id: Id,
    shape: &Shape,
    value: &Json,
    env: &mut Env<'_>,
) -> Option<Change> {
    match shape {
        Shape::Bool => {
            let mut b = value.as_bool().unwrap_or(false);
            let r = ui.checkbox(&mut b, "");
            env.part(ui, "", r.rect);
            r.changed().then_some(Change::Done(Json::Bool(b)))
        }
        Shape::Int { range, slider } => {
            let current = value
                .as_i64()
                .or_else(|| value.as_f64().map(|f| f.round() as i64))
                .unwrap_or(0);
            let mut i = pending(ui, id).and_then(|v| v.as_i64()).unwrap_or(current);
            let r = if *slider {
                ui.add(egui::Slider::new(&mut i, range.0..=range.1).update_while_editing(false))
            } else {
                ui.add(
                    egui::DragValue::new(&mut i)
                        .speed(int_speed(current))
                        .range(range.0..=range.1)
                        .update_while_editing(false),
                )
            };
            env.part(ui, "", r.rect);
            settle(ui, id, &[r], Json::from(i), value)
        }
        Shape::Number {
            range,
            slider,
            single,
        } => {
            let current = value.as_f64().unwrap_or(0.0);
            let mut x = pending(ui, id).and_then(|v| v.as_f64()).unwrap_or(current);
            let r = if *slider {
                ui.add(
                    egui::Slider::new(&mut x, range.0..=range.1)
                        .max_decimals(6)
                        .update_while_editing(false),
                )
            } else {
                ui.add(
                    egui::DragValue::new(&mut x)
                        .speed(speed(current, *range))
                        .range(range.0..=range.1)
                        .max_decimals(6)
                        .update_while_editing(false),
                )
            };
            env.part(ui, "", r.rect);
            settle(ui, id, &[r], number(x, *single), value)
        }
        Shape::Choice(variants) => {
            let current = value.as_str().unwrap_or("");
            let mut chosen = None;
            // No value: the kind's default, which its schema does not name.
            let shown = match value.as_str() {
                Some(v) => RichText::new(v),
                None => RichText::new("default").italics().weak(),
            };
            let r = egui::ComboBox::from_id_salt(id)
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for (v, doc) in variants {
                        let mut item = ui.selectable_label(v == current, v);
                        if let Some(doc) = doc {
                            item = item.on_hover_text(doc);
                        }
                        env.part(ui, &format!("={v}"), item.rect);
                        if item.clicked() && v != current {
                            chosen = Some(v.clone());
                        }
                    }
                });
            env.part(ui, "", r.response.rect);
            chosen.map(|v| Change::Done(Json::String(v)))
        }
        Shape::Nullable(inner) => {
            ui.horizontal(|ui| {
                let mut set = !value.is_null();
                let r = ui
                    .checkbox(&mut set, "")
                    .on_hover_text("Set a value, or clear to leave it unset");
                env.part(ui, "set", r.rect);
                if r.changed() {
                    return Some(Change::Done(if set { inner.zero() } else { Json::Null }));
                }
                if set {
                    edit(ui, id.with("value"), inner, value, env)
                } else {
                    ui.label(RichText::new("unset").weak());
                    None
                }
            })
            .inner
        }
        Shape::Vector { len, single } => {
            let current = numbers(value, *len, 0.0);
            let mut a = pending(ui, id).map_or_else(|| current.clone(), |p| numbers(&p, *len, 0.0));
            let rs: Vec<egui::Response> = ui
                .horizontal(|ui| {
                    a.iter_mut()
                        .zip(&current)
                        .enumerate()
                        .map(|(i, (x, c))| {
                            let r = ui.add(
                                egui::DragValue::new(x)
                                    .speed(speed(*c, (f64::NEG_INFINITY, f64::INFINITY)))
                                    .max_decimals(6)
                                    .update_while_editing(false),
                            );
                            env.part(ui, &i.to_string(), r.rect);
                            r
                        })
                        .collect()
                })
                .inner;
            let v = Json::Array(a.iter().map(|x| number(*x, *single)).collect());
            settle(ui, id, &rs, v, value)
        }
        Shape::Colour { alpha } => {
            let n = 3 + usize::from(*alpha);
            let current = numbers(value, n, 1.0);
            let shown = pending(ui, id).map_or(current, |p| numbers(&p, n, 1.0));
            let mut c: Vec<f32> = shown.iter().map(|x| *x as f32).collect();
            let r = if *alpha {
                let mut rgba = [c[0], c[1], c[2], c[3]];
                let r = ui.color_edit_button_rgba_unmultiplied(&mut rgba);
                c = rgba.to_vec();
                r
            } else {
                let mut rgb = [c[0], c[1], c[2]];
                let r = ui.color_edit_button_rgb(&mut rgb);
                c = rgb.to_vec();
                r
            };
            env.part(ui, "", r.rect);
            let v = Json::Array(c.iter().map(|x| number(f64::from(*x), true)).collect());
            settle(ui, id, &[r], v, value)
        }
        Shape::Asset(kind) => asset_picker(ui, id, kind, value.as_str().unwrap_or(""), env),
        Shape::Text => text_field(ui, id, value.as_str().unwrap_or(""), 140.0, env, "")
            .map(|t| Change::Done(Json::String(t))),
        Shape::Map(values) => map_editor(ui, id, values, value, env),
        Shape::List(item) => list_editor(ui, id, item, value, env),
        Shape::Raw { types } => {
            let shown = if value.is_null() {
                String::new()
            } else {
                value.to_string()
            };
            let text = text_field(ui, id, &shown, 180.0, env, "")?;
            Some(match serde_json::from_str::<Json>(&text) {
                Ok(v) if fits(types, &v) => Change::Done(v),
                Ok(_) => Change::Invalid(format!(
                    "`{text}` is not {}",
                    types
                        .iter()
                        .map(|t| format!("a JSON {t}"))
                        .collect::<Vec<_>>()
                        .join(" or ")
                )),
                Err(e) => Change::Invalid(format!("`{text}` is not JSON: {e}")),
            })
        }
    }
}

/// The asset files last listed for each kind, and when.
#[derive(Clone, Default)]
struct AssetLists(HashMap<String, (f64, Arc<[String]>)>);

/// The project's files of `kind`, listed again when the last list is older
/// than [`ASSET_LIST_AGE`].
fn asset_list(ui: &egui::Ui, kind: &str, env: &Env<'_>) -> Arc<[String]> {
    let id = Id::new("re_editor.asset_lists");
    let now = ui.input(|i| i.time);
    let cached = ui.data(|d| {
        d.get_temp::<AssetLists>(id)
            .and_then(|l| l.0.get(kind).cloned())
    });
    if let Some((at, files)) = cached
        && now - at < ASSET_LIST_AGE
    {
        return files;
    }
    let files: Arc<[String]> = (env.assets)(kind).into();
    ui.data_mut(|d| {
        d.get_temp_mut_or_default::<AssetLists>(id)
            .0
            .insert(kind.to_string(), (now, files.clone()));
    });
    files
}

/// A path field with a searchable list of the project's files of `kind`.
fn asset_picker(
    ui: &mut egui::Ui,
    id: Id,
    kind: &str,
    current: &str,
    env: &mut Env<'_>,
) -> Option<Change> {
    ui.horizontal(|ui| {
        let mut out =
            text_field(ui, id, current, 140.0, env, "").map(|t| Change::Done(Json::String(t)));
        let search_id = id.with("search");
        let r = ui.menu_button("…", |ui| {
            let mut search = ui
                .data(|d| d.get_temp::<Typed>(search_id))
                .map(|t| t.0)
                .unwrap_or_default();
            ui.add(
                egui::TextEdit::singleline(&mut search).hint_text(format!("search {kind} files")),
            );
            let files = asset_list(ui, kind, env);
            let needle = search.to_lowercase();
            egui::ScrollArea::vertical()
                .id_salt(id.with("files"))
                .max_height(240.0)
                .show(ui, |ui| {
                    if ui.selectable_label(current.is_empty(), "(none)").clicked() {
                        out = Some(Change::Done(Json::String(String::new())));
                        ui.close();
                    }
                    for f in files.iter().filter(|f| f.to_lowercase().contains(&needle)) {
                        let b = ui.selectable_label(f == current, f.as_str());
                        env.part(ui, &format!("={f}"), b.rect);
                        if b.clicked() {
                            out = Some(Change::Done(Json::String(f.clone())));
                            ui.close();
                        }
                    }
                    if files.is_empty() {
                        ui.label(RichText::new(format!("No {kind} files in the project.")).weak());
                    }
                });
            ui.data_mut(|d| d.insert_temp(search_id, Typed(search)));
        });
        env.part(ui, "…", r.response.rect);
        r.response.on_hover_text(format!("Choose a {kind} file"));
        out.filter(|c| !matches!(c, Change::Done(Json::String(s)) if s == current))
    })
    .inner
}

/// A widget per item, each with remove and reorder buttons, and one to add
/// an item; any change is the whole list.
fn list_editor(
    ui: &mut egui::Ui,
    id: Id,
    item: &Shape,
    value: &Json,
    env: &mut Env<'_>,
) -> Option<Change> {
    let items = value.as_array().cloned().unwrap_or_default();
    let with = |i: usize, v: Json| {
        let mut all = items.clone();
        all[i] = v;
        Json::Array(all)
    };
    let mut out = None;
    let body = |ui: &mut egui::Ui, env: &mut Env<'_>, out: &mut Option<Change>| {
        for (i, v) in items.iter().enumerate() {
            ui.horizontal(|ui| {
                let changed = env.nested(&i.to_string(), |env| edit(ui, id.with(i), item, v, env));
                match changed {
                    Some(Change::Live(v)) => *out = Some(Change::Live(with(i, v))),
                    Some(Change::Done(v)) => *out = Some(Change::Done(with(i, v))),
                    Some(invalid @ Change::Invalid(_)) => *out = Some(invalid),
                    None => {}
                }
                let mut moved = |to: usize| {
                    let mut all = items.clone();
                    all.swap(i, to);
                    *out = Some(Change::Done(Json::Array(all)));
                };
                if i > 0 && ui.small_button("⏶").on_hover_text("Move up").clicked() {
                    moved(i - 1);
                }
                if i + 1 < items.len() && ui.small_button("⏷").on_hover_text("Move down").clicked()
                {
                    moved(i + 1);
                }
                let rm = ui.small_button("×").on_hover_text("Remove this item");
                env.part(ui, &format!("×{i}"), rm.rect);
                if rm.clicked() {
                    let mut all = items.clone();
                    all.remove(i);
                    *out = Some(Change::Done(Json::Array(all)));
                }
            });
        }
        let add = ui.small_button("+").on_hover_text("Add an item");
        env.part(ui, "+", add.rect);
        if add.clicked() {
            let mut all = items.clone();
            all.push(item.zero());
            *out = Some(Change::Done(Json::Array(all)));
        }
    };
    if items.len() > OPEN_ITEMS {
        egui::CollapsingHeader::new(format!("{} items", items.len()))
            .id_salt(id.with("open"))
            .show(ui, |ui| body(ui, env, &mut out));
    } else {
        ui.vertical(|ui| body(ui, env, &mut out));
    }
    out
}

/// A key and a value per entry, each with a remove button, and one to add an
/// entry; any change is the whole object, its order kept.
fn map_editor(
    ui: &mut egui::Ui,
    id: Id,
    values: &Shape,
    value: &Json,
    env: &mut Env<'_>,
) -> Option<Change> {
    let map = value.as_object().cloned().unwrap_or_default();
    let mut out = None;
    ui.vertical(|ui| {
        for (i, (k, v)) in map.iter().enumerate() {
            ui.horizontal(|ui| {
                let key = env.nested(&i.to_string(), |env| {
                    text_field(ui, id.with(("key", k)), k, 70.0, env, "key")
                });
                if let Some(new) = key.map(|t| t.trim().to_string())
                    && !new.is_empty()
                    && !map.contains_key(&new)
                {
                    let renamed = map
                        .iter()
                        .map(|(key, v)| {
                            (if key == k { new.clone() } else { key.clone() }, v.clone())
                        })
                        .collect();
                    out = Some(Change::Done(Json::Object(renamed)));
                }
                let changed = env.nested(&i.to_string(), |env| {
                    edit(ui, id.with(("value", k)), values, v, env)
                });
                let with = |v: Json| {
                    let mut all = map.clone();
                    all.insert(k.clone(), v);
                    Json::Object(all)
                };
                match changed {
                    Some(Change::Live(v)) => out = Some(Change::Live(with(v))),
                    Some(Change::Done(v)) => out = Some(Change::Done(with(v))),
                    Some(invalid @ Change::Invalid(_)) => out = Some(invalid),
                    None => {}
                }
                let rm = ui.small_button("×").on_hover_text("Remove this entry");
                env.part(ui, &format!("×{i}"), rm.rect);
                if rm.clicked() {
                    let mut all = map.clone();
                    all.shift_remove(k);
                    out = Some(Change::Done(Json::Object(all)));
                }
            });
        }
        let add = ui.small_button("+").on_hover_text("Add an entry");
        env.part(ui, "+", add.rect);
        if add.clicked() {
            let key = (1..)
                .map(|n| {
                    if n == 1 {
                        "key".to_string()
                    } else {
                        format!("key{n}")
                    }
                })
                .find(|k| !map.contains_key(k))
                .expect("a free key");
            let mut all = map.clone();
            all.insert(key, values.zero());
            out = Some(Change::Done(Json::Object(all)));
        }
    });
    out
}

/// The last live edit sent for a widget: the drag's coalesce key, when it
/// was sent and its value.
#[derive(Clone, Default)]
struct Sent {
    key: String,
    at: f64,
    value: Json,
}

/// What to send for a widget's change, as the value and the coalesce key
/// that makes a drag's edits one undo step, or `None` for nothing.
///
/// A live value is sent only when `live` (the command takes a coalesce key),
/// at most every [`LIVE_INTERVAL`] seconds, and never twice; a finished edit
/// is sent when it differs from `current`, under the drag's key when live
/// edits went before it.
pub(crate) fn to_send(
    ui: &egui::Ui,
    id: Id,
    change: Change,
    current: &Json,
    live: bool,
) -> Option<(Json, Option<String>)> {
    let sent_id = id.with("sent");
    let now = ui.input(|i| i.time);
    match change {
        Change::Live(v) => {
            if !live {
                return None;
            }
            let last = ui.data(|d| d.get_temp::<Sent>(sent_id));
            if let Some(s) = &last
                && (same(&s.value, &v) || now - s.at < LIVE_INTERVAL)
            {
                return None;
            }
            let key = last.map_or_else(|| format!("param {:016x} {now}", id.value()), |s| s.key);
            ui.data_mut(|d| {
                d.insert_temp(
                    sent_id,
                    Sent {
                        key: key.clone(),
                        at: now,
                        value: v.clone(),
                    },
                );
            });
            Some((v, Some(key)))
        }
        Change::Done(v) => {
            let last = ui.data_mut(|d| d.remove_temp::<Sent>(sent_id));
            (!same(&v, current)).then(|| (v, last.map(|s| s.key)))
        }
        Change::Invalid(_) => None,
    }
}

/// `RE8850`: text typed into a JSON box that is not a value of its property,
/// so `what` (a parameter, an override) is unchanged.
pub(crate) fn invalid_text(why: &str, what: &str) -> Diagnostic {
    Diagnostic::warning(codes::PARAM_VALUE, format!("{why}, so {what} is unchanged")).with_hint(
        "type a JSON value of the parameter's type (its description says what it takes): a \
         number, true or false, a string in quotes, a list in [ ] or an object in { }",
    )
}

/// `RE8850`: `what` (a parameter, an override) holds `value`, which its
/// widget cannot show, so it is shown as JSON text.
pub(crate) fn misfit(what: &str, value: &Json, shape: &Shape) -> Diagnostic {
    let expects = match shape.as_raw() {
        Shape::Raw { types } if !types.is_empty() => types.join(" or "),
        _ => "another value".to_string(),
    };
    let mut text = value.to_string();
    if text.chars().count() > 40 {
        text = text.chars().take(39).chain(['…']).collect();
    }
    Diagnostic::warning(
        codes::PARAM_VALUE,
        format!(
            "{what} is {text}, which is not of its type ({expects}), so it is shown as JSON text"
        ),
    )
    .with_hint("set a value of its type, or remove it so the node kind's default applies")
}

/// The file suffixes of an asset kind (ADR-0064 §7); none for `clip` and
/// `skin`, which name a model's animations and skins (`<model>#<name>`).
pub(crate) fn asset_suffixes(kind: &str) -> &'static [&'static str] {
    match kind {
        "material" => &[".material.json"],
        "prefab" => &[".prefab.json"],
        "scene" => &[".scene.json"],
        "graph" => &[".graph.json"],
        "animgraph" => &[".animgraph.json"],
        "script" => &[".luau"],
        "texture" => &[".png", ".jpg", ".jpeg"],
        "model" => &[".gltf", ".glb"],
        "audio" => &[".wav", ".ogg"],
        _ => &[],
    }
}

/// The project's files of an asset kind, sorted, without the engine's own
/// folder (`.real/`) or hidden folders.
pub(crate) fn project_assets(files: &re_core::ProjectFiles, kind: &str) -> Vec<String> {
    let mut out: Vec<String> = asset_suffixes(kind)
        .iter()
        .flat_map(|suffix| files.list("", suffix))
        .map(|p| p.as_str().to_string())
        .filter(|p| !p.split('/').any(|segment| segment.starts_with('.')))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// True when the command `spec` takes a `coalesce` key.
pub(crate) fn takes_coalesce(spec: Option<&re_command::CommandSpec>) -> bool {
    spec.and_then(|s| s.input_schema.get("properties"))
        .and_then(|p| p.get("coalesce"))
        .is_some()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn of(prop: Json) -> Shape {
        Shape::of(&prop, &json!({}))
    }

    #[test]
    fn each_kind_of_property_gets_its_widget() {
        assert_eq!(of(json!({"type": "boolean"})), Shape::Bool);
        assert_eq!(
            of(json!({"type": "integer", "format": "uint32", "minimum": 0})),
            Shape::Int {
                range: (0, i64::from(u32::MAX)),
                slider: false
            }
        );
        assert_eq!(
            of(json!({"type": "integer", "minimum": 1, "maximum": 16})),
            Shape::Int {
                range: (1, 16),
                slider: true
            }
        );
        assert_eq!(
            of(json!({"type": "number", "format": "float"})),
            Shape::Number {
                range: (f64::NEG_INFINITY, f64::INFINITY),
                slider: false,
                single: true
            }
        );
        assert_eq!(
            of(json!({"type": "number", "format": "double", "minimum": 0.0, "maximum": 1.0})),
            Shape::Number {
                range: (0.0, 1.0),
                slider: true,
                single: false
            }
        );
        assert_eq!(
            of(json!({"type": ["number", "null"], "format": "double", "default": null})),
            Shape::Nullable(Box::new(Shape::Number {
                range: (f64::NEG_INFINITY, f64::INFINITY),
                slider: false,
                single: false
            }))
        );
        let two = json!({"type": "array", "items": {"type": "number", "format": "float"}, "minItems": 2, "maxItems": 2});
        assert_eq!(
            of(two.clone()),
            Shape::Vector {
                len: 2,
                single: true
            }
        );
        let colour = json!({"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4, "x-real-widget": "colour"});
        assert_eq!(of(colour), Shape::Colour { alpha: true });
        assert_eq!(
            of(json!({"type": "array", "items": two})),
            Shape::List(Box::new(Shape::Vector {
                len: 2,
                single: true
            }))
        );
        assert_eq!(
            of(json!({"type": "string", "x-real-asset": "material"})),
            Shape::Asset("material".into())
        );
        assert_eq!(of(json!({"type": "string"})), Shape::Text);
        assert_eq!(
            of(
                json!({"type": "object", "additionalProperties": {"type": "string", "x-real-asset": "prefab"}})
            ),
            Shape::Map(Box::new(Shape::Asset("prefab".into())))
        );
        assert_eq!(
            of(json!({"enum": ["a", "b"]})),
            Shape::Choice(vec![("a".into(), None), ("b".into(), None)])
        );
        // A list of structs, and `anyOf` alternatives that are not constants,
        // are JSON text.
        assert_eq!(
            of(
                json!({"type": "array", "items": {"type": "object", "properties": {"tag": {"type": "string"}}}})
            ),
            Shape::Raw {
                types: vec!["array".into()]
            }
        );
        assert_eq!(
            of(json!({"anyOf": [{"type": "array"}, {"type": "object"}]})),
            Shape::Raw { types: vec![] }
        );
    }

    #[test]
    fn a_ref_is_resolved_and_keeps_the_property_description() {
        let root = json!({"$defs": {"Noise": {
            "description": "The noise.",
            "oneOf": [
                {"description": "Rolling hills.", "type": "string", "const": "fbm"},
                {"description": "Sharp crests.", "type": "string", "const": "ridged"}
            ]
        }}});
        let prop =
            json!({"description": "Which noise.", "$ref": "#/$defs/Noise", "default": "fbm"});
        assert_eq!(
            Shape::of(&prop, &root),
            Shape::Choice(vec![
                ("fbm".into(), Some("Rolling hills.".into())),
                ("ridged".into(), Some("Sharp crests.".into()))
            ])
        );
        assert_eq!(doc(&prop, &root), "Which noise.");
        assert_eq!(default(&prop, &root), Some(json!("fbm")));
        // A ref to nothing, and a ref to itself, end.
        assert_eq!(
            Shape::of(&json!({"$ref": "#/$defs/Missing"}), &root),
            Shape::Raw { types: vec![] }
        );
        let looping = json!({"$defs": {"A": {"$ref": "#/$defs/A"}}});
        assert_eq!(
            Shape::of(&json!({"$ref": "#/$defs/A"}), &looping),
            Shape::Raw { types: vec![] }
        );
    }

    #[test]
    fn every_parameter_of_every_built_in_kind_has_a_widget() {
        let registry = re_proc::NodeRegistry::m1();
        let mut raw = Vec::new();
        for def in registry.iter() {
            let schema = &def.params_schema;
            let props = schema.get("properties").and_then(Json::as_object);
            for (name, prop) in props.into_iter().flatten() {
                let shape = Shape::of(prop, schema);
                assert!(
                    !doc(prop, schema).is_empty(),
                    "{}.{name} has no description",
                    def.kind
                );
                if matches!(shape, Shape::Raw { .. }) {
                    raw.push(format!("{}.{name}", def.kind));
                }
            }
        }
        // Only these are edited as JSON text: a list of structs and a list of
        // points that are each a position or a record (`path.spline` gets
        // handles in the viewport instead).
        assert_eq!(raw, ["points.pick.choices", "path.spline.points"]);
        let noise = &registry.get("terrain.heightfield").unwrap().params_schema;
        assert!(matches!(
            Shape::of(&noise["properties"]["noise"], noise),
            Shape::Choice(v) if v.len() == 3
        ));
    }

    #[test]
    fn a_single_precision_number_is_its_shortest_decimal() {
        assert_eq!(number(0.30000001192092896, true).to_string(), "0.3");
        assert_eq!(number(f64::from(0.01f32), true).to_string(), "0.01");
        assert_eq!(number(0.1 + 0.2, false).to_string(), "0.30000000000000004");
        assert_eq!(number(f64::NAN, false), Json::Null);
        assert!(same(&json!(4), &json!(4.0)));
        assert!(same(&json!({"a": [1, 2.0]}), &json!({"a": [1.0, 2]})));
        assert!(!same(&json!([1]), &json!([1, 2])));
    }

    #[test]
    fn json_text_is_checked_against_the_property_type() {
        assert!(fits(&["array".into()], &json!([1])));
        assert!(!fits(&["array".into()], &json!(1)));
        assert!(fits(&["integer".into()], &json!(3.0)));
        assert!(!fits(&["integer".into()], &json!(3.5)));
        assert!(fits(&[], &json!(null)));
    }
}
