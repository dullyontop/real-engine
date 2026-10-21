//! Headless GPU tests of [`SceneExtras`]: fog, sky, colour correction, spot
//! lights, sprites, ribbons and highlights. Run like `tests/render.rs`.

mod common;

use common::*;
use glam::{Mat4, Vec3};
use re_render::{
    Ambient, Capture, ColorGrading, Fog, Gpu, Highlight, Image, Renderer, Ribbon, RibbonPoint,
    SceneExtras, Sky, SpotLight, Sprite, SpriteShape, codes,
};
use re_types::EntityId;
use re_types::render::{DrawFlags, MaterialDesc, MaterialHandle, MeshHandle, RenderSnapshot};

const W: u32 = 320;
const H: u32 = 240;

fn material(renderer: &mut Renderer, color: [f32; 4]) -> MaterialHandle {
    renderer
        .create_material(&MaterialDesc {
            base_color: color,
            roughness: 0.8,
            ..MaterialDesc::default()
        })
        .unwrap()
}

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

fn pixel_at(image: &Image, snapshot: &RenderSnapshot, p: Vec3) -> [u8; 4] {
    let (x, y) = project_to_pixel(view_proj(&snapshot.camera, W as f32 / H as f32), p, W, H)
        .expect("on screen");
    image.pixel(x, y)
}

struct Scene {
    snapshot: RenderSnapshot,
    cube: MeshHandle,
}

/// A ground plane, a grey cube at the origin (entity 2) and a blue cube
/// behind it (entity 3), seen from the front.
fn scene(renderer: &mut Renderer) -> Scene {
    let plane = renderer.upload_mesh(&plane(12.0, 1.0)).unwrap();
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let ground = material(renderer, [0.5, 0.5, 0.5, 1.0]);
    let grey = material(renderer, [0.6, 0.6, 0.6, 1.0]);
    let blue = material(renderer, [0.2, 0.3, 0.8, 1.0]);
    Scene {
        snapshot: RenderSnapshot {
            camera: camera(Vec3::new(0.0, 2.5, 7.0), Vec3::new(0.0, 0.8, 0.0)),
            lights: vec![sun(Vec3::new(-0.3, -1.0, -0.5), 1.0, false)],
            items: vec![
                item(1, plane, ground, Mat4::IDENTITY, DrawFlags::NONE),
                item(
                    2,
                    cube,
                    grey,
                    Mat4::from_translation(Vec3::new(0.0, 0.5, 0.0)),
                    DrawFlags::NONE,
                ),
                item(
                    3,
                    cube,
                    blue,
                    Mat4::from_scale_rotation_translation(
                        Vec3::splat(0.8),
                        glam::Quat::IDENTITY,
                        Vec3::new(0.0, 0.4, -2.5),
                    ),
                    DrawFlags::NONE,
                ),
            ],
            debug_lines: Vec::new(),
            clear: CLEAR,
            ..Default::default()
        },
        cube,
    }
}

fn close(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
    a.iter()
        .zip(&b)
        .take(3)
        .all(|(x, y)| x.abs_diff(*y) <= tolerance)
}

fn srgb(c: [f32; 3]) -> [u8; 4] {
    let e = re_render::capture::linear_to_srgb_u8;
    [e(c[0]), e(c[1]), e(c[2]), 255]
}

