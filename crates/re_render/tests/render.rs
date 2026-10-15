//! Headless GPU tests: golden images, readback, resources and errors.
//!
//! Run with `cargo test -p re_render -- --test-threads=1`, once per backend:
//! `RE_BACKENDS=vulkan` (the default on Linux) and `RE_BACKENDS=gl`. Each backend
//! has its own golden set in `tests/goldens/<backend>/`; `RE_UPDATE_GOLDENS=1`
//! rewrites them. Actual images go to `out/re_render/<backend>/`.

mod common;

use common::*;
use glam::{Mat4, Quat, Vec3};
use re_render::{Capture, Gpu, Renderer, RendererSettings, codes};
use re_types::render::{
    AlphaMode, DebugLine, DrawFlags, MaterialDesc, RenderSnapshot, TextureData, TextureFormat,
    TextureRef,
};

fn material(
    renderer: &mut Renderer,
    color: [f32; 4],
    metallic: f32,
    roughness: f32,
) -> re_types::render::MaterialHandle {
    renderer
        .create_material(&MaterialDesc {
            base_color: color,
            metallic,
            roughness,
            ..MaterialDesc::default()
        })
        .unwrap()
}

/// A 5 x 5 grid of cubes with varied materials, a sun and two point lights.
#[test]
fn golden_lit_cube_grid() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let mut items = Vec::new();
    for z in 0..5 {
        for x in 0..5 {
            let hue = (x * 5 + z) as f32 / 25.0;
            let color = [
                0.2 + 0.7 * hue,
                0.35 + 0.3 * (1.0 - hue),
                0.8 - 0.6 * hue,
                1.0,
            ];
            let mat = material(
                &mut renderer,
                color,
                (x as f32) / 4.0,
                0.2 + 0.7 * (z as f32) / 4.0,
            );
            let world = Mat4::from_scale_rotation_translation(
                Vec3::splat(0.7),
                Quat::from_rotation_y(0.3 * (x + z) as f32),
                Vec3::new(x as f32 - 2.0, 0.35, z as f32 - 2.0),
            );
            items.push(item((z * 5 + x) as u64, cube, mat, world, DrawFlags::NONE));
        }
    }
    let snapshot = RenderSnapshot {
        camera: camera(Vec3::new(4.5, 5.0, 6.5), Vec3::new(0.0, 0.0, 0.0)),
        lights: vec![
            sun(Vec3::new(-0.4, -1.0, -0.3), 0.9, false),
            point(Vec3::new(-2.5, 1.5, 2.5), [1.0, 0.5, 0.2], 60.0, 8.0),
            point(Vec3::new(2.5, 1.0, -2.5), [0.2, 0.5, 1.0], 60.0, 8.0),
        ],
        items,
        debug_lines: Vec::new(),
        clear: CLEAR,
    };
    let image = render_image(&gpu, &mut renderer, &snapshot, 320, 240);
    let stats = image.stats(clear_srgb8());
    assert!(stats.distinct_colors > 500, "{stats:?}");
    assert_eq!(renderer.stats().last_frame.items_drawn, 25);
    check_golden(&gpu, "lit_cube_grid", &image);
}

fn shadow_scene(renderer: &mut Renderer, shadows: bool) -> RenderSnapshot {
    let plane = renderer.upload_mesh(&plane(4.0, 1.0)).unwrap();
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let ball = renderer.upload_mesh(&sphere(16, 24)).unwrap();
    let ground = material(renderer, [0.7, 0.72, 0.68, 1.0], 0.0, 0.9);
    let red = material(renderer, [0.8, 0.2, 0.15, 1.0], 0.0, 0.5);
    let gold = material(renderer, [1.0, 0.77, 0.34, 1.0], 1.0, 0.35);
    RenderSnapshot {
        camera: camera(Vec3::new(3.5, 4.5, 5.5), Vec3::new(0.0, 0.3, 0.0)),
        lights: vec![sun(Vec3::new(-0.6, -1.0, 0.3), 1.0, shadows)],
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
                ball,
                gold,
                Mat4::from_scale_rotation_translation(
                    Vec3::splat(1.2),
                    Quat::IDENTITY,
                    Vec3::new(0.9, 1.1, 0.6),
                ),
                SHADOWED,
            ),
        ],
        debug_lines: Vec::new(),
        clear: CLEAR,
    }
}

