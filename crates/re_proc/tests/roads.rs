//! The polygon and road nodes (ADR-0043 §5) on `examples/roads.graph.json`:
//! what each produces, that it does not depend on the thread count, and
//! that shapes and paths survive the disk cache.

mod support;

use std::path::PathBuf;

use re_proc::{CacheState, EvalOptions, Output, ProcGraph};
use re_types::graph::GraphDoc;
use serde_json::json;
use support::{evaluator, graph, output_hash};

fn roads() -> ProcGraph {
    let doc = GraphDoc::from_json(include_str!("../examples/roads.graph.json")).unwrap();
    ProcGraph::from_doc(&doc, &re_proc::NodeRegistry::m1()).unwrap()
}

fn opts() -> EvalOptions {
    EvalOptions {
        root: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")),
        ..Default::default()
    }
}

#[test]
fn the_roads_example_does_what_it_says() {
    let g = roads();
    let e = evaluator(4).evaluate(&g, &opts());
    assert!(e.ok(), "{:#?}", e.report.diagnostics);
    let shapes = |id| match e.output(id).unwrap() {
        Output::Shapes(s) => s.clone(),
        o => panic!("{id}: {o:?}"),
    };
    let points = |id| match e.output(id).unwrap() {
        Output::Points(p) => p.clone(),
        o => panic!("{id}: {o:?}"),
    };
    let field = |id| match e.output(id).unwrap() {
        Output::Field(f) => f.clone(),
        o => panic!("{id}: {o:?}"),
    };
    // The two roads cross once.
    assert_eq!(e.report.node("roads").unwrap().logs, ["1 junctions"]);
    let print = shapes("print");
    assert_eq!(print.shapes.len(), 1, "the crossing roads are one area");
    // The yard is the plaza less the roads.
    let (plaza, yard) = (shapes("plaza"), shapes("yard"));
    assert!(
        yard.area() < plaza.area() && yard.area() > 0.0,
        "{} {}",
        yard.area(),
        plaza.area()
    );
    assert!((plaza.area() - (900.0 - 50.0)).abs() < 1e-6);
    // No masked point is on a road.
    let (spots, off) = (points("spots"), points("off_road"));
    assert!(off.points.len() < spots.points.len());
    for p in &off.points {
        assert!(!print.contains([f64::from(p.position.x), f64::from(p.position.z)]));
    }
    // The clearing mask is 0 on the roads and 1 far from them.
    let (clear, flat, height) = (field("clear"), field("flat"), field("height"));
    let main = match e.output("main").unwrap() {
        Output::Path(p) => p.clone(),
        o => panic!("{o:?}"),
    };
    let mid = main.samples[main.samples.len() / 2].position;
    let (x, z) = (mid[0] as f32, mid[2] as f32);
    assert_eq!(clear.0.height_at(x, z), Some(0.0));
    assert_eq!(clear.0.height_at(-60.0, 60.0), Some(1.0));
    // The terrain is flattened to just under the road, and untouched far
    // from it.
    let ground = flat.0.height_at(x, z).unwrap();
    assert!(
        (ground - (mid[1] as f32 - 0.05)).abs() < 0.05,
        "{ground} {mid:?}"
    );
    assert_eq!(
        flat.0.height_at(-60.0, 60.0),
        height.0.height_at(-60.0, 60.0)
    );
}

#[test]
fn outputs_do_not_depend_on_the_thread_count() {
    let g = roads();
    let a = evaluator(1).evaluate(&g, &opts());
    let b = evaluator(6).evaluate(&g, &opts());
    for id in g.nodes.keys() {
        assert_eq!(
            output_hash(a.output(id).unwrap()),
            output_hash(b.output(id).unwrap()),
            "node {id}"
        );
    }
}

#[test]
fn shapes_and_paths_come_back_from_the_disk_cache() {
    let dir = tempfile::tempdir().unwrap();
    let g = roads();
    let make = || evaluator(2).with_disk_cache(dir.path().to_path_buf());
    let first = make().evaluate(&g, &opts());
    assert!(first.ok());
    let second = make().evaluate(&g, &opts());
    for id in ["main", "print", "yard", "roads", "flat"] {
        assert_eq!(
            second.report.node(id).unwrap().cache,
            CacheState::Disk,
            "{id}"
        );
        assert_eq!(
            output_hash(first.output(id).unwrap()),
            output_hash(second.output(id).unwrap()),
            "{id}"
        );
    }
}

#[test]
fn bad_shapes_and_paths_report_their_codes() {
    let code = |nodes: &[(&str, &str, serde_json::Value)], edges: &[(&str, &str, &str, &str)]| {
        let doc = graph(1, nodes, edges);
        match ProcGraph::from_doc(&doc, &re_proc::NodeRegistry::m1()) {
            Err(diags) => diags.iter().map(|d| d.code.to_string()).collect::<Vec<_>>(),
            Ok(g) => evaluator(1)
                .evaluate(&g, &opts())
                .report
                .nodes
                .iter()
                .flat_map(|n| &n.diagnostics)
                .map(|d| d.code.to_string())
                .collect(),
        }
    };
    // Too few points is a parameter error.
    assert_eq!(
        code(
            &[("p", "shape.polygon", json!({ "points": [[0, 0], [1, 0]] }))],
            &[]
        ),
        ["RE4005"]
    );
    // Too far for the 1 mm grid.
    assert_eq!(
        code(
            &[(
                "p",
                "shape.polygon",
                json!({ "points": [[0, 0], [2.0e6, 0], [0, 1]] })
            )],
            &[]
        ),
        ["RE4005"]
    );
    // Control points on top of each other only fail when evaluated.
    assert_eq!(
        code(
            &[(
                "s",
                "path.spline",
                json!({ "points": [[1, 0, 1], [1, 0, 1]] })
            )],
            &[]
        ),
        ["RE4504"]
    );
    // A road mesh needs a path.
    assert_eq!(code(&[("r", "road.mesh", json!({}))], &[]), ["RE4008"]);
}
