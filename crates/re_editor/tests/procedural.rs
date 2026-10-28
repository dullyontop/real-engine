//! The Procedural panel (ADR-0044 amendment §3 and §8), driven with the
//! pointer: the seed scrubber sends one `graph.regenerate` per release,
//! overrides are edited with their parameter's widget, the picker adds one,
//! a source's structural and deletion overrides are cleared and restored
//! from their rows, and the Hierarchy's menu reverts, replaces, inserts,
//! clears and bakes a source's output.
//!
//! These tests need a GPU adapter (lavapipe in CI); run them with
//! `--test-threads=1`.

use std::cell::RefCell;
use std::path::Path;

use re_app::{App, AppConfig};
use re_command::{AllowAll, CommandSpec, Origin, PolicyCheck};
use re_core::Diagnostic;
use re_editor::layout::PanelId;
use re_editor::{HeadlessEditor, Panel, ProceduralSpot, Selection};
use re_reflect::Value;
use serde_json::{Value as Json, json};

const PATH: &str = "graphs/flat.graph.json";

/// The overrides the island starts with: one of a parameter of `h`, and one
/// of a node the graph does not have.
const PARAMS: &str = r#"{"gone":{"x":1},"h":{"height":3.0}}"#;

fn scene(params: &str) -> String {
    let scene = json!({
        "format": "real-engine/scene",
        "version": 1,
        "entities": [
            { "id": "camera", "name": "Camera", "components": {
                "Camera": { "active": true },
                "Transform": { "translation": [0.0, 30.0, 40.0] } } },
            { "id": "island", "name": "Island", "components": {
                "GraphSource": { "graph": PATH, "params": params },
                "Procedural": { "generator": "graph", "seed": 1 },
                "Transform": {} } }
        ]
    });
    serde_json::to_string_pretty(&scene).unwrap()
}

fn graph() -> Json {
    json!({
        "format": "real-engine/graph", "version": 1, "name": "flat", "seed": 1,
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 9, "cell": 4.0, "height": 2.0 } },
            { "id": "ground", "kind": "terrain.mesh", "version": 3, "params": {} }
        ],
        "edges": [
            { "from": ["h", "field"], "to": ["ground", "field"] }
        ]
    })
}

