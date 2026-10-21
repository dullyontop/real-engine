//! Luau node kinds (ADR-0043 §4): declarations, evaluation, the twin of a
//! Rust node, the deterministic budget, output checks and migrations.

use std::path::Path;
use std::sync::Arc;

use re_core::TaskPool;
use re_proc::{
    CustomKind, CustomPort, EvalOptions, Evaluator, Field, NodeRegistry, Output, PortType,
    ProcGraph, Streaming, codes,
};
use re_script::generator::{Declared, Limits, declare};
use re_types::graph::GraphDoc;
use serde_json::{Value as Json, json};

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, source).unwrap();
    }
    dir
}

fn graph(seed: i64, nodes: &[(&str, &str, Json)], edges: &[(&str, &str, &str, &str)]) -> GraphDoc {
    let doc = json!({
        "format": "real-engine/graph",
        "version": 1,
        "name": "test",
        "seed": seed,
        "nodes": nodes.iter().map(|(id, kind, params)| json!({
            "id": id, "kind": kind, "version": 1, "params": params,
        })).collect::<Vec<_>>(),
        "edges": edges.iter().map(|(f, fp, t, tp)| json!({
            "from": [f, fp], "to": [t, tp],
        })).collect::<Vec<_>>(),
    });
    GraphDoc::from_json(&doc.to_string()).unwrap()
}

fn declared(root: &Path, limits: Limits) -> (Declared, Arc<NodeRegistry>) {
    let d = declare(root, &NodeRegistry::m1(), limits);
    let r = Arc::new(d.registry(&NodeRegistry::m1()));
    (d, r)
}

fn evaluator(registry: Arc<NodeRegistry>, threads: usize) -> Evaluator {
    Evaluator::new(registry).with_pool(Arc::new(TaskPool::new(threads).unwrap()))
}

fn field_samples(o: &Output) -> Vec<u32> {
    match o {
        Output::Field(f) => f.0.samples.iter().map(|v| v.to_bits()).collect(),
        other => panic!("not a field: {:?}", other.port_type()),
    }
}

const SHAPE: &str = r#"
return proc.node {
    kind = "project.shape",
    version = 1,
    doc = "Shapes a field: v * v * k + bias.",
    inputs = { { name = "field", type = "field", required = true, doc = "The field." } },
    output = "field",
    params = {
        k = proc.param.number { default = 0.5, doc = "The factor." },
        bias = proc.param.number { default = 1 },
    },
    eval = function(ctx, inputs, params)
        local f = inputs.field
        local count = f.n * f.n
        local out = buffer.create(count * 4)
        for i = 0, count - 1 do
            local v = buffer.readf32(f.samples, i * 4)
            buffer.writef32(out, i * 4, v * v * params.k + params.bias)
        end
        ctx:log("shaped " .. count)
        return { n = f.n, spacing = f.spacing, offset = f.offset, samples = out }
    end,
}
"#;

/// The Rust twin of `project.shape`, in f64 as Luau computes.
fn rust_shape() -> re_proc::NodeDef {
    re_proc::NodeDef::custom(CustomKind {
        kind: "test.shape".into(),
        version: 1,
        oldest: 1,
        streaming: Streaming::Whole,
        implementation: [7; 32],
        doc: "The twin.".into(),
        inputs: vec![CustomPort {
            name: "field".into(),
            accepts: vec![PortType::Field],
            required: true,
            doc: String::new(),
        }],
        output: PortType::Field,
        uses_seed: false,
        params_schema: json!({ "type": "object" }),
        check: Arc::new(|_| Ok(())),
        eval: Arc::new(|ctx, _params| {
            let Output::Field(f) = &ctx.inputs[0].1 else {
                unreachable!()
            };
            let h = &f.0;
            let samples = h
                .samples
                .iter()
                .map(|&v| {
                    let v = f64::from(v);
                    (v * v * 0.5 + 1.0) as f32
                })
                .collect();
            let mut out = h.clone();
            out.samples = samples;
            Ok(Output::Field(Arc::new(Field(out))))
        }),
        migrate: None,
    })
}

