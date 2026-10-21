//! Cascaded sun shadows (ADR-0038): goldens, stability under camera motion,
//! casters outside the view, and the cascade settings.
//!
//! Run with `cargo test -p re_render -- --test-threads=1`, once per backend (see
//! render.rs).

mod common;

use common::*;
use glam::{Mat4, Vec3};
use re_render::{Capture, Renderer, RendererSettings, ShadowProbe};
use re_types::render::{DrawFlags, MaterialDesc, MaterialHandle, RenderSnapshot, SunShadows};

const SUN: Vec3 = Vec3::new(-0.5, -1.0, -0.35);

fn material(renderer: &mut Renderer, color: [f32; 4]) -> MaterialHandle {
    renderer
        .create_material(&MaterialDesc {
            base_color: color,
            roughness: 0.9,
            ..MaterialDesc::default()
        })
        .unwrap()
}

fn renderer(gpu: &re_render::Gpu, map_size: u32) -> Renderer {
    Renderer::with_settings(
        gpu,
        Capture::FORMAT,
        RendererSettings {
            shadow_map_size: map_size,
            ..RendererSettings::default()
        },
    )
}

/// A 160 x 160 ground with two rows of pillars running away from the origin
/// along -Z, from 2 to 120 units out, so every cascade has casters.
fn pillar_scene(renderer: &mut Renderer) -> Vec<re_types::render::DrawItem> {
    let ground = renderer.upload_mesh(&plane(80.0, 16.0)).unwrap();
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let grey = material(renderer, [0.75, 0.75, 0.72, 1.0]);
    let stone = material(renderer, [0.55, 0.4, 0.3, 1.0]);
    let mut items = vec![item(1, ground, grey, Mat4::IDENTITY, SHADOWED)];
    for i in 0..24 {
        let z = -2.0 - 5.0 * i as f32;
        for (k, x) in [-3.0, 3.0].into_iter().enumerate() {
            let height = 1.5 + 0.25 * (i % 4) as f32;
            items.push(item(
                10 + 2 * i as u64 + k as u64,
                cube,
                stone,
                Mat4::from_translation(Vec3::new(x, 0.5 * height, z))
                    * Mat4::from_scale(Vec3::new(0.6, height, 0.6)),
                SHADOWED,
            ));
        }
    }
    items
}

fn snapshot(items: Vec<re_types::render::DrawItem>, eye: Vec3, target: Vec3) -> RenderSnapshot {
    RenderSnapshot {
        camera: camera(eye, target),
        lights: vec![sun(SUN, 1.0, true)],
        items,
        clear: CLEAR,
        ..RenderSnapshot::default()
    }
}

/// Pillars receding to the horizon: shadows near the camera come from the
/// sharp first cascade, far ones from the coarser ones, and none fall past the
/// shadow distance.
#[test]
fn golden_cascaded_shadows() {
    let gpu = gpu();
    let mut renderer = renderer(&gpu, 1024);
    let items = pillar_scene(&mut renderer);
    let mut scene = snapshot(items, Vec3::new(0.0, 2.5, 4.0), Vec3::new(0.0, 0.5, -30.0));
    scene.camera.far = 400.0;
    scene.shadows.distance = 100.0;
    let image = render_image(&gpu, &mut renderer, &scene, 320, 240);
    let stats = renderer.stats().last_frame;
    assert_eq!(stats.shadow_cascades, 4, "{stats:?}");
    check_golden(&gpu, "cascaded_shadows", &image);

    // Probe the ground beside each pillar's shadow.
    let shadow_at = |pillar: Vec3, height: f32| {
        // Half way up the pillar, moved along the sun's direction to the ground.
        let mid = pillar + Vec3::Y * (0.5 * height);
        mid + SUN * (mid.y / -SUN.y)
    };
    let near = shadow_at(Vec3::new(3.0, 0.0, -2.0), 1.5);
    let mid = shadow_at(Vec3::new(3.0, 0.0, -42.0), 1.5);
    let beyond = shadow_at(Vec3::new(3.0, 0.0, -117.0), 2.25);
    let probes = renderer
        .probe_sun_shadow(
            &gpu.device,
            &gpu.queue,
            &[near, mid, beyond, Vec3::new(0.0, 0.0, -10.0)],
            Vec3::Y,
        )
        .unwrap();
    println!("{probes:?}");
    assert!(
        probes[0].visibility < 0.1 && probes[0].cascade == Some(0),
        "{:?}",
        probes[0]
    );
    assert!(
        probes[1].visibility < 0.1 && probes[1].cascade >= Some(2),
        "{:?}",
        probes[1]
    );
    assert_eq!(
        probes[2],
        ShadowProbe {
            visibility: 1.0,
            cascade: None,
            blend: 0.0
        }
    );
    assert!(
        probes[3].visibility > 0.99,
        "open ground is lit: {:?}",
        probes[3]
    );
}