fn project(params: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for d in ["scenes", "graphs"] {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    std::fs::write(
        dir.path().join("real.toml"),
        "[project]\nname = \"procedural\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("scenes/main.scene.json"), scene(params)).unwrap();
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

/// The editor with the Procedural panel filling the window.
fn editor(app: &mut App) -> HeadlessEditor {
    let ed = editor_showing(app, PanelId::Graph);
    assert!(ed.editor.rects().get(Panel::Graph).is_some());
    ed
}

/// The editor with `first` filling the window, the other panels behind it.
fn editor_showing(app: &mut App, first: PanelId) -> HeadlessEditor {
    let mut ed = HeadlessEditor::new(app, 1280, 720).expect("a headless GPU");
    // The first frame reads the project's layout; rearrange after it.
    idle(&mut ed, app);
    let mut tabs = vec![first];
    tabs.extend(PanelId::ALL.into_iter().filter(|p| *p != first));
    *ed.editor.dock_mut() = egui_dock::DockState::new(tabs);
    for _ in 0..3 {
        idle(&mut ed, app);
    }
    ed
}

fn frame(ed: &mut HeadlessEditor, app: &mut App, events: Vec<egui::Event>) {
    frame_with(ed, app, &AllowAll, events);
}

fn frame_with(
    ed: &mut HeadlessEditor,
    app: &mut App,
    policy: &dyn PolicyCheck,
    mut events: Vec<egui::Event>,
) {
    events.insert(0, egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    let input = egui::RawInput {
        events,
        ..egui::RawInput::default()
    };
    ed.frame(app, policy, input).unwrap();
}

/// A policy that allows everything and keeps each call it is asked about.
#[derive(Default)]
struct Calls(RefCell<Vec<(String, Json)>>);

impl PolicyCheck for Calls {
    fn check(&self, spec: &CommandSpec, args: &Json, _dry_run: bool) -> Result<(), Diagnostic> {
        self.0.borrow_mut().push((spec.name.clone(), args.clone()));
        Ok(())
    }
}

/// The commands a click at `at` sends.
fn clicked(ed: &mut HeadlessEditor, app: &mut App, at: egui::Pos2) -> Vec<(String, Json)> {
    let calls = Calls::default();
    let events = vec![
        egui::Event::PointerMoved(at),
        button(at, true),
        button(at, false),
    ];
    frame_with(ed, app, &calls, events);
    for _ in 0..2 {
        frame_with(ed, app, &calls, vec![]);
    }
    calls.0.into_inner()
}

fn idle(ed: &mut HeadlessEditor, app: &mut App) {
    frame(ed, app, vec![]);
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    pointer(pos, egui::PointerButton::Primary, pressed)
}

fn pointer(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn spot(ed: &HeadlessEditor, s: ProceduralSpot<'_>) -> egui::Pos2 {
    ed.editor
        .procedural_spot(s)
        .unwrap_or_else(|| panic!("{s:?} is not on screen"))
        .center()
}

fn click(ed: &mut HeadlessEditor, app: &mut App, at: egui::Pos2) {
    frame(
        ed,
        app,
        vec![
            egui::Event::PointerMoved(at),
            button(at, true),
            button(at, false),
        ],
    );
    for _ in 0..2 {
        idle(ed, app);
    }
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
    frame(ed, app, vec![key(egui::Key::A, egui::Modifiers::COMMAND)]);
    for ch in text.chars() {
        frame(ed, app, vec![egui::Event::Text(ch.to_string())]);
    }
    frame(ed, app, vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
    for _ in 0..2 {
        idle(ed, app);
    }
}

fn seed(app: &App) -> i64 {
    match app.world().get("island", "Procedural", "seed") {
        Ok(Value::Int(s)) => s,
        other => panic!("no seed: {other:?}"),
    }
}

/// `GraphSource.params` as the source holds it.
fn params_text(app: &App) -> String {
    match app.world().get("island", "GraphSource", "params") {
        Ok(Value::Str(s)) => s,
        other => panic!("no params: {other:?}"),
    }
}

fn params(app: &App) -> Json {
    let text = params_text(app);
    if text.is_empty() {
        return json!({});
    }
    serde_json::from_str(&text).unwrap()
}

fn log_len(app: &App) -> usize {
    app.history().log().len()
}

fn last_label(app: &App) -> String {
    app.history()
        .log()
        .last()
        .expect("a transaction")
        .label
        .clone()
}

/// The console lines that mention `code`.
fn lines_with(ed: &HeadlessEditor, code: &str) -> usize {
    ed.editor
        .console()
        .iter()
        .filter(|l| l.text.contains(code))
        .count()
}

#[test]
fn the_seed_scrubber_sends_one_regenerate_per_release() {
    let dir = project("");
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    assert_eq!(log_len(&app), 0, "drawing sends nothing");
    assert_eq!(seed(&app), 1);

    // ▶ and ◀ step the seed by one, each one `graph.regenerate`.
    let next = spot(&ed, ProceduralSpot::SeedButton("island", "▶"));
    click(&mut ed, &mut app, next);
    assert_eq!(seed(&app), 2, "{:?}", ed.editor.console());
    assert_eq!(last_label(&app), "graph.regenerate island seed 2");
    let back = spot(&ed, ProceduralSpot::SeedButton("island", "◀"));
    click(&mut ed, &mut app, back);
    assert_eq!(seed(&app), 1);

    // "New seed" scrambles it, the same way every time.
    let new = spot(&ed, ProceduralSpot::SeedButton("island", "New seed"));
    click(&mut ed, &mut app, new);
    let scrambled = seed(&app);
    assert!(scrambled != 1 && scrambled >= 0, "{scrambled}");
    assert_eq!(log_len(&app), 3);

    // Dragging across the strip moves one seed every 8 points and sends
    // nothing until the button is released.
    let strip = spot(&ed, ProceduralSpot::Strip("island"));
    frame(
        &mut ed,
        &mut app,
        vec![egui::Event::PointerMoved(strip), button(strip, true)],
    );
    for dx in [10.0, 20.0, 30.0, 41.0] {
        let at = strip + egui::vec2(dx, 0.0);
        frame(&mut ed, &mut app, vec![egui::Event::PointerMoved(at)]);
    }
    assert_eq!(log_len(&app), 3, "nothing is sent during the drag");
    assert_eq!(seed(&app), scrambled);
    frame(
        &mut ed,
        &mut app,
        vec![button(strip + egui::vec2(41.0, 0.0), false)],
    );
    for _ in 0..2 {
        idle(&mut ed, &mut app);
    }
    assert_eq!(seed(&app), scrambled + 5, "{:?}", ed.editor.console());
    assert_eq!(log_len(&app), 4, "one transaction for the drag");

    // A seed typed into the field is sent on Enter.
    let field = spot(&ed, ProceduralSpot::Seed("island"));
    type_into(&mut ed, &mut app, field, "77");
    assert_eq!(seed(&app), 77, "{:?}", ed.editor.console());
    assert_eq!(log_len(&app), 5);

    // "Regenerate" runs the generator again with the seed the source has:
    // no transaction.
    let again = spot(&ed, ProceduralSpot::SeedButton("island", "Regenerate"));
    let calls = clicked(&mut ed, &mut app, again);
    assert_eq!(
        calls,
        [(
            "graph.regenerate".to_string(),
            json!({ "source": "island" })
        )]
    );
    assert_eq!(seed(&app), 77);
    assert_eq!(log_len(&app), 5);

    // Each seed is one undo step.
    for _ in 0..5 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    assert_eq!(seed(&app), 1);
}

#[test]
fn overrides_are_edited_with_their_parameter_widgets() {
    let dir = project(PARAMS);
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    assert_eq!(log_len(&app), 0, "drawing sends nothing");

    // A number override is a drag, sent once when released.
    let height = spot(&ed, ProceduralSpot::Override("island", "h", "height", ""));
    let primary = [height, height + egui::vec2(60.0, 0.0)];
    frame(
        &mut ed,
        &mut app,
        vec![
            egui::Event::PointerMoved(primary[0]),
            button(primary[0], true),
        ],
    );
    for t in [0.25, 0.5, 0.75, 1.0] {
        let at = primary[0] + (primary[1] - primary[0]) * t;
        frame(&mut ed, &mut app, vec![egui::Event::PointerMoved(at)]);
    }
    frame(&mut ed, &mut app, vec![button(primary[1], false)]);
    for _ in 0..2 {
        idle(&mut ed, &mut app);
    }
    assert_eq!(log_len(&app), 1, "{:?}", ed.editor.console());
    assert_eq!(last_label(&app), "graph.set_param h.height");
    let p = params(&app);
    assert!(p["h"]["height"].as_f64().unwrap() > 3.0, "{p}");

    // An override of a node the graph does not have is listed with
    // "Remove", which writes the overrides without it.
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Override("island", "gone", "x", "×"))
            .is_none()
    );
    let remove = spot(
        &ed,
        ProceduralSpot::Override("island", "gone", "x", "Remove"),
    );
    click(&mut ed, &mut app, remove);
    assert_eq!(log_len(&app), 2, "{:?}", ed.editor.console());
    assert!(params(&app).get("gone").is_none(), "{}", params_text(&app));

    // × removes an override.
    let x = spot(&ed, ProceduralSpot::Override("island", "h", "height", "×"));
    click(&mut ed, &mut app, x);
    assert_eq!(log_len(&app), 3, "{:?}", ed.editor.console());
    assert_eq!(params(&app), json!({}));

    // Each edit is one undo step, and undo gives the text back exactly.
    for _ in 0..3 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    assert_eq!(params_text(&app), PARAMS);
}

#[test]
fn the_picker_adds_an_override_from_the_value_in_use() {
    let dir = project(PARAMS);
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Picker("island", "Set"))
            .is_none(),
        "no value before a node and a parameter are picked"
    );

    // The node list names the graph's nodes and their kinds.
    let nodes = spot(&ed, ProceduralSpot::Picker("island", "node"));
    click(&mut ed, &mut app, nodes);
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Picker("island", "node=ground"))
            .is_some()
    );
    let h = spot(&ed, ProceduralSpot::Picker("island", "node=h"));
    click(&mut ed, &mut app, h);

    // The parameter list leaves out what the source already overrides.
    let list = spot(&ed, ProceduralSpot::Picker("island", "param"));
    click(&mut ed, &mut app, list);
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Picker("island", "param=height"))
            .is_none()
    );
    let noise = spot(&ed, ProceduralSpot::Picker("island", "param=noise"));
    click(&mut ed, &mut app, noise);

    // A choice: pick "ridged" from its list, then "Set".
    let choice = spot(&ed, ProceduralSpot::Picker("island", "value"));
    click(&mut ed, &mut app, choice);
    let ridged = spot(&ed, ProceduralSpot::Picker("island", "value/=ridged"));
    click(&mut ed, &mut app, ridged);
    assert_eq!(log_len(&app), 0, "nothing is sent before Set");
    let set = spot(&ed, ProceduralSpot::Picker("island", "Set"));
    click(&mut ed, &mut app, set);
    assert_eq!(log_len(&app), 1, "{:?}", ed.editor.console());
    assert_eq!(last_label(&app), "graph.set_param h.noise");
    assert_eq!(params(&app)["h"]["noise"], "ridged");
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Picker("island", "Set"))
            .is_none(),
        "the picker starts again"
    );
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Override("island", "h", "noise", ""))
            .is_some(),
        "the new override is listed"
    );

    // A number starts from the value the graph gives it (the node's 4.0);
    // a typed value is kept until Set.
    let nodes = spot(&ed, ProceduralSpot::Picker("island", "node"));
    click(&mut ed, &mut app, nodes);
    let h = spot(&ed, ProceduralSpot::Picker("island", "node=h"));
    click(&mut ed, &mut app, h);
    let list = spot(&ed, ProceduralSpot::Picker("island", "param"));
    click(&mut ed, &mut app, list);
    let cell = spot(&ed, ProceduralSpot::Picker("island", "param=cell"));
    click(&mut ed, &mut app, cell);
    let set = spot(&ed, ProceduralSpot::Picker("island", "Set"));
    click(&mut ed, &mut app, set);
    assert_eq!(params(&app)["h"]["cell"], 4.0, "{:?}", ed.editor.console());
    let nodes = spot(&ed, ProceduralSpot::Picker("island", "node"));
    click(&mut ed, &mut app, nodes);
    let h = spot(&ed, ProceduralSpot::Picker("island", "node=h"));
    click(&mut ed, &mut app, h);
    let list = spot(&ed, ProceduralSpot::Picker("island", "param"));
    click(&mut ed, &mut app, list);
    let octaves = spot(&ed, ProceduralSpot::Picker("island", "param=octaves"));
    click(&mut ed, &mut app, octaves);
    let value = spot(&ed, ProceduralSpot::Picker("island", "value"));
    type_into(&mut ed, &mut app, value, "7");
    assert_eq!(log_len(&app), 2, "typing sends nothing before Set");
    let set = spot(&ed, ProceduralSpot::Picker("island", "Set"));
    click(&mut ed, &mut app, set);
    assert_eq!(log_len(&app), 3, "{:?}", ed.editor.console());
    assert_eq!(params(&app)["h"]["octaves"], 7);

    for _ in 0..3 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    assert_eq!(params_text(&app), PARAMS);
}

