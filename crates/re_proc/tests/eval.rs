//! Evaluation: determinism (D1), seed stability, incremental re-evaluation,
//! caches and their keys, budgets, input warnings, `points.instance` and
//! golden outputs per node.

mod support;

use std::sync::Arc;
use std::time::Duration;

use re_core::time::Instant;
use re_proc::{
    CacheState, CustomKind, Domain, EvalOptions, Evaluator, Field, NodeDef, NodeRegistry, Output,
    PortType, ProcGraph, Streaming, fragment_hash_hex,
};
use re_reflect::Value;
use re_types::graph::GraphDoc;
use serde_json::{Value as Json, json};
use support::{evaluator, fixtures, graph, island, output_hash};

fn compile(doc: &GraphDoc) -> ProcGraph {
    ProcGraph::from_doc(doc, &NodeRegistry::m1()).expect("graph is valid")
}

fn opts() -> EvalOptions {
    EvalOptions {
        root: Some(fixtures()),
        ..Default::default()
    }
}

fn example() -> GraphDoc {
    GraphDoc::from_json(include_str!("../examples/island.graph.json")).unwrap()
}

fn example_opts() -> EvalOptions {
    EvalOptions {
        root: Some(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")),
        ..Default::default()
    }
}

/// The fragment hash of one evaluation.
fn fragment_hash(ev: &Evaluator, g: &ProcGraph, o: &EvalOptions) -> String {
    let world = re_scene::World::new();
    let e = ev.evaluate(g, o);
    assert!(e.ok(), "{:#?}", e.report.diagnostics);
    let (frag, _) = e.scene_fragment(&world.registry).expect("fragment");
    fragment_hash_hex(&frag)
}

#[test]
fn d1_same_graph_and_seed_give_the_same_fragment() {
    let g = compile(&example());
    let o = example_opts();
    let a = fragment_hash(&evaluator(1), &g, &o);
    let b = fragment_hash(&evaluator(1), &g, &o);
    let c = fragment_hash(&evaluator(4), &g, &o);
    assert_eq!(a, b, "two runs");
    assert_eq!(a, c, "1 vs 4 threads");

    // Every node output, not only the fragment, is identical.
    let one = evaluator(1).evaluate(&g, &o);
    let four = evaluator(4).evaluate(&g, &o);
    for id in g.nodes.keys() {
        assert_eq!(
            output_hash(one.output(id).unwrap()),
            output_hash(four.output(id).unwrap()),
            "node {id}"
        );
    }

    // Another seed gives another island.
    let other = EvalOptions {
        seed: Some(99),
        ..example_opts()
    };
    assert_ne!(a, fragment_hash(&evaluator(2), &g, &other));
}

#[test]
fn adding_an_unrelated_node_changes_no_other_output() {
    let base = island(7);
    let mut extended = base.clone();
    let extra = graph(
        7,
        &[
            ("extra", "scatter.poisson", json!({ "radius": 3.0 })),
            ("noise", "terrain.heightfield", json!({ "size": 16 })),
        ],
        &[("noise", "field", "extra", "field")],
    );
    // Insert the new nodes first, so document order changes too.
    let mut nodes = extra.nodes.clone();
    nodes.extend(extended.nodes);
    extended.nodes = nodes;
    extended.edges.extend(extra.edges);

    let a = compile(&base);
    let b = compile(&extended);
    let ea = evaluator(2).evaluate(&a, &opts());
    let eb = evaluator(2).evaluate(&b, &opts());
    assert!(ea.ok() && eb.ok());
    for id in a.nodes.keys() {
        assert_eq!(
            output_hash(ea.output(id).unwrap()),
            output_hash(eb.output(id).unwrap()),
            "node {id} changed when unrelated nodes were added"
        );
        assert_eq!(
            ea.report.node(id).unwrap().key,
            eb.report.node(id).unwrap().key
        );
    }
}

#[test]
fn changing_one_parameter_reevaluates_only_downstream_nodes() {
    let ev = evaluator(2);
    let doc = island(3);
    let first = ev.evaluate(&compile(&doc), &opts());
    assert!(first.ok());
    assert_eq!(first.report.misses, doc.nodes.len());
    assert_eq!(first.report.memory_hits, 0);

    let again = ev.evaluate(&compile(&doc), &opts());
    assert_eq!(again.report.memory_hits, doc.nodes.len());
    assert_eq!(again.report.misses, 0);

    // `mask` feeds land -> ground, grid -> crates.
    let mut changed = doc.clone();
    changed
        .nodes
        .iter_mut()
        .find(|n| n.id == "mask")
        .unwrap()
        .params
        .insert("radius".into(), json!(0.6));
    let third = ev.evaluate(&compile(&changed), &opts());
    assert!(third.ok());
    let downstream = ["mask", "land", "ground", "grid", "crates"];
    for n in &third.report.nodes {
        let expected = if downstream.contains(&n.id.as_str()) {
            CacheState::Miss
        } else {
            CacheState::Memory
        };
        assert_eq!(n.cache, expected, "node {}", n.id);
    }
    assert_eq!(third.report.misses, downstream.len());
    assert_eq!(third.report.memory_hits, doc.nodes.len() - downstream.len());
}

#[test]
fn a_new_seed_reevaluates_seeded_nodes_only() {
    let ev = evaluator(2);
    let mut o = opts();
    ev.evaluate(&compile(&island(1)), &o);
    o.seed = Some(2);
    let e = ev.evaluate(&compile(&island(1)), &o);
    // The mask, the primitive, its material and the water plane take no seed.
    for id in ["mask", "box", "painted", "water"] {
        assert_eq!(e.report.node(id).unwrap().cache, CacheState::Memory, "{id}");
    }
    assert_eq!(e.report.node("h").unwrap().cache, CacheState::Miss);
}

#[test]
fn the_disk_cache_serves_a_new_evaluator() {
    let dir = tempfile::tempdir().unwrap();
    let g = compile(&island(5));
    let make = || evaluator(2).with_disk_cache(dir.path().to_path_buf());
    let first = make().evaluate(&g, &opts());
    assert!(first.ok());
    assert_eq!(first.report.disk_hits, 0);

    let second = make().evaluate(&g, &opts());
    assert!(second.ok(), "{:#?}", second.report.diagnostics);
    // Fields, meshes and points come from disk; fragments are rebuilt.
    assert!(second.report.disk_hits >= 5, "{:#?}", second.report);
    assert_eq!(second.report.node("h").unwrap().cache, CacheState::Disk);
    assert_eq!(second.report.node("box").unwrap().cache, CacheState::Disk);
    assert_eq!(second.report.node("grid").unwrap().cache, CacheState::Disk);
    for id in g.nodes.keys() {
        assert_eq!(
            output_hash(first.output(id).unwrap()),
            output_hash(second.output(id).unwrap()),
            "node {id}"
        );
    }

    // A corrupt entry is recomputed, never trusted.
    for entry in walk(dir.path()) {
        std::fs::write(&entry, b"garbage").unwrap();
    }
    let third = make().evaluate(&g, &opts());
    assert!(third.ok(), "{:#?}", third.report.diagnostics);
    assert_eq!(third.report.disk_hits, 0);
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn the_memory_budget_evicts_entries_the_last_evaluation_did_not_use() {
    let ev = Evaluator::new(Arc::new(NodeRegistry::m1())).with_memory_budget(1);
    let g = compile(&island(5));
    assert!(ev.evaluate(&g, &opts()).ok());
    assert_eq!(
        ev.memory_entries(),
        g.nodes.len(),
        "the running evaluation's outputs are kept"
    );
    // A new seed: the 5 seeded nodes recompute (mask, box, painted and
    // water take no seed); their old outputs go.
    let o = EvalOptions {
        seed: Some(6),
        ..opts()
    };
    let e = ev.evaluate(&g, &o);
    assert_eq!(e.report.memory_hits, 4);
    assert_eq!(ev.memory_entries(), g.nodes.len());
    ev.clear_memory();
    assert_eq!(ev.memory_entries(), 0);
}

/// Parameters are hashed for the cache as their type writes them back:
/// `16` and `16.0` for a float, and a left-out default, are one entry, in
/// memory and on disk.
#[test]
fn parameters_written_differently_share_one_cache_entry() {
    let doc = |params: Json| graph(1, &[("h", "terrain.heightfield", params)], &[]);
    // `height` defaults to 30.
    let variants = [
        json!({ "size": 17, "cell": 16, "height": 30 }),
        json!({ "size": 17, "cell": 16.0, "height": 30.0 }),
        json!({ "size": 17, "cell": 16.0 }),
    ];
    let ev = evaluator(2);
    for (i, params) in variants.iter().enumerate() {
        let e = ev.evaluate(&compile(&doc(params.clone())), &opts());
        assert!(e.ok(), "{:#?}", e.report.diagnostics);
        let expected = if i == 0 {
            CacheState::Miss
        } else {
            CacheState::Memory
        };
        assert_eq!(e.report.node("h").unwrap().cache, expected, "{params}");
    }
    let other = doc(json!({ "size": 17, "cell": 16.0, "height": 31 }));
    let e = ev.evaluate(&compile(&other), &opts());
    assert_eq!(e.report.node("h").unwrap().cache, CacheState::Miss);

    let dir = tempfile::tempdir().unwrap();
    let make = || evaluator(2).with_disk_cache(dir.path().to_path_buf());
    assert!(
        make()
            .evaluate(&compile(&doc(variants[0].clone())), &opts())
            .ok()
    );
    let e = make().evaluate(&compile(&doc(variants[2].clone())), &opts());
    assert_eq!(e.report.node("h").unwrap().cache, CacheState::Disk);
}

/// A kind with no Rust parameter type (a Luau node) is hashed for the cache
/// as its schema writes the parameters: left-out properties at their
/// defaults, `number`s as floats and `integer`s as integers.
#[test]
fn custom_kinds_share_cache_entries_through_their_schema() {
    let mut registry = NodeRegistry::m1();
    registry.register(NodeDef::custom(CustomKind {
        kind: "test.flat".into(),
        version: 1,
        oldest: 1,
        streaming: Streaming::Whole,
        implementation: [7; 32],
        doc: "A flat field.".into(),
        inputs: Vec::new(),
        output: PortType::Field,
        uses_seed: false,
        params_schema: json!({
            "type": "object",
            "properties": {
                "height": { "type": "number", "default": 16 },
                "size": { "type": "integer", "default": 3 },
            },
            "additionalProperties": false,
        }),
        check: Arc::new(|_| Ok(())),
        eval: Arc::new(|_, p| {
            let number =
                |name: &str, default: f64| p.get(name).and_then(Json::as_f64).unwrap_or(default);
            let size = number("size", 3.0) as u32;
            let values = vec![number("height", 16.0) as f32; (size * size) as usize];
            Ok(Output::Field(Arc::new(Field::new(size, 1.0, values)?)))
        }),
        migrate: None,
    }));
    let registry = Arc::new(registry);
    let ev = Evaluator::new(registry.clone());
    let cache = |params: Json| {
        let doc = graph(1, &[("f", "test.flat", params)], &[]);
        let g = ProcGraph::from_doc(&doc, &registry).expect("graph is valid");
        let e = ev.evaluate(&g, &EvalOptions::default());
        assert!(e.ok(), "{:#?}", e.report.diagnostics);
        e.report.node("f").unwrap().cache
    };
    assert_eq!(cache(json!({ "height": 16, "size": 3 })), CacheState::Miss);
    assert_eq!(
        cache(json!({ "height": 16.0, "size": 3.0 })),
        CacheState::Memory
    );
    assert_eq!(cache(json!({})), CacheState::Memory);
    assert_eq!(cache(json!({ "height": 17 })), CacheState::Miss);
}

/// `field.combine` warns (`RE4030`) when its fields cover different areas,
/// whether it runs or its output comes from the cache. The same area at
/// another cell size is fine, and so is an island mask, a relative mask made
/// to be stretched.
#[test]
fn combining_fields_of_different_areas_warns_with_re4030() {
    let combine = |kind: &str, b: Json| {
        graph(
            1,
            &[
                (
                    "a",
                    "terrain.heightfield",
                    json!({ "size": 33, "cell": 2.0 }),
                ),
                ("b", kind, b),
                ("land", "field.combine", json!({ "op": "mul" })),
            ],
            &[("a", "field", "land", "a"), ("b", "field", "land", "b")],
        )
    };
    let ev = evaluator(2);
    let warnings = |doc: &GraphDoc| {
        let e = ev.evaluate(&compile(doc), &opts());
        assert!(e.ok(), "{:#?}", e.report.diagnostics);
        e.report
            .all_diagnostics()
            .filter(|d| d.code.to_string() == "RE4030")
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    };
    let smaller = combine("terrain.heightfield", json!({ "size": 17, "cell": 2.0 }));
    let first = warnings(&smaller);
    assert_eq!(
        first,
        [
            "node `land` (field.combine): `a` covers 64 m from (-32, -32) and `b` 32 m from (-16, -16): `b` is stretched over `a` by normalised position, not matched by world position"
        ]
    );
    assert_eq!(warnings(&smaller), first, "from the memory cache");
    let coarser = combine("terrain.heightfield", json!({ "size": 17, "cell": 4.0 }));
    assert!(warnings(&coarser).is_empty());
    let mask = combine("terrain.island_mask", json!({ "size": 17 }));
    assert!(warnings(&mask).is_empty());
}

/// A streamed cell node reads its input over its window grown by an apron:
/// a nested evaluation through the same caches. It evicts nothing the
/// evaluation it runs in uses, so evaluating the window again is all memory
/// hits even on a one-byte budget.
#[test]
fn nested_reads_never_evict_what_their_evaluation_uses() {
    let doc = graph(
        7,
        &[
            (
                "h",
                "terrain.heightfield",
                json!({ "cell": 4.0, "height": 60.0 }),
            ),
            ("ground", "terrain.mesh", json!({})),
        ],
        &[("h", "field", "ground", "field")],
    );
    let g = compile(&doc);
    let ev = Evaluator::new(Arc::new(NodeRegistry::m1())).with_memory_budget(1);
    let o = EvalOptions {
        domain: Some(Domain {
            origin: [0, 0],
            n: 17,
            spacing: 4.0,
        }),
        ..opts()
    };
    let first = ev.evaluate(&g, &o);
    assert!(first.ok(), "{:#?}", first.report.diagnostics);
    assert_eq!(first.report.misses, 2);
    let second = ev.evaluate(&g, &o);
    assert!(second.ok(), "{:#?}", second.report.diagnostics);
    assert_eq!(second.report.memory_hits, 2, "{:#?}", second.report.nodes);
    assert_eq!(
        output_hash(first.output("ground").unwrap()),
        output_hash(second.output("ground").unwrap())
    );
}

#[test]
fn a_passed_deadline_skips_nodes_with_re4016() {
    let g = compile(&island(5));
    let o = EvalOptions {
        deadline: Some(Instant::now() - Duration::from_millis(1)),
        ..opts()
    };
    let e = evaluator(2).evaluate(&g, &o);
    assert!(!e.ok());
    assert_eq!(e.report.first_error().unwrap().code.to_string(), "RE4016");
    assert!(
        e.report
            .nodes
            .iter()
            .all(|n| n.cache == CacheState::Skipped)
    );
    assert!(e.fragment().is_err());
}

#[test]
fn points_instance_gives_keys_and_components_from_the_prefab() {
    let doc = graph(
        11,
        &[
            (
                "flat",
                "terrain.heightfield",
                json!({ "size": 16, "cell": 2.0, "height": 0.0 }),
            ),
            (
                "grid",
                "scatter.grid_jitter",
                json!({ "spacing": 10.0, "jitter": 0.0 }),
            ),
            (
                "crates",
                "points.instance",
                json!({
                    "prefab": "prefabs/crate.prefab.json",
                    "random_yaw": false,
                    "scale_min": 2.0,
                    "scale_max": 2.0,
                    "offset_y": 1.0
                }),
            ),
        ],
        &[
            ("flat", "field", "grid", "field"),
            ("grid", "points", "crates", "points"),
        ],
    );
    let e = evaluator(1).evaluate(&compile(&doc), &opts());
    assert!(e.ok(), "{:#?}", e.report.diagnostics);
    let world = re_scene::World::new();
    let (frag, assets) = e.scene_fragment(&world.registry).unwrap();
    assert!(assets.is_empty());

    // 3 x 3 points on a 30 m field at 10 m spacing; each instance is a
    // container plus the prefab's two entities.
    let containers: Vec<_> = frag.iter().filter(|n| n.parent_key.is_none()).collect();
    assert_eq!(
        containers.len(),
        9,
        "{:#?}",
        frag.iter().map(|n| &n.key).collect::<Vec<_>>()
    );
    assert_eq!(frag.len(), 27);
    for c in &containers {
        let pid = c
            .key
            .strip_prefix("crates/")
            .expect("instance keys are <node>/<point id>");
        assert!(pid.parse::<u64>().is_ok(), "{}", c.key);
        let t = &c
            .components
            .iter()
            .find(|(n, _)| n == "Transform")
            .expect("a Transform")
            .1;
        let scale = t.iter().find(|(f, _)| f == "scale").map(|(_, v)| v.clone());
        assert_eq!(scale, Some(Value::Vec3(glam::Vec3::splat(2.0))), "{t:?}");
        let translation = t.iter().find(|(f, _)| f == "translation").unwrap();
        let Value::Vec3(t) = translation.1 else {
            panic!("translation is a Vec3: {translation:?}")
        };
        assert_eq!(t.y, 1.0, "flat ground at 0 plus offset_y");

        let crate_key = format!("{}/crate", c.key);
        let lid_key = format!("{}/lid", c.key);
        let body = frag
            .iter()
            .find(|n| n.key == crate_key)
            .expect("the prefab root");
        assert_eq!(body.parent_key.as_deref(), Some(c.key.as_str()));
        assert_eq!(body.name, "Crate");
        let names: Vec<_> = body.components.iter().map(|(n, _)| n.as_str()).collect();
        for comp in ["Collider", "RigidBody", "Transform"] {
            assert!(names.contains(&comp), "{names:?}");
        }
        let lid = frag
            .iter()
            .find(|n| n.key == lid_key)
            .expect("the prefab child");
        assert_eq!(lid.parent_key.as_deref(), Some(crate_key.as_str()));
        assert!(lid.components.iter().any(|(n, _)| n == "MeshRef"));
    }
}

#[test]
fn a_bad_prefab_fails_with_re4014() {
    for (prefab, what) in [
        ("prefabs/missing.prefab.json", "missing file"),
        ("prefabs/broken.prefab.json", "unknown component"),
        ("../outside.prefab.json", "path escaping the root"),
    ] {
        let doc = graph(
            1,
            &[
                ("h", "terrain.heightfield", json!({ "size": 8 })),
                ("grid", "scatter.grid_jitter", json!({ "spacing": 4.0 })),
                ("i", "points.instance", json!({ "prefab": prefab })),
            ],
            &[
                ("h", "field", "grid", "field"),
                ("grid", "points", "i", "points"),
            ],
        );
        let e = evaluator(1).evaluate(&compile(&doc), &opts());
        let world = re_scene::World::new();
        let err = e.scene_fragment(&world.registry).expect_err(what);
        assert_eq!(err.code.to_string(), "RE4014", "{what}: {err:?}");
    }
}

/// Golden output hashes of every node of the example island (all 16 M1 node
/// kinds) with its fixed parameters and seed 1234. A change here means a
/// node's output changed: bump that node kind's version if intended.
#[test]
fn golden_outputs_per_node() {
    const GOLDEN: &[(&str, &str, &str)] = &[
        ("height", "terrain.heightfield", "3f7baef9657387fa"),
        ("mask", "terrain.island_mask", "1ca4423d876eceeb"),
        ("island", "field.combine", "edb4669e64c03a6d"),
        ("shore", "field.remap", "971fbd42d14be810"),
        ("terrain", "terrain.mesh", "d69d26afcf9a88b4"),
        ("water", "terrain.water_plane", "e829d2cfac475f9d"),
        ("scatter", "scatter.poisson", "2e786b66afc3f222"),
        ("kind", "points.pick", "4b41e0ac92d466f8"),
        ("props", "points.instance", "fcc7b43be89dac70"),
        ("spots", "scatter.grid_jitter", "9b0afc7c4f5b646d"),
        ("few", "points.filter", "eae39cf2006f64d2"),
        ("coins", "points.instance", "00dacbc0c5d01e3d"),
        ("plank", "mesh.primitive", "9b4c57e04c0b3e3c"),
        ("post", "mesh.primitive", "a7d9063b2afc1607"),
        ("post_end", "mesh.transform", "dc1d505c90f1875c"),
        ("dock_mesh", "mesh.merge", "5b7e614dde008b92"),
        ("dock", "material.assign", "d80ce6803193eae7"),
        ("out", "fragment.merge", "5b9bc4d45dca1c14"),
    ];
    let g = compile(&example());
    let e = evaluator(3).evaluate(&g, &example_opts());
    assert!(e.ok(), "{:#?}", e.report.diagnostics);
    let actual: Vec<(String, String, String)> = g
        .nodes
        .iter()
        .map(|(id, n)| {
            (
                id.clone(),
                n.kind.clone(),
                output_hash(e.output(id).unwrap()),
            )
        })
        .collect();
    let kinds: std::collections::BTreeSet<_> = actual.iter().map(|a| a.1.as_str()).collect();
    assert_eq!(kinds.len(), 16, "the example uses every M1 node kind");
    let expected: Vec<(String, String, String)> = GOLDEN
        .iter()
        .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
        .collect();
    assert_eq!(
        actual,
        expected,
        "\nactual:\n{}",
        actual
            .iter()
            .map(|(a, b, c)| format!("        (\"{a}\", \"{b}\", \"{c}\"),\n"))
            .collect::<String>()
    );
}
