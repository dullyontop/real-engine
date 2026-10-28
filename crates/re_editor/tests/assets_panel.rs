//! The Assets panel (ADR-0064 §7), driven with the pointer: folders, the
//! grid and the list, the kind filter and the search, an asset's details and
//! the entities that use it, opening by kind, "New material", "Import",
//! files dropped on the window from the system, and the thumbnails of
//! textures and materials with their disk cache.
//!
//! These tests need a GPU adapter (lavapipe in CI); run them with
//! `--test-threads=1`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use re_app::{App, AppConfig};
use re_command::AllowAll;
use re_editor::layout::{self, PanelId};
use re_editor::{AssetSpot, HeadlessEditor, Panel};
use serde_json::{Value as Json, json};

const ROCK: &str = "materials/rock.material.json";
const GRAPH: &str = "graphs/flat.graph.json";

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "camera", "name": "Camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 3.0, 8.0] } } },
    { "id": "crate", "name": "Crate", "components": {
        "MaterialRef": { "asset": "materials/rock.material.json" },
        "MeshRef": { "asset": "builtin:cube" },
        "Transform": {} } },
    { "id": "barrel", "name": "Barrel", "components": {
        "MaterialRef": { "asset": "materials/rock.material.json" },
        "Transform": { "translation": [2.0, 0.0, 0.0] } } },
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

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |path: &str, text: &str| {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write("real.toml", "[project]\nname = \"assets\"\n");
    write("scenes/main.scene.json", SCENE);
    write(ROCK, r#"{ "roughness": 0.8 }"#);
    write(GRAPH, &serde_json::to_string_pretty(&graph()).unwrap());
    write("scripts/game/main.luau", "print('hi')\n");
    png(&root.join("assets/textures/bark.png"));
    dir
}

/// A 2×2 PNG.
fn png(path: &Path) {
    solid(path, 2, 2, [90, 60, 30, 255]);
}

/// A PNG of one colour.
fn solid(path: &Path, width: u32, height: u32, rgba: [u8; 4]) {
    re_render::Image {
        width,
        height,
        rgba: rgba.repeat((width * height) as usize),
    }
    .save_png(path)
    .unwrap();
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

/// The editor with the Assets panel filling the window, its index built.
fn editor(app: &mut App) -> HeadlessEditor {
    let mut ed = HeadlessEditor::new(app, 1280, 720).expect("a headless GPU");
    // The first frame reads the project's layout; rearrange after it.
    idle(&mut ed, app);
    let mut tabs = vec![PanelId::Assets];
    tabs.extend(PanelId::ALL.into_iter().filter(|p| *p != PanelId::Assets));
    *ed.editor.dock_mut() = egui_dock::DockState::new(tabs);
    ed.editor.wait_for_assets(app);
    for _ in 0..3 {
        idle(&mut ed, app);
    }
    assert!(ed.editor.rects().get(Panel::Assets).is_some());
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

fn spot(ed: &HeadlessEditor, s: AssetSpot<'_>) -> egui::Pos2 {
    ed.editor
        .asset_spot(s)
        .unwrap_or_else(|| panic!("{s:?} is not on screen"))
        .center()
}

fn shown(ed: &HeadlessEditor, s: AssetSpot<'_>) -> bool {
    ed.editor.asset_spot(s).is_some()
}

/// Clicks `s`, then lets two frames pass so that what it asked for is done
/// and drawn.
fn click(ed: &mut HeadlessEditor, app: &mut App, s: AssetSpot<'_>) {
    let at = spot(ed, s);
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

fn double_click(ed: &mut HeadlessEditor, app: &mut App, s: AssetSpot<'_>) {
    // Long enough after the last click that these two are not part of a
    // triple click.
    for _ in 0..40 {
        idle(ed, app);
    }
    let at = spot(ed, s);
    frame(
        ed,
        app,
        vec![
            egui::Event::PointerMoved(at),
            button(at, true),
            button(at, false),
        ],
    );
    frame(ed, app, vec![button(at, true), button(at, false)]);
    for _ in 0..2 {
        idle(ed, app);
    }
}

/// Clicks the folder `folder`, and waits for the tree to finish opening it.
fn choose_folder(ed: &mut HeadlessEditor, app: &mut App, folder: &str) {
    click(ed, app, AssetSpot::Folder(folder));
    for _ in 0..10 {
        idle(ed, app);
    }
}

/// Clicks the text box `s` and types `text` into it.
fn type_into(ed: &mut HeadlessEditor, app: &mut App, s: AssetSpot<'_>, text: &str) {
    click(ed, app, s);
    frame(ed, app, vec![egui::Event::Text(text.to_string())]);
    idle(ed, app);
}

/// Puts the Assets panel back in front (the panels it opens assets in come
/// to the front).
fn assets_in_front(ed: &mut HeadlessEditor, app: &mut App) {
    let mut tabs = vec![PanelId::Assets];
    tabs.extend(PanelId::ALL.into_iter().filter(|p| *p != PanelId::Assets));
    *ed.editor.dock_mut() = egui_dock::DockState::new(tabs);
    idle(ed, app);
}

#[test]
fn folders_kinds_search_details_and_opening() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);

    // The project's top holds no assets; its folders do. A folder opens
    // when it is chosen.
    assert!(shown(&ed, AssetSpot::Folder("")));
    for folder in ["assets", "graphs", "materials", "scenes", "scripts"] {
        assert!(shown(&ed, AssetSpot::Folder(folder)), "{folder}");
    }
    assert!(!shown(&ed, AssetSpot::Folder("assets/textures")));
    assert!(!shown(&ed, AssetSpot::Item(ROCK)));
    choose_folder(&mut ed, &mut app, "assets");
    assert!(shown(&ed, AssetSpot::Folder("assets/textures")));
    click(&mut ed, &mut app, AssetSpot::Folder("materials"));
    assert!(shown(&ed, AssetSpot::Item(ROCK)));
    assert!(!shown(&ed, AssetSpot::Item(GRAPH)));

    // A click selects it and shows its details, with the entities using it.
    click(&mut ed, &mut app, AssetSpot::Item(ROCK));
    assert_eq!(ed.editor.selected_asset(), Some(ROCK));
    assert!(shown(&ed, AssetSpot::User("barrel")) && shown(&ed, AssetSpot::User("crate")));
    assert!(!shown(&ed, AssetSpot::User("island")));
    click(&mut ed, &mut app, AssetSpot::User("crate"));
    assert_eq!(ed.editor.selected(), Some("crate"));

    // Opened, a material shows in the Material panel, which comes to the
    // front.
    click(&mut ed, &mut app, AssetSpot::Button("Open"));
    assert_eq!(ed.editor.shown_material(), Some(ROCK));
    assert!(ed.editor.rects().get(Panel::Material).is_some());
    assert!(ed.editor.rects().get(Panel::Assets).is_none());

    // The search looks through every folder; the kind filter keeps one kind.
    assets_in_front(&mut ed, &mut app);
    type_into(&mut ed, &mut app, AssetSpot::Search, "a");
    for path in [
        ROCK,
        GRAPH,
        "assets/textures/bark.png",
        "scenes/main.scene.json",
    ] {
        assert!(shown(&ed, AssetSpot::Item(path)), "{path}");
    }
    click(&mut ed, &mut app, AssetSpot::Filter);
    click(&mut ed, &mut app, AssetSpot::Kind("graph"));
    assert!(shown(&ed, AssetSpot::Item(GRAPH)));
    assert!(!shown(&ed, AssetSpot::Item(ROCK)));

    // A graph opens on the node canvas, for the source that names it.
    double_click(&mut ed, &mut app, AssetSpot::Item(GRAPH));
    assert_eq!(ed.editor.canvas_source(), Some("island"));
    assert!(ed.editor.rects().get(Panel::Canvas).is_some());

    // The list, the filter and the folder are saved with the layout.
    assets_in_front(&mut ed, &mut app);
    click(&mut ed, &mut app, AssetSpot::Button("List"));
    assert!(shown(&ed, AssetSpot::Item(GRAPH)), "the list shows it too");
    ed.editor.save_layout();
    let saved: Json =
        serde_json::from_str(&std::fs::read_to_string(layout::layout_path(dir.path())).unwrap())
            .unwrap();
    assert_eq!(
        saved["panels"]["Assets"],
        json!({ "folder": "materials", "list": true, "kind": "graph" })
    );
}

#[test]
fn new_materials_imports_and_dropped_files() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    click(&mut ed, &mut app, AssetSpot::Folder("materials"));

    // "New material" makes it in the folder shown and shows it.
    type_into(&mut ed, &mut app, AssetSpot::Text("name"), "moss");
    click(&mut ed, &mut app, AssetSpot::Button("New material"));
    let moss = "materials/moss.material.json";
    assert!(app.world().doc(moss).is_some(), "material.create made it");
    assert_eq!(ed.editor.shown_material(), Some(moss));
    assert_eq!(ed.editor.selected_asset(), Some(moss));
    // Without a name, a free default one.
    assets_in_front(&mut ed, &mut app);
    click(&mut ed, &mut app, AssetSpot::Button("New material"));
    assert!(
        app.world()
            .doc("materials/material.material.json")
            .is_some()
    );

    // "Import" copies a file in from outside, into the folder shown under
    // assets/.
    assets_in_front(&mut ed, &mut app);
    choose_folder(&mut ed, &mut app, "assets");
    click(&mut ed, &mut app, AssetSpot::Folder("assets/textures"));
    let outside = tempfile::tempdir().unwrap();
    let leaf = outside.path().join("leaf.png");
    png(&leaf);
    type_into(
        &mut ed,
        &mut app,
        AssetSpot::Text("import"),
        &leaf.display().to_string(),
    );
    click(&mut ed, &mut app, AssetSpot::Button("Import"));
    assert!(dir.path().join("assets/textures/leaf.png").is_file());
    ed.editor.wait_for_assets(&mut app);
    idle(&mut ed, &mut app);
    assert!(shown(&ed, AssetSpot::Item("assets/textures/leaf.png")));
    assert_eq!(ed.editor.selected_asset(), Some("assets/textures/leaf.png"));

    // A file dropped on the window from the system is imported the same way.
    let fern = outside.path().join("fern.png");
    png(&fern);
    let input = egui::RawInput {
        dropped_files: vec![Arc::new(Dropped(fern))],
        ..egui::RawInput::default()
    };
    ed.frame(&mut app, &AllowAll, input).unwrap();
    assert!(dir.path().join("assets/textures/fern.png").is_file());
    ed.editor.wait_for_assets(&mut app);
    idle(&mut ed, &mut app);
    assert!(shown(&ed, AssetSpot::Item("assets/textures/fern.png")));
}

/// A file dropped from the system.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

const BARK: &str = "assets/textures/bark.png";
const WIDE: &str = "assets/textures/wide.png";
const BROKEN: &str = "assets/textures/broken.png";
const RED: &str = "materials/red.material.json";

/// The project with a texture larger than a thumbnail, one that cannot be
/// decoded, and a red material with a texture.
fn thumbnail_project() -> tempfile::TempDir {
    let dir = project();
    let root = dir.path();
    solid(&root.join(WIDE), 200, 100, [200, 40, 40, 255]);
    std::fs::write(root.join(BROKEN), "not a png").unwrap();
    std::fs::write(
        root.join(RED),
        json!({ "base_color": [1.0, 0.0, 0.0, 1.0], "roughness": 0.6, "base_color_texture": BARK })
            .to_string(),
    )
    .unwrap();
    dir
}

/// Lets the drawn tiles ask for their thumbnails, the worker make them and
/// the preview's renderer draw the materials, then the tiles show them.
fn make_thumbnails(ed: &mut HeadlessEditor, app: &mut App) {
    for _ in 0..3 {
        ed.editor.wait_for_assets(app);
        idle(ed, app);
        idle(ed, app);
    }
    ed.editor.wait_for_assets(app);
}

/// The frame's pixel at the middle of the thumbnail of the tile of `path`.
fn thumbnail_pixel(ed: &mut HeadlessEditor, app: &mut App, path: &str) -> [u8; 4] {
    let image = ed.frame(app, &AllowAll, egui::RawInput::default()).unwrap();
    let tile = ed
        .editor
        .asset_spot(AssetSpot::Item(path))
        .unwrap_or_else(|| panic!("{path} is not on screen"));
    // The thumbnail's square is 96 points, 4 in from the tile's top.
    let at = egui::pos2(tile.center().x, tile.min.y + 4.0 + 48.0);
    image.pixel(at.x as u32, at.y as u32)
}

fn near(a: [u8; 4], b: [u8; 3]) -> bool {
    (0..3).all(|c| a[c].abs_diff(b[c]) <= 3)
}

/// The thumbnails in the disk cache.
fn cached(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join(".real/cache/thumbs")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "png"))
        .collect();
    out.sort();
    out
}

