//! Roads and shapes in a streamed world (ADR-0042, ADR-0043 §5): global
//! nodes run once for every cell and chunk, draped on the field sampled on
//! the source's grid; road fields agree wherever windows meet, at every
//! level of detail; cells share out road and shape meshes; and graphs whose
//! windows could see different roads are refused (`RE4301`).

mod support;

use std::sync::Arc;

use glam::Vec3;
use re_command::EditState;
use re_proc::stream::{StreamConfig, Streamer, TerrainView, step};
use re_proc::{
    CacheState, Domain, EvalOptions, Evaluation, Evaluator, Field, GraphStore, MeshValue, NodeCtx,
    NodeDef, NodeRegistry, NodeSpec, Output, Params, PortDef, PortType, ProcGraph, RoadPath,
    Streaming, register_generator,
};
use re_reflect::Value;
use re_scene::World;
use re_types::graph::GraphDoc;
use re_types::mesh::MeshData;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use support::{evaluator, fixtures, graph, output_hash};

/// Metres between the source's samples, and samples per 64 m cell.
const SPACING: f32 = 4.0;
const STEPS: u32 = 16;

/// The road's control points.
const ROAD: [[f64; 3]; 3] = [[-90.0, 0.4, -30.0], [0.0, 0.6, 10.0], [90.0, 0.4, 40.0]];

/// Cell `(x, z)`'s window.
fn cell(x: i64, z: i64) -> Domain {
    let s = i64::from(STEPS);
    Domain {
        origin: [x * s, z * s],
        n: STEPS + 1,
        spacing: SPACING,
    }
}

/// A streamed evaluation of `domain` whose global nodes sample their fields
/// at `global` metres.
fn streamed(domain: Domain, global: Option<f32>) -> EvalOptions {
    EvalOptions {
        seed: Some(7),
        domain: Some(domain),
        global_spacing: global,
        serial: true,
        ..Default::default()
    }
}

/// An eroded valley (light erosion, so debug tests stay quick) with a road
/// draped on it, the ground flattened under it, its surface, its footprint
/// and a mask that clears the ground beside it; with `chunks`, the ground is
/// world streaming's terrain.
fn valley(chunks: bool) -> GraphDoc {
    let mut nodes = vec![
        (
            "h",
            "terrain.heightfield",
            json!({ "cell": 4.0, "height": 40.0, "scale": 0.01 }),
        ),
        (
            "e",
            "terrain.erode",
            json!({ "spacing": 4.0, "droplets": 0.05, "steps": 16 }),
        ),
        (
            "road",
            "path.spline",
            json!({ "points": ROAD, "width": 8.0 }),
        ),
        (
            "flat",
            "terrain.conform",
            json!({ "shoulder": 2.0, "blend": 8.0 }),
        ),
        ("surface", "road.mesh", json!({})),
        ("print", "road.footprint", json!({ "margin": 1.0 })),
        (
            "clear",
            "field.from_shapes",
            json!({ "falloff": 4.0, "invert": true }),
        ),
    ];
    let mut edges = vec![
        ("h", "field", "e", "field"),
        ("e", "field", "road", "field"),
        ("e", "field", "flat", "field"),
        ("road", "path", "flat", "in1"),
        ("road", "path", "surface", "in1"),
        ("road", "path", "print", "in1"),
        ("print", "shapes", "clear", "shapes"),
        ("flat", "field", "clear", "field"),
    ];
    if chunks {
        nodes.push(("ground", "terrain.chunks", json!({ "max_level": 2 })));
        edges.push(("flat", "field", "ground", "field"));
    }
    graph(7, &nodes, &edges)
}

fn build(doc: &GraphDoc) -> ProcGraph {
    ProcGraph::from_doc(doc, &NodeRegistry::m1()).unwrap()
}

fn evaluate(ev: &Evaluator, g: &ProcGraph, opts: &EvalOptions) -> Evaluation {
    let e = ev.evaluate(g, opts);
    assert!(e.ok(), "{:#?}", e.report.nodes);
    e
}

fn field_of(e: &Evaluation, id: &str) -> Arc<Field> {
    match e.output(id) {
        Some(Output::Field(f)) => f.clone(),
        o => panic!("{id}: {o:?}"),
    }
}