/// A plane with a cube and a sphere; the sun casts shadows on the plane.
#[test]
fn golden_shadowed_plane() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let lit = shadow_scene(&mut renderer, true);
    let image = render_image(&gpu, &mut renderer, &lit, 320, 240);
    assert!(renderer.stats().last_frame.shadow_casters >= 3);
    check_golden(&gpu, "shadowed_plane", &image);

    // Where the sphere's shadow falls on the plane (its centre, moved along the
    // sun's direction down to y = 0), the shadowless frame is much brighter.
    let unlit = shadow_scene(&mut renderer, false);
    let unshadowed = render_image(&gpu, &mut renderer, &unlit, 320, 240);
    let dir = Vec3::new(-0.6, -1.0, 0.3);
    let center = Vec3::new(0.9, 1.1, 0.6);
    let ground = center + dir * (center.y / -dir.y);
    let (x, y) = project_to_pixel(view_proj(&lit.camera, 320.0 / 240.0), ground, 320, 240).unwrap();
    let (with, without) = (
        re_render::capture::luma(image.pixel(x, y)),
        re_render::capture::luma(unshadowed.pixel(x, y)),
    );
    println!(
        "luma at the sphere's shadow ({x}, {y}): {with:.1} with shadows, {without:.1} without"
    );
    assert!(
        with + 40.0 < without,
        "no shadow under the sphere ({with} vs {without})"
    );
}

/// World axes and a wire box over a plane: depth-tested lines.
#[test]
fn golden_debug_lines() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let plane_mesh = renderer.upload_mesh(&plane(3.0, 1.0)).unwrap();
    let ground = material(&mut renderer, [0.35, 0.37, 0.4, 1.0], 0.0, 0.8);
    let mut lines = vec![
        DebugLine {
            a: Vec3::ZERO,
            b: Vec3::X * 2.0,
            color: [1.0, 0.1, 0.1, 1.0],
        },
        DebugLine {
            a: Vec3::ZERO,
            b: Vec3::Y * 2.0,
            color: [0.1, 1.0, 0.1, 1.0],
        },
        DebugLine {
            a: Vec3::ZERO,
            b: Vec3::Z * 2.0,
            color: [0.1, 0.3, 1.0, 1.0],
        },
        // Partly below the plane: the hidden half is depth-tested away.
        DebugLine {
            a: Vec3::new(-2.0, -0.5, 1.0),
            b: Vec3::new(2.0, 0.5, 1.0),
            color: [1.0, 1.0, 0.0, 1.0],
        },
    ];
    let (lo, hi) = (Vec3::new(-1.5, 0.0, -1.5), Vec3::new(-0.5, 1.0, -0.5));
    let corner = |i: u32| {
        Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        )
    };
    for (a, b) in [
        (0, 1),
        (2, 3),
        (4, 5),
        (6, 7),
        (0, 2),
        (1, 3),
        (4, 6),
        (5, 7),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ] {
        lines.push(DebugLine {
            a: corner(a),
            b: corner(b),
            color: [0.9, 0.9, 0.95, 1.0],
        });
    }
    let snapshot = RenderSnapshot {
        camera: camera(Vec3::new(3.0, 3.0, 4.0), Vec3::new(0.0, 0.3, 0.0)),
        lights: vec![sun(Vec3::new(-0.3, -1.0, -0.2), 0.9, false)],
        items: vec![item(1, plane_mesh, ground, Mat4::IDENTITY, DrawFlags::NONE)],
        debug_lines: lines,
        clear: CLEAR,
    };
    let image = render_image(&gpu, &mut renderer, &snapshot, 256, 192);
    assert_eq!(renderer.stats().last_frame.debug_lines, 16);
    // The +Y axis is drawn in green above the plane.
    let vp = view_proj(&snapshot.camera, 256.0 / 192.0);
    let (x, y) = project_to_pixel(vp, Vec3::Y * 1.5, 256, 192).unwrap();
    let px = (0..3)
        .flat_map(|dy| (0..3).map(move |dx| (dx, dy)))
        .map(|(dx, dy)| image.pixel(x + dx - 1, y + dy - 1))
        .max_by_key(|p| p[1])
        .unwrap();
    assert!(
        px[1] > 150 && px[0] < 120,
        "no green axis near ({x}, {y}): {px:?}"
    );
    check_golden(&gpu, "debug_lines", &image);
}

