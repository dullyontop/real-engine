//! Graph format version 2 (ADR-0061): a version 1 graph rewritten as version
//! 2 means what it meant (the same keys and output, for every graph file in
//! the repository); `real migrate-graph` upgrades graph files, alone with
//! `--format-only`, and keeps bound node params bound; declared outputs are
//! a graph's output, streamed too; and the graph param commands.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use re_command::{AllowAll, CommandOutcome, CommandRegistry, EditState};
use re_proc::canon::doc_hash;
use re_proc::migrate::{
    MigrateOptions, add_unopened_files, migrate_doc, migration_report, migration_report_with,
    upgrade_format, write_files,
};
use re_proc::stream::{StreamConfig, step};
use re_proc::{
    EvalOptions, Evaluator, Field, GraphStore, Migration, NodeCtx, NodeDef, NodeRegistry, NodeSpec,
    Output, Params, PortDef, PortType, ProcGraph, Streaming, fragment_hash_hex, register_commands,
    register_generator, version_stamp,
};
use re_reflect::Value;
use re_scene::World;
use re_types::graph::GraphDoc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};
use support::evaluator;

/// The graph file of [`file_world`]'s project.
const FILE: &str = "graphs/g.graph.json";

fn codes(ds: &[re_core::Diagnostic]) -> Vec<String> {
    ds.iter().map(|d| d.code.to_string()).collect()
}

fn doc(value: Json) -> GraphDoc {
    GraphDoc::from_json(&value.to_string()).unwrap()
}

/// Every graph file in the repository, with the folder its paths are
/// relative to: its project's (the nearest folder holding `real.toml`), else
/// its own.
fn repo_graph_files() -> Vec<(PathBuf, PathBuf)> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = Vec::new();
    let mut dirs = vec![repo];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                if !name.starts_with('.') && name != "target" {
                    dirs.push(path);
                }
            } else if name.ends_with(".graph.json") {
                let root = path
                    .ancestors()
                    .skip(1)
                    .find(|d| d.join("real.toml").is_file())
                    .unwrap_or_else(|| path.parent().unwrap())
                    .to_path_buf();
                out.push((path, root));
            }
        }
    }
    out.sort();
    out
}

/// The fragment hash and each node's key of `doc`, evaluated by a fresh
/// evaluator with paths relative to `root`.
fn outcome(doc: &GraphDoc, root: &Path) -> (String, Vec<(String, String)>) {
    let g = ProcGraph::from_doc(doc, &NodeRegistry::m1())
        .unwrap_or_else(|d| panic!("{}: {d:#?}", doc.name));
    let e = evaluator(2).evaluate(
        &g,
        &EvalOptions {
            root: Some(root.to_path_buf()),
            ..Default::default()
        },
    );
    assert!(e.ok(), "{}: {:#?}", doc.name, e.report.diagnostics);
    let (fragment, _) = e
        .scene_fragment(&World::new().registry)
        .expect("a fragment");
    let keys = e
        .report
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.key.clone()))
        .collect();
    (fragment_hash_hex(&fragment), keys)
}

#[test]
fn every_graph_file_in_the_repository_means_the_same_at_version_2() {
    let files = repo_graph_files();
    for known in [
        "crates/re_proc/examples/island.graph.json",
        "crates/re_proc/examples/roads.graph.json",
        "examples/valley/graphs/valley.graph.json",
    ] {
        assert!(
            files.iter().any(|(p, _)| p.ends_with(known)),
            "{known}: {files:?}"
        );
    }
    let registry = NodeRegistry::m1();
    for (path, root) in files {
        let name = path.display().to_string();
        let doc = GraphDoc::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc.version,
            GraphDoc::VERSION,
            "{name}: write it at the current format with `real migrate-graph --format-only`"
        );
        if doc.version_2_use().is_some() {
            continue;
        }
        // The file as it was before its rewrite, and rewritten.
        let mut v1 = doc.clone();
        v1.version = 1;
        let v2 = upgrade_format(&v1, &name).unwrap();
        assert_eq!(v2, doc, "{name}: only the version changes");
        assert_eq!(doc_hash(&v1), doc_hash(&v2), "{name}");
        assert_eq!(
            version_stamp(&v1, &registry),
            version_stamp(&v2, &registry),
            "{name}"
        );
        assert_eq!(outcome(&v1, &root), outcome(&v2, &root), "{name}");
    }
}

/// A small terrain: a heightfield, its mesh (`terrain.mesh` at
/// `mesh_version`) and the sea.
fn small(mesh_version: u32) -> GraphDoc {
    doc(json!({
        "format": "real-engine/graph", "version": 1, "name": "small", "seed": 3,
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 17, "height": 6.0 } },
            { "id": "ground", "kind": "terrain.mesh", "version": mesh_version, "params": { "collider": true } },
            { "id": "sea", "kind": "terrain.water_plane", "version": 1, "params": { "level": 1.0, "size": 50.0 } }
        ],
        "edges": [{ "from": ["h", "field"], "to": ["ground", "field"] }]
    }))
}

