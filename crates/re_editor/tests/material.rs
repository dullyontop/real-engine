//! The Material panel (ADR-0064 §3), driven with the pointer: a material is
//! taken from the selection, each finished edit is one `material.edit` (one
//! undo step, and undoing them all gives the file back byte for byte), a
//! texture is sent only once its file is chosen, "New material" is
//! `material.create`, and a file the material loader refuses is read-only.
//!
//! These tests need a GPU adapter (lavapipe in CI); run them with
//! `--test-threads=1`.

use std::cell::RefCell;
use std::path::Path;

use re_app::{App, AppConfig};
use re_command::{AllowAll, CommandSpec, PolicyCheck};
use re_core::Diagnostic;
use re_editor::layout::PanelId;
use re_editor::{HeadlessEditor, MaterialSpot, Panel};
use serde_json::{Value as Json, json};

const RED: &str = "materials/red.material.json";

/// The material as written by hand, with its own spacing, so that a save
/// after undoing every edit has to give these bytes back.
const RED_TEXT: &str = r#"{
  "$schema": "../schemas/material.schema.json",
  "base_color": [1.0, 0.0, 0.0, 1.0],
  "roughness": 0.4
}
"#;

/// A 1×1 PNG.
const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn scene() -> String {
    let scene = json!({
        "format": "real-engine/scene",
        "version": 1,
        "entities": [
            { "id": "camera", "name": "Camera", "components": {
                "Camera": { "active": true },
                "Transform": { "translation": [0.0, 2.0, 6.0] } } },
            { "id": "crate", "name": "Crate", "components": {
                "MaterialRef": { "asset": RED },
                "Transform": {} } }
        ]
    });
    serde_json::to_string_pretty(&scene).unwrap()
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for d in ["scenes", "materials", "textures"] {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    std::fs::write(
        dir.path().join("real.toml"),
        "[project]\nname = \"materials\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("scenes/main.scene.json"), scene()).unwrap();
    std::fs::write(dir.path().join(RED), RED_TEXT).unwrap();
    std::fs::write(dir.path().join("textures/rock.png"), PNG).unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

/// The editor with the Material panel filling the window.
fn editor(app: &mut App) -> HeadlessEditor {
    let mut ed = HeadlessEditor::new(app, 1280, 720).expect("a headless GPU");
    // The first frame reads the project's layout; rearrange after it.
    idle(&mut ed, app);
    let mut tabs = vec![PanelId::Material];
    tabs.extend(PanelId::ALL.into_iter().filter(|p| *p != PanelId::Material));
    *ed.editor.dock_mut() = egui_dock::DockState::new(tabs);
    for _ in 0..3 {
        idle(&mut ed, app);
    }
    assert!(ed.editor.rects().get(Panel::Material).is_some());
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

fn idle(ed: &mut HeadlessEditor, app: &mut App) {
    frame(ed, app, vec![]);
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

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn spot(ed: &HeadlessEditor, s: MaterialSpot<'_>) -> egui::Pos2 {
    ed.editor
        .material_spot(s)
        .unwrap_or_else(|| panic!("{s:?} is not on screen"))
        .center()
}

/// The commands a click on `s` sends.
fn click(ed: &mut HeadlessEditor, app: &mut App, s: MaterialSpot<'_>) -> Vec<(String, Json)> {
    let at = spot(ed, s);
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

fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// The material as its open document holds it.
fn material(app: &App, path: &str) -> Json {
    app.world()
        .doc(path)
        .unwrap_or_else(|| panic!("{path} is not open"))
        .value
        .clone()
}

fn log_len(app: &App) -> usize {
    app.history().log().len()
}

fn edit(path: &str, set: Json) -> (String, Json) {
    (
        "material.edit".to_string(),
        json!({ "path": path, "set": set }),
    )
}

#[test]
fn a_material_is_edited_key_by_key_and_undo_gives_the_file_back() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);

    // The selected entity's material is one click away.
    assert_eq!(ed.editor.shown_material(), None);
    ed.editor.select(Some("crate"));
    idle(&mut ed, &mut app);
    assert!(click(&mut ed, &mut app, MaterialSpot::Button("From selection")).is_empty());
    assert_eq!(ed.editor.shown_material(), Some(RED));
    idle(&mut ed, &mut app);
    assert_eq!(log_len(&app), 0, "drawing sends nothing");

    // A slider sends one edit, when it is released.
    let rail = ed
        .editor
        .material_spot(MaterialSpot::Key("roughness", ""))
        .expect("the roughness slider");
    let from = rail.left_center() + egui::vec2(20.0, 0.0);
    let to = rail.left_center() + egui::vec2(85.0, 0.0);
    let calls = Calls::default();
    frame_with(
        &mut ed,
        &mut app,
        &calls,
        vec![egui::Event::PointerMoved(from), button(from, true)],
    );
    for t in [0.25, 0.5, 0.75, 1.0] {
        let at = from + (to - from) * t;
        frame_with(
            &mut ed,
            &mut app,
            &calls,
            vec![egui::Event::PointerMoved(at)],
        );
    }
    assert!(
        calls.0.borrow().is_empty(),
        "nothing is sent during the drag"
    );
    frame_with(&mut ed, &mut app, &calls, vec![button(to, false)]);
    for _ in 0..2 {
        frame_with(&mut ed, &mut app, &calls, vec![]);
    }
    let calls = calls.0.into_inner();
    assert_eq!(calls.len(), 1, "{calls:?} {:?}", ed.editor.console());
    assert_eq!(calls[0].0, "material.edit");
    let roughness = material(&app, RED)["roughness"].as_f64().unwrap();
    assert!(roughness > 0.6 && roughness <= 1.0, "{roughness}");
    assert_eq!(log_len(&app), 1);

    // × removes a key, so that its default applies.
    assert_eq!(
        click(&mut ed, &mut app, MaterialSpot::Key("roughness", "×")),
        [(
            "material.edit".to_string(),
            json!({ "path": RED, "unset": ["roughness"] })
        )]
    );
    assert!(material(&app, RED).get("roughness").is_none());
    assert!(
        ed.editor
            .material_spot(MaterialSpot::Key("roughness", "×"))
            .is_none(),
        "a key the file does not set has no ×"
    );

    // A key the file does not set shows its default; changing it sets it.
    assert_eq!(
        click(&mut ed, &mut app, MaterialSpot::Key("double_sided", "")),
        [edit(RED, json!({ "double_sided": true }))]
    );

    // A choice is picked from its list.
    assert!(click(&mut ed, &mut app, MaterialSpot::Key("alpha_mode", "")).is_empty());
    assert_eq!(
        click(&mut ed, &mut app, MaterialSpot::Key("alpha_mode", "=blend")),
        [edit(RED, json!({ "alpha_mode": "blend" }))]
    );

    // A texture switched on sends nothing until its file is chosen: a
    // material only names files that exist.
    assert!(
        click(
            &mut ed,
            &mut app,
            MaterialSpot::Key("base_color_texture", "set")
        )
        .is_empty()
    );
    assert!(
        click(
            &mut ed,
            &mut app,
            MaterialSpot::Key("base_color_texture", "…")
        )
        .is_empty()
    );
    assert_eq!(
        click(
            &mut ed,
            &mut app,
            MaterialSpot::Key("base_color_texture", "=textures/rock.png")
        ),
        [edit(
            RED,
            json!({ "base_color_texture": "textures/rock.png" })
        )]
    );
    assert_eq!(
        material(&app, RED)["base_color_texture"],
        json!("textures/rock.png")
    );
    // Switched off, it is removed.
    assert_eq!(
        click(
            &mut ed,
            &mut app,
            MaterialSpot::Key("base_color_texture", "set")
        ),
        [(
            "material.edit".to_string(),
            json!({ "path": RED, "unset": ["base_color_texture"] })
        )]
    );
    assert_eq!(log_len(&app), 6, "{:?}", ed.editor.console());

    // The file changes only when the project is saved.
    assert_eq!(
        std::fs::read_to_string(dir.path().join(RED)).unwrap(),
        RED_TEXT
    );
    app.save_scene().unwrap();
    let saved: Json =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join(RED)).unwrap()).unwrap();
    assert_eq!(saved["alpha_mode"], json!("blend"));

    // Each edit is one undo step; undoing them all and saving gives the
    // file's own text back.
    for _ in 0..6 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    app.save_scene().unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join(RED)).unwrap(),
        RED_TEXT
    );
}