fn terrain() -> (&'static str, &'static str, Json) {
    (
        "h",
        "terrain.heightfield",
        json!({ "size": 33, "cell": 2.0, "height": 10.0, "scale": 0.03 }),
    )
}

#[test]
fn a_luau_node_matches_its_rust_twin_bit_for_bit() {
    let dir = project(&[("scripts/generators/shape.luau", SHAPE)]);
    let (d, registry) = declared(dir.path(), Limits::default());
    assert!(d.diagnostics.is_empty(), "{:#?}", d.diagnostics);
    assert_eq!(d.kinds.len(), 1);
    let mut registry = (*registry).clone();
    registry.register(rust_shape());
    let registry = Arc::new(registry);
    let doc = graph(
        3,
        &[
            terrain(),
            ("luau", "project.shape", json!({})),
            ("rust", "test.shape", json!({})),
        ],
        &[("h", "out", "luau", "field"), ("h", "out", "rust", "field")],
    );
    let g = ProcGraph::from_doc(&doc, &registry).expect("valid graph");
    let opts = EvalOptions {
        root: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let one = evaluator(registry.clone(), 1).evaluate(&g, &opts);
    assert!(one.ok(), "{:#?}", one.report.diagnostics);
    let luau = field_samples(one.output("luau").unwrap());
    assert_eq!(luau, field_samples(one.output("rust").unwrap()));
    assert_eq!(one.report.node("luau").unwrap().logs, ["shaped 1089"]);

    // Same output on four threads.
    let four = evaluator(registry, 4).evaluate(&g, &opts);
    assert_eq!(luau, field_samples(four.output("luau").unwrap()));
}

const SCATTER: &str = r#"
local util = require("lib/util")
return proc.node {
    kind = "project.scatter",
    version = 2,
    doc = "Places named boxes at random.",
    output = "fragment",
    uses_seed = true,
    params = { count = proc.param.integer { default = 3, min = 0, max = 50 } },
    migrate = function(from, params)
        if from == 1 then
            params.count = params.n
            params.n = nil
        end
        return params
    end,
    eval = function(ctx, inputs, params)
        local items = {}
        for i = 1, params.count do
            local x = ctx.rng:int(-10, 10)
            items[i] = { key = "box" .. i, name = util.label(i, x) }
        end
        print("placed", params.count)
        return { items = items }
    end,
}
"#;

const UTIL: &str = r#"
local M = {}
function M.label(i, x)
    return "Box " .. i .. " at " .. x
end
return M
"#;

fn names(o: &Output) -> Vec<String> {
    match o {
        Output::Fragment(f) => f.items.iter().map(|i| i.name.clone()).collect(),
        other => panic!("not a fragment: {:?}", other.port_type()),
    }
}

#[test]
fn a_generator_uses_its_seed_modules_and_migrations() {
    let dir = project(&[
        ("scripts/generators/scatter.luau", SCATTER),
        ("scripts/generators/lib/util.luau", UTIL),
    ]);
    let (d, registry) = declared(dir.path(), Limits::default());
    assert!(d.diagnostics.is_empty(), "{:#?}", d.diagnostics);
    // Only the file directly in scripts/generators/ declares a kind.
    assert_eq!(d.kinds.len(), 1);
    let run = |seed: i64, version: u32, params: Json| {
        let mut doc = graph(seed, &[("s", "project.scatter", params)], &[]);
        doc.nodes[0].version = version;
        let g = ProcGraph::from_doc(&doc, &registry).expect("valid graph");
        let e = evaluator(registry.clone(), 2).evaluate(&g, &EvalOptions::default());
        assert!(e.ok(), "{:#?}", e.report.diagnostics);
        (
            names(e.output("s").unwrap()),
            e.report.node("s").unwrap().logs.clone(),
        )
    };
    let (a, logs) = run(1, 2, json!({ "count": 4 }));
    assert_eq!(a.len(), 4);
    assert!(a[0].starts_with("Box 1 at "), "{a:?}");
    assert_eq!(logs, ["placed\t4"]);
    assert_eq!(
        a,
        run(1, 2, json!({ "count": 4 })).0,
        "same seed, same boxes"
    );
    assert_ne!(a, run(2, 2, json!({ "count": 4 })).0, "another seed");
    // A version 1 node is migrated: `n` became `count`.
    assert_eq!(run(1, 1, json!({ "n": 2 })).0.len(), 2);
}

fn first_error(root: &Path, limits: Limits) -> re_core::Diagnostic {
    let (d, registry) = declared(root, limits);
    assert!(d.diagnostics.is_empty(), "{:#?}", d.diagnostics);
    let g = ProcGraph::from_doc(
        &graph(1, &[("n", "project.bad", json!({}))], &[]),
        &registry,
    )
    .expect("valid graph");
    let e = evaluator(registry, 1).evaluate(&g, &EvalOptions::default());
    e.report.first_error().cloned().expect("an error")
}

fn bad(eval_body: &str, output: &str) -> String {
    format!(
        "return proc.node {{ kind = \"project.bad\", version = 1, doc = \"\", output = \"{output}\",
        eval = function(ctx, inputs, params)
{eval_body}
        end }}"
    )
}