/// A project in a temporary folder whose graph file [`FILE`] holds `doc`,
/// and a world whose source `s` reads it, generated once.
fn file_world(doc: &GraphDoc, registry: NodeRegistry) -> (tempfile::TempDir, EditState) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("graphs")).unwrap();
    std::fs::write(dir.path().join(FILE), doc.to_json()).unwrap();
    let mut st = EditState::new();
    register_generator(&mut st.world.generators);
    let mut store = GraphStore::with_evaluator(
        Arc::new(Evaluator::new(Arc::new(registry))),
        Some(dir.path().to_path_buf()),
    );
    store.insert("s", doc.clone());
    st.world.ecs.insert_resource(store);
    let scene = json!({
        "format": "real-engine/scene", "version": 1,
        "entities": [{
            "id": "s", "name": "S",
            "components": {
                "GraphSource": { "graph": FILE, "params": "" },
                "Procedural": { "generator": "graph", "seed": 7 },
                "Transform": {}
            }
        }]
    });
    st.world
        .load(&re_scene::scene::from_json(&scene.to_string()).unwrap())
        .unwrap();
    let regen = st.world.regenerate_changed();
    assert!(regen.diagnostics.is_empty(), "{:#?}", regen.diagnostics);
    (dir, st)
}

fn on_disk(dir: &Path) -> GraphDoc {
    GraphDoc::from_json(&std::fs::read_to_string(dir.join(FILE)).unwrap()).unwrap()
}

fn format_only() -> MigrateOptions {
    MigrateOptions { format_only: true }
}

#[test]
fn migrate_graph_writes_version_1_files_as_version_2() {
    let (dir, mut st) = file_world(&small(3), NodeRegistry::m1());
    let report = migration_report(&st.world, Some(dir.path()));
    assert!(report.ok, "{report:#?}");
    let s = &report.sources[0];
    assert!(s.nodes.is_empty());
    // A format upgrade changes no output, so nothing is evaluated.
    assert!(!s.output_changed);
    assert!(s.fragment_hash_before.is_none() && s.fragment_hash_after.is_none());
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["files"][0]["path"], FILE);
    assert_eq!(json["files"][0]["format"], json!({ "from": 1, "to": 2 }));
    assert_eq!(json["files"][0]["sources"], json!(["s"]));

    assert_eq!(write_files(&report, dir.path()).unwrap(), [FILE]);
    let written = on_disk(dir.path());
    assert_eq!(written.version, 2);
    let mut back = written.clone();
    back.version = 1;
    assert_eq!(back, small(3), "only the version changes");
    assert_eq!(doc_hash(&written), doc_hash(&small(3)));

    st.world
        .ecs
        .resource_mut::<GraphStore>()
        .insert("s", written);
    let again = migration_report(&st.world, Some(dir.path()));
    assert!(
        again.ok && again.sources.is_empty() && again.files.is_empty(),
        "{again:#?}"
    );
}

#[test]
fn format_only_upgrades_the_format_and_moves_no_node() {
    let (dir, mut st) = file_world(&small(1), NodeRegistry::m1());
    let full = migration_report(&st.world, Some(dir.path()));
    assert!(full.ok, "{full:#?}");
    let moved: Vec<_> = full.sources[0]
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.from, n.to))
        .collect();
    assert_eq!(moved, [("ground", 1, 3)]);
    assert_eq!((full.files[0].format.from, full.files[0].format.to), (1, 2));

    let only = migration_report_with(&st.world, Some(dir.path()), format_only());
    assert!(only.ok && only.format_only, "{only:#?}");
    assert!(only.sources[0].nodes.is_empty());
    assert!(only.sources[0].fragment_hash_before.is_none());
    assert_eq!((only.files[0].format.from, only.files[0].format.to), (1, 2));
    assert_eq!(write_files(&only, dir.path()).unwrap(), [FILE]);
    let written = on_disk(dir.path());
    assert_eq!(written.version, 2);
    assert_eq!(written.node("ground").unwrap().version, 1);

    st.world
        .ecs
        .resource_mut::<GraphStore>()
        .insert("s", written);
    let again = migration_report_with(&st.world, Some(dir.path()), format_only());
    assert!(again.ok && again.sources.is_empty() && again.files.is_empty());
    // Its node still migrates; the format is current already.
    let full = migration_report(&st.world, Some(dir.path()));
    assert_eq!(full.sources[0].nodes.len(), 1);
    assert_eq!((full.files[0].format.from, full.files[0].format.to), (2, 2));
}

