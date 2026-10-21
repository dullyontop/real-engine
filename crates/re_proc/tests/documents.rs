//! Graph files as open documents (ADR-0044): `layout` is ignored by
//! generation, node keys, the document hash and the version stamp; the
//! `graph` generator reads an open document in preference to the stored
//! graph; `.graph.json` documents are validated.

mod support;

use re_command::{History, Op, Origin, Transaction};
use re_proc::canon::doc_hash;
use re_proc::{
    CurrentGraph, EvalOptions, GraphStore, NodeRegistry, ProcGraph, register_doc_validators,
    version_stamp,
};
use re_reflect::Value;
use re_scene::World;
use serde_json::json;
use support::{evaluator, fixtures, island, world};

const PATH: &str = "graphs/island.graph.json";

fn with_layout(seed: i64) -> re_types::graph::GraphDoc {
    let mut doc = island(seed);
    doc.layout.insert("h".into(), [10.0, -4.5]);
    doc.layout.insert("grid".into(), [300.0, 80.0]);
    doc
}

fn crates(w: &World) -> usize {
    w.ids()
        .iter()
        .filter(|(id, _)| id.starts_with("island/crates/"))
        .count()
}

fn set_doc(pointer: &str, value: Option<serde_json::Value>) -> Transaction {
    Transaction::new(
        "edit graph",
        Origin::User,
        vec![Op::SetDoc {
            doc: PATH.into(),
            pointer: pointer.into(),
            value,
        }],
    )
}

#[test]
fn layout_is_not_part_of_hashes_keys_or_the_ir() {
    let registry = NodeRegistry::m1();
    let (plain, laid) = (island(7), with_layout(7));
    assert_eq!(doc_hash(&plain), doc_hash(&laid));
    assert_eq!(
        version_stamp(&plain, &registry),
        version_stamp(&laid, &registry)
    );

    // Node cache keys do not change.
    let opts = EvalOptions {
        root: Some(fixtures()),
        ..Default::default()
    };
    let a = ProcGraph::from_doc(&plain, &registry).unwrap();
    let b = ProcGraph::from_doc(&laid, &registry).unwrap();
    let ka: Vec<String> = evaluator(1)
        .evaluate(&a, &opts)
        .report
        .nodes
        .into_iter()
        .map(|n| n.key)
        .collect();
    let kb: Vec<String> = evaluator(1)
        .evaluate(&b, &opts)
        .report
        .nodes
        .into_iter()
        .map(|n| n.key)
        .collect();
    assert!(ka.iter().all(|k| !k.is_empty()));
    assert_eq!(ka, kb);

    // `from_doc` and `to_doc` carry the layout through unchanged.
    assert_eq!(b.layout, laid.layout);
    assert_eq!(b.to_doc(), laid);
    assert_eq!(b.to_doc().to_json(), laid.to_json());
    assert!(a.to_doc().layout.is_empty());

    // The source's fingerprint does not change: storing the same graph
    // with a layout regenerates nothing.
    let mut st = world(island(7), json!([]));
    st.world
        .ecs
        .resource_mut::<GraphStore>()
        .insert("island", with_layout(7));
    let report = st.world.regenerate_changed();
    assert!(report.sources.is_empty(), "{report:?}");
}

