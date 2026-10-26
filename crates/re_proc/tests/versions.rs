//! Node and generator versions (ADR-0043): a node evaluates at the version it
//! was written for, its parameters migrate in memory, old versions keep their
//! output, keys written before versioning stay valid, and the version stamp
//! changes only with what decides which code runs.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use re_proc::{
    Domain, EvalOptions, Evaluator, Field, Migration, NodeCtx, NodeDef, NodeRegistry, NodeSpec,
    Output, Params, PortDef, PortType, ProcGraph, Streaming, version_stamp,
};
use re_types::graph::GraphDoc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use support::{evaluator, output_hash};

fn example() -> GraphDoc {
    GraphDoc::from_json(include_str!("../examples/island.graph.json")).unwrap()
}

/// The example island with the kinds it has no use for added as extra sinks:
/// `terrain.chunks` (world streaming's terrain) and `terrain.erode` on the
/// shore field.
fn example_with_every_kind() -> GraphDoc {
    let mut doc: Json =
        serde_json::from_str(include_str!("../examples/island.graph.json")).unwrap();
    doc["nodes"].as_array_mut().unwrap().push(json!({
        "id": "lod", "kind": "terrain.chunks", "version": 1, "params": { "name": "Terrain" },
    }));
    doc["nodes"].as_array_mut().unwrap().push(json!({
        "id": "eroded", "kind": "terrain.erode", "version": 1,
        "params": { "droplets": 0.1, "steps": 24 },
    }));
    let edges = doc["edges"].as_array_mut().unwrap();
    edges.push(json!({ "from": ["shore", "field"], "to": ["lod", "field"] }));
    edges.push(json!({ "from": ["shore", "field"], "to": ["eroded", "field"] }));
    GraphDoc::from_json(&doc.to_string()).unwrap()
}

fn example_opts() -> EvalOptions {
    EvalOptions {
        root: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")),
        ..Default::default()
    }
}

/// One golden output hash per (kind, version) the engine evaluates, from the
/// example island's node of that kind ([`example_with_every_kind`]). Entries are only ever added: a kind
/// that changes its output gets a new version (and a new entry here), and the
/// old version keeps its output.
const GOLDEN: &[(&str, u32, &str, &str)] = &[
    ("terrain.heightfield", 1, "height", "3f7baef9657387fa"),
    ("terrain.island_mask", 1, "mask", "1ca4423d876eceeb"),
    ("field.combine", 1, "island", "edb4669e64c03a6d"),
    ("field.remap", 1, "shore", "971fbd42d14be810"),
    ("terrain.mesh", 1, "terrain", "fdab6a0896e440a4"),
    ("terrain.mesh", 2, "terrain", "8b3790f53e5b46b9"),
    ("terrain.mesh", 3, "terrain", "d69d26afcf9a88b4"),
    ("terrain.water_plane", 1, "water", "e829d2cfac475f9d"),
    ("scatter.poisson", 1, "scatter", "2e786b66afc3f222"),
    ("points.pick", 1, "kind", "4b41e0ac92d466f8"),
    ("points.instance", 1, "props", "fcc7b43be89dac70"),
    ("scatter.grid_jitter", 1, "spots", "9b0afc7c4f5b646d"),
    ("points.filter", 1, "few", "eae39cf2006f64d2"),
    ("mesh.primitive", 1, "plank", "9b4c57e04c0b3e3c"),
    ("mesh.transform", 1, "post_end", "dc1d505c90f1875c"),
    ("mesh.merge", 1, "dock_mesh", "5b7e614dde008b92"),
    ("material.assign", 1, "dock", "d80ce6803193eae7"),
    ("fragment.merge", 1, "out", "3e333807edeff974"),
    ("terrain.chunks", 1, "lod", "8fed4ae29c80e636"),
    ("terrain.erode", 1, "eroded", "300f5e16a637b2d1"),
];

