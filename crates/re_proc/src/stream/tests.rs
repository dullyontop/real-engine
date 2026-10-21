//! Streaming tests (ADR-0042). Names start with `stream` where the module
//! path does not already say it, so `cargo test -p re_proc stream` runs them.

use std::path::PathBuf;
use std::sync::Arc;

use glam::Vec3;
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
            "id": id, "kind": kind, "version": 1, "params": params,
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
            deadline: None,
            domain: Some(Domain {
                origin,
                n,
                spacing: 4.0,
            }),
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
fn stream_refuses_meshes_made_at_the_origin() {
    let doc = graph(&[("rock", "mesh.primitive", json!({}))], &[]);
    let mut st = streamed(doc, 0.0, 0.0, json!([]));
    let r = step(&mut st.world, 0, &config());
    assert_eq!(r.diagnostics[0].code.to_string(), "RE4301");
}
