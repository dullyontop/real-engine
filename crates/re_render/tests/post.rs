//! The post chain and world quads (ADR-0038): tone mapping, bloom, sun rays,
//! blur, and quads that show a capture in the world.

mod common;

use common::*;
use glam::{Mat4, Quat, Vec2, Vec3};
use re_render::{Capture, Gpu, Image, Renderer, SceneExtras, Sky};
use re_types::EntityId;
use re_types::render::{
    Bloom, DrawFlags, MaterialDesc, MaterialHandle, MeshHandle, QuadDepth, RenderSnapshot, SunRays,
    ToneMapping, WorldQuad,
};

const W: u32 = 160;
const H: u32 = 120;

fn draw(
    gpu: &Gpu,
    renderer: &mut Renderer,
    snapshot: &RenderSnapshot,
    extras: &SceneExtras,
) -> (Image, re_render::FrameReport) {
    let capture = Capture::new(&gpu.device, W, H).unwrap();
    let report = renderer
        .render_with(&gpu.device, &gpu.queue, capture.view(), snapshot, extras)
        .unwrap_or_else(|d| panic!("{}", d.render_human()));
    (capture.read(&gpu.device, &gpu.queue).unwrap(), report)
}

/// Images side by side, for one golden of several settings.
fn side_by_side(images: &[Image]) -> Image {
    let height = images[0].height;
    let width: u32 = images.iter().map(|i| i.width).sum();
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let mut x0 = 0;
    for image in images {
        for y in 0..height {
            let src = (y * image.width * 4) as usize;
            let dst = ((y * width + x0) * 4) as usize;
            rgba[dst..dst + (image.width * 4) as usize]
                .copy_from_slice(&image.rgba[src..src + (image.width * 4) as usize]);
        }
        x0 += image.width;
    }
    Image {
        width,
        height,
        rgba,
    }
}

fn material(renderer: &mut Renderer, color: [f32; 4], emissive: f32) -> MaterialHandle {
    renderer
        .create_material(&MaterialDesc {
            base_color: color,
            emissive: [color[0], color[1], color[2]],
            emissive_strength: emissive,
            roughness: 0.6,
            ..MaterialDesc::default()
        })
        .unwrap()
}

struct Scene {
    snapshot: RenderSnapshot,
    cube: MeshHandle,
}

/// A ground plane, a matte cube lit by the sun and a glowing cube (values
/// above 1), against the clear colour.
fn scene(renderer: &mut Renderer) -> Scene {
    let plane = renderer.upload_mesh(&plane(8.0, 1.0)).unwrap();
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let ground = material(renderer, [0.4, 0.42, 0.4, 1.0], 0.0);
    let orange = material(renderer, [0.9, 0.45, 0.15, 1.0], 0.0);
    let glow = renderer
        .create_material(&MaterialDesc {
            base_color: [0.05, 0.05, 0.05, 1.0],
            emissive: [0.3, 0.6, 1.0],
            emissive_strength: 2.5,
            ..MaterialDesc::default()
        })
        .unwrap();
    Scene {
        snapshot: RenderSnapshot {
            camera: camera(Vec3::new(0.0, 2.2, 5.5), Vec3::new(0.0, 0.6, 0.0)),
            lights: vec![sun(Vec3::new(-0.4, -1.0, -0.6), 1.2, false)],
            items: vec![
                item(1, plane, ground, Mat4::IDENTITY, DrawFlags::NONE),
                item(
                    2,
                    cube,
                    orange,
                    Mat4::from_rotation_translation(
                        Quat::from_rotation_y(0.5),
                        Vec3::new(-0.9, 0.5, 0.0),
                    ),
                    DrawFlags::NONE,
                ),
                item(
                    3,
                    cube,
                    glow,
                    Mat4::from_scale_rotation_translation(
                        Vec3::splat(0.5),
                        Quat::IDENTITY,
                        Vec3::new(1.1, 0.6, 0.3),
                    ),
                    DrawFlags::NONE,
                ),
            ],
            clear: CLEAR,
            ..RenderSnapshot::default()
        },
        cube,
    }
}