/// The same for the polygon and road kinds (ADR-0043 §5), from
/// `examples/roads.graph.json`.
const GOLDEN_ROADS: &[(&str, u32, &str, &str)] = &[
    ("path.spline", 1, "main", "cb0b0cded6d0a6c6"),
    ("road.mesh", 1, "roads", "751b0c4564adbb49"),
    ("road.footprint", 1, "print", "5bb699525b14169b"),
    ("terrain.conform", 1, "flat", "7a59b37f8acf592f"),
    ("shape.polygon", 1, "plaza", "4bcd533206c73f6e"),
    ("shape.boolean", 1, "yard", "b0b3e348b5136f98"),
    ("shape.offset", 1, "grown", "af63a37014a82f9b"),
    ("shape.mesh", 1, "yard_mesh", "aa134d4fe3693939"),
    ("field.from_shapes", 1, "clear", "29f8c960812b370b"),
    ("points.mask", 1, "off_road", "087b5717f949479c"),
];

#[test]
fn every_supported_kind_version_has_a_golden_output() {
    let registry = NodeRegistry::m1();
    for def in registry.iter() {
        for v in def.oldest..=def.version {
            assert!(
                GOLDEN
                    .iter()
                    .chain(GOLDEN_ROADS)
                    .any(|(k, gv, ..)| *k == def.kind && *gv == v),
                "{}@{v} has no golden in tests/versions.rs",
                def.kind
            );
        }
    }
    let roads = GraphDoc::from_json(include_str!("../examples/roads.graph.json")).unwrap();
    for (doc, version, entries) in [
        (example_with_every_kind(), 1u32, GOLDEN),
        (example_with_every_kind(), 2, GOLDEN),
        (example_with_every_kind(), 3, GOLDEN),
        (roads, 1, GOLDEN_ROADS),
    ] {
        let mut doc = doc;
        for n in &mut doc.nodes {
            n.version = version.min(registry.get(&n.kind).unwrap().version);
        }
        let g = ProcGraph::from_doc(&doc, &registry).unwrap();
        let e = evaluator(2).evaluate(&g, &example_opts());
        assert!(e.ok(), "{:#?}", e.report.diagnostics);
        for (kind, v, id, hash) in entries.iter().filter(|e| e.1 == version) {
            assert_eq!(&g.nodes[*id].kind, kind);
            assert_eq!(
                output_hash(e.output(id).unwrap()),
                *hash,
                "{kind}@{v} (node `{id}`) changed its output: give the kind a new version instead"
            );
        }
    }
}

/// The node keys of the example island computed on `main` before versioning
/// (commit e922999). Equal keys mean disk caches written then stay valid, and
/// that a graph from before the change evaluates unchanged.
#[test]
fn keys_from_before_versioning_are_unchanged() {
    const BEFORE: &[(&str, &str)] = &[
        (
            "height",
            "57696339cd071412df8a485a132f5d280fbc6f4aef3a7b8f58c4f401aa39a21e",
        ),
        (
            "mask",
            "58e96917a6f704a50e314a9d247425f46768d639180c16e98c430fcc7593b87b",
        ),
        (
            "island",
            "52580cd35ce7002989ebe6e355c86344f01c234751147a27c10f4dd957fa83a3",
        ),
        (
            "shore",
            "de0dc687980422a50ad8805f97ea8e7184f2e48dd244f2e4e2fd85a7d00e88fa",
        ),
        (
            "terrain",
            "a377ca758db2bac31ecd80894d8090af87c4d37878ca95dd8267272c7df5f59f",
        ),
        (
            "water",
            "356b45f4bd1ca6f9a1605d5684e4d8e9fecfa1441dbb9be9eaa4c418909ed2e2",
        ),
        (
            "scatter",
            "dd4c4e7bd683d7c84080ba2af28660b3e51fe468e3e70c24d108c560414b88ba",
        ),
        (
            "kind",
            "c6f425efe4b9c62a4bbf86c3ff0866028a4df368f4b04f4f6a72e9192cb4564a",
        ),
        (
            "props",
            "2b02be8d648903a1cb3953760ff03bacbb18d9950bfa18b61fefcfaa69008012",
        ),
        (
            "spots",
            "511c09124574be9c57885771c03006bba0068fa9893a39971de7c713fccc7db1",
        ),
        (
            "few",
            "87d8149fed778e5d755fba42c63cc27689c9f6002c620391665e65f8496fda64",
        ),
        (
            "coins",
            "ffa7b16bdf5de519f666fc3585bad02debdf6a2993d048efde3ca48b3b606565",
        ),
        (
            "plank",
            "3dad667dbfc42db92b08e507ecf52976505ffd84be649b5e7d8b09947c303d0b",
        ),
        (
            "post",
            "e672b930dde0e016451d06169a7bc9fee84b4ce7f49605cf5cf82e83d1ea0859",
        ),
        (
            "post_end",
            "c16b9aea3ea694cc9d16a130ab2ebeb80bf2fca1bf925554582bf447d72da4be",
        ),
        (
            "dock_mesh",
            "26849bac6ac6151d44f7a47509987fb10a87afab6fd4a5e3c27b94d842fd17f7",
        ),
        (
            "dock",
            "9e5b4201d51fba1a59fd0bddb85040a96dcc734e93e9415f1466b7043ec59914",
        ),
        (
            "out",
            "2922c21d47b789261066e6f664c7f5d5a3553ed2f8f0768892b04aa45293d2d4",
        ),
    ];
    // The example as it was written before versioning: every node at 1.
    let mut doc = example();
    for n in &mut doc.nodes {
        n.version = 1;
    }
    let g = ProcGraph::from_doc(&doc, &NodeRegistry::m1()).unwrap();
    let e = evaluator(1).evaluate(&g, &example_opts());
    let keys: Vec<(&str, &str)> = e
        .report
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.key.as_str()))
        .collect();
    assert_eq!(keys, BEFORE);
}

