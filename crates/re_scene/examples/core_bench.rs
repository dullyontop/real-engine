//! The core spike's 100,100-entity benchmark (100 roots x 10 blocks x 99
//! leaves). Run with
//!
//! ```sh
//! cargo run --locked -p re_scene --release --example core_bench -- --check
//! ```
//!
//! With `--check` it exits with status 1 if any measurement exceeds three
//! times the spike's figure (docs/M1_PLAN.md, WP0 acceptance) or if the JSON
//! round trip is not byte-identical.

use std::process::ExitCode;
use std::time::Duration;

use glam::Vec3;
use indexmap::IndexMap;
use re_command::{History, Op, Origin, Transaction};
use re_core::time::Instant;
use re_reflect::{Color, Doc, Value, component};
use re_scene::ecs as bevy_ecs;
use re_scene::scene::{self, SaveOptions};
use re_scene::{Entity, Name, StableId, Transform, World};

component! {
    /// A solid primitive shape (the spike's leaf component).
    pub struct Primitive {
        /// Size along each local axis, in metres.
        pub size: Vec3 = Vec3::ONE,
        /// The colour.
        pub color: Color = Color::rgba(0.64, 0.64, 0.64, 1.0),
        /// Whether it is drawn.
        pub visible: bool = true,
    }
}

const ROOTS: usize = 100;
const BLOCKS: usize = 10;
const LEAVES: usize = 99;

fn new_world() -> World {
    let mut w = World::new();
    w.registry.register::<Primitive>(&mut w.ecs);
    w
}

fn median(mut f: impl FnMut() -> Duration, runs: usize) -> Duration {
    let mut v: Vec<Duration> = (0..runs).map(|_| f()).collect();
    v.sort();
    v[runs / 2]
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed())
}

/// 100 roots x 10 blocks x 99 leaves = 100,100 entities, spawned directly
/// with typed bundles (the id index is kept by the `StableId` hooks).
fn build_direct(w: &mut World) {
    for r in 0..ROOTS {
        let root = w
            .ecs
            .spawn((
                StableId(format!("z{r}")),
                Name(format!("Zone {r}")),
                Transform::from_translation(Vec3::new(r as f32 * 100.0, 0.0, 0.0)),
            ))
            .id();
        for b in 0..BLOCKS {
            let block = w
                .ecs
                .spawn((
                    StableId(format!("z{r}b{b}")),
                    Name("Block".into()),
                    Transform::from_translation(Vec3::new(0.0, 0.0, b as f32 * 10.0)),
                ))
                .id();
            w.ecs.entity_mut(root).add_child(block);
            for l in 0..LEAVES {
                let leaf = w
                    .ecs
                    .spawn((
                        StableId(format!("z{r}b{b}l{l}")),
                        Name("Leaf".into()),
                        Transform::from_translation(Vec3::new(l as f32, 1.0, 0.0)),
                        Primitive::default(),
                    ))
                    .id();
                w.ecs.entity_mut(block).add_child(leaf);
            }
        }
    }
}

fn build_ops() -> Vec<Op> {
    let mut out = Vec::with_capacity(ROOTS * (1 + BLOCKS * (1 + LEAVES)));
    let t = |x: f32, y: f32, z: f32| {
        IndexMap::from([(
            "Transform".to_string(),
            IndexMap::from([(
                "translation".to_string(),
                Doc::List(vec![Doc::F32(x), Doc::F32(y), Doc::F32(z)]),
            )]),
        )])
    };
    for r in 0..ROOTS {
        out.push(Op::Spawn {
            id: Some(format!("z{r}")),
            name: format!("Zone {r}"),
            parent: None,
            index: None,
            components: t(r as f32 * 100.0, 0.0, 0.0),
        });
        for b in 0..BLOCKS {
            out.push(Op::Spawn {
                id: Some(format!("z{r}b{b}")),
                name: "Block".into(),
                parent: Some(format!("z{r}")),
                index: None,
                components: t(0.0, 0.0, b as f32 * 10.0),
            });
            for l in 0..LEAVES {
                let mut c = t(l as f32, 1.0, 0.0);
                c.insert("Primitive".into(), IndexMap::new());
                out.push(Op::Spawn {
                    id: Some(format!("z{r}b{b}l{l}")),
                    name: "Leaf".into(),
                    parent: Some(format!("z{r}b{b}")),
                    index: None,
                    components: c,
                });
            }
        }
    }
    out
}