/// The default settings run one pass, the final one; each step adds its own.
#[test]
fn off_steps_record_no_pass() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let s = scene(&mut renderer);
    let (_, report) = draw(&gpu, &mut renderer, &s.snapshot, &SceneExtras::default());
    assert_eq!(report.stats.post_passes, 1);

    let mut bloom = s.snapshot.clone();
    bloom.post.bloom = Bloom {
        intensity: 1.0,
        radius: 0.05,
        ..Bloom::default()
    };
    // 5% of 120 px is 6 px: three levels, down and up (two up passes).
    let (_, report) = draw(&gpu, &mut renderer, &bloom, &SceneExtras::default());
    assert_eq!(report.stats.post_passes, 1 + 3 + 2);

    let mut blur = s.snapshot.clone();
    blur.post.blur_radius = 0.02;
    let (_, report) = draw(&gpu, &mut renderer, &blur, &SceneExtras::default());
    assert_eq!(report.stats.post_passes, 1 + 2);

    // Sun rays need the sun in front of the camera.
    let mut rays = s.snapshot.clone();
    rays.post.sun_rays.intensity = 1.0;
    // The light travels towards the camera: the sun is ahead, over the scene.
    rays.lights[0].direction = Vec3::new(0.0, -0.3, 1.0).normalize();
    let (_, report) = draw(&gpu, &mut renderer, &rays, &SceneExtras::default());
    assert_eq!(report.stats.post_passes, 2);
    rays.lights[0].direction = Vec3::new(0.0, -0.3, -1.0).normalize();
    let (_, report) = draw(&gpu, &mut renderer, &rays, &SceneExtras::default());
    assert_eq!(report.stats.post_passes, 1);
}

/// The four operators on the same scene: none (clip), PBR Neutral, ACES, AgX.
#[test]
fn golden_tone_mapping() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let s = scene(&mut renderer);
    let mut images = Vec::new();
    for op in [
        ToneMapping::None,
        ToneMapping::PbrNeutral,
        ToneMapping::Aces,
        ToneMapping::AgX,
    ] {
        let mut snapshot = s.snapshot.clone();
        snapshot.post.tone_mapping = op;
        let (image, report) = draw(&gpu, &mut renderer, &snapshot, &SceneExtras::default());
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        images.push(image);
    }
    // The glowing cube clips to white with no tone mapping and keeps its hue
    // through PBR Neutral.
    let glow = Vec3::new(1.1, 0.6, 0.55);
    let vp = view_proj(&s.snapshot.camera, W as f32 / H as f32);
    let (x, y) = project_to_pixel(vp, glow, W, H).unwrap();
    let clipped = images[0].pixel(x, y);
    let neutral = images[1].pixel(x, y);
    assert!(clipped[1] == 255 && clipped[2] == 255, "{clipped:?}");
    assert!(neutral[2] > neutral[0] + 10, "{neutral:?}");
    // Colours below about 0.76 pass through PBR Neutral nearly unchanged: the
    // ground in shadow-free light is darker than that.
    let ground = Vec3::new(0.0, 0.0, 2.5);
    let (gx, gy) = project_to_pixel(vp, ground, W, H).unwrap();
    let (a, b) = (images[0].pixel(gx, gy), images[1].pixel(gx, gy));
    assert!(
        a.iter().zip(&b).take(3).all(|(p, q)| p.abs_diff(*q) <= 12),
        "{a:?} {b:?}"
    );
    check_golden(&gpu, "post_tone_mapping", &side_by_side(&images));
}

