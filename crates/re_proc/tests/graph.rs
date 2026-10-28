//! Graph validation (`RE4000`–`RE4007`, `RE4012`), the node registry and the
//! diagnostic catalogue.

mod support;

use std::collections::BTreeMap;

use re_proc::{CODE_RANGE, DIAGNOSTICS, NodeRegistry, ProcGraph};
use re_types::graph::GraphDoc;
use serde_json::{Value as Json, json};
use support::{graph, island};

fn codes(doc: &GraphDoc) -> Vec<String> {
    match ProcGraph::from_doc(doc, &NodeRegistry::m1()) {
        Ok(_) => Vec::new(),
        Err(ds) => ds.iter().map(|d| d.code.to_string()).collect(),
    }
}

fn one(kind: &str, params: Json) -> Vec<(&'static str, String, Json)> {
    vec![("n", kind.to_string(), params)]
}

fn doc_of(nodes: &[(&str, String, Json)], edges: &[(&str, &str, &str, &str)]) -> GraphDoc {
    let nodes: Vec<(&str, &str, Json)> = nodes
        .iter()
        .map(|(a, b, c)| (*a, b.as_str(), c.clone()))
        .collect();
    graph(1, &nodes, edges)
}

#[test]
fn a_valid_graph_compiles_in_levels() {
    let g = ProcGraph::from_doc(&island(1), &NodeRegistry::m1()).unwrap();
    assert_eq!(g.nodes.len(), 9);
    let levels = g.levels().unwrap();
    assert_eq!(levels.iter().map(Vec::len).sum::<usize>(), 9);
    // Every edge goes from an earlier level to a later one.
    let level_of = |id: &str| {
        let i = g.nodes.get_index_of(id).unwrap();
        levels.iter().position(|l| l.contains(&i)).unwrap()
    };
    for e in &g.edges {
        assert!(level_of(&e.from.0) < level_of(&e.to.0), "{e:?}");
    }
    // The IR round-trips to an equivalent document.
    let back = ProcGraph::from_doc(&g.to_doc(), &NodeRegistry::m1()).unwrap();
    assert_eq!(
        back.nodes.keys().collect::<Vec<_>>(),
        g.nodes.keys().collect::<Vec<_>>()
    );
    assert_eq!(back.edges, g.edges);
}

#[test]
fn out_names_any_nodes_output_port() {
    let with_names = island(1);
    let mut with_out = with_names.clone();
    for e in &mut with_out.edges {
        e.from.1 = "out".into();
    }
    let a = ProcGraph::from_doc(&with_names, &NodeRegistry::m1()).unwrap();
    let b = ProcGraph::from_doc(&with_out, &NodeRegistry::m1()).unwrap();
    assert_eq!(a.edges, b.edges, "`out` is stored as the real port name");
}

#[test]
fn every_validation_problem_has_its_code() {
    let h = || {
        (
            "h",
            "terrain.heightfield".to_string(),
            json!({ "size": 16 }),
        )
    };
    let b = || ("b", "mesh.primitive".to_string(), json!({}));
    let cases: Vec<(&str, GraphDoc, &str)> = vec![
        (
            "unknown kind",
            doc_of(&one("terrain.volcano", json!({})), &[]),
            "RE4001",
        ),
        (
            "bad parameter value",
            doc_of(&one("terrain.heightfield", json!({ "size": 1 })), &[]),
            "RE4005",
        ),
        (
            "unknown parameter",
            doc_of(&one("terrain.heightfield", json!({ "sise": 64 })), &[]),
            "RE4005",
        ),
        (
            "wrong parameter type",
            doc_of(&one("mesh.primitive", json!({ "shape": 3 })), &[]),
            "RE4005",
        ),
        (
            "unknown output port",
            doc_of(
                &[h(), ("m", "terrain.mesh".into(), json!({}))],
                &[("h", "mesh", "m", "field")],
            ),
            "RE4002",
        ),
        (
            "unknown input port",
            doc_of(
                &[h(), ("m", "terrain.mesh".into(), json!({}))],
                &[("h", "field", "m", "ground")],
            ),
            "RE4002",
        ),
        (
            "type mismatch",
            doc_of(
                &[b(), ("m", "terrain.mesh".into(), json!({}))],
                &[("b", "mesh", "m", "field")],
            ),
            "RE4003",
        ),
        (
            "cycle",
            doc_of(
                &[
                    ("x", "field.remap".into(), json!({})),
                    ("y", "field.remap".into(), json!({})),
                ],
                &[("x", "field", "y", "field"), ("y", "field", "x", "field")],
            ),
            "RE4004",
        ),
        (
            "missing input",
            doc_of(&one("terrain.mesh", json!({})), &[]),
            "RE4006",
        ),
        (
            "input connected twice",
            doc_of(
                &[
                    h(),
                    ("h2", "terrain.heightfield".into(), json!({})),
                    ("m", "terrain.mesh".into(), json!({})),
                ],
                &[("h", "field", "m", "field"), ("h2", "field", "m", "field")],
            ),
            "RE4007",
        ),
    ];
    for (what, doc, code) in cases {
        let got = codes(&doc);
        assert!(
            got.iter().any(|c| c == code),
            "{what}: expected {code}, got {got:?}"
        );
    }

    // A future node version.
    let mut doc = doc_of(&one("terrain.heightfield", json!({})), &[]);
    doc.nodes[0].version = 99;
    assert_eq!(codes(&doc), ["RE4012"]);

    // A malformed document (duplicate ids, built in code) is RE4000.
    let mut doc = doc_of(&[h()], &[]);
    doc.nodes.push(doc.nodes[0].clone());
    assert_eq!(codes(&doc), ["RE4000"]);
}