#[test]
fn a_file_version_2_cannot_hold_is_refused_and_left_alone() {
    // A node id version 2 does not allow.
    let mut dotted = small(3);
    dotted.nodes[2].id = "sea.1".into();
    let e = upgrade_format(&dotted, FILE).unwrap_err();
    assert_eq!(e.code.to_string(), "RE4000");
    assert!(
        e.message
            .contains("graphs/g.graph.json cannot be written as graph file version 2"),
        "{}",
        e.message
    );
    // `"$param"` inside a value, which version 1 reads as it is.
    let mut literal = small(3);
    literal.nodes[2]
        .params
        .insert("name".into(), json!({ "$param": "x", "and": 1 }));
    let e = upgrade_format(&literal, FILE).unwrap_err();
    assert!(e.message.contains("\"$param\""), "{}", e.message);

    let (dir, st) = file_world(&dotted, NodeRegistry::m1());
    let report = migration_report(&st.world, Some(dir.path()));
    assert!(!report.ok);
    assert_eq!(codes(&report.sources[0].diagnostics), ["RE4000"]);
    let before = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
    assert!(write_files(&report, dir.path()).unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.path().join(FILE)).unwrap(),
        before
    );
}

#[test]
fn files_no_source_reads_are_upgraded_too() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("graphs")).unwrap();
    std::fs::write(dir.path().join("graphs/a.graph.json"), small(3).to_json()).unwrap();
    std::fs::write(dir.path().join("graphs/b.graph.json"), small(1).to_json()).unwrap();
    let paths = [
        "graphs/a.graph.json".to_string(),
        "graphs/b.graph.json".to_string(),
    ];
    let world = World::new();
    let registry = NodeRegistry::m1();
    let mut report = migration_report(&world, Some(dir.path()));
    add_unopened_files(&mut report, dir.path(), &registry, &paths);
    assert!(report.ok, "{report:#?}");
    let files: Vec<_> = report
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.format.from, f.format.to))
        .collect();
    assert_eq!(
        files,
        [("graphs/a.graph.json", 1, 2), ("graphs/b.graph.json", 1, 2)]
    );
    // Only b's node moves.
    let unopened: Vec<_> = report.unopened.iter().map(|u| u.path.as_str()).collect();
    assert_eq!(unopened, ["graphs/b.graph.json"]);

    let mut only = migration_report_with(&world, Some(dir.path()), format_only());
    add_unopened_files(&mut only, dir.path(), &registry, &paths);
    assert_eq!(only.files.len(), 2);
    assert!(only.unopened.is_empty());
    assert_eq!(only.files[1].doc.node("ground").unwrap().version, 1);
}

// A test kind with two versions: version 1's `factor` is version 2's `k`;
// `floor` means the same in both.

#[derive(Deserialize, Serialize, JsonSchema)]
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
        Ok(())
    }
}

fn rename_factor(params: &mut Map<String, Json>) -> Result<(), String> {
    if let Some(v) = params.remove("factor") {
        params.insert("k".into(), v);
    }
    Ok(())
}

fn scale_registry() -> NodeRegistry {
    let mut r = NodeRegistry::m1();
    r.register(NodeDef::new(
        NodeSpec {
            kind: "test.scale",
            version: 2,
            oldest: 1,
            migrations: &[Migration {
                from: 1,
                note: "`factor` is now `k`",
                changes_output: false,
                params: rename_factor,
            }],
            streaming: Streaming::Pointwise,
            doc: "Scales a field, no lower than `floor`.",
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
            let samples = f.0.samples.iter().map(|s| (s * p.k).max(p.floor)).collect();
            Ok(Output::Field(Arc::new(Field::new(
                f.n(),
                f.0.spacing,
                samples,
            )?)))
        },
    ));
    r
}

/// A version 2 graph with the params `gain` and `floor` and a version 1
/// `test.scale` node `s` with `s_params`; with `kind`, a node kind whose
/// `floor` has no default.
fn bound_scale(kind: bool, s_params: Json) -> GraphDoc {
    let mut value = json!({
        "format": "real-engine/graph", "version": 2, "name": "scaled", "seed": 7,
        "params": [
            { "name": "gain", "type": "number", "default": 2.0 },
            { "name": "floor", "type": "number", "default": 0.5 }
        ],
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 9, "height": 10.0 } },
            { "id": "s", "kind": "test.scale", "version": 1, "params": s_params }
        ],
        "edges": [{ "from": ["h", "field"], "to": ["s", "field"] }]
    });
    if kind {
        value["kind"] = json!({ "name": "project.scaled", "version": 1 });
        value["params"][1]
            .as_object_mut()
            .unwrap()
            .remove("default");
    }
    doc(value)
}

