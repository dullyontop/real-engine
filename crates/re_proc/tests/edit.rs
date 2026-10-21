//! The graph-editing commands (ADR-0044): each edits the graph file as an
//! open document in one undoable transaction of `set_doc` ops, checks what
//! the edit can break, and leaves the file on disk alone until a save.

mod support;

use re_command::{AllowAll, CommandOutcome, CommandRegistry, EditState};
use re_proc::{GraphStore, register_commands};
use re_types::graph::GraphDoc;
use serde_json::{Value as Json, json};
use support::island;

const PATH: &str = "graphs/island.graph.json";

fn commands() -> CommandRegistry<EditState> {
    let mut r = CommandRegistry::new();
    register_commands(&mut r);
    r
}

/// A project with the island graph in `graphs/`, and an edit state whose
/// store reads it.
fn project() -> (tempfile::TempDir, EditState) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("graphs")).unwrap();
    std::fs::write(dir.path().join(PATH), island(7).to_json()).unwrap();
    let mut st = EditState::new();
    st.world
        .ecs
        .insert_resource(GraphStore::for_project(dir.path()));
    (dir, st)
}

fn call(st: &mut EditState, name: &str, args: Json) -> CommandOutcome {
    commands().dispatch(st, name, args, false, &AllowAll)
}

fn ok(st: &mut EditState, name: &str, args: Json) -> Json {
    let o = call(st, name, args);
    assert!(o.ok, "{name}: {:#?}", o.diagnostics);
    o.data
}

fn code(o: &CommandOutcome) -> String {
    assert!(!o.ok, "expected a failure: {:#?}", o.data);
    o.diagnostics[0].code.to_string()
}

fn open(st: &EditState) -> GraphDoc {
    serde_json::from_value(st.world.doc(PATH).expect("open").value.clone()).unwrap()
}

fn undo(st: &mut EditState) {
    assert!(st.history.undo(&mut st.world).unwrap());
}

fn redo(st: &mut EditState) {
    assert!(st.history.redo(&mut st.world).unwrap());
}

#[test]
fn add_node_opens_the_file_checks_the_kind_and_is_one_undo_step() {
    let (dir, mut st) = project();
    let before = island(7);
    let data = ok(
        &mut st,
        "graph.add_node",
        json!({ "graph": PATH, "kind": "field.combine", "params": { "op": "add" }, "pos": [40.0, 8.0] }),
    );
    assert_eq!(data["node"], "combine_1");
    let doc = open(&st);
    let node = doc.node("combine_1").unwrap();
    assert_eq!(node.kind, "field.combine");
    assert_eq!(node.params["op"], "add");
    assert_eq!(doc.layout["combine_1"], [40.0, 8.0]);
    // The file is written by a save, not by the edit.
    assert_eq!(
        std::fs::read_to_string(dir.path().join(PATH)).unwrap(),
        before.to_json()
    );
    assert_eq!(st.world.unsaved_docs(), [PATH]);

    // A second node gets the next free id.
    let data = ok(
        &mut st,
        "graph.add_node",
        json!({ "graph": PATH, "kind": "field.combine" }),
    );
    assert_eq!(data["node"], "combine_2");

    undo(&mut st);
    undo(&mut st);
    assert_eq!(open(&st), before);
    assert!(st.world.unsaved_docs().is_empty());
    redo(&mut st);
    assert_eq!(open(&st).layout["combine_1"], [40.0, 8.0]);

    let unknown = call(
        &mut st,
        "graph.add_node",
        json!({ "graph": PATH, "kind": "field.combin" }),
    );
    assert_eq!(code(&unknown), "RE4001");
    assert!(unknown.diagnostics[0].message.contains("field.combine"));
    let bad = call(
        &mut st,
        "graph.add_node",
        json!({ "graph": PATH, "kind": "field.combine", "params": { "op": "pow" } }),
    );
    assert_eq!(code(&bad), "RE4005");
    let taken = call(
        &mut st,
        "graph.add_node",
        json!({ "graph": PATH, "kind": "field.combine", "id": "h" }),
    );
    assert_eq!(code(&taken), "RE4000");
    let not_graph = call(
        &mut st,
        "graph.add_node",
        json!({ "graph": "scenes/main.scene.json", "kind": "field.combine" }),
    );
    assert_eq!(code(&not_graph), "RE4000");
}

