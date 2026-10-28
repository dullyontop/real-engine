//! The Inspector's pickers and container editors (ADR-0064 §9), driven with
//! the pointer: an asset field chooses from the asset index's files of its
//! kind and takes an asset of that kind dropped from the Assets panel; an
//! entity field chooses from a searched list of the world's entities or the
//! next click in the viewport; lists, records and maps are edited item by
//! item, each finished edit one `entity.set` of the whole value; and a
//! generated entity's overridden field is marked, with Revert.
//!
//! These tests need a GPU adapter (lavapipe in CI); run them with
//! `--test-threads=1`.

use std::collections::BTreeMap;
use std::path::Path;

use egui_dock::{DockState, NodeIndex};
use glam::Vec3;
use re_app::{App, AppConfig};
use re_command::AllowAll;
use re_editor::layout::PanelId;
use re_editor::{AssetSpot, HeadlessEditor, LineKind};
use re_reflect::{FieldType, RecordField, RecordSchema, Value};
use serde_json::{Value as Json, json};

const ROCK: &str = "materials/rock.material.json";
const RED: &str = "materials/red.material.json";
const BARK: &str = "assets/textures/bark.png";
const GRAPH: &str = "graphs/flat.graph.json";

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "script_components": [
    { "name": "Patrol", "fields": [
        { "name": "target", "type": { "kind": "entity" }, "default": null },
        { "name": "stops", "type": { "kind": "list", "item": { "kind": "vec3" } }, "default": [] },
        { "name": "speeds", "type": { "kind": "record", "record": "SpeedRange" },
          "default": { "low": 1.0, "high": 2.0 } } ] }
  ],
  "entities": [
    { "id": "camera", "name": "Camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 3.0, 8.0] } } },
    { "id": "lamp", "name": "Lamp", "components": {
        "Light": { "kind": "Point" },
        "Transform": {} } },
    { "id": "post", "name": "Post", "components": {
        "Transform": { "translation": [3.0, 0.0, 0.0] } } },
    { "id": "guard", "name": "Guard", "components": {
        "Attributes": { "values": { "hp": 10 } },
        "MaterialRef": { "asset": "materials/rock.material.json" },
        "MeshRef": { "asset": "builtin:cube" },
        "Patrol": {},
        "Tags": { "tags": ["a", "b"] },
        "Transform": { "translation": [-3.0, 0.0, 0.0] } } },
    { "id": "island", "name": "Island", "components": {
        "GraphSource": { "graph": "graphs/flat.graph.json" },
        "Procedural": { "generator": "graph", "seed": 1 },
        "Transform": {} } }
  ]
}
"#;

fn graph() -> Json {
    json!({
        "format": "real-engine/graph", "version": 1, "name": "flat", "seed": 1,
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 9, "cell": 4.0, "height": 0.0 } },
            { "id": "ground", "kind": "terrain.mesh", "version": 3, "params": {} }
        ],
        "edges": [ { "from": ["h", "field"], "to": ["ground", "field"] } ]
    })
}

/// The record type `Patrol.speeds` names, registered before the scene loads.
fn register_records() {
    RecordSchema::new(
        "SpeedRange",
        vec![
            RecordField::new("low", FieldType::F32),
            RecordField::new("high", FieldType::F32),
        ],
    )
    .register()
    .unwrap();
}

