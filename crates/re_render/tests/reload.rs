//! Shader hot reload (ADR-0038): files set at run time rebuild the pipelines
//! that use them, and a file that does not build leaves the last good
//! pipelines drawing. Run like `tests/render.rs`.

mod common;

use common::*;
use glam::{Mat4, Vec3};
use re_core::Location;
use re_render::shader::{ROOTS, SOURCES};
use re_render::{
    Capture, Gpu, Highlight, Image, Renderer, SceneExtras, ShaderReload, Sky, Sprite, SpriteShape,
    codes,
};
use re_types::EntityId;
use re_types::render::{Bloom, DebugLine, DrawFlags, MaterialDesc, RenderSnapshot, SunRays};

const W: u32 = 160;
const H: u32 = 120;

/// The built-in text of the shader file `name`.
fn built_in(name: &str) -> &'static str {
    SOURCES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| *s)
        .unwrap_or_else(|| panic!("no built-in `{name}`"))
}

/// A scene that uses every pipeline family: lit items with a shadowed sun, a
/// sky, sprites, debug lines, a highlight, and bloom and sun rays in the post
/// chain.
fn scene(renderer: &mut Renderer) -> (RenderSnapshot, SceneExtras) {
    let plane = renderer.upload_mesh(&plane(6.0, 1.0)).unwrap();
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let mut material = |color: [f32; 4], emissive: f32| {
        renderer
            .create_material(&MaterialDesc {
                base_color: color,
                emissive: [color[0], color[1], color[2]],
                emissive_strength: emissive,
                roughness: 0.6,
                ..MaterialDesc::default()
            })
            .unwrap()
    };
    let ground = material([0.5, 0.5, 0.5, 1.0], 0.0);
    let red = material([0.8, 0.2, 0.1, 1.0], 0.0);
    let glow = material([0.2, 0.6, 1.0, 1.0], 4.0);
    let mut snapshot = RenderSnapshot {
        camera: camera(Vec3::new(0.0, 2.5, 6.0), Vec3::new(0.0, 0.6, 0.0)),
        lights: vec![sun(Vec3::new(-0.4, -1.0, -0.3), 1.0, true)],
        items: vec![
            item(1, plane, ground, Mat4::IDENTITY, SHADOWED),
            item(
                2,
                cube,
                red,
                Mat4::from_translation(Vec3::new(-0.8, 0.5, 0.0)),
                SHADOWED,
            ),
            item(
                3,
                cube,
                glow,
                Mat4::from_scale_rotation_translation(
                    Vec3::splat(0.5),
                    glam::Quat::IDENTITY,
                    Vec3::new(1.0, 0.25, 0.5),
                ),
                DrawFlags::NONE,
            ),
        ],
        debug_lines: vec![DebugLine {
            a: Vec3::new(-2.0, 0.05, 1.5),
            b: Vec3::new(2.0, 0.05, 1.5),
            color: [1.0, 1.0, 0.2, 1.0],
        }],
        clear: CLEAR,
        ..Default::default()
    };
    snapshot.post.bloom = Bloom {
        intensity: 0.6,
        ..Bloom::default()
    };
    snapshot.post.sun_rays = SunRays {
        intensity: 0.4,
        ..SunRays::default()
    };
    let extras = SceneExtras {
        sky: Some(Sky {
            zenith: [0.15, 0.3, 0.7],
            horizon: [0.55, 0.6, 0.7],
            ground: [0.3, 0.28, 0.25],
            sun: Some((Vec3::new(0.4, 1.0, 0.3), [1.0, 0.9, 0.7])),
        }),
        sprites: vec![Sprite {
            position: Vec3::new(0.2, 1.6, 0.0),
            size: 0.6,
            rotation: 0.0,
            color: [1.0, 0.6, 0.2, 0.8],
            emission: 1.0,
            shape: SpriteShape::Soft,
        }],
        highlights: vec![Highlight {
            entities: vec![EntityId(2)],
            fill: [1.0, 0.3, 0.1, 0.3],
            outline: [1.0, 1.0, 1.0, 1.0],
            always_on_top: false,
        }],
        ..SceneExtras::default()
    };
    (snapshot, extras)
}

/// A line of text near the bottom-right corner: it uses the UI pipelines and
/// leaves the scene's corners in view.
fn label(ui: &mut egui::Ui) {
    egui::Area::new(egui::Id::new("label"))
        .fixed_pos(egui::pos2(90.0, 96.0))
        .show(ui.ctx(), |ui| {
            ui.label("hot reload");
        });
}

