//! The modelling kinds (ADR-0062) on `examples/poly.graph.json` and small
//! graphs: the entities a polygon mesh becomes, the codes of what they
//! refuse, the disk cache, and parameter schemas that agree with `validate`.

mod support;

use std::path::PathBuf;

use re_geom::poly::{AttrValue, Column, PolyMesh};
use re_proc::{CacheState, EvalOptions, Evaluation, FragItem, Output, ProcGraph};
use re_reflect::Value;
use re_types::graph::GraphDoc;
use serde_json::{Value as Json, json};
use support::{evaluator, graph, output_hash};

fn cottage() -> ProcGraph {
    let doc = GraphDoc::from_json(include_str!("../examples/poly.graph.json")).unwrap();
    ProcGraph::from_doc(&doc, &re_proc::NodeRegistry::m1()).unwrap()
}

fn opts() -> EvalOptions {
    EvalOptions {
        root: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")),
        ..Default::default()
    }
}

fn eval(nodes: &[(&str, &str, Json)], edges: &[(&str, &str, &str, &str)]) -> Evaluation {
    let g = ProcGraph::from_doc(&graph(7, nodes, edges), &re_proc::NodeRegistry::m1()).unwrap();
    evaluator(1).evaluate(&g, &opts())
}

fn polymesh(e: &Evaluation, id: &str) -> std::sync::Arc<PolyMesh> {
    match e.output(id) {
        Some(Output::PolyMesh(m)) => m.clone(),
        o => panic!("{id}: {o:?} {:#?}", e.report),
    }
}

/// The string field `field` of component `component`, if set.
fn field<'a>(item: &'a FragItem, component: &str, field: &str) -> Option<&'a str> {
    let (_, fields) = item.components.iter().find(|(c, _)| c == component)?;
    match fields.iter().find(|(f, _)| f == field)? {
        (_, Value::Str(s)) => Some(s),
        _ => None,
    }
}

/// The codes of every diagnostic, from loading or from the nodes.
fn codes(nodes: &[(&str, &str, Json)], edges: &[(&str, &str, &str, &str)]) -> Vec<String> {
    match ProcGraph::from_doc(&graph(7, nodes, edges), &re_proc::NodeRegistry::m1()) {
        Err(diags) => diags.iter().map(|d| d.code.to_string()).collect(),
        Ok(g) => evaluator(1)
            .evaluate(&g, &opts())
            .report
            .nodes
            .iter()
            .flat_map(|n| &n.diagnostics)
            .map(|d| d.code.to_string())
            .collect(),
    }
}

#[test]
fn the_cottage_becomes_one_entity_per_material_part() {
    let e = evaluator(4).evaluate(&cottage(), &opts());
    assert!(e.ok(), "{:#?}", e.report);
    let welded = polymesh(&e, "welded");
    welded.validate().unwrap();
    assert_eq!(
        welded.materials,
        [
            "",
            "materials/plaster.material.json",
            "materials/roof.material.json"
        ]
    );
    let f = e.fragment().unwrap();
    let keys: Vec<&str> = f.items.iter().map(|i| i.key.as_str()).collect();
    // Inputs in natural port order: `ball`, then `cottage`'s parts.
    assert_eq!(
        keys,
        [
            "out/group",
            "out/ball",
            "out/cottage",
            "out/cottage.1",
            "out/cottage.2"
        ]
    );
    let materials: Vec<Option<&str>> = f.items[2..]
        .iter()
        .map(|i| field(i, "MaterialRef", "asset"))
        .collect();
    assert_eq!(
        materials,
        [
            None,
            Some("materials/plaster.material.json"),
            Some("materials/roof.material.json")
        ]
    );
    // Each entity draws a derived mesh the fragment carries, under the group.
    for item in &f.items[1..] {
        let asset = field(item, "MeshRef", "asset").unwrap();
        assert!(f.assets.iter().any(|(n, _)| n == asset), "{asset}");
        assert_eq!(item.parent_key.as_deref(), Some("out/group"));
    }
    assert_eq!(f.assets.len(), 4);
    // The parts' triangles are the whole mesh's.
    let tris: usize = welded
        .to_meshes()
        .unwrap()
        .iter()
        .map(|m| m.triangle_count())
        .sum();
    let drawn: usize = f.assets[1..]
        .iter()
        .map(|(_, a)| match a {
            re_proc::DerivedAsset::Mesh(m) => m.triangle_count(),
            _ => panic!("a heightfield"),
        })
        .sum();
    assert_eq!(drawn, tris);
}