fn project() -> tempfile::TempDir {
    register_records();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |path: &str, text: &str| {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write("real.toml", "[project]\nname = \"inspector\"\n");
    write("scenes/main.scene.json", SCENE);
    write(ROCK, r#"{ "roughness": 0.8 }"#);
    write(RED, r#"{ "base_color": [1.0, 0.0, 0.0, 1.0] }"#);
    write(GRAPH, &serde_json::to_string_pretty(&graph()).unwrap());
    let bark = root.join(BARK);
    std::fs::create_dir_all(bark.parent().unwrap()).unwrap();
    re_render::Image {
        width: 2,
        height: 2,
        rgba: [90, 60, 30, 255].repeat(4),
    }
    .save_png(&bark)
    .unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

/// The editor with the viewport on the left, the Inspector on the right and
/// the Assets panel under the viewport, with `id` selected.
fn editor(app: &mut App, id: &str) -> HeadlessEditor {
    let mut ed = HeadlessEditor::new(app, 1280, 720).expect("a headless GPU");
    // The first frame reads the project's layout; rearrange after it.
    idle(&mut ed, app);
    let mut dock = DockState::new(vec![PanelId::Viewport]);
    let tree = dock.main_surface_mut();
    let [left, _] = tree.split_right(NodeIndex::root(), 0.55, vec![PanelId::Inspector]);
    tree.split_below(left, 0.6, vec![PanelId::Assets]);
    *ed.editor.dock_mut() = dock;
    ed.editor.select(Some(id));
    ed.editor.wait_for_assets(app);
    for _ in 0..3 {
        idle(&mut ed, app);
    }
    ed
}

fn frame(ed: &mut HeadlessEditor, app: &mut App, events: Vec<egui::Event>) {
    let mut all = vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE)];
    all.extend(events);
    let input = egui::RawInput {
        events: all,
        ..egui::RawInput::default()
    };
    ed.frame(app, &AllowAll, input).unwrap();
}

fn idle(ed: &mut HeadlessEditor, app: &mut App) {
    frame(ed, app, vec![]);
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
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

/// Where part `part` of a field of `id` was drawn.
fn part(ed: &HeadlessEditor, id: &str, field: (&str, &str), part: &str) -> egui::Pos2 {
    ed.editor
        .field_part(id, field.0, field.1, part)
        .unwrap_or_else(|| panic!("part {part:?} of {field:?} is not on screen"))
        .center()
}

fn has_part(ed: &HeadlessEditor, id: &str, field: (&str, &str), part: &str) -> bool {
    ed.editor.field_part(id, field.0, field.1, part).is_some()
}

/// Clicks at `at`, then lets two frames pass so that what the click asked
/// for is done and drawn.
fn click_at(ed: &mut HeadlessEditor, app: &mut App, at: egui::Pos2) {
    // Long enough after the last click that this one is not a double click.
    for _ in 0..30 {
        idle(ed, app);
    }
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

fn click(ed: &mut HeadlessEditor, app: &mut App, id: &str, field: (&str, &str), p: &str) {
    let at = part(ed, id, field, p);
    click_at(ed, app, at);
}

/// Drags with the primary button from `from` to `to` in a few steps, then
/// lets two frames pass.
fn drag(ed: &mut HeadlessEditor, app: &mut App, from: egui::Pos2, to: egui::Pos2) {
    for _ in 0..30 {
        idle(ed, app);
    }
    frame(
        ed,
        app,
        vec![egui::Event::PointerMoved(from), button(from, true)],
    );
    for i in 1..=8 {
        let at = from + (to - from) * (i as f32 / 8.0);
        frame(ed, app, vec![egui::Event::PointerMoved(at)]);
    }
    frame(ed, app, vec![button(to, false)]);
    for _ in 0..2 {
        idle(ed, app);
    }
}

/// Clicks the text box `p`, selects what it shows and types `text`, then
/// Enter.
fn type_into(
    ed: &mut HeadlessEditor,
    app: &mut App,
    id: &str,
    field: (&str, &str),
    p: &str,
    text: &str,
) {
    click(ed, app, id, field, p);
    frame(ed, app, vec![key(egui::Key::A, egui::Modifiers::COMMAND)]);
    frame(ed, app, vec![egui::Event::Text(text.to_string())]);
    frame(ed, app, vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
    for _ in 0..2 {
        idle(ed, app);
    }
}

fn get(app: &App, id: &str, field: (&str, &str)) -> Value {
    app.world().get(id, field.0, field.1).unwrap()
}

fn log(app: &App) -> usize {
    app.history().log().len()
}

fn entity(app: &App, id: &str) -> Value {
    Value::Entity(Some(app.world().entity(id).unwrap()))
}

const MATERIAL: (&str, &str) = ("MaterialRef", "asset");
const TARGET: (&str, &str) = ("Patrol", "target");
const STOPS: (&str, &str) = ("Patrol", "stops");
const SPEEDS: (&str, &str) = ("Patrol", "speeds");
const TAGS: (&str, &str) = ("Tags", "tags");
const VALUES: (&str, &str) = ("Attributes", "values");

#[test]
fn asset_fields_choose_from_the_index_and_take_dropped_assets() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app, "guard");
    let start = log(&app);

    // The picker lists the project's materials; choosing one is one step.
    click(&mut ed, &mut app, "guard", MATERIAL, "…");
    assert!(has_part(&ed, "guard", MATERIAL, &format!("={ROCK}")));
    assert!(
        !has_part(&ed, "guard", MATERIAL, &format!("={BARK}")),
        "only materials are listed"
    );
    click(&mut ed, &mut app, "guard", MATERIAL, &format!("={RED}"));
    assert_eq!(get(&app, "guard", MATERIAL), Value::Str(RED.into()));
    assert_eq!(log(&app), start + 1);

    // A material dragged from the Assets panel onto the field sets it.
    let folder = ed
        .editor
        .asset_spot(AssetSpot::Folder("materials"))
        .expect("the materials folder");
    click_at(&mut ed, &mut app, folder.center());
    for _ in 0..10 {
        idle(&mut ed, &mut app);
    }
    let rock = ed
        .editor
        .asset_spot(AssetSpot::Item(ROCK))
        .expect("the rock material's tile")
        .center();
    let field = part(&ed, "guard", MATERIAL, "");
    drag(&mut ed, &mut app, rock, field);
    assert_eq!(get(&app, "guard", MATERIAL), Value::Str(ROCK.into()));
    assert_eq!(log(&app), start + 2, "{:?}", ed.editor.console());

    // A texture is not a material: dropping one changes nothing.
    for folder in ["assets", "assets/textures"] {
        let at = ed
            .editor
            .asset_spot(AssetSpot::Folder(folder))
            .unwrap_or_else(|| panic!("the folder {folder}"));
        click_at(&mut ed, &mut app, at.center());
        for _ in 0..10 {
            idle(&mut ed, &mut app);
        }
    }
    let bark = ed
        .editor
        .asset_spot(AssetSpot::Item(BARK))
        .expect("the bark texture's tile")
        .center();
    let field = part(&ed, "guard", MATERIAL, "");
    drag(&mut ed, &mut app, bark, field);
    assert_eq!(get(&app, "guard", MATERIAL), Value::Str(ROCK.into()));
    assert_eq!(log(&app), start + 2);

    // Each edit was one undo step.
    for _ in 0..2 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    assert_eq!(get(&app, "guard", MATERIAL), Value::Str(ROCK.into()));
    assert!(!app.history().can_undo());
}

#[test]
fn entity_fields_choose_from_a_list_or_a_click_in_the_viewport() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app, "guard");
    let start = log(&app);

    // The list is searched by name or id.
    click(&mut ed, &mut app, "guard", TARGET, "…");
    assert!(has_part(&ed, "guard", TARGET, "=post"));
    assert!(has_part(&ed, "guard", TARGET, "=lamp"));
    let search = part(&ed, "guard", TARGET, "search");
    click_at(&mut ed, &mut app, search);
    frame(&mut ed, &mut app, vec![egui::Event::Text("pos".into())]);
    idle(&mut ed, &mut app);
    assert!(has_part(&ed, "guard", TARGET, "=post"));
    assert!(!has_part(&ed, "guard", TARGET, "=lamp"));
    click(&mut ed, &mut app, "guard", TARGET, "=post");
    assert_eq!(get(&app, "guard", TARGET), entity(&app, "post"));
    assert_eq!(log(&app), start + 1);

    // "pick" waits for a click in the viewport: the lamp's icon sets the
    // field, and the selection stays.
    click(&mut ed, &mut app, "guard", TARGET, "pick");
    assert!(ed.editor.is_picking());
    let lamp = ed.editor.screen_point(&app, Vec3::ZERO).unwrap();
    click_at(&mut ed, &mut app, lamp);
    assert!(!ed.editor.is_picking());
    assert_eq!(get(&app, "guard", TARGET), entity(&app, "lamp"));
    assert_eq!(ed.editor.selected(), Some("guard"));
    assert_eq!(log(&app), start + 2);

    // Escape stops waiting: the next click selects as usual.
    click(&mut ed, &mut app, "guard", TARGET, "pick");
    assert!(ed.editor.is_picking());
    frame(
        &mut ed,
        &mut app,
        vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
    );
    idle(&mut ed, &mut app);
    assert!(!ed.editor.is_picking());

    // Typing an id still works, and "(none)" clears the field.
    type_into(&mut ed, &mut app, "guard", TARGET, "", "post");
    assert_eq!(get(&app, "guard", TARGET), entity(&app, "post"));
    click(&mut ed, &mut app, "guard", TARGET, "…");
    click(&mut ed, &mut app, "guard", TARGET, "=");
    assert_eq!(get(&app, "guard", TARGET), Value::Entity(None));
    assert_eq!(log(&app), start + 4, "{:?}", ed.editor.console());
}

#[test]
fn lists_records_and_maps_are_set_whole_once_per_finished_edit() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app, "guard");
    let start = log(&app);
    let strings =
        |items: &[&str]| Value::List(items.iter().map(|s| Value::Str(s.to_string())).collect());

    // A list of strings: add, type into, reorder and remove items.
    click(&mut ed, &mut app, "guard", TAGS, "+");
    assert_eq!(get(&app, "guard", TAGS), strings(&["a", "b", ""]));
    type_into(&mut ed, &mut app, "guard", TAGS, "2", "c");
    assert_eq!(get(&app, "guard", TAGS), strings(&["a", "b", "c"]));
    click(&mut ed, &mut app, "guard", TAGS, "⏶2");
    assert_eq!(get(&app, "guard", TAGS), strings(&["a", "c", "b"]));
    click(&mut ed, &mut app, "guard", TAGS, "×0");
    assert_eq!(get(&app, "guard", TAGS), strings(&["c", "b"]));
    assert_eq!(log(&app), start + 4);

    // A list of vectors: a drag of a component is one step, on release.
    click(&mut ed, &mut app, "guard", STOPS, "+");
    assert_eq!(
        get(&app, "guard", STOPS),
        Value::List(vec![Value::Vec3(Vec3::ZERO)])
    );
    let x = part(&ed, "guard", STOPS, "0/0");
    drag(&mut ed, &mut app, x, x + egui::vec2(40.0, 0.0));
    let Value::List(stops) = get(&app, "guard", STOPS) else {
        panic!("a list");
    };
    let Value::Vec3(v) = stops[0] else {
        panic!("a vector");
    };
    assert!(v.x > 0.1 && v.y == 0.0 && v.z == 0.0, "{v:?}");
    assert_eq!(log(&app), start + 6);

    // A record: a widget per field.
    let low = part(&ed, "guard", SPEEDS, "low");
    drag(&mut ed, &mut app, low, low + egui::vec2(40.0, 0.0));
    let Value::Record(_, speeds) = get(&app, "guard", SPEEDS) else {
        panic!("a record");
    };
    let Value::Float(low) = speeds[0] else {
        panic!("a number");
    };
    assert!(low > 1.1, "{low}");
    assert_eq!(speeds[1], Value::Float(2.0));
    assert_eq!(log(&app), start + 7);

    // A map: add an entry, choose its type, rename it; a key that is taken
    // is refused.
    let values = |app: &App| match get(app, "guard", VALUES) {
        Value::Map(m) => m,
        other => panic!("a map, not {other:?}"),
    };
    click(&mut ed, &mut app, "guard", VALUES, "+");
    assert_eq!(values(&app).get("key"), Some(&Value::Float(0.0)));
    click(&mut ed, &mut app, "guard", VALUES, "key/type");
    click(&mut ed, &mut app, "guard", VALUES, "key/type=string");
    assert_eq!(values(&app).get("key"), Some(&Value::Str(String::new())));
    type_into(&mut ed, &mut app, "guard", VALUES, "key/key", "name");
    let expected: BTreeMap<String, Value> = [
        ("hp".to_string(), Value::Float(10.0)),
        ("name".to_string(), Value::Str(String::new())),
    ]
    .into();
    assert_eq!(values(&app), expected);
    assert_eq!(log(&app), start + 10);
    type_into(&mut ed, &mut app, "guard", VALUES, "name/key", "hp");
    assert_eq!(values(&app), expected);
    assert_eq!(log(&app), start + 10);
    assert!(
        ed.editor.console().iter().any(|l| l.kind == LineKind::Error
            && l.text.contains("RE8803")
            && l.text.contains("\"hp\"")),
        "{:?}",
        ed.editor.console()
    );

    // Each finished edit was one undo step.
    for _ in 0..10 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    assert!(!app.history().can_undo());
    assert_eq!(get(&app, "guard", TAGS), strings(&["a", "b"]));
    assert_eq!(values(&app).get("hp"), Some(&Value::Float(10.0)));
    assert_eq!(values(&app).len(), 1);
}