/// Bloom spreads the glowing cube's light around it; blur softens the image.
#[test]
fn golden_bloom_and_blur() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let s = scene(&mut renderer);
    let mut neutral = s.snapshot.clone();
    neutral.post.tone_mapping = ToneMapping::PbrNeutral;
    let plain = draw(&gpu, &mut renderer, &neutral, &SceneExtras::default()).0;
    let mut bloom = neutral.clone();
    bloom.post.bloom = Bloom {
        intensity: 1.0,
        threshold: 1.0,
        knee: 0.5,
        radius: 0.08,
    };
    let (bloomed, report) = draw(&gpu, &mut renderer, &bloom, &SceneExtras::default());
    assert!(report.stats.post_passes > 3);
    // Next to the glowing cube, against the clear colour, it is brighter.
    let vp = view_proj(&s.snapshot.camera, W as f32 / H as f32);
    let (x, y) = project_to_pixel(vp, Vec3::new(1.1, 1.05, 0.3), W, H).unwrap();
    let (a, b) = (plain.pixel(x, y - 3), bloomed.pixel(x, y - 3));
    assert!(u16::from(b[2]) > u16::from(a[2]) + 10, "{a:?} {b:?}");
    let mut blur = neutral.clone();
    blur.post.blur_radius = 0.03;
    let blurred = draw(&gpu, &mut renderer, &blur, &SceneExtras::default()).0;
    let background = clear_srgb8();
    assert!(
        blurred.stats(background).distinct_colors > plain.stats(background).distinct_colors,
        "a blur makes gradients"
    );
    let mut both = bloom.clone();
    both.post.blur_radius = 0.03;
    let both = draw(&gpu, &mut renderer, &both, &SceneExtras::default()).0;
    check_golden(
        &gpu,
        "post_bloom_blur",
        &side_by_side(&[plain, bloomed, blurred, both]),
    );
}

/// Sun rays stream from the sky's sun past a pillar that hides part of it.
#[test]
fn golden_sun_rays() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let s = scene(&mut renderer);
    let mut snapshot = s.snapshot.clone();
    // Looking up at the sun, low over the horizon, behind the cube.
    let to_sun = Vec3::new(0.0, 0.35, -1.0).normalize();
    snapshot.camera = camera(Vec3::new(0.0, 0.6, 4.0), Vec3::new(0.0, 1.2, -1.0));
    snapshot.lights = vec![sun(-to_sun, 1.0, false)];
    snapshot.post.tone_mapping = ToneMapping::PbrNeutral;
    snapshot.items.push(item(
        4,
        s.cube,
        material(&mut renderer, [0.2, 0.2, 0.25, 1.0], 0.0),
        Mat4::from_scale_rotation_translation(
            Vec3::new(0.5, 8.0, 0.5),
            Quat::IDENTITY,
            Vec3::new(0.0, 4.0, -6.0),
        ),
        DrawFlags::NONE,
    ));
    let extras = SceneExtras {
        sky: Some(Sky {
            zenith: [0.1, 0.25, 0.6],
            horizon: [0.6, 0.6, 0.65],
            ground: [0.3, 0.28, 0.25],
            sun: Some((to_sun, [4.0, 3.5, 2.5])),
        }),
        ..SceneExtras::default()
    };
    let (plain, _) = draw(&gpu, &mut renderer, &snapshot, &extras);
    snapshot.post.sun_rays = SunRays {
        intensity: 1.0,
        spread: 0.3,
    };
    let (rays, report) = draw(&gpu, &mut renderer, &snapshot, &extras);
    assert_eq!(report.stats.post_passes, 2);
    let luma = |i: &Image| i.stats([0; 4]).luma_mean;
    assert!(
        luma(&rays) > luma(&plain) + 1.0,
        "{} {}",
        luma(&rays),
        luma(&plain)
    );
    check_golden(&gpu, "post_sun_rays", &side_by_side(&[plain, rays]));
}

