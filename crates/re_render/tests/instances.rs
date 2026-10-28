//! Instance sets (ADR-0052): a small forest drawn as a set and as the same
//! trees as draw items, culling by cluster, distance ranges and the
//! statistics through the renderer, the shadow and highlight passes, hot
//! reload of the set shaders, and the upload and handle errors. Run like
//! `tests/render.rs`.

mod common;

use common::*;
use glam::{Mat4, Quat, Vec3};
use re_render::imgdiff::{self, Tolerance};
use re_render::{Capture, FrameStats, Gpu, Highlight, Image, Renderer, SceneExtras, codes};
use re_types::EntityId;
use re_types::mesh::Aabb;
use re_types::render::{
    DrawFlags, DrawItem, InstanceCluster, InstancePart, InstanceSet, MaterialDesc, MaterialHandle,
    Placement, RenderSnapshot,
};

fn material(renderer: &mut Renderer, rgb: [f32; 3]) -> MaterialHandle {
    renderer
        .create_material(&MaterialDesc {
            base_color: [rgb[0], rgb[1], rgb[2], 1.0],
            roughness: 0.85,
            ..MaterialDesc::default()
        })
        .unwrap()
}

/// A number in 0..1 from `i` and `salt`, the same on every run.
fn hash(i: u32, salt: u32) -> f32 {
    let mut x = i.wrapping_mul(0x9e37_79b9) ^ salt.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// The two parts of the valley's tree: a bark cube trunk and a leaf sphere
/// crown, the trunk's child.
fn tree_parts(renderer: &mut Renderer, flags: DrawFlags) -> Vec<InstancePart> {
    let trunk_mesh = renderer.upload_mesh(&cube()).unwrap();
    let crown_mesh = renderer.upload_mesh(&sphere(10, 16)).unwrap();
    let bark = material(renderer, [0.36, 0.24, 0.15]);
    let leaves = material(renderer, [0.16, 0.38, 0.14]);
    let trunk = Mat4::from_translation(Vec3::new(0.0, 1.6, 0.0))
        * Mat4::from_scale(Vec3::new(0.5, 3.2, 0.5));
    let crown = trunk
        * Mat4::from_translation(Vec3::new(0.0, 0.85, 0.0))
        * Mat4::from_scale(Vec3::new(6.0, 0.9, 6.0));
    vec![
        InstancePart {
            mesh: trunk_mesh,
            material: bark,
            transform: trunk,
            flags,
            min_distance: 0.0,
            max_distance: 0.0,
        },
        InstancePart {
            mesh: crown_mesh,
            material: leaves,
            transform: crown,
            flags,
            min_distance: 0.0,
            max_distance: 0.0,
        },
    ]
}

/// `side` x `side` trees on a jittered 4.5 m grid from the set's origin, in
/// four clusters (the quadrants, rows by z then x), turned and scaled each
/// their own way; one tree leans.
fn forest_placements(side: u32) -> (Vec<Placement>, Vec<InstanceCluster>) {
    let half = side / 2;
    let mut quadrants: [Vec<Placement>; 4] = Default::default();
    for j in 0..side {
        for i in 0..side {
            let n = j * side + i;
            let position = Vec3::new(
                (i as f32 + 0.2 + 0.6 * hash(n, 1)) * 4.5,
                0.0,
                (j as f32 + 0.2 + 0.6 * hash(n, 2)) * 4.5,
            );
            let mut rotation = Quat::from_rotation_y(hash(n, 3) * std::f32::consts::TAU);
            if n == side + 1 {
                rotation = Quat::from_rotation_z(0.3) * rotation;
            }
            let scale = 0.8 + 0.5 * hash(n, 4);
            let quadrant = usize::from(j >= half) * 2 + usize::from(i >= half);
            quadrants[quadrant].push(Placement::new(position, rotation, scale, n));
        }
    }
    let clusters = quadrants
        .iter()
        .map(|q| InstanceCluster::of(q).unwrap())
        .collect();
    (quadrants.concat(), clusters)
}

/// The same copies as draw items: each part at `set × placement × part`.
fn as_items(placements: &[Placement], set: &InstanceSet) -> Vec<DrawItem> {
    let mut items = Vec::new();
    for (n, p) in placements.iter().enumerate() {
        for (k, part) in set.parts.iter().enumerate() {
            items.push(item(
                1000 + 10 * n as u64 + k as u64,
                part.mesh,
                part.material,
                set.transform * p.transform() * part.transform,
                DrawFlags(set.flags.0 & part.flags.0),
            ));
        }
    }
    items
}

const SUN: Vec3 = Vec3::new(-0.45, -1.0, -0.3);

fn forest_scene(renderer: &mut Renderer) -> (RenderSnapshot, Vec<Placement>) {
    let ground = renderer.upload_mesh(&plane(40.0, 8.0)).unwrap();
    let grass = material(renderer, [0.36, 0.52, 0.22]);
    let parts = tree_parts(renderer, SHADOWED);
    let (placements, clusters) = forest_placements(7);
    let handle = renderer
        .upload_instance_set(&placements, &clusters)
        .unwrap_or_else(|d| panic!("{}", d.render_human()));
    let set = InstanceSet {
        entity: EntityId(2),
        set: handle,
        // A streamed cell's corner, turned a little so the set's own matrix
        // takes part in the normals.
        transform: Mat4::from_rotation_translation(
            Quat::from_rotation_y(0.3),
            Vec3::new(-19.5, 0.0, -16.5),
        ),
        parts,
        flags: SHADOWED,
    };
    let snapshot = RenderSnapshot {
        camera: camera(Vec3::new(0.0, 17.0, 32.0), Vec3::new(0.0, 0.0, -5.0)),
        lights: vec![sun(SUN, 1.0, true)],
        items: vec![item(
            1,
            ground,
            grass,
            Mat4::IDENTITY,
            DrawFlags::RECEIVE_SHADOWS,
        )],
        instance_sets: vec![set],
        clear: CLEAR,
        ..RenderSnapshot::default()
    };
    (snapshot, placements)
}

/// The same snapshot with the set's copies as draw items.
fn without_set(snapshot: &RenderSnapshot, placements: &[Placement]) -> RenderSnapshot {
    let mut items = snapshot.clone();
    let set = items.instance_sets.remove(0);
    items.items.extend(as_items(placements, &set));
    items
}

fn draw(gpu: &Gpu, renderer: &mut Renderer, snapshot: &RenderSnapshot) -> (Image, FrameStats) {
    let image = render_image(gpu, renderer, snapshot, 320, 240);
    (image, renderer.stats().last_frame)
}

/// A small forest with shadows, drawn as a set and as the same trees as draw
/// items: both match the one golden (rotations are snorm16 in a placement, so
/// the items use the placements' own transforms).
#[test]
fn golden_instance_forest_matches_the_same_trees_as_draw_items() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let (snapshot, placements) = forest_scene(&mut renderer);
    let (set_image, stats) = draw(&gpu, &mut renderer, &snapshot);
    println!("{stats:?}");
    assert_eq!(stats.instance_sets, 1);
    assert_eq!(stats.set_instances, 2 * 49, "every copy of both parts");
    assert_eq!(stats.set_clusters_culled, 0);
    assert_eq!(stats.items_drawn, 1, "the ground");
    assert_eq!(stats.shadow_cascades, 4);
    check_golden(&gpu, "instance_forest", &set_image);

    let items = without_set(&snapshot, &placements);
    let (item_image, item_stats) = draw(&gpu, &mut renderer, &items);
    // Items are culled one by one, a set's copies by cluster.
    assert_eq!(item_stats.items_drawn + item_stats.items_culled, 1 + 2 * 49);
    assert!(stats.triangles >= item_stats.triangles);
    // Every copy casts into a cascade or more; a cascade takes a set's
    // casters by cluster, so it may take more copies than items, never fewer.
    assert!(item_stats.shadow_casters >= 2 * 49, "{item_stats:?}");
    assert!(
        stats.shadow_casters >= item_stats.shadow_casters,
        "{} set copies cast, {} items",
        stats.shadow_casters,
        item_stats.shadow_casters
    );
    let report = imgdiff::compare(&item_image, &set_image, Tolerance::default()).unwrap();
    println!("items against the set: {}", report.summary());
    assert!(report.matches(), "{}", report.summary());

    // The ground under a crown is in the set's shadow, as under the item's.
    let set = &snapshot.instance_sets[0];
    let crown = set.transform
        * placements[24].transform()
        * set.parts[1].transform
        * Vec3::ZERO.extend(1.0);
    let shade = crown.truncate() + SUN.normalize() * (crown.y / -SUN.normalize().y);
    for scene in [&snapshot, &items] {
        render_image(&gpu, &mut renderer, scene, 320, 240);
        let probe = renderer
            .probe_sun_shadow(&gpu.device, &gpu.queue, &[shade], Vec3::Y)
            .unwrap();
        assert!(probe[0].visibility < 0.2, "{probe:?}");
    }
}

