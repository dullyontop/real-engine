//! Scheduling: a node starts as soon as the nodes it reads are done; a
//! deadline or a cancel stops nodes while they run, and nothing a stopped
//! node returns is used or cached; a node evaluates a graph inside its own
//! evaluation (`NodeCtx::evaluate`).

mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use glam::Vec3;
use indexmap::IndexMap;
use re_core::time::Instant;
use re_proc::{
    CacheState, Edge, EvalOptions, Evaluation, Evaluator, Field, Node, NodeCtx, NodeDef, NodeError,
    NodeRegistry, NodeSpec, Output, Params, Point, Points, PortDef, PortType, ProcGraph, Streaming,
    SubInput,
};
use re_types::graph::GraphDoc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use support::graph;

#[derive(Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
struct Count {
    /// Milliseconds, rows, samples or a key byte, by kind.
    n: u32,
}

impl Params for Count {}

const POINTS_IN: &[PortDef] = &[PortDef {
    name: "points",
    accepts: &[PortType::Points],
    required: true,
    doc: "Points.",
}];

const FIELD_IN: &[PortDef] = &[PortDef {
    name: "field",
    accepts: &[PortType::Field],
    required: true,
    doc: "A field.",
}];

fn kind(
    kind: &'static str,
    inputs: &'static [PortDef],
    output: PortType,
    eval: impl Fn(&NodeCtx, &Count) -> Result<Output, NodeError> + Send + Sync + 'static,
) -> NodeDef {
    NodeDef::new(
        NodeSpec {
            kind,
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: Streaming::Whole,
            doc: "A test kind.",
            inputs,
            variadic: None,
            output,
            uses_seed: false,
            file_params: &[],
        },
        eval,
    )
}

fn one_point(id: u32) -> Output {
    Output::Points(Arc::new(Points {
        points: vec![Point {
            id,
            position: Vec3::ZERO,
            normal: Vec3::Y,
            tag: String::new(),
        }],
    }))
}

fn point_id(e: &Evaluation, node: &str) -> Option<u32> {
    match e.output(node)? {
        Output::Points(p) => p.points.first().map(|p| p.id),
        _ => None,
    }
}

/// Set by `test.signal`, which `test.wait` waits for.
static SIGNAL: AtomicBool = AtomicBool::new(false);

/// What `test.nested` saw of the graph it evaluated: its key byte, and the
/// inner node's key and where its output came from. Tests run at once, so
/// only `a_node_evaluates_a_graph_with_its_inputs_bound` uses key bytes 7
/// and 8, and reads only those rows.
static NESTED: Mutex<Vec<(u32, [u8; 32], CacheState)>> = Mutex::new(Vec::new());

/// The graph `test.nested` evaluates: its input `field` doubled.
fn doubling() -> ProcGraph {
    let version = NodeRegistry::m1().get("field.remap").unwrap().version;
    let mut nodes = IndexMap::new();
    nodes.insert(
        "double".to_string(),
        Node {
            kind: "field.remap".into(),
            version,
            params: json!({ "mode": "linear", "in_min": 0.0, "in_max": 1.0, "out_min": 0.0, "out_max": 2.0 }),
        },
    );
    ProcGraph {
        id: "doubling".into(),
        seed: 0,
        nodes,
        edges: vec![Edge {
            from: ("$in".into(), "field".into()),
            to: ("double".into(), "field".into()),
        }],
        layout: Default::default(),
    }
}

/// The graph `test.nested2` evaluates: one `test.nested` on its input
/// `field`, which evaluates `doubling()` in turn.
fn nesting(n: u32) -> ProcGraph {
    let mut nodes = IndexMap::new();
    nodes.insert(
        "inner".to_string(),
        Node {
            kind: "test.nested".into(),
            version: 1,
            params: json!({ "n": n }),
        },
    );
    ProcGraph {
        id: "nesting".into(),
        seed: 0,
        nodes,
        edges: vec![Edge {
            from: ("$in".into(), "field".into()),
            to: ("inner".into(), "field".into()),
        }],
        layout: Default::default(),
    }
}

