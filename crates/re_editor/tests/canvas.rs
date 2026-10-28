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
            { "id": "ground", "kind": "terrain.mesh", "version": 3, "params": {} },
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

/// The parameters node `id` sets in the open document.
fn params(app: &App, id: &str) -> serde_json::Map<String, Json> {
    open_doc(app).node(id).expect("the node").params.clone()
}

fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// Clicks the box at `at`, types `text` over what it shows, a key a frame,
/// and presses Enter.
fn type_into(ed: &mut HeadlessEditor, app: &mut App, at: egui::Pos2, text: &str) {
    click(ed, app, at);
    let none = egui::Modifiers::NONE;
    frame(
        ed,
        app,
        vec![key(egui::Key::A, egui::Modifiers::COMMAND)],
        none,
    );
    for ch in text.chars() {
        frame(ed, app, vec![egui::Event::Text(ch.to_string())], none);
    }
    frame(ed, app, vec![key(egui::Key::Enter, none)], none);
    for _ in 0..2 {
        idle(ed, app);
    }
}

#[test]
fn parameters_are_edited_with_their_widgets() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    assert!(
        ed.editor.canvas_commands().is_empty(),
        "drawing sends nothing"
    );

    // Dragging a number sends one `graph.set_default`, when it is released.
    let height = spot(&ed, CanvasSpot::Param("h", "height", ""));
    drag(
        &mut ed,
        &mut app,
        height,
        height + egui::vec2(60.0, 0.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(
        sent(&ed, 0),
        ["graph.set_default"],
        "{:?}",
        ed.editor.console()
    );
    let h = params(&app, "h");
    assert!(h["height"].as_f64().unwrap() > 2.0, "{h:?}");
    assert_eq!(app.history().log().len(), 1);

    // A choice is one click to open the list and one to pick.
    let noise = spot(&ed, CanvasSpot::Param("h", "noise", ""));
    click(&mut ed, &mut app, noise);
    let ridged = spot(&ed, CanvasSpot::Param("h", "noise", "=ridged"));
    click(&mut ed, &mut app, ridged);
    assert_eq!(
        sent(&ed, 1),
        ["graph.set_default"],
        "{:?}",
        ed.editor.console()
    );
    assert_eq!(params(&app, "h")["noise"], "ridged");

    // × removes what the node sets, so the kind's default shows again.
    let cell = spot(&ed, CanvasSpot::Param("h", "cell", "×"));
    click(&mut ed, &mut app, cell);
    assert_eq!(sent(&ed, 2), ["graph.set_default"]);
    assert!(!params(&app, "h").contains_key("cell"));
    assert!(
        ed.editor
            .canvas_spot(CanvasSpot::Param("h", "cell", "×"))
            .is_none()
    );

    // Typing into a parameter the node does not set sets it, on Enter.
    let octaves = spot(&ed, CanvasSpot::Param("h", "octaves", ""));
    type_into(&mut ed, &mut app, octaves, "7");
    assert_eq!(
        sent(&ed, 3),
        ["graph.set_default"],
        "{:?}",
        ed.editor.console()
    );
    assert_eq!(params(&app, "h")["octaves"], 7);

    // A value the kind refuses changes nothing, and says why.
    let size = spot(&ed, CanvasSpot::Param("mask", "size", ""));
    type_into(&mut ed, &mut app, size, "1");
    assert_eq!(sent(&ed, 4), ["graph.set_default"]);
    assert_eq!(params(&app, "mask")["size"], 9);
    assert_eq!(app.history().log().len(), 4);
    assert!(
        ed.editor.console().iter().any(|l| l.text.contains("size")),
        "{:?}",
        ed.editor.console()
    );

    replayed(dir.path(), &ed, &app);
    // Each edit is one undo step.
    for _ in 0..4 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    assert_eq!(serde_json::to_value(open_doc(&app)).unwrap(), graph());
}

/// A value of another type than its parameter's is shown as JSON text, with
/// one `RE8850` warning, and typing a value of the type puts the widget back.
#[test]
fn a_value_of_another_type_is_shown_as_json_text() {
    let dir = project();
    let mut doc = graph();
    doc["nodes"][0]["params"]["height"] = json!("tall");
    std::fs::write(
        dir.path().join(PATH),
        serde_json::to_string_pretty(&doc).unwrap(),
    )
    .unwrap();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    let warnings = |ed: &HeadlessEditor| {
        ed.editor
            .console()
            .iter()
            .filter(|l| l.text.contains("RE8850"))
            .count()
    };
    assert_eq!(warnings(&ed), 1, "{:?}", ed.editor.console());
    for _ in 0..3 {
        idle(&mut ed, &mut app);
    }
    assert_eq!(warnings(&ed), 1, "reported once");

    let height = spot(&ed, CanvasSpot::Param("h", "height", ""));
    type_into(&mut ed, &mut app, height, "\"short\"");
    assert_eq!(sent(&ed, 0), Vec::<&str>::new(), "a string is not a number");
    assert_eq!(warnings(&ed), 2, "{:?}", ed.editor.console());
    type_into(&mut ed, &mut app, height, "3.5");
    assert_eq!(
        sent(&ed, 0),
        ["graph.set_default"],
        "{:?}",
        ed.editor.console()
    );
    assert_eq!(params(&app, "h")["height"], 3.5);
    // The widget is a number again.
    let height = spot(&ed, CanvasSpot::Param("h", "height", ""));
    drag(
        &mut ed,
        &mut app,
        height,
        height + egui::vec2(30.0, 0.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(
        sent(&ed, 1),
        ["graph.set_default"],
        "{:?}",
        ed.editor.console()
    );
    assert!(params(&app, "h")["height"].as_f64().unwrap() > 3.5);
}

/// A builder script that makes the graph [`graph`] holds.
const BUILDER: &str = r#"local g = proc.graph("flat")
local h = g:node("terrain.heightfield", { size = 9, cell = 4.0, height = 2.0 }, nil, "h")
local mask = g:node("terrain.island_mask", { size = 9 }, nil, "mask")
local land = g:node("field.combine", { op = "mul" }, { a = h, b = mask }, "land")
g:node("terrain.mesh", {}, { field = land }, "ground")
g:node("terrain.heightfield", { size = 9, cell = 4.0, height = 0.0 }, nil, "flat")
return g
"#;

const SCRIPT: &str = "scripts/tools/flat.luau";

/// A project whose island's graph the builder script [`BUILDER`] makes.
fn builder_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for d in ["scenes", "scripts/tools"] {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    std::fs::write(
        dir.path().join("real.toml"),
        "[project]\nname = \"canvas\"\n",
    )
    .unwrap();
    let scene = SCENE.replace(
        r#""graph": "graphs/flat.graph.json""#,
        &format!(r#""builder": "{SCRIPT}""#),
    );
    std::fs::write(dir.path().join("scenes/main.scene.json"), scene).unwrap();
    std::fs::write(dir.path().join(SCRIPT), BUILDER).unwrap();
    dir
}

/// Regenerates the island; the hash of the fragment its graph made.
fn fragment(app: &mut App) -> String {
    let out = app.dispatch(
        "graph.regenerate",
        json!({ "source": "island" }),
        false,
        &AllowAll,
        Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    app.world()
        .ecs
        .resource::<re_proc::GraphStore>()
        .report("island")
        .and_then(|r| r.fragment_hash)
        .expect("a fragment hash")
}

fn source_field(app: &App, name: &str) -> String {
    match app.world().get("island", "GraphSource", name) {
        Ok(re_reflect::Value::Str(s)) => s,
        other => panic!("GraphSource.{name}: {other:?}"),
    }
}

/// The console lines that mention `text`.
fn lines_with(ed: &HeadlessEditor, text: &str) -> usize {
    ed.editor
        .console()
        .iter()
        .filter(|l| l.text.contains(text))
        .count()
}

#[test]
fn a_builder_graph_is_read_only_until_it_is_converted_to_a_graph_file() {
    let dir = builder_project();
    let mut app = open(dir.path());
    let built = fragment(&mut app);
    let mut ed = editor(&mut app);
    assert_eq!(ed.editor.canvas_source(), Some("island"));
    let none = egui::Modifiers::NONE;

    // Laid out by depth, as a graph file without positions is.
    let x = |ed: &HeadlessEditor, n: &str| spot(ed, CanvasSpot::Collapse(n)).x;
    assert!(x(&ed, "h") < x(&ed, "land") && x(&ed, "land") < x(&ed, "ground"));

    // Nothing sends a command: a wire dragged, a node moved, the menus, a
    // parameter's widget dragged and clicked.
    let (from, to) = (
        spot(&ed, CanvasSpot::Output("flat")),
        spot(&ed, CanvasSpot::Input("ground", "field")),
    );
    drag(&mut ed, &mut app, from, to, none);
    let header = spot(&ed, CanvasSpot::Header("flat"));
    drag(
        &mut ed,
        &mut app,
        header,
        header + egui::vec2(80.0, 40.0),
        none,
    );
    assert_eq!(
        spot(&ed, CanvasSpot::Header("flat")),
        header,
        "the node goes back"
    );
    let land = spot(&ed, CanvasSpot::Header("land"));
    right_click(&mut ed, &mut app, land);
    assert!(
        ed.editor
            .canvas_spot(CanvasSpot::MenuItem("Remove node"))
            .is_none()
    );
    let canvas = ed.editor.rects().get(Panel::Canvas).unwrap();
    right_click(
        &mut ed,
        &mut app,
        canvas.right_bottom() - egui::vec2(200.0, 60.0),
    );
    assert!(
        ed.editor
            .canvas_spot(CanvasSpot::MenuItem("field"))
            .is_none()
    );
    let height = spot(&ed, CanvasSpot::Param("h", "height", ""));
    drag(
        &mut ed,
        &mut app,
        height,
        height + egui::vec2(60.0, 0.0),
        none,
    );
    click(&mut ed, &mut app, height);
    assert!(ed.editor.canvas_commands().is_empty());
    assert!(app.history().log().is_empty());
    assert_eq!(lines_with(&ed, "RE8852"), 1, "{:?}", ed.editor.console());

    // "Open script" names the script.
    let open_script = spot(&ed, CanvasSpot::Button("Open script"));
    click(&mut ed, &mut app, open_script);
    let named = format!("is {SCRIPT}");
    assert_eq!(lines_with(&ed, &named), 1, "{:?}", ed.editor.console());

    // "Convert to a graph file": one command, with the positions the nodes
    // are drawn at, so they stay where they are.
    let convert = spot(&ed, CanvasSpot::Button("Convert to a graph file"));
    click(&mut ed, &mut app, convert);
    assert_eq!(
        sent(&ed, 0),
        ["proc.convert_builder"],
        "{:?}",
        ed.editor.console()
    );
    assert_eq!(app.history().log().len(), 1);
    const FILE: &str = "graphs/island.graph.json";
    assert_eq!(source_field(&app, "graph"), FILE);
    assert_eq!(source_field(&app, "builder"), "");
    let doc: GraphDoc =
        serde_json::from_value(app.world().doc(FILE).expect("open").value.clone()).unwrap();
    assert_eq!(doc.layout.len(), 5, "{:?}", doc.layout);
    assert_eq!(spot(&ed, CanvasSpot::Header("land")), land);
    assert!(
        ed.editor
            .canvas_spot(CanvasSpot::Button("Open script"))
            .is_none()
    );
    assert_eq!(fragment(&mut app), built);

    // The canvas edits the file now.
    let (from, to) = (
        spot(&ed, CanvasSpot::Output("flat")),
        spot(&ed, CanvasSpot::Input("ground", "field")),
    );
    drag(&mut ed, &mut app, from, to, none);
    assert_eq!(sent(&ed, 1), ["graph.connect"], "{:?}", ed.editor.console());

    // Two undos: the wire, then the conversion. The builder makes the graph
    // again, and the canvas shows it read-only.
    for _ in 0..2 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    for _ in 0..2 {
        idle(&mut ed, &mut app);
    }
    assert_eq!(source_field(&app, "builder"), SCRIPT);
    assert!(app.world().doc(FILE).expect("open").is_absent());
    assert!(
        ed.editor
            .canvas_spot(CanvasSpot::Button("Open script"))
            .is_some()
    );
    assert_eq!(fragment(&mut app), built);
}