/// A row of ten 16 m clusters of four copies along +X, the camera at its
/// start looking along it.
fn row_scene(renderer: &mut Renderer) -> RenderSnapshot {
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let grey = material(renderer, [0.6, 0.6, 0.6]);
    let red = material(renderer, [0.7, 0.2, 0.2]);
    let placements: Vec<Placement> = (0..40)
        .map(|i| Placement::new(Vec3::new(4.0 * i as f32, 0.5, 0.0), Quat::IDENTITY, 1.0, i))
        .collect();
    let clusters: Vec<InstanceCluster> = placements
        .chunks(4)
        .map(|c| InstanceCluster::of(c).unwrap())
        .collect();
    let handle = renderer
        .upload_instance_set(&placements, &clusters)
        .unwrap();
    let part = |material, max_distance| InstancePart {
        mesh: cube,
        material,
        transform: Mat4::IDENTITY,
        flags: SHADOWED,
        min_distance: 0.0,
        max_distance,
    };
    let mut camera = camera(Vec3::new(-10.0, 2.0, 0.0), Vec3::new(100.0, 0.5, 0.0));
    camera.far = 500.0;
    RenderSnapshot {
        camera,
        lights: vec![sun(SUN, 1.0, false)],
        instance_sets: vec![InstanceSet {
            entity: EntityId(7),
            set: handle,
            transform: Mat4::IDENTITY,
            // The red part only nearer than 50 m: clusters 0 to 2.
            parts: vec![part(grey, 0.0), part(red, 50.0)],
            flags: SHADOWED,
        }],
        clear: CLEAR,
        ..RenderSnapshot::default()
    }
}

