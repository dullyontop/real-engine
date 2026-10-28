//! The procedural panel: each procedural source's seed and regeneration, and
//! its graph parameter overrides (ADR-0044 amendment §3).
//!
//! The seed scrubber shows the seed (drag it, or click it and type one), ◀
//! and ▶ for the seed before and after, "New seed" (a fixed scramble, so a
//! session's seeds are reproducible) and a strip that changes the seed by one
//! for every [`SCRUB`] points the pointer is dragged across it, showing the
//! seed under the pointer. A seed is sent once, when the pointer is released
//! or a button clicked: one `graph.regenerate {source, seed}`, so one undo
//! step.
//!
//! A graph source's overrides (`GraphSource.params`) are listed with the
//! widget their node kind's schema gives each parameter
//! ([`crate::widgets::schema`]), each with × to remove it. An override whose
//! node or parameter the graph does not have is listed in red with "Remove",
//! which writes the overrides without it (`graph.set_param` refuses to name
//! such a parameter, even to remove it). One is added with a picker: one of
//! the graph's nodes, then one of its kind's parameters the source does not
//! override yet, then the parameter's widget, starting from the value the
//! source uses now; "Set" sends `graph.set_param`. Builder sources work the
//! same, on the graph their script built.
//!
//! A source's structural overrides (moved, replaced, inserted) and deletion
//! overrides are listed by key (ADR-0044 amendment §8), with "Clear"
//! (`proc.clear_structure {source, key}`) for a key's structural ones and
//! "Restore" (`proc.revert {source, key}`) for a deletion: one command, so
//! one undo step, each.

use std::collections::HashSet;

use egui::{Color32, Id, RichText};
use re_app::App;
use re_core::Location;
use re_proc::{GraphStore, NodeDef, NodeRegistry};
use re_reflect::{FieldType, Value};
use re_scene::structure::Replacement;
use re_scene::{Entity, OverrideDoc, Overrides, World};
use re_types::graph::GraphDoc;
use serde_json::{Map, Value as Json, json};

use crate::editor::{Action, Editor, Pending, command};
use crate::widgets::schema::{self, Change, Env, Shape};

/// Points the pointer is dragged across a seed strip per seed.
const SCRUB: f32 = 8.0;
/// A seed strip's least and greatest width, in points.
const STRIP: (f32, f32) = (120.0, 360.0);