/// Everything at once, as a golden image.
#[test]
fn golden_scene_extras() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let mut s = scene(&mut renderer);
    let neon = renderer
        .create_material(&MaterialDesc {
            base_color: [0.1, 0.1, 0.1, 1.0],
            emissive: [1.0, 0.2, 0.6],
            emissive_strength: 2.0,
            ..MaterialDesc::default()
        })
        .unwrap();
    s.snapshot.items.push(item(
        4,
        s.cube,
        neon,
        Mat4::from_scale_rotation_translation(
            Vec3::splat(0.5),
            glam::Quat::IDENTITY,
            Vec3::new(2.2, 0.25, 0.5),
        ),
        DrawFlags::NONE,
    ));
    let sprites = (0..24)
        .map(|i| {
            let t = i as f32 / 24.0;
            Sprite {
                position: Vec3::new(-2.2 + 0.4 * libm::sinf(t * 9.0), 0.3 + 2.0 * t, 0.5),
                size: 0.5 - 0.3 * t,
                rotation: t,
                color: [1.0, 0.6 - 0.4 * t, 0.1, 1.0 - t],
                emission: 1.0,
                shape: SpriteShape::Soft,
            }
        })
        .collect();
    let beam = Ribbon {
        points: (0..=12)
            .map(|i| {
                let t = i as f32 / 12.0;
                RibbonPoint {
                    position: Vec3::new(-2.0 + 4.0 * t, 2.2 + 0.4 * libm::sinf(t * 6.0), -1.0),
                    width: 0.15 + 0.2 * t,
                    color: [0.2 + 0.8 * t, 0.9, 1.0 - 0.6 * t, 0.8],
                }
            })
            .collect(),
        normal: None,
        emission: 0.5,
    };
    let extras = SceneExtras {
        ambient: Some(Ambient {
            sky: [0.25, 0.27, 0.32],
            ground: [0.12, 0.1, 0.08],
        }),
        fog: Some(Fog {
            start: 8.0,
            end: 22.0,
            color: [0.55, 0.6, 0.7],
        }),
        sky: Some(Sky {
            zenith: [0.15, 0.3, 0.7],
            horizon: [0.55, 0.6, 0.7],
            ground: [0.3, 0.28, 0.25],
            sun: Some((Vec3::new(0.3, 1.0, -0.5), [1.0, 0.9, 0.7])),
        }),
        grading: Some(ColorGrading {
            saturation: 0.15,
            ..ColorGrading::default()
        }),
        spot_lights: vec![SpotLight {
            position: Vec3::new(-1.5, 3.0, 1.5),
            direction: Vec3::new(0.2, -1.0, -0.2).normalize(),
            color: [1.0, 0.85, 0.5],
            intensity: 120.0,
            range: 8.0,
            half_angle: 0.45,
        }],
        sprites,
        ribbons: vec![beam],
        highlights: vec![Highlight {
            entities: vec![EntityId(3)],
            fill: [1.0, 0.3, 0.1, 0.35],
            outline: [1.0, 1.0, 1.0, 1.0],
            always_on_top: true,
        }],
    };
    let (image, report) = draw(&gpu, &mut renderer, &s.snapshot, &extras);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let stats = report.stats;
    assert_eq!(stats.spot_lights, 1);
    assert_eq!(stats.highlights, 1);
    assert_eq!(stats.effect_quads, 24 + 12);
    check_golden(&gpu, "scene_extras", &image);
}

#[test]
fn the_sky_replaces_the_clear_colour_and_fog_reaches_its_colour() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let s = scene(&mut renderer);
    let sky = Sky {
        zenith: [0.1, 0.2, 0.8],
        horizon: [0.8, 0.8, 0.8],
        ground: [0.2, 0.2, 0.2],
        sun: None,
    };
    let extras = SceneExtras {
        sky: Some(sky),
        fog: Some(Fog {
            start: 0.0,
            end: 1.0,
            color: [0.9, 0.1, 0.1],
        }),
        ..SceneExtras::default()
    };
    let (image, _) = draw(&gpu, &mut renderer, &s.snapshot, &extras);
    // The top row looks up past the horizon: bluer than it is red, not the clear colour.
    let top = image.pixel(W / 2, 0);
    assert!(!close(top, clear_srgb8(), 3), "{top:?}");
    assert!(top[2] > top[0], "{top:?}");
    // Every drawn surface is more than one unit away: fully fogged.
    let cube = pixel_at(&image, &s.snapshot, Vec3::new(0.0, 0.5, 0.5));
    assert!(close(cube, srgb([0.9, 0.1, 0.1]), 2), "{cube:?}");
}

#[test]
fn a_spot_light_lights_inside_its_cone_only() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let mut s = scene(&mut renderer);
    s.snapshot.lights.clear();
    s.snapshot.items.truncate(1);
    let dark = SceneExtras {
        ambient: Some(Ambient {
            sky: [0.0; 3],
            ground: [0.0; 3],
        }),
        ..SceneExtras::default()
    };
    let lit = SceneExtras {
        spot_lights: vec![SpotLight {
            position: Vec3::new(2.0, 3.0, 0.0),
            direction: Vec3::NEG_Y,
            color: [1.0; 3],
            intensity: 400.0,
            range: 10.0,
            half_angle: 0.3,
        }],
        ..dark.clone()
    };
    let (before, _) = draw(&gpu, &mut renderer, &s.snapshot, &dark);
    let (after, report) = draw(&gpu, &mut renderer, &s.snapshot, &lit);
    assert_eq!(report.stats.spot_lights, 1);
    let under = Vec3::new(2.0, 0.0, 0.0);
    let outside = Vec3::new(-2.0, 0.0, 0.0);
    assert!(pixel_at(&before, &s.snapshot, under)[0] < 5);
    assert!(pixel_at(&after, &s.snapshot, under)[0] > 60);
    assert!(close(
        pixel_at(&after, &s.snapshot, outside),
        pixel_at(&before, &s.snapshot, outside),
        1
    ));
}