#[test]
fn connect_checks_ports_types_and_cycles_and_replaces_the_inputs_edge() {
    let (_dir, mut st) = project();
    let mismatch = call(
        &mut st,
        "graph.connect",
        json!({ "graph": PATH, "from": ["grid", "points"], "to": ["ground", "field"] }),
    );
    assert_eq!(code(&mismatch), "RE4003");
    let no_port = call(
        &mut st,
        "graph.connect",
        json!({ "graph": PATH, "from": ["h", "field"], "to": ["ground", "heights"] }),
    );
    assert_eq!(code(&no_port), "RE4002");
    let no_output = call(
        &mut st,
        "graph.connect",
        json!({ "graph": PATH, "from": ["h", "mesh"], "to": ["ground", "field"] }),
    );
    assert_eq!(code(&no_output), "RE4002");

    // land -> c2.a, then c2 -> land.a would close a cycle.
    ok(
        &mut st,
        "graph.add_node",
        json!({ "graph": PATH, "kind": "field.combine", "id": "c2" }),
    );
    ok(
        &mut st,
        "graph.connect",
        json!({ "graph": PATH, "from": ["land", "field"], "to": ["c2", "a"] }),
    );
    let cycle = call(
        &mut st,
        "graph.connect",
        json!({ "graph": PATH, "from": ["c2", "out"], "to": ["land", "a"] }),
    );
    assert_eq!(code(&cycle), "RE4004");

    // `mask -> land.a` replaces `h -> land.a` in one step.
    let before = open(&st);
    let data = ok(
        &mut st,
        "graph.connect",
        json!({ "graph": PATH, "from": ["mask", "out"], "to": ["land", "a"] }),
    );
    assert_eq!(
        data["replaced"],
        json!([{ "from": ["h", "field"], "to": ["land", "a"] }])
    );
    let doc = open(&st);
    let into_a: Vec<_> = doc
        .edges
        .iter()
        .filter(|e| e.to == ("land".into(), "a".into()))
        .collect();
    assert_eq!(into_a.len(), 1);
    assert_eq!(into_a[0].from, ("mask".into(), "field".into()));
    undo(&mut st);
    assert_eq!(open(&st), before);

    ok(
        &mut st,
        "graph.disconnect",
        json!({ "graph": PATH, "from": ["h", "field"], "to": ["land", "a"] }),
    );
    assert!(
        !open(&st)
            .edges
            .iter()
            .any(|e| e.to == ("land".into(), "a".into()))
    );
    let gone = call(
        &mut st,
        "graph.disconnect",
        json!({ "graph": PATH, "from": ["h", "field"], "to": ["land", "a"] }),
    );
    assert_eq!(code(&gone), "RE4000");
}

#[test]
fn remove_node_takes_its_edges_and_position_in_one_step() {
    let (_dir, mut st) = project();
    ok(
        &mut st,
        "graph.move_nodes",
        json!({ "graph": PATH, "positions": { "land": [1.0, 2.0], "h": [0.0, 0.0] } }),
    );
    let before = open(&st);
    let data = ok(
        &mut st,
        "graph.remove_node",
        json!({ "graph": PATH, "node": "land" }),
    );
    assert_eq!(data["edges_removed"].as_array().unwrap().len(), 4);
    let doc = open(&st);
    assert!(doc.node("land").is_none());
    assert!(
        !doc.edges
            .iter()
            .any(|e| e.from.0 == "land" || e.to.0 == "land")
    );
    assert!(!doc.layout.contains_key("land"));
    assert_eq!(doc.edges.len(), 2);
    undo(&mut st);
    assert_eq!(open(&st), before);
    assert_eq!(open(&st).to_json(), before.to_json());
}