/// A place in the Procedural panel, in egui points, as drawn in the last
/// frame: for tests and tools that drive the panel with the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProceduralSpot<'a> {
    /// A source's seed: drag it, or click it and type a seed.
    Seed(&'a str),
    /// A button by a source's seed, by its text: `"◀"`, `"▶"`, `"New seed"`
    /// or `"Regenerate"`.
    SeedButton(&'a str, &'a str),
    /// A source's seed strip: drag across it to scrub the seed.
    Strip(&'a str),
    /// A part of one of a source's overrides: `(source, node, parameter,
    /// part)`, where the part is `"name"`, `"×"` (removes the override),
    /// `"Remove"` (for an override the graph does not have), or a part of the
    /// parameter's widget, as [`CanvasSpot::Param`] names them.
    ///
    /// [`CanvasSpot::Param`]: crate::CanvasSpot::Param
    Override(&'a str, &'a str, &'a str, &'a str),
    /// A part of a source's override picker: `"node"` and `"param"` (its
    /// lists), `"node=<id>"` and `"param=<name>"` (their items, while one is
    /// open), `"Set"`, and `"value"`, the value's widget, with its parts
    /// after a slash (`"value/=ridged"`).
    Picker(&'a str, &'a str),
    /// A button by one key of a source's structural and deletion overrides:
    /// `(source, key, button)`, where the button is `"Clear"` (the key's
    /// structural overrides) or `"Restore"` (its deletion).
    Kept(&'a str, &'a str, &'a str),
}

impl ProceduralSpot<'_> {
    fn key(self) -> String {
        match self {
            ProceduralSpot::Seed(s) => format!("proc\u{1f}seed\u{1f}{s}"),
            ProceduralSpot::SeedButton(s, b) => format!("proc\u{1f}button\u{1f}{s}\u{1f}{b}"),
            ProceduralSpot::Strip(s) => format!("proc\u{1f}strip\u{1f}{s}"),
            ProceduralSpot::Override(s, n, p, part) => {
                format!("proc\u{1f}override\u{1f}{s}\u{1f}{n}\u{1f}{p}\u{1f}{part}")
            }
            ProceduralSpot::Picker(s, part) => format!("proc\u{1f}picker\u{1f}{s}\u{1f}{part}"),
            ProceduralSpot::Kept(s, k, b) => format!("proc\u{1f}kept\u{1f}{s}\u{1f}{k}\u{1f}{b}"),
        }
    }
}

impl Editor {
    /// Where the Procedural panel drew `spot` in the last frame, in egui
    /// points.
    pub fn procedural_spot(&self, spot: ProceduralSpot<'_>) -> Option<egui::Rect> {
        self.pending
            .rects
            .get(&spot.key())
            .and_then(|r| r.first().copied())
    }
}

fn record(pending: &mut Pending, spot: ProceduralSpot<'_>, rect: egui::Rect) {
    pending.rects.insert(spot.key(), vec![rect]);
}

pub(super) fn graph(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    ui.heading("Graph parameters");
    let world = app.world();
    let registry = &world.registry;
    let (Some(proc_key), graph_key) = (
        registry.try_key("Procedural"),
        registry.try_key("GraphSource"),
    ) else {
        return;
    };
    let mut sources: Vec<(&str, Entity)> = world
        .ids()
        .iter()
        .filter(|(_, e)| registry.has(&world.ecs, *e, proc_key))
        .collect();
    sources.sort();
    if sources.is_empty() {
        ui.label(RichText::new("No procedural sources in this scene.").weak());
        return;
    }
    let has_regenerate = app.commands().spec("graph.regenerate").is_some();
    let files = app.files().clone();
    let assets = move |kind: &str| schema::project_assets(&files, kind);
    for (id, e) in sources {
        let generator = match world.get(id, "Procedural", "generator") {
            Ok(Value::Str(s)) => s,
            _ => String::new(),
        };
        let seed = match world.get(id, "Procedural", "seed") {
            Ok(Value::Int(s)) => s,
            _ => 0,
        };
        let header = format!("{}  ({generator})", display(world, e, id));
        egui::CollapsingHeader::new(header)
            .id_salt(("re_editor.graph", id))
            .default_open(true)
            .show(ui, |ui| {
                match seed_rows(&mut ed.pending, ui, id, seed, has_regenerate) {
                    Some(SeedSend::Seed(s)) if s != seed => {
                        actions.push(set_seed(id, s, has_regenerate));
                    }
                    Some(SeedSend::Again) => {
                        actions.push(command("graph.regenerate", json!({ "source": id })));
                    }
                    _ => {}
                }
                if let Some(gk) = graph_key
                    && registry.has(&world.ecs, e, gk)
                {
                    graph_params(ed, ui, app, id, &assets, actions);
                }
                if let Some(overrides) = world.ecs.get::<Overrides>(e) {
                    kept_rows(&mut ed.pending, ui, app, id, overrides, actions);
                }
            });
    }
}

/// One key of a source's structural and deletion overrides, as the panel
/// lists it.
#[derive(Debug, PartialEq)]
struct Kept {
    key: String,
    /// What its overrides do, in the order of the source's list.
    what: Vec<String>,
    /// It has a structural override ("Clear").
    structural: bool,
    /// It has a deletion override ("Restore").
    deleted: bool,
}

/// The keys of `overrides`' structural and deletion overrides, in the order
/// the list first names each.
fn kept(overrides: &Overrides) -> Vec<Kept> {
    let under = |parent: Option<&str>| match parent {
        Some(p) => format!("under `{p}`"),
        None => "under the source".to_string(),
    };
    let with = |r: &Replacement| match r {
        Replacement::Prefab(p) => p.clone(),
        Replacement::Components(c) if c.is_empty() => "no components".to_string(),
        Replacement::Components(c) => c.keys().map(String::as_str).collect::<Vec<_>>().join(", "),
    };
    let mut out: Vec<Kept> = Vec::new();
    for o in &overrides.0 {
        let what = match o {
            OverrideDoc::Field { .. } => continue,
            OverrideDoc::Deleted { .. } => "deleted".to_string(),
            OverrideDoc::Moved { to, .. } => match to.index {
                Some(i) => format!("moved {} at {i}", under(to.parent.as_deref())),
                None => format!("moved {}", under(to.parent.as_deref())),
            },
            OverrideDoc::Replaced { replace, .. } if replace.keep_children => {
                format!(
                    "replaced with {}, keeping its children",
                    with(&replace.with)
                )
            }
            OverrideDoc::Replaced { replace, .. } => {
                format!("replaced with {}", with(&replace.with))
            }
            OverrideDoc::Inserted { insert, .. } => format!(
                "inserted {}: {}",
                under(insert.parent.as_deref()),
                with(&insert.with)
            ),
        };
        let key = o.target();
        let at = match out.iter().position(|k| k.key == key) {
            Some(at) => at,
            None => {
                out.push(Kept {
                    key: key.to_string(),
                    what: Vec::new(),
                    structural: false,
                    deleted: false,
                });
                out.len() - 1
            }
        };
        let k = &mut out[at];
        k.what.push(what);
        if matches!(o, OverrideDoc::Deleted { .. }) {
            k.deleted = true;
        } else {
            k.structural = true;
        }
    }
    out
}

/// Source `id`'s structural and deletion overrides, by key, with "Clear"
/// and "Restore"; nothing when it keeps none.
fn kept_rows(
    pending: &mut Pending,
    ui: &mut egui::Ui,
    app: &App,
    id: &str,
    overrides: &Overrides,
    actions: &mut Vec<Action>,
) {
    let kept = kept(overrides);
    if kept.is_empty() {
        return;
    }
    let can_clear = app.commands().spec("proc.clear_structure").is_some();
    let can_restore = app.commands().spec("proc.revert").is_some();
    egui::CollapsingHeader::new(format!("Structure and deletions ({})", kept.len()))
        .id_salt(("re_editor.kept", id))
        .default_open(true)
        .show(ui, |ui| {
            for k in &kept {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&k.key).monospace());
                    ui.label(k.what.join(", "));
                    let mut button = |ui: &mut egui::Ui, text, can: bool, hover, name: &str| {
                        let r = ui
                            .add_enabled(can, egui::Button::new(text))
                            .on_hover_text(hover)
                            .on_disabled_hover_text(format!(
                                "{name} is not registered in this build"
                            ));
                        record(pending, ProceduralSpot::Kept(id, &k.key, text), r.rect);
                        if r.clicked() {
                            actions.push(command(name, json!({ "source": id, "key": k.key })));
                        }
                    };
                    if k.structural {
                        let hover = "Remove this key's structural overrides (proc.clear_structure)";
                        button(ui, "Clear", can_clear, hover, "proc.clear_structure");
                    }
                    if k.deleted {
                        let hover =
                            "Bring the entity back as the source generates it (proc.revert)";
                        button(ui, "Restore", can_restore, hover, "proc.revert");
                    }
                });
            }
        });
}