// A test kind with two versions:
// * version 1 took `factor` and returned `samples * factor`;
// * version 2 renamed it `k`, added `floor`, and clamps the result to it.
// Its migration renames the parameter; version 1 nodes keep version 1's
// output (no clamp).

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScaleParams {
    #[serde(default = "one")]
    k: f32,
    #[serde(default)]
    floor: f32,
}

fn one() -> f32 {
    1.0
}

impl Params for ScaleParams {
    fn validate(&self) -> Result<(), String> {
        if self.k.is_finite() {
            Ok(())
        } else {
            Err("`k` must be finite".into())
        }
    }
}

fn rename_factor(params: &mut Map<String, Json>) -> Result<(), String> {
    if let Some(v) = params.remove("factor") {
        if !v.is_number() {
            return Err("`factor` must be a number".into());
        }
        params.insert("k".into(), v);
    }
    Ok(())
}

const RENAME: &[Migration] = &[Migration {
    from: 1,
    note: "`factor` is now `k`; version 2 clamps to `floor`",
    changes_output: true,
    params: rename_factor,
}];

fn scale_def(oldest: u32) -> NodeDef {
    NodeDef::new(
        NodeSpec {
            kind: "test.scale",
            version: 2,
            oldest,
            migrations: RENAME,
            streaming: Streaming::Pointwise,
            doc: "Scales a field.",
            inputs: &[PortDef {
                name: "field",
                accepts: &[PortType::Field],
                required: true,
                doc: "The field.",
            }],
            variadic: None,
            output: PortType::Field,
            uses_seed: false,
            file_params: &[],
        },
        |ctx: &NodeCtx, p: &ScaleParams| -> Result<Output, String> {
            let f = ctx.need_field("field")?;
            let samples =
                f.0.samples
                    .iter()
                    .map(|s| {
                        let v = s * p.k;
                        if ctx.version >= 2 { v.max(p.floor) } else { v }
                    })
                    .collect();
            Ok(Output::Field(Arc::new(Field::new(
                f.n(),
                f.0.spacing,
                samples,
            )?)))
        },
    )
}

fn registry(oldest: u32) -> Arc<NodeRegistry> {
    let mut r = NodeRegistry::m1();
    r.register(scale_def(oldest));
    Arc::new(r)
}

fn scale_graph(version: u32, params: Json) -> GraphDoc {
    let doc = json!({
        "format": "real-engine/graph", "version": 1, "name": "s", "seed": 7,
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1,
              "params": { "size": 9, "height": 10.0 } },
            { "id": "s", "kind": "test.scale", "version": version, "params": params },
        ],
        "edges": [{ "from": ["h", "field"], "to": ["s", "field"] }],
    });
    GraphDoc::from_json(&doc.to_string()).unwrap()
}

fn samples(e: &re_proc::Evaluation) -> Vec<f32> {
    match e.output("s") {
        Some(Output::Field(f)) => f.0.samples.clone(),
        other => panic!("no field: {other:?}"),
    }
}

