//! The forest benchmark (ADR-0052 §7): trees, each a bark trunk and a leaf
//! crown, drawn as instance sets or as the same trees as draw items, on a
//! flat ground of 256 m chunks with a shadowed sun and the post chain, while
//! the camera drives along a lane through the forest at 15 m/s. It reports the
//! renderer's time per frame as `PerfReport` names it (`render_submit`:
//! preparing and recording a frame; `queue_submit`: inside the queue's
//! submit), the last frame's statistics and the GPU memory.
//!
//! ```text
//! cargo run --release --locked -p re_render --example forest -- --instances 100000 --json
//! ```
//!
//! * `--mode sets|items` (`sets`): one instance set per 256 m cell, or a draw
//!   item per trunk and per crown.
//! * `--instances N` (100,000): the trees.
//! * `--cluster M` (64): the side of a set's square clusters, in metres.
//! * `--area M`: the side of the square the trees fill, in metres; about
//!   30 m² per tree, and at least one cell.
//! * `--max-distance M` (0, none): the parts' draw distance.
//! * `--frames F` measured after `--warmup W` frames: by default 30 after 5
//!   up to 10,000 trees, 10 after 2 up to 100,000 and 5 after 1 above, since
//!   lavapipe takes about 20 s to draw a frame of 100,000 trees.
//! * `--size WxH` (1280x720), `--no-shadows`, `--png PATH` (the first measured
//!   frame) and `--json` (the report as JSON).

use std::collections::BTreeMap;
use std::f32::consts::{PI, TAU};
use std::time::Duration;

use glam::{Mat4, Quat, Vec3};
use re_core::time::Instant;
use re_render::{Capture, FrameStats, Gpu, Renderer, wgpu};
use re_types::EntityId;
use re_types::mesh::{MeshData, Vertex};
use re_types::render::{
    Bloom, Camera, ColorGrading, DrawFlags, DrawItem, InstanceCluster, InstancePart, InstanceSet,
    Light, LightKind, MaterialDesc, Placement, PostSettings, RenderSnapshot, SunShadows,
    ToneMapping,
};

/// The side of a streamed cell, whose corner is a set's origin.
const CELL: f32 = 256.0;

fn vertex(pos: Vec3, normal: Vec3, uv: [f32; 2]) -> Vertex {
    Vertex {
        pos: pos.to_array(),
        normal: normal.to_array(),
        uv,
        color: [1.0; 4],
    }
}