/// What the procedural tools draw over the viewport and the pointer input
/// they take there (WP43), after the gizmo. Returns true when they took the
/// pointer this frame, so the viewport does not also act on it. Nothing yet.
pub(super) fn viewport_overlay(
    _ed: &mut Editor,
    _ui: &mut egui::Ui,
    _app: &App,
    _view: &crate::camera::View,
    _actions: &mut Vec<Action>,
) -> bool {
    false
}

fn display(world: &World, e: Entity, id: &str) -> String {
    let name = world.name_of(e);
    if name.is_empty() {
        id.to_string()
    } else {
        format!("{name} {id}")
    }
}

/// The next seed of the "New seed" button: a fixed scramble of the current
/// one, so a session's seeds are reproducible.
fn next_seed(seed: i64) -> i64 {
    let mut z = (seed as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    ((z ^ (z >> 31)) >> 33) as i64
}

/// Sends source `id`'s new seed: `graph.regenerate`, or `entity.set` on
/// `Procedural.seed` when the command is not registered.
fn set_seed(id: &str, seed: i64, has_regenerate: bool) -> Action {
    if has_regenerate {
        return command("graph.regenerate", json!({ "source": id, "seed": seed }));
    }
    Action::SetField {
        id: id.to_string(),
        component: "Procedural".to_string(),
        field: "seed".to_string(),
        ty: FieldType::I64,
        value: Value::Int(seed),
    }
}

/// A drag across a seed strip: the seed when it began, where it began and how
/// far the pointer has moved since, in points.
#[derive(Clone, Copy, Debug, Default)]
struct Scrub {
    from: i64,
    origin: f32,
    dx: f32,
}

impl Scrub {
    /// The seed under the pointer.
    fn seed(self) -> i64 {
        self.from.saturating_add((self.dx / SCRUB).trunc() as i64)
    }
}

/// What the seed rows of a source send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeedSend {
    /// This seed.
    Seed(i64),
    /// The generator again, with the seed the source has ("Regenerate").
    Again,
}