#[test]
fn clusters_are_culled_by_the_view_and_by_each_parts_distance_range() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let mut snapshot = row_scene(&mut renderer);
    let (_, stats) = draw(&gpu, &mut renderer, &snapshot);
    // One run, so one draw, for each part.
    assert_eq!(stats.draw_calls, 2, "{stats:?}");
    assert_eq!(stats.instance_sets, 1);
    assert_eq!((stats.set_clusters, stats.set_clusters_culled), (13, 7));
    assert_eq!(stats.set_instances, 40 + 12);
    assert_eq!(stats.triangles, 12 * 52);
    assert_eq!(
        stats.items_drawn + stats.items_culled,
        0,
        "sets are not items"
    );

    // From 30 m on, the red part draws clusters 2 to 9 instead.
    snapshot.instance_sets[0].parts[1].min_distance = 30.0;
    snapshot.instance_sets[0].parts[1].max_distance = 0.0;
    let (_, stats) = draw(&gpu, &mut renderer, &snapshot);
    assert_eq!(stats.draw_calls, 2, "{stats:?}");
    assert_eq!((stats.set_clusters, stats.set_clusters_culled), (10 + 8, 2));

    // Looking away, nothing is drawn and every cluster is culled.
    snapshot.camera.world = camera(Vec3::new(-10.0, 2.0, 0.0), Vec3::new(-100.0, 0.5, 0.0)).world;
    let (_, stats) = draw(&gpu, &mut renderer, &snapshot);
    assert_eq!(stats.draw_calls, 0);
    assert_eq!(stats.instance_sets, 0);
    assert_eq!((stats.set_clusters, stats.set_clusters_culled), (0, 20));
    assert_eq!(stats.set_instances, 0);
}