/// A checker-textured cube and plane, with a normal-mapped sphere.
#[test]
fn golden_textured_mesh() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let checker = TextureData {
        width: 64,
        height: 64,
        format: TextureFormat::Rgba8Srgb,
        pixels: (0..64 * 64)
            .flat_map(|i| {
                let (x, y) = (i % 64, i / 64);
                if (x / 8 + y / 8) % 2 == 0 {
                    [235, 230, 220, 255]
                } else {
                    [60, 90, 160, 255]
                }
            })
            .collect(),
    };
    // Bumps: a normal map of round dimples.
    let bumps = TextureData {
        width: 32,
        height: 32,
        format: TextureFormat::Rgba8Unorm,
        pixels: (0..32 * 32)
            .flat_map(|i| {
                let (x, y) = ((i % 32) as f32 / 32.0, (i / 32) as f32 / 32.0);
                let (dx, dy) = ((x * 4.0).fract() - 0.5, (y * 4.0).fract() - 0.5);
                let n = Vec3::new(dx * 1.5, -dy * 1.5, 1.0).normalize();
                let enc = |c: f32| ((c * 0.5 + 0.5) * 255.0).round() as u8;
                [enc(n.x), enc(n.y), enc(n.z), 255]
            })
            .collect(),
    };
    let checker = renderer.upload_texture(&checker).unwrap();
    let bumps = renderer.upload_texture(&bumps).unwrap();
    let textured = renderer
        .create_material(&MaterialDesc {
            base_color_texture: Some(TextureRef::Handle(checker)),
            roughness: 0.6,
            ..MaterialDesc::default()
        })
        .unwrap();
    let bumpy = renderer
        .create_material(&MaterialDesc {
            base_color: [0.3, 0.75, 0.45, 1.0],
            normal_texture: Some(TextureRef::Handle(bumps)),
            roughness: 0.4,
            ..MaterialDesc::default()
        })
        .unwrap();
    let cube_mesh = renderer.upload_mesh(&cube()).unwrap();
    let plane_mesh = renderer.upload_mesh(&plane(3.0, 3.0)).unwrap();
    let ball = renderer.upload_mesh(&sphere(24, 32)).unwrap();
    let snapshot = RenderSnapshot {
        camera: camera(Vec3::new(2.6, 2.2, 3.4), Vec3::new(0.0, 0.5, 0.0)),
        lights: vec![
            sun(Vec3::new(-0.45, -1.0, -0.65), 0.8, false),
            point(Vec3::new(-1.2, 1.4, 1.6), [1.0, 0.55, 0.25], 30.0, 6.0),
        ],
        items: vec![
            item(1, plane_mesh, textured, Mat4::IDENTITY, DrawFlags::NONE),
            item(
                2,
                cube_mesh,
                textured,
                Mat4::from_rotation_translation(
                    Quat::from_rotation_y(0.5),
                    Vec3::new(-0.3, 0.5, 0.0),
                ),
                DrawFlags::NONE,
            ),
            item(
                3,
                ball,
                bumpy,
                Mat4::from_scale_rotation_translation(
                    Vec3::splat(0.9),
                    Quat::IDENTITY,
                    Vec3::new(1.0, 0.45, 0.8),
                ),
                DrawFlags::NONE,
            ),
        ],
        debug_lines: Vec::new(),
        clear: CLEAR,
    };
    let image = render_image(&gpu, &mut renderer, &snapshot, 256, 192);
    assert!(image.stats(clear_srgb8()).distinct_colors > 300);
    check_golden(&gpu, "textured_mesh", &image);
}