/// A graph of one `test.spin` that runs for `ms` milliseconds.
fn spinning(ms: u32) -> ProcGraph {
    let mut nodes = IndexMap::new();
    nodes.insert(
        "spin".to_string(),
        Node {
            kind: "test.spin".into(),
            version: 1,
            params: json!({ "n": ms }),
        },
    );
    ProcGraph {
        id: "spinning".into(),
        seed: 0,
        nodes,
        edges: Vec::new(),
        layout: Default::default(),
    }
}

fn registry() -> Arc<NodeRegistry> {
    let mut r = NodeRegistry::m1();
    r.register(kind("test.points", &[], PortType::Points, |_, p| {
        Ok(one_point(p.n))
    }));
    r.register(kind("test.pass", POINTS_IN, PortType::Points, |ctx, _| {
        Ok(Output::Points(ctx.need_points("points")?.clone()))
    }));
    // Waits up to 10 s for `test.signal`: point 1 if it came, 0 if not.
    r.register(kind("test.wait", &[], PortType::Points, |_, _| {
        let start = Instant::now();
        while !SIGNAL.load(Ordering::SeqCst) {
            if start.elapsed() > Duration::from_secs(10) {
                return Ok(one_point(0));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(one_point(1))
    }));
    r.register(kind(
        "test.signal",
        POINTS_IN,
        PortType::Points,
        |ctx, _| {
            SIGNAL.store(true, Ordering::SeqCst);
            Ok(Output::Points(ctx.need_points("points")?.clone()))
        },
    ));
    // `n` steps of a millisecond, checking the stop after each.
    r.register(kind("test.spin", &[], PortType::Points, |ctx, p| {
        for _ in 0..p.n {
            std::thread::sleep(Duration::from_millis(1));
            ctx.check()?;
        }
        Ok(one_point(p.n))
    }));
    // `n` rows of 4,096 values through `par_rows`, a millisecond each.
    r.register(kind("test.rows", &[], PortType::Points, |ctx, p| {
        let mut values = vec![0.0f32; 4096 * p.n as usize];
        ctx.par_rows(&mut values, 4096, |z, row| {
            std::thread::sleep(Duration::from_millis(1));
            row.fill(z as f32);
        });
        Ok(one_point(p.n))
    }));
    // An `n` × `n` field sloping down along x, so droplets run their whole lives.
    r.register(kind("test.ramp", &[], PortType::Field, |_, p| {
        let n = p.n as usize;
        let values = (0..n * n).map(|i| (n - i % n) as f32 * 0.5).collect();
        Ok(Output::Field(Arc::new(Field::new(p.n, 1.0, values)?)))
    }));
    // Its input doubled, by evaluating `doubling()` with the input on
    // `$in.field` under the key `[n; 32]`.
    r.register(kind("test.nested", FIELD_IN, PortType::Field, |ctx, p| {
        let field = ctx.need_field("field")?;
        let key = [p.n as u8; 32];
        let sub = ctx.evaluate(
            &doubling(),
            ctx.seed,
            &[SubInput {
                name: "field".into(),
                output: Output::Field(field.clone()),
                key,
                cache_key: key,
            }],
        );
        let row = sub.report.node("double").expect("a row per node");
        NESTED
            .lock()
            .unwrap()
            .push((p.n, sub.key("double").unwrap_or_default(), row.cache));
        sub.output("double").cloned().ok_or_else(|| {
            NodeError::from(
                sub.report
                    .first_error()
                    .map_or("no output".into(), |d| d.message.clone()),
            )
        })
    }));
    // Its input doubled two graphs down: it evaluates `nesting(n)`, whose
    // `test.nested` evaluates `doubling()`.
    r.register(kind("test.nested2", FIELD_IN, PortType::Field, |ctx, p| {
        let field = ctx.need_field("field")?;
        let key = [p.n as u8; 32];
        let sub = ctx.evaluate(
            &nesting(p.n),
            ctx.seed,
            &[SubInput {
                name: "field".into(),
                output: Output::Field(field.clone()),
                key,
                cache_key: key,
            }],
        );
        sub.output("inner").cloned().ok_or_else(|| {
            NodeError::from(
                sub.report
                    .first_error()
                    .map_or("no output".into(), |d| d.message.clone()),
            )
        })
    }));
    // Evaluates `spinning(n)`, which stops with this node's evaluation.
    r.register(kind("test.nested_spin", &[], PortType::Points, |ctx, p| {
        let sub = ctx.evaluate(&spinning(p.n), ctx.seed, &[]);
        sub.output("spin")
            .cloned()
            .ok_or_else(|| NodeError::from("the spin did not finish".to_string()))
    }));
    Arc::new(r)
}

fn evaluator(threads: usize) -> Evaluator {
    Evaluator::new(registry()).with_pool(Arc::new(re_core::TaskPool::new(threads).expect("pool")))
}

fn compile(doc: &GraphDoc) -> ProcGraph {
    ProcGraph::from_doc(doc, &registry()).expect("graph is valid")
}

fn with_deadline(ms: u64) -> EvalOptions {
    EvalOptions {
        deadline: Some(Instant::now() + Duration::from_millis(ms)),
        ..Default::default()
    }
}

/// Evaluates on another thread and fails, rather than hanging, if that takes
/// a minute.
fn evaluate_within_a_minute(ev: Evaluator, g: ProcGraph, opts: EvalOptions) -> Evaluation {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(ev.evaluate(&g, &opts));
    });
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the evaluation finishes: no deadlock")
}

