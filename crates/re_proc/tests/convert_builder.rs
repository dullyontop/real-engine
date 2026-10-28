//! `proc.convert_builder` (the ADR-0044 amendment, §7): a source whose graph
//! a builder script makes becomes one that reads a graph file, made through
//! an absent document (ADR-0064), and generates the same fragment from it;
//! one undo gives the builder back and empties the document, so the next
//! save removes the file.

mod support;

use re_command::{AllowAll, CommandOutcome, CommandRegistry, EditState, LogKind};
use re_proc::{CurrentGraph, GraphStore, register_commands, register_generator};
use re_reflect::Value;
use re_scene::DocFile;
use re_types::graph::GraphDoc;
use serde_json::{Value as Json, json};
use support::island;

const BUILDER: &str = "scripts/tools/island_graph.luau";
const PATH: &str = "graphs/island.graph.json";

/// A project holding the island's crate prefab, and an edit state whose
/// source `island` is a builder source: the store holds the graph its
/// builder made (as `re_app` fills it), and the source has generated once.
fn project() -> (tempfile::TempDir, EditState) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("prefabs")).unwrap();
    std::fs::copy(
        support::fixtures().join("prefabs/crate.prefab.json"),
        dir.path().join("prefabs/crate.prefab.json"),
    )
    .unwrap();
    let mut st = EditState::new();
    register_generator(&mut st.world.generators);
    let mut store = GraphStore::for_project(dir.path());
    store.insert("island", island(7));
    st.world.ecs.insert_resource(store);
    let scene = json!({
        "format": "real-engine/scene",
        "version": 1,
        "entities": [
            { "id": "world", "name": "World" },
            {
                "id": "island",
                "name": "Island",
                "parent": "world",
                "components": {
                    "GraphSource": { "builder": BUILDER },
                    "Procedural": { "generator": "graph", "seed": 7 },
                    "Transform": {}
                }
            }
        ]
    });
    let doc = re_scene::scene::from_json(&scene.to_string()).expect("scene parses");
    st.world.load(&doc).expect("scene loads");
    let report = st.world.regenerate_changed();
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    (dir, st)
}

fn dispatch(st: &mut EditState, args: Json, dry_run: bool) -> CommandOutcome {
    let mut r = CommandRegistry::new();
    register_commands(&mut r);
    r.dispatch(st, "proc.convert_builder", args, dry_run, &AllowAll)
}

fn convert(st: &mut EditState, args: Json) -> Json {
    let o = dispatch(st, args, false);
    assert!(o.ok, "{:#?}", o.diagnostics);
    o.data
}

fn code(o: &CommandOutcome) -> String {
    assert!(!o.ok, "expected a refusal: {:#?}", o.data);
    o.diagnostics[0].code.to_string()
}

/// Regenerates the source; the hash of the fragment its graph made.
fn regenerate(st: &mut EditState) -> String {
    let mut r = CommandRegistry::new();
    register_commands(&mut r);
    let o = r.dispatch(
        st,
        "graph.regenerate",
        json!({ "source": "island" }),
        false,
        &AllowAll,
    );
    assert!(o.ok, "{:#?}", o.diagnostics);
    st.world
        .ecs
        .resource::<GraphStore>()
        .report("island")
        .and_then(|r| r.fragment_hash)
        .expect("a fragment hash")
}

fn field(st: &EditState, name: &str) -> String {
    match st.world.get("island", "GraphSource", name) {
        Ok(Value::Str(s)) => s,
        other => panic!("GraphSource.{name}: {other:?}"),
    }
}

/// How many transactions were applied: each is an undo step.
fn steps(st: &EditState) -> usize {
    st.history
        .log()
        .iter()
        .filter(|e| e.kind == LogKind::Do)
        .count()
}

/// The island's built graph with the canvas's positions for `h` and `ground`.
fn placed() -> GraphDoc {
    let mut doc = island(7);
    doc.layout.insert("h".into(), [0.0, 0.0]);
    doc.layout.insert("ground".into(), [240.0, 30.0]);
    doc
}

