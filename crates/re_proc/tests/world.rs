//! The `graph` generator through `re_scene`, and the `graph.*` commands.

mod support;

use re_command::{AllowAll, CommandOutcome, CommandRegistry, EditState, Op};
use re_proc::{GraphStore, register_commands};
use re_reflect::Value;
use re_scene::scene::{self, SceneDoc};
use re_scene::{SaveOptions, World};
use serde_json::{Value as Json, json};
use support::{island, world};

fn commands() -> CommandRegistry<EditState> {
    let mut r = CommandRegistry::new();
    register_commands(&mut r);
    r
}

fn call(st: &mut EditState, name: &str, args: Json) -> CommandOutcome {
    commands().dispatch(st, name, args, false, &AllowAll)
}

fn codes(o: &CommandOutcome) -> Vec<String> {
    o.diagnostics.iter().map(|d| d.code.to_string()).collect()
}

fn asset(w: &World, id: &str) -> String {
    match w.get(id, "MeshRef", "asset").unwrap() {
        Value::Str(s) => s,
        other => panic!("{other:?}"),
    }
}

fn saved(w: &mut World) -> SceneDoc {
    w.save(SaveOptions::default())
}

/// Crate `ix, iz` of the 5 x 5 grid (64 m square, 12 m spacing).
fn crate_key(ix: u32, iz: u32) -> String {
    format!("island/crates/{}", iz * 65_536 + ix)
}

#[test]
fn the_generator_spawns_the_graph_output() {
    let st = world(island(1), json!([]));
    let w = &st.world;
    // Terrain, water, a painted box, 25 crates of 3 entities.
    for id in [
        "island/ground/terrain",
        "island/water/water",
        "island/painted/mesh",
    ] {
        assert!(w.entity(id).is_ok(), "{id}");
    }
    let ground = asset(w, "island/ground/terrain");
    assert!(ground.starts_with("derived:"), "{ground}");
    let store = w.ecs.resource::<GraphStore>();
    assert!(store.mesh(&ground).is_some());
    let collider = match w
        .get("island/ground/terrain", "Collider", "source")
        .unwrap()
    {
        Value::Str(s) => s,
        other => panic!("{other:?}"),
    };
    assert!(store.heightfield(&collider).is_some(), "{collider}");
    assert!(w.entity(&format!("{}/lid", crate_key(4, 4))).is_ok());
    let report = store.report("island").expect("a report");
    assert!(report.ok());
    assert_eq!(report.entities, Some(3 + 25 * 3));
}

#[test]
fn a_new_seed_keeps_surviving_entities_overrides_and_deletions() {
    let kept = crate_key(1, 2);
    let deleted = crate_key(3, 3);
    let overrides = json!([
        { "target": format!("{}/crate", kept.trim_start_matches("island/")), "component": "RigidBody", "field": "mass", "value": 99.0 },
        { "target": deleted.trim_start_matches("island/"), "deleted": true }
    ]);
    let mut st = world(island(1), overrides);
    let body = format!("{kept}/crate");
    assert_eq!(
        st.world.get(&body, "RigidBody", "mass").unwrap(),
        Value::Float(99.0)
    );
    assert!(
        st.world.entity(&deleted).is_err(),
        "the deletion override applies"
    );

    let before_crate = st.world.entity(&body).unwrap();
    let before_ground = st.world.entity("island/ground/terrain").unwrap();
    let old_ground = asset(&st.world, "island/ground/terrain");
    let old_box = asset(&st.world, "island/painted/mesh");
    let old_y = st.world.get(&kept, "Transform", "translation").unwrap();

    let out = call(
        &mut st,
        "graph.regenerate",
        json!({ "source": "island", "seed": 2 }),
    );
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(out.changes.len(), 1, "one set_field on Procedural.seed");
    assert!(
        matches!(&out.changes[0], Op::SetField { component, field, .. } if component == "Procedural" && field == "seed")
    );
    assert!(codes(&out).is_empty(), "{:?}", out.diagnostics);

    // Same entities, new content.
    assert_eq!(st.world.entity(&body).unwrap(), before_crate);
    assert_eq!(
        st.world.entity("island/ground/terrain").unwrap(),
        before_ground
    );
    assert_ne!(
        st.world.get(&kept, "Transform", "translation").unwrap(),
        old_y
    );
    assert_eq!(
        st.world.get(&body, "RigidBody", "mass").unwrap(),
        Value::Float(99.0)
    );
    assert!(st.world.entity(&deleted).is_err());

    // The old terrain mesh is released (and dropped from the store); the
    // unseeded box keeps its mesh.
    let new_ground = asset(&st.world, "island/ground/terrain");
    assert_ne!(new_ground, old_ground);
    assert_eq!(asset(&st.world, "island/painted/mesh"), old_box);
    let released: Vec<String> =
        serde_json::from_value(out.data["regen"]["released_assets"].clone()).unwrap();
    assert_eq!(released, vec![old_ground.clone()]);
    let store = st.world.ecs.resource::<GraphStore>();
    assert!(store.mesh(&old_ground).is_none());
    assert!(store.mesh(&new_ground).is_some());
    assert!(store.mesh(&old_box).is_some());

    // Undo restores the seed; regenerating restores the old terrain.
    st.history.undo(&mut st.world).unwrap();
    st.world.regenerate_changed();
    assert_eq!(asset(&st.world, "island/ground/terrain"), old_ground);
    assert_eq!(
        st.world.get(&kept, "Transform", "translation").unwrap(),
        old_y
    );
}

