//! The node canvas (ADR-0044), driven with the pointer: every edit is a
//! `graph.*` command, and the open graph document is exactly what those
//! commands make of the file.
//!
//! These tests need a GPU adapter (lavapipe in CI); run them with
//! `--test-threads=1`.

use std::path::Path;

use re_app::{App, AppConfig};
use re_command::{AllowAll, Origin};
use re_editor::layout::{self, PanelId};
use re_editor::{CanvasSpot, HeadlessEditor, Panel};
use re_types::graph::GraphDoc;
use serde_json::{Value as Json, json};

const PATH: &str = "graphs/flat.graph.json";

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "camera", "name": "Camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 30.0, 40.0] } } },
    { "id": "island", "name": "Island", "components": {
        "GraphSource": { "graph": "graphs/flat.graph.json" },
        "Procedural": { "generator": "graph", "seed": 1 },
        "Transform": {} } }
  ]
}
"#;

/// `h` and `mask` combine into `land`, which `ground` meshes; `flat` is not
/// connected. No node has a position, so the canvas lays them out by depth.
fn graph() -> Json {
    json!({
        "format": "real-engine/graph", "version": 1, "name": "flat", "seed": 1,
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 9, "cell": 4.0, "height": 2.0 } },
            { "id": "mask", "kind": "terrain.island_mask", "version": 1, "params": { "size": 9 } },
            { "id": "land", "kind": "field.combine", "version": 1, "params": { "op": "mul" } },
            { "id": "ground", "kind": "terrain.mesh", "version": 2, "params": {} },
            { "id": "flat", "kind": "terrain.heightfield", "version": 1, "params": { "size": 9, "cell": 4.0, "height": 0.0 } }
        ],
        "edges": [
            { "from": ["h", "field"], "to": ["land", "a"] },
            { "from": ["mask", "field"], "to": ["land", "b"] },
            { "from": ["land", "field"], "to": ["ground", "field"] }
        ]
    })
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for d in ["scenes", "graphs"] {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    std::fs::write(
        dir.path().join("real.toml"),
        "[project]\nname = \"canvas\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("scenes/main.scene.json"), SCENE).unwrap();
    std::fs::write(
        dir.path().join(PATH),
        serde_json::to_string_pretty(&graph()).unwrap(),
    )
    .unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

/// The editor with the node canvas brought to the front.
fn editor(app: &mut App) -> HeadlessEditor {
    let mut ed = HeadlessEditor::new(app, 1280, 720).expect("a headless GPU");
    // The first frame reads the project's layout; rearrange after it.
    idle(&mut ed, app);
    let dock = ed.editor.dock_mut();
    let at = dock.find_tab(&PanelId::Canvas).unwrap();
    dock.set_active_tab(at).unwrap();
    for _ in 0..3 {
        idle(&mut ed, app);
    }
    assert!(ed.editor.rects().get(Panel::Canvas).is_some());
    assert!(ed.editor.rects().get(Panel::Viewport).is_none());
    ed
}

fn frame(
    ed: &mut HeadlessEditor,
    app: &mut App,
    mut events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
) {
    events.insert(0, egui::Event::ModifiersChanged(modifiers));
    let input = egui::RawInput {
        events,
        ..egui::RawInput::default()
    };
    ed.frame(app, &AllowAll, input).unwrap();
}

fn idle(ed: &mut HeadlessEditor, app: &mut App) {
    frame(ed, app, vec![], egui::Modifiers::NONE);
}

fn button(
    pos: egui::Pos2,
    button: egui::PointerButton,
    pressed: bool,
    modifiers: egui::Modifiers,
) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers,
    }
}

fn spot(ed: &HeadlessEditor, s: CanvasSpot<'_>) -> egui::Pos2 {
    ed.editor
        .canvas_spot(s)
        .unwrap_or_else(|| panic!("{s:?} is not on screen"))
        .center()
}

/// Presses at `from`, moves to `to` over a few frames and releases there.
fn drag(
    ed: &mut HeadlessEditor,
    app: &mut App,
    from: egui::Pos2,
    to: egui::Pos2,
    m: egui::Modifiers,
) {
    let primary = egui::PointerButton::Primary;
    frame(
        ed,
        app,
        vec![
            egui::Event::PointerMoved(from),
            button(from, primary, true, m),
        ],
        m,
    );
    for t in [0.2, 0.4, 0.6, 0.8, 1.0] {
        frame(
            ed,
            app,
            vec![egui::Event::PointerMoved(from + (to - from) * t)],
            m,
        );
    }
    frame(ed, app, vec![button(to, primary, false, m)], m);
    for _ in 0..2 {
        idle(ed, app);
    }
}

fn click_with(ed: &mut HeadlessEditor, app: &mut App, at: egui::Pos2, b: egui::PointerButton) {
    let m = egui::Modifiers::NONE;
    frame(
        ed,
        app,
        vec![
            egui::Event::PointerMoved(at),
            button(at, b, true, m),
            button(at, b, false, m),
        ],
        m,
    );
    for _ in 0..2 {
        idle(ed, app);
    }
}