/// Paints every cached thumbnail magenta, keeping its size.
fn paint_cache_magenta(dir: &Path) {
    for path in cached(dir) {
        let image = re_render::Image::load_png(&path).unwrap();
        solid(&path, image.width, image.height, [255, 0, 255, 255]);
    }
}

fn notes_about(ed: &HeadlessEditor, path: &str) -> usize {
    ed.editor
        .console()
        .iter()
        .filter(|l| l.text.contains("RE8853") && l.text.contains(path))
        .count()
}

#[test]
fn texture_thumbnails_are_made_once_kept_on_disk_and_made_again_when_the_file_changes() {
    let dir = thumbnail_project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    choose_folder(&mut ed, &mut app, "assets");
    click(&mut ed, &mut app, AssetSpot::Folder("assets/textures"));
    make_thumbnails(&mut ed, &mut app);

    // A texture shows its pixels: a small one drawn larger, a large one
    // shrunk to fit.
    assert!(ed.editor.assets().has_thumbnail(BARK));
    assert!(ed.editor.assets().has_thumbnail(WIDE));
    let px = thumbnail_pixel(&mut ed, &mut app, BARK);
    assert!(near(px, [90, 60, 30]), "{px:?}");
    let px = thumbnail_pixel(&mut ed, &mut app, WIDE);
    assert!(near(px, [200, 40, 40]), "{px:?}");
    // One that cannot be decoded is noted once and shows its icon.
    assert!(!ed.editor.assets().has_thumbnail(BROKEN));
    make_thumbnails(&mut ed, &mut app);
    assert_eq!(notes_about(&ed, BROKEN), 1);
    let wide = re_render::Image::load_png(&cached(dir.path())[0]).unwrap();
    assert!(wide.width <= 96 && wide.height <= 96);
    assert_eq!(cached(dir.path()).len(), 2, "one a texture");
    assert!(
        !dir.path().join(format!("{BARK}.meta.json")).exists(),
        "nothing was imported"
    );

    // Another editor reads them from the disk cache: they are not made again.
    paint_cache_magenta(dir.path());
    drop(ed);
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    make_thumbnails(&mut ed, &mut app);
    let px = thumbnail_pixel(&mut ed, &mut app, BARK);
    assert!(near(px, [255, 0, 255]), "{px:?}");

    // A texture that changes gets a new thumbnail.
    solid(&dir.path().join(BARK), 2, 2, [20, 200, 20, 255]);
    app.apply_file_changes(&[re_assets::AssetChange::Modified(BARK.into())]);
    make_thumbnails(&mut ed, &mut app);
    let px = thumbnail_pixel(&mut ed, &mut app, BARK);
    assert!(near(px, [20, 200, 20]), "{px:?}");
    assert_eq!(cached(dir.path()).len(), 3);
}