#[test]
fn the_budget_stops_a_loop_at_the_same_place_and_pcall_cannot_catch_it() {
    let body = "local n = 0
        while true do
            pcall(function() while true do n += 1 end end)
            n += 1
        end";
    let src = bad(body, "field");
    let dir = project(&[("scripts/generators/bad.luau", &src)]);
    let limits = Limits {
        budget: 50_000,
        memory_mb: 64,
    };
    let a = first_error(dir.path(), limits);
    assert_eq!(a.code, codes::LUAU_BUDGET, "{a:#?}");
    assert!(a.message.contains("50000"), "{}", a.message);
    let b = first_error(dir.path(), limits);
    assert_eq!(a.message, b.message);
}

#[test]
fn memory_past_the_limit_is_a_budget_error() {
    let body = "local t = {}
        for i = 1, 1e9 do t[i] = buffer.create(1024 * 1024) end";
    let src = bad(body, "field");
    let dir = project(&[("scripts/generators/bad.luau", &src)]);
    let e = first_error(
        dir.path(),
        Limits {
            budget: 1_000_000_000,
            memory_mb: 16,
        },
    );
    assert_eq!(e.code, codes::LUAU_BUDGET, "{e:#?}");
    assert!(e.message.contains("memory"), "{}", e.message);
}

#[test]
fn invalid_outputs_are_refused() {
    let cases = [
        ("return 3", "field"),
        (
            "return { n = 4, spacing = 1, samples = buffer.create(8) }",
            "field",
        ),
        ("return { count = 1, data = buffer.create(3) }", "points"),
        (
            "local i = buffer.create(12) buffer.writeu32(i, 0, 5) return { positions = buffer.create(12), indices = i }",
            "mesh",
        ),
        ("return { items = { { key = \"/a\" } } }", "fragment"),
        (
            "return { items = { { key = \"a\" }, { key = \"a\" } } }",
            "fragment",
        ),
    ];
    for (body, output) in cases {
        let src = bad(body, output);
        let dir = project(&[("scripts/generators/bad.luau", &src)]);
        let (_, registry) = declared(dir.path(), Limits::default());
        let g = ProcGraph::from_doc(
            &graph(1, &[("n", "project.bad", json!({}))], &[]),
            &registry,
        )
        .unwrap();
        let e = evaluator(registry, 1).evaluate(&g, &EvalOptions::default());
        let e = e
            .report
            .first_error()
            .cloned()
            .unwrap_or_else(|| panic!("{body}: no error"));
        assert_eq!(e.code, codes::LUAU_OUTPUT, "{body}: {e:#?}");
    }
}