/// Lets the collapse animation finish.
fn settle(ed: &mut HeadlessEditor, app: &mut App) {
    for _ in 0..20 {
        idle(ed, app);
    }
}

fn click(ed: &mut HeadlessEditor, app: &mut App, at: egui::Pos2) {
    click_with(ed, app, at, egui::PointerButton::Primary);
}

fn right_click(ed: &mut HeadlessEditor, app: &mut App, at: egui::Pos2) {
    click_with(ed, app, at, egui::PointerButton::Secondary);
}

fn open_doc(app: &App) -> GraphDoc {
    serde_json::from_value(
        app.world()
            .doc(PATH)
            .expect("the graph is open")
            .value
            .clone(),
    )
    .unwrap()
}

fn edges(app: &App) -> Vec<String> {
    let mut out: Vec<String> = open_doc(app)
        .edges
        .iter()
        .map(|e| format!("{}.{} -> {}.{}", e.from.0, e.from.1, e.to.0, e.to.1))
        .collect();
    out.sort();
    out
}

/// The commands the canvas sent since `from`, by name.
fn sent(ed: &HeadlessEditor, from: usize) -> Vec<&str> {
    ed.editor.canvas_commands()[from..]
        .iter()
        .map(|(n, _)| n.as_str())
        .collect()
}

/// The canvas edited the graph only through its commands: sending them to a
/// fresh app gives the same document, and every transaction was one of them.
fn replayed(dir: &Path, ed: &HeadlessEditor, app: &App) {
    let mut fresh = open(dir);
    for (name, args) in ed.editor.canvas_commands() {
        // A command the app refused is refused again.
        fresh.dispatch(name, args.clone(), false, &AllowAll, Origin::User);
    }
    assert_eq!(
        fresh.world().doc(PATH).map(|d| &d.value),
        app.world().doc(PATH).map(|d| &d.value)
    );
    let graph_entries = app
        .history()
        .log()
        .iter()
        .filter(|e| e.label.starts_with("graph."))
        .count();
    assert_eq!(
        graph_entries,
        app.history().log().len(),
        "only graph commands"
    );
    assert_eq!(fresh.history().log().len(), app.history().log().len());
    // Nothing was written: the file waits for a save.
    let on_disk: Json =
        serde_json::from_str(&std::fs::read_to_string(dir.join(PATH)).unwrap()).unwrap();
    assert_eq!(on_disk, graph());
}

#[test]
fn wires_are_connected_cut_and_rerouted_with_commands() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    assert_eq!(ed.editor.canvas_source(), Some("island"));
    assert!(
        ed.editor.canvas_commands().is_empty(),
        "drawing sends nothing"
    );
    assert!(app.world().doc(PATH).is_none(), "not opened until an edit");

    // Right-clicking an input cuts its wire.
    let land_b = spot(&ed, CanvasSpot::Input("land", "b"));
    right_click(&mut ed, &mut app, land_b);
    assert_eq!(
        sent(&ed, 0),
        ["graph.disconnect"],
        "{:?}",
        ed.editor.console()
    );
    assert_eq!(
        edges(&app),
        ["h.field -> land.a", "land.field -> ground.field"]
    );
    assert_eq!(app.world().unsaved_docs(), [PATH]);

    // Dragging from an output to an input connects them.
    let from = spot(&ed, CanvasSpot::Output("flat"));
    let to = spot(&ed, CanvasSpot::Input("land", "b"));
    drag(&mut ed, &mut app, from, to, egui::Modifiers::NONE);
    assert_eq!(sent(&ed, 1), ["graph.connect"], "{:?}", ed.editor.console());
    assert_eq!(
        edges(&app),
        [
            "flat.field -> land.b",
            "h.field -> land.a",
            "land.field -> ground.field"
        ]
    );

    // A wire that would close a cycle is refused, and the canvas still shows
    // the graph as it is: nothing more is sent.
    let log = app.history().log().len();
    let from = spot(&ed, CanvasSpot::Output("land"));
    let to = spot(&ed, CanvasSpot::Input("land", "a"));
    drag(&mut ed, &mut app, from, to, egui::Modifiers::NONE);
    assert_eq!(sent(&ed, 2), ["graph.connect"]);
    assert!(
        ed.editor
            .console()
            .iter()
            .any(|l| l.text.contains("RE4004")),
        "{:?}",
        ed.editor.console()
    );
    assert_eq!(app.history().log().len(), log);
    for _ in 0..3 {
        idle(&mut ed, &mut app);
    }
    assert_eq!(ed.editor.canvas_commands().len(), 3);

    // Ctrl-dragging an output's wires moves them to another output: snarl
    // cuts them itself, and the canvas sends the cut and the new wire.
    let from = spot(&ed, CanvasSpot::Output("h"));
    let to = spot(&ed, CanvasSpot::Output("mask"));
    drag(&mut ed, &mut app, from, to, egui::Modifiers::COMMAND);
    let mut last = sent(&ed, 3);
    last.sort();
    assert_eq!(
        last,
        ["graph.connect", "graph.disconnect"],
        "{:?}",
        ed.editor.console()
    );
    assert_eq!(
        edges(&app),
        [
            "flat.field -> land.b",
            "land.field -> ground.field",
            "mask.field -> land.a"
        ]
    );
    for _ in 0..3 {
        idle(&mut ed, &mut app);
    }
    assert_eq!(
        ed.editor.canvas_commands().len(),
        5,
        "the reroute is sent once"
    );

    replayed(dir.path(), &ed, &app);
    // Each command is one undo step.
    for _ in 0..4 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    assert_eq!(serde_json::to_value(open_doc(&app)).unwrap(), graph());
}