#[test]
fn material_thumbnails_are_drawn_on_the_sphere_without_importing_their_textures() {
    let dir = thumbnail_project();
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    click(&mut ed, &mut app, AssetSpot::Folder("materials"));
    make_thumbnails(&mut ed, &mut app);

    // The sphere in the middle of a dark square: red where the light falls.
    assert!(ed.editor.assets().has_thumbnail(RED));
    assert!(ed.editor.assets().has_thumbnail(ROCK));
    let px = thumbnail_pixel(&mut ed, &mut app, RED);
    assert!(
        px[0] > 30 && px[0] > 2 * px[1].max(px[2]),
        "a red sphere: {px:?}"
    );
    let px = thumbnail_pixel(&mut ed, &mut app, ROCK);
    assert!(
        px[0] > 60 && px[0].abs_diff(px[2]) < 30,
        "a white sphere: {px:?}"
    );
    // Its texture was read, not imported: no sidecar, nothing loaded.
    assert!(!dir.path().join(format!("{BARK}.meta.json")).exists());
    assert!(app.media().asset_server().loaded(BARK).is_none());
    assert_eq!(cached(dir.path()).len(), 2, "one a material");

    // From the disk cache in another editor.
    paint_cache_magenta(dir.path());
    drop(ed);
    let mut app = open(dir.path());
    let mut ed = editor(&mut app);
    click(&mut ed, &mut app, AssetSpot::Folder("materials"));
    make_thumbnails(&mut ed, &mut app);
    let px = thumbnail_pixel(&mut ed, &mut app, RED);
    assert!(near(px, [255, 0, 255]), "{px:?}");

    // Its texture changing draws it again; the other material stays.
    solid(&dir.path().join(BARK), 2, 2, [20, 200, 20, 255]);
    app.apply_file_changes(&[re_assets::AssetChange::Modified(BARK.into())]);
    make_thumbnails(&mut ed, &mut app);
    let px = thumbnail_pixel(&mut ed, &mut app, RED);
    assert!(!near(px, [255, 0, 255]), "drawn again: {px:?}");
    let px = thumbnail_pixel(&mut ed, &mut app, ROCK);
    assert!(near(px, [255, 0, 255]), "{px:?}");
    assert_eq!(cached(dir.path()).len(), 3);
}
