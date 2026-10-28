//! The scene's depth (ADR-0038's amendment for WP32): reversed Z with no far
//! plane keeps a road 5 cm above the ground in front of it far into the view.
//! With standard Z the ground showed through from 231 m on lavapipe and 264 m
//! on GL, seen from 100 m up.
//!
//! Run once per backend, like the other GPU tests: `RE_BACKENDS=vulkan` (the
//! default on Linux) and `RE_BACKENDS=gl`.

mod common;

use common::*;
use glam::{Mat4, Vec3};
use re_render::{Capture, Renderer};
use re_types::mesh::MeshData;
use re_types::render::{Camera, DrawFlags, MaterialDesc, RenderSnapshot};

const W: u32 = 1280;
const H: u32 = 720;
/// The eye's height above the ground.
const EYE: f32 = 100.0;
/// How far the road must stay in front of the ground.
const REACH: f32 = 2000.0;

/// An unlit plane x in -500..500, z in 0..-12000 at y = 0, facing +Y.
fn strip() -> MeshData {
    let mut mesh = MeshData::default();
    push_quad(
        &mut mesh,
        Vec3::new(0.0, 0.0, -6000.0),
        Vec3::X * 500.0,
        Vec3::NEG_Z * 6000.0,
        [1.0; 4],
    );
    mesh
}

#[test]
fn a_road_above_the_ground_stays_in_front_out_to_two_kilometres() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let strip = renderer.upload_mesh(&strip()).unwrap();
    let mut unlit = |color: [f32; 4]| {
        renderer
            .create_material(&MaterialDesc {
                base_color: color,
                unlit: true,
                ..MaterialDesc::default()
            })
            .unwrap()
    };
    // The ground's material comes first, so the ground is drawn first and the
    // road, at the same depth or nearer, must pass the depth test to show.
    let ground = unlit([1.0, 0.0, 0.0, 1.0]);
    let road = unlit([0.0, 1.0, 0.0, 1.0]);
    let fov = 60f32.to_radians();
    // Looking down -Z, 2 degrees below the horizon.
    let pitch = -2f32.to_radians();
    let dir = Vec3::new(0.0, libm::sinf(pitch), -libm::cosf(pitch));
    let camera = Camera {
        world: glam::camera::rh::view::look_to_mat4(Vec3::Y * EYE, dir, Vec3::Y).inverse(),
        fov_y_radians: fov,
        near: 0.1,
        far: 6000.0,
    };
    let snapshot = RenderSnapshot {
        camera,
        items: vec![
            item(1, strip, ground, Mat4::IDENTITY, DrawFlags::NONE),
            item(
                2,
                strip,
                road,
                Mat4::from_translation(Vec3::Y * 0.05),
                DrawFlags::NONE,
            ),
        ],
        clear: CLEAR,
        ..Default::default()
    };
    let image = render_image(&gpu, &mut renderer, &snapshot, W, H);

    // Each row below the horizon looks at the ground at one distance. Of the
    // strip's pixels in a row (red ground or green road), none may be red up to
    // `REACH`.
    let tan = libm::tanf(fov * 0.5);
    let mut checked = 0;
    // The rows where the ground shows: distance, red pixels, strip pixels.
    let mut lost = Vec::new();
    for y in 0..H {
        let ndc_y = 1.0 - (y as f32 + 0.5) / H as f32 * 2.0;
        let ray = camera
            .world
            .transform_vector3(Vec3::new(0.0, ndc_y * tan, -1.0))
            .normalize();
        if ray.y >= 0.0 {
            continue;
        }
        let distance = EYE / -ray.y * ray.with_y(0.0).length();
        if distance > REACH {
            continue;
        }
        let (mut red, mut green) = (0, 0);
        for x in 0..W {
            let [r, g, ..] = image.pixel(x, y);
            if r > 128 && r > g {
                red += 1;
            } else if g > 128 {
                green += 1;
            }
        }
        assert!(red + green > 0, "row {y} ({distance:.0} m) shows no strip");
        if red > 0 {
            lost.push((distance, red, red + green));
        }
        checked += 1;
    }
    // The rows from the bottom of the view (about 160 m) to 2 km.
    assert!(checked > 100, "{checked} rows checked");
    if let Some(&(nearest, red, all)) = lost.iter().min_by(|a, b| a.0.total_cmp(&b.0)) {
        panic!(
            "the ground shows through the road from {nearest:.0} m ({red} of {all} pixels \
             there), in {} of {checked} rows",
            lost.len()
        );
    }
}