/// An egui panel painted over a cleared frame, after one warm-up frame.
#[test]
fn golden_egui_panel() {
    let gpu = gpu();
    let image = egui_panel(&gpu);
    let stats = image.stats([0; 4]);
    assert!(
        stats.distinct_colors > 30,
        "the panel looks empty: {stats:?}"
    );
    check_golden(&gpu, "egui_panel", &image);
}

fn egui_panel(gpu: &Gpu) -> re_render::Image {
    let (w, h) = (320, 200);
    let mut renderer = Renderer::new(gpu, Capture::FORMAT);
    let capture = Capture::new(&gpu.device, w, h).unwrap();
    let ctx = egui::Context::default();
    // The first egui frame lays out wrongly (its bottom panel sits too high), so
    // frame 0 is painted and discarded.
    for frame in 0..2 {
        renderer
            .render(
                &gpu.device,
                &gpu.queue,
                capture.view(),
                &RenderSnapshot {
                    clear: CLEAR,
                    ..RenderSnapshot::default()
                },
            )
            .unwrap();
        let ui = ui_frame(&ctx, w, h, frame, demo_panel);
        renderer
            .paint_ui(&gpu.device, &gpu.queue, capture.view(), &ui)
            .unwrap_or_else(|d| panic!("{}", d.render_human()));
    }
    assert_eq!(gpu.errors().count(), 0, "{:?}", gpu.errors().take());
    capture.read(&gpu.device, &gpu.queue).unwrap()
}

/// The GL panel is not darker than the Vulkan one: egui is composited with a
/// manual gamma conversion, so both backends produce the same colours.
#[test]
fn egui_panel_luminance_matches_across_backends() {
    let (Ok(vulkan), Ok(gl)) = (
        Gpu::headless(wgpu::Backends::VULKAN),
        Gpu::headless(wgpu::Backends::GL),
    ) else {
        // Linux CI has lavapipe and llvmpipe; Windows and macOS runners have neither.
        assert!(
            std::env::var_os("CI").is_none() || !cfg!(target_os = "linux"),
            "CI must have both a Vulkan and a GL adapter"
        );
        println!("skipped: this machine lacks a Vulkan or a GL adapter");
        return;
    };
    let (a, b) = (egui_panel(&vulkan), egui_panel(&gl));
    let (la, lb) = (a.stats([0; 4]).luma_mean, b.stats([0; 4]).luma_mean);
    let report =
        re_render::imgdiff::compare(&a, &b, re_render::imgdiff::Tolerance::default()).unwrap();
    println!(
        "egui panel mean luma: vulkan {la:.3}, gl {lb:.3}; {}",
        report.summary()
    );
    // Measured on lavapipe and llvmpipe (Mesa 25.2.8): a difference of 0.000.
    // The tolerance is 0.5 levels of the 0-255 scale; the darkening this guards
    // against was tens of levels.
    assert!((la - lb).abs() <= 0.5, "vulkan {la} vs gl {lb}");
}

