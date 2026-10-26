//! Shader files from the project and from the engine's shader directory
//! (ADR-0038): they replace or add to the renderer's files from the start,
//! packaged runs included; a development run rebuilds them when they change and
//! keeps the last good pipelines when they do not build; `real verify` checks
//! them. Needs a Vulkan or GL adapter (lavapipe or llvmpipe on CI).

use std::path::Path;

use re_app::{App, AppConfig};
use re_assets::AssetChange;
use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, ProjectPath};
use re_core::{Diagnostic, Location};

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 1.0, 5.0] } } },
    { "id": "cube", "components": {
        "MaterialRef": { "asset": "materials/red.material.json" },
        "MeshRef": { "asset": "builtin:cube" },
        "Transform": { "translation": [0.0, 1.0, 0.0] } } }
  ]
}
"#;

/// Tone mapping that ignores the operator and swaps channels with `swap`, from
/// a new file.
const TONEMAP: &str = "// Every operator clamps, then swaps.\n#include \"swap.wgsl\"\n\nfn tonemap(color: vec3<f32>, op: u32) -> vec3<f32> {\n    return swap(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)));\n}\n";
const SWAP_TO_BLUE: &str = "fn swap(c: vec3<f32>) -> vec3<f32> {\n    return c.bgr;\n}\n";
const SWAP_TO_GREEN: &str = "fn swap(c: vec3<f32>) -> vec3<f32> {\n    return c.grb;\n}\n";
/// Line 2 returns a `vec2` from a function that returns a `vec3`.
const SWAP_BROKEN: &str = "fn swap(c: vec3<f32>) -> vec3<f32> {\n    return c.rg;\n}\n";
/// Tone mapping that turns the red cube green, by itself.
const TONEMAP_GREEN: &str = "fn tonemap(color: vec3<f32>, op: u32) -> vec3<f32> {\n    return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)).grb;\n}\n";

/// The project's files: an unlit red cube in front of the camera, and `extra`.
fn files(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut files = vec![
        (
            "real.toml".to_owned(),
            "[project]\nname = \"shaders\"\n\n[render]\nwidth = 160\nheight = 120\n".to_owned(),
        ),
        ("scenes/main.scene.json".to_owned(), SCENE.to_owned()),
        (
            "materials/red.material.json".to_owned(),
            r#"{ "base_color": [1.0, 0.0, 0.0, 1.0], "unlit": true }"#.to_owned(),
        ),
    ];
    files.extend(
        extra
            .iter()
            .map(|(p, s)| ((*p).to_owned(), (*s).to_owned())),
    );
    files
}

fn project(extra: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files(extra) {
        write(dir.path(), &path, &text);
    }
    dir
}