#[test]
fn a_node_evaluates_at_its_recorded_version_with_migrated_parameters() {
    let reg = registry(1);
    let ev = Evaluator::new(reg.clone());
    let o = EvalOptions::default();
    // Version 1 parameters, at version 1: no clamp.
    let v1 = ProcGraph::from_doc(&scale_graph(1, json!({ "factor": -1.0 })), &reg).unwrap();
    let e1 = ev.evaluate(&v1, &o);
    assert!(e1.ok(), "{:#?}", e1.report.diagnostics);
    let heights = match e1.output("h") {
        Some(Output::Field(f)) => f.0.samples.clone(),
        _ => unreachable!(),
    };
    assert!(heights.iter().any(|h| *h > 0.0));
    let negated: Vec<f32> = heights.iter().map(|h| -h).collect();
    assert_eq!(samples(&e1), negated, "version 1 output is kept");
    // The same values written for version 2: clamped at `floor` (0).
    let v2 = ProcGraph::from_doc(&scale_graph(2, json!({ "k": -1.0 })), &reg).unwrap();
    let e2 = ev.evaluate(&v2, &o);
    assert!(samples(&e2).iter().all(|v| *v >= 0.0));
    assert_ne!(samples(&e1), samples(&e2));
    // Different versions never share a key.
    assert_ne!(
        e1.report.node("s").unwrap().key,
        e2.report.node("s").unwrap().key
    );
    // The document still says version 1 with its own parameters.
    assert_eq!(v1.to_doc(), scale_graph(1, json!({ "factor": -1.0 })));
    // An outdated node is reported, but evaluates.
    let warn = v1.outdated(&reg);
    assert_eq!(warn.len(), 1);
    assert_eq!(warn[0].code.to_string(), "RE4019");
    assert!(v2.outdated(&reg).is_empty());
}

#[test]
fn version_errors_name_the_problem() {
    let codes = |reg: &NodeRegistry, v: u32, params: Json| -> Vec<String> {
        match ProcGraph::from_doc(&scale_graph(v, params), reg) {
            Ok(_) => Vec::new(),
            Err(ds) => ds.iter().map(|d| d.code.to_string()).collect(),
        }
    };
    let reg = registry(1);
    assert_eq!(
        codes(&reg, 3, json!({})),
        ["RE4012"],
        "newer than the engine"
    );
    assert_eq!(codes(&reg, 0, json!({})), ["RE4012"], "version 0");
    assert_eq!(
        codes(&reg, 1, json!({ "factor": "x" })),
        ["RE4020"],
        "migration refuses"
    );
    assert_eq!(
        codes(&reg, 1, json!({ "nope": 1 })),
        ["RE4005"],
        "checked after migrating"
    );
    assert_eq!(
        codes(&reg, 2, json!({ "factor": 2 })),
        ["RE4005"],
        "no migration from 2"
    );
    let retired = registry(2);
    let ds = ProcGraph::from_doc(&scale_graph(1, json!({})), &retired).unwrap_err();
    assert_eq!(ds[0].code.to_string(), "RE4018");
    assert!(
        ds[0].message.contains("versions 2 to 2"),
        "{}",
        ds[0].message
    );
}

#[test]
fn the_version_stamp_follows_versions_and_implementations_not_parameters() {
    let reg = registry(1);
    let a = version_stamp(&scale_graph(1, json!({ "factor": 2.0 })), &reg);
    let b = version_stamp(&scale_graph(1, json!({ "factor": 3.0 })), &reg);
    let c = version_stamp(&scale_graph(2, json!({ "k": 2.0 })), &reg);
    assert_eq!(a, b, "parameters are not part of it");
    assert_ne!(a, c, "versions are");
    let mut other = NodeRegistry::m1();
    other.register(scale_def(1).with_implementation([7; 32]));
    let d = version_stamp(&scale_graph(1, json!({ "factor": 2.0 })), &other);
    assert_ne!(a, d, "implementations are");
    // The report carries it.
    let g = ProcGraph::from_doc(&scale_graph(1, json!({})), &reg).unwrap();
    let e = Evaluator::new(reg.clone()).evaluate(&g, &EvalOptions::default());
    assert_eq!(e.report.stamp, a.to_hex().to_string());
}

