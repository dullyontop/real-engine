//! Streaming tests (ADR-0042). Names start with `stream` where the module
//! path does not already say it, so `cargo test -p re_proc stream` runs them.

use std::collections::BTreeMap;
use std::f32::consts::FRAC_1_SQRT_2;
use std::path::PathBuf;
use std::sync::Arc;

use glam::{Quat, Vec2, Vec3};
use re_command::EditState;
use re_reflect::Value;
use re_scene::World;
use re_types::graph::GraphDoc;
use serde_json::{Value as Json, json};

use super::*;
use crate::{Domain, EvalOptions, Evaluator, GraphStore, Output, ProcGraph, register_generator};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn graph(nodes: &[(&str, &str, Json)], edges: &[(&str, &str, &str, &str)]) -> GraphDoc {
    let doc = json!({
        "format": "real-engine/graph",
        "version": 1,
        "name": "world",
        "seed": 7,
        "nodes": nodes.iter().map(|(id, kind, params)| json!({
            "id": id, "kind": kind, "params": params,
            "version": crate::NodeRegistry::m1().get(kind).map_or(1, |d| d.version),
        })).collect::<Vec<_>>(),
        "edges": edges.iter().map(|(f, fp, t, tp)| json!({
            "from": [f, fp], "to": [t, tp],
        })).collect::<Vec<_>>(),
    });
    GraphDoc::from_json(&doc.to_string()).expect("test graph parses")
}

/// Terrain with a collider, water, and crates on a jittered grid: about 50
/// entities per 64 m cell.
fn world_graph() -> GraphDoc {
    graph(
        &[
            (
                "h",
                "terrain.heightfield",
                json!({ "cell": 4.0, "height": 40.0, "scale": 0.01 }),
            ),
            ("ground", "terrain.mesh", json!({ "collider": true })),
            ("water", "terrain.water_plane", json!({ "level": 3.0 })),
            (
                "grid",
                "scatter.grid_jitter",
                json!({ "spacing": 16.0, "jitter": 0.8 }),
            ),
            (
                "crates",
                "points.instance",
                json!({ "prefab": "prefabs/crate.prefab.json", "random_yaw": true }),
            ),
        ],
        &[
            ("h", "field", "ground", "field"),
            ("h", "field", "grid", "field"),
            ("grid", "points", "crates", "points"),
        ],
    )
}

fn config() -> StreamConfig {
    StreamConfig {
        cell_size: 64.0,
        radius: 64.0,
        radius_ahead: 128.0,
        unload_margin: 32.0,
        delay_ticks: 3,
        apply_entities: 400,
        ..StreamConfig::default()
    }
}

/// A world with the streamed source `terrain` (bounds 256 m) and a focus
/// `player` at `(x, z)` looking down -Z.
fn streamed(doc: GraphDoc, x: f32, z: f32, extra: Json) -> EditState {
    let mut st = EditState::new();
    register_generator(&mut st.world.generators);
    let mut store = GraphStore::with_evaluator(Arc::new(Evaluator::default()), Some(fixtures()));
    store.insert("terrain", doc);
    st.world.ecs.insert_resource(store);
    let mut entities = vec![
        json!({
            "id": "terrain",
            "name": "Terrain",
            "components": {
                "GraphSource": { "builder": "scripts/tools/world.luau" },
                "Procedural": { "generator": "graph", "seed": 7 },
                "Streamed": { "bounds": 256.0 },
                "Transform": {}
            }
        }),
        json!({
            "id": "player",
            "name": "Player",
            "components": {
                "StreamFocus": {},
                "Transform": { "translation": [x, 0.0, z] }
            }
        }),
    ];
    if let Json::Array(more) = extra {
        entities.extend(more);
    }
    let scene = json!({ "format": "real-engine/scene", "version": 1, "entities": entities });
    let doc = re_scene::scene::from_json(&scene.to_string()).expect("scene parses");
    st.world.load(&doc).expect("scene loads");
    let r = st.world.update();
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    st
}

fn run(w: &mut World, ticks: std::ops::Range<u64>) -> Vec<StepReport> {
    ticks.map(|t| step(w, t, &config())).collect()
}

fn loaded(w: &World) -> Vec<String> {
    w.ecs
        .resource::<Streamer>()
        .cells()
        .map(|c| c.key.clone())
        .collect()
}

fn move_to(w: &mut World, x: f32, z: f32) {
    w.set(
        "player",
        "Transform",
        "translation",
        Value::Vec3(Vec3::new(x, 0.0, z)),
    )
    .unwrap();
}