#[test]
fn an_unconsumed_polymesh_is_its_parts_and_equal_parts_share_a_mesh() {
    let e = eval(
        &[
            ("a", "poly.box", json!({})),
            ("b", "poly.box", json!({})),
            (
                "roofed",
                "poly.material",
                json!({ "group": "top", "material": "materials/roof.material.json" }),
            ),
            (
                "stone",
                "poly.material",
                json!({ "material": "materials/stone.material.json" }),
            ),
        ],
        &[
            ("a", "polymesh", "roofed", "mesh"),
            ("b", "polymesh", "stone", "mesh"),
        ],
    );
    assert!(e.ok(), "{:#?}", e.report);
    let f = e.fragment().unwrap();
    let keys: Vec<&str> = f.items.iter().map(|i| i.key.as_str()).collect();
    // Painted all over, `stone` has one part, so one entity.
    assert_eq!(keys, ["roofed/mesh", "roofed/mesh.1", "stone/mesh"]);
    assert_eq!(field(&f.items[0], "MaterialRef", "asset"), None);
    assert_eq!(
        field(&f.items[2], "MaterialRef", "asset"),
        Some("materials/stone.material.json")
    );
    assert_eq!(f.assets.len(), 3);
    // Two equal parts in two nodes share one derived mesh.
    let e = eval(
        &[("a", "poly.box", json!({})), ("b", "poly.box", json!({}))],
        &[],
    );
    let f = e.fragment().unwrap();
    assert_eq!(f.items.len(), 2);
    assert_eq!(
        field(&f.items[0], "MeshRef", "asset"),
        field(&f.items[1], "MeshRef", "asset")
    );
    assert_eq!(f.assets.len(), 1);
}

#[test]
fn a_face_that_cannot_be_triangulated_fails_the_output() {
    // A quad whose boundary crosses itself, with a non-zero area: valid, but
    // nothing can draw it.
    let mut m = PolyMesh::new();
    m.positions = vec![
        glam::Vec3::new(0.0, 0.0, 0.0),
        glam::Vec3::new(2.0, 0.0, 0.0),
        glam::Vec3::new(0.0, 0.0, -1.0),
        glam::Vec3::new(1.0, 0.0, 1.0),
    ];
    m.corner_point = vec![0, 1, 2, 3];
    m.face_start = vec![0, 4];
    m.validate().unwrap();
    let output = Output::PolyMesh(std::sync::Arc::new(m));
    let err = re_proc::nodes::sink_fragment("bad", &output).unwrap_err();
    assert_eq!(err.code.to_string(), "RE4505");
}

#[test]
fn names_a_mesh_lacks_are_re4150() {
    let on_box = |kind: &str, params: Json| {
        codes(
            &[("box", "poly.box", json!({})), ("op", kind, params)],
            &[("box", "polymesh", "op", "mesh")],
        )
    };
    assert_eq!(
        on_box(
            "poly.material",
            json!({ "group": "roof", "material": "m.json" })
        ),
        ["RE4150"]
    );
    assert_eq!(on_box("poly.flip", json!({ "group": "roof" })), ["RE4150"]);
    assert_eq!(
        on_box(
            "poly.group",
            json!({ "by": "union", "groups": ["top", "roof"] })
        ),
        ["RE4150"]
    );
    assert_eq!(
        on_box(
            "poly.group",
            json!({ "by": "material", "material": "m.json" })
        ),
        ["RE4150"]
    );
    assert_eq!(
        on_box("poly.to_mesh", json!({ "material": "m.json" })),
        ["RE4150"]
    );
    // `uv` is a corner `vec2` on the box: a `vec3` of that name is refused.
    assert_eq!(
        on_box(
            "poly.attribute",
            json!({ "domain": "corner", "name": "uv", "type": "vec3", "value": [0, 0, 0] })
        ),
        ["RE4005"]
    );
    let e = eval(
        &[
            ("box", "poly.box", json!({})),
            (
                "a",
                "poly.attribute",
                json!({ "name": "heat", "value": 2.0 }),
            ),
            (
                "b",
                "poly.attribute",
                json!({ "name": "heat", "value": [1.0, 2.0] }),
            ),
        ],
        &[
            ("box", "polymesh", "a", "mesh"),
            ("a", "polymesh", "b", "mesh"),
        ],
    );
    let row = e.report.node("b").unwrap();
    assert_eq!(row.diagnostics[0].code.to_string(), "RE4150");
    assert!(
        row.diagnostics[0].message.contains("`heat` is a `float`"),
        "{}",
        row.diagnostics[0].message
    );
    // The message lists the groups the mesh has.
    let e = eval(
        &[
            ("box", "poly.box", json!({})),
            ("op", "poly.flip", json!({ "group": "roof" })),
        ],
        &[("box", "polymesh", "op", "mesh")],
    );
    let message = &e.report.node("op").unwrap().diagnostics[0].message;
    assert!(
        message.contains("(its groups: bottom, side, top)"),
        "{message}"
    );
}

