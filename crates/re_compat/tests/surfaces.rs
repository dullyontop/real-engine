//! WP27 "surfaces": a `SurfaceGui` on a part and a `ViewportFrame` in the
//! player's UI, drawn by the app (`re_app`) as `real run` and `real verify`
//! draw them, against golden images.

use re_compat::CompatSetup;
use re_compat::scene::create_place;
use re_scene::{SaveOptions, World};

/// Builds the sign, the viewport and its parts from a server script.
const BUILD: &str = r#"
local sign = Instance.new("Part")
sign.Name = "Sign"
sign.Anchored = true
sign.Size = Vector3.new(16, 8, 1)
sign.CFrame = CFrame.new(-6, 14, 0) * CFrame.Angles(0, 0.3, 0)
sign.Color = Color3.fromRGB(40, 40, 48)
sign.Parent = workspace

local surface = Instance.new("SurfaceGui")
surface.Face = Enum.NormalId.Back
surface.SizingMode = Enum.SurfaceGuiSizingMode.PixelsPerStud
surface.PixelsPerStud = 40
surface.LightInfluence = 0
surface.Parent = sign

local banner = Instance.new("TextLabel")
banner.Size = UDim2.new(1, -40, 0.5, -20)
banner.Position = UDim2.new(0, 20, 0, 20)
banner.BackgroundColor3 = Color3.fromRGB(230, 120, 30)
banner.TextColor3 = Color3.new(1, 1, 1)
banner.TextScaled = true
banner.Text = "SURFACE"
banner.Parent = surface

local stripe = Instance.new("Frame")
stripe.Size = UDim2.new(0.5, 0, 0.25, 0)
stripe.Position = UDim2.new(0.25, 0, 0.65, 0)
stripe.BackgroundColor3 = Color3.fromRGB(40, 160, 220)
stripe.Parent = surface

local screen = Instance.new("ScreenGui")
screen.Name = "Hud"
local frame = Instance.new("ViewportFrame")
frame.Size = UDim2.new(0, 220, 0, 160)
frame.Position = UDim2.new(1, -240, 0, 20)
frame.BackgroundColor3 = Color3.fromRGB(20, 30, 40)
frame.Parent = screen

local model = Instance.new("WorldModel")
model.Parent = frame
local cube = Instance.new("Part")
cube.Size = Vector3.new(4, 4, 4)
cube.CFrame = CFrame.Angles(0.4, 0.7, 0)
cube.Color = Color3.fromRGB(220, 60, 60)
cube.Parent = model
local ball = Instance.new("Part")
ball.Shape = Enum.PartType.Ball
ball.Size = Vector3.new(3, 3, 3)
ball.CFrame = CFrame.new(4, 0, 0)
ball.Color = Color3.fromRGB(80, 200, 90)
ball.Parent = model

local camera = Instance.new("Camera")
camera.CFrame = CFrame.lookAt(Vector3.new(2, 4, 12), Vector3.new(2, 0, 0))
camera.Parent = frame
frame.CurrentCamera = camera

screen.Parent = game:GetService("StarterGui")
"#;