#[test]
fn released_assets_lists_derived_meshes_no_longer_referenced() {
    let mut st = world(island(1), json!([]));
    let old_box = asset(&st.world, "island/painted/mesh");
    let out = call(
        &mut st,
        "graph.set_param",
        json!({ "source": "island", "node": "box", "param": "size", "value": [3.0, 1.0, 3.0] }),
    );
    assert!(out.ok, "{:#?}", out.diagnostics);
    let report = st.world.regenerate_changed();
    assert_eq!(report.released_assets, vec![old_box.clone()]);
    assert_ne!(asset(&st.world, "island/painted/mesh"), old_box);
}

#[test]
fn set_param_changes_the_output_and_undo_restores_it() {
    let mut st = world(island(4), json!([]));
    let hash0 = st.world.state_hash();
    let box0 = asset(&st.world, "island/painted/mesh");
    let frag0 = st
        .world
        .ecs
        .resource::<GraphStore>()
        .report("island")
        .unwrap()
        .fragment_hash;

    let out = call(
        &mut st,
        "graph.set_param",
        json!({ "source": "island", "node": "box", "param": "size", "value": [4.0, 1.0, 4.0] }),
    );
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(out.changes.len(), 1);
    match &out.changes[0] {
        Op::SetField {
            id,
            component,
            field,
            value,
        } => {
            assert_eq!(
                (id.as_str(), component.as_str(), field.as_str()),
                ("island", "GraphSource", "params")
            );
            assert_eq!(
                *value,
                re_reflect::Doc::Str(r#"{"box":{"size":[4.0,1.0,4.0]}}"#.into())
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(st.history.log().len(), 1);
    // The graph document itself is untouched; the source carries the override.
    let store = st.world.ecs.resource::<GraphStore>();
    assert_eq!(store.graph("island").unwrap(), &island(4));

    st.world.regenerate_changed();
    let box1 = asset(&st.world, "island/painted/mesh");
    assert_ne!(box1, box0);
    let frag1 = st
        .world
        .ecs
        .resource::<GraphStore>()
        .report("island")
        .unwrap()
        .fragment_hash;
    assert_ne!(frag1, frag0);
    assert_ne!(st.world.state_hash(), hash0);

    // Saved with the scene.
    let text = scene::to_json(&saved(&mut st.world));
    assert!(
        text.contains(r#"{\"box\":{\"size\":[4.0,1.0,4.0]}}"#),
        "{text}"
    );

    assert!(st.history.undo(&mut st.world).unwrap());
    st.world.regenerate_changed();
    assert_eq!(asset(&st.world, "island/painted/mesh"), box0);
    assert_eq!(st.world.state_hash(), hash0);
    let frag2 = st
        .world
        .ecs
        .resource::<GraphStore>()
        .report("island")
        .unwrap()
        .fragment_hash;
    assert_eq!(frag2, frag0);

    // A null value clears an override.
    call(
        &mut st,
        "graph.set_param",
        json!({ "source": "island", "node": "box", "param": "size", "value": [2.0, 2.0, 2.0] }),
    );
    let out = call(
        &mut st,
        "graph.set_param",
        json!({ "source": "island", "node": "box", "param": "size", "value": null }),
    );
    assert!(out.ok);
    assert_eq!(
        st.world.get("island", "GraphSource", "params").unwrap(),
        Value::Str(String::new())
    );
}

#[test]
fn set_param_refuses_unknown_targets_and_bad_values() {
    let mut st = world(island(1), json!([]));
    let hash = st.world.state_hash();
    for (args, code) in [
        (
            json!({ "source": "island", "node": "bx", "param": "size", "value": 1 }),
            "RE4011",
        ),
        (
            json!({ "source": "island", "node": "box", "param": "sise", "value": 1 }),
            "RE4011",
        ),
        (
            json!({ "source": "island", "node": "box", "param": "size", "value": [-1.0, 1.0, 1.0] }),
            "RE4005",
        ),
        (
            json!({ "source": "world", "node": "box", "param": "size", "value": 1 }),
            "RE4017",
        ),
    ] {
        let out = call(&mut st, "graph.set_param", args.clone());
        assert!(!out.ok, "{args}");
        assert_eq!(codes(&out)[0], code, "{args}: {:#?}", out.diagnostics);
    }
    assert_eq!(st.world.state_hash(), hash);
    assert!(st.history.log().is_empty());
}

#[test]
fn an_orphaned_override_gives_re4010_and_stays_in_the_file() {
    // Crate (4, 0) only exists while the grid has 5 columns.
    let target = format!("{}/crate", crate_key(4, 0).trim_start_matches("island/"));
    let overrides =
        json!([{ "target": target, "component": "RigidBody", "field": "mass", "value": 5.0 }]);
    let mut st = world(island(1), overrides);
    assert!(
        call(
            &mut st,
            "graph.set_param",
            json!({ "source": "island", "node": "grid", "param": "spacing", "value": 30.0 })
        )
        .ok
    );
    let out = call(&mut st, "graph.regenerate", json!({ "source": "island" }));
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(codes(&out), ["RE4010"], "{:#?}", out.diagnostics);
    assert!(
        out.diagnostics[0].message.contains(&target),
        "{}",
        out.diagnostics[0].message
    );
    assert_eq!(out.data["regen"]["orphans"].as_array().unwrap().len(), 1);
    assert!(st.world.entity(&crate_key(4, 0)).is_err());

    let doc = saved(&mut st.world);
    let source = doc.entities.iter().find(|e| e.id == "island").unwrap();
    assert_eq!(source.overrides.len(), 1, "the override is kept");
    assert_eq!(source.overrides[0].target(), target);
}

#[test]
fn a_dry_run_regenerate_changes_nothing() {
    let mut st = world(island(1), json!([]));
    let hash = st.world.state_hash();
    let out = commands().dispatch(
        &mut st,
        "graph.regenerate",
        json!({ "source": "island", "seed": 9 }),
        true,
        &AllowAll,
    );
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(out.data["dry_run"], json!(true));
    assert!(
        out.data["regen"]["updated"].as_u64().unwrap_or(0) > 0
            || out.data["regen"]["updated"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
        "{}",
        out.data
    );
    assert_eq!(st.world.state_hash(), hash);
    assert!(st.history.log().is_empty());
}

#[test]
fn generate_reports_without_changing_the_world() {
    let mut st = world(island(1), json!([]));
    let hash = st.world.state_hash();
    let report0 = st
        .world
        .ecs
        .resource::<GraphStore>()
        .report("island")
        .unwrap();

    let out = call(
        &mut st,
        "graph.generate",
        json!({ "source": "island", "seed": 5, "params": { "grid": { "spacing": 30.0 } } }),
    );
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(out.data["entities"], json!(3 + 4 * 3));
    assert!(out.data["fragment_hash"].is_string());
    assert!(out.data["eval"]["nodes"].as_array().unwrap().len() == 9);
    assert!(out.changes.is_empty());
    assert_eq!(st.world.state_hash(), hash);
    assert_eq!(
        st.world
            .ecs
            .resource::<GraphStore>()
            .report("island")
            .unwrap(),
        report0
    );
    assert!(st.history.log().is_empty());

    // The same seed and parameters as the world give the world's hash.
    let out = call(&mut st, "graph.generate", json!({ "source": "island" }));
    assert_eq!(out.data["fragment_hash"], json!(report0.fragment_hash));

    // Budgets.
    let out = call(
        &mut st,
        "graph.generate",
        json!({ "source": "island", "max_entities": 10 }),
    );
    assert!(!out.ok);
    assert_eq!(codes(&out)[0], "RE4016");
    let out = call(
        &mut st,
        "graph.generate",
        json!({ "source": "island", "params": { "box": { "shape": "pyramid" } } }),
    );
    assert_eq!(codes(&out)[0], "RE4005", "{:#?}", out.diagnostics);
    assert_eq!(st.world.state_hash(), hash);
}

#[test]
fn report_returns_the_last_evaluation() {
    let mut st = world(island(1), json!([]));
    let out = call(&mut st, "graph.report", json!({ "source": "island" }));
    assert!(out.ok);
    assert_eq!(out.data["evaluated"], json!(true));
    assert_eq!(out.data["report"]["graph"], json!("test"));
    assert_eq!(out.data["report"]["nodes"].as_array().unwrap().len(), 9);
    let out = call(&mut st, "graph.report", json!({ "source": "nowhere" }));
    assert!(!out.ok);
}

#[test]
fn a_source_without_a_graph_fails_with_re4009() {
    let mut st = world(island(1), json!([]));
    let kept = st.world.entity("island/ground/terrain").unwrap();
    st.world.ecs.resource_mut::<GraphStore>().remove("island");
    let out = call(
        &mut st,
        "graph.regenerate",
        json!({ "source": "island", "seed": 3 }),
    );
    assert!(!out.ok);
    assert!(
        codes(&out).contains(&"RE4009".to_string()),
        "{:#?}",
        out.diagnostics
    );
    // A failing generator keeps the old output.
    assert_eq!(st.world.entity("island/ground/terrain").unwrap(), kept);
}

#[test]
fn every_command_is_registered_with_a_schema() {
    let r = commands();
    for name in [
        "graph.set_param",
        "graph.regenerate",
        "graph.generate",
        "graph.report",
    ] {
        let spec = r.spec(name).unwrap_or_else(|| panic!("{name}"));
        assert!(
            serde_json::to_value(spec).unwrap()["input_schema"].is_object(),
            "{name}"
        );
    }
}