#[test]
fn all_problems_are_reported_at_once() {
    let doc = doc_of(
        &[
            ("a", "terrain.volcano".into(), json!({})),
            ("b", "terrain.heightfield".into(), json!({ "size": 0 })),
            ("c", "terrain.mesh".into(), json!({})),
        ],
        &[],
    );
    let mut got = codes(&doc);
    got.sort();
    assert_eq!(got, ["RE4001", "RE4005", "RE4006"]);
}

#[test]
fn the_registry_describes_44_kinds_with_schemas() {
    let r = NodeRegistry::m1();
    let kinds: Vec<&str> = r.kinds().collect();
    assert_eq!(kinds.len(), 44, "{kinds:?}");
    for kind in &kinds {
        let d = r.get(kind).unwrap().describe();
        assert_eq!(d["kind"], json!(kind));
        assert!(d["params"].is_object(), "{kind}: {d}");
        assert!(d["output"]["name"].is_string(), "{kind}: {d}");
    }
    let schema = r.graph_json_schema();
    let text = schema.to_string();
    for kind in &kinds {
        assert!(text.contains(kind), "graph.schema.json lacks {kind}");
    }
}

#[test]
fn every_schema_ref_resolves() {
    fn refs(v: &Json, out: &mut Vec<String>) {
        match v {
            Json::Object(m) => {
                for (k, x) in m {
                    match (k.as_str(), x) {
                        ("$ref", Json::String(r)) => out.push(r.clone()),
                        _ => refs(x, out),
                    }
                }
            }
            Json::Array(a) => a.iter().for_each(|x| refs(x, out)),
            _ => {}
        }
    }
    let schema = NodeRegistry::m1().graph_json_schema();
    let mut all = Vec::new();
    refs(&schema, &mut all);
    assert!(all.len() > 5, "{all:?}");
    for r in all {
        let pointer = r.strip_prefix('#').expect("local refs only");
        assert!(schema.pointer(pointer).is_some(), "dangling {r}");
    }
}

#[test]
fn the_example_island_graph_is_valid() {
    let doc = GraphDoc::from_json(include_str!("../examples/island.graph.json")).unwrap();
    assert!(codes(&doc).is_empty());
}

#[test]
fn catalogue_is_valid() {
    let problems = re_core::validate_catalog(DIAGNOSTICS, CODE_RANGE);
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn catalogue_matches_the_explain_pages() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("explain");
    let mut pages = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let code = name.strip_suffix(".md").unwrap().to_string();
        pages.insert(
            code,
            std::fs::read_to_string(&path)
                .unwrap()
                .replace("\r\n", "\n"),
        );
    }
    let catalogue: BTreeMap<String, String> = DIAGNOSTICS
        .iter()
        .map(|d| (d.code.to_string(), d.page.replace("\r\n", "\n")))
        .collect();
    assert_eq!(catalogue.len(), DIAGNOSTICS.len(), "duplicate codes");
    assert_eq!(catalogue, pages);
}
