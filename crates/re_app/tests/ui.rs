//! Native UI from scene components (ADR-0058 part 7): a sheet with every UI
//! component, its exact layout values at two view sizes and its goldens on
//! Vulkan and GL, also from a packaged game's processed image; world roots;
//! themes, styles and states, and files that change; and the theme codes
//! `RE7700`, `RE7701` and `RE7702`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use re_app::ui::theme::State;
use re_app::{App, AppConfig};
use re_core::Diagnostic;
use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles};
use re_render::Image;
use re_render::imgdiff::{self, Tolerance};

/// A screen root (`sheet`, laid out for a view 320 points high) with a
/// styled panel of text, an image, three buttons (one hovered by the test,
/// one disabled), a bar and a text input; a scrolled list of rows on a dark
/// shelf; a gradient card with rich text; a faded, turned tag; a second
/// screen root in points that takes no input; and a world root over an
/// entity in front of the camera.
const SHEET: &str = r#"
    { "id": "camera", "components": {
        "Camera": { "active": true, "fov_y_deg": 50.0 },
        "Transform": { "translation": [0.0, 1.0, 4.0] } } },
    { "id": "sheet", "components": { "UiRoot": { "reference_height": 320.0 } } },
    { "id": "panel", "parent": "sheet", "components": {
        "UiNode": { "x": 12.0, "y": 12.0, "width": 216.0, "height": 296.0,
            "padding_left": 12.0, "padding_top": 12.0, "padding_right": 12.0,
            "padding_bottom": 12.0, "layout": "Column", "gap": 6.0, "style": "panel" } } },
    { "id": "heading", "parent": "panel", "components": {
        "UiNode": { "width": 0.0, "width_scale": 1.0, "height": 32.0, "order": 0,
            "style": "heading" },
        "UiText": { "text": "Components" } } },
    { "id": "body", "parent": "panel", "components": {
        "UiNode": { "width": 0.0, "width_scale": 1.0, "height": 26.0, "order": 1,
            "style": "body" },
        "UiText": { "text": "From scene components." } } },
    { "id": "picture", "parent": "panel", "components": {
        "UiNode": { "width": 40.0, "height": 40.0, "order": 2 },
        "UiImage": { "image": "assets/ui/checker.png", "tint": [1.0, 0.8, 0.5, 1.0] } } },
    { "id": "play", "parent": "panel", "components": {
        "UiNode": { "width": 0.0, "width_scale": 1.0, "height": 26.0, "order": 3,
            "style": "button" },
        "UiButton": {},
        "UiText": { "text": "Play" } } },
    { "id": "save", "parent": "panel", "components": {
        "UiNode": { "width": 0.0, "width_scale": 1.0, "height": 26.0, "order": 4,
            "style": "button.primary" },
        "UiButton": {},
        "UiText": { "text": "Save" } } },
    { "id": "quit", "parent": "panel", "components": {
        "UiNode": { "width": 0.0, "width_scale": 1.0, "height": 26.0, "order": 5,
            "style": "button" },
        "UiButton": { "enabled": false },
        "UiText": { "text": "Quit" } } },
    { "id": "health", "parent": "panel", "components": {
        "UiNode": { "width": 0.0, "width_scale": 1.0, "height": 10.0, "order": 6,
            "style": "bar" },
        "UiBar": { "value": 0.65 } } },
    { "id": "name", "parent": "panel", "components": {
        "UiNode": { "width": 0.0, "width_scale": 1.0, "height": 26.0, "order": 7,
            "style": "input", "padding_left": 8.0 },
        "UiTextInput": { "placeholder": "Your name" },
        "UiText": { "text": "" } } },
    { "id": "shelf", "parent": "sheet", "components": {
        "UiNode": { "x": 236.0, "y": 8.0, "width": 236.0, "height": 128.0 },
        "UiPanel": { "color": [0.01, 0.012, 0.02, 0.9], "corner": 10.0 } } },
    { "id": "list", "parent": "sheet", "components": {
        "UiNode": { "x": 240.0, "y": 12.0, "width": 228.0, "height": 120.0,
            "layout": "Column", "gap": 4.0, "style": "list" },
        "UiScroll": { "scroll_y": 40.0 } } },
    { "id": "row1", "parent": "list", "components": {
        "UiNode": { "width": -6.0, "width_scale": 1.0, "height": 28.0, "order": 1, "style": "row",
            "padding_left": 8.0 },
        "UiText": { "text": "Row 1" } } },
    { "id": "row2", "parent": "list", "components": {
        "UiNode": { "width": -6.0, "width_scale": 1.0, "height": 28.0, "order": 2, "style": "row",
            "padding_left": 8.0 },
        "UiText": { "text": "Row 2" } } },
    { "id": "row3", "parent": "list", "components": {
        "UiNode": { "width": -6.0, "width_scale": 1.0, "height": 28.0, "order": 3, "style": "row",
            "padding_left": 8.0 },
        "UiText": { "text": "Row 3" } } },
    { "id": "row4", "parent": "list", "components": {
        "UiNode": { "width": -6.0, "width_scale": 1.0, "height": 28.0, "order": 4, "style": "row",
            "padding_left": 8.0 },
        "UiText": { "text": "Row 4" } } },
    { "id": "row5", "parent": "list", "components": {
        "UiNode": { "width": -6.0, "width_scale": 1.0, "height": 28.0, "order": 5, "style": "row",
            "padding_left": 8.0 },
        "UiText": { "text": "Row 5" } } },
    { "id": "row6", "parent": "list", "components": {
        "UiNode": { "width": -6.0, "width_scale": 1.0, "height": 28.0, "order": 6, "style": "row",
            "padding_left": 8.0 },
        "UiText": { "text": "Row 6" } } },
    { "id": "row7", "parent": "list", "components": {
        "UiNode": { "width": -6.0, "width_scale": 1.0, "height": 28.0, "order": 7, "style": "row",
            "padding_left": 8.0 },
        "UiText": { "text": "Row 7" } } },
    { "id": "row8", "parent": "list", "components": {
        "UiNode": { "width": -6.0, "width_scale": 1.0, "height": 28.0, "order": 8, "style": "row",
            "padding_left": 8.0 },
        "UiText": { "text": "Row 8" } } },
    { "id": "card", "parent": "sheet", "components": {
        "UiNode": { "x": 240.0, "y": 142.0, "width": 228.0, "height": 56.0 },
        "UiPanel": { "color": [0.05, 0.3, 0.6, 1.0], "corner": 10.0, "outline": 2.0,
            "outline_color": [1.0, 1.0, 1.0, 1.0], "gradient": [0.6, 0.1, 0.4, 1.0],
            "gradient_angle_deg": 30.0 } } },
    { "id": "card_text", "parent": "card", "components": {
        "UiNode": { "width": 0.0, "height": 0.0, "width_scale": 1.0, "height_scale": 1.0 },
        "UiText": { "text": "Gradient, <b>bold</b> and <font size=\"26\">big</font>",
            "rich": true, "align_x": "Center", "align_y": "Center",
            "outline_color": [0.0, 0.0, 0.0, 1.0] } } },
    { "id": "tag", "parent": "sheet", "components": {
        "UiNode": { "x": 372.0, "y": 204.0, "width": 96.0, "height": 30.0, "opacity": 0.5,
            "rotation_deg": -6.0 },
        "UiPanel": { "color": [0.9, 0.5, 0.1, 1.0], "corner": 6.0 } } },
    { "id": "tag_text", "parent": "tag", "components": {
        "UiNode": { "width": 0.0, "height": 0.0, "width_scale": 1.0, "height_scale": 1.0 },
        "UiText": { "text": "Faded", "font": "Mono", "weight": 700, "size": 20.0,
            "align_x": "Center", "align_y": "Center" } } },
    { "id": "hud", "components": { "UiRoot": { "order": 1, "input": false } } },
    { "id": "score", "parent": "hud", "components": {
        "UiNode": { "x": -12.0, "x_scale": 1.0, "y": -12.0, "y_scale": 1.0,
            "anchor_x": 1.0, "anchor_y": 1.0, "width": 140.0, "height": 28.0 },
        "UiText": { "text": "Score 120", "font": "Display", "weight": 700, "size": 22.0,
            "align_x": "End", "align_y": "Center", "outline_color": [0.0, 0.0, 0.0, 1.0] } } },
    { "id": "sign", "components": {
        "Transform": { "translation": [0.7, -0.25, 0.0] },
        "UiRoot": { "space": "World", "world_height": 0.25, "offset": [0.0, 0.2, 0.0] },
        "UiNode": { "width": 160.0, "height": 40.0 },
        "UiPanel": { "color": [0.0, 0.0, 0.0, 0.6], "corner": 8.0 } } },
    { "id": "sign_text", "parent": "sign", "components": {
        "UiNode": { "width": 0.0, "height": 0.0, "width_scale": 1.0, "height_scale": 1.0 },
        "UiText": { "text": "Shop", "align_x": "Center", "align_y": "Center" } } }