/// The seed scrubber of source `id` (and "Regenerate"), which returns what
/// to send, when a button was clicked or the pointer released.
fn seed_rows(
    pending: &mut Pending,
    ui: &mut egui::Ui,
    id: &str,
    seed: i64,
    has_regenerate: bool,
) -> Option<SeedSend> {
    let strip_id = Id::new(("re_editor.scrub", id));
    let scrub = ui.data(|d| d.get_temp::<Scrub>(strip_id));
    let mut send = None;
    ui.horizontal_wrapped(|ui| {
        ui.label("seed");
        let button = |ui: &mut egui::Ui, pending: &mut Pending, text: &str, hover: &str| {
            let r = ui.button(text).on_hover_text(hover);
            record(pending, ProceduralSpot::SeedButton(id, text), r.rect);
            r.clicked()
        };
        if button(ui, pending, "◀", "Regenerate with the seed before") {
            send = Some(SeedSend::Seed(seed.saturating_sub(1)));
        }
        let shown = scrub.map_or(seed, Scrub::seed);
        let (r, change) = schema::drag_int(
            ui,
            Id::new(("re_editor.seed", id)),
            shown,
            f64::from(1.0 / SCRUB),
        );
        let r = r.on_hover_text("The source's seed: drag it, or click it and type one");
        record(pending, ProceduralSpot::Seed(id), r.rect);
        if let Some(Change::Done(v)) = change {
            send = v.as_i64().map(SeedSend::Seed);
        }
        if button(ui, pending, "▶", "Regenerate with the seed after") {
            send = Some(SeedSend::Seed(seed.saturating_add(1)));
        }
        let hover = "Regenerate with the next seed of a fixed sequence";
        if button(ui, pending, "New seed", hover) {
            send = Some(SeedSend::Seed(next_seed(seed)));
        }
        let regenerate = ui
            .add_enabled(has_regenerate, egui::Button::new("Regenerate"))
            .on_hover_text("Run the generator again now (graph.regenerate)")
            .on_disabled_hover_text(
                "graph.regenerate is not registered in this build; changing the seed or a \
                 parameter regenerates the source",
            );
        record(
            pending,
            ProceduralSpot::SeedButton(id, "Regenerate"),
            regenerate.rect,
        );
        if regenerate.clicked() {
            send = Some(SeedSend::Again);
        }
    });

    // The strip: one seed for every `SCRUB` points dragged across it.
    let size = egui::vec2(
        ui.available_width().clamp(STRIP.0, STRIP.1),
        ui.spacing().interact_size.y,
    );
    let (rect, r) = ui.allocate_exact_size(size, egui::Sense::drag());
    record(pending, ProceduralSpot::Strip(id), rect);
    let mut state = scrub;
    if r.drag_started() {
        let origin = ui
            .input(|i| i.pointer.press_origin())
            .or(r.interact_pointer_pos())
            .map_or(rect.center().x, |p| p.x);
        state = Some(Scrub {
            from: seed,
            origin,
            dx: 0.0,
        });
    }
    if let (Some(s), Some(p)) = (&mut state, r.interact_pointer_pos()) {
        s.dx = p.x - s.origin;
    }
    if r.drag_stopped() {
        send = state.take().map(|s| SeedSend::Seed(s.seed()));
    }
    match state {
        Some(s) if r.dragged() => {
            ui.data_mut(|d| d.insert_temp(strip_id, s));
        }
        _ => {
            state = None;
            ui.data_mut(|d| d.remove::<Scrub>(strip_id));
        }
    }
    paint_strip(ui, rect, &r, state);
    r.on_hover_text(format!(
        "Drag across to scrub the seed: one seed every {SCRUB} points, sent when released"
    ));
    send
}

/// Draws a seed strip: ticks a seed apart that move with the drag, and the
/// seed under the pointer while it is dragged.
fn paint_strip(ui: &egui::Ui, rect: egui::Rect, r: &egui::Response, state: Option<Scrub>) {
    let visuals = ui.style().interact(r);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, visuals.corner_radius, ui.visuals().extreme_bg_color);
    let stroke = ui.visuals().widgets.noninteractive.fg_stroke;
    let offset = state.map_or(0.0, |s| s.dx.rem_euclid(SCRUB));
    let mut x = rect.left() + offset;
    while x <= rect.right() {
        let y = rect.center().y;
        painter.vline(x, (y - 3.0)..=(y + 3.0), stroke);
        x += SCRUB;
    }
    let font = egui::FontId::monospace(12.0);
    match state {
        Some(s) => {
            let text = s.seed().to_string();
            let at = egui::pos2(
                (s.origin + s.dx).clamp(rect.left() + 20.0, rect.right() - 20.0),
                rect.center().y,
            );
            let galley = painter.layout_no_wrap(text, font, visuals.text_color());
            let label = egui::Rect::from_center_size(at, galley.size() + egui::vec2(6.0, 0.0));
            painter.rect_filled(label, 2.0, ui.visuals().extreme_bg_color);
            painter.galley(
                label.min + egui::vec2(3.0, 0.0),
                galley,
                visuals.text_color(),
            );
        }
        None => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "drag to scrub the seed",
                font,
                ui.visuals().weak_text_color(),
            );
        }
    }
}