#[test]
fn a_generator_has_no_clock_files_or_global_randomness() {
    for body in [
        "return os.time()",
        "return math.random()",
        "return require(\"../../scripts/other\")",
        "return loadstring(\"return 1\")",
    ] {
        let src = bad(body, "field");
        let dir = project(&[
            ("scripts/generators/bad.luau", &src),
            ("scripts/other.luau", "return 1"),
        ]);
        let e = first_error(dir.path(), Limits::default());
        assert_eq!(e.code, codes::NODE_FAILED, "{body}: {e:#?}");
    }
}

#[test]
fn bad_declarations_are_reported_and_the_rest_declared() {
    let dir = project(&[
        ("scripts/generators/good.luau", SHAPE),
        (
            "scripts/generators/engine.luau",
            &SHAPE.replace("project.shape", "field.remap"),
        ),
        (
            "scripts/generators/prefix.luau",
            &SHAPE.replace("project.shape", "shape"),
        ),
        ("scripts/generators/twice.luau", SHAPE),
        ("scripts/generators/syntax.luau", "return proc.node {"),
        (
            "scripts/generators/plain.luau",
            "return { kind = \"project.x\" }",
        ),
        (
            "scripts/generators/param.luau",
            &SHAPE.replace(
                "proc.param.number { default = 0.5, doc = \"The factor.\" }",
                "proc.param.integer { default = 0.5 }",
            ),
        ),
    ]);
    let (d, _) = declared(dir.path(), Limits::default());
    let kinds: Vec<&str> = d.kinds.iter().map(|k| k.kind).collect();
    assert_eq!(kinds, ["project.shape"], "{:#?}", d.diagnostics);
    assert_eq!(d.diagnostics.len(), 6, "{:#?}", d.diagnostics);
    assert!(
        d.diagnostics
            .iter()
            .all(|x| x.code == codes::LUAU_DECLARATION)
    );
    let text: Vec<&str> = d.diagnostics.iter().map(|x| x.message.as_str()).collect();
    assert!(
        text.iter().any(|m| m.contains("already declares")),
        "{text:#?}"
    );
    assert!(
        text.iter()
            .any(|m| m.contains("must start with `project.`")),
        "{text:#?}"
    );
    assert!(text.iter().any(|m| m.contains("proc.node")), "{text:#?}");
    assert!(
        text.iter().any(|m| m.contains("expected an integer")),
        "{text:#?}"
    );
}

#[test]
fn editing_any_generator_script_changes_the_implementation_hash() {
    let dir = project(&[
        ("scripts/generators/shape.luau", SHAPE),
        ("scripts/generators/lib/util.luau", UTIL),
    ]);
    let a = declare(dir.path(), &NodeRegistry::m1(), Limits::default()).implementation;
    std::fs::write(
        dir.path().join("scripts/generators/lib/util.luau"),
        UTIL.replace("Box", "Crate"),
    )
    .unwrap();
    let b = declare(dir.path(), &NodeRegistry::m1(), Limits::default()).implementation;
    assert_ne!(a, b);
    let none = tempfile::tempdir().unwrap();
    let d = declare(none.path(), &NodeRegistry::m1(), Limits::default());
    assert!(d.kinds.is_empty() && d.diagnostics.is_empty());
}

mod source {
    use super::*;
    use re_command::{AllowAll, CommandOutcome, CommandRegistry, EditState};
    use re_proc::{Evaluator, GraphStore, register_commands, register_generator};

    fn call(st: &mut EditState, name: &str, args: Json) -> CommandOutcome {
        let mut r = CommandRegistry::new();
        register_commands(&mut r);
        r.dispatch(st, name, args, false, &AllowAll)
    }