"#;

/// The ids of [`SHEET`]'s entities with UI components.
const IDS: &[&str] = &[
    "sheet",
    "panel",
    "heading",
    "body",
    "picture",
    "play",
    "save",
    "quit",
    "health",
    "name",
    "shelf",
    "list",
    "row1",
    "row2",
    "row3",
    "row4",
    "row5",
    "row6",
    "row7",
    "row8",
    "card",
    "card_text",
    "tag",
    "tag_text",
    "hud",
    "score",
    "sign",
    "sign_text",
];

/// A project whose view is `width` by `height` with `entities` (scene JSON
/// objects, comma-separated) in its entry scene, an 8 by 8 checker texture
/// and `extra` files.
fn project(width: u32, height: u32, entities: &str, extra: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    for d in ["scenes", "assets/ui", "ui"] {
        std::fs::create_dir_all(p.join(d)).unwrap();
    }
    std::fs::write(
        p.join("real.toml"),
        format!("[project]\nname = \"ui\"\n\n[render]\nwidth = {width}\nheight = {height}\n"),
    )
    .unwrap();
    std::fs::write(
        p.join("scenes/main.scene.json"),
        format!(
            "{{ \"format\": \"real-engine/scene\", \"version\": 1, \"entities\": [{entities}] }}\n"
        ),
    )
    .unwrap();
    let mut rgba = Vec::new();
    for y in 0..8u32 {
        for x in 0..8u32 {
            let v = if (x + y) % 2 == 0 { 255 } else { 40 };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    Image {
        width: 8,
        height: 8,
        rgba,
    }
    .save_png(&p.join("assets/ui/checker.png"))
    .unwrap();
    for (path, text) in extra {
        std::fs::write(p.join(path), text).unwrap();
    }
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn codes(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics.iter().map(|d| d.code.to_string()).collect()
}

/// Runs `frames` frames, failing on any error.
fn run(app: &mut App, frames: u64) -> Vec<Diagnostic> {
    let report = app.run_headless(Some(frames), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    report.diagnostics
}

fn f(x: f32) -> String {
    format!("{x:?}")
}

/// Every visit of the last Sync as text, by entity id, each `f32` written
/// so that it reads back to the same bits.
fn dump(app: &App) -> String {
    let names: HashMap<_, &str> = IDS
        .iter()
        .map(|id| (app.world().entity(id).unwrap(), *id))
        .collect();
    let ui = app.ui();
    let mut s = String::new();
    for v in ui.visits() {
        let name = match ui.built().entity(v.node) {
            Some(e) => names.get(&e).copied().unwrap_or("?"),
            None => "(root)",
        };
        let _ = write!(
            s,
            "{name} at ({}, {}) size ({}, {}) rotation {}",
            f(v.position.x),
            f(v.position.y),
            f(v.size.x),
            f(v.size.y),
            f(v.rotation)
        );
        if let Some((bounds, fits)) = v.text {
            let _ = write!(
                s,
                " text ({}, {}) {}",
                f(bounds.x),
                f(bounds.y),
                if fits { "fits" } else { "overflows" }
            );
        }
        if let Some(sc) = v.scroll {
            let _ = write!(s, " canvas ({}, {})", f(sc.canvas.x), f(sc.canvas.y));
            if let Some(at) = sc.clamped {
                let _ = write!(s, " held at ({}, {})", f(at.x), f(at.y));
            }
        }
        if let Some(a) = v.arranged {
            let _ = write!(s, " content ({}, {})", f(a.content.x), f(a.content.y));
        }
        s.push('\n');
    }
    s
}

/// Compares `actual` with `tests/fixtures/ui/<name>.txt`;
/// `RE_UPDATE_GOLDENS=1` writes the file instead. A difference leaves the
/// whole output in `out/re_app/ui/` and names the first line that differs.
fn expect_values(name: &str, actual: &str) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("tests/fixtures/ui").join(format!("{name}.txt"));
    if std::env::var_os("RE_UPDATE_GOLDENS").is_some_and(|v| !v.is_empty()) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        println!("updated {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "{}: {e}: run with RE_UPDATE_GOLDENS=1 and review the file",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    if expected == actual {
        return;
    }
    let out = root.join("../../out/re_app/ui");
    std::fs::create_dir_all(&out).unwrap();
    let saved = out.join(format!("{name}.txt"));
    std::fs::write(&saved, actual).unwrap();
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    let n = (0..e.len().max(a.len()))
        .find(|&i| e.get(i) != a.get(i))
        .unwrap_or(e.len());
    panic!(
        "{name}: line {} differs from {}\n  expected: {}\n  actual:   {}\n(the whole output is in {})",
        n + 1,
        path.display(),
        e.get(n).unwrap_or(&"(end)"),
        a.get(n).unwrap_or(&"(end)"),
        saved.display()
    );
}

/// The sheet's app at `width` by `height`, with `save` hovered, after one
/// frame.
fn sheet(width: u32, height: u32, graphics: bool) -> (tempfile::TempDir, App) {
    let dir = project(width, height, SHEET, &[]);
    let mut app = open(dir.path());
    if graphics {
        app.ensure_graphics().unwrap();
    }
    hover_and_run(&mut app);
    (dir, app)
}

/// Hovers the sheet's `save` and runs one frame.
fn hover_and_run(app: &mut App) {
    let save = app.world().entity("save").unwrap();
    app.ui_mut().set_state(
        save,
        State {
            hover: true,
            ..State::default()
        },
    );
    let diagnostics = run(app, 1);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// The layout values of every component, exactly, in a headless run at two
/// view sizes: the sheet scales with the view's height, the second screen
/// root stays in points, and the world root is placed by the camera.
#[test]
fn components_lay_out_exactly_at_two_view_sizes() {
    for (w, h) in [(480, 320), (1280, 720)] {
        let (_dir, app) = sheet(w, h, false);
        expect_values(&format!("sheet_{w}x{h}"), &dump(&app));
    }
}

// --- Goldens ------------------------------------------------------------------

fn golden_set(app: &App) -> Option<&'static str> {
    match app.graphics()?.backend() {
        "vulkan" => Some("vulkan"),
        "gl" => Some("gl"),
        _ => None,
    }
}

/// Compares `image` with `tests/goldens/<set>/<name>.png` at the M1 tolerance,
/// as `re_render`'s goldens do (Linux only). The actual image goes to
/// `out/re_app/<set>/`; `RE_UPDATE_GOLDENS=1` rewrites the golden.
fn check_golden(app: &App, name: &str, image: &Image) {
    let Some(set) = golden_set(app).filter(|_| cfg!(target_os = "linux")) else {
        println!("no golden set here; skipping the comparison of {name}");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = root.join("../../out/re_app").join(set);
    image.save_png(&out.join(format!("{name}.png"))).unwrap();
    let golden_path = root
        .join("tests/goldens")
        .join(set)
        .join(format!("{name}.png"));
    if std::env::var_os("RE_UPDATE_GOLDENS").is_some_and(|v| !v.is_empty()) {
        image.save_png(&golden_path).unwrap();
        println!("updated {}", golden_path.display());
        return;
    }
    let golden = Image::load_png(&golden_path).unwrap_or_else(|d| {
        panic!(
            "{}missing golden: run with RE_UPDATE_GOLDENS=1 and review the image",
            d.render_human()
        )
    });
    let report = imgdiff::compare(image, &golden, Tolerance::default())
        .unwrap_or_else(|d| panic!("{}", d.render_human()));
    println!("{name} ({set}): {}", report.summary());
    if !report.matches() {
        let diff = imgdiff::diff_image(image, &golden, Tolerance::default()).unwrap();
        diff.save_png(&out.join(format!("{name}.diff.png")))
            .unwrap();
        panic!(
            "{name} does not match its {set} golden: {} (actual and diff in {})",
            report.summary(),
            out.display()
        );
    }
}

/// Every component drawn, at two view sizes: panels with corners, outlines
/// and a gradient, text in the three families with rich runs and outlines,
/// a tinted image, buttons in their states, a bar, a text input's
/// placeholder, a clipped scrolled list, opacity and rotation, and a world
/// root's plate.
#[test]
fn components_draw_at_two_view_sizes() {
    for (w, h) in [(480, 320), (1280, 720)] {
        let (_dir, mut app) = sheet(w, h, true);
        let image = app.screenshot(None).unwrap();
        assert_eq!((image.width, image.height), (w, h));
        check_golden(&app, &format!("ui_sheet_{w}x{h}"), &image);
    }
}

/// A packaged game stores an image the build imported as the importer's
/// output (block-compressed where the encoder is built in), not its source
/// (ADR-0037). The UI reads every image in that form, a development run's
/// too, so the packaged game draws exactly what the project shows.
#[test]
fn a_packaged_game_draws_images_from_their_processed_form() {
    let dir = project(480, 320, SHEET, &[]);
    let (_, mut dev) = sheet(480, 320, true);
    let in_project = dev.screenshot(None).unwrap();
    let image = "assets/ui/checker.png";
    let mut server = re_assets::AssetServer::with_files(ProjectFiles::project(dir.path()));
    let processed = server.processed(image).unwrap().expect("a PNG imports");
    // Block-compressed where the encoder is built in, as in `real build` and
    // in CI's test run, which builds re_cli's with it.
    assert_eq!(
        processed.textures[0].format.is_compressed(),
        re_assets::TEXTURE_ENCODER
    );
    let mut layer = MemLayer::new(LayerKind::Memory);
    for path in ProjectFiles::project(dir.path()).list("", "") {
        if path.as_str().starts_with(".real/") {
            continue;
        }
        if path.as_str() == image {
            let form = Form::Processed {
                importer: processed.importer.into(),
                version: processed.version,
            };
            layer.insert(path, processed.bytes.clone(), form);
        } else {
            let bytes = std::fs::read(dir.path().join(path.as_str())).unwrap();
            layer.insert(path, bytes, Form::Source);
        }
    }
    let files = ProjectFiles::for_tests(layer);
    assert!(files.read_source(image).is_err(), "no source to read");
    let config = AppConfig::load_from(&files).unwrap();
    let mut app = App::open_files(files, config).unwrap();
    app.ensure_graphics().unwrap();
    hover_and_run(&mut app);
    let picture = app.world().entity("picture").unwrap();
    let v = app.ui().visit(picture).unwrap();
    let node = app.ui().built().tree.node(v.node);
    assert!(
        matches!(node.image, Some(re_ui::Image::Texture { .. })),
        "{:?}",
        node.image
    );
    let shot = app.screenshot(None).unwrap();
    check_golden(&app, "ui_sheet_480x320", &shot);
    assert!(
        shot.rgba == in_project.rgba,
        "the project draws its image unlike the package"
    );
}

/// A copy of the app that borrows its graphics (`play.run`) lays out and
/// paints with the app's UI context, whose font atlas and images those
/// graphics hold: its screenshot is the app's own, HUD text included, and the
/// app draws as before once the graphics come back.
#[test]
fn a_play_copy_paints_with_the_ui_context_it_borrows() {
    let hud = r#", { "id": "lives", "components": { "HudText": { "text": "Lives 3" } } }"#;
    let dir = project(480, 320, &format!("{SHEET}{hud}"), &[]);
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    run(&mut app, 1);
    let live = app.screenshot(None).unwrap();
    let (report, _, copy) = app.play_copy(1, None, &[], true).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert!(
        copy.unwrap().rgba == live.rgba,
        "the copy draws the UI unlike the app"
    );
    run(&mut app, 1);
    assert!(
        app.screenshot(None).unwrap().rgba == live.rgba,
        "the app's UI changed"
    );
}

// --- Themes, styles and their codes -------------------------------------------

/// A screen root with `nodes` under it.
fn screen(nodes: &str) -> String {
    format!(r#"{{ "id": "root", "components": {{ "UiRoot": {{}} }} }}, {nodes}"#)
}

/// The look `entity`'s node was built with at the last Sync.
fn look_of(app: &App, id: &str) -> (Option<f32>, [f32; 4], Option<f32>) {
    let ui = app.ui();
    let e = app.world().entity(id).unwrap();
    let v = ui
        .visit(e)
        .unwrap_or_else(|| panic!("{id} was not laid out"));
    let node = ui.built().tree.node(v.node);
    let corner = node.look.corner.map(|c| c.offset);
    let size = node.text.as_ref().map(|t| t.size);
    (corner, node.look.background.0, size)
}

/// A style sets a node's look over its components, and its state variants
/// over that; fields the style does not set stay the node's.
#[test]
fn styles_and_states_set_the_look() {
    let dir = project(
        480,
        320,
        &screen(
            r#"
            { "id": "plain", "parent": "root", "components": {
                "UiNode": {}, "UiPanel": { "color": [1.0, 0.0, 0.0, 1.0], "corner": 3.0 } } },
            { "id": "styled", "parent": "root", "components": {
                "UiNode": { "style": "button" }, "UiButton": {},
                "UiText": { "text": "OK", "wrap": true } } },
            { "id": "pressed", "parent": "root", "components": {
                "UiNode": { "style": "button" }, "UiButton": {}, "UiText": { "text": "OK" } } },
            { "id": "off", "parent": "root", "components": {
                "UiNode": { "style": "button" }, "UiButton": { "enabled": false },
                "UiText": { "text": "OK" } } }"#,
        ),
        &[],
    );
    let mut app = open(dir.path());
    let e = app.world().entity("pressed").unwrap();
    app.ui_mut().set_state(
        e,
        State {
            pressed: true,
            ..State::default()
        },
    );
    assert!(run(&mut app, 1).is_empty());
    let (corner, plain, _) = look_of(&app, "plain");
    assert_eq!(corner, Some(3.0));
    // Linear red in sRGB.
    assert_eq!(plain, [1.0, 0.0, 0.0, 1.0]);
    let (corner, styled, size) = look_of(&app, "styled");
    assert_eq!(
        (corner, size),
        (Some(8.0), Some(18.0)),
        "the button style's"
    );
    let e = app.world().entity("styled").unwrap();
    let wraps = |app: &App| {
        let v = app.ui().visit(e).unwrap();
        app.ui()
            .built()
            .tree
            .node(v.node)
            .text
            .as_ref()
            .unwrap()
            .wrapped
    };
    assert!(wraps(&app), "UiText.wrap, which the style does not set");
    let (_, pressed, _) = look_of(&app, "pressed");
    let (_, off, _) = look_of(&app, "off");
    assert_ne!(pressed, styled, "the pressed variant");
    assert_ne!(off, styled, "the disabled variant");
    assert_ne!(off, pressed);
    // Out of the state, the node looks as its style says again.
    let pressed = app.world().entity("pressed").unwrap();
    app.ui_mut().set_state(pressed, State::default());
    assert!(run(&mut app, 1).is_empty());
    assert_eq!(look_of(&app, "pressed").1, styled);
}

/// A style the theme lacks is `RE7701`, once per name however many nodes
/// and frames name it; the node keeps its own look.
#[test]
fn a_missing_style_is_re7701_once() {
    let dir = project(
        480,
        320,
        &screen(
            r#"
            { "id": "a", "parent": "root", "components": {
                "UiNode": { "style": "nope" }, "UiPanel": { "color": [1.0, 0.0, 0.0, 1.0] } } },
            { "id": "b", "parent": "root", "components": { "UiNode": { "style": "nope" } } }"#,
        ),
        &[],
    );
    let mut app = open(dir.path());
    let first = run(&mut app, 3);
    assert_eq!(codes(&first), ["RE7701"], "{first:?}");
    assert!(first[0].message.contains("`nope`"), "{}", first[0].message);
    assert_eq!(look_of(&app, "a").1, [1.0, 0.0, 0.0, 1.0]);
    assert!(run(&mut app, 3).is_empty());
}

/// In a development run, a field set away from its default on a node whose
/// style also sets it is `RE7702`, once per node and field, since the
/// style's value is the one that shows.
#[test]
fn a_field_a_style_hides_is_re7702_once() {
    let dir = project(
        480,
        320,
        &screen(
            r#"
            { "id": "fine", "parent": "root", "components": {
                "UiNode": { "style": "button" }, "UiButton": {},
                "UiText": { "text": "Fine", "wrap": true } } },
            { "id": "hidden", "parent": "root", "components": {
                "UiNode": { "style": "button" }, "UiButton": {},
                "UiPanel": { "color": [1.0, 0.0, 0.0, 1.0] },
                "UiText": { "text": "Hidden", "size": 30.0 } } }"#,
        ),
        &[],
    );
    let mut app = open(dir.path());
    let first = run(&mut app, 3);
    assert_eq!(codes(&first), ["RE7702", "RE7702"], "{first:?}");
    let messages: Vec<&str> = first.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("UiPanel.color")),
        "{messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("UiText.size")),
        "{messages:?}"
    );
    assert_eq!(
        look_of(&app, "hidden").2,
        Some(18.0),
        "the style's size shows"
    );
    assert!(run(&mut app, 3).is_empty());
}

/// A project theme that cannot be used is `RE7700` when the app opens, and
/// the engine's theme is used instead. A root's own theme that cannot be
/// used is `RE7700` once, and the root uses the project's; one that loads
/// replaces the project's for that root.
#[test]
fn themes_layer_and_a_bad_one_is_re7700() {
    let round = r#"{ "format": "real-engine/theme", "version": 1,
        "styles": { "button": { "base": "button", "panel": { "corner": 2.0 } } } }"#;
    let nodes = r#"
        { "id": "a", "parent": "root", "components": {
            "UiNode": { "style": "button" }, "UiButton": {}, "UiText": { "text": "A" } } },
        { "id": "own", "components": { "UiRoot": { "order": 1, "theme": "ui/round.theme.json" } } },
        { "id": "b", "parent": "own", "components": {
            "UiNode": { "style": "button" }, "UiButton": {}, "UiText": { "text": "B" } } },
        { "id": "lost", "components": { "UiRoot": { "order": 2, "theme": "ui/lost.theme.json" } } },
        { "id": "c", "parent": "lost", "components": {
            "UiNode": { "style": "button" }, "UiButton": {}, "UiText": { "text": "C" } } }"#;
    let dir = project(
        480,
        320,
        &screen(nodes),
        &[
            ("ui/bad.theme.json", r#"{ "format": "real-engine/theme" }"#),
            ("ui/round.theme.json", round),
        ],
    );
    let toml = dir.path().join("real.toml");
    let mut config = std::fs::read_to_string(&toml).unwrap();
    config.push_str("\n[ui]\ntheme = \"ui/bad.theme.json\"\n");
    std::fs::write(&toml, config).unwrap();
    let mut app = open(dir.path());
    let opened = app.take_diagnostics();
    assert_eq!(codes(&opened), ["RE7700"], "{opened:?}");
    let report = app.run_headless(Some(2), None).unwrap();
    assert_eq!(
        codes(&report.diagnostics),
        ["RE7700"],
        "{:?}",
        report.diagnostics
    );
    assert!(report.diagnostics[0].message.contains("lost.theme.json"));
    // The engine's button, the root theme's rounder one, and the project's
    // (here the engine's) for the root whose theme is missing.
    assert_eq!(look_of(&app, "a").0, Some(8.0));
    assert_eq!(look_of(&app, "b").0, Some(2.0));
    assert_eq!(look_of(&app, "c").0, Some(8.0));
    let report = app.run_headless(Some(2), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

/// Theme files and images that change on disk load again (a development
/// run's watcher, or a command's write): the themes in use at once, and an
/// image at the next Sync. A theme that no longer loads is `RE7700`, and
/// the last good one stays.
#[test]
fn changed_themes_and_images_load_again() {
    let theme = |corner: f32| {
        format!(
            r#"{{ "format": "real-engine/theme", "version": 1, "styles": {{
                "button": {{ "base": "button", "panel": {{ "corner": {corner:?} }} }} }} }}"#
        )
    };
    let nodes = r#"
        { "id": "a", "parent": "root", "components": {
            "UiNode": { "style": "button" }, "UiButton": {}, "UiText": { "text": "A" } } },
        { "id": "picture", "parent": "root", "components": {
            "UiNode": { "y": 100.0, "width": 40.0, "height": 40.0 },
            "UiImage": { "image": "assets/ui/checker.png" } } },
        { "id": "own", "components": { "UiRoot": { "order": 1, "theme": "ui/own.theme.json" } } },
        { "id": "b", "parent": "own", "components": {
            "UiNode": { "style": "button" }, "UiButton": {}, "UiText": { "text": "B" } } }"#;
    let (game, own) = (theme(2.0), theme(3.0));
    let dir = project(
        480,
        320,
        &screen(nodes),
        &[("ui/game.theme.json", &game), ("ui/own.theme.json", &own)],
    );
    let toml = dir.path().join("real.toml");
    let mut config = std::fs::read_to_string(&toml).unwrap();
    config.push_str("\n[ui]\ntheme = \"ui/game.theme.json\"\n");
    std::fs::write(&toml, config).unwrap();
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    assert!(run(&mut app, 1).is_empty());
    let texture = |app: &App| {
        let e = app.world().entity("picture").unwrap();
        let v = app.ui().visit(e).unwrap();
        match app.ui().built().tree.node(v.node).image {
            Some(re_ui::Image::Texture { texture, .. }) => texture,
            ref other => panic!("{other:?}"),
        }
    };
    let first = texture(&app);
    assert_eq!(look_of(&app, "a").0, Some(2.0));
    assert_eq!(look_of(&app, "b").0, Some(3.0));

    let changed = |p: &str| re_assets::AssetChange::Modified(p.to_owned());
    std::fs::write(dir.path().join("ui/game.theme.json"), theme(5.0)).unwrap();
    std::fs::write(dir.path().join("ui/own.theme.json"), theme(6.0)).unwrap();
    let checker = dir.path().join("assets/ui/checker.png");
    let mut png = Image::load_png(&checker).unwrap();
    png.rgba[0] = 0;
    png.save_png(&checker).unwrap();
    let problems = app.apply_file_changes(&[
        changed("ui/game.theme.json"),
        changed("ui/own.theme.json"),
        changed("assets/ui/checker.png"),
    ]);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(run(&mut app, 1).is_empty());
    assert_eq!(look_of(&app, "a").0, Some(5.0));
    assert_eq!(look_of(&app, "b").0, Some(6.0));
    assert_ne!(texture(&app), first, "the image loaded again");
    // Its sidecar's settings change how it is processed, so it loads again
    // when the sidecar changes too.
    let second = texture(&app);
    let problems = app.apply_file_changes(&[changed("assets/ui/checker.png.meta.json")]);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(run(&mut app, 1).is_empty());
    assert_ne!(
        texture(&app),
        second,
        "the image loaded again for its sidecar"
    );

    for path in ["ui/game.theme.json", "ui/own.theme.json"] {
        std::fs::write(dir.path().join(path), "{").unwrap();
    }
    let problems =
        app.apply_file_changes(&[changed("ui/game.theme.json"), changed("ui/own.theme.json")]);
    assert_eq!(codes(&problems), ["RE7700", "RE7700"], "{problems:?}");
    app.take_diagnostics();
    assert!(run(&mut app, 1).is_empty());
    assert_eq!(look_of(&app, "a").0, Some(5.0), "the last good theme");
    assert_eq!(look_of(&app, "b").0, Some(6.0), "the last good theme");
}

/// An image that cannot be loaded is `RE0520` once, with the asset loader's
/// error, and its node is drawn without it.
#[test]
fn an_image_that_cannot_be_loaded_is_re0520_once() {
    let nodes = r#"{ "id": "picture", "parent": "root", "components": {
        "UiNode": { "width": 40.0, "height": 40.0 },
        "UiImage": { "image": "assets/ui/missing.png" } } }"#;
    let dir = project(480, 320, &screen(nodes), &[]);
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    let report = app.run_headless(Some(3), None).unwrap();
    let found: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code.to_string() == "RE0520")
        .collect();
    assert_eq!(found.len(), 1, "{:?}", report.diagnostics);
    assert_eq!(codes(&found[0].related), ["RE7001"], "{found:?}");
    let e = app.world().entity("picture").unwrap();
    let v = app.ui().visit(e).unwrap();
    assert!(app.ui().built().tree.node(v.node).image.is_none());
}