/// An override of another type than its parameter's is shown as JSON text
/// with one `RE8850` warning; text that is not a value of the type is
/// refused with another, and a value of the type is set.
#[test]
fn an_override_of_another_type_is_shown_as_json_text() {
    let dir = project(r#"{"h":{"height":"tall"}}"#);
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    for _ in 0..3 {
        idle(&mut ed, &mut app);
    }
    assert_eq!(lines_with(&ed, "RE8850"), 1, "{:?}", ed.editor.console());

    let text = spot(&ed, ProceduralSpot::Override("island", "h", "height", ""));
    type_into(&mut ed, &mut app, text, "[1");
    assert_eq!(lines_with(&ed, "RE8850"), 2, "{:?}", ed.editor.console());
    assert_eq!(log_len(&app), 0);

    let text = spot(&ed, ProceduralSpot::Override("island", "h", "height", ""));
    type_into(&mut ed, &mut app, text, "2.5");
    assert_eq!(log_len(&app), 1, "{:?}", ed.editor.console());
    assert_eq!(params(&app)["h"]["height"], 2.5);
    for _ in 0..3 {
        idle(&mut ed, &mut app);
    }
    assert_eq!(lines_with(&ed, "RE8850"), 2, "reported once");
}

/// Runs `name` as the person would, through the App (the edit frame
/// regenerates).
fn run(app: &mut App, name: &str, args: Json) {
    let out = app.dispatch(name, args, false, &AllowAll, Origin::User);
    assert!(out.ok, "{name}: {:#?}", out.diagnostics);
}

/// The ids of the entities the island generates.
fn generated(app: &App) -> Vec<String> {
    let mut ids: Vec<String> = app
        .world()
        .ids()
        .iter()
        .map(|(id, _)| id.to_string())
        .filter(|id| id.starts_with("island/"))
        .collect();
    ids.sort();
    ids
}

#[test]
fn structural_and_deletion_overrides_are_cleared_and_restored_from_their_rows() {
    let dir = project("{}");
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    let before = generated(&app);
    let id = before
        .first()
        .expect("the island generates an entity")
        .clone();
    let key = id.strip_prefix("island/").unwrap().to_string();
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Kept("island", &key, "Restore"))
            .is_none(),
        "nothing kept yet"
    );

    // A deletion and an insert.
    run(&mut app, "entity.delete", json!({ "id": id }));
    run(
        &mut app,
        "proc.insert",
        json!({ "source": "island", "key": "lamp", "components": { "Transform": {} } }),
    );
    assert!(!generated(&app).contains(&id));
    assert!(generated(&app).contains(&"island/+lamp".to_string()));
    for _ in 0..2 {
        idle(&mut ed, &mut app);
    }
    let steps = log_len(&app);
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Kept("island", &key, "Clear"))
            .is_none(),
        "a deletion has no Clear"
    );

    // "Restore" brings the deleted entity back: one proc.revert, one step.
    let restore = spot(&ed, ProceduralSpot::Kept("island", &key, "Restore"));
    assert_eq!(
        clicked(&mut ed, &mut app, restore),
        [(
            "proc.revert".to_string(),
            json!({ "source": "island", "key": key })
        )]
    );
    assert!(generated(&app).contains(&id));
    assert_eq!(log_len(&app), steps + 1);
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Kept("island", &key, "Restore"))
            .is_none()
    );

    // "Clear" removes the insert.
    let clear = spot(&ed, ProceduralSpot::Kept("island", "+lamp", "Clear"));
    assert_eq!(
        clicked(&mut ed, &mut app, clear),
        [(
            "proc.clear_structure".to_string(),
            json!({ "source": "island", "key": "+lamp" })
        )]
    );
    assert_eq!(generated(&app), before);
    assert_eq!(log_len(&app), steps + 2);
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Kept("island", "+lamp", "Clear"))
            .is_none()
    );

    // Undo lists them again.
    for _ in 0..2 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    for _ in 0..2 {
        idle(&mut ed, &mut app);
    }
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Kept("island", &key, "Restore"))
            .is_some()
    );
    assert!(
        ed.editor
            .procedural_spot(ProceduralSpot::Kept("island", "+lamp", "Clear"))
            .is_some()
    );
}