#[test]
fn migrating_keeps_bound_node_params_bound() {
    let registry = scale_registry();
    // `floor` keeps its value through the migration, so it stays bound.
    let kept = bound_scale(
        false,
        json!({ "factor": 3.0, "floor": { "$param": "floor" } }),
    );
    let (migrated, moved) = migrate_doc(&kept, &registry).unwrap();
    assert_eq!(
        Json::Object(moved[0].params_after.clone()),
        json!({ "k": 3.0, "floor": { "$param": "floor" } })
    );
    let s = migrated.node("s").unwrap();
    assert_eq!(s.version, 2);
    assert_eq!(s.params["floor"], json!({ "$param": "floor" }));

    // `factor` becomes `k`: a binding the migration cannot carry is refused.
    let renamed = bound_scale(false, json!({ "factor": { "$param": "gain" } }));
    let e = migrate_doc(&renamed, &registry).unwrap_err();
    assert_eq!(e.code.to_string(), "RE4020");
    assert!(
        e.message
            .contains("drops or changes `s.factor`, which is bound to graph param `gain`"),
        "{}",
        e.message
    );
    // A kind's param without a default has no value to migrate with.
    let unvalued = bound_scale(
        true,
        json!({ "factor": 3.0, "floor": { "$param": "floor" } }),
    );
    let e = migrate_doc(&unvalued, &registry).unwrap_err();
    assert_eq!(e.code.to_string(), "RE4020");
    assert!(e.message.contains("which has no default"), "{}", e.message);

    // Through `real migrate-graph`: the source's output is compared with
    // the params bound, and the file keeps the reference.
    let (dir, st) = file_world(&kept, scale_registry());
    let report = migration_report(&st.world, Some(dir.path()));
    assert!(report.ok, "{report:#?}");
    let source = &report.sources[0];
    assert!(source.diagnostics.is_empty(), "{:#?}", source.diagnostics);
    assert!(source.fragment_hash_before.is_some());
    assert_eq!(write_files(&report, dir.path()).unwrap(), [FILE]);
    assert_eq!(
        on_disk(dir.path()).node("s").unwrap().params["floor"],
        json!({ "$param": "floor" })
    );
}

/// A version 2 graph: a heightfield and its mesh, the sea, a pond and a box,
/// with the declared outputs `outputs` (`[name, type, node, port]`).
fn with_outputs(outputs: &[(&str, &str, &str, &str)]) -> GraphDoc {
    let mut edges = vec![json!({ "from": ["h", "field"], "to": ["ground", "field"] })];
    edges.extend(
        outputs
            .iter()
            .map(|(name, _, node, port)| json!({ "from": [node, port], "to": ["$out", name] })),
    );
    doc(json!({
        "format": "real-engine/graph", "version": 2, "name": "outs", "seed": 3,
        "outputs": outputs.iter().map(|(name, ty, ..)| json!({ "name": name, "type": ty })).collect::<Vec<_>>(),
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 17, "height": 6.0 } },
            { "id": "ground", "kind": "terrain.mesh", "version": 3, "params": {} },
            { "id": "sea", "kind": "terrain.water_plane", "version": 1, "params": { "level": 1.0 } },
            { "id": "pond", "kind": "terrain.water_plane", "version": 1, "params": { "level": 2.0, "size": 4.0 } },
            { "id": "box", "kind": "mesh.primitive", "version": 1, "params": { "shape": "box" } }
        ],
        "edges": edges
    }))
}

/// The nodes `doc`'s entities come from, in fragment order, once each.
fn producers(doc: &GraphDoc) -> Vec<String> {
    let g = ProcGraph::from_doc(doc, &NodeRegistry::m1()).unwrap();
    let e = evaluator(1).evaluate(&g, &EvalOptions::default());
    assert!(e.ok(), "{:#?}", e.report.diagnostics);
    let mut out: Vec<String> = Vec::new();
    for item in e.fragment().unwrap().items {
        let node = item.key.split('/').next().unwrap().to_string();
        if out.last() != Some(&node) {
            out.push(node);
        }
    }
    out
}

#[test]
fn declared_outputs_are_the_graphs_output_in_their_order() {
    // Without declarations, every sink is output, in node order.
    assert_eq!(
        producers(&with_outputs(&[])),
        ["ground", "sea", "pond", "box"]
    );
    let sea_and_box = [
        ("water", "fragment", "sea", "fragment"),
        ("shape", "mesh", "box", "mesh"),
    ];
    assert_eq!(producers(&with_outputs(&sea_and_box)), ["sea", "box"]);
    let swapped = with_outputs(&[sea_and_box[1], sea_and_box[0]]);
    assert_eq!(producers(&swapped), ["box", "sea"]);
    // A node that feeds two outputs is output once.
    let twice = with_outputs(&[
        ("a", "fragment", "sea", "fragment"),
        ("b", "fragment", "sea", "out"),
    ]);
    assert_eq!(producers(&twice), ["sea"]);
}