/// The parameter overrides (`GraphSource.params`) of source `id` and its
/// picker.
fn graph_params(
    ed: &mut Editor,
    ui: &mut egui::Ui,
    app: &App,
    id: &str,
    assets: &dyn Fn(&str) -> Vec<String>,
    actions: &mut Vec<Action>,
) {
    let world = app.world();
    let text = |field| match world.get(id, "GraphSource", field) {
        Ok(Value::Str(s)) => s,
        _ => String::new(),
    };
    let (file, builder, params) = (text("graph"), text("builder"), text("params"));
    if !file.is_empty() {
        ui.label(RichText::new(format!("graph {file}")).weak());
    }
    if !builder.is_empty() {
        ui.label(RichText::new(format!("builder {builder}")).weak());
    }
    let overrides: Map<String, Json> = if params.trim().is_empty() {
        Map::new()
    } else {
        match serde_json::from_str::<Json>(&params) {
            Ok(Json::Object(m)) => m,
            _ => {
                ui.label(
                    RichText::new("GraphSource.params is not a JSON object").color(Color32::RED),
                );
                return;
            }
        }
    };
    let store = world.ecs.get_resource::<GraphStore>();
    let current = store.and_then(|s| s.current(world, id));
    let doc: Option<&GraphDoc> = match &current {
        Some(Ok(doc)) => Some(doc),
        Some(Err(d)) => {
            ui.label(RichText::new(&d.message).color(Color32::RED));
            None
        }
        None => None,
    };
    let registry = store.map(|s| s.evaluator().registry());
    let has_set_param = app.commands().spec("graph.set_param").is_some();
    let live = schema::takes_coalesce(app.commands().spec("graph.set_param"));
    let types = Types { doc, registry };

    if overrides.is_empty() {
        ui.label(RichText::new("No parameter overrides.").weak());
    }
    let pending = &mut ed.pending;
    egui::Grid::new(("re_editor.params", id))
        .num_columns(3)
        .show(ui, |ui| {
            for (node, params) in &overrides {
                let Json::Object(params) = params else {
                    // Not an object of parameters: it can only be removed.
                    let name = ui
                        .label(RichText::new(node).color(Color32::RED))
                        .on_hover_text("not an object of parameter values; remove it");
                    record(
                        pending,
                        ProceduralSpot::Override(id, node, "", "name"),
                        name.rect,
                    );
                    ui.label(RichText::new(params.to_string()).weak());
                    let remove = ui.button("Remove");
                    record(
                        pending,
                        ProceduralSpot::Override(id, node, "", "Remove"),
                        remove.rect,
                    );
                    if remove.clicked() {
                        actions.push(set_param(&overrides, id, node, "", Json::Null, false));
                    }
                    ui.end_row();
                    continue;
                };
                for (param, value) in params {
                    let row = Row {
                        source: id,
                        node,
                        param,
                        value,
                    };
                    let edit = row.show(ui, pending, &types, assets, actions);
                    match edit {
                        Some(RowEdit::Remove) => {
                            actions.push(set_param(&overrides, id, node, param, Json::Null, false));
                        }
                        Some(RowEdit::Change(Change::Invalid(why))) => {
                            let what = format!("the override of `{node}.{param}`");
                            actions.push(Action::Report(
                                schema::invalid_text(&why, &what)
                                    .at(Location::entity(id.to_string())),
                            ));
                        }
                        Some(RowEdit::Change(change)) => {
                            let wid = row.id();
                            if let Some((v, key)) = schema::to_send(ui, wid, change, value, live) {
                                let mut action =
                                    set_param(&overrides, id, node, param, v, has_set_param);
                                if let (Some(key), Action::Command { args, .. }) =
                                    (key, &mut action)
                                {
                                    args["coalesce"] = Json::String(key);
                                }
                                actions.push(action);
                            }
                        }
                        None => {}
                    }
                    ui.end_row();
                }
            }
        });

    if let (Some(doc), Some(registry)) = (doc, registry) {
        let state = ed.graph_new.entry(id.to_string()).or_default();
        let picked = Picker {
            source: id,
            doc,
            registry,
            overrides: &overrides,
        }
        .show(ui, &mut ed.pending, state, assets, actions);
        if let Some((node, param, value)) = picked {
            actions.push(set_param(
                &overrides,
                id,
                &node,
                &param,
                value,
                has_set_param,
            ));
            ed.graph_new.remove(id);
        }
    }
}

/// What the panel knows of a source's graph: its nodes, and their kinds'
/// schemas.
struct Types<'a> {
    doc: Option<&'a GraphDoc>,
    registry: Option<&'a NodeRegistry>,
}

/// How an override's parameter is typed.
enum Typed<'a> {
    /// A parameter of the node's kind, with its schema.
    Param { def: &'a NodeDef, prop: &'a Json },
    /// The graph's node takes the override, but its type is not known here:
    /// the graph is not loaded, or the node is written for another version
    /// of its kind (whose overrides are checked after its migrations).
    Untyped(String),
    /// The graph has no such node or parameter: why.
    Missing(String),
}

impl<'a> Types<'a> {
    fn param(&self, node: &str, param: &str) -> Typed<'a> {
        let (Some(doc), Some(registry)) = (self.doc, self.registry) else {
            return Typed::Untyped("the source's graph is not loaded".into());
        };
        let Some(n) = doc.node(node) else {
            return Typed::Missing(format!("the graph has no node `{node}`; remove it"));
        };
        let Some(def) = registry.get(&n.kind) else {
            return Typed::Missing(format!(
                "node `{node}` has kind `{}`, which this build does not know; remove it",
                n.kind
            ));
        };
        if n.version != def.version {
            return Typed::Untyped(format!(
                "node `{node}` is written for version {} of {}, so its override is typed as JSON",
                n.version, n.kind
            ));
        }
        match def
            .params_schema
            .get("properties")
            .and_then(|p| p.get(param))
        {
            Some(prop) => Typed::Param { def, prop },
            None => Typed::Missing(format!(
                "`{param}` is not a parameter of {} (node `{node}`); remove it",
                n.kind
            )),
        }
    }

    /// The value the graph gives `node`'s `param`: its own, else the kind's
    /// default.
    fn graph_value(&self, node: &str, param: &str) -> Option<Json> {
        let n = self.doc?.node(node)?;
        if let Some(v) = n.params.get(param) {
            return Some(v.clone());
        }
        let def = self.registry?.get(&n.kind)?;
        let root = &def.params_schema;
        schema::default(root.get("properties")?.get(param)?, root)
    }
}

