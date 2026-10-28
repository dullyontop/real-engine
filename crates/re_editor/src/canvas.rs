//! The node canvas (ADR-0044): a graph file drawn with egui-snarl and edited
//! only through the `graph.*` commands.
//!
//! The snarl is a view of the graph, rebuilt whenever the graph changes; the
//! canvas never edits the document. What the person does on it becomes
//! commands: a wire dragged between two pins is `graph.connect`, a wire cut
//! is `graph.disconnect` (snarl's Ctrl-drag reroutes, which change its wires
//! directly, are found by comparing its wires with the graph's once the
//! button is up), a node dragged is one `graph.move_nodes` when the button is
//! released, and the menus add and remove nodes. After anything is sent the
//! view is rebuilt from the graph, so a refused command leaves the canvas
//! showing the file as it is. Which nodes are collapsed is view state, saved
//! with the editor layout. Nodes without a position in the file's `layout`
//! are placed by their depth in the graph.
//!
//! A node's body shows every parameter of its kind, in its schema's order,
//! with the widget its schema gives it (`widgets::schema`); one the node does
//! not set shows the kind's default, dimmed. Each finished edit is one
//! `graph.set_default`, so one undo step, and × removes what the node sets.
//! While a value is dragged, the value under the pointer is sent at most
//! every 200 ms under one coalesce key, when `graph.set_default` takes one,
//! so the drag is still one undo step. A parameter the shown source
//! overrides (`GraphSource.params`) is marked, and its menu removes the
//! override.
//!
//! A source whose graph a builder script makes (`GraphSource.builder`) is
//! listed too, marked "builder", with the graph the script made laid out
//! by depth. It is read-only: no menus, no wiring, disabled widgets, and
//! snarl's own changes (a node moved, a wire rerouted) are dropped when the
//! button is up; collapsing stays. Trying to edit it notes `RE8852` once.
//! "Open script" names the script in the console, and "Convert to a graph
//! file" sends `proc.convert_builder` with the positions the nodes are drawn
//! at, after which the source reads the file and the canvas edits it.
//!
//! Under each node, badges from the source's last evaluation (the
//! `EvalReport` that `graph.report` returns): the time the node took, with a
//! bar against the slowest node; where its output came from (the memory or
//! disk cache, computed, skipped or failed); the output's size; and the
//! node's problems and logs, listed on hover. They are dimmed while the
//! report is of another seed than the source's. A subgraph instance `a`
//! has no row of its own, only its inner nodes' rows (`a/x`): its badges sum
//! them (ADR-0061 §7).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use egui::{Color32, Id, RichText, UiBuilder};
use egui_snarl::ui::{PinInfo, PinWireInfo, SnarlPin, SnarlStyle, SnarlViewer, SnarlWidget};
use egui_snarl::{InPin, InPinId, NodeId, OutPin, OutPinId, Snarl};
use re_app::App;
use re_core::{Diagnostic, Location};
use re_proc::{CacheState, EvalReport, GraphStore, NodeDef, NodeRegistry, NodeReport, PortType};
use re_reflect::Value;
use re_scene::World;
use re_types::graph::{EdgeDoc, GraphDoc, NodeDoc};
use serde_json::{Map, Value as Json, json};

use crate::codes;
use crate::editor::{Action, Editor, LineKind, command};
use crate::layout::Collapsed;
use crate::widgets::schema::{self, Change, Env, Shape};

/// The space between the columns, and between the nodes of a column, of
/// nodes placed by depth, in points.
const GAP: egui::Vec2 = egui::vec2(60.0, 30.0);
/// The most commands [`Editor::canvas_commands`] keeps; older ones are dropped.
const LOG: usize = 256;
/// The colour of the marker on a parameter the shown source overrides.
const OVERRIDDEN: Color32 = Color32::from_rgb(0xe0, 0x9a, 0x3c);