fn project() -> tempfile::TempDir {
    let setup = CompatSetup::standard().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut w = World::new();
    create_place(
        &mut w,
        &setup.classes,
        &[
            "Workspace",
            "Players",
            "StarterGui",
            "StarterPlayer",
            "ServerScriptService",
            "ReplicatedStorage",
            "Lighting",
        ],
    )
    .unwrap();
    let mut scene: serde_json::Value =
        serde_json::from_str(&re_scene::scene::to_json(&w.save(SaveOptions::default()))).unwrap();
    let rel = "scripts/ServerScriptService/Build.server.luau";
    scene["entities"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "build",
            "name": "Build",
            "parent": "ServerScriptService",
            "components": {
                "ClassMarker": { "class": "Script" },
                "Compat_BaseScript": { "RunContext": "Legacy" },
                "Compat_Instance": { "Archivable": true },
                "ScriptRef": { "path": rel, "context": "Server" },
            },
        }));
    let p = dir.path();
    std::fs::create_dir_all(p.join("scenes")).unwrap();
    std::fs::create_dir_all(p.join("scripts/ServerScriptService")).unwrap();
    std::fs::write(p.join(rel), BUILD).unwrap();
    std::fs::write(
        p.join("scenes/main.scene.json"),
        serde_json::to_string_pretty(&scene).unwrap(),
    )
    .unwrap();
    std::fs::write(
        p.join("real.toml"),
        "[project]\nname = \"surfaces\"\nentry_scene = \"scenes/main.scene.json\"\n\n\
         [render]\nwidth = 480\nheight = 270\n\n[compat]\napi = \"roblox\"\n",
    )
    .unwrap();
    dir
}

/// The golden set of the backend, `None` (skipped) without an adapter or
/// off Linux, where goldens are not compared.
fn golden_set() -> Option<&'static str> {
    if !cfg!(target_os = "linux") {
        println!("skipped: goldens are compared on Linux");
        return None;
    }
    let backends = re_render::backends_from_env().expect("RE_BACKENDS");
    let gpu = match re_render::Gpu::headless(backends) {
        Ok(g) => g,
        Err(d) if std::env::var_os("CI").is_none() => {
            println!("skipped: no adapter: {}", d.render_human());
            return None;
        }
        Err(d) => panic!("{}", d.render_human()),
    };
    match gpu.backend() {
        re_render::wgpu::Backend::Vulkan => Some("vulkan"),
        re_render::wgpu::Backend::Gl => Some("gl"),
        other => {
            println!("no golden set for {other:?}");
            None
        }
    }
}

fn check_golden(set: &str, name: &str, image: &re_render::Image) {
    use re_render::imgdiff::{self, Tolerance};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = root.join("../../out/re_compat").join(set);
    image.save_png(&out.join(format!("{name}.png"))).unwrap();
    let golden = root
        .join("tests/fixtures/render/goldens")
        .join(set)
        .join(format!("{name}.png"));
    if std::env::var_os("RE_UPDATE_GOLDENS").is_some_and(|v| !v.is_empty()) {
        image.save_png(&golden).unwrap();
        println!("updated {}", golden.display());
        return;
    }
    let expected = re_render::Image::load_png(&golden).unwrap_or_else(|d| {
        panic!(
            "{}missing golden: run with RE_UPDATE_GOLDENS=1 and review the image",
            d.render_human()
        )
    });
    let report = imgdiff::compare(image, &expected, Tolerance::default()).unwrap();
    println!("{name} ({set}): {}", report.summary());
    if !report.matches() {
        let diff = imgdiff::diff_image(image, &expected, Tolerance::default()).unwrap();
        diff.save_png(&out.join(format!("{name}.diff.png")))
            .unwrap();
        panic!(
            "{name} does not match its {set} golden: {} (actual and diff in {})",
            report.summary(),
            out.display()
        );
    }
}

#[test]
fn golden_surface_and_viewport() {
    let Some(set) = golden_set() else {
        return;
    };
    let dir = project();
    let config = re_app::AppConfig::load(dir.path()).unwrap();
    let mut app = re_app::App::open(
        dir.path(),
        re_app::AppConfig {
            headless: true,
            persist_edits: false,
            ..config
        },
    )
    .unwrap_or_else(|d| panic!("{}", d.render_human()));
    let dt = app.step();
    for _ in 0..90 {
        app.frame(dt);
    }
    let image = app
        .screenshot(None)
        .unwrap_or_else(|d| panic!("{}", d.render_human()));
    let diagnostics = app.take_diagnostics();
    assert!(
        diagnostics.iter().all(|d| !d.is_error()),
        "{diagnostics:#?}"
    );
    check_golden(set, "surface_and_viewport", &image);
}