#[test]
fn bad_parameters_are_re4005() {
    for (kind, params) in [
        ("poly.box", json!({ "radius": 0.6 })),
        ("poly.box", json!({ "radius": 0.1, "segments": 3 })),
        ("poly.box", json!({ "size": [1.0, 0.0, 1.0] })),
        ("poly.cone", json!({ "radius": 0.0, "top_radius": 0.0 })),
        ("poly.torus", json!({ "radius": 1.0, "tube": 1.0 })),
        ("poly.arch", json!({ "span": 1.0, "rise": 0.6 })),
        ("poly.icosphere", json!({ "subdivisions": 7 })),
        ("poly.stairs", json!({ "steps": 0 })),
        ("poly.transform", json!({ "scale": [1.0, 0.0, 1.0] })),
        ("poly.weld", json!({ "distance": -1.0 })),
        ("poly.group", json!({ "name": "uv" })),
        ("poly.group", json!({ "name": "Top" })),
        ("poly.group", json!({ "direction": [0.0, 0.0, 0.0] })),
        ("poly.group", json!({ "by": "union" })),
        (
            "poly.group",
            json!({ "by": "box", "min": [1, 1, 1], "max": [0, 0, 0] }),
        ),
        ("poly.group", json!({ "by": "index", "start": 3, "end": 2 })),
        (
            "poly.attribute",
            json!({ "name": "material", "value": 1, "type": "int" }),
        ),
        (
            "poly.attribute",
            json!({ "name": "position", "value": [0, 0, 0] }),
        ),
        ("poly.attribute", json!({ "name": "color", "value": 1.0 })),
        (
            "poly.attribute",
            json!({ "name": "heat", "value": 1.5, "type": "int" }),
        ),
        (
            "poly.attribute",
            json!({ "name": "heat", "value": [1, 2, 3, 4, 5] }),
        ),
        ("poly.attribute", json!({ "name": "heat", "value": "hot" })),
        (
            "poly.attribute",
            json!({ "name": "spin", "value": [0, 0, 0, 0], "type": "quat" }),
        ),
        ("poly.flip", json!({ "group": "No Such" })),
        ("poly.material", json!({ "unknown": 1 })),
    ] {
        // A kind with a required input also misses it here (`RE4006`).
        let got = codes(&[("n", kind, params.clone())], &[]);
        assert!(
            got.iter().all(|c| c == "RE4005" || c == "RE4006") && got.contains(&"RE4005".into()),
            "{kind} {params}: {got:?}"
        );
    }
}

#[test]
fn groups_choose_faces_by_each_rule() {
    let count = |params: Json| {
        let e = eval(
            &[
                ("box", "poly.box", json!({})),
                (
                    "roofed",
                    "poly.material",
                    json!({ "group": "top", "material": "roof.json" }),
                ),
                ("g", "poly.group", params.clone()),
            ],
            &[
                ("box", "polymesh", "roofed", "mesh"),
                ("roofed", "polymesh", "g", "mesh"),
            ],
        );
        assert!(e.ok(), "{params}: {:#?}", e.report);
        let m = polymesh(&e, "g");
        m.validate().unwrap();
        m.faces
            .bools("selection")
            .unwrap()
            .iter()
            .filter(|s| **s)
            .count()
    };
    assert_eq!(count(json!({})), 1, "facing up within 45°");
    assert_eq!(count(json!({ "angle": 91.0 })), 5);
    assert_eq!(count(json!({ "angle": 180.0 })), 6);
    assert_eq!(
        count(json!({ "direction": [0.0, 0.0, -2.0], "angle": 10.0 })),
        1
    );
    assert_eq!(
        count(json!({ "by": "box", "min": [-1, 0.4, -1], "max": [1, 1, 1] })),
        1
    );
    assert_eq!(
        count(json!({ "by": "material", "material": "roof.json" })),
        1
    );
    assert_eq!(count(json!({ "by": "material", "material": "" })), 5);
    assert_eq!(count(json!({ "by": "index", "start": 2, "end": 4 })), 2);
    assert_eq!(count(json!({ "by": "index", "start": 5, "end": 99 })), 1);
    assert_eq!(count(json!({ "by": "random", "fraction": 0.0 })), 0);
    assert_eq!(count(json!({ "by": "random", "fraction": 1.0 })), 6);
    assert_eq!(
        count(json!({ "by": "union", "groups": ["top", "bottom"] })),
        2
    );
    assert_eq!(
        count(json!({ "by": "intersection", "groups": ["top", "side"] })),
        0
    );
    assert_eq!(count(json!({ "by": "complement", "groups": ["side"] })), 2);
    // A random fraction depends on the seed, and only on it.
    let pick = |seed: i64| {
        let doc = graph(
            seed,
            &[
                ("ball", "poly.icosphere", json!({ "subdivisions": 2 })),
                (
                    "g",
                    "poly.group",
                    json!({ "by": "random", "fraction": 0.5 }),
                ),
            ],
            &[("ball", "polymesh", "g", "mesh")],
        );
        let g = ProcGraph::from_doc(&doc, &re_proc::NodeRegistry::m1()).unwrap();
        let e = evaluator(2).evaluate(&g, &opts());
        polymesh(&e, "g").faces.bools("selection").unwrap().to_vec()
    };
    let (a, b) = (pick(1), pick(2));
    assert_eq!(a, pick(1));
    assert_ne!(a, b);
    let chosen = a.iter().filter(|s| **s).count();
    assert!((100..220).contains(&chosen), "{chosen} of 320");
}