/// The prototype's lit-scene checks: the cube in front of the plane wins the depth
/// test, its visible faces are shaded differently, and the frame is not flat.
#[test]
fn lit_scene_is_depth_tested_and_shaded() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let cube_mesh = renderer.upload_mesh(&cube()).unwrap();
    let plane_mesh = renderer.upload_mesh(&plane(4.0, 1.0)).unwrap();
    let orange = material(&mut renderer, [0.95, 0.42, 0.12, 1.0], 0.0, 0.5);
    let grey = material(&mut renderer, [0.55, 0.6, 0.55, 1.0], 0.0, 0.8);
    let angle = 30f32.to_radians();
    let cube_world =
        Mat4::from_rotation_translation(Quat::from_rotation_y(angle), Vec3::new(0.0, 0.5, 0.0));
    let snapshot = RenderSnapshot {
        camera: camera(Vec3::new(2.6, 2.2, 3.4), Vec3::new(0.0, 0.5, 0.0)),
        lights: vec![sun(Vec3::new(-0.45, -1.0, -0.65), 0.9, false)],
        // The cube first, then the plane: without a working depth test the plane
        // would cover the cube.
        items: vec![
            item(1, cube_mesh, orange, cube_world, DrawFlags::NONE),
            item(2, plane_mesh, grey, Mat4::IDENTITY, DrawFlags::NONE),
        ],
        debug_lines: Vec::new(),
        clear: CLEAR,
    };
    let (w, h) = (150, 100);
    let image = render_image(&gpu, &mut renderer, &snapshot, w, h);
    let stats = image.stats(clear_srgb8());
    let [r, g, b, _] = stats.center;
    assert!(
        r > g && r > b,
        "centre {:?} is not the orange cube",
        stats.center
    );
    assert!(
        stats.pixels_differing_from_background > (w * h / 10) as usize,
        "{stats:?}"
    );
    assert!(stats.luma_stddev_foreground > 8.0, "{stats:?}");
    let vp = view_proj(&snapshot.camera, w as f32 / h as f32);
    let eye = snapshot.camera.position();
    let mut lumas = Vec::new();
    for n in [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::Z, Vec3::NEG_Z] {
        let nw = Quat::from_rotation_y(angle) * n;
        let face = Vec3::new(0.0, 0.5, 0.0) + nw * 0.5;
        if nw.dot(eye - face) <= 0.0 {
            continue;
        }
        let (x, y) = project_to_pixel(vp, face, w, h).unwrap();
        lumas.push(re_render::capture::luma(image.pixel(x, y)));
    }
    assert!(lumas.len() >= 2, "{lumas:?}");
    for (i, a) in lumas.iter().enumerate() {
        for b in &lumas[i + 1..] {
            assert!(
                (a - b).abs() >= 4.0,
                "faces have nearly equal luma: {lumas:?}"
            );
        }
    }
}

/// Readback strips the 256-byte row padding: widths that are not multiples of 64
/// pixels come back intact.
#[test]
fn readback_handles_unaligned_widths() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let cube_mesh = renderer.upload_mesh(&cube()).unwrap();
    let white = renderer
        .create_material(&MaterialDesc {
            unlit: true,
            ..MaterialDesc::default()
        })
        .unwrap();
    for (w, h) in [(150, 100), (1, 1), (63, 7), (257, 3)] {
        let snapshot = RenderSnapshot {
            clear: CLEAR,
            ..RenderSnapshot::default()
        };
        let image = render_image(&gpu, &mut renderer, &snapshot, w, h);
        assert_eq!(image.rgba.len(), (w * h * 4) as usize);
        assert!(
            image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == clear_srgb8()),
            "{w}x{h}: padding leaked into the pixels"
        );
    }
    // A white unlit cube filling the middle: the last column of each row is still
    // the clear colour, and the middle pixel of each middle row is white.
    let snapshot = RenderSnapshot {
        camera: camera(Vec3::new(0.0, 0.0, 2.2), Vec3::ZERO),
        items: vec![item(1, cube_mesh, white, Mat4::IDENTITY, DrawFlags::NONE)],
        clear: CLEAR,
        ..RenderSnapshot::default()
    };
    let (w, h) = (150, 100);
    let image = render_image(&gpu, &mut renderer, &snapshot, w, h);
    for y in 40..60 {
        assert_eq!(image.pixel(w / 2, y), [255; 4], "row {y}");
        assert_eq!(image.pixel(w - 1, y), clear_srgb8(), "row {y}");
    }
}