#[test]
fn an_overridden_field_of_a_generated_entity_is_marked_and_reverted() {
    let dir = project();
    let mut app = open(dir.path());
    let id = app
        .world()
        .ids()
        .iter()
        .map(|(id, _)| id.to_string())
        .filter(|id| id.starts_with("island/"))
        .min()
        .expect("the island generates an entity");
    let field = ("Transform", "translation");
    let generated = get(&app, &id, field);
    let mut ed = editor(&mut app, &id);
    assert!(!has_part(&ed, &id, field, "revert"), "not overridden yet");

    let out = ed.editor.run(
        &mut app,
        &AllowAll,
        "entity.set",
        json!({ "id": id, "component": "Transform", "field": "translation", "value": [0.0, 5.0, 0.0] }),
        false,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    for _ in 0..2 {
        idle(&mut ed, &mut app);
    }
    assert_eq!(get(&app, &id, field), Value::Vec3(Vec3::new(0.0, 5.0, 0.0)));
    assert!(!has_part(&ed, &id, ("Transform", "scale"), "revert"));

    let start = log(&app);
    click(&mut ed, &mut app, &id, field, "revert");
    assert_eq!(get(&app, &id, field), generated);
    assert!(!has_part(&ed, &id, field, "revert"));
    assert_eq!(log(&app), start + 1, "{:?}", ed.editor.console());
}