#[test]
fn nodes_are_moved_added_and_removed_with_commands() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);

    // Laid out by depth: inputs on the left, the mesh on the right.
    let x = |ed: &HeadlessEditor, n: &str| spot(ed, CanvasSpot::Collapse(n)).x;
    assert!(x(&ed, "h") < x(&ed, "land") && x(&ed, "land") < x(&ed, "ground"));
    assert_eq!(x(&ed, "h"), x(&ed, "mask"));

    // Dragging a node sends one move when the button is released.
    let header = spot(&ed, CanvasSpot::Header("ground"));
    drag(
        &mut ed,
        &mut app,
        header,
        header + egui::vec2(60.0, 40.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(
        sent(&ed, 0),
        ["graph.move_nodes"],
        "{:?}",
        ed.editor.console()
    );
    let doc = open_doc(&app);
    assert_eq!(doc.layout.keys().collect::<Vec<_>>(), ["ground"]);
    let moved = spot(&ed, CanvasSpot::Header("ground"));
    assert!(
        (moved - header - egui::vec2(60.0, 40.0)).length() < 2.0,
        "{moved:?}"
    );
    assert_eq!(app.history().log().len(), 1);

    // The canvas menu adds a node where it was opened.
    let canvas = ed.editor.rects().get(Panel::Canvas).unwrap();
    let empty = canvas.right_bottom() - egui::vec2(200.0, 60.0);
    right_click(&mut ed, &mut app, empty);
    let group = spot(&ed, CanvasSpot::MenuItem("field"));
    click(&mut ed, &mut app, group);
    let kind = spot(&ed, CanvasSpot::MenuItem("field.combine"));
    click(&mut ed, &mut app, kind);
    assert_eq!(
        sent(&ed, 1),
        ["graph.add_node"],
        "{:?}",
        ed.editor.console()
    );
    let doc = open_doc(&app);
    let added = doc.node("combine_1").expect("a new node");
    assert_eq!(added.kind, "field.combine");
    assert!(doc.layout.contains_key("combine_1"));
    let at = spot(&ed, CanvasSpot::Header("combine_1"));
    assert!((at - empty).length() < 100.0, "{at:?} near {empty:?}");

    // A node's menu removes it.
    let flat = spot(&ed, CanvasSpot::Header("flat"));
    right_click(&mut ed, &mut app, flat);
    let remove = spot(&ed, CanvasSpot::MenuItem("Remove node"));
    click(&mut ed, &mut app, remove);
    assert_eq!(
        sent(&ed, 2),
        ["graph.remove_node"],
        "{:?}",
        ed.editor.console()
    );
    assert!(open_doc(&app).node("flat").is_none());
    assert!(ed.editor.canvas_spot(CanvasSpot::Header("flat")).is_none());

    replayed(dir.path(), &ed, &app);
    for _ in 0..3 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    assert_eq!(serde_json::to_value(open_doc(&app)).unwrap(), graph());
}

#[test]
fn collapsed_nodes_are_saved_with_the_layout_not_the_graph() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    let open_height = spot(&ed, CanvasSpot::Header("land")).y;
    // A collapsed node draws its pins along its header.
    let pins_apart = |ed: &HeadlessEditor| {
        spot(ed, CanvasSpot::Input("land", "b")).y - spot(ed, CanvasSpot::Input("land", "a")).y
    };
    assert!(pins_apart(&ed) > 5.0);

    let toggle = spot(&ed, CanvasSpot::Collapse("land"));
    click(&mut ed, &mut app, toggle);
    settle(&mut ed, &mut app);
    assert!(pins_apart(&ed) < 1.0, "collapsed");
    assert!(ed.editor.canvas_commands().is_empty());
    assert!(app.history().log().is_empty());
    assert!(app.world().doc(PATH).is_none());
    let text = std::fs::read_to_string(layout::layout_path(dir.path())).unwrap();
    let saved: Json = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["collapsed"], json!({ PATH: ["land"] }));
    drop(ed);

    // The next editor shows it collapsed, in the same place.
    let mut ed = editor(&mut app);
    assert!(pins_apart(&ed) < 1.0, "still collapsed");
    assert_eq!(spot(&ed, CanvasSpot::Header("land")).y, open_height);
    let toggle = spot(&ed, CanvasSpot::Collapse("land"));
    click(&mut ed, &mut app, toggle);
    settle(&mut ed, &mut app);
    assert!(pins_apart(&ed) > 5.0, "expanded");
    let text = std::fs::read_to_string(layout::layout_path(dir.path())).unwrap();
    let saved: Json = serde_json::from_str(&text).unwrap();
    assert!(saved.get("collapsed").is_none(), "{text}");
}