/// The sun's visibility at fixed points on the ground stays the same while the
/// camera moves and turns, as long as each point stays in one cascade.
#[test]
fn shadows_do_not_shimmer_as_the_camera_moves() {
    let gpu = gpu();
    let mut renderer = renderer(&gpu, 1024);
    let items = pillar_scene(&mut renderer);
    // A grid of points along the near pillars, where shadow edges are.
    let points: Vec<Vec3> = (0..40)
        .flat_map(|i| (0..24).map(move |j| Vec3::new(-6.0 + 0.3 * j as f32, 0.0, -0.37 * i as f32)))
        .collect();
    let mut history: Vec<Vec<ShadowProbe>> = Vec::new();
    for step in 0..12 {
        let t = step as f32;
        // Odd steps of the path: not whole texels of any cascade.
        let eye = Vec3::new(0.137 * t, 2.5 + 0.031 * t, 6.0 - 0.293 * t);
        let target = eye + Vec3::new(0.05 * t - 0.3, -0.35, -1.0);
        let scene = snapshot(items.clone(), eye, target);
        let capture = Capture::new(&gpu.device, 64, 48).unwrap();
        renderer
            .render(&gpu.device, &gpu.queue, capture.view(), &scene)
            .unwrap();
        history.push(
            renderer
                .probe_sun_shadow(&gpu.device, &gpu.queue, &points, Vec3::Y)
                .unwrap(),
        );
    }
    let (mut compared, mut shadowed, mut edges, mut worst) = (0, 0, 0, 0.0f32);
    for (p, point) in points.iter().enumerate() {
        for pair in history.windows(2) {
            let (a, b) = (pair[0][p], pair[1][p]);
            if a.cascade.is_none() || a.cascade != b.cascade || a.blend > 0.0 || b.blend > 0.0 {
                continue;
            }
            compared += 1;
            shadowed += usize::from(a.visibility < 0.5);
            edges += usize::from(a.visibility > 0.05 && a.visibility < 0.95);
            // Float rounding in the matrices moves a sample by far less than a
            // texel; a shimmering shadow moves by a fraction of one.
            worst = worst.max((a.visibility - b.visibility).abs());
            assert!(
                (a.visibility - b.visibility).abs() <= 1e-3,
                "the shadow at {point} changed from {} to {} while it stayed in cascade {:?}",
                a.visibility,
                b.visibility,
                a.cascade
            );
        }
    }
    println!(
        "{compared} comparisons, {shadowed} shadowed, {edges} on edges, largest change {worst}"
    );
    assert!(
        compared > 2000 && shadowed > 100 && edges > 20,
        "the test sees too little"
    );
}

/// A wall behind the camera, outside the view, still shadows the ground in
/// front of it: casters are culled per cascade, not by the view.
#[test]
fn casters_outside_the_view_still_cast() {
    let gpu = gpu();
    let mut renderer = renderer(&gpu, 1024);
    let ground = renderer.upload_mesh(&plane(40.0, 8.0)).unwrap();
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let grey = material(&mut renderer, [0.75, 0.75, 0.72, 1.0]);
    let wall = Mat4::from_translation(Vec3::new(0.0, 4.0, 6.0))
        * Mat4::from_scale(Vec3::new(30.0, 8.0, 0.5));
    let mut scene = snapshot(
        vec![
            item(1, ground, grey, Mat4::IDENTITY, SHADOWED),
            item(2, cube, grey, wall, DrawFlags::CAST_SHADOWS),
        ],
        Vec3::new(0.0, 2.0, 4.0),
        Vec3::new(0.0, 0.0, -6.0),
    );
    // A low sun from behind the wall.
    scene.lights = vec![sun(Vec3::new(0.0, -0.5, -1.0), 1.0, true)];
    render_image(&gpu, &mut renderer, &scene, 160, 120);
    let stats = renderer.stats().last_frame;
    assert_eq!((stats.items_drawn, stats.items_culled), (1, 1), "{stats:?}");
    assert!(stats.shadow_casters >= 1, "{stats:?}");
    let probes = renderer
        .probe_sun_shadow(
            &gpu.device,
            &gpu.queue,
            &[Vec3::new(0.0, 0.0, -2.0), Vec3::new(0.0, 0.0, -20.0)],
            Vec3::Y,
        )
        .unwrap();
    assert!(
        probes[0].visibility < 0.1,
        "in the wall's shadow: {:?}",
        probes[0]
    );
    assert!(probes[1].visibility > 0.99, "past it: {:?}", probes[1]);
}

/// The snapshot's cascade count and distance apply; a sun without shadows
/// draws none.
#[test]
fn cascade_settings_apply() {
    let gpu = gpu();
    let mut renderer = renderer(&gpu, 512);
    let items = pillar_scene(&mut renderer);
    let mut scene = snapshot(items, Vec3::new(0.0, 2.5, 4.0), Vec3::new(0.0, 0.5, -30.0));
    let far = Vec3::new(0.0, 0.0, -60.0);
    for (cascades, distance, want) in [(1, 150.0, Some(0)), (2, 150.0, Some(1)), (4, 40.0, None)] {
        scene.shadows = SunShadows {
            cascades,
            distance,
            ..SunShadows::default()
        };
        render_image(&gpu, &mut renderer, &scene, 64, 48);
        assert_eq!(
            renderer.stats().last_frame.shadow_cascades,
            u32::from(cascades)
        );
        let probe = renderer
            .probe_sun_shadow(&gpu.device, &gpu.queue, &[far], Vec3::Y)
            .unwrap()[0];
        assert_eq!(
            probe.cascade, want,
            "{cascades} cascades to {distance}: {probe:?}"
        );
    }
    scene.lights = vec![sun(SUN, 1.0, false)];
    render_image(&gpu, &mut renderer, &scene, 64, 48);
    let stats = renderer.stats().last_frame;
    assert_eq!((stats.shadow_cascades, stats.shadow_casters), (0, 0));
    let probe = renderer
        .probe_sun_shadow(&gpu.device, &gpu.queue, &[far], Vec3::Y)
        .unwrap()[0];
    assert_eq!(
        probe,
        ShadowProbe {
            visibility: 1.0,
            cascade: None,
            blend: 0.0
        }
    );
}