#[test]
fn move_nodes_writes_layout_only() {
    let (_dir, mut st) = project();
    // No `layout` yet: the first move creates it, in one op.
    let o = call(
        &mut st,
        "graph.move_nodes",
        json!({ "graph": PATH, "positions": { "h": [5.0, 6.0], "mask": [7.0, 8.0] } }),
    );
    assert!(o.ok, "{:#?}", o.diagnostics);
    assert_eq!(o.changes.len(), 1);
    let o = call(
        &mut st,
        "graph.move_nodes",
        json!({ "graph": PATH, "positions": { "h": [9.0, 9.0] } }),
    );
    assert_eq!(o.changes.len(), 1);
    let doc = open(&st);
    assert_eq!(doc.layout["h"], [9.0, 9.0]);
    assert_eq!(doc.layout["mask"], [7.0, 8.0]);
    let mut plain = doc.clone();
    plain.layout.clear();
    assert_eq!(plain, island(7));
    let unknown = call(
        &mut st,
        "graph.move_nodes",
        json!({ "graph": PATH, "positions": { "hh": [0.0, 0.0] } }),
    );
    assert_eq!(code(&unknown), "RE4000");
    assert!(unknown.diagnostics[0].message.contains("`h`"));
}

#[test]
fn set_default_changes_the_files_parameter() {
    let (_dir, mut st) = project();
    ok(
        &mut st,
        "graph.set_default",
        json!({ "graph": PATH, "node": "h", "param": "height", "value": 25.0 }),
    );
    assert_eq!(open(&st).node("h").unwrap().params["height"], 25.0);
    let bad = call(
        &mut st,
        "graph.set_default",
        json!({ "graph": PATH, "node": "h", "param": "height", "value": "tall" }),
    );
    assert_eq!(code(&bad), "RE4005");
    let unknown = call(
        &mut st,
        "graph.set_default",
        json!({ "graph": PATH, "node": "h", "param": "heigth", "value": 1 }),
    );
    assert_eq!(code(&unknown), "RE4005");
    ok(
        &mut st,
        "graph.set_default",
        json!({ "graph": PATH, "node": "h", "param": "height", "value": null }),
    );
    assert!(!open(&st).node("h").unwrap().params.contains_key("height"));
    undo(&mut st);
    undo(&mut st);
    assert_eq!(open(&st), island(7));
}

#[test]
fn a_dry_run_changes_nothing() {
    let (_dir, mut st) = project();
    let o = commands().dispatch(
        &mut st,
        "graph.remove_node",
        json!({ "graph": PATH, "node": "land" }),
        true,
        &AllowAll,
    );
    assert!(o.ok, "{:#?}", o.diagnostics);
    assert_eq!(o.changes.len(), 5); // four edges and the node
    // The file was opened for the dry run only.
    assert!(st.world.doc(PATH).is_none());
    assert!(st.history.log().is_empty());

    // A refused edit leaves the file closed too; one already open stays open.
    let refused = call(
        &mut st,
        "graph.connect",
        json!({ "graph": PATH, "from": ["land", "field"], "to": ["land", "a"] }),
    );
    assert_eq!(code(&refused), "RE4004", "a self-loop is a cycle");
    assert!(st.world.doc(PATH).is_none());
    ok(
        &mut st,
        "graph.move_nodes",
        json!({ "graph": PATH, "positions": { "h": [1.0, 1.0] } }),
    );
    let refused = call(
        &mut st,
        "graph.connect",
        json!({ "graph": PATH, "from": ["land", "field"], "to": ["land", "a"] }),
    );
    assert_eq!(code(&refused), "RE4004");
    assert_eq!(open(&st).layout["h"], [1.0, 1.0]);
}

#[test]
fn node_ids_and_positions_are_checked() {
    let (_dir, mut st) = project();
    for id in ["a/b", "a b", "ä", ""] {
        let o = call(
            &mut st,
            "graph.add_node",
            json!({ "graph": PATH, "kind": "field.combine", "id": id }),
        );
        assert_eq!(code(&o), "RE4000", "{id:?}");
        assert!(
            o.diagnostics[0].message.contains("not a valid node id"),
            "{id:?}"
        );
    }
    ok(
        &mut st,
        "graph.add_node",
        json!({ "graph": PATH, "kind": "field.combine", "id": "Mix-2_b" }),
    );
    // Too large for f32: not finite.
    let far = call(
        &mut st,
        "graph.add_node",
        json!({ "graph": PATH, "kind": "field.combine", "pos": [1e300, 0.0] }),
    );
    assert_eq!(code(&far), "RE4000");
    assert!(far.diagnostics[0].message.contains("finite"));
    let far = call(
        &mut st,
        "graph.move_nodes",
        json!({ "graph": PATH, "positions": { "h": [0.0, -1e300] } }),
    );
    assert_eq!(code(&far), "RE4000");
    assert!(open(&st).node("combine_1").is_none());
}