/// A gamma (Unorm) target gets the same bytes as an sRGB target: the shader
/// encodes by hand. (MSAA is off: with it, edges differ because the resolve
/// averages gamma values in one case and linear values in the other.)
#[test]
fn unorm_targets_are_gamma_encoded() {
    let gpu = gpu();
    let mut renderer = Renderer::with_settings(
        &gpu,
        Capture::FORMAT,
        RendererSettings {
            msaa: false,
            ..RendererSettings::default()
        },
    );
    let snapshot = shadow_scene(&mut renderer, true);
    let srgb = render_image(&gpu, &mut renderer, &snapshot, 160, 120);
    let unorm =
        Capture::with_format(&gpu.device, 160, 120, wgpu::TextureFormat::Rgba8Unorm).unwrap();
    renderer
        .render(&gpu.device, &gpu.queue, unorm.view(), &snapshot)
        .unwrap();
    let unorm = unorm.read(&gpu.device, &gpu.queue).unwrap();
    let report =
        re_render::imgdiff::compare(&srgb, &unorm, re_render::imgdiff::Tolerance::default())
            .unwrap();
    println!("sRGB vs Unorm target: {}", report.summary());
    assert!(report.matches(), "{}", report.summary());
}

/// One mesh uploaded once and drawn 1000 times: one draw call, one mesh.
#[test]
fn one_mesh_drawn_a_thousand_times() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let cube_mesh = renderer.upload_mesh(&cube()).unwrap();
    let mat = material(&mut renderer, [0.8, 0.6, 0.3, 1.0], 0.0, 0.5);
    let items = (0..1000)
        .map(|i| {
            let (x, z) = ((i % 40) as f32, (i / 40) as f32);
            let world = Mat4::from_scale_rotation_translation(
                Vec3::splat(0.6),
                Quat::from_rotation_y(i as f32 * 0.1),
                Vec3::new(x - 20.0, 0.3, -z),
            );
            item(i, cube_mesh, mat, world, DrawFlags::NONE)
        })
        .collect();
    let snapshot = RenderSnapshot {
        camera: camera(Vec3::new(0.0, 12.0, 14.0), Vec3::new(0.0, 0.0, -10.0)),
        lights: vec![sun(Vec3::new(-0.3, -1.0, -0.4), 0.9, false)],
        items,
        debug_lines: Vec::new(),
        clear: CLEAR,
    };
    let start = std::time::Instant::now();
    let image = render_image(&gpu, &mut renderer, &snapshot, 320, 240);
    println!(
        "1000 instances rendered and read back in {:?}",
        start.elapsed()
    );
    let stats = renderer.stats();
    assert_eq!(stats.meshes.count, 1);
    assert_eq!(stats.last_frame.items_drawn, 1000);
    assert_eq!(
        stats.last_frame.draw_calls, 1,
        "the instances are batched into one draw"
    );
    assert_eq!(stats.last_frame.triangles, 12_000);
    assert!(image.stats(clear_srgb8()).pixels_differing_from_background > 320 * 240 / 4);
}