#[test]
fn a_domain_is_part_of_every_key_only_when_set() {
    let g = ProcGraph::from_doc(&scale_graph(2, json!({})), &registry(1)).unwrap();
    let ev = Evaluator::new(registry(1));
    let keys = |domain: Option<Domain>| -> Vec<String> {
        let o = EvalOptions {
            domain,
            ..Default::default()
        };
        ev.evaluate(&g, &o)
            .report
            .nodes
            .iter()
            .map(|n| n.key.clone())
            .collect()
    };
    let none = keys(None);
    let a = keys(Some(Domain {
        origin: [0, 0],
        n: 9,
        spacing: 1.0,
    }));
    let b = keys(Some(Domain {
        origin: [8, 0],
        n: 9,
        spacing: 1.0,
    }));
    for i in 0..none.len() {
        assert_ne!(none[i], a[i]);
        assert_ne!(a[i], b[i]);
    }
}

#[test]
fn describe_lists_versions_and_streaming() {
    let reg = registry(1);
    let d = reg.get("test.scale").unwrap().describe();
    assert_eq!(d["version"], 2);
    assert_eq!(d["oldest"], 1);
    assert_eq!(d["streaming"], "pointwise");
    assert_eq!(
        NodeRegistry::m1()
            .get("terrain.island_mask")
            .unwrap()
            .describe()["streaming"],
        "whole"
    );
}

/// A project in a temporary directory with one graph file, and a world whose
/// source `s` reads it with `overrides`, generated once.
fn migration_world(version: u32, overrides: Json) -> (tempfile::TempDir, re_command::EditState) {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = scale_graph(version, json!({ "factor": 2.0, "floor": 5.0 }));
    doc.nodes.push(
        GraphDoc::from_json(
            &json!({
                "format": "real-engine/graph", "version": 1, "name": "x", "seed": 0,
                "nodes": [{ "id": "ground", "kind": "terrain.mesh", "version": 3, "params": {} }],
                "edges": [],
            })
            .to_string(),
        )
        .unwrap()
        .nodes
        .remove(0),
    );
    doc.edges.push(re_types::graph::EdgeDoc {
        from: ("s".into(), "field".into()),
        to: ("ground".into(), "field".into()),
    });
    std::fs::create_dir_all(dir.path().join("graphs")).unwrap();
    std::fs::write(dir.path().join("graphs/s.graph.json"), doc.to_json()).unwrap();

    let mut st = re_command::EditState::new();
    re_proc::register_generator(&mut st.world.generators);
    let mut store = re_proc::GraphStore::with_evaluator(
        Arc::new(Evaluator::new(registry(1))),
        Some(dir.path().to_path_buf()),
    );
    store.insert("s", doc);
    st.world.ecs.insert_resource(store);
    let scene = json!({
        "format": "real-engine/scene", "version": 1,
        "entities": [{
            "id": "s", "name": "S",
            "components": {
                "GraphSource": { "graph": "graphs/s.graph.json", "params": overrides.to_string() },
                "Procedural": { "generator": "graph", "seed": 7 },
                "Transform": {}
            }
        }]
    });
    let scene = re_scene::scene::from_json(&scene.to_string()).unwrap();
    st.world.load(&scene).unwrap();
    let regen = st.world.regenerate_changed();
    assert!(regen.diagnostics.is_empty(), "{:#?}", regen.diagnostics);
    (dir, st)
}