#[test]
fn stream_config_defaults_and_errors() {
    let c = StreamConfig::default();
    assert!(c.validate().is_ok());
    assert_eq!(
        (c.cell_size, c.delay_ticks, c.apply_entities),
        (256.0, 30, 400)
    );
    assert_eq!(c.jobs(3), 2);
    assert_eq!(c.jobs(1), 1);
    let bad = StreamConfig {
        radius_ahead: 100.0,
        ..StreamConfig::default()
    };
    let d = bad.validate().unwrap_err();
    assert_eq!(d.code.to_string(), "RE4300");
    assert!(d.message.contains("radius_ahead"), "{}", d.message);
    let nan = StreamConfig {
        cell_size: f32::NAN,
        ..StreamConfig::default()
    };
    assert!(nan.validate().is_err());
    // Unknown keys are refused, missing ones take their defaults.
    assert!(serde_json::from_str::<StreamConfig>(r#"{"radius": 10, "radiuss": 1}"#).is_err());
    let partial: StreamConfig = serde_json::from_str(r#"{"cell_size": 128}"#).unwrap();
    assert_eq!(partial.cell_size, 128.0);
    assert_eq!(partial.radius, 768.0);
}

#[test]
fn stream_windows_agree_on_shared_samples() {
    let doc = world_graph();
    let registry = Arc::new(crate::NodeRegistry::m1());
    let graph = ProcGraph::from_doc(&doc, &registry).unwrap();
    let ev = Evaluator::new(registry);
    let eval = |origin: [i64; 2], n: u32| {
        let opts = EvalOptions {
            seed: Some(7),
            root: Some(fixtures()),
            domain: Some(Domain {
                origin,
                n,
                spacing: 4.0,
            }),
            ..Default::default()
        };
        ev.evaluate(&graph, &opts)
    };
    let a = eval([0, 0], 17);
    let b = eval([16, 0], 17);
    let big = eval([0, 0], 33);
    let field = |e: &crate::Evaluation| match e.output("h") {
        Some(Output::Field(f)) => f.clone(),
        other => panic!("{other:?}"),
    };
    let (fa, fb, fbig) = (field(&a), field(&b), field(&big));
    for z in 0..17 {
        // a's last column is b's first, and both match the big window.
        assert_eq!(fa.0.sample(16, z).to_bits(), fb.0.sample(0, z).to_bits());
        for x in 0..17 {
            assert_eq!(fa.0.sample(x, z).to_bits(), fbig.0.sample(x, z).to_bits());
            assert_eq!(
                fb.0.sample(x, z).to_bits(),
                fbig.0.sample(x + 16, z).to_bits()
            );
        }
    }
    assert_eq!(fb.0.offset, Vec3::new(64.0, 0.0, 0.0));
    // The jittered grid continues across windows: a's and b's points are the
    // big window's, split by the half-open border.
    let points = |e: &crate::Evaluation| match e.output("grid") {
        Some(Output::Points(p)) => p
            .points
            .iter()
            .map(|p| (p.position.x.to_bits(), p.position.z.to_bits()))
            .collect::<std::collections::BTreeSet<_>>(),
        other => panic!("{other:?}"),
    };
    let mut both = points(&a);
    both.extend(points(&b));
    let whole: std::collections::BTreeSet<_> = points(&big)
        .into_iter()
        .filter(|(_, z)| f32::from_bits(*z) < 64.0)
        .collect();
    assert_eq!(both, whole);
    assert_eq!(points(&a).len(), 16);
}

#[test]
fn stream_cells_load_at_their_tick_within_the_budget() {
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    let r = run(w, 0..3);
    assert!(r.iter().all(|r| r.loaded.is_empty()), "{r:?}");
    assert!(r[0].diagnostics.is_empty(), "{:#?}", r[0].diagnostics);
    let s = w.ecs.resource::<Streamer>();
    assert_eq!(s.pending_at("c/0_0"), Some(3));
    assert!(w.entity("terrain/c/0_0/ground/terrain").is_err());

    // Tick 3: as many cells as fit in 400 entities, in key order.
    let r = step(w, 3, &config());
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    assert!(!r.loaded.is_empty() && r.deferred > 0, "{r:?}");
    let mut sorted = r.loaded.clone();
    sorted.sort();
    assert_eq!(r.loaded, sorted);
    assert!(r.entities <= 400 + 60, "{}", r.entities);
    let events = w.ecs.resource::<StreamEvents>();
    assert_eq!(events.loaded.len(), r.loaded.len());
    let first = &events.loaded[0];
    assert_eq!(first.key, r.loaded[0]);
    assert_eq!(first.sources, ["terrain"]);
    assert_eq!(first.version.len(), 64);
    assert!(first.max.y > first.min.y);

    // The rest follow on later ticks.
    let more = run(w, 4..12);
    assert!(more.last().unwrap().pending == 0, "{:?}", more.last());
    let cells = loaded(w);
    for k in ["c/-1_-1", "c/0_0", "c/1_1", "c/0_-2"] {
        assert!(cells.contains(&k.to_string()), "{k}: {cells:?}");
    }
    assert!(!cells.contains(&"c/3_0".to_string()), "{cells:?}");
    assert!(w.entity("terrain/c/0_0/ground/terrain").is_ok());
    assert!(w.entity("terrain/c/0_0/water/water").is_ok());
    let s = w.ecs.resource::<Streamer>();
    assert!(s.memory_bytes() > 0);
    // Generated content of an unloaded cell is RE4304.
    assert!(s.check_loaded(w, "terrain/c/0_0/water/water").is_ok());
    let d = s.check_loaded(w, "terrain/c/3_3/water/water").unwrap_err();
    assert_eq!(d.code.to_string(), "RE4304");
}

#[test]
fn stream_walks_are_deterministic() {
    let walk = || {
        let mut st = streamed(world_graph(), -200.0, -200.0, json!([]));
        let w = &mut st.world;
        let mut log = Vec::new();
        for t in 0..120u64 {
            let p = -200.0 + t as f32 * 4.0;
            move_to(w, p, p);
            let r = step(w, t, &config());
            assert!(r.diagnostics.is_empty(), "tick {t}: {:#?}", r.diagnostics);
            log.push((r.loaded, r.unloaded, r.deferred, r.entities));
            w.update();
        }
        (log, w.state_hash())
    };
    let (a, ha) = walk();
    let (b, hb) = walk();
    assert_eq!(a, b);
    assert_eq!(ha, hb);
    assert!(a.iter().any(|l| !l.1.is_empty()), "the walk unloads cells");
}

/// A focus that looks along −Z and goes along +Z, as a car drives along its
/// +Z: the cells further ahead load the way it goes, not the way it looks.
#[test]
fn stream_cells_load_ahead_of_the_way_a_focus_goes() {
    // From (32, -96), in c/0_-2: c/0_-4 is 96 m along −Z, c/0_0 96 m along
    // +Z and c/2_-2 96 m along +X, all beyond `radius` and within
    // `radius_ahead`.
    let has = |cells: &[String], k: &str| cells.iter().any(|c| c == k);
    let mut still = streamed(world_graph(), 32.0, -96.0, json!([]));
    run(&mut still.world, 0..12);
    let cells = loaded(&still.world);
    assert!(has(&cells, "c/0_-4") && !has(&cells, "c/0_0"), "{cells:?}");

    // The same focus driving 2 m along +Z, then standing.
    let mut st = streamed(world_graph(), 32.0, -98.0, json!([]));
    let w = &mut st.world;
    for t in 0..12u64 {
        move_to(w, 32.0, (-98.0 + t as f32 * 0.5).min(-96.0));
        let r = step(w, t, &config());
        assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    }
    let cells = loaded(w);
    assert!(has(&cells, "c/0_0") && !has(&cells, "c/0_-4"), "{cells:?}");

    // Turned to look along +X and creeping 0.9 m that way, less than a
    // metre, it keeps the way it last went: c/2_-2 is not wanted.
    let along_x = Quat::from_xyzw(0.0, -FRAC_1_SQRT_2, 0.0, FRAC_1_SQRT_2);
    w.set("player", "Transform", "rotation", Value::Quat(along_x))
        .unwrap();
    for t in 12..24u64 {
        move_to(w, 32.0 + (t - 11).min(3) as f32 * 0.3, -96.0);
        step(w, t, &config());
    }
    let cells = loaded(w);
    assert!(has(&cells, "c/0_0") && !has(&cells, "c/2_-2"), "{cells:?}");
    let s = w.ecs.resource::<Streamer>();
    assert_eq!(s.pending_at("c/2_-2"), None);

    // A fork goes on the same way as the world it came from.
    let mut fork = w.fork();
    fork.ecs
        .insert_resource(w.ecs.resource::<GraphStore>().clone());
    let streamer = w.ecs.resource::<Streamer>().forked(w, &fork);
    fork.ecs.insert_resource(streamer);
    run(w, 24..36);
    run(&mut fork, 24..36);
    assert_eq!(loaded(&fork), loaded(w));
    assert!(!has(&loaded(&fork), "c/2_-2"));

    // A jump of more than a cell in one step, as a teleport, starts again:
    // from (32, 32) it looks ahead along its −Z, now +X (c/2_0), not the
    // way it jumped (c/0_2).
    move_to(w, 32.0, 32.0);
    run(w, 36..48);
    let cells = loaded(w);
    assert!(has(&cells, "c/2_0") && !has(&cells, "c/0_2"), "{cells:?}");
}

#[test]
fn stream_unloading_waits_for_the_margin_and_keeps_overrides() {
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    run(w, 0..12);
    let grid = w.entity("terrain").unwrap();
    let mut ov = re_scene::Overrides::default();
    ov.set_deleted("c/0_0/water/water", true);
    w.ecs.entity_mut(grid).insert(ov);
    // The override applies when the cell is next generated; nothing reloads
    // the cell by itself, so unload it by walking away and come back.
    move_to(w, 32.0 + 64.0 * 2.0, 32.0);
    let r = run(w, 12..16);
    assert!(
        r.iter().all(|r| !r.unloaded.contains(&"c/0_0".to_string())),
        "c/0_0 is within radius_ahead + margin"
    );
    move_to(w, 32.0 + 64.0 * 5.0, 32.0);
    let r = run(w, 16..20);
    assert!(
        r.iter().any(|r| r.unloaded.contains(&"c/0_0".to_string())),
        "{r:?}"
    );
    assert!(w.entity("terrain/c/0_0/ground/terrain").is_err());
    let events: Vec<_> = r.iter().flat_map(|r| r.unloaded.clone()).collect();
    assert!(events.windows(2).all(|p| p[0] < p[1] || p.len() < 2));
    move_to(w, 32.0, 32.0);
    run(w, 20..32);
    assert!(w.entity("terrain/c/0_0/ground/terrain").is_ok());
    assert!(w.entity("terrain/c/0_0/water/water").is_err(), "deleted");
    assert!(w.entity("terrain/c/1_0/water/water").is_ok());
}

#[test]
fn stream_a_deleted_cell_root_keeps_no_assets() {
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    run(w, 0..12);
    let held = |w: &World| w.ecs.resource::<GraphStore>().derived_count();
    let before = held(w);
    let cells = loaded(w);
    let grid = w.entity("terrain").unwrap();
    let mut ov = re_scene::Overrides::default();
    ov.set_deleted("c/0_0", true);
    w.ecs.entity_mut(grid).insert(ov);
    move_to(w, 32.0 + 64.0 * 5.0, 32.0);
    run(w, 12..20);
    move_to(w, 32.0, 32.0);
    run(w, 20..32);
    assert_eq!(loaded(w), cells);
    assert!(w.entity("terrain/c/0_0/ground/terrain").is_err());
    assert!(held(w) < before, "{} of {before}", held(w));
    // In a loaded cell, a deleted entity is not "in an unloaded cell".
    let s = w.ecs.resource::<Streamer>();
    assert!(s.check_loaded(w, "terrain/c/0_0/ground/terrain").is_ok());
    assert!(s.check_loaded(w, "terrain/c/9_9/ground/terrain").is_err());
}

#[test]
fn stream_regenerates_cells_when_the_graph_changes() {
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    run(w, 0..12);
    let before = w
        .get("terrain/c/0_0/water/water", "Transform", "translation")
        .unwrap();
    w.set(
        "terrain",
        "GraphSource",
        "params",
        Value::Str(r#"{"water":{"level":5.0}}"#.into()),
    )
    .unwrap();
    let r = run(w, 12..20);
    assert!(
        r.iter().any(|r| r.reloaded.contains(&"c/0_0".to_string())),
        "{r:?}"
    );
    let after = w
        .get("terrain/c/0_0/water/water", "Transform", "translation")
        .unwrap();
    assert_ne!(before, after);
    assert_eq!(after, Value::Vec3(Vec3::new(32.0, 5.0, 32.0)));
}

#[test]
fn stream_refuses_unstreamable_and_moved_sources() {
    let doc = graph(
        &[
            ("h", "terrain.heightfield", json!({})),
            ("mask", "terrain.island_mask", json!({})),
            ("land", "field.combine", json!({ "op": "mul" })),
            ("ground", "terrain.mesh", json!({})),
        ],
        &[
            ("h", "field", "land", "a"),
            ("mask", "field", "land", "b"),
            ("land", "field", "ground", "field"),
        ],
    );
    let mut st = streamed(doc, 0.0, 0.0, json!([]));
    let r = step(&mut st.world, 0, &config());
    assert_eq!(r.diagnostics.len(), 1, "{:#?}", r.diagnostics);
    assert_eq!(r.diagnostics[0].code.to_string(), "RE4301");
    assert!(r.diagnostics[0].message.contains("`mask`"));
    assert!(
        step(&mut st.world, 1, &config()).diagnostics.is_empty(),
        "once"
    );

    // A spacing that does not divide the cell.
    let odd = graph(&[("h", "terrain.heightfield", json!({ "cell": 3.0 }))], &[]);
    let mut st = streamed(odd, 0.0, 0.0, json!([]));
    let r = step(&mut st.world, 0, &config());
    assert_eq!(r.diagnostics[0].code.to_string(), "RE4301");
    // One that divides it but is not a multiple of 1/1024 m: chunks at their
    // corners would not meet exactly.
    let inexact = graph(
        &[("h", "terrain.heightfield", json!({ "cell": 0.64 }))],
        &[],
    );
    let mut st = streamed(inexact, 0.0, 0.0, json!([]));
    let r = step(&mut st.world, 0, &config());
    assert_eq!(r.diagnostics[0].code.to_string(), "RE4301");
    assert!(
        r.diagnostics[0].message.contains("1/1024"),
        "{}",
        r.diagnostics[0].message
    );

    let mut st = streamed(world_graph(), 0.0, 0.0, json!([]));
    st.world
        .set("terrain", "Transform", "translation", Value::Vec3(Vec3::X))
        .unwrap();
    let r = run(&mut st.world, 0..5);
    assert_eq!(r[0].diagnostics[0].code.to_string(), "RE4302");
    assert!(loaded(&st.world).is_empty());
}

#[test]
fn stream_stashes_authored_roots_with_their_cell() {
    let extra = json!([
        {
            "id": "tower",
            "name": "Tower",
            "components": {
                "Streamed": {},
                "Transform": { "translation": [100.0, 0.0, 10.0] }
            }
        },
        { "id": "tower_top", "name": "Top", "parent": "tower", "components": { "Transform": {} } },
        {
            "id": "far_hut",
            "name": "Hut",
            "components": {
                "Streamed": {},
                "Transform": { "translation": [2000.0, 0.0, 0.0] }
            }
        }
    ]);
    let mut st = streamed(world_graph(), 32.0, 32.0, extra);
    let w = &mut st.world;
    let saved = re_scene::scene::to_json(&w.save(re_scene::SaveOptions::default()));
    step(w, 0, &config());
    // The hut's cell is not wanted: stashed at once, still saved.
    assert!(w.entity("far_hut").is_err());
    assert_eq!(w.stashed().unwrap().cell_of("far_hut"), Some("c/31_0"));
    // The tower's cell (c/1_0) is pending, so the tower stays.
    assert!(w.entity("tower").is_ok());
    run(w, 1..12);
    assert!(w.entity("tower_top").is_ok());
    assert_eq!(
        re_scene::scene::to_json(&w.save(re_scene::SaveOptions::default())),
        saved
    );
    move_to(w, 32.0 - 64.0 * 5.0, 32.0);
    run(w, 12..20);
    assert!(w.entity("tower").is_err());
    assert_eq!(w.stashed().unwrap().cell_of("tower_top"), Some("c/1_0"));
    move_to(w, 32.0, 32.0);
    run(w, 20..32);
    assert!(w.entity("tower_top").is_ok());
    // A loaded cell lists no source when only authored content is in it.
    assert!(w.ecs.resource::<Streamer>().is_loaded("c/1_0"));
}

fn water_y(w: &World, cell: &str) -> f32 {
    match w
        .get(
            &format!("terrain/{cell}/water/water"),
            "Transform",
            "translation",
        )
        .unwrap()
    {
        Value::Vec3(v) => v.y,
        other => panic!("{other:?}"),
    }
}

fn set_water(w: &mut World, level: f32) {
    w.set(
        "terrain",
        "GraphSource",
        "params",
        Value::Str(format!(r#"{{"water":{{"level":{level:?}}}}}"#)),
    )
    .unwrap();
}

#[test]
fn stream_regenerates_kept_cells_no_focus_wants() {
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    run(w, 0..12);
    // c/0_0 stays loaded by the margin only.
    move_to(w, 160.0, 32.0);
    run(w, 12..16);
    assert!(w.ecs.resource::<Streamer>().is_loaded("c/0_0"));
    set_water(w, 5.0);
    let r = run(w, 16..40);
    assert_eq!(water_y(w, "c/0_0"), 5.0);
    assert_eq!(r.last().unwrap().pending, 0, "{:?}", r.last());
}

#[test]
fn stream_drops_deferred_content_whose_inputs_changed() {
    for budget in [410, 425, 440] {
        let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
        let w = &mut st.world;
        let cfg = StreamConfig {
            apply_entities: budget,
            ..config()
        };
        for t in 0..4 {
            step(w, t, &cfg);
        }
        set_water(w, 5.0);
        for t in 4..60 {
            step(w, t, &cfg);
        }
        for c in w
            .ecs
            .resource::<Streamer>()
            .cells()
            .map(|c| c.key.clone())
            .collect::<Vec<_>>()
        {
            if w.entity(&format!("terrain/{c}/water/water")).is_ok() {
                assert_eq!(water_y(w, &c), 5.0, "{c} with budget {budget}");
            }
        }
    }
}

#[test]
fn stream_memory_pressure_shrinks_the_radius_instead_of_thrashing() {
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    let cfg = StreamConfig {
        memory_mb: 1,
        radius_ahead: 224.0,
        ..config()
    };
    let mut loads = std::collections::BTreeMap::<String, usize>::new();
    let mut codes = Vec::new();
    for t in 0..60 {
        let r = step(w, t, &cfg);
        for k in r.loaded {
            *loads.entry(k).or_default() += 1;
        }
        codes.extend(r.diagnostics.iter().map(|d| d.code.to_string()));
    }
    assert!(loads.values().all(|n| *n <= 2), "{loads:?}");
    assert_eq!(codes, ["RE4306"]);
    let s = w.ecs.resource::<Streamer>();
    assert!(s.radius() < 64.0);
}

#[test]
fn stream_unloads_the_cells_of_a_source_that_stops_streaming() {
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    run(w, 0..12);
    assert!(w.entity("terrain/c/0_0/ground/terrain").is_ok());
    w.set("terrain", "Transform", "translation", Value::Vec3(Vec3::X))
        .unwrap();
    let r = run(w, 12..20);
    assert_eq!(r[0].diagnostics[0].code.to_string(), "RE4302");
    assert!(loaded(w).is_empty());
    assert!(w.entity("terrain/c/0_0/ground/terrain").is_err());
}

#[test]
fn stream_removes_a_stopped_source_from_cells_another_keeps() {
    let rocks = json!([{
        "id": "rocks",
        "name": "Rocks",
        "components": {
            "GraphSource": { "builder": "scripts/tools/world.luau" },
            "Procedural": { "generator": "graph", "seed": 9 },
            "Streamed": { "bounds": 256.0 },
            "Transform": {}
        }
    }]);
    let mut st = streamed(world_graph(), 32.0, 32.0, rocks);
    let w = &mut st.world;
    w.ecs
        .resource_mut::<GraphStore>()
        .insert("rocks", world_graph());
    run(w, 0..12);
    assert!(w.entity("rocks/c/0_0/ground/terrain").is_ok());
    w.set("rocks", "Transform", "translation", Value::Vec3(Vec3::X))
        .unwrap();
    run(w, 12..20);
    assert!(loaded(w).contains(&"c/0_0".to_string()));
    assert!(w.entity("terrain/c/0_0/ground/terrain").is_ok());
    assert!(w.entity("rocks/c/0_0/ground/terrain").is_err());
}

#[test]
fn stream_refuses_meshes_made_at_the_origin() {
    let doc = graph(&[("rock", "mesh.primitive", json!({}))], &[]);
    let mut st = streamed(doc, 0.0, 0.0, json!([]));
    let r = step(&mut st.world, 0, &config());
    assert_eq!(r.diagnostics[0].code.to_string(), "RE4301");
}

#[test]
fn stream_a_forked_world_unloads_its_own_cells() {
    // Ids that sort before `terrain` give it another handle in the fork.
    let extra = json!([
        { "id": "a1", "components": { "Transform": {} } },
        { "id": "a2", "components": { "Transform": {} } },
        { "id": "a3", "components": { "Transform": {} } },
    ]);
    let mut st = streamed(world_graph(), 32.0, 32.0, extra);
    let w = &mut st.world;
    run(w, 0..12);
    let mut fork = w.fork();
    assert_ne!(
        w.entity("terrain").unwrap(),
        fork.entity("terrain").unwrap()
    );
    fork.ecs
        .insert_resource(w.ecs.resource::<GraphStore>().clone());
    let streamer = w.ecs.resource::<Streamer>().forked(w, &fork);
    fork.ecs.insert_resource(streamer);
    let before = w.state_hash();
    move_to(&mut fork, 32.0 + 64.0 * 5.0, 32.0);
    let r = run(&mut fork, 12..20);
    assert!(r.iter().any(|r| r.unloaded.contains(&"c/0_0".to_string())));
    assert!(fork.entity("terrain/c/0_0/ground/terrain").is_err());
    assert!(fork.entity("a1").is_ok() && fork.entity("a3").is_ok());
    move_to(&mut fork, 32.0, 32.0);
    run(&mut fork, 20..32);
    assert!(fork.entity("terrain/c/0_0/ground/terrain").is_ok());
    // The original world is untouched.
    assert_eq!(w.state_hash(), before);
}

/// An editor's view focus replaces the world's focuses (the `StreamFocus`
/// entities, else the active camera) for the cells while it is set; a fork
/// does not keep it, and clearing it streams around the world's focuses
/// again.
#[test]
fn stream_a_view_focus_replaces_the_focuses_until_cleared() {
    let has = |cells: &[String], k: &str| cells.iter().any(|c| c == k);
    for camera in [false, true] {
        let extra = json!([{
            "id": "camera",
            "components": {
                "Camera": { "active": true },
                "Transform": { "translation": [160.0, 10.0, 160.0] }
            }
        }]);
        let mut st = streamed(world_graph(), 160.0, 160.0, extra);
        let w = &mut st.world;
        if camera {
            let player = w.entity("player").unwrap();
            w.ecs.entity_mut(player).remove::<re_scene::StreamFocus>();
        }
        run(w, 0..12);
        assert!(has(&loaded(w), "c/2_2"), "{:?}", loaded(w));

        // In c/-3_-2, looking along +Z: c/-3_0 is 96 m ahead and loads,
        // c/-3_-4 is 96 m behind and does not, and the cells around the
        // world's focus unload.
        let view = Focus {
            position: Vec2::new(-160.0, -96.0),
            forward: Vec2::new(0.0, 2.0),
        };
        w.ecs.resource_mut::<Streamer>().set_view_focus(Some(view));
        let s = w.ecs.resource::<Streamer>();
        assert_eq!(s.view_focus().map(|f| f.forward), Some(Vec2::Y));
        let r = run(w, 12..24);
        assert!(r.iter().all(|r| r.diagnostics.is_empty()), "{r:#?}");
        let cells = loaded(w);
        assert!(has(&cells, "c/-3_-2") && has(&cells, "c/-3_0"), "{cells:?}");
        assert!(
            !has(&cells, "c/-3_-4") && !has(&cells, "c/2_2"),
            "{cells:?}"
        );

        // A fork streams around the world's focus.
        let mut fork = w.fork();
        fork.ecs
            .insert_resource(w.ecs.resource::<GraphStore>().clone());
        let streamer = w.ecs.resource::<Streamer>().forked(w, &fork);
        assert_eq!(streamer.view_focus(), None);
        fork.ecs.insert_resource(streamer);
        run(&mut fork, 24..36);

        // Cleared, the world's focus is streamed around again.
        w.ecs.resource_mut::<Streamer>().set_view_focus(None);
        run(w, 24..36);
        let cells = loaded(w);
        assert!(has(&cells, "c/2_2") && !has(&cells, "c/-3_-2"), "{cells:?}");
        assert_eq!(loaded(&fork), cells, "camera: {camera}");
    }
}

#[test]
fn stream_jobs_run_ahead_and_waits_are_counted() {
    // With no delay a result can never be ready in time: every load waits.
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    let now = StreamConfig {
        delay_ticks: 0,
        ..config()
    };
    let r = step(w, 0, &now);
    assert!(!r.loaded.is_empty());
    assert!(r.waits >= r.loaded.len(), "{r:?}");

    // With a delay, jobs start at the request and run on the pool.
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    let r = step(w, 0, &config());
    assert!(r.in_flight > 0 && r.in_flight <= config().jobs(re_core::TaskPool::global().threads()));
    let all = run(w, 1..20);
    assert_eq!(all.last().unwrap().pending, 0);
}

#[test]
fn stream_late_results_apply_when_they_arrive() {
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    let mut s = Streamer::new(StreamConfig {
        delay_ticks: 0,
        ..config()
    })
    .unwrap();
    s.set_lateness(Lateness::Late);
    w.ecs.insert_resource(s);
    let first = step(w, 0, &config());
    assert_eq!(first.waits, 0);
    assert!(first.loaded.is_empty(), "nothing is ready at tick 0");
    // Results arrive later; they are applied without waiting, with RE4305.
    let mut late = 0;
    for t in 1..2000u64 {
        let r = step(w, t, &config());
        assert_eq!(r.waits, 0);
        late += r.late;
        if r.pending == 0 && r.in_flight == 0 && t > 2 {
            break;
        }
        std::thread::yield_now();
    }
    assert!(!loaded(w).is_empty());
    // How late depends on the machine; that is the point of this mode.
    let _ = late;
}

#[test]
fn stream_cancels_jobs_for_cells_no_longer_wanted() {
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    let cfg = StreamConfig {
        delay_ticks: 50,
        ..config()
    };
    step(w, 0, &cfg);
    assert!(w.ecs.resource::<Streamer>().in_flight() > 0);
    // Far outside the source's bounds: nothing is wanted any more.
    move_to(w, 5000.0, 5000.0);
    let r = step(w, 1, &cfg);
    assert_eq!((r.pending, r.in_flight), (0, 0), "{r:?}");
}

#[test]
fn stream_waits_never_apply_a_job_issued_with_old_inputs() {
    // One worker, held busy: the cells' jobs are still queued when the
    // water level changes, as on a slow machine.
    let pool = Arc::new(re_core::TaskPool::new(1).unwrap());
    let (release, held) = std::sync::mpsc::channel::<()>();
    pool.spawn(move || {
        let _ = held.recv();
    });
    let mut st = streamed(world_graph(), 32.0, 32.0, json!([]));
    let w = &mut st.world;
    let cfg = StreamConfig {
        delay_ticks: 2,
        ..config()
    };
    w.ecs
        .insert_resource(Streamer::new(cfg.clone()).unwrap().with_pool(pool));
    let r = step(w, 0, &cfg);
    assert!(r.in_flight > 0, "{r:?}");
    set_water(w, 5.0);
    step(w, 1, &cfg);
    release.send(()).unwrap();
    let r = step(w, 2, &cfg);
    assert!(!r.loaded.is_empty(), "{r:?}");
    for c in loaded(w) {
        assert_eq!(water_y(w, &c), 5.0, "{c} has the new water level");
    }
}

/// A height field drawn as level-of-detail chunks (levels 0 to 2: chunks of
/// 256, 512 and 1024 m), with crates on it.
fn chunked_graph() -> GraphDoc {
    graph(
        &[
            (
                "h",
                "terrain.heightfield",
                json!({ "cell": 4.0, "height": 40.0, "scale": 0.01 }),
            ),
            ("ground", "terrain.chunks", json!({ "max_level": 2 })),
            (
                "grid",
                "scatter.grid_jitter",
                json!({ "spacing": 16.0, "jitter": 0.8 }),
            ),
            (
                "crates",
                "points.instance",
                json!({ "prefab": "prefabs/crate.prefab.json", "random_yaw": true }),
            ),
        ],
        &[
            ("h", "field", "ground", "field"),
            ("h", "field", "grid", "field"),
            ("grid", "points", "crates", "points"),
        ],
    )
}

fn terrain_config() -> StreamConfig {
    StreamConfig {
        terrain_view: 1024.0,
        ..config()
    }
}

/// Steps with the terrain view on until no terrain job is in flight; checks
/// after every tick that no ground is drawn twice.
fn settle_view(w: &mut World, from: u64) -> u64 {
    if !w.ecs.contains_resource::<Streamer>() {
        let mut s = Streamer::new(terrain_config()).unwrap();
        s.set_view(true);
        w.ecs.insert_resource(s);
    }
    for t in from..from + 2000 {
        let r = step(w, t, &terrain_config());
        assert!(r.diagnostics.is_empty(), "tick {t}: {:#?}", r.diagnostics);
        w.update();
        assert_no_overlap(w);
        let busy = w.ecs.resource::<Streamer>().in_flight() > 0;
        let shown = w
            .ecs
            .get_resource::<TerrainView>()
            .map_or(0, TerrainView::len);
        let missing = w.ecs.resource::<Streamer>().view_missing();
        if !busy && shown > 0 && missing == 0 && t > from + 10 {
            return t + 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the terrain view never settled");
}

fn assert_no_overlap(w: &World) {
    let Some(view) = w.ecs.get_resource::<TerrainView>() else {
        return;
    };
    let keys: Vec<_> = view.chunks().map(|c| c.key).collect();
    for (i, a) in keys.iter().enumerate() {
        for b in &keys[i + 1..] {
            let (fine, coarse) = if a.level <= b.level { (a, b) } else { (b, a) };
            let d = u32::from(coarse.level - fine.level);
            assert!(
                (fine.x >> d, fine.z >> d) != (coarse.x, coarse.z),
                "{a:?} and {b:?} are both drawn"
            );
        }
    }
}

#[test]
fn stream_terrain_chunks_may_feed_a_merge() {
    // Not streamed, the merge gathers the chunks node's mesh and the water.
    // Streamed, world streaming draws the terrain and each cell's merge
    // gathers the rest.
    let doc = graph(
        &[
            (
                "h",
                "terrain.heightfield",
                json!({ "cell": 4.0, "height": 40.0, "scale": 0.01 }),
            ),
            ("ground", "terrain.chunks", json!({ "max_level": 2 })),
            ("water", "terrain.water_plane", json!({ "level": 3.0 })),
            ("out", "fragment.merge", json!({})),
        ],
        &[
            ("h", "field", "ground", "field"),
            ("ground", "fragment", "out", "a"),
            ("water", "fragment", "out", "b"),
        ],
    );
    let mut st = streamed(doc, 32.0, 32.0, json!([]));
    let r = run(&mut st.world, 0..5);
    assert!(r.iter().all(|r| r.diagnostics.is_empty()), "{r:?}");
    let w = &st.world;
    assert!(w.entity("terrain/c/0_0/water/water").is_ok());
    let e = w.entity("terrain").unwrap();
    let s = source::describe(w, e, &config(), &mut source::DescribeCache::new()).unwrap();
    assert!(s.terrain.is_some());
}

#[test]
fn stream_terrain_view_is_off_by_default() {
    let mut st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    run(&mut st.world, 0..10);
    assert!(!st.world.ecs.contains_resource::<TerrainView>());
}

#[test]
fn stream_terrain_chunks_cover_the_source_with_matching_seams() {
    let mut st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    let w = &mut st.world;
    settle_view(w, 0);
    let view = w.ecs.resource::<TerrainView>();
    assert!(view.len() >= 4, "{} chunks", view.len());
    // The finest chunks are under the focus.
    assert!(view.chunks().any(|c| c.key.level == 0));
    let terrain = w.entity("terrain").unwrap();
    // Every vertex two chunks share has one height, and every chunk lies
    // over the source's square (bounds 256 m).
    let mut heights: std::collections::HashMap<(i64, i64), f32> = Default::default();
    for c in view.chunks() {
        assert_eq!(c.source, terrain);
        assert!(c.mesh.starts_with("derived:terrain/terrain/"), "{}", c.mesh);
        for v in &c.data().vertices {
            let (x, z) = (c.corner.x + v.pos[0], c.corner.z + v.pos[2]);
            let at = ((x * 16.0) as i64, (z * 16.0) as i64);
            let y = *heights.entry(at).or_insert(v.pos[1]);
            assert!(
                (y - v.pos[1]).abs() < 1e-3,
                "seam at {at:?}: {y} and {}",
                v.pos[1]
            );
        }
    }
}

#[test]
fn stream_terrain_view_keeps_simulation_state_unchanged() {
    let walk = |view: bool| {
        let mut st = streamed(chunked_graph(), -100.0, -100.0, json!([]));
        let w = &mut st.world;
        let mut s = Streamer::new(terrain_config()).unwrap();
        s.set_view(view);
        w.ecs.insert_resource(s);
        for t in 0..80u64 {
            let p = -100.0 + t as f32 * 3.0;
            move_to(w, p, p);
            let r = step(w, t, &terrain_config());
            assert!(r.diagnostics.is_empty(), "tick {t}: {:#?}", r.diagnostics);
            w.update();
        }
        w.state_hash()
    };
    assert_eq!(walk(false), walk(true));
}

#[test]
fn stream_terrain_follows_the_focus_without_drawing_ground_twice() {
    let mut st = streamed(chunked_graph(), -200.0, -200.0, json!([]));
    let w = &mut st.world;
    let t = settle_view(w, 0);
    let fine_at = |w: &World| -> Vec<(i32, i32)> {
        let mut v: Vec<_> = w
            .ecs
            .resource::<TerrainView>()
            .chunks()
            .filter(|c| c.key.level == 0)
            .map(|c| (c.key.x, c.key.z))
            .collect();
        v.sort();
        v
    };
    assert_eq!(fine_at(w), [(-1, -1), (-1, 0), (0, -1), (0, 0)]);
    // Far away, the source is drawn coarser; back again, finer.
    move_to(w, 1200.0, 1200.0);
    let t = settle_view(w, t);
    assert_eq!(fine_at(w), []);
    move_to(w, -200.0, -200.0);
    settle_view(w, t);
    assert_eq!(fine_at(w), [(-1, -1), (-1, 0), (0, -1), (0, 0)]);
}

#[test]
fn stream_the_terrain_view_follows_a_view_focus() {
    let fine = |w: &World| -> Vec<(i32, i32)> {
        let mut v: Vec<_> = w
            .ecs
            .resource::<TerrainView>()
            .chunks()
            .filter(|c| c.key.level == 0)
            .map(|c| (c.key.x, c.key.z))
            .collect();
        v.sort();
        v
    };
    // The player is far away; the view focus is where the fine chunks are.
    let mut st = streamed(chunked_graph(), 1200.0, 1200.0, json!([]));
    let w = &mut st.world;
    let mut s = Streamer::new(terrain_config()).unwrap();
    s.set_view_focus(Some(Focus {
        position: Vec2::new(-200.0, -200.0),
        forward: Vec2::ZERO,
    }));
    w.ecs.insert_resource(s);
    complete_view(w);
    assert_eq!(fine(w), [(-1, -1), (-1, 0), (0, -1), (0, 0)]);
    // Stepped, it stays there; cleared, it follows the player.
    let t = settle_view(w, 0);
    assert_eq!(fine(w), [(-1, -1), (-1, 0), (0, -1), (0, 0)]);
    w.ecs.resource_mut::<Streamer>().set_view_focus(None);
    settle_view(w, t);
    assert_eq!(fine(w), []);
}

#[test]
fn stream_terrain_view_regenerates_when_the_inputs_change() {
    let mut st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    let w = &mut st.world;
    let heights = |w: &World| -> Vec<(re_geom::terrain::ChunkKey, Vec<f32>)> {
        w.ecs
            .resource::<TerrainView>()
            .chunks()
            .map(|c| (c.key, c.data().vertices.iter().map(|v| v.pos[1]).collect()))
            .collect()
    };
    let t = settle_view(w, 0);
    let before = heights(w);
    w.set("terrain", "Procedural", "seed", Value::Int(8))
        .unwrap();
    settle_view(w, t);
    let after = heights(w);
    assert_eq!(before.len(), after.len());
    for (k, h) in &before {
        let (_, now) = after.iter().find(|(a, _)| a == k).expect("still drawn");
        assert_ne!(h, now, "{k:?} kept the old seed's heights");
    }
}

/// The drawn chunks: key, stitching, corner, material, mesh name and mesh,
/// in order.
type Drawn = Vec<(
    re_geom::terrain::ChunkKey,
    re_geom::terrain::Edges,
    Vec3,
    String,
    String,
    re_types::mesh::MeshData,
)>;

fn drawn(w: &World) -> Drawn {
    w.ecs
        .resource::<TerrainView>()
        .chunks()
        .map(|c| {
            (
                c.key,
                c.edges,
                c.corner,
                c.material.clone(),
                c.mesh.clone(),
                (**c.data()).clone(),
            )
        })
        .collect()
}

#[test]
fn stream_terrain_view_completes_whatever_came_before() {
    // From scratch: one call draws the whole selection, view off before.
    let mut fresh = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    let a = &mut fresh.world;
    a.ecs
        .insert_resource(Streamer::new(terrain_config()).unwrap());
    let before = a.state_hash();
    complete_view(a);
    let s = a.ecs.resource::<Streamer>();
    assert!(s.view(), "completing turns the view on");
    assert_eq!((s.view_missing(), s.in_flight()), (0, 0));
    assert_no_overlap(a);
    assert!(drawn(a).iter().any(|c| c.0.level == 0));
    assert_eq!(a.state_hash(), before, "the view is not simulation state");

    // Another world gets there from far away, with its one worker held
    // busy for a few ticks: the chunks it needs are not made yet, and coarse
    // ones are still drawn.
    let mut walked = streamed(chunked_graph(), 1200.0, -1200.0, json!([]));
    let b = &mut walked.world;
    let pool = Arc::new(re_core::TaskPool::new(1).unwrap());
    let mut s = Streamer::new(terrain_config())
        .unwrap()
        .with_pool(pool.clone());
    s.set_view(true);
    b.ecs.insert_resource(s);
    let t = settle_view(b, 0);
    assert_ne!(drawn(b), drawn(a));
    let (release, held) = std::sync::mpsc::channel::<()>();
    pool.spawn(move || {
        let _ = held.recv();
    });
    move_to(b, 0.0, 0.0);
    // Within delay_ticks, so no cell or collision chunk is due (and waited
    // for) while the worker is held.
    for t in t..t + 2 {
        step(b, t, &terrain_config());
    }
    assert!(b.ecs.resource::<Streamer>().view_missing() > 0);
    release.send(()).unwrap();
    complete_view(b);
    assert_eq!(b.ecs.resource::<Streamer>().view_missing(), 0);
    assert_eq!(
        drawn(b),
        drawn(a),
        "the same chunks, stitching, meshes and mesh names"
    );
}

#[test]
fn stream_terrain_view_does_not_retry_failed_chunks() {
    let mut st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    let w = &mut st.world;
    let e = w.entity("terrain").unwrap();
    let mut cache = source::DescribeCache::new();
    let mut s = source::describe(w, e, &terrain_config(), &mut cache).unwrap();
    // A field no node makes: every chunk fails.
    let t = s.terrain.clone().expect("the graph has terrain.chunks");
    s.terrain = Some(Arc::new(source::TerrainSource {
        graph: t.graph.clone(),
        field: "nowhere".into(),
        material: String::new(),
        max_level: t.max_level,
    }));
    let pool = re_core::TaskPool::new(1).unwrap();
    let mut view = terrain::TerrainState::default();
    let at = [Vec3::ZERO];
    let failures = view.step(w, &[s.clone()], &at, 1024.0, &pool, terrain::Pace::Complete);
    let selected = view.missing();
    assert!(selected > 0);
    assert_eq!(view.in_flight(), 0);
    assert_eq!(
        failures.len(),
        selected,
        "one problem per chunk, one subject"
    );
    assert!(
        failures
            .iter()
            .all(|(subject, _)| *subject == failures[0].0)
    );
    let d = &failures[0].1;
    assert_eq!(d.code.to_string(), "RE4307");
    assert!(
        d.message.starts_with("terrain: terrain view chunk 0/"),
        "{}",
        d.message
    );
    assert!(w.ecs.resource::<TerrainView>().is_empty());
    // With the worker held, a job started now stays in flight.
    let (release, held) = std::sync::mpsc::channel::<()>();
    pool.spawn(move || {
        let _ = held.recv();
    });
    let failures = view.step(w, &[s.clone()], &at, 1024.0, &pool, terrain::Pace::Jobs(64));
    assert_eq!(view.in_flight(), 0, "failed chunks are not tried again");
    assert!(failures.is_empty());
    assert_eq!(view.missing(), selected);
    // New inputs: they are.
    s.inputs = blake3::hash(b"new inputs");
    view.step(w, &[s], &at, 1024.0, &pool, terrain::Pace::Jobs(64));
    assert_eq!(view.in_flight(), selected);
    release.send(()).unwrap();
}

#[test]
fn stream_terrain_view_leaves_failed_ground_bare_whatever_came_before() {
    // The source's chunks, and the same inputs with chunks that fail (a
    // field no node makes), driven by hand: two chunks near the focus are
    // made, the other two fail.
    let world = || {
        let st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
        let e = st.world.entity("terrain").unwrap();
        let mut cache = source::DescribeCache::new();
        let good = source::describe(&st.world, e, &terrain_config(), &mut cache).unwrap();
        let mut bad = good.clone();
        let t = good.terrain.clone().unwrap();
        bad.terrain = Some(Arc::new(source::TerrainSource {
            graph: t.graph.clone(),
            field: "nowhere".into(),
            material: t.material.clone(),
            max_level: t.max_level,
        }));
        (st, good, bad)
    };
    let pool = re_core::TaskPool::new(2).unwrap();
    let step =
        |w: &mut World, view: &mut terrain::TerrainState, s: &source::Source, at: Vec3, pace| {
            view.step(w, std::slice::from_ref(s), &[at], 1024.0, &pool, pace)
        };
    let settle = |w: &mut World, view: &mut terrain::TerrainState, s: &source::Source, at: Vec3| {
        step(w, view, s, at, terrain::Pace::Jobs(2));
        while view.in_flight() > 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            step(w, view, s, at, terrain::Pace::Jobs(0));
        }
    };
    let near = Vec3::ZERO;
    let keys = |w: &World| -> Vec<(u8, i32, i32)> {
        w.ecs
            .resource::<TerrainView>()
            .chunks()
            .map(|c| (c.key.level, c.key.x, c.key.z))
            .collect()
    };

    let (mut fresh, good, bad) = world();
    let a = &mut fresh.world;
    let mut view_a = terrain::TerrainState::default();
    settle(a, &mut view_a, &good, near);
    assert_eq!(keys(a), [(0, -1, -1), (0, -1, 0)], "the two nearest first");
    let failures = step(a, &mut view_a, &bad, near, terrain::Pace::Complete);
    assert_eq!(failures.len(), 2);
    assert_eq!(view_a.missing(), 2);

    // Coarse chunks drawn from far away cover the ground that fails later.
    let (mut walked, good, bad) = world();
    let b = &mut walked.world;
    let mut view_b = terrain::TerrainState::default();
    let far = Vec3::new(1200.0, 0.0, -1200.0);
    for _ in 0..10 {
        settle(b, &mut view_b, &good, far);
    }
    assert!(
        keys(b).contains(&(1, 0, -1)),
        "a coarse chunk over (0, 0, -1): {:?}",
        keys(b)
    );
    settle(b, &mut view_b, &good, near);
    step(b, &mut view_b, &bad, near, terrain::Pace::Complete);
    assert_eq!(view_b.missing(), 2);
    assert_eq!(keys(b), keys(a), "bare where the chunks failed");
    assert_eq!(drawn(b), drawn(a));

    // New inputs whose chunks all fail: what the old ones drew goes too.
    let (mut changed, good, mut bad) = world();
    let c = &mut changed.world;
    let mut view_c = terrain::TerrainState::default();
    for _ in 0..2 {
        settle(c, &mut view_c, &good, near);
    }
    assert_eq!(keys(c).len(), 4);
    bad.inputs = blake3::hash(b"new inputs");
    step(c, &mut view_c, &bad, near, terrain::Pace::Complete);
    assert_eq!((keys(c).len(), view_c.missing()), (0, 4));
}

#[test]
fn stream_terrain_view_completes_the_same_after_its_inputs_change() {
    // Finer samples (so every chunk has another corner) and another
    // material: a view drawn before the change ends up as one drawn after.
    let params = Value::Str(
        r#"{"h":{"cell":2.0},"ground":{"material":"materials/rock.material.json"}}"#.into(),
    );
    let key = |level, x, z| re_geom::terrain::ChunkKey { level, x, z };
    let mut before = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    let b = &mut before.world;
    settle_view(b, 0);
    assert!(drawn(b).iter().any(|c| c.0 == key(0, -1, -1)));
    b.set("terrain", "GraphSource", "params", params.clone())
        .unwrap();
    complete_view(b);
    assert_eq!(b.ecs.resource::<Streamer>().view_missing(), 0);
    assert_no_overlap(b);

    let mut after = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    let a = &mut after.world;
    a.set("terrain", "GraphSource", "params", params).unwrap();
    a.ecs
        .insert_resource(Streamer::new(terrain_config()).unwrap());
    complete_view(a);
    let fresh = drawn(a);
    let near = fresh.iter().find(|c| c.0 == key(0, -1, -1)).expect("drawn");
    assert_eq!(near.2, Vec3::new(-128.0, 0.0, -128.0), "64 cells of 2 m");
    assert!(fresh.iter().all(|c| c.3 == "materials/rock.material.json"));
    assert_eq!(drawn(b), fresh);
}

#[test]
fn stream_terrain_view_completes_new_stitching_after_a_move() {
    // A source 2 km across, so the view has every level and stitched seams;
    // driven by hand.
    let world = || {
        let st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
        let e = st.world.entity("terrain").unwrap();
        let mut cache = source::DescribeCache::new();
        let mut s = source::describe(&st.world, e, &terrain_config(), &mut cache).unwrap();
        s.bounds = 1024.0;
        (st, s)
    };
    let pool = re_core::TaskPool::new(1).unwrap();
    let step =
        |w: &mut World, view: &mut terrain::TerrainState, s: &source::Source, at: Vec3, pace| {
            view.step(w, std::slice::from_ref(s), &[at], 1024.0, &pool, pace)
        };
    let (from, to) = (
        Vec3::new(-900.0, 0.0, -900.0),
        Vec3::new(-900.0, 0.0, -400.0),
    );

    let (mut fresh, s) = world();
    let a = &mut fresh.world;
    let mut view_a = terrain::TerrainState::default();
    step(a, &mut view_a, &s, to, terrain::Pace::Complete);
    assert_eq!(view_a.missing(), 0);
    let whole = drawn(a);
    assert!(whole.iter().any(|c| c.0.level == 1));
    assert!(
        whole
            .iter()
            .any(|c| c.1 != re_geom::terrain::Edges::default()),
        "stitched seams"
    );

    // A world drawn for the old focus, with jobs for the new one in flight
    // behind its held worker when it completes.
    let (mut moved, s) = world();
    let b = &mut moved.world;
    let mut view_b = terrain::TerrainState::default();
    step(b, &mut view_b, &s, from, terrain::Pace::Complete);
    let before = drawn(b);
    let (release, held) = std::sync::mpsc::channel::<()>();
    pool.spawn(move || {
        let _ = held.recv();
    });
    step(b, &mut view_b, &s, to, terrain::Pace::Jobs(4));
    assert_eq!(view_b.in_flight(), 4);
    release.send(()).unwrap();
    step(b, &mut view_b, &s, to, terrain::Pace::Complete);
    assert_eq!((view_b.missing(), view_b.in_flight()), (0, 0));
    let after = drawn(b);
    assert!(
        before
            .iter()
            .any(|c| after.iter().any(|d| d.0 == c.0 && d.1 != c.1)),
        "a chunk drawn for both focuses is stitched another way"
    );
    assert_eq!(after, whole);
    assert_no_overlap(b);
}

/// [`chunked_graph`]'s heights, scaled past `f32::MAX` wherever they rise
/// above 22.7 m: a chunk over such ground fails (heights that are not
/// finite) at its first sample there, so chunks fail at other samples. The
/// graph makes no cells.
fn overflowing_graph() -> GraphDoc {
    graph(
        &[
            (
                "h",
                "terrain.heightfield",
                json!({ "cell": 4.0, "height": 40.0, "scale": 0.01 }),
            ),
            (
                "up",
                "field.remap",
                json!({
                    "mode": "linear", "in_min": 0.0, "in_max": 20.0,
                    "out_min": 0.0, "out_max": 3.0e38,
                }),
            ),
            ("ground", "terrain.chunks", json!({ "max_level": 2 })),
        ],
        &[
            ("h", "field", "up", "field"),
            ("up", "field", "ground", "field"),
        ],
    )
}

/// Steps with the terrain view on until no terrain job is in flight, and
/// returns the next tick and the problems reported.
fn run_view(w: &mut World, from: u64) -> (u64, Vec<re_core::Diagnostic>) {
    if !w.ecs.contains_resource::<Streamer>() {
        let mut s = Streamer::new(terrain_config()).unwrap();
        s.set_view(true);
        w.ecs.insert_resource(s);
    }
    let mut problems = Vec::new();
    for t in from..from + 2000 {
        problems.extend(step(w, t, &terrain_config()).diagnostics);
        w.update();
        if w.ecs.resource::<Streamer>().in_flight() == 0 && t > from + 10 {
            return (t + 1, problems);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the terrain view never settled");
}

#[test]
fn stream_terrain_view_reports_failed_chunks_once_per_cause() {
    // The source's ground is in view, but no cell or collision chunk is near
    // the focus: only view chunks are made, and those over high ground fail,
    // each at its own sample. Their problems share one subject.
    let at = Vec3::new(600.0, 0.0, 600.0);
    let mut by_hand = streamed(overflowing_graph(), at.x, at.z, json!([]));
    let w = &mut by_hand.world;
    let e = w.entity("terrain").unwrap();
    let mut cache = source::DescribeCache::new();
    let s = source::describe(w, e, &terrain_config(), &mut cache).unwrap();
    let pool = re_core::TaskPool::new(2).unwrap();
    let mut view = terrain::TerrainState::default();
    let failures = view.step(w, &[s], &[at], 1024.0, &pool, terrain::Pace::Complete);
    let causes: std::collections::BTreeSet<&str> = failures
        .iter()
        .map(|(_, d)| d.related[0].message.as_str())
        .collect();
    assert!(causes.len() > 1, "{causes:?}");
    assert!(failures.iter().all(|(s, _)| *s == failures[0].0));

    // The streamer reports the first of them.
    let mut st = streamed(overflowing_graph(), at.x, at.z, json!([]));
    let w = &mut st.world;
    let (t, problems) = run_view(w, 0);
    assert!(loaded(w).is_empty());
    let failed = w.ecs.resource::<Streamer>().view_missing();
    assert_eq!(failed, failures.len());
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert_eq!(problems[0].code.to_string(), "RE4307");
    assert!(
        problems[0].message.contains("terrain view chunk"),
        "{}",
        problems[0].message
    );
    // Out of view and back: the chunks are made again and fail the same
    // way, unreported.
    move_to(w, 1900.0, 1900.0);
    let (t, away) = run_view(w, t);
    assert_eq!(w.ecs.resource::<Streamer>().view_missing(), 0);
    move_to(w, at.x, at.z);
    let (t, back) = run_view(w, t);
    assert_eq!(w.ecs.resource::<Streamer>().view_missing(), failed);
    assert!(away.is_empty() && back.is_empty(), "{away:#?} {back:#?}");
    // Another seed fails again, with other inputs: one more report.
    w.set("terrain", "Procedural", "seed", Value::Int(8))
        .unwrap();
    let (_, again) = run_view(w, t);
    assert_eq!(again.len(), 1, "{again:#?}");
}

#[test]
fn stream_terrain_view_goes_with_its_source_or_when_turned_off() {
    // The source's ground is in view, but no cell or collision chunk is near
    // the focus: the view is all the manager holds.
    let mut st = streamed(chunked_graph(), 600.0, 600.0, json!([]));
    let w = &mut st.world;
    let t = settle_view(w, 0);
    assert!(loaded(w).is_empty());
    // Turned off, the view goes at the next step.
    w.ecs.resource_mut::<Streamer>().set_view(false);
    assert!(
        !w.ecs.resource::<Streamer>().is_idle(),
        "its chunks are still drawn"
    );
    step(w, t, &terrain_config());
    assert!(!w.ecs.contains_resource::<TerrainView>());
    assert!(w.ecs.resource::<Streamer>().is_idle());
    // On again, and the source goes: a world without streamed sources steps
    // until its manager is idle.
    w.ecs.resource_mut::<Streamer>().set_view(true);
    let t = settle_view(w, t + 1);
    let terrain = w.entity("terrain").unwrap();
    w.despawn_runtime(terrain).unwrap();
    assert!(
        !w.ecs.resource::<Streamer>().is_idle(),
        "its chunks are still drawn"
    );
    step(w, t, &terrain_config());
    assert!(w.ecs.resource::<TerrainView>().is_empty());
    assert!(w.ecs.resource::<Streamer>().is_idle());
}

#[test]
fn stream_terrain_mesh_names_name_one_mesh_in_every_world() {
    // A play copy draws with the live app's GPU side, which keeps meshes by
    // name: worlds whose chunks differ share no name, and worlds whose chunks
    // are the same share them all.
    let complete = |seed: i64| {
        let mut st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
        let w = &mut st.world;
        w.set("terrain", "Procedural", "seed", Value::Int(seed))
            .unwrap();
        w.ecs
            .insert_resource(Streamer::new(terrain_config()).unwrap());
        complete_view(w);
        drawn(w)
    };
    let (a, b) = (complete(7), complete(8));
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.0, y.0);
        assert_ne!(x.5, y.5, "{:?}: another seed, other heights", x.0);
        assert!(!a.iter().any(|z| z.4 == y.4), "{} is in both", y.4);
    }
    assert_eq!(complete(7), a);
}

fn collision_config() -> StreamConfig {
    StreamConfig {
        physics_radius: 100.0,
        apply_chunks: 1,
        ..config()
    }
}

/// The collision chunks of `terrain`: key and heightfield, in key order.
fn collision_chunks(w: &World) -> Vec<(String, Arc<re_geom::Heightfield>)> {
    let terrain = w.entity("terrain").unwrap();
    let store = w.ecs.resource::<GraphStore>();
    w.terrain_chunks(terrain)
        .into_iter()
        .map(|k| {
            let e = w.terrain_chunk(terrain, &k).unwrap();
            let c = w.ecs.get::<re_scene::Collider>(e).unwrap();
            assert_eq!(c.shape, re_scene::ColliderShape::Heightfield);
            assert!(collider_precision(&c.source).is_some(), "{}", c.source);
            let h = store
                .heightfield(&c.source)
                .expect("the heightfield is stored");
            (k, h)
        })
        .collect()
}

fn collider_names(w: &World) -> Vec<String> {
    let terrain = w.entity("terrain").unwrap();
    w.terrain_chunks(terrain)
        .iter()
        .map(|k| {
            let e = w.terrain_chunk(terrain, k).unwrap();
            w.ecs.get::<re_scene::Collider>(e).unwrap().source.clone()
        })
        .collect()
}

#[test]
fn stream_terrain_collision_chunks_apply_at_their_tick_within_the_budget() {
    let mut st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    let w = &mut st.world;
    let cfg = collision_config();
    let mut counts = Vec::new();
    for t in 0..8u64 {
        let r = step(w, t, &cfg);
        assert!(r.diagnostics.is_empty(), "tick {t}: {:#?}", r.diagnostics);
        w.update();
        counts.push(collision_chunks(w).len());
    }
    // Requested at tick 0 (four 256 m chunks touch a focus at the origin),
    // applied from tick 3, one per tick.
    assert_eq!(counts, [0, 0, 0, 1, 2, 3, 4, 4]);
    let keys: Vec<String> = collision_chunks(w).into_iter().map(|c| c.0).collect();
    assert_eq!(keys, ["t/-1_-1", "t/-1_0", "t/0_-1", "t/0_0"]);
    let names = collider_names(w);
    // Far away, they go (one per tick) once past the margin.
    move_to(w, 1200.0, 1200.0);
    for t in 8..14u64 {
        step(w, t, &cfg);
        w.update();
    }
    assert!(collision_chunks(w).is_empty());
    let store = w.ecs.resource::<GraphStore>();
    assert!(names.iter().all(|n| store.heightfield(n).is_none()));
}

#[test]
fn stream_terrain_collision_chunks_share_their_edges() {
    let mut st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    let w = &mut st.world;
    for t in 0..10u64 {
        step(w, t, &collision_config());
        w.update();
    }
    let chunks: BTreeMap<String, Arc<re_geom::Heightfield>> =
        collision_chunks(w).into_iter().collect();
    let n = 65usize;
    let (a, b) = (&chunks["t/-1_0"], &chunks["t/0_0"]);
    assert_eq!(a.n, 65);
    // Local to the chunk entity, which sits at the chunk's corner.
    assert_eq!(a.offset, Vec3::ZERO);
    let corner = |k: &str| match w.get(&format!("terrain/{k}"), "Transform", "translation") {
        Ok(Value::Vec3(v)) => v,
        other => panic!("{other:?}"),
    };
    assert_eq!(corner("t/-1_0"), Vec3::new(-256.0, 0.0, 0.0));
    assert_eq!(corner("t/0_0"), Vec3::ZERO);
    for z in 0..n {
        // a's last column is b's first, bit for bit.
        assert_eq!(
            a.samples[z * n + n - 1].to_bits(),
            b.samples[z * n].to_bits(),
            "row {z}"
        );
    }
    let c = &chunks["t/0_-1"];
    for x in 0..n {
        assert_eq!(c.samples[(n - 1) * n + x].to_bits(), b.samples[x].to_bits());
    }
}

#[test]
fn stream_terrain_collision_chunks_are_deterministic_and_regenerate_on_change() {
    let walk = || {
        let mut st = streamed(chunked_graph(), -300.0, -300.0, json!([]));
        let w = &mut st.world;
        let mut log = Vec::new();
        for t in 0..60u64 {
            let p = -300.0 + t as f32 * 10.0;
            move_to(w, p, p);
            step(w, t, &collision_config());
            w.update();
            log.push(
                collision_chunks(w)
                    .into_iter()
                    .map(|c| c.0)
                    .collect::<Vec<_>>(),
            );
        }
        (log, w.state_hash())
    };
    let (a, ha) = walk();
    let (b, hb) = walk();
    assert_eq!(a, b);
    assert_eq!(ha, hb);
    assert!(a.iter().any(|l| l.is_empty()) && a.iter().any(|l| !l.is_empty()));

    // A new seed gives new heights at a deterministic tick, and the old
    // heightfields are released.
    let mut st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    let w = &mut st.world;
    for t in 0..10u64 {
        step(w, t, &collision_config());
        w.update();
    }
    let before = collision_chunks(w);
    let old = collider_names(w);
    w.set("terrain", "Procedural", "seed", Value::Int(8))
        .unwrap();
    for t in 10..20u64 {
        step(w, t, &collision_config());
        w.update();
    }
    let after = collision_chunks(w);
    assert_eq!(before.len(), after.len());
    assert_ne!(before[0].1.samples, after[0].1.samples);
    let store = w.ecs.resource::<GraphStore>();
    for n in &old {
        assert!(store.heightfield(n).is_none(), "{n} is kept");
    }
}

#[test]
fn stream_terrain_collision_precision_is_only_for_chunks() {
    assert_eq!(
        collider_precision("derived:terrain-collider/terrain/0_0/0123456789abcdef"),
        Some(re_types::physics::HeightfieldPrecision::FINE)
    );
    assert_eq!(collider_precision("derived:0123"), None);
}

/// A height field, eroded (light settings, so debug tests stay quick).
fn eroded_graph() -> GraphDoc {
    graph(
        &[
            (
                "h",
                "terrain.heightfield",
                json!({ "cell": 4.0, "height": 40.0, "scale": 0.01 }),
            ),
            (
                "e",
                "terrain.erode",
                json!({ "spacing": 4.0, "droplets": 0.05, "steps": 16 }),
            ),
            ("ground", "terrain.chunks", json!({ "max_level": 2 })),
        ],
        &[
            ("h", "field", "e", "field"),
            ("e", "field", "ground", "field"),
        ],
    )
}

#[test]
fn stream_erosion_windows_agree_across_levels_and_tiles() {
    let registry = Arc::new(crate::NodeRegistry::m1());
    let graph = ProcGraph::from_doc(&eroded_graph(), &registry).unwrap();
    let ev = Evaluator::new(registry);
    let eval = |origin: [i64; 2], n: u32, spacing: f32, id: &str| {
        let opts = EvalOptions {
            seed: Some(7),
            domain: Some(Domain { origin, n, spacing }),
            serial: true,
            ..Default::default()
        };
        let e = ev.evaluate(&graph, &opts);
        assert!(e.ok(), "{:#?}", e.report.diagnostics);
        match e.output(id) {
            Some(Output::Field(f)) => f.clone(),
            other => panic!("{other:?}"),
        }
    };
    // Windows either side of the tile border at x = 1024 m (256 cells of
    // 4 m), at 4 m and at 8 m.
    let a = eval([240, 0], 17, 4.0, "e");
    let b = eval([256, 0], 17, 4.0, "e");
    let coarse = eval([120, 0], 17, 8.0, "e");
    let raw = eval([240, 0], 17, 4.0, "h");
    for z in 0..17 {
        assert_eq!(a.0.sample(16, z).to_bits(), b.0.sample(0, z).to_bits());
    }
    for z in (0..17).step_by(2) {
        for x in (0..17).step_by(2) {
            // Coarse sample (x/2 + 120) is fine sample (x + 240).
            assert_eq!(
                coarse.0.sample(x / 2, z / 2).to_bits(),
                a.0.sample(x, z).to_bits(),
                "({x}, {z})"
            );
        }
    }
    assert_ne!(a.0.samples, raw.0.samples, "erosion changes the terrain");
    // The same again from a fresh evaluator (no cache): bit for bit.
    let ev2 = Evaluator::new(Arc::new(crate::NodeRegistry::m1()));
    let opts = EvalOptions {
        seed: Some(7),
        domain: Some(Domain {
            origin: [240, 0],
            n: 17,
            spacing: 4.0,
        }),
        ..Default::default()
    };
    match ev2.evaluate(&graph, &opts).output("e") {
        Some(Output::Field(f)) => assert_eq!(f.0.samples, a.0.samples),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stream_eroded_terrain_streams_with_matching_collision_seams() {
    let mut st = streamed(eroded_graph(), 0.0, 0.0, json!([]));
    let w = &mut st.world;
    for t in 0..10u64 {
        let r = step(w, t, &collision_config());
        assert!(r.diagnostics.is_empty(), "tick {t}: {:#?}", r.diagnostics);
        w.update();
    }
    let chunks: BTreeMap<String, Arc<re_geom::Heightfield>> =
        collision_chunks(w).into_iter().collect();
    assert_eq!(chunks.len(), 4);
    let (a, b) = (&chunks["t/-1_0"], &chunks["t/0_0"]);
    for z in 0..65 {
        assert_eq!(
            a.samples[z * 65 + 64].to_bits(),
            b.samples[z * 65].to_bits()
        );
    }
}

#[test]
fn stream_origin_follows_the_focus_on_its_grid() {
    let mut st = streamed(world_graph(), 0.0, 0.0, json!([]));
    let w = &mut st.world;
    let origin_after = |w: &mut World, x: f32, z: f32, t: u64| {
        move_to(w, x, z);
        let r = step(w, t, &config());
        assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
        assert_eq!(r.origin, w.ecs.resource::<Streamer>().origin());
        r.origin
    };
    assert_eq!(origin_after(w, 0.0, 0.0, 0), Vec3::ZERO);
    // Within origin_shift (2048 m) on both axes: it stays.
    assert_eq!(origin_after(w, 2000.0, -2040.0, 1), Vec3::ZERO);
    // Past it: the grid point (1024 m) nearest the focus.
    assert_eq!(
        origin_after(w, 2100.0, -300.0, 2),
        Vec3::new(2048.0, 0.0, 0.0)
    );
    assert_eq!(
        origin_after(w, 4000.0, -300.0, 3),
        Vec3::new(2048.0, 0.0, 0.0)
    );
    assert_eq!(
        origin_after(w, 4200.0, -2400.0, 4),
        Vec3::new(4096.0, 0.0, -2048.0)
    );
    // A fork carries it.
    let fork = w.fork();
    let f = w.ecs.resource::<Streamer>().forked(w, &fork);
    assert_eq!(f.origin(), Vec3::new(4096.0, 0.0, -2048.0));
}

/// A view focus picks cells, not the simulation: the floating origin, the
/// way each focus is going and terrain collision stay as they are while it
/// is set, and follow the world's focuses again once it is cleared.
#[test]
fn stream_a_view_focus_leaves_the_simulation_state_as_it_is() {
    let mut st = streamed(chunked_graph(), 0.0, 0.0, json!([]));
    let w = &mut st.world;
    let cfg = collision_config();
    let steps = |w: &mut World, ticks: std::ops::Range<u64>| {
        for t in ticks {
            let r = step(w, t, &cfg);
            assert!(r.diagnostics.is_empty(), "tick {t}: {:#?}", r.diagnostics);
            w.update();
        }
    };
    steps(w, 0..8);
    let chunks = collision_chunks(w);
    assert_eq!(chunks.len(), 4);

    // A view focus beyond origin_shift and physics' reach neither moves the
    // origin nor reports RE4308, and the player's collision chunks stay in
    // place while it is set, even when the player moves away.
    let view = Focus {
        position: Vec2::new(4500.0, 0.0),
        forward: Vec2::ZERO,
    };
    w.ecs.resource_mut::<Streamer>().set_view_focus(Some(view));
    move_to(w, 2100.0, -300.0);
    steps(w, 8..20);
    assert_eq!(w.ecs.resource::<Streamer>().origin(), Vec3::ZERO);
    let same: Vec<String> = collision_chunks(w).into_iter().map(|c| c.0).collect();
    let before: Vec<String> = chunks.into_iter().map(|c| c.0).collect();
    assert_eq!(same, before);

    // Cleared: the origin follows the player, and the chunks it left go.
    w.ecs.resource_mut::<Streamer>().set_view_focus(None);
    steps(w, 20..32);
    assert_eq!(
        w.ecs.resource::<Streamer>().origin(),
        Vec3::new(2048.0, 0.0, 0.0)
    );
    assert!(collision_chunks(w).is_empty());
}

#[test]
fn stream_origin_reports_focuses_out_of_reach_and_content_out_of_precision() {
    let mut st = streamed(
        world_graph(),
        0.0,
        0.0,
        json!([{
            "id": "player2",
            "name": "Player 2",
            "components": {
                "StreamFocus": {},
                "Transform": { "translation": [12000.0, 0.0, 0.0] }
            }
        }]),
    );
    let w = &mut st.world;
    let r = step(w, 0, &config());
    // The origin sits at the mean (6000 m, so 6144 m): both focuses are
    // more than 5 km from it.
    assert_eq!(r.origin, Vec3::new(6144.0, 0.0, 0.0));
    let codes: Vec<_> = r.diagnostics.iter().map(|d| d.code).collect();
    assert_eq!(
        codes,
        [crate::codes::ORIGIN_RANGE, crate::codes::ORIGIN_RANGE]
    );
    assert!(
        r.diagnostics[1]
            .message
            .contains("focus `player2` is 5.9 km"),
        "{}",
        r.diagnostics[1].message
    );
    // Reported once.
    assert!(step(w, 1, &config()).diagnostics.is_empty());

    // An unbounded source with a focus 17 km out: its cells there are
    // beyond the precision limit.
    w.set("terrain", "Streamed", "bounds", Value::Float(0.0))
        .unwrap();
    w.set(
        "player2",
        "Transform",
        "translation",
        Value::Vec3(Vec3::new(0.0, 0.0, 0.0)),
    )
    .unwrap();
    move_to(w, 17_000.0, 0.0);
    let r = step(w, 2, &config());
    let far: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.code == crate::codes::PRECISION_LIMIT)
        .collect();
    assert!(!far.is_empty(), "{:#?}", r.diagnostics);
    assert!(far[0].message.contains("beyond the 16 km precision limit"));
}

#[test]
fn stream_reports_outdated_nodes_once() {
    // Streamed cells skip the generator, so the streamer says itself that
    // `terrain.mesh` 1 still puts streamed cells in world space.
    let mut doc = world_graph();
    for n in &mut doc.nodes {
        if n.kind == "terrain.mesh" {
            n.version = 1;
        }
    }
    let mut st = streamed(doc, 32.0, 32.0, json!([]));
    let r = run(&mut st.world, 0..5);
    let codes: Vec<Vec<String>> = r
        .iter()
        .map(|r| r.diagnostics.iter().map(|d| d.code.to_string()).collect())
        .collect();
    assert_eq!(codes[0], ["RE4019"], "{:#?}", r[0].diagnostics);
    assert!(codes[1..].iter().all(Vec::is_empty), "{codes:?}");
    let d = &r[0].diagnostics[0];
    assert!(
        d.message.contains("node `ground` (terrain.mesh)"),
        "{}",
        d.message
    );
}

#[test]
fn stream_cell_bounds_span_the_terrain_heights() {
    // Terrain lifted to 100 m and more: the cell's bounds hold its mesh's
    // heights, not the corner-local entity's origin at y = 0.
    let doc = graph(
        &[
            (
                "h",
                "terrain.heightfield",
                json!({ "cell": 4.0, "height": 40.0 }),
            ),
            (
                "lift",
                "field.remap",
                json!({ "mode": "curve", "points": [[0.0, 100.0], [40.0, 140.0]] }),
            ),
            ("ground", "terrain.mesh", json!({})),
        ],
        &[
            ("h", "field", "lift", "field"),
            ("lift", "field", "ground", "field"),
        ],
    );
    let mut st = streamed(doc, 32.0, 32.0, json!([]));
    let mut r = run(&mut st.world, 0..3);
    r.push(step(&mut st.world, 3, &config()));
    assert!(r.iter().all(|r| r.diagnostics.is_empty()), "{r:?}");
    let events = st.world.ecs.resource::<StreamEvents>();
    assert!(!events.loaded.is_empty());
    for cell in &events.loaded {
        assert!(
            cell.min.y >= 99.0 && cell.max.y <= 141.0 && cell.max.y > cell.min.y,
            "{}: {} to {}",
            cell.key,
            cell.min.y,
            cell.max.y
        );
    }
}