fn quad(mesh: &mut MeshData, c: Vec3, u: Vec3, v: Vec3) {
    let n = u.cross(v).normalize();
    let base = mesh.vertices.len() as u32;
    for (p, uv) in [
        (c - u - v, [0.0, 1.0]),
        (c + u - v, [1.0, 1.0]),
        (c + u + v, [1.0, 0.0]),
        (c - u + v, [0.0, 0.0]),
    ] {
        mesh.vertices.push(vertex(p, n, uv));
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// A unit cube, as `builtin:cube`.
fn cube() -> MeshData {
    let mut mesh = MeshData::default();
    for (n, u, v) in [
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
    ] {
        quad(&mut mesh, n * 0.5, u * 0.5, v * 0.5);
    }
    mesh
}

/// A sphere of radius 0.5, 24 segments by 16 rings, as `builtin:sphere`.
fn sphere() -> MeshData {
    let (segments, rings) = (24u32, 16u32);
    let mut mesh = MeshData::default();
    for r in 0..=rings {
        let phi = PI * r as f32 / rings as f32;
        for s in 0..=segments {
            let theta = TAU * s as f32 / segments as f32;
            let n = Vec3::new(
                libm::sinf(phi) * libm::cosf(theta),
                libm::cosf(phi),
                libm::sinf(phi) * libm::sinf(theta),
            );
            mesh.vertices.push(vertex(
                n * 0.5,
                n,
                [s as f32 / segments as f32, r as f32 / rings as f32],
            ));
        }
    }
    let stride = segments + 1;
    for row in 0..rings {
        for s in 0..segments {
            let a = row * stride + s;
            let b = a + stride;
            mesh.indices
                .extend_from_slice(&[a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    mesh
}

/// A flat square of `side` metres from the origin along +X and +Z, `n` cells
/// a side.
fn ground(side: f32, n: u32) -> MeshData {
    let mut mesh = MeshData::default();
    for z in 0..=n {
        for x in 0..=n {
            let p = Vec3::new(x as f32, 0.0, z as f32) * (side / n as f32);
            mesh.vertices.push(vertex(
                p,
                Vec3::Y,
                [x as f32 / n as f32, z as f32 / n as f32],
            ));
        }
    }
    let stride = n + 1;
    for z in 0..n {
        for x in 0..n {
            let a = z * stride + x;
            let (b, c, d) = (a + 1, a + stride, a + stride + 1);
            mesh.indices.extend_from_slice(&[a, c, d, a, d, b]);
        }
    }
    mesh
}

/// SplitMix64: the same trees on every run and platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in 0..1.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

struct Tree {
    position: Vec3,
    rotation: Quat,
    scale: f32,
}

/// `n` trees on a jittered grid filling a square of `area` metres centred on
/// the origin, keeping a lane of `lane` metres clear along X at z = 0.
fn scatter(n: usize, area: f32, lane: f32) -> Vec<Tree> {
    let keep = 1.0 - (lane / area).min(0.5);
    let k = ((n as f32 / keep).sqrt().ceil() as usize).max(1) + 1;
    let spacing = area / k as f32;
    let mut rng = Rng(0x5eed);
    let mut trees = Vec::with_capacity(n);
    'rows: for j in 0..k {
        for i in 0..k {
            let x = -area / 2.0 + (i as f32 + 0.15 + 0.7 * rng.unit()) * spacing;
            let z = -area / 2.0 + (j as f32 + 0.15 + 0.7 * rng.unit()) * spacing;
            let yaw = rng.unit() * TAU;
            let scale = 0.8 + 0.5 * rng.unit();
            if z.abs() < lane / 2.0 {
                continue;
            }
            trees.push(Tree {
                position: Vec3::new(x, 0.0, z),
                rotation: Quat::from_rotation_y(yaw),
                scale,
            });
            if trees.len() == n {
                break 'rows;
            }
        }
    }
    trees
}

/// A cell, as its x and z on the grid of cells.
type Cell = (i32, i32);

/// The cell holding `p`.
fn cell_of(p: Vec3) -> Cell {
    (
        libm::floorf(p.x / CELL) as i32,
        libm::floorf(p.z / CELL) as i32,
    )
}

/// The corner of `cell`, its set's origin.
fn corner(cell: Cell) -> Vec3 {
    Vec3::new(cell.0 as f32 * CELL, 0.0, cell.1 as f32 * CELL)
}

/// A cell's instance set before upload.
struct CellSet {
    corner: Vec3,
    /// From the corner, in cluster order.
    placements: Vec<Placement>,
    clusters: Vec<InstanceCluster>,
}

/// One set per cell, its placements in square clusters of `cluster` metres
/// (rows by z then x), as a producer makes them.
fn sets_by_cell(trees: &[Tree], cluster: f32) -> Vec<CellSet> {
    let side = if cluster > 0.0 {
        cluster.min(CELL)
    } else {
        CELL
    };
    // By cell, then by cluster: its z and x on the cell's grid of clusters.
    let mut members: BTreeMap<(Cell, (i32, i32)), Vec<Placement>> = BTreeMap::new();
    for (id, t) in trees.iter().enumerate() {
        let cell = cell_of(t.position);
        let local = t.position - corner(cell);
        let key = (
            libm::floorf(local.z / side) as i32,
            libm::floorf(local.x / side) as i32,
        );
        members
            .entry((cell, key))
            .or_default()
            .push(Placement::new(local, t.rotation, t.scale, id as u32));
    }
    let mut sets: Vec<(Cell, CellSet)> = Vec::new();
    for ((cell, _), placements) in members {
        if sets.last().is_none_or(|(c, _)| *c != cell) {
            sets.push((
                cell,
                CellSet {
                    corner: corner(cell),
                    placements: Vec::new(),
                    clusters: Vec::new(),
                },
            ));
        }
        let (_, set) = sets.last_mut().expect("pushed above");
        set.clusters
            .push(InstanceCluster::of(&placements).expect("a cluster has members"));
        set.placements.extend(placements);
    }
    sets.into_iter().map(|(_, set)| set).collect()
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn number<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> T {
    arg(args, name).map_or(default, |s| {
        s.parse()
            .unwrap_or_else(|_| panic!("{name} takes a number, not {s:?}"))
    })
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The mean, median, 95th percentile and largest of `v`, in milliseconds
/// rounded to microseconds.
fn summary(v: &mut [f64]) -> serde_json::Value {
    if v.is_empty() {
        return serde_json::Value::Null;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let p95 = v[((0.95 * n as f64).ceil() as usize).clamp(1, n) - 1];
    let r = |x: f64| (x * 1000.0).round() / 1000.0;
    serde_json::json!({
        "mean": r(v.iter().sum::<f64>() / n as f64),
        "p50": r(v[n / 2]),
        "p95": r(p95),
        "max": r(v[n - 1]),
    })
}

fn frame_json(s: &FrameStats) -> serde_json::Value {
    serde_json::json!({
        "draw_calls": s.draw_calls,
        "triangles": s.triangles,
        "items_drawn": s.items_drawn,
        "items_culled": s.items_culled,
        "shadow_cascades": s.shadow_cascades,
        "shadow_casters": s.shadow_casters,
        "instance_sets": s.instance_sets,
        "set_instances": s.set_instances,
        "set_clusters": s.set_clusters,
        "set_clusters_culled": s.set_clusters_culled,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = arg(&args, "--mode").unwrap_or("sets").to_string();
    assert!(
        mode == "sets" || mode == "items",
        "--mode is sets or items, not {mode:?}"
    );
    let n: usize = number(&args, "--instances", 100_000);
    let cluster: f32 = number(&args, "--cluster", 64.0);
    let (frames, warmup) = match n {
        0..=10_000 => (30, 5),
        10_001..=100_000 => (10, 2),
        _ => (5, 1),
    };
    let frames: usize = number(&args, "--frames", frames).max(1);
    let warmup: usize = number(&args, "--warmup", warmup);
    let max_distance: f32 = number(&args, "--max-distance", 0.0);
    let shadows = !args.iter().any(|a| a == "--no-shadows");
    let json = args.iter().any(|a| a == "--json");
    // About one tree per 30 m², as a dense valley forest; at least one cell.
    let area: f32 = number(&args, "--area", libm::sqrtf(n as f32 * 30.0).max(CELL));
    let (width, height) = arg(&args, "--size").map_or((1280, 720), |s| {
        let (w, h) = s.split_once('x').expect("--size is WIDTHxHEIGHT");
        (w.parse().unwrap(), h.parse().unwrap())
    });
    let png = arg(&args, "--png").map(std::path::PathBuf::from);

    let gpu = Gpu::headless(re_render::backends_from_env().unwrap())
        .unwrap_or_else(|d| panic!("{}", d.render_human()));
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let capture = Capture::new(&gpu.device, width, height).unwrap();

    let trunk_mesh = renderer.upload_mesh(&cube()).unwrap();
    let crown_mesh = renderer.upload_mesh(&sphere()).unwrap();
    let ground_mesh = renderer.upload_mesh(&ground(CELL, 32)).unwrap();
    let material = |rgb: [f32; 3], roughness: f32| MaterialDesc {
        base_color: [rgb[0], rgb[1], rgb[2], 1.0],
        roughness,
        ..MaterialDesc::default()
    };
    let bark = renderer
        .create_material(&material([0.36, 0.24, 0.15], 0.9))
        .unwrap();
    let leaves = renderer
        .create_material(&material([0.16, 0.38, 0.14], 0.8))
        .unwrap();
    let grass = renderer
        .create_material(&material([0.36, 0.52, 0.22], 0.92))
        .unwrap();
    let flags = DrawFlags::CAST_SHADOWS | DrawFlags::RECEIVE_SHADOWS;
    // The valley's tree prefab: the crown is the trunk's child.
    let trunk = Mat4::from_translation(Vec3::new(0.0, 1.6, 0.0))
        * Mat4::from_scale(Vec3::new(0.5, 3.2, 0.5));
    let crown = trunk
        * Mat4::from_translation(Vec3::new(0.0, 0.85, 0.0))
        * Mat4::from_scale(Vec3::new(6.0, 0.9, 6.0));
    let parts = vec![
        InstancePart {
            mesh: trunk_mesh,
            material: bark,
            transform: trunk,
            flags,
            min_distance: 0.0,
            max_distance,
        },
        InstancePart {
            mesh: crown_mesh,
            material: leaves,
            transform: crown,
            flags,
            min_distance: 0.0,
            max_distance,
        },
    ];

    let mut snapshot = RenderSnapshot {
        clear: [0.55, 0.7, 0.9, 1.0],
        post: PostSettings {
            tone_mapping: ToneMapping::PbrNeutral,
            bloom: Bloom {
                intensity: 0.08,
                threshold: 1.0,
                ..Bloom::default()
            },
            grading: Some(ColorGrading {
                saturation: 0.1,
                ..ColorGrading::default()
            }),
            ..PostSettings::default()
        },
        shadows: SunShadows {
            cascades: 4,
            distance: 200.0,
            split_lambda: 0.85,
            ..SunShadows::default()
        },
        ..RenderSnapshot::default()
    };
    // The valley's sun.
    let sun = Quat::from_xyzw(-0.502_518_9, 0.301_511_35, 0.100_503_79, 0.804_030_3);
    snapshot.lights.push(Light {
        kind: LightKind::Directional,
        direction: (sun * Vec3::NEG_Z).normalize(),
        intensity: 1.2,
        shadows,
        ..Light::default()
    });
    // A ground chunk per cell, as draw items in both modes.
    let cells_side = (area / CELL).ceil() as i32 + 1;
    let first_cell = -(cells_side / 2);
    for cz in first_cell..first_cell + cells_side {
        for cx in first_cell..first_cell + cells_side {
            snapshot.items.push(DrawItem {
                entity: EntityId(1),
                mesh: ground_mesh,
                material: grass,
                world: Mat4::from_translation(Vec3::new(cx as f32 * CELL, 0.0, cz as f32 * CELL)),
                flags: DrawFlags::RECEIVE_SHADOWS,
            });
        }
    }

    let trees = scatter(n, area, 8.0);
    let setup = Instant::now();
    let mut upload = Duration::ZERO;
    let mut clusters = 0;
    if mode == "items" {
        for (i, t) in trees.iter().enumerate() {
            let root =
                Mat4::from_scale_rotation_translation(Vec3::splat(t.scale), t.rotation, t.position);
            for (p, part) in parts.iter().enumerate() {
                snapshot.items.push(DrawItem {
                    entity: EntityId(10 + 2 * i as u64 + p as u64),
                    mesh: part.mesh,
                    material: part.material,
                    world: root * part.transform,
                    flags,
                });
            }
        }
    } else {
        for cell in sets_by_cell(&trees, cluster) {
            clusters += cell.clusters.len();
            let start = Instant::now();
            let set = renderer
                .upload_instance_set(&cell.placements, &cell.clusters)
                .unwrap_or_else(|d| panic!("{}", d.render_human()));
            upload += start.elapsed();
            snapshot.instance_sets.push(InstanceSet {
                entity: EntityId(2),
                set,
                transform: Mat4::from_translation(cell.corner),
                parts: parts.clone(),
                flags,
            });
        }
    }
    let setup = setup.elapsed();

    // Along the lane at 15 m/s, looking ahead with a slow sway.
    let camera_at = |f: usize| {
        let t = f as f32 / 60.0;
        let x = -area / 2.0 + 30.0 + 15.0 * t;
        let yaw = -PI / 2.0 + 0.35 * libm::sinf(t * 0.4);
        Camera {
            world: Mat4::from_rotation_translation(
                Quat::from_rotation_y(yaw) * Quat::from_rotation_x(-0.04),
                Vec3::new(x, 1.8, 0.0),
            ),
            fov_y_radians: 60f32.to_radians(),
            near: 0.1,
            far: 6000.0,
        }
    };

    let (mut render_submit, mut queue_submit, mut frame) = (vec![], vec![], vec![]);
    let mut last = FrameStats::default();
    for f in 0..warmup + frames {
        snapshot.camera = camera_at(f);
        let start = Instant::now();
        let report = renderer
            .render(&gpu.device, &gpu.queue, capture.view(), &snapshot)
            .unwrap_or_else(|d| panic!("{}", d.render_human()));
        gpu.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .unwrap();
        let elapsed = start.elapsed();
        let drawn = renderer.take_draw_time();
        if f == 0 {
            for w in &report.warnings {
                eprintln!("{}", w.render_human());
            }
        }
        if f == warmup
            && let Some(path) = &png
        {
            let image = capture.read(&gpu.device, &gpu.queue).unwrap();
            image
                .save_png(path)
                .unwrap_or_else(|d| panic!("{}", d.render_human()));
        }
        if f >= warmup {
            render_submit.push(ms(drawn.encode));
            queue_submit.push(ms(drawn.submit));
            frame.push(ms(elapsed));
            last = report.stats;
        }
    }
    let resources = renderer.stats();
    let report = serde_json::json!({
        "mode": mode,
        "instances": trees.len(),
        "area_m": area,
        "cluster_m": cluster,
        "max_distance_m": max_distance,
        "shadows": shadows,
        "size": [width, height],
        "adapter": gpu.adapter_line(),
        "frames": frames,
        "warmup": warmup,
        "render_submit_ms": summary(&mut render_submit),
        "queue_submit_ms": summary(&mut queue_submit),
        "frame_ms": summary(&mut frame),
        "setup_ms": (ms(setup) * 1000.0).round() / 1000.0,
        "set_upload_ms": (ms(upload) * 1000.0).round() / 1000.0,
        "sets": snapshot.instance_sets.len(),
        "clusters": clusters,
        "snapshot_items": snapshot.items.len(),
        "gpu_bytes": {
            "meshes": resources.meshes.bytes,
            "instance_sets": resources.instance_sets.bytes,
        },
        "last_frame": frame_json(&last),
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        let mean = |key: &str| report[key]["mean"].as_f64().unwrap_or(0.0);
        println!(
            "{mode}: {} trees on {area} m ({} sets, {clusters} clusters) on {}",
            trees.len(),
            snapshot.instance_sets.len(),
            gpu.adapter_line()
        );
        println!(
            "render_submit {:.3} ms, queue_submit {:.3} ms, frame {:.3} ms (means of {frames} frames)",
            mean("render_submit_ms"),
            mean("queue_submit_ms"),
            mean("frame_ms")
        );
        println!(
            "last frame: {} draws, {} triangles, {} set instances in {} clusters ({} culled), {} shadow casters",
            last.draw_calls,
            last.triangles,
            last.set_instances,
            last.set_clusters,
            last.set_clusters_culled,
            last.shadow_casters
        );
    }
}