#[test]
fn additive_sprites_brighten_and_grading_tints() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let s = scene(&mut renderer);
    let p = Vec3::new(0.0, 0.5, 0.6);
    let (plain, _) = draw(&gpu, &mut renderer, &s.snapshot, &SceneExtras::default());
    let glow = SceneExtras {
        sprites: vec![Sprite {
            position: p + Vec3::Z * 0.5,
            size: 1.0,
            rotation: 0.0,
            color: [0.0, 0.5, 0.0, 1.0],
            emission: 1.0,
            shape: SpriteShape::Square,
        }],
        ..SceneExtras::default()
    };
    let (lit, _) = draw(&gpu, &mut renderer, &s.snapshot, &glow);
    let (a, b) = (
        pixel_at(&plain, &s.snapshot, p),
        pixel_at(&lit, &s.snapshot, p),
    );
    assert!(b[1] > a[1] + 30 && b[0].abs_diff(a[0]) <= 1, "{a:?} {b:?}");

    let red = SceneExtras {
        grading: Some(ColorGrading {
            tint: [1.0, 0.0, 0.0],
            ..ColorGrading::default()
        }),
        ..SceneExtras::default()
    };
    let (tinted, _) = draw(&gpu, &mut renderer, &s.snapshot, &red);
    let t = pixel_at(&tinted, &s.snapshot, p);
    assert!(t[0] > 50 && t[1] == 0 && t[2] == 0, "{t:?}");
}

#[test]
fn highlights_fill_and_outline_with_depth_modes() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let s = scene(&mut renderer);
    // The blue cube (entity 3) is hidden behind the grey one, at the centre of
    // the view; its top shows above it.
    let hidden = Vec3::new(0.0, 0.4, -2.1);
    let highlight = |always_on_top| SceneExtras {
        highlights: vec![Highlight {
            entities: vec![EntityId(3)],
            fill: [0.0, 1.0, 0.0, 1.0],
            outline: [1.0, 0.0, 1.0, 1.0],
            always_on_top,
        }],
        ..SceneExtras::default()
    };
    let (plain, _) = draw(&gpu, &mut renderer, &s.snapshot, &SceneExtras::default());
    let (occluded, report) = draw(&gpu, &mut renderer, &s.snapshot, &highlight(false));
    assert_eq!(report.stats.highlights, 1);
    let (on_top, _) = draw(&gpu, &mut renderer, &s.snapshot, &highlight(true));
    let green = [0, 255, 0, 255];
    let magenta = [255, 0, 255, 255];
    // Behind the grey cube: only the on-top highlight shows there.
    assert!(close(
        pixel_at(&occluded, &s.snapshot, hidden),
        pixel_at(&plain, &s.snapshot, hidden),
        1
    ));
    assert!(close(pixel_at(&on_top, &s.snapshot, hidden), green, 1));
    // The visible top of the blue cube is filled in both modes.
    let top = Vec3::new(0.0, 0.8, -2.5);
    assert!(close(pixel_at(&occluded, &s.snapshot, top), green, 1));
    // An outline runs around the filled region.
    let count = |image: &Image, c: [u8; 4]| {
        (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|&(x, y)| close(image.pixel(x, y), c, 1))
            .count()
    };
    assert!(count(&on_top, magenta) > 40);
    assert!(count(&on_top, green) > count(&occluded, green));
}

#[test]
fn too_many_highlights_warn_and_the_first_wins() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let s = scene(&mut renderer);
    let highlights: Vec<Highlight> = (0..40)
        .map(|i| Highlight {
            entities: vec![EntityId(2)],
            fill: [i as f32 / 40.0, 0.0, 0.0, 1.0],
            outline: [0.0; 4],
            always_on_top: false,
        })
        .collect();
    let extras = SceneExtras {
        highlights,
        ..SceneExtras::default()
    };
    let (image, report) = draw(&gpu, &mut renderer, &s.snapshot, &extras);
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.warnings[0].code, codes::TOO_MANY_HIGHLIGHTS);
    assert_eq!(report.stats.highlights, re_render::MAX_HIGHLIGHTS as u32);
    // The first highlight's fill (black) covers the grey cube.
    let c = pixel_at(&image, &s.snapshot, Vec3::new(0.0, 0.5, 0.5));
    assert!(close(c, [0, 0, 0, 255], 1), "{c:?}");
}