fn write(dir: &Path, path: &str, text: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

/// The colour of the cube in a screenshot: "red", "green", "blue" or the pixel.
fn cube_colour(app: &mut App) -> String {
    let image = app.screenshot(None).unwrap();
    match image.pixel(80, 60) {
        [r, g, b, _] if r > 200 && g < 40 && b < 40 => "red".to_owned(),
        [r, g, b, _] if r < 40 && g > 200 && b < 40 => "green".to_owned(),
        [r, g, b, _] if r < 40 && g < 40 && b > 200 => "blue".to_owned(),
        other => format!("{other:?}"),
    }
}

/// Plays frames until `done` holds for the cube's colour and the diagnostics
/// so far, and returns those: the watcher delivers changes between frames.
fn play_until(
    app: &mut App,
    what: &str,
    done: impl Fn(&str, &[Diagnostic]) -> bool,
) -> Vec<Diagnostic> {
    let deadline = re_core::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut problems = Vec::new();
    loop {
        problems.extend(app.run_headless(Some(1), None).unwrap().diagnostics);
        let colour = cube_colour(app);
        if done(&colour, &problems) {
            return problems;
        }
        assert!(
            re_core::time::Instant::now() < deadline,
            "{what} never happened: the cube is {colour}, problems {problems:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn file_and_line(d: &Diagnostic) -> (String, Option<u32>) {
    match &d.location {
        Some(Location::File { file, line, .. }) => (file.clone(), *line),
        other => panic!("not at a file: {other:?} ({d})"),
    }
}

fn errors(diagnostics: &[Diagnostic]) -> Vec<&Diagnostic> {
    diagnostics.iter().filter(|d| d.is_error()).collect()
}

#[test]
fn project_shader_files_are_used_and_rebuilt_when_they_change() {
    let dir = project(&[
        ("shaders/tonemap.wgsl", TONEMAP),
        ("shaders/swap.wgsl", SWAP_TO_BLUE),
    ]);
    let mut app = open(dir.path());
    app.enable_hot_reload().unwrap();
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(
        cube_colour(&mut app),
        "blue",
        "the project's tone mapping draws"
    );
    assert_eq!(errors(&app.take_diagnostics()), Vec::<&Diagnostic>::new());

    // An edit of a file the replacement includes rebuilds it.
    write(dir.path(), "shaders/swap.wgsl", SWAP_TO_GREEN);
    play_until(&mut app, "the rebuild", |colour, _| colour == "green");

    // A file that does not compile is reported, and the last good pipelines
    // keep drawing.
    write(dir.path(), "shaders/swap.wgsl", SWAP_BROKEN);
    let problems = play_until(&mut app, "the compile error", |_, problems| {
        problems
            .iter()
            .any(|d| d.code == re_render::codes::SHADER_COMPILE)
    });
    for d in errors(&problems) {
        assert_eq!(d.code, re_render::codes::SHADER_COMPILE, "{d}");
        assert_eq!(
            file_and_line(d),
            ("shaders/swap.wgsl".to_owned(), Some(2)),
            "{d}"
        );
    }
    assert_eq!(cube_colour(&mut app), "green");

    // Without the replacement the engine's own file draws again; the broken
    // file is included by nothing now.
    std::fs::remove_file(dir.path().join("shaders/tonemap.wgsl")).unwrap();
    let problems = play_until(&mut app, "the engine's file", |colour, _| colour == "red");
    assert_eq!(errors(&problems), Vec::<&Diagnostic>::new());
}

#[test]
fn a_shader_file_that_does_not_compile_is_reported_at_its_project_path() {
    let dir = project(&[
        ("shaders/tonemap.wgsl", TONEMAP),
        ("shaders/swap.wgsl", SWAP_TO_BLUE),
    ]);
    let mut app = open(dir.path());
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(cube_colour(&mut app), "blue");

    write(dir.path(), "shaders/swap.wgsl", SWAP_BROKEN);
    let problems = app.apply_file_changes(&[AssetChange::Modified("shaders/swap.wgsl".into())]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    let d = &problems[0];
    assert_eq!(d.code, re_render::codes::SHADER_COMPILE, "{d}");
    assert_eq!(
        file_and_line(d),
        ("shaders/swap.wgsl".to_owned(), Some(2)),
        "{d}"
    );
    assert!(d.message.starts_with("`swap.wgsl` line 2: "), "{d}");
    assert!(
        app.take_diagnostics().contains(d),
        "also in the app's diagnostics"
    );
    assert_eq!(
        cube_colour(&mut app),
        "blue",
        "the last good pipelines draw"
    );

    write(dir.path(), "shaders/swap.wgsl", SWAP_TO_GREEN);
    let problems = app.apply_file_changes(&[AssetChange::Modified("shaders/swap.wgsl".into())]);
    assert_eq!(problems, []);
    assert_eq!(cube_colour(&mut app), "green");
}

#[test]
fn a_packaged_run_reads_its_shader_files_once_at_boot() {
    let mut layer = MemLayer::new(LayerKind::Memory);
    for (path, text) in files(&[
        ("shaders/tonemap.wgsl", TONEMAP),
        ("shaders/swap.wgsl", SWAP_TO_BLUE),
    ]) {
        layer.insert(
            ProjectPath::new(&path).unwrap(),
            text.into_bytes(),
            Form::Source,
        );
    }
    let files = ProjectFiles::for_tests(layer);
    let config = AppConfig::load_from(&files).unwrap();
    let mut app = App::open_files(files, config).unwrap();
    // Nothing to watch without a project directory.
    app.enable_hot_reload().unwrap();
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(cube_colour(&mut app), "blue");
}

#[test]
fn engine_shader_files_replace_the_built_in_ones_beneath_the_projects() {
    let engine = tempfile::tempdir().unwrap();
    write(engine.path(), "tonemap.wgsl", TONEMAP_GREEN);
    write(engine.path(), "notes.txt", "not a shader");
    let dir = project(&[]);
    let mut app = open(dir.path());
    app.watch_engine_shaders(engine.path()).unwrap();
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(
        cube_colour(&mut app),
        "green",
        "the engine directory's file draws"
    );

    // The project's file goes over it, and the engine's comes back without it.
    write(dir.path(), "shaders/tonemap.wgsl", TONEMAP);
    write(dir.path(), "shaders/swap.wgsl", SWAP_TO_BLUE);
    let created = [
        AssetChange::Created("shaders/tonemap.wgsl".into()),
        AssetChange::Created("shaders/swap.wgsl".into()),
    ];
    assert_eq!(app.apply_file_changes(&created), []);
    assert_eq!(cube_colour(&mut app), "blue");
    std::fs::remove_file(dir.path().join("shaders/tonemap.wgsl")).unwrap();
    let removed = [AssetChange::Removed("shaders/tonemap.wgsl".into())];
    assert_eq!(app.apply_file_changes(&removed), []);
    assert_eq!(cube_colour(&mut app), "green");

    // An edit in the engine directory is watched: here it does not compile.
    write(
        engine.path(),
        "tonemap.wgsl",
        &TONEMAP_GREEN.replace(".grb;", ".gr;"),
    );
    let problems = play_until(&mut app, "the compile error", |_, problems| {
        problems
            .iter()
            .any(|d| d.code == re_render::codes::SHADER_COMPILE)
    });
    let d = errors(&problems)
        .into_iter()
        .find(|d| d.code == re_render::codes::SHADER_COMPILE)
        .cloned()
        .unwrap_or_else(|| panic!("no RE6031 in {problems:?}"));
    let path = engine.path().join("tonemap.wgsl");
    let (file, line) = file_and_line(&d);
    assert_eq!(
        (file, line),
        (path.to_string_lossy().replace('\\', "/"), Some(2)),
        "{d}"
    );
    assert_eq!(cube_colour(&mut app), "green");
}

#[test]
fn a_renderer_handed_to_another_project_drops_the_first_projects_files() {
    let first = project(&[
        ("shaders/tonemap.wgsl", TONEMAP),
        ("shaders/swap.wgsl", SWAP_TO_BLUE),
    ]);
    let mut app = open(first.path());
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(cube_colour(&mut app), "blue");
    // What the editor does when it opens another project.
    let graphics = app.take_graphics().unwrap();
    let second = project(&[]);
    let mut other = open(second.path());
    other.set_graphics(graphics);
    other.run_headless(Some(1), None).unwrap();
    assert_eq!(cube_colour(&mut other), "red");
    assert_eq!(
        other
            .graphics()
            .unwrap()
            .renderer()
            .shader_sources()
            .set_files()
            .count(),
        0
    );
}

#[test]
fn verify_checks_the_shader_files_and_warns_about_replaced_engine_files() {
    let dir = project(&[
        ("shaders/tonemap.wgsl", TONEMAP),
        ("shaders/swap.wgsl", SWAP_TO_BLUE),
    ]);
    let config = AppConfig::load(dir.path()).unwrap();
    let options = re_app::VerifyOptions::default();
    let report = re_app::verify::run_project(dir.path(), &config, &options, &[]);
    let compile = report.stage("compile").unwrap();
    assert!(compile.ok, "{:?}", compile.diagnostics);
    assert_eq!(compile.data["shaders"], 2);
    let [warning] = compile.diagnostics.as_slice() else {
        panic!("one warning: {:?}", compile.diagnostics);
    };
    assert_eq!(warning.code, re_render::codes::SHADER_REPLACED);
    assert_eq!(
        file_and_line(warning),
        ("shaders/tonemap.wgsl".to_owned(), None)
    );
    assert!(
        report.ok,
        "a warning does not fail verify: {:?}",
        report.stages
    );

    write(dir.path(), "shaders/swap.wgsl", SWAP_BROKEN);
    let report = re_app::verify::run_project(dir.path(), &config, &options, &[]);
    let compile = report.stage("compile").unwrap();
    assert!(!compile.ok);
    let error = errors(&compile.diagnostics);
    assert_eq!(error.len(), 1, "{error:?}");
    assert_eq!(error[0].code, re_render::codes::SHADER_COMPILE);
    assert_eq!(
        file_and_line(error[0]),
        ("shaders/swap.wgsl".to_owned(), Some(2))
    );
}
