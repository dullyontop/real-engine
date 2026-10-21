//! GPU skinning (ADR-0039): skinned items against their rigid equivalents, the
//! skinned shadow and highlight passes, undrawable items, and a golden of three
//! poses of one skinned mesh.
//!
//! Run like the other GPU tests: `cargo test -p re_render -- --test-threads=1`
//! with `RE_BACKENDS=vulkan` and `RE_BACKENDS=gl`.

mod common;

use common::*;
use glam::{Mat4, Quat, Vec3};
use re_render::imgdiff::{self, Tolerance};
use re_render::{Capture, Gpu, Highlight, Image, Renderer, SceneExtras, codes};
use re_types::EntityId;
use re_types::mesh::{Aabb, MeshData, SkinWeights};
use re_types::render::{MaterialDesc, MaterialHandle, MeshHandle, RenderSnapshot, SkinnedDrawItem};

const W: u32 = 320;
const H: u32 = 240;

/// The arm's joints: the shoulder at the origin, the elbow at y = 1.
const ELBOW: Vec3 = Vec3::new(0.0, 1.0, 0.0);

/// A square column from y = 0 to y = 2, eight segments high, with a cap; the
/// lower half follows joint 0, the upper half joint 1, blended around the elbow.
fn arm() -> (MeshData, SkinWeights) {
    let mut mesh = MeshData::default();
    let mut weights = SkinWeights::default();
    let half = 0.18;
    let segments = 8;
    let mut push = |mesh: &mut MeshData, p: Vec3, n: Vec3| {
        let t = ((p.y - 0.7) / 0.6).clamp(0.0, 1.0);
        let upper = (t * t * (3.0 - 2.0 * t) * 65535.0).round() as u16;
        mesh.vertices
            .push(vertex(p, n, [0.0, 0.0], [1.0, 1.0, 1.0, 1.0]));
        weights.joints.push([0, 1, 0, 0]);
        weights.weights.push([65535 - upper, upper, 0, 0]);
    };
    for (n, u) in [
        (Vec3::Z, Vec3::X),
        (Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Z, Vec3::NEG_X),
        (Vec3::NEG_X, Vec3::Z),
    ] {
        let base = mesh.vertices.len() as u32;
        for k in 0..=segments {
            let y = 2.0 * k as f32 / segments as f32;
            let c = n * half + Vec3::Y * y;
            push(&mut mesh, c - u * half, n);
            push(&mut mesh, c + u * half, n);
        }
        for k in 0..segments {
            let a = base + 2 * k;
            mesh.indices
                .extend_from_slice(&[a, a + 1, a + 3, a, a + 3, a + 2]);
        }
    }
    let base = mesh.vertices.len() as u32;
    for (x, z) in [(-1.0, 1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
        push(&mut mesh, Vec3::new(x * half, 2.0, z * half), Vec3::Y);
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    (mesh, weights)
}

/// The palette of the arm bent by `angle` at the elbow (joint matrix times
/// inverse bind; the shoulder stays at the rest pose).
fn bent(angle: f32) -> [Mat4; 2] {
    let elbow = Mat4::from_rotation_translation(Quat::from_rotation_z(angle), ELBOW);
    [Mat4::IDENTITY, elbow * Mat4::from_translation(-ELBOW)]
}

/// A world bound of `mesh` posed by `palette` (each vertex by each matrix,
/// which contains every blend of them).
fn bounds(mesh: &MeshData, palette: &[Mat4], world: Mat4) -> Aabb {
    Aabb::from_points(mesh.vertices.iter().flat_map(|v| {
        palette
            .iter()
            .map(move |m| world.transform_point3(m.transform_point3(Vec3::from(v.pos))))
    }))
    .unwrap()
}

fn skinned(
    entity: u64,
    mesh: (MeshHandle, &MeshData),
    material: MaterialHandle,
    world: Mat4,
    palette: &mut Vec<Mat4>,
    pose: &[Mat4],
) -> SkinnedDrawItem {
    let first = palette.len() as u32;
    palette.extend_from_slice(pose);
    SkinnedDrawItem {
        entity: EntityId(entity),
        mesh: mesh.0,
        material,
        world,
        flags: SHADOWED,
        palette: first,
        joints: pose.len() as u32,
        bounds: bounds(mesh.1, pose, world),
    }
}

fn material(renderer: &mut Renderer, color: [f32; 4]) -> MaterialHandle {
    renderer
        .create_material(&MaterialDesc {
            base_color: color,
            roughness: 0.6,
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

fn base_scene(renderer: &mut Renderer) -> RenderSnapshot {
    let ground = renderer.upload_mesh(&plane(4.0, 1.0)).unwrap();
    let grey = material(renderer, [0.7, 0.72, 0.68, 1.0]);
    RenderSnapshot {
        camera: camera(Vec3::new(1.5, 2.8, 6.5), Vec3::new(0.0, 1.0, 0.0)),
        lights: vec![sun(Vec3::new(-0.5, -1.0, -0.4), 1.0, true)],
        items: vec![item(100, ground, grey, Mat4::IDENTITY, SHADOWED)],
        clear: CLEAR,
        ..Default::default()
    }
}

fn highlight(entity: u64) -> SceneExtras {
    SceneExtras {
        highlights: vec![Highlight {
            entities: vec![EntityId(entity)],
            fill: [1.0, 0.4, 0.1, 0.3],
            outline: [1.0, 1.0, 1.0, 1.0],
            always_on_top: false,
        }],
        ..SceneExtras::default()
    }
}

fn assert_same(a: &Image, b: &Image, what: &str) {
    let report = imgdiff::compare(a, b, Tolerance::default()).unwrap();
    assert!(report.matches(), "{what}: {}", report.summary());
}

/// Three poses of one skinned arm, with shadows and a highlight.
#[test]
fn golden_skinned_arms() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let (arm_mesh, weights) = arm();
    let arm = renderer.upload_skinned_mesh(&arm_mesh, &weights).unwrap();
    let colors = [
        [0.8, 0.25, 0.2, 1.0],
        [0.25, 0.65, 0.3, 1.0],
        [0.2, 0.4, 0.85, 1.0],
    ];
    let mut snapshot = base_scene(&mut renderer);
    let mut palette = Vec::new();
    for (i, (angle, color)) in [0.0f32, 0.7, 1.5].into_iter().zip(colors).enumerate() {
        let mat = material(&mut renderer, color);
        let world = Mat4::from_translation(Vec3::new(1.6 * i as f32 - 1.6, 0.0, 0.0));
        let item = skinned(
            i as u64 + 1,
            (arm, &arm_mesh),
            mat,
            world,
            &mut palette,
            &bent(angle),
        );
        snapshot.skinned.push(item);
    }
    snapshot.joint_palette = palette;
    let (image, report) = draw(&gpu, &mut renderer, &snapshot, &highlight(2));
    let stats = renderer.stats().last_frame;
    if renderer.supports_skinning() {
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(stats.skinned_items, 3);
    } else {
        assert_eq!(report.warnings[0].code, codes::SKINNING_UNSUPPORTED);
        eprintln!("no GPU skinning on this adapter; skipping the golden");
        return;
    }
    assert_eq!(stats.items_drawn, 4);
    assert!(stats.shadow_casters >= 4, "{stats:?}");
    check_golden(&gpu, "skinned_arms", &image);
}

/// A skinned item whose joints all move rigidly draws like the plain item
/// moved the same way, in the scene, shadow and highlight passes; at the bind
/// pose it draws like the unskinned mesh.
#[test]
fn rigid_skins_match_plain_items() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    if !renderer.supports_skinning() {
        eprintln!("no GPU skinning on this adapter; skipping");
        return;
    }
    let (arm_mesh, weights) = arm();
    let skin = renderer.upload_skinned_mesh(&arm_mesh, &weights).unwrap();
    let plain = renderer.upload_mesh(&arm_mesh).unwrap();
    let red = material(&mut renderer, [0.8, 0.25, 0.2, 1.0]);
    let world = Mat4::from_translation(Vec3::new(-0.5, 0.0, 0.3));
    let moved = Mat4::from_rotation_translation(
        Quat::from_rotation_y(0.8) * Quat::from_rotation_z(-0.4),
        Vec3::new(0.6, 0.4, -0.2),
    );
    for pose in [Mat4::IDENTITY, moved] {
        let mut expected = base_scene(&mut renderer);
        expected
            .items
            .push(item(1, plain, red, world * pose, SHADOWED));
        let mut actual = base_scene(&mut renderer);
        let mut palette = vec![Mat4::from_scale(Vec3::splat(9.0))]; // not this item's
        let item = skinned(1, (skin, &arm_mesh), red, world, &mut palette, &[pose; 2]);
        actual.skinned.push(item);
        actual.joint_palette = palette;
        let extras = highlight(1);
        let (want, _) = draw(&gpu, &mut renderer, &expected, &extras);
        let (got, report) = draw(&gpu, &mut renderer, &actual, &extras);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.stats.skinned_items, 1);
        assert_same(&got, &want, &format!("pose {pose}"));
    }
}

/// Plain items can draw a skinned mesh (in its bind pose), and skinned and
/// plain instances of one mesh batch apart.
#[test]
fn skinned_meshes_draw_as_plain_items() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let (arm_mesh, weights) = arm();
    let skin = renderer.upload_skinned_mesh(&arm_mesh, &weights).unwrap();
    let plain = renderer.upload_mesh(&arm_mesh).unwrap();
    let red = material(&mut renderer, [0.8, 0.25, 0.2, 1.0]);
    let mut a = base_scene(&mut renderer);
    a.items.push(item(1, skin, red, Mat4::IDENTITY, SHADOWED));
    let mut b = base_scene(&mut renderer);
    b.items.push(item(1, plain, red, Mat4::IDENTITY, SHADOWED));
    let (got, report) = draw(&gpu, &mut renderer, &a, &highlight(1));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.stats.skinned_items, 0);
    let (want, _) = draw(&gpu, &mut renderer, &b, &highlight(1));
    assert_same(&got, &want, "skinned mesh as a plain item");

    if renderer.supports_skinning() {
        // One plain and two skinned instances of one mesh and material: two
        // scene draw calls besides the ground.
        let mut palette = Vec::new();
        let first = skinned(
            2,
            (skin, &arm_mesh),
            red,
            Mat4::IDENTITY,
            &mut palette,
            &bent(0.5),
        );
        let second = skinned(
            3,
            (skin, &arm_mesh),
            red,
            Mat4::IDENTITY,
            &mut palette,
            &bent(1.0),
        );
        a.skinned = vec![first, second];
        a.joint_palette = palette;
        let (_, report) = draw(&gpu, &mut renderer, &a, &SceneExtras::default());
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.stats.skinned_items, 2);
        assert_eq!(report.stats.draw_calls, 3);
    }
}