/// Uploading and releasing 100 meshes returns the stats to where they started; a
/// released handle is rejected and skipped.
#[test]
fn released_resources_leave_no_trace() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let before = renderer.stats();
    let handles: Vec<_> = (0..100)
        .map(|_| renderer.upload_mesh(&sphere(8, 12)).unwrap())
        .collect();
    let during = renderer.stats();
    assert_eq!(during.meshes.count, before.meshes.count + 100);
    assert!(during.meshes.bytes > before.meshes.bytes);
    for h in &handles {
        renderer.release_mesh(*h).unwrap();
    }
    let after = renderer.stats();
    assert_eq!(
        (after.meshes, after.textures, after.materials),
        (before.meshes, before.textures, before.materials)
    );

    let stale = handles[7];
    let err = renderer.release_mesh(stale).unwrap_err();
    assert_eq!(err.code, codes::STALE_MESH);
    // The slot is reused with a new generation; the old handle still does not match.
    let reused = renderer.upload_mesh(&cube()).unwrap();
    assert!(handles.iter().any(|h| h.index == reused.index));
    assert!(!handles.contains(&reused));
    assert!(renderer.mesh_bounds(reused).is_some());
    let mat = material(&mut renderer, [1.0; 4], 0.0, 0.5);
    let snapshot = RenderSnapshot {
        camera: camera(Vec3::new(0.0, 1.0, 3.0), Vec3::ZERO),
        items: vec![
            item(1, stale, mat, Mat4::IDENTITY, DrawFlags::NONE),
            item(2, stale, mat, Mat4::IDENTITY, DrawFlags::NONE),
            item(3, reused, mat, Mat4::IDENTITY, DrawFlags::NONE),
        ],
        ..RenderSnapshot::default()
    };
    let capture = Capture::new(&gpu.device, 64, 64).unwrap();
    let report = renderer
        .render(&gpu.device, &gpu.queue, capture.view(), &snapshot)
        .unwrap();
    assert_eq!(
        (report.stats.items_drawn, report.stats.items_skipped),
        (1, 2)
    );
    assert_eq!(
        report.warnings.len(),
        1,
        "one warning per stale handle: {:?}",
        report.warnings
    );
    assert_eq!(report.warnings[0].code, codes::STALE_MESH);

    // Textures and materials behave the same way.
    let tex = renderer
        .upload_texture(&TextureData {
            width: 2,
            height: 2,
            format: TextureFormat::Rgba8Srgb,
            pixels: vec![255; 16],
        })
        .unwrap();
    assert_eq!(renderer.stats().textures.count, 1);
    assert_eq!(renderer.stats().textures.bytes, 16 + 4, "two mip levels");
    renderer.release_texture(tex).unwrap();
    let err = renderer
        .create_material(&MaterialDesc {
            base_color_texture: Some(TextureRef::Handle(tex)),
            ..MaterialDesc::default()
        })
        .unwrap_err();
    assert_eq!(err.code, codes::STALE_TEXTURE);
    assert_eq!(
        renderer.release_texture(tex).unwrap_err().code,
        codes::STALE_TEXTURE
    );
    renderer.release_material(mat).unwrap();
    assert_eq!(
        renderer.release_material(mat).unwrap_err().code,
        codes::STALE_MATERIAL
    );
    assert_eq!(gpu.errors().count(), 0, "{:?}", gpu.errors().take());
}

/// Bad input is a diagnostic, never a GPU error or a panic.
#[test]
fn bad_input_is_rejected_with_diagnostics() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let mut broken = cube();
    broken.indices.push(0);
    assert_eq!(
        renderer.upload_mesh(&broken).unwrap_err().code,
        codes::BAD_MESH
    );
    broken.indices.extend([1, 999]);
    assert_eq!(
        renderer.upload_mesh(&broken).unwrap_err().code,
        codes::BAD_MESH
    );
    let mut nan = cube();
    nan.vertices[3].pos[1] = f32::NAN;
    assert_eq!(
        renderer.upload_mesh(&nan).unwrap_err().code,
        codes::BAD_MESH
    );
    let empty = renderer
        .upload_mesh(&re_types::mesh::MeshData::default())
        .unwrap();
    assert!(renderer.mesh_bounds(empty).is_none());

    let short = TextureData {
        width: 4,
        height: 4,
        format: TextureFormat::Rgba8Srgb,
        pixels: vec![0; 12],
    };
    assert_eq!(
        renderer.upload_texture(&short).unwrap_err().code,
        codes::BAD_TEXTURE
    );
    let zero = TextureData {
        width: 0,
        height: 4,
        format: TextureFormat::Rgba8Srgb,
        pixels: Vec::new(),
    };
    assert_eq!(
        renderer.upload_texture(&zero).unwrap_err().code,
        codes::BAD_TEXTURE
    );

    let asset = MaterialDesc {
        base_color_texture: Some(TextureRef::Asset("textures/rock.png".into())),
        ..MaterialDesc::default()
    };
    assert_eq!(
        renderer.create_material(&asset).unwrap_err().code,
        codes::UNRESOLVED_TEXTURE
    );
    let not_finite = MaterialDesc {
        roughness: f32::NAN,
        ..MaterialDesc::default()
    };
    assert_eq!(
        renderer.create_material(&not_finite).unwrap_err().code,
        codes::BAD_MATERIAL
    );
    let masked = MaterialDesc {
        alpha_mode: AlphaMode::Mask { cutoff: 0.5 },
        double_sided: true,
        ..MaterialDesc::default()
    };
    let masked = renderer.create_material(&masked).unwrap();

    let capture = Capture::new(&gpu.device, 32, 32).unwrap();
    let mut snapshot = RenderSnapshot {
        items: vec![item(1, empty, masked, Mat4::IDENTITY, DrawFlags::NONE)],
        ..RenderSnapshot::default()
    };
    renderer
        .render(&gpu.device, &gpu.queue, capture.view(), &snapshot)
        .unwrap();
    snapshot.camera.near = 0.0;
    let err = renderer
        .render(&gpu.device, &gpu.queue, capture.view(), &snapshot)
        .unwrap_err();
    assert_eq!(err.code, codes::INVALID_CAMERA);

    assert_eq!(
        Capture::new(&gpu.device, 0, 8).unwrap_err().code,
        codes::INVALID_TARGET
    );
    assert_eq!(
        Capture::with_format(&gpu.device, 8, 8, wgpu::TextureFormat::R8Unorm)
            .unwrap_err()
            .code,
        codes::INVALID_TARGET
    );
    let ctx = egui::Context::default();
    let ui = ui_frame(&ctx, 64, 64, 0, demo_panel);
    let err = renderer
        .paint_ui(&gpu.device, &gpu.queue, capture.view(), &ui)
        .unwrap_err();
    assert_eq!(err.code, codes::INVALID_TARGET);

    let lights = (0..10)
        .map(|i| point(Vec3::new(i as f32, 1.0, 0.0), [1.0; 3], 100.0, 5.0))
        .collect();
    snapshot.camera = RenderSnapshot::default().camera;
    snapshot.lights = lights;
    let report = renderer
        .render(&gpu.device, &gpu.queue, capture.view(), &snapshot)
        .unwrap();
    assert_eq!(report.stats.point_lights, 8);
    assert_eq!(report.warnings[0].code, codes::TOO_MANY_LIGHTS);
    assert_eq!(gpu.errors().count(), 0, "{:?}", gpu.errors().take());
}