#[test]
fn new_material_makes_a_file_in_the_materials_folder() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);

    let name = spot(&ed, MaterialSpot::NewName);
    frame(
        &mut ed,
        &mut app,
        vec![
            egui::Event::PointerMoved(name),
            button(name, true),
            button(name, false),
        ],
    );
    for ch in "blue".chars() {
        frame(&mut ed, &mut app, vec![egui::Event::Text(ch.to_string())]);
    }
    frame(
        &mut ed,
        &mut app,
        vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
    );
    idle(&mut ed, &mut app);
    let blue = "materials/blue.material.json";
    assert_eq!(
        click(&mut ed, &mut app, MaterialSpot::Button("New material")),
        [("material.create".to_string(), json!({ "path": blue }))]
    );
    assert_eq!(ed.editor.shown_material(), Some(blue));
    assert_eq!(
        material(&app, blue),
        json!({ "$schema": "../schemas/material.schema.json" })
    );
    // Its keys are there to edit at once.
    assert_eq!(
        click(&mut ed, &mut app, MaterialSpot::Key("unlit", "")),
        [edit(blue, json!({ "unlit": true }))]
    );

    // Undone, it does not exist, and the save makes no file.
    for _ in 0..2 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    idle(&mut ed, &mut app);
    assert!(
        ed.editor
            .material_spot(MaterialSpot::Key("unlit", ""))
            .is_none()
    );
    app.save_scene().unwrap();
    assert!(!dir.path().join(blue).exists());
}

#[test]
fn a_file_the_loader_refuses_is_read_only() {
    let dir = project();
    let bad = "materials/bad.material.json";
    std::fs::write(
        dir.path().join(bad),
        r#"{ "roughness": 0.5, "shininess": 3 }"#,
    )
    .unwrap();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    ed.editor.show_material(bad);
    for _ in 0..2 {
        idle(&mut ed, &mut app);
    }
    assert!(
        click(&mut ed, &mut app, MaterialSpot::Key("double_sided", "")).is_empty(),
        "the form is read-only"
    );
    assert_eq!(log_len(&app), 0);
    assert!(app.world().doc(bad).is_none(), "nothing opened it");
}