#[test]
fn the_generator_reads_the_open_document() {
    let mut st = world(island(7), json!([]));
    st.world
        .set("island", "GraphSource", "graph", Value::Str(PATH.into()))
        .unwrap();
    st.world.regenerate_changed();
    let before = crates(&st.world);
    assert!(before > 0);
    let mut h = History::default();

    // Opening the file with the same content regenerates nothing.
    register_doc_validators(&mut st.world);
    st.world
        .open_doc(PATH, serde_json::to_value(island(7)).unwrap())
        .unwrap();
    assert!(st.world.regenerate_changed().sources.is_empty());

    // Moving a node does not regenerate either.
    h.execute(
        &mut st.world,
        set_doc("/layout", Some(json!({"h": [1.0, 2.0]}))),
    )
    .unwrap();
    assert!(st.world.regenerate_changed().sources.is_empty());

    // An unsaved parameter edit does: half as many grid points per side.
    assert_eq!(st.world.doc(PATH).unwrap().value["nodes"][5]["id"], "grid");
    h.execute(
        &mut st.world,
        set_doc("/nodes/5/params/spacing", Some(json!(24.0))),
    )
    .unwrap();
    assert_eq!(st.world.regenerate_changed().sources, ["island"]);
    let after = crates(&st.world);
    assert!(after < before, "{after} < {before}");
    {
        let store = st.world.ecs.resource::<GraphStore>();
        let current = store.current(&st.world, "island").unwrap().unwrap();
        assert!(matches!(current, CurrentGraph::Open(_)));
        assert_eq!(current.node("grid").unwrap().params["spacing"], 24.0);
        assert_eq!(current.layout["h"], [1.0, 2.0]);
        // The stored graph (the file as loaded) is untouched.
        assert_eq!(
            store.graph("island").unwrap().node("grid").unwrap().params["spacing"],
            12.0
        );
    }

    // A world copy generates from the copy's open document.
    let mut copy = st.world.fork();
    copy.ecs
        .insert_resource(st.world.ecs.resource::<GraphStore>().clone());
    assert_eq!(copy.doc(PATH), st.world.doc(PATH));
    let fresh = copy.entity("island").unwrap();
    copy.regenerate(fresh);
    assert_eq!(crates(&copy), after);

    // A graph left invalid is rejected (RE2008) and nothing changes.
    let err = h
        .execute(&mut st.world, set_doc("/nodes/0", None))
        .unwrap_err();
    assert_eq!(err.error.code().to_string(), "RE2008");
    assert!(err.to_string().contains("which does not exist"), "{err}");
    assert!(st.world.regenerate_changed().sources.is_empty());

    // Undo brings the old parameters back, and the old output.
    h.undo(&mut st.world).unwrap();
    assert_eq!(st.world.regenerate_changed().sources, ["island"]);
    assert_eq!(crates(&st.world), before);

    // Closing the document falls back to the stored graph.
    st.world.close_doc(PATH);
    let store = st.world.ecs.resource::<GraphStore>();
    assert!(matches!(
        store.current(&st.world, "island"),
        Some(Ok(CurrentGraph::Stored(_)))
    ));
}

#[test]
fn graph_documents_are_validated_by_suffix() {
    let mut w = World::new();
    register_doc_validators(&mut w);
    let mut doc = serde_json::to_value(island(1)).unwrap();
    w.open_doc(PATH, doc.clone()).unwrap();
    doc["colour"] = json!(1);
    let err = w.open_doc("graphs/other.graph.json", doc).unwrap_err();
    assert!(err.to_string().contains("invalid graph file"), "{err}");
    // Other files are not graphs.
    w.open_doc("notes/a.json", json!({"anything": true}))
        .unwrap();
    assert_eq!(w.validate_doc("notes/a.json"), Some(Ok(())));
}

#[test]
fn a_graph_file_of_any_name_is_validated_and_an_invalid_one_is_reported() {
    const OTHER: &str = "graphs/island.json";
    let mut st = world(island(7), json!([]));
    st.world
        .set("island", "GraphSource", "graph", Value::Str(OTHER.into()))
        .unwrap();
    st.world.regenerate_changed();

    // Opened before any validator knows it is a graph: an invalid open
    // document is an error, not a silent fall back to the stored graph.
    let mut bad = serde_json::to_value(island(7)).unwrap();
    bad["edges"][0]["from"][0] = json!("nobody");
    st.world.open_doc(OTHER, bad).unwrap();
    {
        let store = st.world.ecs.resource::<GraphStore>();
        let err = store.current(&st.world, "island").unwrap().unwrap_err();
        assert_eq!(err.code.to_string(), "RE4000");
        assert!(err.message.contains("graphs/island.json"), "{err:?}");
    }
    let report = st.world.regenerate_changed();
    assert_eq!(report.sources, ["island"]);
    assert!(!report.diagnostics.is_empty());

    // Registering validators covers every source's graph file, whatever
    // its name: a breaking edit is refused.
    register_doc_validators(&mut st.world);
    st.world
        .open_doc(OTHER, serde_json::to_value(island(7)).unwrap())
        .unwrap();
    let mut h = History::default();
    let breaking = Transaction::new(
        "edit graph",
        Origin::User,
        vec![Op::SetDoc {
            doc: OTHER.into(),
            pointer: "/nodes/0".into(),
            value: None,
        }],
    );
    let err = h.execute(&mut st.world, breaking).unwrap_err();
    assert_eq!(err.error.code().to_string(), "RE2008");
}

#[test]
fn layout_positions_must_be_finite() {
    let mut w = World::new();
    register_doc_validators(&mut w);
    w.open_doc(PATH, serde_json::to_value(island(1)).unwrap())
        .unwrap();
    let mut h = History::default();
    let err = h
        .execute(&mut w, set_doc("/layout", Some(json!({"h": [1e300, 0.0]}))))
        .unwrap_err();
    assert_eq!(err.error.code().to_string(), "RE2008");
    assert!(err.to_string().contains("not a finite pair"), "{err}");
    assert!(re_proc::validate_graph_json(&w.doc(PATH).unwrap().value).is_ok());
}