// --- World roots ----------------------------------------------------------------

/// A world root `id` at `x, y, z`, `height` metres high, with a 100 by 50
/// point canvas.
fn plate(id: &str, at: [f32; 3], height: f32, max_distance: f32) -> String {
    format!(
        r#"{{ "id": "{id}", "components": {{
            "Transform": {{ "translation": [{}, {}, {}] }},
            "UiRoot": {{ "space": "World", "world_height": {height}, "max_distance": {max_distance} }},
            "UiNode": {{ "width": 100.0, "height": 50.0 }},
            "UiPanel": {{ "color": [1.0, 1.0, 1.0, 1.0] }} }} }}"#,
        at[0], at[1], at[2]
    )
}

/// World roots face the camera at their height in metres, far ones drawn
/// first; one behind the camera or past its `max_distance` is not drawn.
#[test]
fn world_roots_are_placed_by_the_camera() {
    let entities = [
        r#"{ "id": "camera", "components": {
            "Camera": { "active": true, "fov_y_deg": 60.0 },
            "Transform": { "translation": [0.0, 0.0, 10.0] } } }"#
            .to_string(),
        plate("near", [0.0, 0.0, 6.0], 1.0, 0.0),
        plate("far", [0.0, 0.0, -10.0], 1.0, 0.0),
        plate("behind", [0.0, 0.0, 12.0], 1.0, 0.0),
        plate("limited", [1.0, 0.0, 0.0], 1.0, 5.0),
    ];
    let dir = project(640, 360, &entities.join(","), &[]);
    let mut app = open(dir.path());
    assert!(run(&mut app, 1).is_empty());
    let ui = app.ui();
    let at = |id: &str| ui.visit(app.world().entity(id).unwrap()).cloned();
    assert!(at("behind").is_none() && at("limited").is_none());
    let (near, far) = (at("near").unwrap(), at("far").unwrap());
    // 1 metre at 4 and at 20 metres, by a 60 degree view 360 points high.
    let focal = 180.0 / libm::tanf(30f32.to_radians());
    assert!((near.size.y - focal / 4.0).abs() < 1e-3, "{near:?}");
    assert!((far.size.y - focal / 20.0).abs() < 1e-3, "{far:?}");
    assert!((near.size.x - 2.0 * near.size.y).abs() < 1e-3, "{near:?}");
    // Centred on the view.
    for v in [near, far] {
        let centre = v.position + v.size * 0.5;
        assert!(
            (centre - glam::Vec2::new(320.0, 180.0)).length() < 1e-3,
            "{v:?}"
        );
    }
    let order: Vec<_> = ui
        .visits()
        .iter()
        .filter_map(|v| ui.built().entity(v.node))
        .map(|e| {
            if e == app.world().entity("far").unwrap() {
                "far"
            } else {
                "near"
            }
        })
        .collect();
    assert_eq!(order, ["far", "near"], "far roots first");
}