/// `test.wait` waits for `test.signal`, which runs after a fast node. With a
/// barrier between levels of the graph, the signal would run only after the
/// wait gave up.
#[test]
fn a_slow_node_holds_up_only_the_nodes_that_read_it() {
    let g = compile(&graph(
        1,
        &[
            ("slow", "test.wait", json!({})),
            ("fast", "test.points", json!({ "n": 5 })),
            ("signal", "test.signal", json!({})),
        ],
        &[("fast", "points", "signal", "points")],
    ));
    let e = evaluator(2).evaluate(&g, &EvalOptions::default());
    assert!(e.ok(), "{:#?}", e.report);
    assert_eq!(
        point_id(&e, "slow"),
        Some(1),
        "the signal ran during the wait"
    );
    assert_eq!(point_id(&e, "signal"), Some(5));
}

#[test]
fn a_deadline_stops_a_node_while_it_runs_and_nothing_it_returns_is_kept() {
    let g = compile(&graph(
        1,
        &[
            ("spin", "test.spin", json!({ "n": 600 })),
            ("after", "test.pass", json!({})),
        ],
        &[("spin", "points", "after", "points")],
    ));
    for serial in [false, true] {
        let ev = evaluator(2);
        let start = Instant::now();
        let e = ev.evaluate(
            &g,
            &EvalOptions {
                serial,
                ..with_deadline(100)
            },
        );
        let took = start.elapsed();
        assert!(
            took < Duration::from_millis(450),
            "{took:?} (serial: {serial})"
        );
        let d = e.report.first_error().expect("stopped");
        assert_eq!(d.code.to_string(), "RE4016");
        assert!(d.message.contains("in node `spin`"), "{}", d.message);
        for id in ["spin", "after"] {
            assert_eq!(
                e.report.node(id).unwrap().cache,
                CacheState::Skipped,
                "{id}"
            );
            assert!(e.output(id).is_none(), "{id}");
        }
        // Nothing was cached: without a deadline the node runs again, to the end.
        let e = ev.evaluate(&g, &EvalOptions::default());
        assert!(e.ok(), "{:#?}", e.report);
        assert_eq!(e.report.node("spin").unwrap().cache, CacheState::Miss);
        assert_eq!(point_id(&e, "after"), Some(600));
    }
}

#[test]
fn a_cancel_stops_a_node_while_it_runs() {
    let g = compile(&graph(
        1,
        &[("spin", "test.spin", json!({ "n": 600 }))],
        &[],
    ));
    for serial in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        let e = evaluator(2).evaluate(
            &g,
            &EvalOptions {
                cancel: Some(cancel),
                serial,
                ..Default::default()
            },
        );
        let took = start.elapsed();
        canceller.join().unwrap();
        assert!(
            took < Duration::from_millis(450),
            "{took:?} (serial: {serial})"
        );
        assert_eq!(e.report.first_error().unwrap().code.to_string(), "RE4310");
        assert_eq!(e.report.node("spin").unwrap().cache, CacheState::Skipped);
    }
}