#[test]
fn undrawable_skinned_items_are_skipped_with_warnings() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    if !renderer.supports_skinning() {
        eprintln!("no GPU skinning on this adapter; skipping");
        return;
    }
    let (arm_mesh, weights) = arm();
    let skin = renderer.upload_skinned_mesh(&arm_mesh, &weights).unwrap();
    let plain = renderer.upload_mesh(&arm_mesh).unwrap();
    let red = material(&mut renderer, [0.8, 0.25, 0.2, 1.0]);
    let mut palette = Vec::new();
    let good = skinned(
        1,
        (skin, &arm_mesh),
        red,
        Mat4::IDENTITY,
        &mut palette,
        &bent(0.3),
    );
    let unskinned = SkinnedDrawItem {
        entity: EntityId(2),
        mesh: plain,
        ..good
    };
    let past_end = SkinnedDrawItem {
        entity: EntityId(3),
        palette: 1,
        ..good
    };
    let too_few = SkinnedDrawItem {
        entity: EntityId(4),
        joints: 1,
        ..good
    };
    let mut snapshot = base_scene(&mut renderer);
    snapshot.skinned = vec![good, unskinned, past_end, too_few];
    snapshot.joint_palette = palette;
    let (_, report) = draw(&gpu, &mut renderer, &snapshot, &SceneExtras::default());
    let codes: Vec<_> = report.warnings.iter().map(|w| w.code).collect();
    assert_eq!(
        codes,
        vec![codes::BAD_SKINNED_ITEM; 3],
        "{:?}",
        report.warnings
    );
    for (entity, words) in [(2, "no skin"), (3, "palette 1..3"), (4, "use 2")] {
        let w = &report.warnings[entity - 2];
        assert!(
            w.message.contains(&format!("entity {entity}")),
            "{}",
            w.message
        );
        assert!(w.message.contains(words), "{}", w.message);
    }
    assert_eq!(report.stats.items_skipped, 3);
    assert_eq!(report.stats.skinned_items, 1);
    assert_eq!(report.stats.items_drawn, 2);
}

#[test]
fn malformed_skin_weights_are_rejected() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let (mesh, weights) = arm();
    let mut short = weights.clone();
    short.joints.pop();
    let mut unnormalized = weights.clone();
    unnormalized.weights[3] = [100, 100, 0, 0];
    for bad in [short, unnormalized] {
        let err = renderer.upload_skinned_mesh(&mesh, &bad).unwrap_err();
        assert_eq!(err.code, codes::BAD_MESH, "{err}");
    }
    assert_eq!(renderer.stats().meshes.count, 0);
    // A malformed mesh is still a malformed mesh.
    let mut odd = mesh.clone();
    odd.indices.pop();
    let err = renderer.upload_skinned_mesh(&odd, &weights).unwrap_err();
    assert_eq!(err.code, codes::BAD_MESH);
}