/// What was done to one override's row.
enum RowEdit {
    Change(Change),
    /// "Remove" on an override the graph does not have.
    Remove,
}

/// One override of a source.
struct Row<'a> {
    source: &'a str,
    node: &'a str,
    param: &'a str,
    value: &'a Json,
}

/// Misfit overrides already reported, so each is reported once.
#[derive(Clone, Default)]
struct Reported(HashSet<String>);

impl Row<'_> {
    fn id(&self) -> Id {
        Id::new(("re_editor.override", self.source, self.node, self.param))
    }

    fn spot<'s>(&'s self, part: &'s str) -> ProceduralSpot<'s> {
        ProceduralSpot::Override(self.source, self.node, self.param, part)
    }

    /// Its name (red when the graph lacks it), widget and × or "Remove".
    fn show(
        &self,
        ui: &mut egui::Ui,
        pending: &mut Pending,
        types: &Types<'_>,
        assets: &dyn Fn(&str) -> Vec<String>,
        actions: &mut Vec<Action>,
    ) -> Option<RowEdit> {
        let typed = types.param(self.node, self.param);
        let (shape, mut hover, missing) = match &typed {
            Typed::Param { def, prop } => {
                let root = &def.params_schema;
                (Shape::of(prop, root), schema::doc(prop, root), false)
            }
            Typed::Untyped(why) => (Shape::Raw { types: Vec::new() }, why.clone(), false),
            Typed::Missing(why) => (Shape::Raw { types: Vec::new() }, why.clone(), true),
        };
        let misfit = !missing && !shape.fits(self.value);
        if misfit {
            let key = format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                self.source, self.node, self.param, self.value
            );
            let first = ui.data_mut(|d| {
                d.get_temp_mut_or_default::<Reported>(Id::new("re_editor.procedural.misfits"))
                    .0
                    .insert(key)
            });
            if first {
                let what = format!("the override of `{}.{}`", self.node, self.param);
                actions.push(Action::Report(
                    schema::misfit(&what, self.value, &shape)
                        .at(Location::entity(self.source.to_string())),
                ));
            }
            hover.push_str(
                "\n\nThe override is not of this parameter's type, so it is shown as JSON text.",
            );
        }
        if let Some(v) = types.graph_value(self.node, self.param) {
            hover.push_str(&format!(
                "\n\nThe graph's value: {v}. The override wins for this source."
            ));
        }
        let text = RichText::new(format!("{}.{}", self.node, self.param));
        let name = ui
            .label(if missing || misfit {
                text.color(Color32::RED)
            } else {
                text
            })
            .on_hover_text(hover.trim_start());
        record(pending, self.spot("name"), name.rect);

        if missing {
            ui.label(RichText::new(self.value.to_string()).weak());
            let remove = ui
                .button("Remove")
                .on_hover_text("Remove it from this source's overrides");
            record(pending, self.spot("Remove"), remove.rect);
            return remove.clicked().then_some(RowEdit::Remove);
        }
        let shape = if misfit { shape.as_raw() } else { shape };
        let mut env = Env::new(assets);
        let change = schema::edit(ui, self.id(), &shape, self.value, &mut env);
        for (part, rect) in &env.parts {
            record(pending, self.spot(part), *rect);
        }
        let x = ui
            .small_button("×")
            .on_hover_text("Remove this override: the graph's value applies again");
        record(pending, self.spot("×"), x.rect);
        if x.clicked() {
            return Some(RowEdit::Change(Change::Done(Json::Null)));
        }
        change.map(RowEdit::Change)
    }
}

/// The picker that adds an override to a source.
struct Picker<'a> {
    source: &'a str,
    doc: &'a GraphDoc,
    registry: &'a NodeRegistry,
    overrides: &'a Map<String, Json>,
}

