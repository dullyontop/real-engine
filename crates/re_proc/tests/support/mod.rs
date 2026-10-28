//! Shared test helpers: graphs, worlds with a graph source, output hashes.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use re_command::EditState;
use re_proc::{Evaluator, GraphStore, NodeRegistry, Output, register_generator};
use re_types::graph::GraphDoc;
use serde_json::{Value as Json, json};

/// The fixture project root (holds `prefabs/crate.prefab.json`).
pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A graph document from `{name, seed, nodes: [[id, kind, params]], edges: [[from, port, to, port]]}`.
pub fn graph(
    seed: i64,
    nodes: &[(&str, &str, Json)],
    edges: &[(&str, &str, &str, &str)],
) -> GraphDoc {
    let doc = json!({
        "format": "real-engine/graph",
        "version": 1,
        "name": "test",
        "seed": seed,
        "nodes": nodes.iter().map(|(id, kind, params)| json!({
            "id": id, "kind": kind, "params": params,
            // Each kind's current version.
            "version": re_proc::NodeRegistry::m1().get(kind).map_or(1, |d| d.version),
        })).collect::<Vec<_>>(),
        "edges": edges.iter().map(|(f, fp, t, tp)| json!({
            "from": [f, fp], "to": [t, tp],
        })).collect::<Vec<_>>(),
    });
    GraphDoc::from_json(&doc.to_string()).expect("test graph parses")
}

/// A small island: terrain with a collider, water, crates on a jittered
/// grid (stable keys across seeds), and a material-assigned primitive.
pub fn island(seed: i64) -> GraphDoc {
    graph(
        seed,
        &[
            (
                "h",
                "terrain.heightfield",
                json!({ "size": 33, "cell": 2.0, "height": 10.0, "scale": 0.03 }),
            ),
            ("mask", "terrain.island_mask", json!({ "size": 17 })),
            ("land", "field.combine", json!({ "op": "mul" })),
            (
                "ground",
                "terrain.mesh",
                json!({ "collider": true, "material": "materials/grass.material.json" }),
            ),
            (
                "water",
                "terrain.water_plane",
                json!({ "level": 0.5, "size": 100.0 }),
            ),
            (
                "grid",
                "scatter.grid_jitter",
                json!({ "spacing": 12.0, "jitter": 0.5 }),
            ),
            (
                "crates",
                "points.instance",
                json!({ "prefab": "prefabs/crate.prefab.json" }),
            ),
            (
                "box",
                "mesh.primitive",
                json!({ "shape": "box", "size": [2.0, 1.0, 2.0] }),
            ),
            (
                "painted",
                "material.assign",
                json!({ "material": "materials/red.material.json" }),
            ),
        ],
        &[
            ("h", "field", "land", "a"),
            ("mask", "field", "land", "b"),
            ("land", "field", "ground", "field"),
            ("land", "field", "grid", "field"),
            ("grid", "points", "crates", "points"),
            ("box", "mesh", "painted", "mesh"),
        ],
    )
}

/// An evaluator with its own pool of `threads` workers and no disk cache.
pub fn evaluator(threads: usize) -> Evaluator {
    Evaluator::new(Arc::new(NodeRegistry::m1()))
        .with_pool(Arc::new(re_core::TaskPool::new(threads).expect("pool")))
}

/// An edit state whose world has the `graph` generator, a `GraphStore`
/// (fixture root) holding `doc` for source `island`, and the source entity
/// (with `GraphSource.builder` set, as a builder-sourced source), already
/// generated once. `overrides` is the source's `overrides` list.
pub fn world(doc: GraphDoc, overrides: Json) -> EditState {
    let mut st = EditState::new();
    register_generator(&mut st.world.generators);
    let mut store = GraphStore::with_evaluator(Arc::new(Evaluator::default()), Some(fixtures()));
    store.insert("island", doc);
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
                    "GraphSource": { "builder": "scripts/tools/island_graph.luau" },
                    "Procedural": { "generator": "graph", "seed": doc_seed(&st) },
                    "Transform": {}
                },
                "overrides": overrides
            }
        ]
    });
    let doc = re_scene::scene::from_json(&scene.to_string()).expect("scene parses");
    st.world.load(&doc).expect("scene loads");
    let report = st.world.regenerate_changed();
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    st
}