#[test]
fn port_declarations_and_their_wiring_are_checked() {
    let registry = NodeRegistry::m1();
    let problems = |outputs: Json, edges: Json, more: Json| -> Vec<String> {
        let mut value = json!({
            "format": "real-engine/graph", "version": 2, "name": "p", "seed": 1,
            "outputs": outputs,
            "nodes": [
                { "id": "sea", "kind": "terrain.water_plane", "version": 1, "params": {} },
                { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 9 } }
            ],
            "edges": edges
        });
        for (k, v) in more.as_object().unwrap() {
            value[k] = v.clone();
        }
        match ProcGraph::from_doc(&doc(value), &registry) {
            Ok(_) => Vec::new(),
            Err(ds) => codes(&ds),
        }
    };
    let o = json!([{ "name": "o", "type": "fragment" }]);
    let fed = json!([{ "from": ["sea", "fragment"], "to": ["$out", "o"] }]);
    assert!(problems(o.clone(), fed.clone(), json!({})).is_empty());
    // Bad declarations (RE4205).
    for outputs in [
        json!([{ "name": "o", "type": "fragmnt" }]),
        json!([{ "name": "o", "type": "fragment" }, { "name": "o", "type": "fragment" }]),
        json!([{ "name": "o-1", "type": "fragment" }]),
        json!([{ "name": "o", "type": "fragment", "required": true }]),
    ] {
        assert!(
            problems(outputs.clone(), fed.clone(), json!({})).contains(&"RE4205".to_string()),
            "{outputs}"
        );
    }
    // Inputs belong to node kinds.
    let inputs = json!({ "inputs": [{ "name": "f", "type": "field" }] });
    assert_eq!(problems(o.clone(), fed.clone(), inputs), ["RE4205"]);
    // Wiring (RE4206): no edge, two, the wrong type, an undeclared output.
    assert_eq!(problems(o.clone(), json!([]), json!({})), ["RE4206"]);
    let two = json!([
        { "from": ["sea", "fragment"], "to": ["$out", "o"] },
        { "from": ["sea", "out"], "to": ["$out", "o"] }
    ]);
    assert_eq!(problems(o.clone(), two, json!({})), ["RE4206"]);
    let field = json!([{ "from": ["h", "field"], "to": ["$out", "o"] }]);
    assert_eq!(problems(o.clone(), field, json!({})), ["RE4206"]);
    let undeclared = json!([
        { "from": ["sea", "fragment"], "to": ["$out", "o"] },
        { "from": ["sea", "fragment"], "to": ["$out", "p"] }
    ]);
    assert_eq!(problems(o, undeclared, json!({})), ["RE4206"]);
}

#[test]
fn a_kinds_declared_inputs_connect_node_inputs() {
    let registry = NodeRegistry::m1();
    let problems = |edges: Json| -> Vec<String> {
        let value = json!({
            "format": "real-engine/graph", "version": 2, "name": "k", "seed": 1,
            "kind": { "name": "project.ground", "version": 1 },
            "inputs": [{ "name": "f", "type": "field" }],
            "outputs": [{ "name": "o", "type": "fragment" }],
            "nodes": [
                { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 9 } },
                { "id": "ground", "kind": "terrain.mesh", "version": 3, "params": {} }
            ],
            "edges": edges
        });
        match ProcGraph::from_doc(&doc(value), &registry) {
            Ok(_) => Vec::new(),
            Err(ds) => codes(&ds),
        }
    };
    let out = json!({ "from": ["ground", "fragment"], "to": ["$out", "o"] });
    let from_input = json!({ "from": ["$in", "f"], "to": ["ground", "field"] });
    let from_node = json!({ "from": ["h", "field"], "to": ["ground", "field"] });
    // The declared input feeds the mesh's required input.
    assert!(problems(json!([from_input, out])).is_empty());
    // A node input takes one edge, from a node or from a declared input.
    assert_eq!(problems(json!([from_input, from_node, out])), ["RE4007"]);
    assert_eq!(problems(json!([out])), ["RE4006"]);
}

fn commands() -> CommandRegistry<EditState> {
    let mut r = CommandRegistry::new();
    register_commands(&mut r);
    r
}

fn call(st: &mut EditState, name: &str, args: Json) -> CommandOutcome {
    commands().dispatch(st, name, args, false, &AllowAll)
}