impl Picker<'_> {
    fn spot<'s>(&'s self, part: &'s str) -> ProceduralSpot<'s> {
        ProceduralSpot::Picker(self.source, part)
    }

    /// Draws the picker; `state` is the node, the parameter and the value
    /// chosen so far (JSON text, empty for the value the source uses now).
    /// Returns the override to set when "Set" is clicked.
    fn show(
        &self,
        ui: &mut egui::Ui,
        pending: &mut Pending,
        state: &mut [String; 3],
        assets: &dyn Fn(&str) -> Vec<String>,
        actions: &mut Vec<Action>,
    ) -> Option<(String, String, Json)> {
        let [node, param, value] = state;
        let set: Vec<&String> = self
            .overrides
            .get(node.as_str())
            .and_then(Json::as_object)
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        let n = self.doc.node(node);
        let def = n.and_then(|n| self.registry.get(&n.kind));
        let root = def.map(|d| &d.params_schema);
        let free: Vec<(&String, &Json)> = root
            .and_then(|r| r.get("properties"))
            .and_then(Json::as_object)
            .into_iter()
            .flatten()
            .filter(|(name, _)| !set.contains(name))
            .collect();

        ui.horizontal_wrapped(|ui| {
            ui.label("add");
            let shown = if node.is_empty() {
                RichText::new("node").italics().weak()
            } else {
                RichText::new(node.as_str())
            };
            let r = egui::ComboBox::from_id_salt(("re_editor.picker.node", self.source))
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for n in &self.doc.nodes {
                        let item =
                            ui.selectable_label(*node == n.id, format!("{}  ({})", n.id, n.kind));
                        let part = format!("node={}", n.id);
                        record(pending, self.spot(&part), item.rect);
                        if item.clicked() && *node != n.id {
                            *node = n.id.clone();
                            param.clear();
                            value.clear();
                        }
                    }
                });
            record(pending, self.spot("node"), r.response.rect);

            let shown = if param.is_empty() {
                RichText::new("parameter").italics().weak()
            } else {
                RichText::new(param.as_str())
            };
            let r = ui.add_enabled_ui(!free.is_empty(), |ui| {
                egui::ComboBox::from_id_salt(("re_editor.picker.param", self.source))
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for (name, prop) in &free {
                            let item = ui.selectable_label(*param == **name, name.as_str());
                            let doc = root.map(|r| schema::doc(prop, r)).unwrap_or_default();
                            let item = if doc.is_empty() {
                                item
                            } else {
                                item.on_hover_text(doc)
                            };
                            let part = format!("param={name}");
                            record(pending, self.spot(&part), item.rect);
                            if item.clicked() && param != *name {
                                param.clone_from(name);
                                value.clear();
                            }
                        }
                    })
                    .response
            });
            let hover = if node.is_empty() {
                "pick a node first"
            } else if def.is_none() {
                "this build does not know the node's kind"
            } else {
                "the source overrides every parameter of this node"
            };
            r.response.on_disabled_hover_text(hover);
            record(pending, self.spot("param"), r.inner.rect);
        });

        // The parameter's widget, from the value the source uses now.
        let (Some(n), Some(def), Some(root)) = (n, def, root) else {
            return None;
        };
        let prop = free.iter().find(|(name, _)| *name == param)?.1;
        let mut shape = Shape::of(prop, root);
        let start = n
            .params
            .get(param.as_str())
            .cloned()
            .or_else(|| schema::default(prop, root))
            .unwrap_or_else(|| shape.zero());
        let mut chosen = serde_json::from_str::<Json>(value).unwrap_or(start);
        if n.version != def.version || !shape.fits(&chosen) {
            shape = shape.as_raw();
        }
        let mut picked = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(param.as_str())
                .on_hover_text(schema::doc(prop, root));
            let id = Id::new(("re_editor.picker.value", self.source, &*node, &*param));
            let mut env = Env::new(assets);
            let change = schema::edit(ui, id, &shape, &chosen, &mut env);
            for (part, rect) in &env.parts {
                let part = if part.is_empty() {
                    "value".to_string()
                } else {
                    format!("value/{part}")
                };
                record(pending, self.spot(&part), *rect);
            }
            match change {
                Some(Change::Done(v)) => {
                    *value = v.to_string();
                    chosen = v;
                }
                Some(Change::Invalid(why)) => {
                    let what = format!("the value to set for `{node}.{param}`");
                    actions.push(Action::Report(
                        schema::invalid_text(&why, &what)
                            .at(Location::entity(self.source.to_string())),
                    ));
                }
                Some(Change::Live(_)) | None => {}
            }
            let set = ui
                .button("Set")
                .on_hover_text("Override it for this source (graph.set_param)");
            record(pending, self.spot("Set"), set.rect);
            if set.clicked() {
                picked = Some((node.clone(), param.clone(), chosen.clone()));
            }
        });
        picked
    }
}