/// A place on the node canvas, in egui points, as drawn in the last frame:
/// for tests and tools that drive the canvas with the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasSpot<'a> {
    /// The title in a node's header: drag it to move the node; right-click it
    /// for the node's menu.
    Header(&'a str),
    /// The button that collapses or expands a node.
    Collapse(&'a str),
    /// A node's input pin, by port name.
    Input(&'a str, &'a str),
    /// A node's output pin.
    Output(&'a str),
    /// An item of the open menu, by its text: a node kind or kind group in
    /// the canvas menu, "Remove node" in a node's menu, "Remove override"
    /// and "Reset to default" in a parameter's menu.
    MenuItem(&'a str),
    /// A part of a node's parameter row: `(node, parameter, part)`, where the
    /// part is `"name"` (right-click it for the parameter's menu), `"×"` (the
    /// button that removes what the node sets), or a part of the parameter's
    /// widget: `""` the widget itself, `"0"`, `"1"`… a vector's components,
    /// `"set"` a nullable value's checkbox, `"=<value>"` an item of a choice's
    /// list.
    Param(&'a str, &'a str, &'a str),
    /// A button above the canvas, by its text: `"Open script"` and
    /// `"Convert to a graph file"` for a builder source.
    Button(&'a str),
    /// A badge under a node, from the source's last evaluation, by node id
    /// and badge: `"time"`; where the output came from, by its text
    /// (`"memory"`, `"disk"`, `"computed"`, `"skipped"` or `"failed"`);
    /// `"size"`; `"problems"`; `"logs"`.
    Badge(&'a str, &'a str),
}

impl CanvasSpot<'_> {
    fn key(self) -> String {
        match self {
            CanvasSpot::Header(n) => format!("header\u{1f}{n}"),
            CanvasSpot::Collapse(n) => format!("collapse\u{1f}{n}"),
            CanvasSpot::Input(n, p) => format!("in\u{1f}{n}\u{1f}{p}"),
            CanvasSpot::Output(n) => format!("out\u{1f}{n}"),
            CanvasSpot::MenuItem(m) => format!("menu\u{1f}{m}"),
            CanvasSpot::Param(n, p, part) => format!("param\u{1f}{n}\u{1f}{p}\u{1f}{part}"),
            CanvasSpot::Button(b) => format!("button\u{1f}{b}"),
            CanvasSpot::Badge(n, b) => format!("badge\u{1f}{n}\u{1f}{b}"),
        }
    }
}

/// Where the canvas drew its spots in the last frame, in screen points. The
/// pins record themselves while snarl draws them, so they share it.
type Spots = Arc<Mutex<HashMap<String, egui::Rect>>>;

fn record(spots: &Spots, ctx: &egui::Context, layer: egui::LayerId, key: String, rect: egui::Rect) {
    let to_screen = ctx.layer_transform_to_global(layer).unwrap_or_default();
    spots
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, to_screen * rect);
}

/// The node canvas's state.
#[derive(Default)]
pub(crate) struct Canvas {
    /// The procedural source whose graph is shown (the first one when unset
    /// or gone).
    source: Option<String>,
    collapsed: Collapsed,
    view: Option<View>,
    spots: Spots,
    /// The last [`LOG`] commands the canvas sent, oldest first.
    log: Vec<(String, Json)>,
    /// The values already reported as not fitting their parameter
    /// (`RE8850`), so each is reported once.
    misfits: HashSet<String>,
    /// The builder sources already noted as read-only (`RE8852`).
    noted: HashSet<String>,
}

impl Canvas {
    pub(crate) fn collapsed(&self) -> &Collapsed {
        &self.collapsed
    }

    pub(crate) fn set_collapsed(&mut self, collapsed: Collapsed) {
        self.collapsed = collapsed;
        self.view = None;
    }
}

/// The graph as the snarl shows it.
struct View {
    source: String,
    path: String,
    /// The graph the view was built from.
    doc: GraphDoc,
    snarl: Snarl<NodeView>,
    /// The graph's edges, by the wire that shows each.
    wires: HashMap<(OutPinId, InPinId), EdgeDoc>,
    /// Node positions when the pointer button went down.
    at_press: Option<Vec<(NodeId, egui::Pos2)>>,
    /// A command was sent: rebuild once the button is up, whatever it did.
    stale: bool,
}

/// One node as the canvas shows it.
struct NodeView {
    id: String,
    kind: String,
    /// The kind's description, or `None` for a kind this build does not know.
    doc: Option<&'static str>,
    /// Input ports: the kind's own, the ports edges name, and for a kind with
    /// any number of inputs one free port.
    inputs: Vec<(String, Option<PortType>)>,
    output: (String, Option<PortType>),
    params: Vec<ParamRow>,
    /// Why the node's parameters cannot be edited here (a kind this build
    /// does not know, or a node written for another version of its kind).
    locked: Option<String>,
}

/// One parameter of a node as the canvas shows it.
struct ParamRow {
    name: String,
    /// The property's description.
    doc: String,
    shape: Shape,
    /// What the node sets, or `None` when the kind's default applies.
    value: Option<Json>,
    /// The kind's default (`null` when its schema gives none).
    default: Json,
    /// Set by the node but not a parameter of its kind: it can only be
    /// removed.
    unknown: bool,
    /// What the node sets is not of the parameter's type, so it is shown as
    /// JSON text (`RE8850`).
    misfit: bool,
}

/// The rows of a node of kind `def` that sets `set`: the kind's parameters
/// in its schema's order, then any the node sets that the kind lacks.
fn param_rows(def: &NodeDef, set: &Map<String, Json>) -> Vec<ParamRow> {
    let root = &def.params_schema;
    let props = root.get("properties").and_then(Json::as_object);
    let mut rows: Vec<ParamRow> = props
        .into_iter()
        .flatten()
        .map(|(name, prop)| {
            let shape = Shape::of(prop, root);
            let value = set.get(name).cloned();
            let misfit = value.as_ref().is_some_and(|v| !shape.fits(v));
            ParamRow {
                name: name.clone(),
                doc: schema::doc(prop, root),
                shape: if misfit { shape.as_raw() } else { shape },
                value,
                default: schema::default(prop, root).unwrap_or(Json::Null),
                unknown: false,
                misfit,
            }
        })
        .collect();
    for (name, value) in set {
        if !props.is_some_and(|p| p.contains_key(name)) {
            rows.push(unknown_row(name, value));
        }
    }
    rows
}

fn unknown_row(name: &str, value: &Json) -> ParamRow {
    ParamRow {
        name: name.to_string(),
        doc: String::new(),
        shape: Shape::Raw { types: Vec::new() },
        value: Some(value.clone()),
        default: Json::Null,
        unknown: true,
        misfit: false,
    }
}

/// The id of a parameter's widget, the same in every view of the graph.
fn param_id(path: &str, node: &str, param: &str) -> Id {
    Id::new(("re_editor.canvas.param", path, node, param))
}

/// The parameter overrides of source `id` (`GraphSource.params`), by node.
fn overrides(world: &World, id: &str) -> Map<String, Json> {
    match world.get(id, "GraphSource", "params") {
        Ok(Value::Str(s)) if !s.trim().is_empty() => match serde_json::from_str(&s) {
            Ok(Json::Object(m)) => m,
            _ => Map::new(),
        },
        _ => Map::new(),
    }
}

/// What the viewer was asked to do while snarl drew.
enum Queued {
    Connect(OutPinId, InPinId),
    Disconnect(OutPinId, InPinId),
    Command(String, Json),
}

impl Editor {
    /// The procedural source whose graph the node canvas shows.
    pub fn canvas_source(&self) -> Option<&str> {
        self.canvas.source.as_deref()
    }

    /// Shows the graph of procedural source `id` on the node canvas (from the
    /// next frame).
    pub fn set_canvas_source(&mut self, id: Option<&str>) {
        self.canvas.source = id.map(str::to_string);
    }

    /// Where the node canvas drew `spot` in the last frame, in egui points.
    pub fn canvas_spot(&self, spot: CanvasSpot<'_>) -> Option<egui::Rect> {
        let spots = self.canvas.spots.lock().unwrap_or_else(|e| e.into_inner());
        spots.get(&spot.key()).copied()
    }

    /// The last 256 commands the node canvas sent, oldest first, as
    /// `(name, args)`.
    pub fn canvas_commands(&self) -> &[(String, Json)] {
        &self.canvas.log
    }
}

/// A procedural source the canvas can show.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Shown {
    id: String,
    /// The graph file; for a builder source, its script. It keys the view
    /// state (collapsed nodes) and the widgets.
    path: String,
    /// True when a builder script makes the graph (`GraphSource.builder`, and
    /// no `GraphSource.graph`): the canvas shows it read-only.
    builder: bool,
}

/// The procedural sources that read a graph file or whose graph a builder
/// script makes, sorted by id.
fn graph_sources(world: &World) -> Vec<Shown> {
    let Some(key) = world.registry.try_key("GraphSource") else {
        return Vec::new();
    };
    let field = |id: &str, name: &str| match world.get(id, "GraphSource", name) {
        Ok(Value::Str(p)) if !p.trim().is_empty() => Some(re_scene::doc_path(&p).unwrap_or(p)),
        _ => None,
    };
    let mut out: Vec<Shown> = world
        .ids()
        .iter()
        .filter(|(_, e)| world.registry.has(&world.ecs, *e, key))
        .filter_map(|(id, _)| {
            let (path, builder) = match field(id, "graph") {
                Some(path) => (path, false),
                None => (field(id, "builder")?, true),
            };
            Some(Shown {
                id: id.to_string(),
                path,
                builder,
            })
        })
        .collect();
    out.sort();
    out
}

/// The hover text of what a builder graph does not let the person do.
fn read_only_text(script: &str) -> String {
    format!(
        "The builder script {script} makes this graph, so it is read-only here: edit the \
         script, or \"Convert to a graph file\" to edit the graph on the canvas"
    )
}

/// Draws the node canvas.
pub(crate) fn ui(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    let canvas = &mut ed.canvas;
    canvas
        .spots
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    let world = app.world();
    let sources = graph_sources(world);
    let Some(first) = sources.first() else {
        canvas.view = None;
        ui.label(
            RichText::new(
                "No procedural source in this scene has a graph (GraphSource.graph or \
                 GraphSource.builder).",
            )
            .weak(),
        );
        return;
    };
    let Shown {
        id: source,
        path,
        builder,
    } = canvas
        .source
        .as_ref()
        .and_then(|s| sources.iter().find(|shown| shown.id == *s))
        .unwrap_or(first)
        .clone();
    let mut chosen = source.clone();
    let (mut open_script, mut convert) = (false, false);
    ui.horizontal(|ui| {
        let mark = |builder: bool| if builder { "  (builder)" } else { "" };
        egui::ComboBox::from_id_salt("re_editor.canvas.source")
            .selected_text(format!("{source}{}", mark(builder)))
            .show_ui(ui, |ui| {
                for s in &sources {
                    let text = format!("{}  {}{}", s.id, s.path, mark(s.builder));
                    ui.selectable_value(&mut chosen, s.id.clone(), text);
                }
            });
        ui.label(RichText::new(&path).weak());
        if builder {
            ui.label(RichText::new("builder").italics())
                .on_hover_text(read_only_text(&path));
            let r = ui
                .button("Open script")
                .on_hover_text("Open the builder script");
            record(
                &canvas.spots,
                ui.ctx(),
                ui.layer_id(),
                CanvasSpot::Button("Open script").key(),
                r.rect,
            );
            open_script = r.clicked();
            let r = ui
                .add_enabled(
                    app.commands().spec("proc.convert_builder").is_some(),
                    egui::Button::new("Convert to a graph file"),
                )
                .on_hover_text(
                    "Make a graph file holding this graph (graphs/<source>.graph.json), as the \
                     canvas lays it out, and read the source's graph from it from now on \
                     (proc.convert_builder; one undo step). The script is kept.",
                )
                .on_disabled_hover_text("proc.convert_builder is not registered in this build");
            record(
                &canvas.spots,
                ui.ctx(),
                ui.layer_id(),
                CanvasSpot::Button("Convert to a graph file").key(),
                r.rect,
            );
            convert = r.clicked();
        } else if world.unsaved_docs().contains(&path) {
            ui.label(RichText::new("unsaved").italics())
                .on_hover_text("Saving the scene writes the graph file");
        }
    });
    canvas.source = Some(chosen);

    let Some(store) = world.ecs.get_resource::<GraphStore>() else {
        canvas.view = None;
        return;
    };
    let doc = match store.current(world, &source) {
        Some(Ok(doc)) => doc,
        Some(Err(d)) => {
            canvas.view = None;
            ui.label(RichText::new(&d.message).color(Color32::RED));
            return;
        }
        None => {
            canvas.view = None;
            ui.label(RichText::new(format!("The graph of `{source}` is not loaded.")).weak());
            return;
        }
    };
    let registry = store.evaluator().registry();
    let down = ui.input(|i| i.pointer.any_down());
    let rebuild = canvas.view.as_ref().is_none_or(|v| {
        v.source != source || v.path != path || v.doc != *doc || (v.stale && !down)
    });
    if rebuild {
        let view = build(&source, &path, &doc, registry, canvas.collapsed.get(&path));
        for node in view.snarl.nodes() {
            for row in node.params.iter().filter(|r| r.misfit) {
                let value = row.value.as_ref().unwrap_or(&Json::Null);
                let key = format!("{path}\u{1f}{}\u{1f}{}\u{1f}{value}", node.id, row.name);
                if canvas.misfits.insert(key) {
                    let what = format!("parameter `{}` of node `{}`", row.name, node.id);
                    actions.push(Action::Report(
                        schema::misfit(&what, value, &row.shape).at(Location::file(path.clone())),
                    ));
                }
            }
        }
        canvas.view = Some(view);
    }
    let view = canvas.view.as_mut().expect("built above");
    if down && view.at_press.is_none() {
        view.at_press = Some(
            view.snarl
                .nodes_pos_ids()
                .map(|(id, p, _)| (id, p))
                .collect(),
        );
    }

    let lists = ed.assets.lists();
    let assets = move |kind: &str| lists.of(kind);
    let overrides = overrides(world, &source);
    let read_only = builder.then(|| read_only_text(&path));
    let report = store.report(&source);
    let seed = match world.get(&source, "Procedural", "seed") {
        Ok(Value::Int(s)) => Some(s as u64),
        _ => None,
    };
    let badges = report.map(|r| Badges::new(r, seed));
    let mut viewer = Viewer {
        path: &path,
        source: &source,
        registry,
        spots: canvas.spots.clone(),
        overrides: &overrides,
        assets: &assets,
        read_only: read_only.as_deref(),
        badges: badges.as_ref(),
        tried: false,
        queued: Vec::new(),
        edits: Vec::new(),
    };
    let mut style = SnarlStyle::new();
    // The header draws its own collapse button, so a test can find it.
    style.collapsible = Some(false);
    SnarlWidget::new()
        .id_salt(("re_editor.canvas", &path))
        .style(style)
        .show(&mut view.snarl, &mut viewer, ui);
    let (queued, edits, tried) = (viewer.queued, viewer.edits, viewer.tried);

    // Collapsed nodes, by id.
    let collapsed: BTreeSet<String> = view
        .snarl
        .nodes_info()
        .filter(|n| !n.open)
        .map(|n| n.value.id.clone())
        .collect();
    let layout_changed = canvas
        .collapsed
        .get(&path)
        .map_or(!collapsed.is_empty(), |c| *c != collapsed);
    if collapsed.is_empty() {
        canvas.collapsed.remove(&path);
    } else {
        canvas.collapsed.insert(path.clone(), collapsed);
    }

    if builder {
        // Read-only: nothing is sent, and what snarl changed itself (a node
        // moved, a wire rerouted) is dropped once the button is up.
        let moved = !down
            && view.at_press.take().is_some_and(|before| {
                before.iter().any(|(node, was)| {
                    view.snarl
                        .get_node_info(*node)
                        .is_some_and(|info| info.pos.round() != was.round())
                })
            });
        let rewired = !down
            && view.snarl.wires().collect::<HashSet<_>>()
                != view.wires.keys().copied().collect::<HashSet<_>>();
        if moved || rewired {
            view.stale = true;
        }
        if (tried || moved || rewired || !queued.is_empty() || !edits.is_empty())
            && canvas.noted.insert(source.clone())
        {
            actions.push(Action::Report(read_only_note(&source, &path)));
        }
        if convert {
            // The positions the nodes are drawn at, so the file shows the
            // graph where it is now.
            let layout: BTreeMap<String, [f32; 2]> = view
                .snarl
                .nodes_pos_ids()
                .map(|(_, p, n)| (n.id.clone(), [p.x.round(), p.y.round()]))
                .collect();
            let args = json!({ "source": source, "layout": layout });
            actions.push(command("proc.convert_builder", args.clone()));
            canvas.log.push(("proc.convert_builder".into(), args));
            view.stale = true;
        }
        trim(&mut canvas.log);
        if open_script {
            ed.log(
                LineKind::Info,
                format!("The builder script of `{source}` is {path}"),
            );
        }
        if layout_changed {
            ed.layout_changed();
        }
        return;
    }

    // Wires snarl changed itself (a Ctrl-drag reroute), once the button is up.
    let (mut removed, added): (Vec<_>, Vec<_>) = if down {
        (Vec::new(), Vec::new())
    } else {
        let now: HashSet<(OutPinId, InPinId)> = view.snarl.wires().collect();
        let removed = view
            .wires
            .keys()
            .filter(|w| !now.contains(w))
            .copied()
            .collect();
        let added = now
            .into_iter()
            .filter(|w| !view.wires.contains_key(w))
            .collect();
        (removed, added)
    };
    let mut disconnects = Vec::new();
    let mut rest = Vec::new();
    for q in queued {
        match q {
            Queued::Connect(from, to) => {
                if let Some(i) = removed.iter().position(|w| *w == (from, to)) {
                    // Dropped back where it was.
                    removed.swap_remove(i);
                } else if !view.wires.contains_key(&(from, to)) {
                    rest.push(connect_args(&path, &view.snarl, from, to));
                }
            }
            Queued::Disconnect(from, to) => {
                if view.wires.contains_key(&(from, to)) && !removed.contains(&(from, to)) {
                    disconnects.push((from, to));
                }
            }
            Queued::Command(name, args) => rest.push((name, args)),
        }
    }
    // Parameter edits: each finished one, and a drag's live ones under its
    // coalesce key when the command takes one.
    let live = schema::takes_coalesce(app.commands().spec("graph.set_default"));
    for edit in edits {
        let what = format!("parameter `{}` of node `{}`", edit.param, edit.node);
        if let Change::Invalid(why) = &edit.change {
            actions.push(Action::Report(
                schema::invalid_text(why, &what).at(Location::file(path.clone())),
            ));
            continue;
        }
        let id = param_id(&path, &edit.node, &edit.param);
        if let Some((value, key)) = schema::to_send(ui, id, edit.change, &edit.shown, live) {
            let mut args = json!({
                "graph": path, "node": edit.node, "param": edit.param, "value": value,
            });
            if let Some(key) = key {
                args["coalesce"] = Json::String(key);
            }
            rest.push(("graph.set_default".into(), args));
        }
    }
    let mut sent: Vec<(String, Json)> = Vec::new();
    disconnects.extend(removed);
    disconnects.sort_by_key(|(o, i)| (o.node.0, o.output, i.node.0, i.input));
    disconnects.dedup();
    for w in disconnects {
        let e = &view.wires[&w];
        sent.push((
            "graph.disconnect".into(),
            json!({ "graph": path, "from": e.from, "to": e.to }),
        ));
    }
    sent.extend(rest);
    for (from, to) in added {
        sent.push(connect_args(&path, &view.snarl, from, to));
    }

    // Nodes dragged, once the button is up: one command.
    if !down && let Some(before) = view.at_press.take() {
        let mut positions = BTreeMap::new();
        for (node, was) in before {
            let Some(info) = view.snarl.get_node_info(node) else {
                continue;
            };
            let now = info.pos.round();
            if now != was.round() {
                positions.insert(info.value.id.clone(), [now.x, now.y]);
            }
        }
        if !positions.is_empty() {
            sent.push((
                "graph.move_nodes".into(),
                json!({ "graph": path, "positions": positions }),
            ));
        }
    }

    if !sent.is_empty() {
        view.stale = true;
    }
    for (name, args) in sent {
        actions.push(command(&name, args.clone()));
        canvas.log.push((name, args));
    }
    trim(&mut canvas.log);
    if layout_changed {
        ed.layout_changed();
    }
}

/// Keeps the last [`LOG`] commands of `log`.
fn trim(log: &mut Vec<(String, Json)>) {
    if log.len() > LOG {
        let extra = log.len() - LOG;
        log.drain(..extra);
    }
}

/// `RE8852`: source `id`'s graph, made by `script`, is read-only here.
fn read_only_note(id: &str, script: &str) -> Diagnostic {
    Diagnostic::note(
        codes::BUILDER_READ_ONLY,
        format!(
            "the graph of `{id}` is made by the builder script {script}, so the canvas \
             shows it read-only"
        ),
    )
    .at(Location::entity(id.to_string()))
    .with_hint(
        "edit the script, or press \"Convert to a graph file\" to edit the graph on the \
         canvas; parameter overrides still work in the Procedural panel",
    )
}

fn connect_args(
    path: &str,
    snarl: &Snarl<NodeView>,
    from: OutPinId,
    to: InPinId,
) -> (String, Json) {
    let (f, t) = (&snarl[from.node], &snarl[to.node]);
    (
        "graph.connect".into(),
        json!({
            "graph": path,
            "from": [f.id, f.output.0],
            "to": [t.id, t.inputs[to.input].0],
        }),
    )
}

/// The view of `doc`.
fn build(
    source: &str,
    path: &str,
    doc: &GraphDoc,
    registry: &NodeRegistry,
    collapsed: Option<&BTreeSet<String>>,
) -> View {
    let views: Vec<NodeView> = doc
        .nodes
        .iter()
        .map(|n| node_view(doc, n, registry))
        .collect();
    let sizes: HashMap<&str, egui::Vec2> =
        views.iter().map(|v| (v.id.as_str(), v.size())).collect();
    let auto = auto_layout(doc, |id| sizes.get(id).copied().unwrap_or_default());
    let mut snarl = Snarl::new();
    let mut ids = HashMap::new();
    for (n, node) in doc.nodes.iter().zip(views) {
        let pos = doc
            .layout
            .get(&n.id)
            .map(|p| egui::pos2(p[0], p[1]))
            .or_else(|| auto.get(&n.id).copied())
            .unwrap_or_default();
        let id = if collapsed.is_some_and(|c| c.contains(&n.id)) {
            snarl.insert_node_collapsed(pos, node)
        } else {
            snarl.insert_node(pos, node)
        };
        ids.insert(n.id.as_str(), id);
    }
    let mut wires = HashMap::new();
    for e in &doc.edges {
        let (Some(&from), Some(&to)) = (ids.get(e.from.0.as_str()), ids.get(e.to.0.as_str()))
        else {
            continue;
        };
        let Some(input) = snarl[to].inputs.iter().position(|(p, _)| *p == e.to.1) else {
            continue;
        };
        let wire = (
            OutPinId {
                node: from,
                output: 0,
            },
            InPinId { node: to, input },
        );
        snarl.connect(wire.0, wire.1);
        wires.insert(wire, e.clone());
    }
    View {
        source: source.to_string(),
        path: path.to_string(),
        doc: doc.clone(),
        snarl,
        wires,
        at_press: None,
        stale: false,
    }
}

/// Node `n` of `doc` as the canvas shows it.
fn node_view(doc: &GraphDoc, n: &NodeDoc, registry: &NodeRegistry) -> NodeView {
    let def = registry.get(&n.kind);
    let mut inputs: Vec<(String, Option<PortType>)> = def
        .map(|d| {
            d.inputs
                .iter()
                .map(|p| (p.name.to_string(), p.accepts.first().copied()))
                .collect()
        })
        .unwrap_or_default();
    let variadic = def.and_then(|d| d.variadic);
    let any = variadic.and_then(|v| v.first().copied());
    for e in doc.edges.iter().filter(|e| e.to.0 == n.id) {
        if !inputs.iter().any(|(p, _)| *p == e.to.1) {
            inputs.push((e.to.1.clone(), any));
        }
    }
    if variadic.is_some() {
        let free = (1..)
            .map(|i| format!("in{i}"))
            .find(|p| !inputs.iter().any(|(q, _)| q == p))
            .expect("a free port name");
        inputs.push((free, any));
    }
    let output = match def {
        Some(d) => (d.output_port().to_string(), Some(d.output)),
        None => ("out".to_string(), None),
    };
    let params = match def {
        Some(d) => param_rows(d, &n.params),
        None => n.params.iter().map(|(k, v)| unknown_row(k, v)).collect(),
    };
    let locked = match def {
        None => Some(format!(
            "`{}` is not a node kind this build knows, so its parameters cannot be edited",
            n.kind
        )),
        Some(d) if n.version != d.version => Some(format!(
            "this node is written for version {} of {}, and its parameters are edited at \
             version {}: run `real migrate-graph` first",
            n.version, n.kind, d.version
        )),
        Some(_) => None,
    };
    NodeView {
        id: n.id.clone(),
        kind: n.kind.clone(),
        doc: def.map(|d| d.doc),
        inputs,
        output,
        params,
        locked,
    }
}

impl NodeView {
    /// About how big snarl draws the node, in points, for placing the nodes
    /// the file does not place.
    fn size(&self) -> egui::Vec2 {
        let lines: usize = self
            .params
            .iter()
            .map(|r| r.shape.lines(r.value.as_ref().unwrap_or(&r.default)))
            .sum();
        let name = self
            .params
            .iter()
            .map(|r| r.name.chars().count())
            .max()
            .unwrap_or(0);
        let widget = self
            .params
            .iter()
            .map(|r| r.shape.width())
            .fold(0.0, f32::max);
        let body = (name as f32 * 6.5 + widget + 40.0).max(self.kind.chars().count() as f32 * 7.0);
        let rows = (lines + 1).max(self.inputs.len());
        egui::vec2(90.0 + body, 40.0 + 23.0 * rows as f32)
    }
}

/// Positions for nodes the file does not place: a column per depth (the
/// longest path from a node without inputs), in file order down each column,
/// each node `size` apart from the next.
fn auto_layout(doc: &GraphDoc, size: impl Fn(&str) -> egui::Vec2) -> HashMap<String, egui::Pos2> {
    let mut depth: HashMap<&str, usize> = doc.nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    // A valid graph has no cycle, so this settles in at most one round per
    // node; the bound keeps an invalid one from looping.
    for _ in 0..doc.nodes.len() {
        let mut changed = false;
        for e in &doc.edges {
            let (Some(&f), Some(&t)) = (depth.get(e.from.0.as_str()), depth.get(e.to.0.as_str()))
            else {
                continue;
            };
            if t < f + 1 {
                depth.insert(e.to.0.as_str(), f + 1);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Each column as wide as its widest node.
    let columns = depth.values().copied().max().map_or(0, |d| d + 1);
    let mut widths = vec![0.0f32; columns];
    for n in &doc.nodes {
        let d = depth[n.id.as_str()];
        widths[d] = widths[d].max(size(&n.id).x);
    }
    let mut xs = vec![0.0f32; columns];
    for d in 1..columns {
        xs[d] = xs[d - 1] + widths[d - 1] + GAP.x;
    }
    let mut ys: HashMap<usize, f32> = HashMap::new();
    doc.nodes
        .iter()
        .map(|n| {
            let d = depth[n.id.as_str()];
            let y = ys.entry(d).or_default();
            let pos = egui::pos2(xs[d], *y);
            *y += size(&n.id).y + GAP.y;
            (n.id.clone(), pos)
        })
        .collect()
}

/// A parameter value on one short line.
fn short(v: &Json) -> String {
    let text = v.to_string();
    if text.chars().count() <= 32 {
        return text;
    }
    let mut cut: String = text.chars().take(31).collect();
    cut.push('…');
    cut
}

fn colour(ty: Option<PortType>) -> Color32 {
    match ty {
        Some(PortType::Field) => Color32::from_rgb(0xd9, 0xa4, 0x41),
        Some(PortType::Mesh) => Color32::from_rgb(0x4a, 0x90, 0xd9),
        Some(PortType::Points) => Color32::from_rgb(0x5c, 0xb8, 0x5c),
        Some(PortType::Fragment) => Color32::from_rgb(0xa0, 0x6c, 0xd5),
        Some(PortType::Shapes) => Color32::from_rgb(0xd5, 0x6c, 0x8a),
        Some(PortType::Path) => Color32::from_rgb(0x6c, 0xc5, 0xd5),
        None => Color32::GRAY,
    }
}

/// A pin that records where it was drawn.
struct Pin {
    info: PinInfo,
    key: String,
    spots: Spots,
}

impl SnarlPin for Pin {
    fn draw(
        self,
        snarl_style: &SnarlStyle,
        style: &egui::Style,
        rect: egui::Rect,
        painter: &egui::Painter,
    ) -> PinWireInfo {
        record(
            &self.spots,
            painter.ctx(),
            painter.layer_id(),
            self.key,
            rect,
        );
        self.info.draw(snarl_style, style, rect, painter)
    }
}

struct Viewer<'a> {
    path: &'a str,
    /// The procedural source the graph is shown for.
    source: &'a str,
    registry: &'a NodeRegistry,
    spots: Spots,
    /// The source's parameter overrides, by node.
    overrides: &'a Map<String, Json>,
    /// The project's files of an asset kind, for asset pickers.
    assets: &'a dyn Fn(&str) -> Arc<[String]>,
    /// Why nothing can be edited (a builder graph), shown on the disabled
    /// widgets.
    read_only: Option<&'a str>,
    /// The badges from the source's last evaluation.
    badges: Option<&'a Badges>,
    /// An edit was tried on a read-only graph.
    tried: bool,
    queued: Vec<Queued>,
    edits: Vec<ParamEdit>,
}

/// What a parameter's widget did while snarl drew.
struct ParamEdit {
    node: String,
    param: String,
    /// The value the widget showed.
    shown: Json,
    change: Change,
}

impl Viewer<'_> {
    fn spot(&self, ui: &egui::Ui, spot: CanvasSpot<'_>, rect: egui::Rect) {
        record(&self.spots, ui.ctx(), ui.layer_id(), spot.key(), rect);
    }

    /// Records parts already in screen points (a widget's [`Env::parts`]).
    fn screen_spot(&self, spot: CanvasSpot<'_>, rect: egui::Rect) {
        self.spots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(spot.key(), rect);
    }

    /// One parameter's row: its name (with a marker when the source
    /// overrides it, and a menu), its widget, and × when the node sets it.
    fn param_row(&mut self, ui: &mut egui::Ui, node: &NodeView, row: &ParamRow) {
        let over = self.overrides.get(&node.id).and_then(|p| p.get(&row.name));
        let name = ui
            .horizontal(|ui| {
                if over.is_some() {
                    label(ui, RichText::new("●").small().color(OVERRIDDEN));
                }
                let text = RichText::new(&row.name).small();
                label(
                    ui,
                    if row.unknown || row.misfit {
                        text.color(Color32::RED)
                    } else {
                        text
                    },
                )
            })
            .inner;
        let mut hover = if row.unknown {
            format!(
                "`{}` is not a parameter of {}; remove it",
                row.name, node.kind
            )
        } else {
            row.doc.clone()
        };
        if row.misfit {
            hover.push_str(
                "\n\nThe file's value is not of this parameter's type, so it is shown as JSON text.",
            );
        }
        if let Some(v) = over {
            let file = row
                .value
                .as_ref()
                .map_or_else(|| format!("the default {}", short(&row.default)), short);
            hover.push_str(&format!(
                "\n\nOverridden on {}: {} (the file: {file}). The override wins for that \
                 source; editing here changes the file.",
                self.source,
                short(v)
            ));
        }
        let name = if hover.is_empty() {
            name
        } else {
            name.on_hover_text(hover.trim_start())
        };
        self.spot(
            ui,
            CanvasSpot::Param(&node.id, &row.name, "name"),
            name.rect,
        );
        let editable = node.locked.is_none() && self.read_only.is_none();
        if editable && (over.is_some() || row.value.is_some()) {
            name.context_menu(|ui| {
                if over.is_some() {
                    let b = ui.button("Remove override");
                    self.spot(ui, CanvasSpot::MenuItem("Remove override"), b.rect);
                    if b.clicked() {
                        self.queued.push(Queued::Command(
                            "graph.set_param".into(),
                            json!({
                                "source": self.source, "node": node.id,
                                "param": row.name, "value": null,
                            }),
                        ));
                        ui.close();
                    }
                }
                if row.value.is_some() {
                    let b = ui.button("Reset to default");
                    self.spot(ui, CanvasSpot::MenuItem("Reset to default"), b.rect);
                    if b.clicked() {
                        self.queued.push(self.unset(node, row));
                        ui.close();
                    }
                }
            });
        }

        // The widget, dimmed while it shows the kind's default.
        let shown = row.value.clone().unwrap_or_else(|| row.default.clone());
        let id = param_id(self.path, &node.id, &row.name);
        let mut env = Env::new(self.assets);
        let change = ui
            .scope_builder(UiBuilder::new().id(id.with("row")), |ui| {
                if row.value.is_none() {
                    ui.multiply_opacity(0.5);
                }
                let r = ui.add_enabled_ui(editable && !row.unknown, |ui| {
                    schema::edit(ui, id, &row.shape, &shown, &mut env)
                });
                if let Some(why) = self.read_only.or(node.locked.as_deref()) {
                    r.response.on_disabled_hover_text(why);
                }
                r.inner
            })
            .inner;
        for (part, rect) in &env.parts {
            self.screen_spot(CanvasSpot::Param(&node.id, &row.name, part), *rect);
        }
        if let Some(change) = change {
            self.edits.push(ParamEdit {
                node: node.id.clone(),
                param: row.name.clone(),
                shown,
                change,
            });
        }

        if row.value.is_some() {
            let x = ui
                .add_enabled(editable, egui::Button::new("×").small())
                .on_hover_text("Remove it from the node: the kind's default applies");
            self.spot(ui, CanvasSpot::Param(&node.id, &row.name, "×"), x.rect);
            if x.clicked() {
                self.queued.push(self.unset(node, row));
            }
        } else {
            label(ui, "");
        }
    }

    /// The `graph.set_default` that removes what `node` sets for `row`.
    fn unset(&self, node: &NodeView, row: &ParamRow) -> Queued {
        Queued::Command(
            "graph.set_default".into(),
            json!({ "graph": self.path, "node": node.id, "param": row.name, "value": null }),
        )
    }

    fn pin(&self, spot: CanvasSpot<'_>, ty: Option<PortType>) -> Pin {
        Pin {
            info: PinInfo::circle().with_fill(colour(ty)),
            key: spot.key(),
            spots: self.spots.clone(),
        }
    }
}

fn label(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Label::new(text).selectable(false))
}

/// The badges of a source's last evaluation, by node.
struct Badges {
    rows: HashMap<String, NodeReport>,
    /// The subgraph instances among `rows`, with the number of inner nodes
    /// each one's row sums.
    instances: HashMap<String, usize>,
    /// The slowest node's time, in milliseconds: the time bars' full width.
    slowest: f64,
    /// Why the report is not of the graph as shown, when it is not.
    stale: Option<String>,
}

impl Badges {
    fn new(report: EvalReport, seed: Option<u64>) -> Badges {
        let stale = seed.filter(|s| *s != report.seed).map(|s| {
            format!(
                "The last evaluation used seed {}; the source's seed is {s}. These \
                 badges change when the source next regenerates.",
                report.seed
            )
        });
        let mut rows: HashMap<String, NodeReport> = HashMap::new();
        let mut inner: BTreeMap<String, Vec<NodeReport>> = BTreeMap::new();
        for n in report.nodes {
            if let Some((instance, _)) = n.id.split_once('/') {
                inner
                    .entry(instance.to_string())
                    .or_default()
                    .push(n.clone());
            }
            rows.insert(n.id.clone(), n);
        }
        let mut instances = HashMap::new();
        for (id, nodes) in inner {
            if rows.contains_key(&id) {
                continue;
            }
            instances.insert(id.clone(), nodes.len());
            let row = summed(id.clone(), nodes);
            rows.insert(id, row);
        }
        let slowest = rows.values().map(|n| n.ms).fold(0.0, f64::max);
        Badges {
            rows,
            instances,
            slowest,
            stale,
        }
    }

    /// Draws the badges of one node's `row`, and returns where each one is
    /// (by its [`CanvasSpot::Badge`] name).
    fn show(&self, ui: &mut egui::Ui, row: &NodeReport) -> Vec<(&'static str, egui::Rect)> {
        let mut parts = Vec::new();
        if self.stale.is_some() {
            ui.multiply_opacity(0.45);
        }
        let hover = |text: String| match &self.stale {
            Some(why) => format!("{text}\n\n{why}"),
            None => text,
        };
        // The time, with a bar against the slowest node.
        let text = label(ui, RichText::new(time_text(row.ms)).small().monospace());
        let (rect, bar) = ui.allocate_exact_size(egui::vec2(BAR_WIDTH, 6.0), egui::Sense::hover());
        let fill = if self.slowest > 0.0 {
            (row.ms / self.slowest).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
        let mut done = rect;
        done.set_width(BAR_WIDTH * fill);
        painter.rect_filled(done, 2.0, ui.visuals().selection.bg_fill);
        let what = match self.instances.get(&row.id) {
            Some(n) => format!("this subgraph's {n} inner nodes, together,"),
            None => "this node".to_string(),
        };
        let time = text.union(bar).on_hover_text(hover(format!(
            "{} loading or computing {what} in the last evaluation; the bar is against \
             the slowest node's {}",
            time_text(row.ms),
            time_text(self.slowest)
        )));
        parts.push(("time", time.rect));

        let (cache, colour, about) = cache_text(row.cache);
        let r = label(ui, RichText::new(cache).small().color(colour));
        parts.push((cache, r.on_hover_text(hover(about.into())).rect));

        if let Some(out) = &row.output {
            let r = label(ui, RichText::new(size_text(out)).small()).on_hover_text(hover(format!(
                "The {} output: {}, about {} in memory",
                out.ty.port_name(),
                items_text(out),
                bytes_text(out.bytes)
            )));
            parts.push(("size", r.rect));
        }

        if !row.diagnostics.is_empty() {
            let error = row
                .diagnostics
                .iter()
                .any(|d| d.severity == re_core::Severity::Error);
            let colour = if error { Color32::RED } else { WARNING };
            let list: Vec<String> = row
                .diagnostics
                .iter()
                .map(|d| format!("{}: {}", d.code, d.message))
                .collect();
            let text = format!("⚠ {}", row.diagnostics.len());
            let r = label(ui, RichText::new(text).small().color(colour))
                .on_hover_text(hover(list.join("\n")));
            parts.push(("problems", r.rect));
        }

        if !row.logs.is_empty() {
            let text = format!("log {}", row.logs.len());
            let r = label(ui, RichText::new(text).small().weak())
                .on_hover_text(hover(row.logs.join("\n")));
            parts.push(("logs", r.rect));
        }
        parts
    }
}

/// The time bar's width, in points.
const BAR_WIDTH: f32 = 36.0;

/// The colour of a node's warnings.
const WARNING: Color32 = Color32::from_rgb(0xe0, 0xc0, 0x40);

/// A node's time: `< 0.1 ms`, `12.3 ms` or `1.25 s`.
fn time_text(ms: f64) -> String {
    if ms < 0.1 {
        "< 0.1 ms".into()
    } else if ms < 1000.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

/// Where a node's output came from: the badge, its colour and what it means.
fn cache_text(cache: CacheState) -> (&'static str, Color32, &'static str) {
    match cache {
        CacheState::Memory => (
            "memory",
            Color32::from_rgb(0x6c, 0xc0, 0x7a),
            "The output came from the memory cache",
        ),
        CacheState::Disk => (
            "disk",
            Color32::from_rgb(0x6c, 0xb0, 0xc0),
            "The output came from the disk cache",
        ),
        CacheState::Miss => (
            "computed",
            Color32::GRAY,
            "The node computed its output: nothing cached matched its inputs, parameters and seed",
        ),
        CacheState::Skipped => (
            "skipped",
            Color32::DARK_GRAY,
            "The node did not run: an input failed, the budget ran out, or a streamed \
             evaluation did not need it",
        ),
        CacheState::Failed => ("failed", Color32::RED, "The node ran and failed"),
    }
}

/// An output's size: its items and its bytes.
fn size_text(out: &re_proc::OutputInfo) -> String {
    format!("{} · {}", items_text(out), bytes_text(out.bytes))
}

/// An output's items, in its type's unit: `81 samples`, `1 vertex`.
fn items_text(out: &re_proc::OutputInfo) -> String {
    let (one, many) = match out.ty {
        PortType::Field | PortType::Path => ("sample", "samples"),
        PortType::Mesh => ("vertex", "vertices"),
        PortType::Points | PortType::Shapes => ("point", "points"),
        PortType::Fragment => ("entity", "entities"),
    };
    let unit = if out.items == 1 { one } else { many };
    format!("{} {unit}", out.items)
}

/// One row for a subgraph instance `id` from its inner nodes' rows: their
/// time summed, the worst of their cache states (failed, computed, disk,
/// memory; skipped only when every one was), and their problems and logs.
/// No output: the report does not say which inner node feeds the instance's
/// `$out`.
fn summed(id: String, nodes: Vec<NodeReport>) -> NodeReport {
    let rank = |c: CacheState| match c {
        CacheState::Skipped => 0,
        CacheState::Memory => 1,
        CacheState::Disk => 2,
        CacheState::Miss => 3,
        CacheState::Failed => 4,
    };
    let cache = nodes
        .iter()
        .map(|n| n.cache)
        .max_by_key(|c| rank(*c))
        .unwrap_or(CacheState::Skipped);
    let mut row = NodeReport {
        id,
        kind: "subgraph".into(),
        key: String::new(),
        cache,
        ms: 0.0,
        output: None,
        diagnostics: Vec::new(),
        logs: Vec::new(),
    };
    for n in nodes {
        row.ms += n.ms;
        row.diagnostics.extend(n.diagnostics);
        row.logs.extend(n.logs);
    }
    row
}

/// `bytes` in B, KiB, MiB or GiB.
fn bytes_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

impl SnarlViewer<NodeView> for Viewer<'_> {
    fn title(&mut self, node: &NodeView) -> String {
        node.id.clone()
    }

    fn show_header(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeView>,
    ) {
        let open = snarl.get_node_info(node).is_some_and(|n| n.open);
        let size = egui::vec2(ui.spacing().icon_width, ui.spacing().icon_width);
        let (_, toggle) = ui.allocate_exact_size(size, egui::Sense::click());
        egui::collapsing_header::paint_default_icon(ui, if open { 1.0 } else { 0.0 }, &toggle);
        let n = &snarl[node];
        self.spot(ui, CanvasSpot::Collapse(&n.id), toggle.rect);
        let title = label(ui, RichText::new(&n.id).strong()).on_hover_text(&n.kind);
        self.spot(ui, CanvasSpot::Header(&n.id), title.rect);
        if toggle.clicked() {
            snarl.open_node(node, !open);
        }
    }

    fn inputs(&mut self, node: &NodeView) -> usize {
        node.inputs.len()
    }

    fn show_input(
        &mut self,
        pin: &InPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeView>,
    ) -> impl SnarlPin + 'static {
        let n = &snarl[pin.id.node];
        let (port, ty) = &n.inputs[pin.id.input];
        label(ui, port.as_str());
        self.pin(CanvasSpot::Input(&n.id, port), *ty)
    }

    fn outputs(&mut self, _node: &NodeView) -> usize {
        1
    }

    fn show_output(
        &mut self,
        pin: &OutPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeView>,
    ) -> impl SnarlPin + 'static {
        let n = &snarl[pin.id.node];
        label(ui, n.output.0.as_str());
        self.pin(CanvasSpot::Output(&n.id), n.output.1)
    }

    fn has_body(&mut self, _node: &NodeView) -> bool {
        true
    }

    fn show_body(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeView>,
    ) {
        let n = &snarl[node];
        ui.vertical(|ui| {
            match n.doc {
                Some(doc) => label(ui, RichText::new(&n.kind).weak()).on_hover_text(doc),
                None => label(
                    ui,
                    RichText::new(format!("{} (unknown kind)", n.kind)).color(Color32::RED),
                ),
            };
            if n.params.is_empty() {
                return;
            }
            egui::Grid::new(("re_editor.canvas.params", self.path, &n.id))
                .num_columns(3)
                .show(ui, |ui| {
                    for row in &n.params {
                        self.param_row(ui, n, row);
                        ui.end_row();
                    }
                });
        });
    }

    fn has_footer(&mut self, node: &NodeView) -> bool {
        self.badges.is_some_and(|b| b.rows.contains_key(&node.id))
    }

    fn show_footer(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeView>,
    ) {
        let n = &snarl[node];
        let Some(badges) = self.badges else {
            return;
        };
        let Some(row) = badges.rows.get(&n.id) else {
            return;
        };
        let parts = ui.horizontal(|ui| badges.show(ui, row)).inner;
        for (badge, rect) in parts {
            self.spot(ui, CanvasSpot::Badge(&n.id, badge), rect);
        }
    }

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<NodeView>) -> bool {
        self.read_only.is_none()
    }

    fn show_graph_menu(
        &mut self,
        pos: egui::Pos2,
        ui: &mut egui::Ui,
        _snarl: &mut Snarl<NodeView>,
    ) {
        label(ui, RichText::new("Add node").strong());
        // Kinds by their first name segment, in registration order.
        let mut groups: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
        for def in self.registry.iter() {
            let group = def.kind.split('.').next().unwrap_or(def.kind);
            match groups.iter_mut().find(|(g, _)| *g == group) {
                Some((_, kinds)) => kinds.push((def.kind, def.doc)),
                None => groups.push((group, vec![(def.kind, def.doc)])),
            }
        }
        for (group, kinds) in groups {
            let r = ui.menu_button(group, |ui| {
                for (kind, doc) in kinds {
                    let b = ui.button(kind).on_hover_text(doc);
                    self.spot(ui, CanvasSpot::MenuItem(kind), b.rect);
                    if b.clicked() {
                        self.queued.push(Queued::Command(
                            "graph.add_node".into(),
                            json!({
                                "graph": self.path,
                                "kind": kind,
                                "pos": [pos.x.round(), pos.y.round()],
                            }),
                        ));
                        ui.close();
                    }
                }
            });
            self.spot(ui, CanvasSpot::MenuItem(group), r.response.rect);
        }
    }

    fn has_node_menu(&mut self, _node: &NodeView) -> bool {
        self.read_only.is_none()
    }

    fn show_node_menu(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeView>,
    ) {
        let b = ui.button("Remove node");
        self.spot(ui, CanvasSpot::MenuItem("Remove node"), b.rect);
        if b.clicked() {
            self.queued.push(Queued::Command(
                "graph.remove_node".into(),
                json!({ "graph": self.path, "node": snarl[node].id }),
            ));
            ui.close();
        }
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, _snarl: &mut Snarl<NodeView>) {
        if self.read_only.is_some() {
            self.tried = true;
            return;
        }
        self.queued.push(Queued::Connect(from.id, to.id));
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, _snarl: &mut Snarl<NodeView>) {
        if self.read_only.is_some() {
            self.tried = true;
            return;
        }
        self.queued.push(Queued::Disconnect(from.id, to.id));
    }

    fn drop_outputs(&mut self, pin: &OutPin, _snarl: &mut Snarl<NodeView>) {
        if self.read_only.is_some() {
            self.tried = true;
            return;
        }
        for to in &pin.remotes {
            self.queued.push(Queued::Disconnect(pin.id, *to));
        }
    }

    fn drop_inputs(&mut self, pin: &InPin, _snarl: &mut Snarl<NodeView>) {
        if self.read_only.is_some() {
            self.tried = true;
            return;
        }
        for from in &pin.remotes {
            self.queued.push(Queued::Disconnect(*from, pin.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unplaced_nodes_are_laid_out_by_depth() {
        let node = |id: &str| NodeDoc {
            id: id.into(),
            kind: "field.combine".into(),
            version: 1,
            params: Default::default(),
        };
        let edge = |f: &str, t: &str, p: &str| EdgeDoc {
            from: (f.into(), "field".into()),
            to: (t.into(), p.into()),
        };
        let doc = GraphDoc {
            nodes: vec![node("c"), node("a"), node("b"), node("d")],
            // a -> b -> c, a -> c, d alone.
            edges: vec![
                edge("b", "c", "a"),
                edge("a", "b", "a"),
                edge("a", "c", "b"),
            ],
            ..GraphDoc::new("t", 1)
        };
        // Every node 200 wide and 100 high but `a`, which is 300 wide.
        let size = |id: &str| egui::vec2(if id == "a" { 300.0 } else { 200.0 }, 100.0);
        let at = auto_layout(&doc, size);
        assert_eq!(at["a"], egui::pos2(0.0, 0.0));
        assert_eq!(at["d"], egui::pos2(0.0, 100.0 + GAP.y));
        assert_eq!(at["b"], egui::pos2(300.0 + GAP.x, 0.0));
        assert_eq!(at["c"], egui::pos2(500.0 + 2.0 * GAP.x, 0.0));
    }

    #[test]
    fn badges_are_stale_while_the_report_is_of_another_seed() {
        let node = |id: &str, ms: f64| NodeReport {
            id: id.into(),
            kind: "terrain.heightfield".into(),
            key: String::new(),
            cache: CacheState::Miss,
            ms,
            output: None,
            diagnostics: Vec::new(),
            logs: Vec::new(),
        };
        let report = EvalReport {
            seed: 7,
            nodes: vec![node("a", 2.0), node("b", 8.0)],
            ..Default::default()
        };
        let badges = Badges::new(report.clone(), Some(7));
        assert!(badges.stale.is_none());
        assert_eq!(badges.slowest, 8.0);
        assert_eq!(badges.rows["a"].ms, 2.0);
        // A source with no seed field (or no report seed to compare) is not
        // called stale.
        assert!(Badges::new(report.clone(), None).stale.is_none());
        let stale = Badges::new(report, Some(8)).stale.unwrap();
        assert!(
            stale.contains("seed 7") && stale.contains("seed is 8"),
            "{stale}"
        );
    }

    #[test]
    fn a_node_shows_the_badges_its_report_has() {
        let plain = NodeReport {
            id: "a".into(),
            kind: "terrain.heightfield".into(),
            key: String::new(),
            cache: CacheState::Memory,
            ms: 1.0,
            output: None,
            diagnostics: Vec::new(),
            logs: Vec::new(),
        };
        let full = NodeReport {
            id: "b".into(),
            cache: CacheState::Failed,
            output: Some(re_proc::OutputInfo {
                ty: PortType::Field,
                items: 81,
                bytes: 324,
            }),
            diagnostics: vec![Diagnostic::warning(codes::PARAM_VALUE, "odd")],
            logs: vec!["hello".into()],
            ..plain.clone()
        };
        let report = EvalReport {
            nodes: vec![plain.clone(), full.clone()],
            ..Default::default()
        };
        let badges = Badges::new(report, None);
        let ctx = egui::Context::default();
        let mut shown = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for row in [&plain, &full] {
                let parts = ui.horizontal(|ui| badges.show(ui, row)).inner;
                shown.push(parts.into_iter().map(|(b, _)| b).collect::<Vec<_>>());
            }
        });
        // Nothing paints the font atlas here.
        output.textures_delta.clear();
        assert_eq!(shown[0], ["time", "memory"]);
        assert_eq!(shown[1], ["time", "failed", "size", "problems", "logs"]);
    }

    #[test]
    fn a_subgraph_instance_sums_its_inner_nodes() {
        let node = |id: &str, cache, ms: f64, logs: &[&str]| NodeReport {
            id: id.into(),
            kind: "field.combine".into(),
            key: String::new(),
            cache,
            ms,
            output: None,
            diagnostics: Vec::new(),
            logs: logs.iter().map(|l| l.to_string()).collect(),
        };
        let report = EvalReport {
            nodes: vec![
                node("h", CacheState::Miss, 1.0, &[]),
                node("a/x", CacheState::Memory, 2.0, &["x ran"]),
                node("a/y", CacheState::Miss, 3.0, &[]),
                node("a/b/z", CacheState::Disk, 4.0, &["z ran"]),
                node("s/x", CacheState::Skipped, 0.0, &[]),
            ],
            ..Default::default()
        };
        let badges = Badges::new(report, None);
        let a = &badges.rows["a"];
        assert_eq!(a.ms, 9.0);
        assert_eq!(a.cache, CacheState::Miss);
        assert_eq!(a.logs, ["x ran", "z ran"]);
        assert!(a.output.is_none());
        assert_eq!(badges.instances["a"], 3);
        // Skipped only when every inner node was.
        assert_eq!(badges.rows["s"].cache, CacheState::Skipped);
        // The bars are against the slowest row shown, the instance's sum.
        assert_eq!(badges.slowest, 9.0);
        assert!(!badges.instances.contains_key("h"));
    }

    #[test]
    fn badge_texts() {
        assert_eq!(time_text(0.04), "< 0.1 ms");
        assert_eq!(time_text(12.34), "12.3 ms");
        assert_eq!(time_text(1250.0), "1.25 s");
        assert_eq!(bytes_text(512), "512 B");
        assert_eq!(bytes_text(1536), "1.5 KiB");
        assert_eq!(bytes_text(3 * 1024 * 1024), "3.0 MiB");
        let out = |ty, items| re_proc::OutputInfo {
            ty,
            items,
            bytes: 48,
        };
        assert_eq!(size_text(&out(PortType::Mesh, 1)), "1 vertex · 48 B");
        assert_eq!(size_text(&out(PortType::Field, 81)), "81 samples · 48 B");
        assert_eq!(size_text(&out(PortType::Fragment, 2)), "2 entities · 48 B");
    }
}