#[test]
fn an_old_graph_loads_unchanged_and_migrates_with_a_report() {
    let (dir, mut st) = migration_world(1, json!({ "s": { "factor": 3.0 } }));
    let store = st.world.ecs.resource::<re_proc::GraphStore>();
    let current = store.report("s").unwrap();
    // The old node evaluates as version 1 (no clamp), with the old-shaped
    // override applied, and the report warns that it is outdated.
    assert!(current.ok());
    let warned: Vec<_> = current
        .diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect();
    assert_eq!(warned, ["RE4019"], "{:#?}", current.diagnostics);
    let current_hash = current.fragment_hash.clone().unwrap();

    let report = re_proc::migrate::migration_report(&st.world, Some(dir.path()));
    assert!(report.ok, "{:#?}", report);
    assert_eq!(report.sources.len(), 1);
    let s = &report.sources[0];
    assert_eq!(s.file.as_deref(), Some("graphs/s.graph.json"));
    assert_eq!(s.nodes.len(), 1);
    let n = &s.nodes[0];
    assert_eq!((n.id.as_str(), n.from, n.to), ("s", 1, 2));
    assert!(n.changes_output);
    assert_eq!(n.params_after["k"], json!(2.0));
    assert!(!n.params_after.contains_key("factor"));
    assert_eq!(
        s.fragment_hash_before.as_deref(),
        Some(current_hash.as_str())
    );
    assert!(s.output_changed, "version 2 clamps to `floor`");
    assert!(s.keys_added.is_empty() && s.keys_removed.is_empty());
    assert_eq!(s.overrides_orphaned, ["s.factor"]);
    assert!(s.diagnostics.is_empty(), "{:#?}", s.diagnostics);

    // The read-only command gives the same report and changes nothing.
    let mut commands = re_command::CommandRegistry::new();
    re_proc::register_commands(&mut commands);
    let out = commands.dispatch(
        &mut st,
        "graph.migration_report",
        json!({}),
        false,
        &re_command::AllowAll,
    );
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert!(out.changes.is_empty());
    assert_eq!(
        out.data["sources"][0]["overrides_orphaned"],
        json!(["s.factor"])
    );
    assert_eq!(out.data["files"][0]["path"], "graphs/s.graph.json");

    // Writing it moves the file to version 2; nothing is left to migrate.
    let written = re_proc::migrate::write_files(&report, dir.path()).unwrap();
    assert_eq!(written, ["graphs/s.graph.json"]);
    let text = std::fs::read_to_string(dir.path().join("graphs/s.graph.json")).unwrap();
    let on_disk = GraphDoc::from_json(&text).unwrap();
    let node = on_disk.node("s").unwrap();
    assert_eq!(node.version, 2);
    assert_eq!(node.params["k"], json!(2.0));
    st.world
        .ecs
        .resource_mut::<re_proc::GraphStore>()
        .insert("s", on_disk);
    let again = re_proc::migrate::migration_report(&st.world, Some(dir.path()));
    assert!(again.ok && again.sources.is_empty() && again.files.is_empty());
}

#[test]
fn a_migration_reads_the_file_not_an_unsaved_open_graph() {
    let (dir, mut st) = migration_world(1, json!({}));
    let open = scale_graph(1, json!({ "factor": 9.0 }));
    let mut store = st.world.ecs.resource_mut::<re_proc::GraphStore>();
    let mut edited = store.graph("s").unwrap().clone();
    edited.nodes[1].params = open.nodes[1].params.clone();
    store.insert("s", edited);
    let report = re_proc::migrate::migration_report(&st.world, Some(dir.path()));
    let s = &report.sources[0];
    assert_eq!(s.nodes[0].params_after["k"], json!(2.0), "the file's value");
    let codes: Vec<_> = s.diagnostics.iter().map(|d| d.code.to_string()).collect();
    assert_eq!(codes, ["RE4022"]);
}

#[test]
fn a_migration_refuses_an_open_graph_with_unsaved_edits() {
    let (dir, mut st) = migration_world(1, json!({}));
    let path = "graphs/s.graph.json";
    let text = std::fs::read_to_string(dir.path().join(path)).unwrap();
    st.world
        .open_doc(path, serde_json::from_str(&text).unwrap())
        .unwrap();
    // A clean open document matches its file: nothing to say.
    let report = re_proc::migrate::migration_report(&st.world, Some(dir.path()));
    assert!(report.ok);
    assert!(report.sources[0].diagnostics.is_empty());

    let mut h = re_command::History::default();
    let edit = re_command::Transaction::new(
        "edit",
        re_command::Origin::User,
        vec![re_command::Op::SetDoc {
            doc: path.into(),
            pointer: "/nodes/1/params/factor".into(),
            value: Some(json!(9.0)),
        }],
    );
    h.execute(&mut st.world, edit).unwrap();
    let report = re_proc::migrate::migration_report(&st.world, Some(dir.path()));
    assert!(
        !report.ok,
        "a rewrite would be overwritten by the next save"
    );
    let d = &report.sources[0].diagnostics[0];
    assert_eq!(d.code.to_string(), "RE4022");
    assert_eq!(d.severity, re_core::Severity::Error);
    assert!(
        re_proc::migrate::write_files(&report, dir.path())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(path)).unwrap(),
        text
    );
}