    fn children(st: &mut EditState, source: &str) -> Vec<String> {
        let found: Vec<(re_scene::Entity, String)> = st
            .world
            .ecs
            .query::<(&re_scene::Generated, &re_scene::Name)>()
            .iter(&st.world.ecs)
            .map(|(g, n)| (g.source, n.0.clone()))
            .collect();
        let mut names: Vec<String> = found
            .into_iter()
            .filter(|(e, _)| st.world.id_of(*e).map(|i| i.to_string()).as_deref() == Some(source))
            .map(|(_, n)| n)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_luau_generator_becomes_a_source_with_create_source() {
        let dir = project(&[
            ("scripts/generators/scatter.luau", SCATTER),
            ("scripts/generators/lib/util.luau", UTIL),
            ("scripts/generators/shape.luau", SHAPE),
        ]);
        let (d, registry) = declared(dir.path(), Limits::default());
        assert!(d.diagnostics.is_empty(), "{:#?}", d.diagnostics);
        let mut st = EditState::new();
        register_generator(&mut st.world.generators);
        let store = GraphStore::with_evaluator(
            Arc::new(Evaluator::new(registry)),
            Some(dir.path().to_path_buf()),
        );
        st.world.ecs.insert_resource(store);

        // Kinds that are not fragments, or need inputs, are refused.
        let o = call(
            &mut st,
            "proc.create_source",
            json!({ "kind": "project.shape" }),
        );
        assert!(!o.ok);
        assert_eq!(o.diagnostics[0].code, codes::CREATE_SOURCE, "{o:#?}");
        let o = call(
            &mut st,
            "proc.create_source",
            json!({ "kind": "project.nope" }),
        );
        assert!(
            o.diagnostics[0].message.contains("project.scatter"),
            "{o:#?}"
        );
        let o = call(
            &mut st,
            "proc.create_source",
            json!({ "kind": "project.scatter", "params": { "count": 99 } }),
        );
        assert_eq!(o.diagnostics[0].code, codes::INVALID_PARAMS, "{o:#?}");

        let o = call(
            &mut st,
            "proc.create_source",
            json!({ "kind": "project.scatter", "id": "rocks", "seed": 5, "params": { "count": 2 } }),
        );
        assert!(o.ok, "{o:#?}");
        assert_eq!(o.data["graph"], json!("graphs/rocks.graph.json"));
        let file = std::fs::read_to_string(dir.path().join("graphs/rocks.graph.json")).unwrap();
        let doc = GraphDoc::from_json(&file).unwrap();
        assert_eq!(doc.nodes[0].kind, "project.scatter");
        assert_eq!(doc.nodes[0].version, 2);
        let report = st.world.regenerate_changed();
        assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
        assert_eq!(children(&mut st, "rocks").len(), 2);

        // Parameters are graph parameters.
        let o = call(
            &mut st,
            "graph.set_param",
            json!({ "source": "rocks", "node": "gen", "param": "count", "value": 5 }),
        );
        assert!(o.ok, "{o:#?}");
        st.world.regenerate_changed();
        assert_eq!(children(&mut st, "rocks").len(), 5);

        // A second source of the same kind gets its own file.
        let o = call(
            &mut st,
            "proc.create_source",
            json!({ "kind": "project.scatter" }),
        );
        assert!(o.ok, "{o:#?}");
        assert_eq!(o.data["graph"], json!("graphs/scatter.graph.json"));
        let o = call(
            &mut st,
            "proc.create_source",
            json!({ "kind": "project.scatter", "graph": "graphs/rocks.graph.json" }),
        );
        assert!(
            o.diagnostics[0].message.contains("already exists"),
            "{o:#?}"
        );
        let o = call(
            &mut st,
            "proc.create_source",
            json!({ "kind": "project.scatter", "graph": "../out.graph.json" }),
        );
        assert_eq!(o.diagnostics[0].code, codes::CREATE_SOURCE, "{o:#?}");
    }
}