/// An injected validation error is logged as RE6001, and the process goes on.
#[test]
fn validation_errors_are_logged_not_fatal() {
    let gpu = gpu();
    // MAP_READ may only be combined with COPY_DST.
    let _bad = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("invalid-usage"),
        size: 256,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(gpu.errors().count(), 1);
    let errors = gpu.errors().take();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, codes::GPU_ERROR);
    assert!(
        errors[0].message.contains("invalid-usage"),
        "{}",
        errors[0].message
    );
    println!("{}", errors[0].render_human());
    // The device still works.
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let image = render_image(
        &gpu,
        &mut renderer,
        &RenderSnapshot {
            clear: CLEAR,
            ..RenderSnapshot::default()
        },
        8,
        8,
    );
    assert_eq!(image.pixel(4, 4), clear_srgb8());
}

/// Settings take effect: MSAA off renders single-sampled, exposure scales light.
#[test]
fn settings_apply_to_the_next_frame() {
    let gpu = gpu();
    let mut renderer = Renderer::with_settings(
        &gpu,
        Capture::FORMAT,
        RendererSettings {
            msaa: false,
            ..RendererSettings::default()
        },
    );
    let snapshot = shadow_scene(&mut renderer, true);
    let dim = render_image(&gpu, &mut renderer, &snapshot, 96, 64);
    assert_eq!(renderer.stats().last_frame.samples, 1);
    renderer.settings_mut().exposure = 2.0;
    renderer.settings_mut().shadow_map_size = 512;
    let bright = render_image(&gpu, &mut renderer, &snapshot, 96, 64);
    assert!(bright.stats([0; 4]).luma_mean > dim.stats([0; 4]).luma_mean + 5.0);
}

/// A backend set with no adapter is an `RE6002` diagnostic, not a panic.
#[cfg(target_os = "linux")]
#[test]
fn missing_adapters_are_diagnostics() {
    // Metal is not compiled on Linux, so no adapter can match.
    let err = Gpu::headless(wgpu::Backends::METAL).unwrap_err();
    assert_eq!(err.code, codes::NO_ADAPTER);
    println!("{}", err.render_human());
}