// --- The theme's fields ---------------------------------------------------------

/// A field's kind in a JSON Schema: `colour`, `number`, `integer`, `boolean`
/// or the values of an enum, whatever the schema's way of writing it.
fn kind(schema: &serde_json::Value, defs: &serde_json::Value) -> String {
    use serde_json::Value;
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        return kind(&defs[r.trim_start_matches("#/$defs/")], defs);
    }
    if let Some(any) = schema.get("anyOf").and_then(Value::as_array) {
        let not_null: Vec<&Value> = any.iter().filter(|s| s["type"] != "null").collect();
        if let [one] = not_null[..] {
            return kind(one, defs);
        }
    }
    let mut values: Vec<String> = Vec::new();
    if let Some(e) = schema.get("enum").and_then(Value::as_array) {
        values.extend(e.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if let Some(one) = schema.get("oneOf").and_then(Value::as_array) {
        for v in one {
            if let Some(c) = v.get("const").and_then(Value::as_str) {
                values.push(c.to_string());
            }
            if let Some(e) = v.get("enum").and_then(Value::as_array) {
                values.extend(e.iter().filter_map(Value::as_str).map(str::to_string));
            }
        }
    }
    if !values.is_empty() {
        values.sort();
        return format!("one of {values:?}");
    }
    let ty = match &schema["type"] {
        Value::Array(types) => types
            .iter()
            .find(|t| *t != "null")
            .cloned()
            .unwrap_or_default(),
        t => t.clone(),
    };
    match ty.as_str() {
        Some("array") if schema["items"]["type"] == "number" && schema["maxItems"] == 4 => {
            "colour".into()
        }
        Some(t) => t.into(),
        None => format!("unknown: {schema}"),
    }
}

/// Every field a style can set is a field of its component, of the same
/// kind, so a theme and a scene mean the same thing by a name.
#[test]
fn style_fields_are_component_fields() {
    let scene = re_scene::World::new().scene_json_schema();
    let theme = serde_json::to_value(schemars::schema_for!(re_app::ui::theme::ThemeFile)).unwrap();
    let mut checked = 0;
    for (patch, component) in [
        ("PanelPatch", "UiPanel"),
        ("TextPatch", "UiText"),
        ("BarPatch", "UiBar"),
        ("ScrollPatch", "UiScroll"),
    ] {
        let fields = theme["$defs"][patch]["properties"].as_object().unwrap();
        for (name, field) in fields {
            let theirs = &scene["$defs"][component]["properties"][name];
            assert!(!theirs.is_null(), "{component} has no field `{name}`");
            assert_eq!(
                kind(field, &theme["$defs"]),
                kind(theirs, &scene["$defs"]),
                "{patch}.{name} and {component}.{name}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 21);
}
