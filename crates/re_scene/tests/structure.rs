//! Structural overrides in scene files and in regeneration (ADR-0043 §3).

use std::sync::Arc;

use re_reflect::{Value, component};
use re_scene::ecs as bevy_ecs;
use re_scene::{GenNode, GeneratorInfo, OverrideDoc, World, hierarchy};
use serde_json::json;

component! {
    /// The test generator's parameters.
    pub struct Keys {
        /// Keys to generate, `parent:key` or `key`, comma-separated.
        pub list: String = String::new(),
    }
}

/// A world with a `keys` generator and a source `src` generating `list`,
/// with `overrides` on it.
fn world(list: &str, overrides: serde_json::Value) -> World {
    let mut w = World::new();
    w.registry.register::<Keys>(&mut w.ecs);
    w.generators.register(GeneratorInfo {
        name: "keys".into(),
        doc: String::new(),
        params_component: "Keys".into(),
        run: Arc::new(|ctx| {
            let list: String = ctx.param("list")?;
            Ok(list
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let (parent, key) = match s.split_once(':') {
                        Some((p, k)) => (Some(p.to_string()), k.to_string()),
                        None => (None, s.to_string()),
                    };
                    GenNode {
                        name: key.clone(),
                        key,
                        parent_key: parent,
                        components: vec![("Transform".into(), vec![])],
                    }
                })
                .collect())
        }),
        fingerprint_extra: None,
        version: 1,
        oldest: 1,
    });
    let scene = json!({
        "format": "real-engine/scene",
        "version": 1,
        "entities": [{
            "id": "src",
            "components": {
                "Procedural": { "generator": "keys" },
                "Keys": { "list": list },
                "Transform": {}
            },
            "overrides": overrides
        }]
    });
    w.load(&re_scene::scene::from_json(&scene.to_string()).unwrap())
        .unwrap();
    w
}

fn children(w: &World, id: &str) -> Vec<String> {
    let e = w.entity(id).unwrap();
    hierarchy::children(&w.ecs, e)
        .iter()
        .map(|c| w.id_of(*c).unwrap().to_string())
        .collect()
}