#[test]
fn par_rows_stops_with_the_evaluation() {
    let g = compile(&graph(
        1,
        &[("rows", "test.rows", json!({ "n": 1000 }))],
        &[],
    ));
    for serial in [false, true] {
        let start = Instant::now();
        let e = evaluator(2).evaluate(
            &g,
            &EvalOptions {
                serial,
                ..with_deadline(50)
            },
        );
        let took = start.elapsed();
        assert!(
            took < Duration::from_millis(400),
            "{took:?} (serial: {serial})"
        );
        let d = e.report.first_error().expect("stopped");
        assert!(d.message.contains("in node `rows`"), "{}", d.message);
        assert_eq!(e.report.node("rows").unwrap().cache, CacheState::Skipped);
    }
}

/// A whole field erodes in batches of droplets, checking the deadline
/// between them: 1025² is half a million droplets, seconds of work.
#[test]
fn whole_field_erosion_stops_between_batches_of_droplets() {
    let g = compile(&graph(
        1,
        &[
            ("ramp", "test.ramp", json!({ "n": 1025 })),
            ("erode", "terrain.erode", json!({})),
        ],
        &[("ramp", "field", "erode", "field")],
    ));
    let start = Instant::now();
    let e = evaluator(2).evaluate(&g, &with_deadline(300));
    let took = start.elapsed();
    let d = e.report.first_error().expect("stopped");
    assert_eq!(d.code.to_string(), "RE4016");
    assert!(d.message.contains("in node `erode`"), "{}", d.message);
    assert!(took < Duration::from_secs(3), "{took:?}");
    assert_eq!(e.report.node("ramp").unwrap().cache, CacheState::Miss);
    assert_eq!(e.report.node("erode").unwrap().cache, CacheState::Skipped);
}

/// A node evaluates a graph with its input on `["$in", "field"]`: the inner
/// node's key follows the key given with the input, so two nodes giving the
/// same input and key share the inner output through the memory cache.
#[test]
fn a_node_evaluates_a_graph_with_its_inputs_bound() {
    let ramp = json!({ "n": 9 });
    let g = compile(&graph(
        1,
        &[
            ("ramp", "test.ramp", ramp),
            ("a", "test.nested", json!({ "n": 7 })),
            ("b", "test.nested", json!({ "n": 7 })),
            ("c", "test.nested", json!({ "n": 8 })),
        ],
        &[
            ("ramp", "field", "a", "field"),
            ("ramp", "field", "b", "field"),
            ("ramp", "field", "c", "field"),
        ],
    ));
    NESTED.lock().unwrap().retain(|row| !matches!(row.0, 7 | 8));
    // One thread: `a`, `b` and `c` run in document order.
    let e = evaluator(1).evaluate(&g, &EvalOptions::default());
    assert!(e.ok(), "{:#?}", e.report);
    let (Some(Output::Field(input)), Some(Output::Field(doubled))) =
        (e.output("ramp"), e.output("a"))
    else {
        panic!("fields");
    };
    let twice: Vec<f32> = input.0.samples.iter().map(|v| v * 2.0).collect();
    assert_eq!(doubled.0.samples, twice);
    let seen: Vec<_> = NESTED
        .lock()
        .unwrap()
        .iter()
        .copied()
        .filter(|row| matches!(row.0, 7 | 8))
        .collect();
    assert_eq!(seen.len(), 3);
    let (a, b, c) = (seen[0], seen[1], seen[2]);
    assert_eq!((a.0, b.0, c.0), (7, 7, 8));
    assert_eq!(a.1, b.1, "same input key, same inner key");
    assert_ne!(a.1, c.1, "another input key, another inner key");
    assert_eq!(a.2, CacheState::Miss);
    assert_eq!(b.2, CacheState::Memory, "shared through the memory cache");
    assert_eq!(c.2, CacheState::Miss);
}

