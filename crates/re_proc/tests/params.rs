//! Graph params (ADR-0061 §2 and §3): declarations, values and bindings
//! (`RE4200`–`RE4204`).

use re_proc::NodeRegistry;
use re_proc::params::{
    Binding, apply_values, bind, bindings, check_bound_nodes, check_doc, check_value, valid_name,
};
use re_types::graph::{GraphDoc, ParamDoc, ParamType};
use serde_json::{Map, Value as Json, json};

fn decl(ty: ParamType) -> ParamDoc {
    ParamDoc {
        name: "p".into(),
        ty,
        default: None,
        min: None,
        max: None,
        values: Vec::new(),
        extensions: Vec::new(),
        doc: String::new(),
    }
}

/// A version 2 graph with `params` and one `terrain.heightfield` node `h`
/// whose params are `h_params`.
fn doc(params: Json, h_params: Json) -> GraphDoc {
    let text = json!({
        "format": "real-engine/graph", "version": 2, "name": "island", "seed": 1,
        "params": params,
        "nodes": [ { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": h_params } ],
        "edges": []
    });
    GraphDoc::from_json(&text.to_string()).unwrap()
}

fn codes(ds: &[re_core::Diagnostic]) -> Vec<String> {
    ds.iter().map(|d| d.code.to_string()).collect()
}

#[test]
fn param_names() {
    for ok in ["height", "a", "a_1", "Height2"] {
        assert!(valid_name(ok), "{ok}");
    }
    for bad in ["", "1a", "_a", "a-b", "a b", "a.b", "é"] {
        assert!(!valid_name(bad), "{bad}");
    }
    assert!(valid_name(&"a".repeat(64)));
    assert!(!valid_name(&"a".repeat(65)));
}

#[test]
fn values_are_checked_by_type() {
    let mut number = decl(ParamType::Number);
    number.min = Some(0.0);
    number.max = Some(400.0);
    assert_eq!(check_value(&number, &json!(12.5)).unwrap(), json!(12.5));
    assert_eq!(check_value(&number, &json!(400)).unwrap(), json!(400));
    assert!(
        check_value(&number, &json!(-1))
            .unwrap_err()
            .contains("outside 0 to 400")
    );
    assert!(check_value(&number, &json!("1")).is_err());

    // Whole numbers; a zero fraction is accepted and written as an integer.
    let mut integer = decl(ParamType::Integer);
    integer.min = Some(1.0);
    assert_eq!(check_value(&integer, &json!(4.0)).unwrap(), json!(4));
    assert_eq!(check_value(&integer, &json!(4)).unwrap(), json!(4));
    assert!(check_value(&integer, &json!(4.5)).is_err());
    assert!(
        check_value(&integer, &json!(0))
            .unwrap_err()
            .contains("below the minimum 1")
    );
    integer.min = None;
    assert!(check_value(&integer, &json!(9_007_199_254_740_992_i64)).is_ok());
    assert!(check_value(&integer, &json!(9_007_199_254_740_993_i64)).is_err());
    assert!(check_value(&integer, &json!(i64::MIN)).is_err());
    assert!(check_value(&integer, &json!(u64::MAX)).is_err());

    assert!(check_value(&decl(ParamType::Bool), &json!(true)).is_ok());
    assert!(check_value(&decl(ParamType::Bool), &json!(1)).is_err());
    assert!(check_value(&decl(ParamType::Str), &json!("")).is_ok());
    assert!(check_value(&decl(ParamType::Str), &json!(null)).is_err());

    let mut choice = decl(ParamType::Enum);
    choice.values = vec!["fbm".into(), "ridged".into()];
    assert!(check_value(&choice, &json!("ridged")).is_ok());
    let e = check_value(&choice, &json!("warped")).unwrap_err();
    assert!(e.contains("`fbm`, `ridged`"), "{e}");

    assert!(check_value(&decl(ParamType::Vec2), &json!([1, 2.5])).is_ok());
    assert!(check_value(&decl(ParamType::Vec2), &json!([1, 2, 3])).is_err());
    assert!(check_value(&decl(ParamType::Vec3), &json!([1, 2, 3])).is_ok());
    assert!(check_value(&decl(ParamType::Vec3), &json!([1, "2", 3])).is_err());
    assert!(check_value(&decl(ParamType::Color), &json!([1, 0.5, 0])).is_ok());
    assert!(check_value(&decl(ParamType::Color), &json!([1, 0.5, 0, 0.5])).is_ok());
    assert!(check_value(&decl(ParamType::Color), &json!([1, 0.5])).is_err());

    let mut file = decl(ParamType::File);
    file.extensions = vec![".prefab.json".into()];
    assert!(check_value(&file, &json!("")).is_ok());
    assert!(check_value(&file, &json!("prefabs/tree.prefab.json")).is_ok());
    assert!(
        check_value(&file, &json!("prefabs/tree.json"))
            .unwrap_err()
            .contains("`.prefab.json`")
    );
    for bad in [
        "../tree.prefab.json",
        "/tree.prefab.json",
        "a//tree.prefab.json",
        "a\\tree.prefab.json",
    ] {
        assert!(check_value(&file, &json!(bad)).is_err(), "{bad}");
    }

    let curve = decl(ParamType::Curve);
    assert!(check_value(&curve, &json!([[0, 0], [1, 2]])).is_ok());
    assert!(check_value(&curve, &json!([[0, 0]])).is_err());
    let e = check_value(&curve, &json!([[0, 0], [1, 2], [1, 3]])).unwrap_err();
    assert!(e.contains("increase strictly"), "{e}");
    let long: Vec<Json> = (0..257).map(|i| json!([i, 0])).collect();
    assert!(check_value(&curve, &Json::Array(long[..256].to_vec())).is_ok());
    assert!(check_value(&curve, &Json::Array(long)).is_err());
}

#[test]
fn declarations_are_checked() {
    let bad = |params: Json| codes(&check_doc(&doc(params, json!({}))));
    assert!(bad(json!([{ "name": "height", "type": "number", "default": 4 }])).is_empty());
    for params in [
        json!([{ "name": "1height", "type": "number", "default": 4 }]),
        json!([{ "name": "a", "type": "number", "default": 4 }, { "name": "a", "type": "bool", "default": true }]),
        json!([{ "name": "a", "type": "number", "default": 4, "min": 5, "max": 1 }]),
        json!([{ "name": "a", "type": "string", "default": "x", "min": 1 }]),
        json!([{ "name": "a", "type": "enum", "default": "x", "values": [] }]),
        json!([{ "name": "a", "type": "enum", "default": "x", "values": ["x", "x"] }]),
        json!([{ "name": "a", "type": "bool", "default": true, "values": ["x"] }]),
        json!([{ "name": "a", "type": "bool", "default": true, "extensions": [".json"] }]),
        json!([{ "name": "a", "type": "file", "default": "", "extensions": ["json"] }]),
        json!([{ "name": "a", "type": "number", "default": 500, "max": 400 }]),
        json!([{ "name": "a", "type": "number" }]),
    ] {
        let found = bad(params.clone());
        assert!(
            !found.is_empty() && found.iter().all(|c| c == "RE4200"),
            "{params}: {found:?}"
        );
    }
    // A node kind's params may leave out their default.
    let mut kind = doc(json!([{ "name": "a", "type": "number" }]), json!({}));
    kind.kind =
        Some(serde_json::from_value(json!({ "name": "project.thing", "version": 1 })).unwrap());
    assert!(check_doc(&kind).is_empty());
}

#[test]
fn references_are_checked() {
    let params = json!([{ "name": "height", "type": "number", "default": 40 }]);
    let ds = check_doc(&doc(
        params.clone(),
        json!({ "height": { "$param": "hieght" } }),
    ));
    assert_eq!(codes(&ds), ["RE4201"]);
    assert!(
        ds[0].message.contains("did you mean `height`?"),
        "{}",
        ds[0].message
    );
    // `"$param"` anywhere but as a whole value is refused.
    for misplaced in [
        json!({ "offset": [{ "$param": "height" }, 0] }),
        json!({ "height": { "$param": 5 } }),
        json!({ "height": { "$param": "height", "and": 1 } }),
    ] {
        assert_eq!(
            codes(&check_doc(&doc(params.clone(), misplaced.clone()))),
            ["RE4204"],
            "{misplaced}"
        );
    }
    assert!(check_doc(&doc(params, json!({ "height": { "$param": "height" } }))).is_empty());
    // A version 1 document's node params are literals, whatever keys they hold.
    let mut v1 = GraphDoc::new("old", 1);
    v1.nodes.push(
        serde_json::from_value(json!({
            "id": "h", "kind": "terrain.heightfield", "version": 1,
            "params": { "offset": [{ "$param": "x" }, 0] }
        }))
        .unwrap(),
    );
    assert!(check_doc(&v1).is_empty());
}

#[test]
fn binding_writes_each_params_value() {
    let params = json!([
        { "name": "height", "type": "number", "default": 40, "min": 0, "max": 400 },
        { "name": "octaves", "type": "integer", "default": 5.0, "min": 1, "max": 8 }
    ]);
    let d = doc(
        params,
        json!({ "height": { "$param": "height" }, "octaves": { "$param": "octaves" }, "size": 33 }),
    );
    assert_eq!(
        bindings(&d),
        [
            Binding {
                node: "h".into(),
                param: "height".into(),
                graph_param: "height".into()
            },
            Binding {
                node: "h".into(),
                param: "octaves".into(),
                graph_param: "octaves".into()
            },
        ]
    );
    let bound = bind(&d).unwrap();
    let h = bound.node("h").unwrap();
    assert_eq!(h.params["height"], json!(40));
    // An integer's default is written as a whole number.
    assert_eq!(h.params["octaves"], json!(5));
    assert_eq!(h.params["size"], json!(33));
    assert!(bindings(&bound).is_empty());
    // The declarations stay.
    assert_eq!(bound.params.len(), 2);

    // A source's values replace the defaults; others are kept.
    let values: Map<String, Json> =
        serde_json::from_value(json!({ "height": 120, "octaves": null })).unwrap();
    let (with, warnings) = apply_values(&d, &values, "island");
    assert!(warnings.is_empty());
    let h = bind(&with).unwrap().node("h").unwrap().clone();
    assert_eq!(h.params["height"], json!(120));
    assert_eq!(h.params["octaves"], json!(5));

    // A value that no longer fits, or names no param, is skipped with a warning.
    let values: Map<String, Json> =
        serde_json::from_value(json!({ "height": 900, "roughness": 1 })).unwrap();
    let (with, warnings) = apply_values(&d, &values, "island");
    assert_eq!(codes(&warnings), ["RE4202", "RE4201"]);
    assert!(
        warnings
            .iter()
            .all(|w| w.severity == re_core::Severity::Warning)
    );
    assert_eq!(
        bind(&with).unwrap().node("h").unwrap().params["height"],
        json!(40)
    );
}

#[test]
fn a_bound_param_without_a_value_is_refused() {
    let mut d = doc(
        json!([{ "name": "height", "type": "number" }]),
        json!({ "height": { "$param": "height" } }),
    );
    d.kind =
        Some(serde_json::from_value(json!({ "name": "project.thing", "version": 1 })).unwrap());
    assert_eq!(codes(&bind(&d).unwrap_err()), ["RE4202"]);
    let values: Map<String, Json> = serde_json::from_value(json!({ "height": 3 })).unwrap();
    let (with, _) = apply_values(&d, &values, "preview");
    assert_eq!(
        bind(&with).unwrap().node("h").unwrap().params["height"],
        json!(3)
    );
}

#[test]
fn bound_nodes_accept_every_offered_value() {
    let registry = NodeRegistry::m1();
    let check = |params: Json, h: Json| codes(&check_bound_nodes(&doc(params, h), &registry));
    // `octaves` takes 1 to 16.
    assert!(
        check(
            json!([{ "name": "detail", "type": "integer", "default": 4, "min": 1, "max": 16 }]),
            json!({ "octaves": { "$param": "detail" } })
        )
        .is_empty()
    );
    let ds = check_bound_nodes(
        &doc(
            json!([{ "name": "detail", "type": "integer", "default": 4, "min": 0, "max": 20 }]),
            json!({ "octaves": { "$param": "detail" } }),
        ),
        &registry,
    );
    assert_eq!(codes(&ds), ["RE4203", "RE4203"]);
    assert!(
        ds[0]
            .message
            .contains("allows 0 to 20, but `h.octaves` (terrain.heightfield) refuses 0"),
        "{}",
        ds[0].message
    );
    // A `number` bound to a whole-number param is refused at its bounds.
    assert_eq!(
        check(
            json!([{ "name": "detail", "type": "number", "default": 4, "min": 2, "max": 8 }]),
            json!({ "octaves": { "$param": "detail" } })
        ),
        ["RE4203", "RE4203"]
    );
    // Every enum value is tried.
    assert_eq!(
        check(
            json!([{ "name": "look", "type": "enum", "default": "fbm", "values": ["fbm", "ridged", "billowy"] }]),
            json!({ "noise": { "$param": "look" } })
        ),
        ["RE4203"]
    );
    // The default is the node's own value, checked as any value is (RE4005), not here.
    assert!(
        check(
            json!([{ "name": "detail", "type": "integer", "default": 40 }]),
            json!({ "octaves": { "$param": "detail" } })
        )
        .is_empty()
    );
    // A node kind's node with a bound param that has no value yet is checked
    // once an instance gives one.
    let mut kind = doc(
        json!([
            { "name": "height", "type": "number" },
            { "name": "detail", "type": "integer", "default": 4, "min": 0, "max": 8 }
        ]),
        json!({ "height": { "$param": "height" }, "octaves": { "$param": "detail" } }),
    );
    kind.kind =
        Some(serde_json::from_value(json!({ "name": "project.thing", "version": 1 })).unwrap());
    assert!(check_bound_nodes(&kind, &registry).is_empty());
    let values: Map<String, Json> = serde_json::from_value(json!({ "height": 3 })).unwrap();
    let (with, _) = apply_values(&kind, &values, "preview");
    assert_eq!(codes(&check_bound_nodes(&with, &registry)), ["RE4203"]);
}