#[test]
fn attributes_take_the_type_their_value_has() {
    let e = eval(
        &[
            ("box", "poly.box", json!({})),
            (
                "f",
                "poly.attribute",
                json!({ "name": "heat", "value": 2.5 }),
            ),
            (
                "v",
                "poly.attribute",
                json!({ "domain": "point", "name": "wind", "value": [1, 2] }),
            ),
            (
                "b",
                "poly.attribute",
                json!({ "name": "lit", "value": true, "group": "top" }),
            ),
            (
                "i",
                "poly.attribute",
                json!({ "domain": "corner", "name": "id", "type": "int", "value": 3 }),
            ),
            (
                "q",
                "poly.attribute",
                json!({ "domain": "point", "name": "spin", "type": "quat", "value": [0, 0, 0, 2] }),
            ),
            (
                "c",
                "poly.attribute",
                json!({ "name": "color", "value": [0.5, 0.25, 1.0, 0.5] }),
            ),
            (
                "c3",
                "poly.attribute",
                json!({ "name": "color", "value": [1.0, 0.0, 0.0], "group": "top" }),
            ),
        ],
        &[
            ("box", "polymesh", "f", "mesh"),
            ("f", "polymesh", "v", "mesh"),
            ("v", "polymesh", "b", "mesh"),
            ("b", "polymesh", "i", "mesh"),
            ("i", "polymesh", "q", "mesh"),
            ("q", "polymesh", "c", "mesh"),
            ("c", "polymesh", "c3", "mesh"),
        ],
    );
    assert!(e.ok(), "{:#?}", e.report);
    let m = polymesh(&e, "c3");
    m.validate().unwrap();
    assert_eq!(m.faces.floats("heat").unwrap(), [2.5; 6]);
    assert_eq!(m.points.vec2s("wind").unwrap()[7], [1.0, 2.0]);
    // A `bool` face attribute is a group.
    assert_eq!(m.faces.bools("lit").unwrap(), m.faces.bools("top").unwrap());
    assert_eq!(m.corners.ints("id").unwrap()[0], 3);
    assert_eq!(m.points.quats("spin").unwrap()[0], glam::Quat::IDENTITY);
    // A `vec3` colour on a `vec4` column is opaque.
    let top = m
        .faces
        .bools("top")
        .unwrap()
        .iter()
        .position(|t| *t)
        .unwrap();
    match m.faces.get("color") {
        Some(Column::Vec4(c)) => {
            assert_eq!(c[top], [1.0, 0.0, 0.0, 1.0]);
            assert_eq!(c[(top + 1) % 6], [0.5, 0.25, 1.0, 0.5]);
        }
        c => panic!("{c:?}"),
    }
    assert_eq!(m.faces.get("heat").unwrap().get(0), AttrValue::Float(2.5));
}