/// A capture shown in the world: unlit and depth-tested (hidden behind the
/// cube in front of it), lit, and on top of everything.
#[test]
fn golden_world_quads() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let s = scene(&mut renderer);

    // The quads' picture: the scene itself, drawn into a small capture.
    let picture = Capture::new(&gpu.device, 64, 48).unwrap();
    let mut scene = s.snapshot.clone();
    scene.clear = [0.6, 0.1, 0.4, 1.0];
    renderer
        .render(&gpu.device, &gpu.queue, picture.view(), &scene)
        .unwrap();
    let texture = renderer.capture_texture(&picture);
    assert_eq!(renderer.stats().textures.bytes, 0);

    let quad = |x: f32, depth: QuadDepth, light_influence: f32| WorldQuad {
        entity: EntityId(10),
        world: Mat4::from_translation(Vec3::new(x, 1.2, -1.0)),
        size: Vec2::new(1.2, 0.9),
        texture,
        depth,
        light_influence,
        brightness: 1.0,
    };
    let mut snapshot = s.snapshot.clone();
    snapshot.quads = vec![
        quad(-1.3, QuadDepth::Tested { bias: 0.0 }, 0.0),
        quad(0.1, QuadDepth::Tested { bias: 0.0 }, 1.0),
        WorldQuad {
            world: Mat4::from_rotation_translation(
                Quat::from_rotation_y(-0.4),
                Vec3::new(1.4, 0.4, 1.5),
            ),
            ..quad(0.0, QuadDepth::OnTop, 0.0)
        },
    ];
    let (image, report) = draw(&gpu, &mut renderer, &snapshot, &SceneExtras::default());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.stats.world_quads, 3);
    check_golden(&gpu, "world_quads", &image);

    // A released texture skips its quad with a warning.
    renderer.release_texture(texture).unwrap();
    let (_, report) = draw(&gpu, &mut renderer, &snapshot, &SceneExtras::default());
    assert_eq!(report.stats.world_quads, 0);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert_eq!(report.warnings[0].code, re_render::codes::STALE_TEXTURE);
}

/// An unlit quad shows its texture's colours as they are; brightness scales
/// them, and the depth bias lifts a quad off a surface it lies on.
#[test]
fn quads_show_their_texture_unlit() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    // A flat colour as the texture: a capture cleared to it.
    let picture = Capture::new(&gpu.device, 8, 8).unwrap();
    let flat = RenderSnapshot {
        clear: [0.2, 0.5, 0.1, 1.0],
        ..RenderSnapshot::default()
    };
    renderer
        .render(&gpu.device, &gpu.queue, picture.view(), &flat)
        .unwrap();
    let texture = renderer.capture_texture(&picture);
    let wall = renderer.upload_mesh(&cube()).unwrap();
    let grey = material(&mut renderer, [0.5, 0.5, 0.5, 1.0], 0.0);
    let snapshot = |brightness: f32, bias: f32| RenderSnapshot {
        camera: camera(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO),
        // A wall whose front face is at z = 0, where the quad lies.
        items: vec![item(
            1,
            wall,
            grey,
            Mat4::from_scale_rotation_translation(
                Vec3::new(4.0, 4.0, 1.0),
                Quat::IDENTITY,
                Vec3::new(0.0, 0.0, -0.5),
            ),
            DrawFlags::NONE,
        )],
        quads: vec![WorldQuad {
            entity: EntityId(2),
            world: Mat4::IDENTITY,
            size: Vec2::splat(1.0),
            texture,
            depth: QuadDepth::Tested { bias },
            light_influence: 0.0,
            brightness,
        }],
        clear: CLEAR,
        ..RenderSnapshot::default()
    };
    let centre = |image: &Image| image.pixel(W / 2, H / 2);
    let (image, _) = draw(
        &gpu,
        &mut renderer,
        &snapshot(1.0, 0.01),
        &SceneExtras::default(),
    );
    let want = [0.2f32, 0.5, 0.1].map(re_render::capture::linear_to_srgb_u8);
    let got = centre(&image);
    assert!(
        got.iter().zip(&want).all(|(a, b)| a.abs_diff(*b) <= 2),
        "{got:?} vs {want:?}"
    );
    let (half, _) = draw(
        &gpu,
        &mut renderer,
        &snapshot(0.5, 0.01),
        &SceneExtras::default(),
    );
    let want = [0.1f32, 0.25, 0.05].map(re_render::capture::linear_to_srgb_u8);
    let got = centre(&half);
    assert!(
        got.iter().zip(&want).all(|(a, b)| a.abs_diff(*b) <= 2),
        "{got:?} vs {want:?}"
    );
    // Behind the wall's face, the quad is hidden.
    let (hidden, _) = draw(
        &gpu,
        &mut renderer,
        &snapshot(1.0, -0.05),
        &SceneExtras::default(),
    );
    assert!(centre(&hidden)[1] < 150, "{:?}", centre(&hidden));
}