fn ok(st: &mut EditState, name: &str, args: Json) -> Json {
    let o = call(st, name, args);
    assert!(o.ok, "{name}: {:#?}", o.diagnostics);
    o.data
}

/// The first diagnostic's code of a refused call.
fn refused(o: &CommandOutcome) -> String {
    assert!(!o.ok, "expected a refusal: {:#?}", o.data);
    o.diagnostics[0].code.to_string()
}

fn open(st: &EditState) -> GraphDoc {
    serde_json::from_value(st.world.doc(FILE).expect("open").value.clone()).unwrap()
}

#[test]
fn graph_params_are_declared_bound_and_removed_in_the_file() {
    let (_dir, mut st) = file_world(&small(3), NodeRegistry::m1());
    let data = ok(&mut st, "graph.params", json!({ "graph": FILE }));
    assert_eq!(data["version"], 1);
    assert_eq!(data["params"], json!([]));

    // The first param makes the file version 2, in the same undo step.
    let height = json!({ "graph": FILE, "name": "height", "type": "number", "default": 6.0, "min": 1.0, "max": 30.0, "doc": "Hill height." });
    let data = ok(&mut st, "graph.declare_param", height.clone());
    assert_eq!(
        (data["version"].clone(), data["replaced"].clone()),
        (json!(2), json!(false))
    );
    let doc = open(&st);
    assert_eq!(doc.version, 2);
    assert_eq!(doc.params[0].name, "height");
    assert!(st.history.undo(&mut st.world).unwrap());
    assert_eq!(open(&st), small(3));
    assert!(st.history.redo(&mut st.world).unwrap());
    // Declaring it again as it is changes nothing.
    let label = st.history.undo_label().map(str::to_string);
    ok(&mut st, "graph.declare_param", height);
    assert_eq!(st.history.undo_label().map(str::to_string), label);
    for bad in [
        json!({ "graph": FILE, "name": "1height", "type": "number", "default": 1 }),
        json!({ "graph": FILE, "name": "rough", "type": "number" }),
        json!({ "graph": FILE, "name": "look", "type": "enum", "default": "a" }),
        json!({ "graph": FILE, "name": "big", "type": "number", "default": 50, "max": 30 }),
    ] {
        assert_eq!(
            refused(&call(&mut st, "graph.declare_param", bad.clone())),
            "RE4200",
            "{bad}"
        );
    }

    // Binding through `graph.set_default`.
    ok(
        &mut st,
        "graph.set_default",
        json!({ "graph": FILE, "node": "h", "param": "height", "value": { "$param": "height" } }),
    );
    let data = ok(&mut st, "graph.params", json!({ "graph": FILE }));
    assert_eq!(data["params"][0]["in_use"], json!(6.0));
    assert_eq!(
        data["params"][0]["bound"],
        json!([{ "node": "h", "param": "height" }])
    );
    let unknown = call(
        &mut st,
        "graph.set_default",
        json!({ "graph": FILE, "node": "h", "param": "scale", "value": { "$param": "hieght" } }),
    );
    assert_eq!(refused(&unknown), "RE4201");
    // A param whose values the bound node refuses (`octaves` takes 1 to 16).
    ok(
        &mut st,
        "graph.declare_param",
        json!({ "graph": FILE, "name": "detail", "type": "integer", "default": 4, "min": 1, "max": 16 }),
    );
    ok(
        &mut st,
        "graph.set_default",
        json!({ "graph": FILE, "node": "h", "param": "octaves", "value": { "$param": "detail" } }),
    );
    let wider = call(
        &mut st,
        "graph.declare_param",
        json!({ "graph": FILE, "name": "detail", "type": "integer", "default": 4, "min": 1, "max": 20 }),
    );
    assert_eq!(refused(&wider), "RE4203");
    assert!(
        wider.diagnostics[0].message.contains("`h.octaves`"),
        "{}",
        wider.diagnostics[0].message
    );

    // A bound param is removed only with `unbind`, which writes its default.
    let bound = call(
        &mut st,
        "graph.remove_param",
        json!({ "graph": FILE, "name": "height" }),
    );
    assert_eq!(refused(&bound), "RE4201");
    assert!(bound.diagnostics[0].message.contains("`h.height`"));
    let data = ok(
        &mut st,
        "graph.remove_param",
        json!({ "graph": FILE, "name": "height", "unbind": true }),
    );
    assert_eq!(data["unbound"], json!([{ "node": "h", "param": "height" }]));
    let doc = open(&st);
    assert_eq!(doc.node("h").unwrap().params["height"], json!(6.0));
    assert_eq!(doc.params.len(), 1);
    assert_eq!(doc.version, 2, "nothing lowers the version");
    let gone = call(
        &mut st,
        "graph.remove_param",
        json!({ "graph": FILE, "name": "height" }),
    );
    assert_eq!(refused(&gone), "RE4201");
}