/// Sets one parameter override: through `graph.set_param` (which checks the
/// value against the node's schema) when it is registered, else by writing
/// `GraphSource.params` with `entity.set`. A `null` value removes it, and a
/// node left with no parameters (or whose entry is not an object of them).
fn set_param(
    overrides: &Map<String, Json>,
    id: &str,
    node: &str,
    param: &str,
    value: Json,
    has_set_param: bool,
) -> Action {
    if has_set_param {
        return command(
            "graph.set_param",
            json!({ "source": id, "node": node, "param": param, "value": value }),
        );
    }
    let mut all = overrides.clone();
    let entry = all
        .entry(node.to_string())
        .or_insert_with(|| Json::Object(Map::new()));
    if !entry.is_object() {
        *entry = Json::Object(Map::new());
    }
    let params = entry.as_object_mut().expect("made an object above");
    if value.is_null() {
        params.remove(param);
    } else {
        params.insert(param.to_string(), value);
    }
    if params.is_empty() {
        all.remove(node);
    }
    all.sort_keys();
    for v in all.values_mut() {
        if let Json::Object(m) = v {
            m.sort_keys();
        }
    }
    let text = if all.is_empty() {
        String::new()
    } else {
        Json::Object(all).to_string()
    };
    Action::SetField {
        id: id.to_string(),
        component: "GraphSource".to_string(),
        field: "params".to_string(),
        ty: FieldType::String,
        value: Value::Str(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_overrides_are_listed_by_key_with_what_they_do() {
        use re_scene::structure::{Insert, Move, Replace};
        let overrides = Overrides(vec![
            OverrideDoc::Field {
                target: "b".into(),
                component: "Transform".into(),
                field: "scale".into(),
                value: re_reflect::Doc::Null,
            },
            OverrideDoc::Moved {
                target: "b".into(),
                to: Move {
                    parent: Some("a".into()),
                    index: Some(0),
                },
            },
            OverrideDoc::Deleted { target: "c".into() },
            OverrideDoc::Inserted {
                target: "+lamp".into(),
                insert: Insert {
                    parent: None,
                    index: None,
                    name: None,
                    with: Replacement::Prefab("prefabs/lamp.prefab.json".into()),
                },
            },
            OverrideDoc::Deleted { target: "b".into() },
            OverrideDoc::Replaced {
                target: "d".into(),
                replace: Replace {
                    with: Replacement::Components(Default::default()),
                    keep_children: true,
                },
            },
        ]);
        let kept = kept(&overrides);
        let listed: Vec<(&str, String, bool, bool)> = kept
            .iter()
            .map(|k| (k.key.as_str(), k.what.join(", "), k.structural, k.deleted))
            .collect();
        assert_eq!(
            listed,
            [
                ("b", "moved under `a` at 0, deleted".to_string(), true, true),
                ("c", "deleted".to_string(), false, true),
                (
                    "+lamp",
                    "inserted under the source: prefabs/lamp.prefab.json".to_string(),
                    true,
                    false
                ),
                (
                    "d",
                    "replaced with no components, keeping its children".to_string(),
                    true,
                    false
                ),
            ]
        );
    }

    fn params(action: Action) -> String {
        match action {
            Action::SetField {
                component,
                field,
                value: Value::Str(s),
                ..
            } if component == "GraphSource" && field == "params" => s,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn set_param_without_the_command_writes_sorted_overrides() {
        let none = Map::new();
        let one = params(set_param(
            &none,
            "island",
            "terrain",
            "height",
            json!(12.5),
            false,
        ));
        assert_eq!(one, r#"{"terrain":{"height":12.5}}"#);
        let Json::Object(current) = serde_json::from_str(&one).unwrap() else {
            unreachable!()
        };
        let two = params(set_param(
            &current,
            "island",
            "rocks",
            "count",
            json!(40),
            false,
        ));
        assert_eq!(two, r#"{"rocks":{"count":40},"terrain":{"height":12.5}}"#);
        let cleared = params(set_param(
            &current,
            "island",
            "terrain",
            "height",
            Json::Null,
            false,
        ));
        assert_eq!(cleared, "");
        // A node entry that is not an object of parameters is removed whole.
        let Json::Object(bad) = json!({"rocks": 3, "terrain": {"height": 1}}) else {
            unreachable!()
        };
        let fixed = params(set_param(&bad, "island", "rocks", "", Json::Null, false));
        assert_eq!(fixed, r#"{"terrain":{"height":1}}"#);
    }

    #[test]
    fn set_param_uses_the_command_when_registered() {
        let action = set_param(&Map::new(), "island", "terrain", "height", json!(3), true);
        assert_eq!(
            action,
            command(
                "graph.set_param",
                json!({ "source": "island", "node": "terrain", "param": "height", "value": 3 })
            )
        );
    }

    #[test]
    fn next_seed_is_a_fixed_non_negative_scramble() {
        assert_eq!(next_seed(1234), next_seed(1234));
        assert_ne!(next_seed(1234), 1234);
        assert!((0..100).all(|s| next_seed(s) >= 0));
    }

    #[test]
    fn a_scrub_moves_one_seed_per_step() {
        let at = |dx| {
            Scrub {
                from: 10,
                origin: 0.0,
                dx,
            }
            .seed()
        };
        assert_eq!(at(0.0), 10);
        assert_eq!(at(SCRUB - 0.5), 10);
        assert_eq!(at(SCRUB), 11);
        assert_eq!(at(5.0 * SCRUB + 1.0), 15);
        assert_eq!(at(-2.0 * SCRUB), 8);
        let edge = Scrub {
            from: i64::MAX,
            origin: 0.0,
            dx: 100.0,
        };
        assert_eq!(edge.seed(), i64::MAX);
    }
}