#[test]
fn a_failed_migration_writes_nothing() {
    let (dir, st) = migration_world(1, json!({}));
    let bad = scale_graph(1, json!({ "factor": "two" }));
    let path = dir.path().join("graphs/s.graph.json");
    let mut doc = GraphDoc::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc.nodes[1].params = bad.nodes[1].params.clone();
    std::fs::write(&path, doc.to_json()).unwrap();
    let report = re_proc::migrate::migration_report(&st.world, Some(dir.path()));
    assert!(!report.ok);
    let codes: Vec<_> = report
        .all_diagnostics()
        .map(|d| d.code.to_string())
        .collect();
    assert_eq!(codes, ["RE4020"]);
    let before = std::fs::read_to_string(&path).unwrap();
    assert!(
        re_proc::migrate::write_files(&report, dir.path())
            .unwrap()
            .is_empty()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn overrides_on_an_old_node_use_its_version_shape() {
    let (_dir, mut st) = migration_world(1, json!({}));
    let mut commands = re_command::CommandRegistry::new();
    re_proc::register_commands(&mut commands);
    let mut set = |param: &str, value: Json| {
        let out = commands.dispatch(
            &mut st,
            "graph.set_param",
            json!({ "source": "s", "node": "s", "param": param, "value": value }),
            false,
            &re_command::AllowAll,
        );
        out.diagnostics
            .iter()
            .map(|d| d.code.to_string())
            .collect::<Vec<_>>()
    };
    // The version-1 name is accepted; the version-2 name would be
    // overwritten by the migration, so it is refused; so is a typo.
    assert!(set("factor", json!(3.0)).is_empty());
    assert_eq!(set("k", json!(2.0)), ["RE4011"]);
    assert_eq!(set("factr", json!(2.0)), ["RE4011"]);
    assert_eq!(set("factor", json!("x")), ["RE4005"]);
}

#[test]
fn erosion_leaves_a_field_too_small_to_carry_water() {
    let doc = json!({
        "format": "real-engine/graph", "version": 1, "name": "e", "seed": 7,
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1,
              "params": { "size": 2, "height": 10.0 } },
            { "id": "e", "kind": "terrain.erode", "version": 1, "params": {} },
        ],
        "edges": [{ "from": ["h", "field"], "to": ["e", "field"] }],
    });
    let reg = Arc::new(NodeRegistry::m1());
    let g = ProcGraph::from_doc(&GraphDoc::from_json(&doc.to_string()).unwrap(), &reg).unwrap();
    let e = Evaluator::new(reg).evaluate(&g, &EvalOptions::default());
    assert!(e.ok(), "{:#?}", e.report.diagnostics);
    let (Some(Output::Field(a)), Some(Output::Field(b))) = (e.output("h"), e.output("e")) else {
        panic!("no fields");
    };
    assert_eq!(a.0.samples, b.0.samples);
}

/// Streamed, `terrain.mesh@2` puts its entity at the window's corner with a
/// corner-local mesh and collider (ADR-0042's floating origin); `@1` keeps
/// them in world space.
#[test]
fn terrain_mesh_2_is_local_to_its_window_corner() {
    let registry = Arc::new(NodeRegistry::m1());
    let at = |version: u32| {
        let doc = json!({
            "format": "real-engine/graph", "version": 1, "name": "t", "seed": 7,
            "nodes": [
                { "id": "h", "kind": "terrain.heightfield", "version": 1,
                  "params": { "cell": 4.0, "height": 10.0 } },
                { "id": "ground", "kind": "terrain.mesh", "version": version,
                  "params": { "collider": true } },
            ],
            "edges": [{ "from": ["h", "field"], "to": ["ground", "field"] }],
        });
        let g = ProcGraph::from_doc(&GraphDoc::from_json(&doc.to_string()).unwrap(), &registry)
            .unwrap();
        let o = EvalOptions {
            domain: Some(Domain {
                origin: [1792, -64],
                n: 17,
                spacing: 4.0,
            }),
            ..Default::default()
        };
        let e = Evaluator::new(registry.clone()).evaluate(&g, &o);
        assert!(e.ok(), "{:#?}", e.report.diagnostics);
        let Some(Output::Fragment(f)) = e.output("ground") else {
            panic!("no fragment");
        };
        let translation = f.items[0]
            .components
            .iter()
            .find(|(c, _)| c == "Transform")
            .and_then(|(_, fields)| fields.iter().find(|(n, _)| n == "translation"))
            .map(|(_, v)| v.clone());
        let offset = f
            .assets
            .iter()
            .find_map(|(_, a)| match a {
                re_proc::DerivedAsset::Heightfield(h) => Some(h.offset),
                _ => None,
            })
            .expect("a collider heightfield");
        (translation, offset)
    };
    let (t1, o1) = at(1);
    assert_eq!(t1, None);
    assert_eq!([o1.x, o1.z], [7168.0, -256.0]);
    let (t2, o2) = at(2);
    assert_eq!(
        t2,
        Some(re_reflect::Value::Vec3(glam::Vec3::new(
            7168.0, 0.0, -256.0
        )))
    );
    assert_eq!([o2.x, o2.z], [0.0, 0.0]);
    assert_eq!(o1.y, o2.y);
}