const ROCK: &str = r#"{
  "format": "real-engine/prefab",
  "version": 1,
  "entities": [
    { "id": "rock", "name": "Rock", "components": { "Transform": { "translation": [0, 5, 0] } } }
  ]
}"#;

const ROCK_PATH: &str = "prefabs/rock.prefab.json";

/// Right-clicks the Hierarchy row of `id` and lets the selection the click
/// made settle: its menu is open.
fn row_menu(ed: &mut HeadlessEditor, app: &mut App, id: &str) {
    let at = ed
        .editor
        .hierarchy_row(id)
        .unwrap_or_else(|| panic!("the row of {id} is shown"))
        .center();
    let right = egui::PointerButton::Secondary;
    let events = vec![
        egui::Event::PointerMoved(at),
        pointer(at, right, true),
        pointer(at, right, false),
    ];
    frame(ed, app, events);
    idle(ed, app);
}

/// The targets of the island's structural overrides.
fn structure(app: &App) -> Vec<String> {
    let world = app.world();
    let island = world.entity("island").unwrap();
    world
        .ecs
        .get::<re_scene::Overrides>(island)
        .map(|o| {
            o.0.iter()
                .filter(|o| o.structural_kind().is_some())
                .map(|o| o.target().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Where the open menu shows `label`.
fn entry(ed: &HeadlessEditor, label: &str) -> egui::Pos2 {
    ed.editor
        .menu_item(label)
        .unwrap_or_else(|| panic!("the menu shows {label}"))
        .center()
}

/// Opens the open menu's submenu `label` by resting the pointer on it.
fn submenu(ed: &mut HeadlessEditor, app: &mut App, label: &str) {
    let at = entry(ed, label);
    for _ in 0..3 {
        frame(ed, app, vec![egui::Event::PointerMoved(at)]);
    }
}

fn closes(ed: &mut HeadlessEditor, app: &mut App) {
    frame(ed, app, vec![key(egui::Key::Escape, egui::Modifiers::NONE)]);
    idle(ed, app);
}

#[test]
fn the_hierarchy_menu_reverts_replaces_inserts_clears_and_bakes_the_output() {
    let dir = project("{}");
    std::fs::create_dir_all(dir.path().join("prefabs")).unwrap();
    std::fs::write(dir.path().join(ROCK_PATH), ROCK).unwrap();
    let mut app = open(dir.path());
    let mut ed = editor_showing(&mut app, PanelId::Hierarchy);
    ed.editor.wait_for_assets(&mut app);
    let before = generated(&app);
    let id = before
        .first()
        .expect("the island generates an entity")
        .clone();
    let key = id.strip_prefix("island/").unwrap().to_string();
    // Selecting the generated entity unfolds the island's row; the space
    // under the rows takes the clicks the row would get until the unfolding
    // is drawn whole.
    ed.editor
        .set_selection(Selection::from_ids([id.clone()], None));
    for _ in 0..10 {
        idle(&mut ed, &mut app);
    }

    // Nothing is edited yet: "Revert overrides" and "Clear structure" send
    // nothing.
    for label in ["Revert overrides", "Clear structure"] {
        row_menu(&mut ed, &mut app, &id);
        let at = entry(&ed, label);
        assert_eq!(clicked(&mut ed, &mut app, at), [], "{label} is disabled");
        closes(&mut ed, &mut app);
    }

    // An edited field is put back by one proc.revert.
    let generated_at = app.world().get(&id, "Transform", "translation").unwrap();
    run(
        &mut app,
        "entity.set",
        json!({ "id": id, "component": "Transform", "field": "translation", "value": [0.0, 7.0, 0.0] }),
    );
    let steps = log_len(&app);
    row_menu(&mut ed, &mut app, &id);
    let at = entry(&ed, "Revert overrides");
    assert_eq!(
        clicked(&mut ed, &mut app, at),
        [("proc.revert".to_string(), json!({ "id": id }))]
    );
    assert_eq!(
        app.world().get(&id, "Transform", "translation").unwrap(),
        generated_at
    );
    assert_eq!(log_len(&app), steps + 1);
    assert!(
        ed.editor.menu_item("Revert overrides").is_none(),
        "the menu closed"
    );

    // "Replace with prefab" lists the project's prefabs.
    row_menu(&mut ed, &mut app, &id);
    submenu(&mut ed, &mut app, "Replace with prefab");
    let at = entry(&ed, ROCK_PATH);
    assert_eq!(
        clicked(&mut ed, &mut app, at),
        [(
            "proc.replace".to_string(),
            json!({ "id": id, "prefab": ROCK_PATH })
        )]
    );
    assert_eq!(structure(&app), std::slice::from_ref(&key));
    assert_eq!(log_len(&app), steps + 2);

    // "Clear structure" takes the replacement away again.
    row_menu(&mut ed, &mut app, &id);
    let at = entry(&ed, "Clear structure");
    assert_eq!(
        clicked(&mut ed, &mut app, at),
        [(
            "proc.clear_structure".to_string(),
            json!({ "source": "island", "key": key })
        )]
    );
    assert!(structure(&app).is_empty());
    assert_eq!(log_len(&app), steps + 3);

    // "Insert prefab" on the source adds it to the output under a free key.
    for (n, inserted) in [(1, "rock"), (2, "rock2")] {
        row_menu(&mut ed, &mut app, "island");
        submenu(&mut ed, &mut app, "Insert prefab");
        let at = entry(&ed, ROCK_PATH);
        assert_eq!(
            clicked(&mut ed, &mut app, at),
            [(
                "proc.insert".to_string(),
                json!({ "source": "island", "key": inserted, "prefab": ROCK_PATH })
            )]
        );
        assert!(generated(&app).contains(&format!("island/+{inserted}")));
        assert_eq!(log_len(&app), steps + 3 + n);
    }

    // "Bake" on the generated entity bakes it, and the source keeps
    // generating the rest.
    ed.editor
        .set_selection(Selection::from_ids([id.clone()], None));
    idle(&mut ed, &mut app);
    row_menu(&mut ed, &mut app, &id);
    let at = entry(&ed, "Bake");
    assert_eq!(
        clicked(&mut ed, &mut app, at),
        [(
            "proc.bake".to_string(),
            json!({ "source": "island", "keys": [key] })
        )]
    );
    assert!(!generated(&app).contains(&id), "{:?}", ed.editor.console());
    // Baked ids are `<source>.<key>`, a key's `/` turned into `.`.
    let baked = format!("island.{}", key.replace('/', "."));
    assert!(app.world().entity(&baked).is_ok());
    assert_eq!(log_len(&app), steps + 6);

    // Each entry was one undo step.
    for _ in 0..6 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    assert_eq!(generated(&app), before);
    assert_eq!(
        app.world().get(&id, "Transform", "translation").unwrap(),
        Value::Vec3([0.0, 7.0, 0.0].into())
    );
}