struct Report {
    rows: Vec<(&'static str, Duration, Duration, Duration)>,
}

impl Report {
    /// Records a measurement with the spike's figure (in microseconds).
    fn row(&mut self, name: &'static str, measured: Duration, spike_us: u64) {
        let spike = Duration::from_micros(spike_us);
        let limit = spike * 3;
        println!(
            "{name:<52} {:>11} (spike {:>9}, limit {:>9}){}",
            fmt(measured),
            fmt(spike),
            fmt(limit),
            if measured > limit { "  OVER" } else { "" }
        );
        self.rows.push((name, measured, spike, limit));
    }
}

fn fmt(d: Duration) -> String {
    if d >= Duration::from_secs(1) {
        format!("{:.2} s", d.as_secs_f64())
    } else {
        format!("{:.2} ms", d.as_secs_f64() * 1e3)
    }
}

fn main() -> ExitCode {
    let check = std::env::args().any(|a| a == "--check");
    let mut report = Report { rows: Vec::new() };
    println!("entities: {}", ROOTS * (1 + BLOCKS * (1 + LEAVES)));

    // Spawn.
    let spawn_direct = median(
        || {
            let mut w = new_world();
            time(|| build_direct(&mut w)).1
        },
        5,
    );
    report.row("direct spawn with ids and hooks", spawn_direct, 173_000);
    let ops = build_ops();
    let spawn_ops = median(
        || {
            let mut w = new_world();
            let mut h = History::default();
            let tx = Transaction::new("bulk", Origin::User, ops.clone());
            time(|| h.execute(&mut w, tx).unwrap()).1
        },
        3,
    );
    report.row(
        "100,100 spawn operations in one transaction",
        spawn_ops,
        1_030_000,
    );

    let mut w = new_world();
    build_direct(&mut w);

    // Propagation.
    w.propagate_all(); // warm up, initialise system state
    let t_full = median(|| time(|| w.propagate_all()).1, 11);
    report.row("propagate_all", t_full, 5_500);

    w.update(); // the first incremental run sees everything as added
    let t_idle = median(
        || {
            time(|| {
                w.update();
            })
            .1
        },
        11,
    );
    report.row("incremental update, nothing changed", t_idle, 420);

    let leaf_ids: Vec<Entity> = (0..1000)
        .map(|i| {
            w.entity(&format!(
                "z{}b{}l{}",
                i % ROOTS,
                (i / ROOTS) % BLOCKS,
                i % LEAVES
            ))
            .unwrap()
        })
        .collect();
    let mut n = 0.0;
    let t_leaves = median(
        || {
            n += 1.0;
            for &e in &leaf_ids {
                w.ecs.get_mut::<Transform>(e).unwrap().translation.y = n;
            }
            time(|| {
                w.update();
            })
            .1
        },
        11,
    );
    report.row(
        "incremental update after moving 1,000 leaves",
        t_leaves,
        990,
    );

    let root_ids: Vec<Entity> = (0..10)
        .map(|i| w.entity(&format!("z{i}")).unwrap())
        .collect();
    let t_roots = median(
        || {
            n += 1.0;
            for &e in &root_ids {
                w.ecs.get_mut::<Transform>(e).unwrap().translation.y = n;
            }
            time(|| {
                w.update();
            })
            .1
        },
        11,
    );
    report.row("incremental update after moving 10 roots", t_roots, 1_360);

    // By-name access.
    const N: usize = 1_000_000;
    let ents: Vec<Entity> = w
        .ecs
        .query::<(Entity, &Primitive)>()
        .iter(&w.ecs)
        .map(|(e, _)| e)
        .take(1000)
        .collect();
    let ids: Vec<String> = ents
        .iter()
        .map(|e| w.id_of(*e).unwrap().to_string())
        .collect();
    let (sum, t) = time(|| {
        let mut s = 0.0f32;
        for i in 0..N {
            if let Ok(Value::Vec3(v)) = w.get(&ids[i % 1000], "Transform", "translation") {
                s += v.x;
            }
        }
        s
    });
    std::hint::black_box(sum);
    report.row("1M by-name reads (World::get)", t, 107_000);
    let key = w.registry.key("Transform").unwrap();
    let fi = w.registry.schema(key).field_index("translation").unwrap();
    let (_, t) = time(|| {
        for i in 0..N {
            w.registry
                .set_field(
                    &mut w.ecs,
                    ents[i % 1000],
                    key,
                    fi,
                    Value::Vec3(Vec3::new(i as f32, 0.0, 0.0)),
                )
                .unwrap();
        }
    });
    report.row("1M set_field with a resolved key", t, 36_000);

    // Scene save and load.
    let (doc, t) = time(|| w.save(SaveOptions::default()));
    report.row("save to SceneDoc", t, 299_000);
    let (json, t) = time(|| scene::to_json(&doc));
    report.row("SceneDoc to JSON", t, 632_000);
    println!("    JSON size: {:.1} MB", json.len() as f64 / 1e6);
    let (parsed, t) = time(|| scene::from_json(&json).unwrap());
    report.row("JSON to SceneDoc", t, 477_000);
    let mut fresh = new_world();
    let (_, t) = time(|| fresh.load(&parsed).unwrap());
    report.row("load SceneDoc into the World", t, 486_000);
    let identical = scene::to_json(&fresh.save(SaveOptions::default())) == json;
    println!(
        "    JSON round trip byte-identical: {}",
        if identical { "yes" } else { "NO" }
    );

    // An undoable structural edit on a big subtree.
    let mut h = History::default();
    let (_, t) = time(|| {
        h.execute(
            &mut w,
            Transaction::new(
                "delete zone",
                Origin::User,
                vec![Op::Despawn { id: "z5".into() }],
            ),
        )
        .unwrap()
    });
    report.row("despawn of a 1,001-entity subtree", t, 6_900);
    let (_, t) = time(|| h.undo(&mut w).unwrap());
    report.row("undo of that despawn", t, 10_200);

    let over: Vec<_> = report.rows.iter().filter(|r| r.1 > r.3).collect();
    if check {
        if !identical {
            eprintln!("check failed: the JSON round trip is not byte-identical");
            return ExitCode::FAILURE;
        }
        if !over.is_empty() {
            for (name, measured, _, limit) in &over {
                eprintln!("check failed: {name}: {} > {}", fmt(*measured), fmt(*limit));
            }
            return ExitCode::FAILURE;
        }
        println!("check passed: every measurement is within 3x of the spike");
    }
    ExitCode::SUCCESS
}