#[test]
fn add_node_is_not_idempotent() {
    let idempotent = |name| commands().spec(name).unwrap().annotations.idempotent_hint;
    assert!(!idempotent("graph.add_node"));
    for name in [
        "graph.remove_node",
        "graph.connect",
        "graph.disconnect",
        "graph.move_nodes",
        "graph.set_default",
    ] {
        assert!(idempotent(name), "{name}");
    }
}

#[test]
fn remove_node_clears_the_sources_overrides_of_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("graphs")).unwrap();
    std::fs::write(dir.path().join(PATH), island(7).to_json()).unwrap();
    // An override of an entity `crates` generates, which stays (RE4010).
    let mut st = support::world(
        island(7),
        json!([{ "target": "crates/0", "deleted": true }]),
    );
    let text = |s: &str| re_reflect::Value::Str(s.into());
    st.world
        .set("island", "GraphSource", "graph", text(PATH))
        .unwrap();
    st.world
        .set(
            "island",
            "GraphSource",
            "params",
            text(r#"{"crates":{"prefab":"prefabs/crate.prefab.json"},"h":{"height":3.0}}"#),
        )
        .unwrap();
    st.world
        .ecs
        .insert_resource(GraphStore::for_project(dir.path()));
    let data = ok(
        &mut st,
        "graph.remove_node",
        json!({ "graph": PATH, "node": "crates" }),
    );
    assert_eq!(
        data["params_removed"],
        json!([{ "source": "island", "params": { "prefab": "prefabs/crate.prefab.json" } }])
    );
    assert_eq!(
        data["orphaned_overrides"],
        json!([{ "source": "island", "target": "crates/0" }])
    );
    let params = |st: &EditState| match st.world.get("island", "GraphSource", "params") {
        Ok(re_reflect::Value::Str(s)) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(params(&st), r#"{"h":{"height":3.0}}"#);
    // A node that takes the id again starts without them.
    ok(
        &mut st,
        "graph.add_node",
        json!({ "graph": PATH, "kind": "field.combine", "id": "crates" }),
    );
    assert_eq!(params(&st), r#"{"h":{"height":3.0}}"#);
    // Undoing the removal brings the overrides back with the node.
    undo(&mut st);
    undo(&mut st);
    assert!(params(&st).contains("prefab"));
    assert_eq!(open(&st).node("crates").unwrap().kind, "points.instance");
}

#[test]
fn set_default_refuses_a_node_of_an_older_version() {
    let (_dir, mut st) = project();
    let mut doc = island(7);
    doc.nodes[0].version = 0; // `h`, written before the current version
    std::fs::write(_dir.path().join(PATH), doc.to_json()).unwrap();
    let mut registry = re_proc::NodeRegistry::m1();
    let mut def = registry.get("terrain.heightfield").unwrap().clone();
    def.version = 2;
    def.oldest = 0;
    registry.register(def);
    st.world.ecs.insert_resource(GraphStore::with_evaluator(
        std::sync::Arc::new(re_proc::Evaluator::new(std::sync::Arc::new(registry))),
        Some(_dir.path().to_path_buf()),
    ));
    let o = call(
        &mut st,
        "graph.set_default",
        json!({ "graph": PATH, "node": "h", "param": "height", "value": 25.0 }),
    );
    assert_eq!(code(&o), "RE4019");
    assert!(
        o.diagnostics[0]
            .hint
            .as_deref()
            .unwrap()
            .contains("real migrate-graph")
    );
    assert!(st.world.doc(PATH).is_none());
}