#[test]
fn meshes_convert_both_ways() {
    let e = eval(
        &[
            ("ball", "mesh.primitive", json!({ "shape": "box" })),
            (
                "stone",
                "material.assign",
                json!({ "material": "stone.json" }),
            ),
            ("p", "poly.from_mesh", json!({})),
            ("m", "poly.to_mesh", json!({})),
            ("only", "poly.to_mesh", json!({ "material": "stone.json" })),
        ],
        &[
            ("ball", "mesh", "stone", "mesh"),
            ("stone", "mesh", "p", "mesh"),
            ("p", "polymesh", "m", "mesh"),
            ("p", "polymesh", "only", "mesh"),
        ],
    );
    assert!(e.ok(), "{:#?}", e.report);
    let p = polymesh(&e, "p");
    assert_eq!(p.materials, ["stone.json"]);
    assert!(p.is_closed());
    let Some(Output::Mesh(m)) = e.output("m") else {
        panic!()
    };
    assert_eq!(m.material, "stone.json");
    assert_eq!(m.mesh.triangle_count(), p.face_count());
    let Some(Output::Mesh(only)) = e.output("only") else {
        panic!()
    };
    assert_eq!(only.mesh, m.mesh);
}

#[test]
fn polymeshes_come_back_from_the_disk_cache() {
    let dir = tempfile::tempdir().unwrap();
    let g = cottage();
    let make = || evaluator(2).with_disk_cache(dir.path().to_path_buf());
    let first = make().evaluate(&g, &opts());
    assert!(first.ok());
    let second = make().evaluate(&g, &opts());
    for id in ["walls", "tint", "bench", "welded"] {
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
    assert_eq!(first.fragment().unwrap(), second.fragment().unwrap());
}

#[test]
fn the_cottage_does_not_depend_on_the_thread_count() {
    let g = cottage();
    let a = evaluator(1).evaluate(&g, &opts());
    let b = evaluator(4).evaluate(&g, &opts());
    for id in g.nodes.keys() {
        assert_eq!(
            output_hash(a.output(id).unwrap()),
            output_hash(b.output(id).unwrap()),
            "node {id}"
        );
    }
}

/// The next `f32` past `x`, away from the range it bounds.
fn outside(x: f32, below: bool) -> f32 {
    if x == 0.0 {
        return if below {
            -f32::from_bits(1)
        } else {
            f32::from_bits(1)
        };
    }
    let up = below == (x < 0.0);
    f32::from_bits(if up { x.to_bits() + 1 } else { x.to_bits() - 1 })
}

/// Every property of every modelling kind has a default, and each
/// `minimum` and `maximum` is accepted while the nearest value past it is
/// refused, except the bounds another parameter narrows at its default.
#[test]
fn modelling_schemas_agree_with_validate() {
    const NARROWED: &[(&str, &str, &str)] = &[
        // Not both radii 0, and the default top radius is 0.
        ("poly.cone", "radius", "minimum"),
        // The tube is thinner than the radius (1 m by default).
        ("poly.torus", "radius", "minimum"),
        ("poly.torus", "tube", "maximum"),
        // The rise is at most half the span (2 m by default).
        ("poly.arch", "span", "minimum"),
        ("poly.arch", "rise", "maximum"),
        // `min` stays below `max`.
        ("poly.group", "min", "maximum"),
        ("poly.group", "max", "minimum"),
    ];
    let registry = re_proc::NodeRegistry::m1();
    let mut checked = 0;
    for def in registry.iter().filter(|d| d.kind.starts_with("poly.")) {
        let Some(props) = def.params_schema["properties"].as_object() else {
            continue;
        };
        for (name, schema) in props {
            assert!(schema.get("default").is_some(), "{}.{name}", def.kind);
            let items = schema.get("items");
            let target = items.unwrap_or(schema);
            let integer = target["type"] == "integer";
            for (key, below) in [("minimum", true), ("maximum", false)] {
                let Some(bound) = target.get(key).and_then(Json::as_f64) else {
                    continue;
                };
                if NARROWED.contains(&(def.kind, name.as_str(), key)) {
                    continue;
                }
                let value = |v: Json| match items {
                    Some(_) => json!([v.clone(), v.clone(), v]),
                    None => v,
                };
                let (inside, past) = if integer {
                    let b = bound as i64;
                    (json!(b), json!(if below { b - 1 } else { b + 1 }))
                } else {
                    let b = bound as f32;
                    (json!(b), json!(outside(b, below)))
                };
                let with = |v: Json| json!({ name.as_str(): value(v) });
                assert!(
                    def.check(&with(inside.clone())).is_ok(),
                    "{}.{name} refuses its {key} {inside}: {:?}",
                    def.kind,
                    def.check(&with(inside.clone()))
                );
                assert!(
                    def.check(&with(past.clone())).is_err(),
                    "{}.{name} accepts {past}, past its {key}",
                    def.kind
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 40, "{checked}");
}