/// Streamed, `terrain.mesh@3` takes each vertex's normal over the triangles
/// beyond its window too, so two cells shade their shared edge as one mesh
/// over both would; `@2` leaves those triangles out, and the edge shows.
#[test]
fn terrain_mesh_3_shades_cell_edges_as_one_mesh_would() {
    let registry = Arc::new(NodeRegistry::m1());
    let evaluator = Evaluator::new(registry.clone());
    let at = |version: u32, origin: [i64; 2], n: u32| {
        let doc = json!({
            "format": "real-engine/graph", "version": 1, "name": "t", "seed": 7,
            "nodes": [
                { "id": "h", "kind": "terrain.heightfield", "version": 1,
                  "params": { "cell": 4.0, "height": 60.0, "scale": 0.01 } },
                { "id": "ground", "kind": "terrain.mesh", "version": version,
                  "params": { "collider": true } },
            ],
            "edges": [{ "from": ["h", "field"], "to": ["ground", "field"] }],
        });
        let g = ProcGraph::from_doc(&GraphDoc::from_json(&doc.to_string()).unwrap(), &registry)
            .unwrap();
        let o = EvalOptions {
            domain: Some(Domain {
                origin,
                n,
                spacing: 4.0,
            }),
            ..Default::default()
        };
        let e = evaluator.evaluate(&g, &o);
        assert!(e.ok(), "{:#?}", e.report.diagnostics);
        let Some(Output::Fragment(f)) = e.output("ground") else {
            panic!("no fragment");
        };
        let mesh = f
            .assets
            .iter()
            .find_map(|(_, a)| match a {
                re_proc::DerivedAsset::Mesh(m) => Some(m.clone()),
                _ => None,
            })
            .expect("a mesh");
        let collider = f
            .assets
            .iter()
            .find_map(|(_, a)| match a {
                re_proc::DerivedAsset::Heightfield(h) => Some(h.clone()),
                _ => None,
            })
            .expect("a collider heightfield");
        (mesh, collider)
    };
    let normal = |m: &re_types::mesh::MeshData, side: usize, x: usize, z: usize| {
        m.vertices[z * side + x].normal
    };
    // Two cells of 16 samples' steps, side by side, and one window over both.
    let (a, b, n) = ([1792, -64], [1808, -64], 17);
    let (a2, a2_collider) = at(2, a, 17);
    let (b2, _) = at(2, b, 17);
    assert!(
        (0..n).any(|z| normal(&a2, n, n - 1, z) != normal(&b2, n, 0, z)),
        "version 2 shades the shared edge twice"
    );
    let (a3, a3_collider) = at(3, a, 17);
    let (b3, _) = at(3, b, 17);
    let (both, _) = at(3, a, 33);
    for z in 0..n {
        for x in 0..n {
            assert_eq!(
                normal(&a3, n, x, z),
                normal(&both, 33, x, z),
                "a ({x}, {z})"
            );
            assert_eq!(
                normal(&b3, n, x, z),
                normal(&both, 33, x + 16, z),
                "b ({x}, {z})"
            );
            if x > 0 && z > 0 && x < n - 1 && z < n - 1 {
                assert_eq!(
                    normal(&a3, n, x, z),
                    normal(&a2, n, x, z),
                    "inside ({x}, {z})"
                );
            }
        }
    }
    // Everything but the edge normals is version 2's.
    assert_eq!(a3.indices, a2.indices);
    for (v3, v2) in a3.vertices.iter().zip(&a2.vertices) {
        assert_eq!((v3.pos, v3.uv, v3.color), (v2.pos, v2.uv, v2.color));
    }
    assert_eq!(a3_collider, a2_collider);
}