/// `per` copies in each of 100 clusters, the 64 m squares of a 640 m grid;
/// the clusters' bounds and largest scale do not depend on `per`.
fn grid_set(renderer: &mut Renderer, per: u32) -> re_types::render::InstanceSetHandle {
    let mut placements = Vec::new();
    let mut clusters = Vec::new();
    for j in 0..10 {
        for i in 0..10 {
            let corner = Vec3::new(64.0 * i as f32, 0.0, 64.0 * j as f32);
            for k in 0..per {
                let n = (j * 10 + i) * per + k;
                let at = Vec3::new(64.0 * hash(n, 5), 0.0, 64.0 * hash(n, 6));
                let rotation = Quat::from_rotation_y(hash(n, 7) * std::f32::consts::TAU);
                placements.push(Placement::new(
                    corner + at,
                    rotation,
                    0.75 + 0.5 * hash(n, 8),
                    n,
                ));
            }
            clusters.push(InstanceCluster {
                count: per,
                bounds: Aabb::new(corner, corner + Vec3::new(64.0, 0.0, 64.0)),
                max_scale: 1.25,
            });
        }
    }
    renderer
        .upload_instance_set(&placements, &clusters)
        .unwrap()
}

/// Sets of 1,000 and 100,000 copies over the same clusters make the same
/// draws and test the same clusters: a frame's work follows its clusters,
/// never its copies.
#[test]
fn sets_of_1000_and_100000_copies_over_the_same_clusters_draw_alike() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let bark = material(&mut renderer, [0.36, 0.24, 0.15]);
    let leaves = material(&mut renderer, [0.16, 0.38, 0.14]);
    let parts = vec![
        InstancePart {
            mesh: cube,
            material: bark,
            transform: Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0))
                * Mat4::from_scale(Vec3::new(0.4, 2.0, 0.4)),
            flags: SHADOWED,
            min_distance: 0.0,
            max_distance: 400.0,
        },
        InstancePart {
            mesh: cube,
            material: leaves,
            transform: Mat4::from_translation(Vec3::new(0.0, 2.5, 0.0))
                * Mat4::from_scale(Vec3::splat(2.0)),
            flags: SHADOWED,
            min_distance: 0.0,
            max_distance: 250.0,
        },
    ];
    let mut camera = camera(Vec3::new(-30.0, 40.0, -30.0), Vec3::new(200.0, 0.0, 260.0));
    camera.far = 600.0;
    let stats: Vec<FrameStats> = [10, 1000]
        .into_iter()
        .map(|per| {
            let set = grid_set(&mut renderer, per);
            let snapshot = RenderSnapshot {
                camera,
                lights: vec![sun(SUN, 1.0, true)],
                instance_sets: vec![InstanceSet {
                    entity: EntityId(3),
                    set,
                    transform: Mat4::IDENTITY,
                    parts: parts.clone(),
                    flags: SHADOWED,
                }],
                clear: CLEAR,
                ..RenderSnapshot::default()
            };
            let (_, stats) = draw(&gpu, &mut renderer, &snapshot);
            renderer.release_instance_set(set).unwrap();
            println!("{per} a cluster: {stats:?}");
            stats
        })
        .collect();
    let (small, large) = (stats[0], stats[1]);
    assert!(
        small.set_clusters > 0 && small.set_clusters_culled > 0,
        "{small:?}"
    );
    assert_eq!(small.draw_calls, large.draw_calls);
    assert_eq!(small.set_clusters, large.set_clusters);
    assert_eq!(small.set_clusters_culled, large.set_clusters_culled);
    assert_eq!(small.instance_sets, large.instance_sets);
    assert_eq!(small.shadow_cascades, large.shadow_cascades);
    assert_eq!(100 * small.set_instances, large.set_instances);
    assert_eq!(100 * small.shadow_casters, large.shadow_casters);
    assert_eq!(100 * small.triangles, large.triangles);
}