/// The scene drawn with a label over it, and the shadow probe at two points.
fn draw(gpu: &Gpu, renderer: &mut Renderer, scene: &(RenderSnapshot, SceneExtras)) -> Image {
    let capture = Capture::new(&gpu.device, W, H).unwrap();
    let report = renderer
        .render_with(&gpu.device, &gpu.queue, capture.view(), &scene.0, &scene.1)
        .unwrap_or_else(|d| panic!("{}", d.render_human()));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let ui = ui_frame(&egui::Context::default(), W, H, 0, label);
    renderer
        .paint_ui(&gpu.device, &gpu.queue, capture.view(), &ui)
        .unwrap_or_else(|d| panic!("{}", d.render_human()));
    let probe = renderer
        .probe_sun_shadow(
            &gpu.device,
            &gpu.queue,
            &[Vec3::new(-0.8, 1.01, 0.0), Vec3::new(2.5, 0.0, 2.5)],
            Vec3::Y,
        )
        .unwrap_or_else(|d| panic!("{}", d.render_human()));
    assert!(
        probe[0].visibility > 0.9 && probe[1].visibility > 0.9,
        "{probe:?}"
    );
    let image = capture.read(&gpu.device, &gpu.queue).unwrap();
    assert_eq!(gpu.errors().count(), 0, "{:?}", gpu.errors().take());
    image
}

/// The root files the renderer builds pipelines from: the skinned ones only
/// with GPU skinning.
fn roots(renderer: &Renderer) -> Vec<&'static str> {
    let mut roots: Vec<&'static str> = ROOTS
        .iter()
        .copied()
        .filter(|root| renderer.supports_skinning() || !root.ends_with("_skinned.wgsl"))
        .collect();
    roots.sort_unstable();
    roots
}

fn rebuilt(report: &ShaderReload) -> Vec<&'static str> {
    let mut rebuilt = report.rebuilt.clone();
    rebuilt.sort_unstable();
    rebuilt
}

/// Every file can be set; every root is rebuilt, draws as before, and goes back
/// to the built-in file with `None`.
#[test]
fn every_shader_file_reloads_and_draws_the_same() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let scene = scene(&mut renderer);
    let before = draw(&gpu, &mut renderer, &scene);
    let nothing = renderer.reload_shaders();
    assert_eq!(nothing, ShaderReload::default(), "nothing changed yet");

    for (name, source) in SOURCES {
        renderer.set_shader_source(name, Some(format!("{source}\n// edited at run time\n")));
    }
    let report = renderer.reload_shaders();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(rebuilt(&report), roots(&renderer));
    assert_eq!(draw(&gpu, &mut renderer, &scene), before);
    assert_eq!(
        renderer.reload_shaders(),
        ShaderReload::default(),
        "built once"
    );

    for (name, _) in SOURCES {
        renderer.set_shader_source(name, None);
    }
    let report = renderer.reload_shaders();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(rebuilt(&report), roots(&renderer));
    assert_eq!(draw(&gpu, &mut renderer, &scene), before);
}

/// A changed file is drawn with: here the sky turns magenta through a new
/// file that `sky.wgsl` includes, then green when only that file changes.
#[test]
fn a_changed_file_is_drawn_with_and_none_restores_it() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let scene = scene(&mut renderer);
    let before = draw(&gpu, &mut renderer, &scene);
    let corner = before.pixel(2, 2);

    let sky = built_in("sky.wgsl").replace(
        "return vec4<f32>(sky(in), 0.0);",
        "return vec4<f32>(tint(), 0.0);",
    );
    renderer.set_shader_source(
        "tint.wgsl",
        Some("fn tint() -> vec3<f32> { return vec3<f32>(1.0, 0.0, 1.0); }\n".to_owned()),
    );
    renderer.set_shader_source("sky.wgsl", Some(format!("#include \"tint.wgsl\"\n{sky}")));
    let report = renderer.reload_shaders();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.rebuilt, ["sky.wgsl"]);
    let magenta = draw(&gpu, &mut renderer, &scene).pixel(2, 2);
    assert!(
        magenta[0] > 200 && magenta[1] < 60 && magenta[2] > 200,
        "{magenta:?}"
    );

    renderer.set_shader_source(
        "tint.wgsl",
        Some("fn tint() -> vec3<f32> { return vec3<f32>(0.0, 1.0, 0.0); }\n".to_owned()),
    );
    let report = renderer.reload_shaders();
    assert_eq!(
        report.rebuilt,
        ["sky.wgsl"],
        "an include change rebuilds its includers"
    );
    let green = draw(&gpu, &mut renderer, &scene).pixel(2, 2);
    assert!(
        green[0] < 60 && green[1] > 200 && green[2] < 60,
        "{green:?}"
    );

    renderer.set_shader_source("sky.wgsl", None);
    renderer.set_shader_source("tint.wgsl", None);
    let report = renderer.reload_shaders();
    assert_eq!(report.rebuilt, ["sky.wgsl"]);
    let after = draw(&gpu, &mut renderer, &scene);
    assert_eq!(after.pixel(2, 2), corner);
    assert_eq!(after, before);
}

