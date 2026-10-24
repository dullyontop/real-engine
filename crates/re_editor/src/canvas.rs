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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use egui::{Color32, RichText};
use egui_snarl::ui::{PinInfo, PinWireInfo, SnarlPin, SnarlStyle, SnarlViewer, SnarlWidget};
use egui_snarl::{InPin, InPinId, NodeId, OutPin, OutPinId, Snarl};
use re_app::App;
use re_proc::{GraphStore, NodeRegistry, PortType};
use re_reflect::Value;
use re_scene::World;
use re_types::graph::{EdgeDoc, GraphDoc};
use serde_json::{Value as Json, json};

use crate::editor::{Action, Editor, command};
use crate::layout::Collapsed;

/// The distance between the columns of nodes placed by depth, in points.
const COLUMN: f32 = 220.0;
/// The distance between the rows of nodes placed by depth, in points.
const ROW: f32 = 120.0;
/// The most commands [`Editor::canvas_commands`] keeps; older ones are dropped.
const LOG: usize = 256;

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
    /// the canvas menu, "Remove node" in a node's menu.
    MenuItem(&'a str),
}

impl CanvasSpot<'_> {
    fn key(self) -> String {
        match self {
            CanvasSpot::Header(n) => format!("header\u{1f}{n}"),
            CanvasSpot::Collapse(n) => format!("collapse\u{1f}{n}"),
            CanvasSpot::Input(n, p) => format!("in\u{1f}{n}\u{1f}{p}"),
            CanvasSpot::Output(n) => format!("out\u{1f}{n}"),
            CanvasSpot::MenuItem(m) => format!("menu\u{1f}{m}"),
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
    params: Vec<String>,
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

/// The procedural sources that read a graph file, as `(id, path)`, sorted.
fn graph_sources(world: &World) -> Vec<(String, String)> {
    let Some(key) = world.registry.try_key("GraphSource") else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = world
        .ids()
        .iter()
        .filter(|(_, e)| world.registry.has(&world.ecs, *e, key))
        .filter_map(|(id, _)| match world.get(id, "GraphSource", "graph") {
            Ok(Value::Str(p)) if !p.trim().is_empty() => {
                Some((id.to_string(), re_scene::doc_path(&p).unwrap_or(p)))
            }
            _ => None,
        })
        .collect();
    out.sort();
    out
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
                "No procedural source in this scene reads a graph file (GraphSource.graph).",
            )
            .weak(),
        );
        return;
    };
    let (source, path) = canvas
        .source
        .as_ref()
        .and_then(|s| sources.iter().find(|(id, _)| id == s))
        .unwrap_or(first)
        .clone();
    let mut chosen = source.clone();
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("re_editor.canvas.source")
            .selected_text(&source)
            .show_ui(ui, |ui| {
                for (id, p) in &sources {
                    ui.selectable_value(&mut chosen, id.clone(), format!("{id}  {p}"));
                }
            });
        ui.label(RichText::new(&path).weak());
        if world.unsaved_docs().contains(&path) {
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
        canvas.view = Some(build(
            &source,
            &path,
            &doc,
            registry,
            canvas.collapsed.get(&path),
        ));
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

    let mut viewer = Viewer {
        path: &path,
        registry,
        spots: canvas.spots.clone(),
        queued: Vec::new(),
    };
    let mut style = SnarlStyle::new();
    // The header draws its own collapse button, so a test can find it.
    style.collapsible = Some(false);
    SnarlWidget::new()
        .id_salt(("re_editor.canvas", &path))
        .style(style)
        .show(&mut view.snarl, &mut viewer, ui);
    let queued = viewer.queued;

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
    if canvas.log.len() > LOG {
        let extra = canvas.log.len() - LOG;
        canvas.log.drain(..extra);
    }
    if layout_changed {
        ed.layout_changed();
    }
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
    let auto = auto_layout(doc);
    let mut snarl = Snarl::new();
    let mut ids = HashMap::new();
    for n in &doc.nodes {
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
        let params = n
            .params
            .iter()
            .map(|(k, v)| format!("{k} = {}", short(v)))
            .collect();
        let pos = doc
            .layout
            .get(&n.id)
            .map(|p| egui::pos2(p[0], p[1]))
            .or_else(|| auto.get(&n.id).copied())
            .unwrap_or_default();
        let node = NodeView {
            id: n.id.clone(),
            kind: n.kind.clone(),
            doc: def.map(|d| d.doc),
            inputs,
            output,
            params,
        };
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

/// Positions for nodes the file does not place: a column per depth (the
/// longest path from a node without inputs), in file order down each column.
fn auto_layout(doc: &GraphDoc) -> HashMap<String, egui::Pos2> {
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
    let mut rows: HashMap<usize, usize> = HashMap::new();
    doc.nodes
        .iter()
        .map(|n| {
            let d = depth[n.id.as_str()];
            let row = rows.entry(d).or_default();
            let pos = egui::pos2(d as f32 * COLUMN, *row as f32 * ROW);
            *row += 1;
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
    registry: &'a NodeRegistry,
    spots: Spots,
    queued: Vec<Queued>,
}

impl Viewer<'_> {
    fn spot(&self, ui: &egui::Ui, spot: CanvasSpot<'_>, rect: egui::Rect) {
        record(&self.spots, ui.ctx(), ui.layer_id(), spot.key(), rect);
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
            for p in &n.params {
                label(ui, RichText::new(p).small().monospace());
            }
        });
    }

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<NodeView>) -> bool {
        true
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
        true
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
        self.queued.push(Queued::Connect(from.id, to.id));
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, _snarl: &mut Snarl<NodeView>) {
        self.queued.push(Queued::Disconnect(from.id, to.id));
    }

    fn drop_outputs(&mut self, pin: &OutPin, _snarl: &mut Snarl<NodeView>) {
        for to in &pin.remotes {
            self.queued.push(Queued::Disconnect(pin.id, *to));
        }
    }

    fn drop_inputs(&mut self, pin: &InPin, _snarl: &mut Snarl<NodeView>) {
        for from in &pin.remotes {
            self.queued.push(Queued::Disconnect(*from, pin.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use re_types::graph::NodeDoc;

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
        let at = auto_layout(&doc);
        assert_eq!(at["a"], egui::pos2(0.0, 0.0));
        assert_eq!(at["d"], egui::pos2(0.0, ROW));
        assert_eq!(at["b"], egui::pos2(COLUMN, 0.0));
        assert_eq!(at["c"], egui::pos2(2.0 * COLUMN, 0.0));
    }
}