#[test]
fn a_converted_source_generates_the_same_fragment_and_undo_gives_the_builder_back() {
    let (dir, mut st) = project();
    let built = regenerate(&mut st);
    let layout = json!({ "h": [0.0, 0.0], "ground": [240.0, 30.0] });

    // A dry run says what it would make, and changes nothing.
    let o = dispatch(
        &mut st,
        json!({ "source": "island", "layout": layout }),
        true,
    );
    assert!(o.ok, "{:#?}", o.diagnostics);
    assert_eq!(o.data["graph"], PATH);
    assert_eq!(o.data["dry_run"], true);
    assert!(st.world.doc(PATH).is_none());
    assert_eq!(field(&st, "builder"), BUILDER);
    assert_eq!(steps(&st), 0);

    let data = convert(&mut st, json!({ "source": "island", "layout": layout }));
    assert_eq!(
        data,
        json!({ "source": "island", "graph": PATH, "builder": BUILDER, "nodes": 9 })
    );
    assert_eq!(steps(&st), 1, "one undo step");
    assert_eq!(field(&st, "graph"), PATH);
    assert_eq!(field(&st, "builder"), "");
    assert!(!dir.path().join(PATH).exists(), "the save writes the file");

    // The document holds the built graph and the canvas's positions, and
    // the save would write them.
    let open = st.world.doc(PATH).expect("open");
    assert!(open.dirty && !open.is_absent());
    let doc: GraphDoc = serde_json::from_value(open.value.clone()).unwrap();
    assert_eq!(doc, placed());
    match st.world.doc_file(PATH) {
        Some(Ok(DocFile::Text(text))) => assert_eq!(GraphDoc::from_json(&text).unwrap(), placed()),
        other => panic!("{other:?}"),
    }

    // Generation reads the open document, and makes the same fragment.
    let store = st.world.ecs.resource::<GraphStore>();
    assert!(matches!(
        store.current(&st.world, "island"),
        Some(Ok(CurrentGraph::Open(_)))
    ));
    assert_eq!(regenerate(&mut st), built);

    // It is a builder source no more.
    let again = dispatch(&mut st, json!({ "source": "island" }), false);
    assert_eq!(code(&again), "RE4090");

    // Undo gives the builder back and empties the document, so the next
    // save removes the file; the builder's graph makes the fragment again.
    assert!(st.history.undo(&mut st.world).unwrap());
    assert_eq!(field(&st, "builder"), BUILDER);
    assert_eq!(field(&st, "graph"), "");
    assert!(st.world.doc(PATH).expect("still open").is_absent());
    assert_eq!(st.world.doc_file(PATH), Some(Ok(DocFile::Absent)));
    assert_eq!(regenerate(&mut st), built);

    // Redo gives the document its graph again.
    assert!(st.history.redo(&mut st.world).unwrap());
    assert_eq!(field(&st, "graph"), PATH);
    let doc: GraphDoc =
        serde_json::from_value(st.world.doc(PATH).expect("open").value.clone()).unwrap();
    assert_eq!(doc, placed());
    assert_eq!(regenerate(&mut st), built);

    // After an undo the absent document is free: converting again fills it.
    assert!(st.history.undo(&mut st.world).unwrap());
    convert(&mut st, json!({ "source": "island" }));
    let doc: GraphDoc =
        serde_json::from_value(st.world.doc(PATH).expect("open").value.clone()).unwrap();
    assert_eq!(doc, island(7), "no positions: the canvas lays it out");
}

#[test]
fn what_cannot_be_converted_is_refused_and_nothing_changes() {
    let (dir, mut st) = project();
    std::fs::create_dir_all(dir.path().join("graphs")).unwrap();
    std::fs::write(dir.path().join(PATH), "{}").unwrap();
    const OPEN: &str = "graphs/open.graph.json";
    st.world
        .open_doc(OPEN, serde_json::to_value(island(1)).unwrap())
        .unwrap();
    let hash = st.world.state_hash();
    let free = "graphs/free.graph.json";
    let cases = [
        // Not a procedural source.
        (json!({ "source": "world" }), "RE4017"),
        // The default file exists.
        (json!({ "source": "island" }), "RE4091"),
        // The file is open with content.
        (json!({ "source": "island", "graph": OPEN }), "RE4091"),
        // Not a graph file path in the project.
        (
            json!({ "source": "island", "graph": "../island.graph.json" }),
            "RE2052",
        ),
        (
            json!({ "source": "island", "graph": "graphs/island.json" }),
            "RE2052",
        ),
        // A position for a node the graph does not have.
        (
            json!({ "source": "island", "graph": free, "layout": { "nope": [0.0, 0.0] } }),
            "RE2005",
        ),
    ];
    for (args, expected) in cases {
        let o = dispatch(&mut st, args.clone(), false);
        assert_eq!(code(&o), expected, "{args}");
        assert_eq!(steps(&st), 0, "{args}");
        assert!(st.world.doc(free).is_none(), "{args}");
        assert!(st.world.doc(PATH).is_none(), "{args}");
        assert_eq!(st.world.state_hash(), hash, "{args}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join(PATH)).unwrap(),
        "{}"
    );

    // A source with no builder script has nothing to convert.
    st.world
        .set(
            "island",
            "GraphSource",
            "builder",
            Value::Str(String::new()),
        )
        .unwrap();
    let o = dispatch(&mut st, json!({ "source": "island", "graph": free }), false);
    assert_eq!(code(&o), "RE4090");
    assert!(st.world.doc(free).is_none());
}