/// A file that does not compile is one `RE6031` at its own file and line, from
/// every shader that includes it, and nothing is rebuilt.
#[test]
fn a_file_that_does_not_compile_keeps_the_last_good_pipelines() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let scene = scene(&mut renderer);
    let before = draw(&gpu, &mut renderer, &scene);

    // `lighting.wgsl` is included by forward, forward_skinned, quad and probe.
    let broken = format!(
        "{}\nfn broken() -> f32 {{\n    return 1.0 +;\n}}\n",
        built_in("lighting.wgsl")
    );
    // At the `;` where an operand should be.
    let line = broken.lines().count() as u32 - 1;
    let column = "    return 1.0 +;".find(';').unwrap() as u32 + 1;
    renderer.set_shader_source("lighting.wgsl", Some(broken));
    let report = renderer.reload_shaders();
    assert!(report.rebuilt.is_empty(), "{:?}", report.rebuilt);
    assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
    let d = &report.problems[0];
    assert_eq!(d.code, codes::SHADER_COMPILE, "{d}");
    assert_eq!(
        d.location,
        Some(Location::file_at("lighting.wgsl", line, column)),
        "{d}"
    );
    assert!(
        d.message
            .starts_with(&format!("`lighting.wgsl` line {line}: ")),
        "{d}"
    );
    assert_eq!(draw(&gpu, &mut renderer, &scene), before);

    // An include of a file that does not exist is `RE6030`, likewise.
    renderer.set_shader_source("lighting.wgsl", None);
    renderer.set_shader_source(
        "sky.wgsl",
        Some(format!(
            "#include \"missing.wgsl\"\n{}",
            built_in("sky.wgsl")
        )),
    );
    let report = renderer.reload_shaders();
    assert!(report.rebuilt.is_empty(), "{:?}", report.rebuilt);
    assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
    assert_eq!(report.problems[0].code, codes::SHADER_INCLUDE);
    assert_eq!(draw(&gpu, &mut renderer, &scene), before);
}

/// A shader naga accepts but the device refuses (here `sky.wgsl` without its
/// fragment entry point) is `RE6032`: the family it belongs to is put back,
/// and no error reaches the device's handler.
#[test]
fn a_shader_the_gpu_refuses_keeps_the_last_good_pipelines() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let scene = scene(&mut renderer);
    let before = draw(&gpu, &mut renderer, &scene);

    let renamed = built_in("sky.wgsl").replace("fn fs_sky(", "fn fs_sky_renamed(");
    renderer.set_shader_source("sky.wgsl", Some(renamed));
    let report = renderer.reload_shaders();
    assert!(report.rebuilt.is_empty(), "{:?}", report.rebuilt);
    assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
    let d = &report.problems[0];
    assert_eq!(d.code, codes::SHADER_REJECTED, "{d}");
    assert_eq!(d.location, Some(Location::file("sky.wgsl")), "{d}");
    assert!(d.message.contains("fs_sky"), "{d}");
    assert_eq!(gpu.errors().count(), 0, "{:?}", gpu.errors().take());
    assert_eq!(draw(&gpu, &mut renderer, &scene), before);

    // The next good version is built.
    renderer.set_shader_source(
        "sky.wgsl",
        Some(format!("{}\n// fixed\n", built_in("sky.wgsl"))),
    );
    let report = renderer.reload_shaders();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.rebuilt, ["sky.wgsl"]);
    assert_eq!(draw(&gpu, &mut renderer, &scene), before);
}
