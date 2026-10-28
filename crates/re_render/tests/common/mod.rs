//! Helpers shared by the GPU tests: device creation, test meshes and scenes,
//! golden-image checks, and the pixel checks ported from the render prototype.
#![allow(dead_code)] // each test binary uses a different subset

use std::path::PathBuf;
use std::time::Duration;

use glam::{Mat4, Vec3, Vec4Swizzles};
use re_render::imgdiff::{self, Tolerance};
use re_render::{Capture, Gpu, Image, Renderer};
use re_types::EntityId;
use re_types::mesh::{MeshData, Vertex};
use re_types::render::{Camera, DrawFlags, DrawItem, Light, LightKind, RenderSnapshot};

/// A device on the backends of `RE_BACKENDS` (Vulkan by default on Linux).
pub fn gpu() -> Gpu {
    let backends = re_render::backends_from_env().expect("RE_BACKENDS");
    let gpu = Gpu::headless(backends).unwrap_or_else(|d| panic!("{}", d.render_human()));
    println!("adapter: {}", gpu.adapter_line());
    gpu
}

/// The golden set for a backend: `vulkan` or `gl`, `None` for others.
pub fn golden_set(gpu: &Gpu) -> Option<&'static str> {
    match gpu.backend() {
        wgpu::Backend::Vulkan => Some("vulkan"),
        wgpu::Backend::Gl => Some("gl"),
        _ => None,
    }
}

/// `out/re_render/<set>/` at the workspace root (git-ignored).
pub fn out_dir(set: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../out/re_render")
        .join(set)
}

/// Compares `image` with `tests/goldens/<set>/<name>.png` at the M1 tolerance
/// (2 levels per channel, 0.1% outliers). The actual image is always written to
/// `out/re_render/<set>/`, and a diff image next to it on a mismatch.
/// `RE_UPDATE_GOLDENS=1` rewrites the golden instead.
pub fn check_golden(gpu: &Gpu, name: &str, image: &Image) {
    let Some(set) = golden_set(gpu) else {
        println!(
            "no golden set for {:?}; skipping the comparison of {name}",
            gpu.backend()
        );
        return;
    };
    let out = out_dir(set);
    image.save_png(&out.join(format!("{name}.png"))).unwrap();
    let golden_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
        let diff_path = out.join(format!("{name}.diff.png"));
        diff.save_png(&diff_path).unwrap();
        panic!(
            "{name} does not match its {set} golden: {} (actual and diff in {})",
            report.summary(),
            out.display()
        );
    }
}

/// Renders `snapshot` into a new `width` x `height` capture and reads it back.
pub fn render_image(
    gpu: &Gpu,
    renderer: &mut Renderer,
    snapshot: &RenderSnapshot,
    width: u32,
    height: u32,
) -> Image {
    let capture = Capture::new(&gpu.device, width, height).unwrap();
    let report = renderer
        .render(&gpu.device, &gpu.queue, capture.view(), snapshot)
        .unwrap_or_else(|d| panic!("{}", d.render_human()));
    assert!(
        report.warnings.is_empty(),
        "unexpected warnings: {:?}",
        report.warnings
    );
    capture.read(&gpu.device, &gpu.queue).unwrap()
}

pub fn vertex(pos: Vec3, normal: Vec3, uv: [f32; 2], color: [f32; 4]) -> Vertex {
    Vertex {
        pos: pos.to_array(),
        normal: normal.to_array(),
        uv,
        color,
    }
}