/// A highlighted cube hidden behind an opaque set stays hidden: the
/// highlight mask's depth pass draws the opaque sets too.
#[test]
fn opaque_sets_hide_occluded_highlights() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let grey = material(&mut renderer, [0.6, 0.6, 0.6]);
    let wall = [Placement::new(Vec3::ZERO, Quat::IDENTITY, 1.0, 0)];
    let handle = renderer
        .upload_instance_set(&wall, &[InstanceCluster::of(&wall).unwrap()])
        .unwrap();
    let snapshot = RenderSnapshot {
        camera: camera(Vec3::new(0.0, 1.0, 6.0), Vec3::new(0.0, 1.0, 0.0)),
        lights: vec![sun(SUN, 1.0, false)],
        // The highlighted cube, behind the wall.
        items: vec![item(
            5,
            cube,
            grey,
            Mat4::from_translation(Vec3::new(0.0, 1.0, -3.0)),
            DrawFlags::NONE,
        )],
        instance_sets: vec![InstanceSet {
            entity: EntityId(6),
            set: handle,
            transform: Mat4::IDENTITY,
            parts: vec![InstancePart {
                mesh: cube,
                material: grey,
                transform: Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0))
                    * Mat4::from_scale(Vec3::new(6.0, 4.0, 0.5)),
                flags: DrawFlags::NONE,
                min_distance: 0.0,
                max_distance: 0.0,
            }],
            flags: DrawFlags::NONE,
        }],
        clear: CLEAR,
        ..RenderSnapshot::default()
    };
    let draw_with = |renderer: &mut Renderer, extras: &SceneExtras| {
        let capture = Capture::new(&gpu.device, 160, 120).unwrap();
        renderer
            .render_with(&gpu.device, &gpu.queue, capture.view(), &snapshot, extras)
            .unwrap();
        capture.read(&gpu.device, &gpu.queue).unwrap()
    };
    let plain = draw_with(&mut renderer, &SceneExtras::default());
    let highlighted = draw_with(
        &mut renderer,
        &SceneExtras {
            highlights: vec![Highlight {
                entities: vec![EntityId(5)],
                fill: [0.0, 1.0, 0.0, 1.0],
                outline: [1.0, 0.0, 1.0, 1.0],
                always_on_top: false,
            }],
            ..SceneExtras::default()
        },
    );
    let report = imgdiff::compare(&highlighted, &plain, Tolerance::default()).unwrap();
    assert!(
        report.matches(),
        "the highlight shows through the set: {}",
        report.summary()
    );
}

/// A placement's tint multiplies its vertex colour, and `SELECTED` on the
/// set marks every copy as items mark theirs.
#[test]
fn tints_and_selection_colour_the_copies() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let cube = renderer.upload_mesh(&cube()).unwrap();
    let white = material(&mut renderer, [0.9, 0.9, 0.9]);
    let centre = |renderer: &mut Renderer, tint: [f32; 3], flags: DrawFlags| {
        let one = [Placement::new(Vec3::ZERO, Quat::IDENTITY, 2.0, 0).with_tint(tint)];
        let handle = renderer
            .upload_instance_set(&one, &[InstanceCluster::of(&one).unwrap()])
            .unwrap();
        let snapshot = RenderSnapshot {
            camera: camera(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO),
            lights: vec![sun(Vec3::new(0.0, 0.0, -1.0), 1.0, false)],
            instance_sets: vec![InstanceSet {
                entity: EntityId(1),
                set: handle,
                transform: Mat4::IDENTITY,
                parts: vec![InstancePart {
                    mesh: cube,
                    material: white,
                    transform: Mat4::IDENTITY,
                    flags: DrawFlags::NONE,
                    min_distance: 0.0,
                    max_distance: 0.0,
                }],
                flags,
            }],
            clear: CLEAR,
            ..RenderSnapshot::default()
        };
        let image = render_image(&gpu, renderer, &snapshot, 64, 64);
        renderer.release_instance_set(handle).unwrap();
        image.pixel(32, 32)
    };
    let plain = centre(&mut renderer, [1.0; 3], DrawFlags::NONE);
    let tinted = centre(&mut renderer, [1.0, 0.25, 0.25], DrawFlags::NONE);
    let selected = centre(&mut renderer, [1.0; 3], DrawFlags::SELECTED);
    println!("plain {plain:?}, tinted {tinted:?}, selected {selected:?}");
    assert!(
        tinted[0].abs_diff(plain[0]) <= 1 && tinted[1] + 40 < plain[1] && tinted[2] + 40 < plain[2],
        "{plain:?} {tinted:?}"
    );
    assert_ne!(selected, plain);
}