/// A version 2 [`small`] whose heightfield's `height` is bound to the
/// graph param `height` (6, from 1 to 30).
fn small_with_height() -> GraphDoc {
    let mut doc = small(3);
    doc.version = 2;
    doc.params = serde_json::from_value(json!([
        { "name": "height", "type": "number", "default": 6.0, "min": 1.0, "max": 30.0 }
    ]))
    .unwrap();
    doc.nodes[0]
        .params
        .insert("height".into(), json!({ "$param": "height" }));
    doc
}

fn source_params(st: &EditState) -> String {
    match st.world.get("s", "GraphSource", "params").unwrap() {
        Value::Str(s) => s,
        other => panic!("{other:?}"),
    }
}

fn fragment_hash(st: &EditState) -> String {
    st.world
        .ecs
        .resource::<GraphStore>()
        .report("s")
        .and_then(|r| r.fragment_hash)
        .expect("a report")
}

#[test]
fn a_source_sets_its_graphs_params() {
    let (_dir, mut st) = file_world(&small_with_height(), NodeRegistry::m1());
    let at_default = fragment_hash(&st);
    let set = |st: &mut EditState, param: &str, value: Json| {
        call(
            st,
            "graph.set_param",
            json!({ "source": "s", "param": param, "value": value }),
        )
    };
    let o = set(&mut st, "height", json!(12));
    assert!(o.ok, "{:#?}", o.diagnostics);
    assert_eq!(source_params(&st), r#"{"$graph":{"height":12}}"#);
    let r = st.world.regenerate_changed();
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    assert_ne!(fragment_hash(&st), at_default);
    let data = ok(&mut st, "graph.params", json!({ "source": "s" }));
    assert_eq!(data["params"][0]["value"], json!(12));
    assert_eq!(data["params"][0]["in_use"], json!(12));
    assert_eq!(data["file"], FILE);

    // Refused: out of range, the wrong type, a name the graph lacks.
    assert_eq!(refused(&set(&mut st, "height", json!(99))), "RE4202");
    assert_eq!(refused(&set(&mut st, "height", json!("tall"))), "RE4202");
    let typo = set(&mut st, "hieght", json!(3));
    assert_eq!(refused(&typo), "RE4201");
    assert!(
        typo.diagnostics[0]
            .message
            .contains("did you mean `height`?")
    );
    // A per-node override replaces the binding for the source, with a warning.
    let o = call(
        &mut st,
        "graph.set_param",
        json!({ "source": "s", "node": "h", "param": "height", "value": 3.0 }),
    );
    assert!(o.ok, "{:#?}", o.diagnostics);
    assert_eq!(codes(&o.diagnostics), ["RE4212"]);
    // `null` clears a value, even one of a param the graph no longer has.
    assert!(set(&mut st, "height", Json::Null).ok);
    assert_eq!(source_params(&st), r#"{"h":{"height":3.0}}"#);
    st.world
        .set(
            "s",
            "GraphSource",
            "params",
            Value::Str(r#"{"$graph":{"gone":1,"height":40}}"#.into()),
        )
        .unwrap();
    let r = st.world.regenerate_changed();
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    // Stale values are skipped with warnings, as stale overrides are.
    let report = st.world.ecs.resource::<GraphStore>().report("s").unwrap();
    let mut warned = codes(&report.diagnostics);
    warned.sort();
    assert_eq!(warned, ["RE4201", "RE4202"], "{:#?}", report.diagnostics);
    assert_eq!(fragment_hash(&st), at_default);
    let o = call(&mut st, "graph.params", json!({ "source": "s" }));
    assert!(o.ok);
    assert_eq!(o.data["params"][0]["in_use"], json!(6.0));
    assert!(set(&mut st, "gone", Json::Null).ok);
    assert_eq!(source_params(&st), r#"{"$graph":{"height":40}}"#);

    // `graph.generate` tries values without storing them.
    let data = ok(
        &mut st,
        "graph.generate",
        json!({ "source": "s", "params": { "$graph": { "height": 20 } } }),
    );
    assert!(data.is_object());
    let o = call(
        &mut st,
        "graph.generate",
        json!({ "source": "s", "params": { "$graph": { "height": 99 } } }),
    );
    assert_eq!(refused(&o), "RE4202");

    // Removing the param removes the sources' values for it.
    let data = ok(
        &mut st,
        "graph.remove_param",
        json!({ "graph": FILE, "name": "height", "unbind": true }),
    );
    assert_eq!(
        data["values_removed"],
        json!([{ "source": "s", "value": 40 }])
    );
    assert_eq!(source_params(&st), "");
}

#[test]
fn connecting_into_a_declared_output() {
    let mut start = small(3);
    start.version = 2;
    start.outputs =
        serde_json::from_value(json!([{ "name": "water", "type": "fragment" }])).unwrap();
    start.edges.push(
        serde_json::from_value(json!({ "from": ["sea", "fragment"], "to": ["$out", "water"] }))
            .unwrap(),
    );
    let (_dir, mut st) = file_world(&start, NodeRegistry::m1());
    let connect = |st: &mut EditState, from: [&str; 2], to: [&str; 2]| {
        call(
            st,
            "graph.connect",
            json!({ "graph": FILE, "from": from, "to": to }),
        )
    };
    // An output takes one edge: a new one replaces it.
    let o = connect(&mut st, ["ground", "out"], ["$out", "water"]);
    assert!(o.ok, "{:#?}", o.diagnostics);
    assert_eq!(
        o.data["replaced"],
        json!([{ "from": ["sea", "fragment"], "to": ["$out", "water"] }])
    );
    let edges: Vec<_> = open(&st)
        .edges
        .iter()
        .filter(|e| e.to.0 == "$out")
        .map(|e| e.from.0.clone())
        .collect();
    assert_eq!(edges, ["ground"]);
    // The declared type, and declared ports only.
    assert_eq!(
        refused(&connect(&mut st, ["h", "field"], ["$out", "water"])),
        "RE4206"
    );
    let undeclared = connect(&mut st, ["sea", "fragment"], ["$out", "watr"]);
    assert_eq!(refused(&undeclared), "RE4206");
    assert!(
        undeclared.diagnostics[0]
            .message
            .contains("did you mean `water`?")
    );
    assert_eq!(
        refused(&connect(&mut st, ["$in", "f"], ["ground", "field"])),
        "RE4206"
    );
}

fn stream_config() -> StreamConfig {
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

#[test]
fn streamed_cells_bind_params_and_keep_to_declared_outputs() {
    // The water's level is a graph param; the pond feeds no output.
    let graph = doc(json!({
        "format": "real-engine/graph", "version": 2, "name": "streamed", "seed": 1,
        "params": [{ "name": "sea", "type": "number", "default": 3.0, "min": 0.0, "max": 10.0 }],
        "outputs": [
            { "name": "ground", "type": "fragment" },
            { "name": "water", "type": "fragment" }
        ],
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "cell": 4.0, "height": 40.0, "scale": 0.01 } },
            { "id": "ground", "kind": "terrain.mesh", "version": 3, "params": {} },
            { "id": "water", "kind": "terrain.water_plane", "version": 1, "params": { "level": { "$param": "sea" } } },
            { "id": "pond", "kind": "terrain.water_plane", "version": 1, "params": { "level": 1.0 } }
        ],
        "edges": [
            { "from": ["h", "field"], "to": ["ground", "field"] },
            { "from": ["ground", "fragment"], "to": ["$out", "ground"] },
            { "from": ["water", "fragment"], "to": ["$out", "water"] }
        ]
    }));
    let mut st = EditState::new();
    register_generator(&mut st.world.generators);
    let mut store = GraphStore::with_evaluator(Arc::new(Evaluator::default()), None);
    store.insert("terrain", graph);
    st.world.ecs.insert_resource(store);
    let scene = json!({
        "format": "real-engine/scene", "version": 1,
        "entities": [
            { "id": "terrain", "name": "Terrain", "components": {
                "GraphSource": { "builder": "scripts/tools/world.luau" },
                "Procedural": { "generator": "graph", "seed": 7 },
                "Streamed": { "bounds": 256.0 },
                "Transform": {} } },
            { "id": "player", "name": "Player", "components": {
                "StreamFocus": {},
                "Transform": { "translation": [32.0, 0.0, 32.0] } } }
        ]
    });
    let w = &mut st.world;
    w.load(&re_scene::scene::from_json(&scene.to_string()).unwrap())
        .unwrap();
    let run = |w: &mut World, ticks: std::ops::Range<u64>| {
        for t in ticks {
            let r = step(w, t, &stream_config());
            assert!(r.diagnostics.is_empty(), "tick {t}: {:#?}", r.diagnostics);
        }
    };
    run(w, 0..12);
    assert_eq!(water_y(w, "c/0_0"), 3.0);
    assert!(w.entity("terrain/c/0_0/ground/terrain").is_ok());
    assert!(w.entity("terrain/c/0_0/pond/water").is_err());
    // A source's value reaches every cell.
    w.set(
        "terrain",
        "GraphSource",
        "params",
        Value::Str(r#"{"$graph":{"sea":5.0}}"#.into()),
    )
    .unwrap();
    run(w, 12..40);
    assert_eq!(water_y(w, "c/0_0"), 5.0);
}