/// Sixteen nodes each evaluating a graph on two workers: a worker waiting
/// for its nested nodes runs other jobs meanwhile, so nothing deadlocks.
#[test]
fn nested_evaluations_on_the_pool_do_not_deadlock() {
    let mut nodes = vec![("ramp".to_string(), "test.ramp", json!({ "n": 33 }))];
    let mut edges = Vec::new();
    for i in 0..16 {
        let id = format!("n{i}");
        edges.push(("ramp".to_string(), id.clone()));
        nodes.push((id, "test.nested", json!({ "n": 16 + i })));
    }
    let nodes: Vec<(&str, &str, serde_json::Value)> = nodes
        .iter()
        .map(|(id, kind, p)| (id.as_str(), *kind, p.clone()))
        .collect();
    let edges: Vec<(&str, &str, &str, &str)> = edges
        .iter()
        .map(|(from, to)| (from.as_str(), "field", to.as_str(), "field"))
        .collect();
    let g = compile(&graph(1, &nodes, &edges));
    let e = evaluate_within_a_minute(evaluator(2), g, EvalOptions::default());
    assert!(e.ok(), "{:#?}", e.report);
    assert_eq!(e.report.misses, 17);
}

/// The ramp's field, doubled, as `node` returned it.
fn assert_doubled(e: &Evaluation, node: &str) {
    let (Some(Output::Field(input)), Some(Output::Field(doubled))) =
        (e.output("ramp"), e.output(node))
    else {
        panic!("fields from `ramp` and `{node}`");
    };
    let twice: Vec<f32> = input.0.samples.iter().map(|v| v * 2.0).collect();
    assert_eq!(doubled.0.samples, twice, "{node}");
}

/// A node evaluating a graph whose node evaluates another graph, on one
/// worker: each evaluation runs its nodes on the calling thread, in order,
/// so nothing waits for a worker.
#[test]
fn graphs_nested_two_deep_evaluate_on_one_worker() {
    let g = compile(&graph(
        1,
        &[
            ("ramp", "test.ramp", json!({ "n": 17 })),
            ("outer", "test.nested2", json!({ "n": 40 })),
        ],
        &[("ramp", "field", "outer", "field")],
    ));
    let e = evaluate_within_a_minute(evaluator(1), g, EvalOptions::default());
    assert!(e.ok(), "{:#?}", e.report);
    assert_doubled(&e, "outer");
}

/// Two such nodes at once on two workers: a worker waiting for the nodes of
/// a graph it evaluates runs them, or other jobs, meanwhile, so nothing
/// deadlocks.
#[test]
fn two_graphs_nested_two_deep_on_two_workers_do_not_deadlock() {
    let g = compile(&graph(
        1,
        &[
            ("ramp", "test.ramp", json!({ "n": 17 })),
            ("one", "test.nested2", json!({ "n": 41 })),
            ("two", "test.nested2", json!({ "n": 42 })),
        ],
        &[
            ("ramp", "field", "one", "field"),
            ("ramp", "field", "two", "field"),
        ],
    ));
    for _ in 0..8 {
        let e = evaluate_within_a_minute(evaluator(2), g.clone(), EvalOptions::default());
        assert!(e.ok(), "{:#?}", e.report);
        assert_doubled(&e, "one");
        assert_doubled(&e, "two");
    }
}

/// A graph a node evaluates stops with the node's evaluation, and the node
/// is stopped with it: nothing either returns is kept.
#[test]
fn a_nested_evaluation_stops_with_its_node() {
    let g = compile(&graph(
        1,
        &[("outer", "test.nested_spin", json!({ "n": 600 }))],
        &[],
    ));
    let start = Instant::now();
    let e = evaluator(2).evaluate(&g, &with_deadline(100));
    assert!(start.elapsed() < Duration::from_millis(450));
    let d = e.report.first_error().expect("stopped");
    assert_eq!(d.code.to_string(), "RE4016");
    assert!(d.message.contains("in node `outer`"), "{}", d.message);
    assert_eq!(e.report.node("outer").unwrap().cache, CacheState::Skipped);
}