/// Appends a quad centred at `c` spanning `±u`, `±v`, facing `u × v`.
pub fn push_quad(mesh: &mut MeshData, c: Vec3, u: Vec3, v: Vec3, color: [f32; 4]) {
    let n = u.cross(v).normalize();
    let base = mesh.vertices.len() as u32;
    for (p, uv) in [
        (c - u - v, [0.0, 1.0]),
        (c + u - v, [1.0, 1.0]),
        (c + u + v, [1.0, 0.0]),
        (c - u + v, [0.0, 0.0]),
    ] {
        mesh.vertices.push(vertex(p, n, uv, color));
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// A unit cube centred on the origin, 24 vertices with flat normals.
pub fn cube() -> MeshData {
    let mut mesh = MeshData::default();
    let faces = [
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
    ];
    for (n, u, v) in faces {
        push_quad(&mut mesh, n * 0.5, u * 0.5, v * 0.5, [1.0; 4]);
    }
    mesh
}

/// A square plane at y = 0, `2 * half` wide, facing +Y, with uvs repeating `tiles` times.
pub fn plane(half: f32, tiles: f32) -> MeshData {
    let mut mesh = MeshData::default();
    push_quad(
        &mut mesh,
        Vec3::ZERO,
        Vec3::X * half,
        Vec3::NEG_Z * half,
        [1.0; 4],
    );
    for v in &mut mesh.vertices {
        v.uv = [v.uv[0] * tiles, v.uv[1] * tiles];
    }
    mesh
}

/// A UV sphere of radius 0.5.
pub fn sphere(rings: u32, segments: u32) -> MeshData {
    let mut mesh = MeshData::default();
    for r in 0..=rings {
        let theta = std::f32::consts::PI * r as f32 / rings as f32;
        for s in 0..=segments {
            let phi = std::f32::consts::TAU * s as f32 / segments as f32;
            let n = Vec3::new(
                libm::sinf(theta) * libm::cosf(phi),
                libm::cosf(theta),
                -libm::sinf(theta) * libm::sinf(phi),
            );
            mesh.vertices.push(vertex(
                n * 0.5,
                n,
                [s as f32 / segments as f32, r as f32 / rings as f32],
                [1.0; 4],
            ));
        }
    }
    let row = segments + 1;
    for r in 0..rings {
        for s in 0..segments {
            let (a, b) = (r * row + s, (r + 1) * row + s);
            mesh.indices
                .extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    mesh
}

/// A camera at `eye` looking at `target`.
pub fn camera(eye: Vec3, target: Vec3) -> Camera {
    Camera {
        world: glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y).inverse(),
        fov_y_radians: 45f32.to_radians(),
        near: 0.1,
        far: 100.0,
    }
}

pub fn sun(direction: Vec3, intensity: f32, shadows: bool) -> Light {
    Light {
        kind: LightKind::Directional,
        direction: direction.normalize(),
        color: [1.0, 0.96, 0.9],
        intensity,
        shadows,
        ..Light::default()
    }
}

pub fn point(position: Vec3, color: [f32; 3], lumens: f32, range: f32) -> Light {
    Light {
        kind: LightKind::Point,
        position,
        color,
        intensity: lumens,
        range,
        ..Light::default()
    }
}

pub fn item(
    entity: u64,
    mesh: re_types::render::MeshHandle,
    material: re_types::render::MaterialHandle,
    world: Mat4,
    flags: DrawFlags,
) -> DrawItem {
    DrawItem {
        entity: EntityId(entity),
        mesh,
        material,
        world,
        flags,
    }
}

pub const SHADOWED: DrawFlags = DrawFlags(DrawFlags::CAST_SHADOWS.0 | DrawFlags::RECEIVE_SHADOWS.0);

pub const CLEAR: [f32; 4] = [0.02, 0.03, 0.06, 1.0];

/// The clear colour as an sRGB target stores it.
pub fn clear_srgb8() -> [u8; 4] {
    [
        re_render::capture::linear_to_srgb_u8(CLEAR[0]),
        re_render::capture::linear_to_srgb_u8(CLEAR[1]),
        re_render::capture::linear_to_srgb_u8(CLEAR[2]),
        255,
    ]
}

/// Projects a world-space point to pixel coordinates (origin top left).
pub fn project_to_pixel(view_proj: Mat4, p: Vec3, width: u32, height: u32) -> Option<(u32, u32)> {
    let clip = view_proj * p.extend(1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.xyz() / clip.w;
    let x = (ndc.x * 0.5 + 0.5) * width as f32;
    let y = (1.0 - (ndc.y * 0.5 + 0.5)) * height as f32;
    (x >= 0.0 && y >= 0.0 && x < width as f32 && y < height as f32).then_some((x as u32, y as u32))
}

/// The view-projection matrix the renderer uses for `camera` at `aspect`
/// (reversed Z with no far plane).
pub fn view_proj(camera: &Camera, aspect: f32) -> Mat4 {
    glam::camera::rh::proj::directx::perspective_infinite_reverse(
        camera.fov_y_radians,
        aspect,
        camera.near,
    ) * camera.view()
}

/// An egui context fed headless input for `width` x `height` pixels.
pub fn ui_frame(
    ctx: &egui::Context,
    width: u32,
    height: u32,
    frame: u32,
    run: impl FnMut(&mut egui::Ui),
) -> re_render::UiFrame {
    let mut raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width as f32, height as f32),
        )),
        max_texture_side: Some(8192),
        // A fixed clock keeps the UI deterministic for golden images.
        time: Some(f64::from(frame) / 60.0),
        focused: true,
        ..Default::default()
    };
    raw.viewports
        .entry(raw.viewport_id)
        .or_default()
        .native_pixels_per_point = Some(1.0);
    let mut output = ctx.run_ui(raw, run);
    re_render::UiFrame::from_output(ctx, &mut output, [width, height])
}

/// A small editor-like panel used by the egui tests.
pub fn demo_panel(ui: &mut egui::Ui) {
    egui::Panel::left("outliner")
        .default_size(150.0)
        .show(ui, |ui| {
            ui.heading("Outliner");
            ui.separator();
            for (i, name) in ["Camera", "Island", "Crates", "Sun"].iter().enumerate() {
                let _ = ui.selectable_label(i == 1, *name);
            }
        });
    egui::Panel::bottom("console")
        .default_size(48.0)
        .show(ui, |ui| {
            ui.monospace("collected 3 / 7");
        });
    egui::CentralPanel::default().show(ui, |ui| {
        ui.label("Viewport");
        let _ = ui.button("Regenerate");
        ui.add(egui::ProgressBar::new(0.62).text("bake 62%"));
    });
}

pub const READBACK_TIMEOUT: Duration = Duration::from_secs(30);