fn doc_seed(st: &EditState) -> i64 {
    st.world
        .ecs
        .resource::<GraphStore>()
        .graph("island")
        .map_or(0, |d| d.seed)
}

/// A hash of a node output's exact content.
pub fn output_hash(output: &Output) -> String {
    let mut h = blake3::Hasher::new();
    let f = |h: &mut blake3::Hasher, x: f32| {
        h.update(&x.to_bits().to_le_bytes());
    };
    match output {
        Output::Field(field) => {
            h.update(&field.0.n.to_le_bytes());
            f(&mut h, field.0.spacing);
            for v in field.0.offset.to_array().iter().chain(&field.0.samples) {
                f(&mut h, *v);
            }
        }
        Output::Mesh(m) => {
            for v in &m.mesh.vertices {
                for x in v.pos.iter().chain(&v.normal).chain(&v.uv).chain(&v.color) {
                    f(&mut h, *x);
                }
            }
            for i in &m.mesh.indices {
                h.update(&i.to_le_bytes());
            }
            h.update(m.material.as_bytes());
            h.update(&m.asset);
        }
        Output::Points(p) => {
            for pt in &p.points {
                h.update(&pt.id.to_le_bytes());
                for x in pt.position.to_array().iter().chain(&pt.normal.to_array()) {
                    f(&mut h, *x);
                }
                h.update(pt.tag.as_bytes());
            }
        }
        Output::Fragment(frag) => {
            let world = re_scene::World::new();
            let nodes =
                re_proc::to_scene_fragment("test", frag, &world.registry).expect("fragment");
            h.update(re_proc::fragment_hash_hex(&nodes).as_bytes());
            for (name, _) in &frag.assets {
                h.update(name.as_bytes());
            }
        }
        Output::Shapes(shapes) => {
            for s in &shapes.shapes {
                for c in std::iter::once(&s.outer).chain(&s.holes) {
                    for p in c {
                        h.update(&p[0].to_bits().to_le_bytes());
                        h.update(&p[1].to_bits().to_le_bytes());
                    }
                    h.update(b"|");
                }
                h.update(b"/");
            }
        }
        Output::Path(path) => {
            h.update(&[u8::from(path.closed)]);
            for s in &path.samples {
                for x in s.position.iter().chain(&[s.width, s.bank, s.distance]) {
                    h.update(&x.to_bits().to_le_bytes());
                }
            }
        }
        Output::PolyMesh(m) => {
            use re_geom::poly::AttrValue;
            for p in &m.positions {
                for x in p.to_array() {
                    f(&mut h, x);
                }
            }
            for i in m.face_start.iter().chain(&m.corner_point) {
                h.update(&i.to_le_bytes());
            }
            for attrs in [&m.points, &m.corners, &m.faces] {
                h.update(b"|");
                for (name, col) in attrs.iter() {
                    h.update(name.as_bytes());
                    h.update(col.ty().name().as_bytes());
                    for i in 0..col.len() {
                        match col.get(i) {
                            AttrValue::Float(v) => f(&mut h, v),
                            AttrValue::Int(v) => {
                                h.update(&v.to_le_bytes());
                            }
                            AttrValue::Bool(v) => {
                                h.update(&[u8::from(v)]);
                            }
                            AttrValue::Vec2(v) => v.iter().for_each(|x| f(&mut h, *x)),
                            AttrValue::Vec3(v) => v.to_array().iter().for_each(|x| f(&mut h, *x)),
                            AttrValue::Vec4(v) => v.iter().for_each(|x| f(&mut h, *x)),
                            AttrValue::Quat(v) => v.to_array().iter().for_each(|x| f(&mut h, *x)),
                        }
                    }
                }
            }
            for path in &m.materials {
                h.update(path.as_bytes());
                h.update(b"\0");
            }
            h.update(format!("{:?}{}", m.collider, m.visible).as_bytes());
        }
    }
    h.finalize().to_hex()[..16].to_string()
}