fn path_of(e: &Evaluation, id: &str) -> Arc<RoadPath> {
    match e.output(id) {
        Some(Output::Path(p)) => p.clone(),
        o => panic!("{id}: {o:?}"),
    }
}

fn mesh_of(e: &Evaluation, id: &str) -> Arc<MeshValue> {
    match e.output(id) {
        Some(Output::Mesh(m)) => m.clone(),
        o => panic!("{id}: {o:?}"),
    }
}

fn key(e: &Evaluation, id: &str) -> String {
    e.report.node(id).unwrap().key.clone()
}

fn cache(e: &Evaluation, id: &str) -> CacheState {
    e.report.node(id).unwrap().cache
}

/// The surface area of a mesh.
fn area(m: &MeshData) -> f64 {
    let at = |i: u32| m.vertices[i as usize].pos.map(f64::from);
    m.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let (a, b, c) = (at(t[0]), at(t[1]), at(t[2]));
            let (u, v) = (
                [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
            );
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
        })
        .sum()
}

/// The XZ distance from `(x, z)` to the path's centreline.
fn distance_to(path: &RoadPath, x: f64, z: f64) -> f64 {
    path.samples
        .windows(2)
        .map(|w| {
            let (a, b) = (w[0].position, w[1].position);
            let (dx, dz) = (b[0] - a[0], b[2] - a[2]);
            let len2 = dx * dx + dz * dz;
            let t = if len2 > 0.0 {
                (((x - a[0]) * dx + (z - a[2]) * dz) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (ex, ez) = (a[0] + dx * t - x, a[2] + dz * t - z);
            (ex * ex + ez * ez).sqrt()
        })
        .fold(f64::INFINITY, f64::min)
}

#[test]
fn global_nodes_run_once_for_every_window() {
    let g = build(&valley(false));
    let ev = evaluator(2);
    let a = evaluate(&ev, &g, &streamed(cell(0, 0), Some(SPACING)));
    let b = evaluate(&ev, &g, &streamed(cell(-1, 0), Some(SPACING)));
    // The road and its footprint are one result for every cell.
    for id in ["road", "print"] {
        assert_eq!(key(&a, id), key(&b, id), "{id}");
        assert_eq!(cache(&a, id), CacheState::Miss, "{id}");
        assert_eq!(cache(&b, id), CacheState::Memory, "{id}");
    }
    // What is over the window has a key per window.
    for id in ["h", "e", "flat", "surface", "clear"] {
        assert_ne!(key(&a, id), key(&b, id), "{id}");
    }
    // A fresh evaluator, in another cell, makes the same road with the same
    // key.
    let c = evaluate(&evaluator(1), &g, &streamed(cell(3, -2), Some(SPACING)));
    assert_eq!(key(&c, "road"), key(&a, "road"));
    assert_eq!(cache(&c, "road"), CacheState::Miss);
    assert_eq!(
        output_hash(c.output("road").unwrap()),
        output_hash(a.output("road").unwrap())
    );
    // Not streamed, the road is keyed as it always was: no global key.
    let whole = ev.evaluate(&g, &EvalOptions::default());
    assert_ne!(key(&whole, "road"), key(&a, "road"));
}

#[test]
fn a_field_only_a_global_node_samples_is_not_run_over_the_window() {
    let doc = graph(
        7,
        &[
            (
                "h",
                "terrain.heightfield",
                json!({ "cell": 4.0, "height": 40.0, "scale": 0.01 }),
            ),
            (
                "e",
                "terrain.erode",
                json!({ "spacing": 4.0, "droplets": 0.05, "steps": 16 }),
            ),
            ("road", "path.spline", json!({ "points": ROAD })),
            ("surface", "road.mesh", json!({})),
        ],
        &[
            ("h", "field", "e", "field"),
            ("e", "field", "road", "field"),
            ("road", "path", "surface", "in1"),
        ],
    );
    let e = evaluate(
        &evaluator(1),
        &build(&doc),
        &streamed(cell(0, 0), Some(4.0)),
    );
    for id in ["h", "e"] {
        assert_eq!(cache(&e, id), CacheState::Skipped, "{id}");
        assert!(e.output(id).is_none(), "{id}");
    }
    assert!(!mesh_of(&e, "surface").mesh.indices.is_empty());
}

#[test]
fn global_paths_drape_on_the_field_sampled_on_the_source_grid() {
    let g = build(&valley(false));
    let ev = evaluator(2);
    let path = path_of(
        &evaluate(&ev, &g, &streamed(cell(0, 0), Some(SPACING))),
        "road",
    );
    // The eroded field around the road, at the source's spacing.
    let around = Domain {
        origin: [-32, -16],
        n: 65,
        spacing: SPACING,
    };
    let field = field_of(&evaluate(&ev, &g, &streamed(around, Some(SPACING))), "e");
    // The road starts and ends at its first and last control points, raised
    // by the ground under them.
    for (sample, point) in [
        (path.samples[0].position, ROAD[0]),
        (path.samples.last().unwrap().position, ROAD[2]),
    ] {
        let ground = field.0.height_at(point[0] as f32, point[2] as f32).unwrap();
        assert!((sample[0] - point[0]).abs() < 1e-9, "{sample:?} {point:?}");
        assert!((sample[2] - point[2]).abs() < 1e-9, "{sample:?} {point:?}");
        assert!(
            (sample[1] - (point[1] + f64::from(ground))).abs() < 1e-3,
            "{sample:?}: {point:?} on {ground}"
        );
    }
}

#[test]
fn level_of_detail_windows_see_the_cells_roads() {
    let g = build(&valley(false));
    let ev = evaluator(2);
    let fine = evaluate(&ev, &g, &streamed(cell(0, 0), Some(SPACING)));
    // A level-1 chunk's window (8 m, with an apron) over the same ground,
    // its roads sampled at the source's spacing.
    let level1 = Domain {
        origin: [-1, -1],
        n: 67,
        spacing: 8.0,
    };
    let coarse = evaluate(&ev, &g, &streamed(level1, Some(SPACING)));
    assert_eq!(key(&coarse, "road"), key(&fine, "road"));
    assert_eq!(cache(&coarse, "road"), CacheState::Memory);
    // The flattened ground agrees on every sample the windows share: fine
    // sample 2k is coarse sample k + 1.
    let (f, c) = (field_of(&fine, "flat"), field_of(&coarse, "flat"));
    for z in (0..=STEPS).step_by(2) {
        for x in (0..=STEPS).step_by(2) {
            assert_eq!(
                f.0.sample(x, z).to_bits(),
                c.0.sample(x / 2 + 1, z / 2 + 1).to_bits(),
                "({x}, {z})"
            );
        }
    }
    assert_ne!(f.0.samples, field_of(&fine, "e").0.samples, "flattened");
    // Sampled at the window's own spacing, the road would be draped on
    // coarser ground: another road.
    let own = evaluate(&ev, &g, &streamed(level1, None));
    assert_ne!(key(&own, "road"), key(&fine, "road"));
    assert_ne!(
        output_hash(own.output("road").unwrap()),
        output_hash(fine.output("road").unwrap())
    );
}

#[test]
fn road_fields_agree_where_windows_meet() {
    let g = build(&valley(false));
    let ev = evaluator(2);
    let a = evaluate(&ev, &g, &streamed(cell(-1, 0), Some(SPACING)));
    let b = evaluate(&ev, &g, &streamed(cell(0, 0), Some(SPACING)));
    // A window over both cells.
    let both = Domain {
        origin: [-16, 0],
        n: 33,
        spacing: SPACING,
    };
    let w = evaluate(&ev, &g, &streamed(both, Some(SPACING)));
    for id in ["flat", "clear"] {
        let (fa, fb, fw) = (field_of(&a, id), field_of(&b, id), field_of(&w, id));
        for z in 0..=STEPS {
            assert_eq!(
                fa.0.sample(STEPS, z).to_bits(),
                fb.0.sample(0, z).to_bits(),
                "{id} at the border, row {z}"
            );
            for x in 0..=STEPS {
                assert_eq!(
                    fa.0.sample(x, z).to_bits(),
                    fw.0.sample(x, z).to_bits(),
                    "{id} ({x}, {z})"
                );
                assert_eq!(
                    fb.0.sample(x, z).to_bits(),
                    fw.0.sample(x + STEPS, z).to_bits(),
                    "{id} ({x}, {z})"
                );
            }
        }
    }
    // The road crosses both cells: the mask is 0 on it, and the ground is
    // flattened.
    assert!(field_of(&w, "clear").0.samples.contains(&0.0));
    assert_ne!(field_of(&w, "flat").0.samples, field_of(&w, "e").0.samples);
}

#[test]
fn cells_share_out_the_road_surface() {
    // Two roads that cross, on no field: streamed, the paths are the same
    // as not streamed.
    let doc = graph(
        7,
        &[
            (
                "main",
                "path.spline",
                json!({ "points": ROAD, "width": 7.0 }),
            ),
            (
                "side",
                "path.spline",
                json!({ "points": [[10.0, 0.5, -58.0], [-5.0, 0.5, 55.0]], "width": 5.0 }),
            ),
            ("surface", "road.mesh", json!({ "junction_radius": 9.0 })),
        ],
        &[
            ("main", "path", "surface", "in1"),
            ("side", "path", "surface", "in2"),
        ],
    );
    let g = build(&doc);
    let ev = evaluator(2);
    let whole = mesh_of(&evaluate(&ev, &g, &EvalOptions::default()), "surface");
    let (mut triangles, mut total, mut crossed) = (0, 0.0, 0);
    // These cells hold both roads.
    for z in -1..1 {
        for x in -2..2 {
            let d = cell(x, z);
            let e = evaluate(&ev, &g, &streamed(d, Some(SPACING)));
            assert_eq!(e.report.node("surface").unwrap().logs, ["1 junctions"]);
            let m = mesh_of(&e, "surface");
            // Each triangle's centre lies in the cell, local to its corner.
            let at = |i: u32| m.mesh.vertices[i as usize].pos;
            for t in m.mesh.indices.as_chunks::<3>().0 {
                let c = [0, 2].map(|k| (at(t[0])[k] + at(t[1])[k] + at(t[2])[k]) / 3.0);
                let size = (STEPS as f32) * SPACING;
                assert!(
                    c.iter().all(|v| (-1e-3..size + 1e-3).contains(v)),
                    "cell ({x}, {z}): {c:?}"
                );
            }
            // A cell the roads cross has one entity at its corner; the
            // others have none.
            let frag = e.fragment().unwrap();
            if m.mesh.indices.is_empty() {
                assert!(frag.items.is_empty(), "cell ({x}, {z})");
            } else {
                crossed += 1;
                let [cx, cz] = d.min();
                assert_eq!(frag.items.len(), 1);
                assert_eq!(frag.items[0].key, "surface/mesh");
                assert_eq!(
                    frag.items[0].components[0],
                    (
                        "Transform".to_string(),
                        vec![(
                            "translation".to_string(),
                            Value::Vec3(Vec3::new(cx, 0.0, cz))
                        )]
                    )
                );
            }
            triangles += m.mesh.indices.len() / 3;
            total += area(&m.mesh);
        }
    }
    assert!(crossed >= 4, "{crossed}");
    // Every triangle of the whole road is in exactly one cell.
    assert_eq!(triangles, whole.mesh.indices.len() / 3);
    let full = area(&whole.mesh);
    assert!((total - full).abs() < 1e-4 * full, "{total} {full}");
    // Far from the roads, a cell has no road at all.
    let far = evaluate(&ev, &g, &streamed(cell(5, 5), Some(SPACING)));
    assert!(mesh_of(&far, "surface").mesh.indices.is_empty());
    assert!(far.fragment().unwrap().items.is_empty());
}

#[test]
fn cells_share_out_shape_meshes() {
    let doc = graph(
        7,
        &[
            (
                "plaza",
                "shape.polygon",
                json!({
                    "points": [[10.0, -5.0], [40.0, -5.0], [40.0, 25.0], [10.0, 25.0]],
                    "holes": [[[15.0, 15.0], [25.0, 15.0], [20.0, 25.0]]],
                }),
            ),
            ("pad", "shape.mesh", json!({ "height": 2.0 })),
        ],
        &[("plaza", "shapes", "pad", "shapes")],
    );
    let g = build(&doc);
    let ev = evaluator(2);
    let whole = area(&mesh_of(&evaluate(&ev, &g, &EvalOptions::default()), "pad").mesh);
    assert!((whole - 850.0).abs() < 1e-6, "{whole}");
    // 16 m cells.
    let mut total = 0.0;
    for z in -1..2 {
        for x in 0..3 {
            let d = Domain {
                origin: [x * 4, z * 4],
                n: 5,
                spacing: 4.0,
            };
            let m = mesh_of(&evaluate(&ev, &g, &streamed(d, Some(4.0))), "pad");
            let [cx, cz] = d.min();
            for v in &m.mesh.vertices {
                // Within the cell, local to its corner, at the pad's height;
                // texture coordinates stay in world metres.
                assert!((-1e-3..16.001).contains(&v.pos[0]), "{v:?}");
                assert!((-1e-3..16.001).contains(&v.pos[2]), "{v:?}");
                assert_eq!(v.pos[1], 2.0);
                assert!((v.uv[0] - (v.pos[0] + cx)).abs() < 1e-4, "{v:?}");
                assert!((v.uv[1] - (v.pos[2] + cz)).abs() < 1e-4, "{v:?}");
            }
            total += area(&m.mesh);
        }
    }
    assert!((total - 850.0).abs() < 1e-3, "{total}");
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

impl Params for NoParams {}

/// A kind that streams as `streaming` and outputs `output`, with no inputs.
fn test_kind(kind: &'static str, streaming: Streaming, output: PortType) -> NodeDef {
    NodeDef::new(
        NodeSpec {
            kind,
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming,
            doc: "A test kind.",
            inputs: &[],
            variadic: None,
            output,
            uses_seed: false,
            file_params: &[],
        },
        |_: &NodeCtx, _: &NoParams| -> Result<Output, String> { Err("not run".into()) },
    )
}

fn registry_with_test_kinds() -> NodeRegistry {
    let mut r = NodeRegistry::m1();
    r.register(test_kind(
        "test.cell_shapes",
        Streaming::Cell,
        PortType::Shapes,
    ));
    r.register(test_kind(
        "test.cell_field",
        Streaming::Cell,
        PortType::Field,
    ));
    r.register(test_kind(
        "test.global_field",
        Streaming::Global,
        PortType::Field,
    ));
    r
}

/// A region kind that reads its field over its own window, as erosion reads
/// its tiles, and also takes a path.
fn region_kind() -> NodeDef {
    NodeDef::new(
        NodeSpec {
            kind: "test.region_read",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: Streaming::Region,
            doc: "Reads its field over its window.",
            inputs: &[
                PortDef {
                    name: "field",
                    accepts: &[PortType::Field],
                    required: true,
                    doc: "The field.",
                },
                PortDef {
                    name: "path",
                    accepts: &[PortType::Path],
                    required: true,
                    doc: "A path.",
                },
            ],
            variadic: None,
            output: PortType::Field,
            uses_seed: false,
            file_params: &[],
        },
        |ctx: &NodeCtx, _: &NoParams| -> Result<Output, String> {
            let (Some(read), Some(d)) = (ctx.upstream, ctx.domain) else {
                return Err("not streamed".into());
            };
            Ok(Output::Field(read("field", d)?))
        },
    )
}

#[test]
fn a_region_node_reads_a_field_that_a_global_node_also_samples() {
    let mut r = NodeRegistry::m1();
    r.register(region_kind());
    let doc = graph(
        7,
        &[
            (
                "h",
                "terrain.heightfield",
                json!({ "cell": 4.0, "height": 40.0, "scale": 0.01 }),
            ),
            ("road", "path.spline", json!({ "points": ROAD })),
            ("r", "test.region_read", json!({})),
        ],
        &[
            ("h", "field", "road", "field"),
            ("h", "field", "r", "field"),
            ("road", "path", "r", "path"),
        ],
    );
    let g = ProcGraph::from_doc(&doc, &r).unwrap();
    let ev = Evaluator::new(Arc::new(r));
    let e = evaluate(&ev, &g, &streamed(cell(0, 0), Some(SPACING)));
    assert_eq!(field_of(&e, "r").0.samples, field_of(&e, "h").0.samples);
}

#[test]
fn streamed_graphs_give_every_window_the_same_roads() {
    let r = registry_with_test_kinds();
    let check = |nodes: &[(&str, &str, Json)], edges: &[(&str, &str, &str, &str)]| {
        re_proc::nodes::check_streaming(&graph(7, nodes, edges), &r)
    };
    let roads = GraphDoc::from_json(include_str!("../examples/roads.graph.json")).unwrap();
    assert_eq!(re_proc::nodes::check_streaming(&roads, &r), Ok(()));
    assert_eq!(re_proc::nodes::check_streaming(&valley(true), &r), Ok(()));
    let road = || ("road", "path.spline", json!({ "points": ROAD }));
    // Shapes from a cell node could differ from cell to cell.
    let err = check(
        &[
            ("s", "test.cell_shapes", json!({})),
            ("m", "shape.mesh", json!({})),
        ],
        &[("s", "shapes", "m", "shapes")],
    )
    .unwrap_err();
    assert!(err.contains("`s`") && err.contains("`cell`"), "{err}");
    // So could a field from one, draped on by a global node...
    let err = check(
        &[("f", "test.cell_field", json!({})), road()],
        &[("f", "field", "road", "field")],
    )
    .unwrap_err();
    assert!(err.contains("`road`") && err.contains("`f`"), "{err}");
    // ...even through a pointwise node.
    let err = check(
        &[
            ("f", "test.cell_field", json!({})),
            ("h", "terrain.heightfield", json!({})),
            ("sum", "field.combine", json!({ "op": "add" })),
            road(),
        ],
        &[
            ("f", "field", "sum", "a"),
            ("h", "field", "sum", "b"),
            ("sum", "field", "road", "field"),
        ],
    )
    .unwrap_err();
    assert!(
        err.contains("`road`") && err.contains("`f`") && err.contains("upstream"),
        "{err}"
    );
    // A global node outputs shapes or a path.
    let err = check(&[("g", "test.global_field", json!({}))], &[]).unwrap_err();
    assert!(err.contains("`g`") && err.contains("`field`"), "{err}");
}

fn config() -> StreamConfig {
    StreamConfig {
        cell_size: 64.0,
        radius: 64.0,
        radius_ahead: 128.0,
        unload_margin: 32.0,
        delay_ticks: 3,
        apply_entities: 400,
        terrain_view: 1024.0,
        ..StreamConfig::default()
    }
}

/// A world streaming `doc` through `evaluator` as source `terrain` (bounds
/// 256 m), with a focus `player` at `(x, z)`.
fn world(doc: GraphDoc, evaluator: Evaluator, x: f32, z: f32) -> EditState {
    let mut st = EditState::new();
    register_generator(&mut st.world.generators);
    let mut store = GraphStore::with_evaluator(Arc::new(evaluator), Some(fixtures()));
    store.insert("terrain", doc);
    st.world.ecs.insert_resource(store);
    let scene = json!({
        "format": "real-engine/scene",
        "version": 1,
        "entities": [
            {
                "id": "terrain",
                "name": "Terrain",
                "components": {
                    "GraphSource": { "builder": "scripts/tools/world.luau" },
                    "Procedural": { "generator": "graph", "seed": 7 },
                    "Streamed": { "bounds": 256.0 },
                    "Transform": {}
                }
            },
            {
                "id": "player",
                "name": "Player",
                "components": {
                    "StreamFocus": {},
                    "Transform": { "translation": [x, 0.0, z] }
                }
            }
        ]
    });
    let doc = re_scene::scene::from_json(&scene.to_string()).expect("scene parses");
    st.world.load(&doc).expect("scene loads");
    let r = st.world.update();
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    st
}

/// Steps with the terrain view on until every cell and chunk is in, checking
/// that no step reports a problem; returns the next tick. Chunks come in off
/// the main thread, so a slow machine takes more ticks.
fn settle(w: &mut World, from: u64) -> u64 {
    if !w.ecs.contains_resource::<Streamer>() {
        let mut s = Streamer::new(config()).unwrap();
        s.set_view(true);
        w.ecs.insert_resource(s);
    }
    for t in from..from + 60_000 {
        let r = step(w, t, &config());
        assert!(r.diagnostics.is_empty(), "tick {t}: {:#?}", r.diagnostics);
        w.update();
        let s = w.ecs.resource::<Streamer>();
        let shown = w
            .ecs
            .get_resource::<TerrainView>()
            .map_or(0, TerrainView::len);
        if r.pending == 0
            && s.in_flight() == 0
            && s.view_missing() == 0
            && shown > 0
            && t > from + 10
        {
            return t + 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the streamed world never settled");
}

/// Checks every terrain chunk vertex off the chunk's edges (where it may be
/// stitched to a coarser neighbour) against `flat`, the flattened ground at
/// the source's spacing over the source's square; returns how many it
/// checked within 12 m of the road.
fn check_chunks(w: &World, flat: &Field, road: &RoadPath) -> usize {
    let mut near = 0;
    for c in w.ecs.resource::<TerrainView>().chunks() {
        let size = 64.0 * SPACING * f32::from(1u16 << c.key.level);
        for v in &c.data().vertices {
            let [lx, _, lz] = v.pos;
            if lx == 0.0 || lz == 0.0 || lx == size || lz == size {
                continue;
            }
            let (x, z) = (c.corner.x + lx, c.corner.z + lz);
            let Some(want) = flat
                .0
                .height_at(x, z)
                .filter(|_| x.abs() < 256.0 && z.abs() < 256.0)
            else {
                continue;
            };
            assert!(
                (v.pos[1] - want).abs() < 1e-4,
                "chunk {:?} at ({x}, {z}): {} but the cells' ground is {want}",
                c.key,
                v.pos[1]
            );
            if distance_to(road, f64::from(x), f64::from(z)) < 12.0 {
                near += 1;
            }
        }
    }
    near
}

#[test]
fn a_streamed_valley_carries_its_road_at_every_level_of_detail() {
    let mut st = world(valley(true), Evaluator::default(), 0.0, 0.0);
    let w = &mut st.world;
    let t = settle(w, 0);
    // The cells the road crosses draw their part of it, at their corners;
    // a loaded cell it misses has none.
    let at = |w: &World, id: &str| w.get(id, "Transform", "translation").ok();
    for (cell, corner) in [
        ("c/0_0", Vec3::ZERO),
        ("c/-1_0", Vec3::new(-64.0, 0.0, 0.0)),
        ("c/-1_-1", Vec3::new(-64.0, 0.0, -64.0)),
    ] {
        assert_eq!(
            at(w, &format!("terrain/{cell}/surface/mesh")),
            Some(Value::Vec3(corner)),
            "{cell}"
        );
    }
    assert!(w.ecs.resource::<Streamer>().is_loaded("c/0_-1"));
    assert!(w.entity("terrain/c/0_-1/surface/mesh").is_err());

    // The ground the cells see, over the source's square, and the road.
    let square = Domain {
        origin: [-64, -64],
        n: 129,
        spacing: SPACING,
    };
    let reference = evaluate(
        &evaluator(2),
        &build(&valley(false)),
        &streamed(square, Some(SPACING)),
    );
    let (flat, road) = (field_of(&reference, "flat"), path_of(&reference, "road"));
    // It is flattened just under the road.
    for s in &road.samples {
        let [x, y, z] = s.position;
        let ground = f64::from(flat.0.height_at(x as f32, z as f32).unwrap());
        assert!(
            (ground - (y - 0.05)).abs() < 0.1,
            "{:?}: {ground}",
            s.position
        );
    }
    // The terrain chunks near the focus are that ground.
    assert!(
        w.ecs
            .resource::<TerrainView>()
            .chunks()
            .any(|c| c.key.level == 0)
    );
    assert!(check_chunks(w, &flat, &road) > 0);

    // Far away, the source is drawn coarser, and still on the same road.
    w.set(
        "player",
        "Transform",
        "translation",
        Value::Vec3(Vec3::new(1200.0, 0.0, 1200.0)),
    )
    .unwrap();
    settle(w, t);
    assert!(
        w.ecs
            .resource::<TerrainView>()
            .chunks()
            .all(|c| c.key.level >= 1)
    );
    assert!(check_chunks(w, &flat, &road) > 0);
}

#[test]
fn a_world_refuses_roads_its_cells_could_see_differently() {
    let doc = graph(
        7,
        &[
            ("f", "test.cell_field", json!({})),
            ("road", "path.spline", json!({ "points": ROAD })),
            ("surface", "road.mesh", json!({})),
        ],
        &[
            ("f", "field", "road", "field"),
            ("road", "path", "surface", "in1"),
        ],
    );
    let ev = Evaluator::new(Arc::new(registry_with_test_kinds()));
    let mut st = world(doc, ev, 0.0, 0.0);
    let r = step(&mut st.world, 0, &config());
    assert_eq!(r.diagnostics.len(), 1, "{:#?}", r.diagnostics);
    assert_eq!(r.diagnostics[0].code.to_string(), "RE4301");
    assert!(
        r.diagnostics[0].message.contains("`road`"),
        "{}",
        r.diagnostics[0].message
    );
}