#[test]
fn every_override_form_reads_and_writes_back_unchanged() {
    let forms = json!([
        { "target": "a", "component": "Transform", "field": "scale", "value": [2, 2, 2] },
        { "target": "b", "deleted": true },
        { "target": "c", "moved": { "parent": "a", "index": 0 } },
        { "target": "d", "moved": { "parent": null } },
        { "target": "e", "replaced": { "prefab": "prefabs/rock.prefab.json" } },
        { "target": "f", "replaced": { "components": { "Transform": {} }, "keep_children": true } },
        { "insert": "lamp", "parent": "a", "index": 2, "name": "Lamp", "components": {} },
        { "insert": "rock", "prefab": "prefabs/rock.prefab.json" }
    ]);
    let docs: Vec<OverrideDoc> = serde_json::from_value(forms.clone()).unwrap();
    assert_eq!(docs[6].target(), "+lamp");
    assert_eq!(serde_json::to_value(&docs).unwrap(), forms);
    for bad in [
        json!({ "target": "a", "moved": {}, "deleted": true }),
        json!({ "target": "a", "replaced": {} }),
        json!({ "target": "a", "replaced": { "prefab": "p", "components": {} } }),
        json!({ "insert": "lamp" }),
        json!({ "insert": "+lamp", "components": {} }),
        json!({ "insert": "a/b", "components": {} }),
        json!({ "insert": "lamp", "target": "x", "components": {} }),
        json!({ "target": "a", "parent": "b" }),
        json!({ "parent": "b", "components": {} }),
    ] {
        assert!(
            serde_json::from_value::<OverrideDoc>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn structure_applies_before_deletions_and_field_overrides() {
    let mut w = world(
        "a,a:x,b,c",
        json!([
            { "target": "c", "moved": { "parent": "x", "index": 0 } },
            { "insert": "lamp", "parent": "c", "components": { "Transform": {} } },
            { "target": "+lamp", "component": "Transform", "field": "scale", "value": [3, 3, 3] },
            { "target": "b", "replaced": { "components": {} } },
            { "target": "b", "component": "Transform", "field": "scale", "value": [2, 2, 2] }
        ]),
    );
    let report = w.regenerate_changed();
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    assert!(report.orphans.is_empty(), "{:#?}", report.orphans);
    assert_eq!(children(&w, "src/x"), ["src/c"]);
    assert_eq!(children(&w, "src/c"), ["src/+lamp"]);
    assert_eq!(
        w.get("src/+lamp", "Transform", "scale").unwrap(),
        Value::Vec3([3.0, 3.0, 3.0].into())
    );
    // A replacement without a Transform keeps the generated one, so a field
    // override of it still applies.
    assert_eq!(
        w.get("src/b", "Transform", "scale").unwrap(),
        Value::Vec3([2.0, 2.0, 2.0].into())
    );
    // Deleting the moved entity's new parent takes it along.
    let mut w = world(
        "a,a:x,b,c",
        json!([
            { "target": "c", "moved": { "parent": "x" } },
            { "target": "a", "deleted": true }
        ]),
    );
    w.regenerate_changed();
    assert!(w.entity("src/c").is_err());
    assert_eq!(children(&w, "src"), ["src/b"]);
}

#[test]
fn a_move_into_its_own_subtree_is_left_in_place_with_a_warning() {
    let mut w = world(
        "a,a:x",
        json!([{ "target": "a", "moved": { "parent": "x" } }]),
    );
    let report = w.regenerate_changed();
    assert_eq!(report.diagnostics.len(), 1, "{:#?}", report.diagnostics);
    assert_eq!(report.diagnostics[0].code.to_string(), "RE1209");
    assert!(report.diagnostics[0].message.contains("cycle"));
    assert_eq!(children(&w, "src"), ["src/a"]);
    assert_eq!(children(&w, "src/a"), ["src/x"]);
}

#[test]
fn a_prefab_without_project_files_is_not_applied() {
    let mut w = world(
        "a",
        json!([{ "insert": "rock", "prefab": "prefabs/rock.prefab.json" }]),
    );
    let report = w.regenerate_changed();
    assert_eq!(report.diagnostics.len(), 1, "{:#?}", report.diagnostics);
    assert_eq!(report.diagnostics[0].code.to_string(), "RE1209");
    assert_eq!(children(&w, "src"), ["src/a"]);
}

#[test]
fn generators_may_not_use_the_structural_key_prefixes() {
    for list in ["+a", "a,a:a/~b"] {
        let mut w = world(list, json!([]));
        let report = w.regenerate_changed();
        assert_eq!(report.diagnostics.len(), 1, "{list}");
        assert_eq!(report.diagnostics[0].code.to_string(), "RE1207");
        assert!(w.entity("src/a").is_err() || list == "+a");
    }
}

#[test]
fn changing_structure_regenerates_and_field_overrides_do_not() {
    let mut w = world("a,b", json!([]));
    w.regenerate_changed();
    let src = w.entity("src").unwrap();
    let mut ov = w
        .ecs
        .get::<re_scene::Overrides>(src)
        .cloned()
        .unwrap_or_default();
    ov.set("a", "Transform", "scale", Some(re_reflect::Doc::Null));
    w.ecs.entity_mut(src).insert(ov.clone());
    assert!(w.regenerate_changed().is_empty());
    let doc: OverrideDoc =
        serde_json::from_value(json!({ "target": "b", "moved": { "parent": "a" } })).unwrap();
    ov.set_structural_at(
        "b",
        re_scene::structure::StructuralKind::Moved,
        Some(doc),
        None,
    );
    w.ecs.entity_mut(src).insert(ov);
    assert!(!w.regenerate_changed().is_empty());
    assert_eq!(children(&w, "src/a"), ["src/b"]);
}

#[test]
fn a_structural_override_never_hides_a_broken_generator_output() {
    let moved = json!([{ "target": "b", "moved": { "parent": null } }]);
    // A duplicate key, a child before its parent, and a parent never
    // generated: each fails with or without the override.
    for list in ["a,b,a", "a:b,a", "x:b"] {
        for overrides in [json!([]), moved.clone()] {
            let mut w = world(list, overrides.clone());
            let report = w.regenerate_changed();
            assert_eq!(report.diagnostics.len(), 1, "{list} {overrides}");
            assert_eq!(
                report.diagnostics[0].code.to_string(),
                "RE1207",
                "{list} {overrides}"
            );
        }
    }
}

#[test]
fn a_replacement_keeps_inserts_under_it_and_reports_those_it_drops() {
    let mut w = world(
        "a,a:x",
        json!([
            { "insert": "lamp", "parent": "x", "components": {} },
            { "target": "a", "replaced": { "components": {} } }
        ]),
    );
    let report = w.regenerate_changed();
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    let orphans: Vec<(String, String)> = report
        .orphans
        .iter()
        .map(|o| (o.target.clone(), o.structural.unwrap().to_string()))
        .collect();
    assert_eq!(orphans, [("+lamp".to_string(), "insert".to_string())]);
    assert!(w.entity("src/+lamp").is_err());
    assert!(w.entity("src/x").is_err());

    // An insert directly under the replaced entity is not generated, so it
    // stays.
    let mut w = world(
        "a,a:x",
        json!([
            { "insert": "lamp", "parent": "a", "components": {} },
            { "target": "a", "replaced": { "components": {} } }
        ]),
    );
    let report = w.regenerate_changed();
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    assert!(report.orphans.is_empty(), "{:#?}", report.orphans);
    assert_eq!(children(&w, "src/a"), ["src/+lamp"]);
}