/// Editing `instances.wgsl` rebuilds the three set pipelines' roots and the
/// forest draws as before.
#[test]
fn the_set_shaders_reload() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let (snapshot, _) = forest_scene(&mut renderer);
    let (before, _) = draw(&gpu, &mut renderer, &snapshot);
    let source = re_render::shader::SOURCES
        .iter()
        .find(|(name, _)| *name == "instances.wgsl")
        .unwrap()
        .1;
    renderer.set_shader_source("instances.wgsl", Some(format!("{source}\n// edited\n")));
    let report = renderer.reload_shaders();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    let mut rebuilt = report.rebuilt.clone();
    rebuilt.sort_unstable();
    assert_eq!(
        rebuilt,
        ["forward_set.wgsl", "mask_set.wgsl", "shadow_set.wgsl"]
    );
    let (after, _) = draw(&gpu, &mut renderer, &snapshot);
    assert_eq!(after, before);
}

#[test]
fn malformed_sets_and_stale_handles_are_reported() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let code =
        |r: Result<re_types::render::InstanceSetHandle, re_core::Diagnostic>| r.unwrap_err().code;
    assert_eq!(
        code(renderer.upload_instance_set(&[], &[])),
        codes::BAD_INSTANCE_SET
    );
    let (placements, mut clusters) = forest_placements(4);
    clusters[2].count -= 1;
    assert_eq!(
        code(renderer.upload_instance_set(&placements, &clusters)),
        codes::BAD_INSTANCE_SET
    );
    clusters[2].count += 1;
    let mut far = placements.clone();
    far[0].position[0] += 100.0;
    assert_eq!(
        code(renderer.upload_instance_set(&far, &clusters)),
        codes::BAD_INSTANCE_SET,
        "a copy outside its cluster"
    );
    assert_eq!(renderer.stats().instance_sets.count, 0);

    let set = renderer
        .upload_instance_set(&placements, &clusters)
        .unwrap();
    let stats = renderer.stats().instance_sets;
    assert_eq!((stats.count, stats.bytes), (1, 32 * 16));

    // A part whose mesh was released is skipped with RE6010; the set's other
    // part still draws.
    let (mut snapshot, _) = forest_scene(&mut renderer);
    snapshot.instance_sets[0].set = set;
    let gone = renderer.upload_mesh(&cube()).unwrap();
    renderer.release_mesh(gone).unwrap();
    snapshot.instance_sets[0].parts[0].mesh = gone;
    let capture = Capture::new(&gpu.device, 64, 48).unwrap();
    let report = renderer
        .render(&gpu.device, &gpu.queue, capture.view(), &snapshot)
        .unwrap();
    let warned: Vec<_> = report.warnings.iter().map(|w| w.code).collect();
    assert_eq!(warned, [codes::STALE_MESH], "{:?}", report.warnings);
    assert!(
        report.warnings[0]
            .message
            .contains("an instance set of entity 2")
    );
    assert_eq!(report.stats.items_skipped, 1);
    assert_eq!(report.stats.set_instances, 16, "the crowns");

    // A released set is skipped and warned about once a frame with RE6104,
    // however many sets name it; releasing it again fails with RE6104.
    assert_eq!(renderer.stats().instance_sets.count, 2, "with the scene's");
    renderer.release_instance_set(set).unwrap();
    assert_eq!(renderer.stats().instance_sets.count, 1);
    assert_eq!(
        renderer.release_instance_set(set).unwrap_err().code,
        codes::STALE_INSTANCE_SET
    );
    let twice = snapshot.instance_sets[0].clone();
    snapshot.instance_sets.push(twice);
    for _ in 0..2 {
        let report = renderer
            .render(&gpu.device, &gpu.queue, capture.view(), &snapshot)
            .unwrap();
        let warned: Vec<_> = report.warnings.iter().map(|w| w.code).collect();
        assert_eq!(warned, [codes::STALE_INSTANCE_SET], "{:?}", report.warnings);
        assert_eq!(report.stats.items_skipped, 2);
        assert_eq!(report.stats.instance_sets, 0);
    }
}

/// Every set pipeline exists on GL too: a set draws there with no GPU error.
#[test]
fn a_set_draws_without_gpu_errors() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let (snapshot, _) = forest_scene(&mut renderer);
    draw(&gpu, &mut renderer, &snapshot);
    assert_eq!(gpu.errors().count(), 0, "{:?}", gpu.errors().take());
}
