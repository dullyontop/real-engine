//! `re_command` tests: the ported core spike tests that need transactions and
//! undo, and the entity core PR's new requirements (deletion overrides by
//! despawn, dry runs, `scene.apply` atomicity, the remote channel).

#[path = "../../re_scene/tests/support/mod.rs"]
mod support;

use indexmap::IndexMap;
use re_command::{
    AllowAll, CommandRegistry, EditState, EditTarget, History, LogKind, Op, Origin, Transaction,
    register_builtin_commands,
};
use re_reflect::{Doc, Value};
use re_scene::hierarchy::children;
use re_scene::{GlobalTransform, OverrideDoc, Overrides, World};
use serde_json::json;
use support::*;

fn tx(ops: Vec<Op>) -> Transaction {
    Transaction::new("test", Origin::Agent("test-agent".into()), ops)
}

fn spawn_op(doc: re_scene::EntityDoc) -> Op {
    Op::Spawn {
        id: Some(doc.id),
        name: doc.name,
        parent: doc.parent,
        index: None,
        components: doc.components,
    }
}

fn set(id: &str, component: &str, field: &str, value: Doc) -> Op {
    Op::SetField {
        id: id.into(),
        component: component.into(),
        field: field.into(),
        value,
    }
}

/// The spike's sample scene, built through one transaction.
fn sample() -> (World, History) {
    let mut w = world();
    let mut h = History::default();
    h.execute(
        &mut w,
        tx(sample_docs().into_iter().map(spawn_op).collect()),
    )
    .unwrap();
    (w, h)
}

fn state() -> EditState {
    let (world, history) = sample();
    EditState { world, history }
}

fn registry() -> CommandRegistry<EditState> {
    let mut r = CommandRegistry::new();
    register_builtin_commands(&mut r);
    r
}

#[test]
fn undo_redo_restores_identical_scenes() {
    let (mut w, mut h) = sample();
    let s0 = scene_json(&mut w);
    let edits = vec![
        tx(vec![set("lamp", "PointLight", "intensity", Doc::Int(1500))]),
        tx(vec![
            Op::Rename {
                id: "crate".into(),
                name: "Box".into(),
            },
            Op::RemoveComponent {
                id: "crate".into(),
                component: "RigidBody".into(),
            },
        ]),
        tx(vec![Op::Despawn { id: "crate".into() }]),
        tx(vec![Op::Spawn {
            id: None,
            name: "anon".into(),
            parent: Some("world".into()),
            index: Some(1),
            components: IndexMap::new(),
        }]),
    ];
    let mut snapshots = vec![s0.clone()];
    for e in edits {
        h.execute(&mut w, e).unwrap();
        snapshots.push(scene_json(&mut w));
    }
    // The despawned crate's reference from the weld is cleared, not dangling.
    assert_eq!(w.get_doc("weld", "Weld", "other").unwrap(), Doc::Null);
    for want in snapshots.iter().rev().skip(1) {
        assert!(h.undo(&mut w).unwrap());
        assert_eq!(&scene_json(&mut w), want);
    }
    assert_eq!(
        h.undo_label(),
        Some("test"),
        "only the sample transaction is left"
    );
    for want in snapshots.iter().skip(1) {
        assert!(h.redo(&mut w).unwrap());
        assert_eq!(
            &scene_json(&mut w),
            want,
            "redo must recreate generated ids too"
        );
    }
    assert!(!h.can_redo());
    // The weld's entity reference to the despawned crate is restored by undo.
    h.undo(&mut w).unwrap();
    h.undo(&mut w).unwrap();
    assert_eq!(
        w.get_doc("weld", "Weld", "other").unwrap(),
        Doc::Str("crate".into())
    );
    let crate_e = w.entity("crate").unwrap();
    assert_eq!(
        w.get("weld", "Weld", "other").unwrap(),
        Value::Entity(Some(crate_e))
    );
    assert_eq!(scene_json(&mut w), snapshots[2]);

    // Reparent order and its undo.
    h.execute(
        &mut w,
        tx(vec![Op::Reparent {
            id: "weld".into(),
            parent: Some("world".into()),
            index: Some(0),
        }]),
    )
    .unwrap();
    assert_eq!(ids_under(&w, "world"), ["weld", "ground", "crate"]);
    h.undo(&mut w).unwrap();
    assert_eq!(ids_under(&w, "world"), ["ground", "crate", "weld"]);
}

#[test]
fn failed_transaction_rolls_back() {
    let (mut w, mut h) = sample();
    let before = scene_json(&mut w);
    let err = h
        .execute(
            &mut w,
            tx(vec![
                set("crate", "Transform", "scale", v3(2.0, 2.0, 2.0)),
                Op::Despawn { id: "lamp".into() },
                spawn_op(entity("new", Some("world"), &[])),
                Op::Despawn { id: "nope".into() },
            ]),
        )
        .unwrap_err();
    assert_eq!(err.index, 3);
    assert_eq!(err.to_string(), r#"command 3: no entity with id "nope""#);
    assert_eq!(scene_json(&mut w), before);
    let last = h.log().last().unwrap();
    assert_eq!(last.kind, LogKind::Rejected);
    assert!(last.error.is_some());
    let d = err.to_diagnostic();
    assert_eq!(d.code.to_string(), "RE2001");
    assert_eq!(d.related[0].code.to_string(), "RE1200");

    // The spike's texts for a cycle, a bad value and a bad variant.
    let err = h
        .execute(
            &mut w,
            tx(vec![Op::Reparent {
                id: "crate".into(),
                parent: Some("lamp".into()),
                index: None,
            }]),
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"command 0: cannot parent "crate" under "lamp": "lamp" is "crate" or one of its descendants"#
    );
    let err = h
        .execute(
            &mut w,
            tx(vec![set(
                "crate",
                "RigidBody",
                "mass",
                Doc::Str("heavy".into()),
            )]),
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "command 0: RigidBody.mass: expected f32, found string \"heavy\""
    );
    let err = h
        .execute(
            &mut w,
            tx(vec![set(
                "crate",
                "RigidBody",
                "kind",
                Doc::Str("Floaty".into()),
            )]),
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "command 0: RigidBody.kind: unknown variant \"Floaty\", expected one of Static, Dynamic, Kinematic"
    );
    let err = h
        .execute(
            &mut w,
            tx(vec![Op::AddComponent {
                id: "crate".into(),
                component: "RigidBody".into(),
                fields: IndexMap::new(),
            }]),
        )
        .unwrap_err();
    assert_eq!(err.error.code().to_string(), "RE2002");
    assert_eq!(scene_json(&mut w), before);
}

#[test]
fn commands_are_plain_json_for_agents() {
    let (mut w, mut h) = sample();
    let json = r#"[
        {"op": "set_field", "id": "crate", "component": "Transform", "field": "translation", "value": {"x": 1, "y": 2, "z": 3}},
        {"op": "spawn", "name": "Spotlight", "parent": "crate", "components": {"PointLight": {"intensity": 2000}}}
    ]"#;
    let ops: Vec<Op> = serde_json::from_str(json).unwrap();
    let applied = h
        .execute(
            &mut w,
            Transaction::new("agent edit", Origin::Agent("claude".into()), ops),
        )
        .unwrap();
    let Op::Spawn {
        id: Some(new_id), ..
    } = &applied[1]
    else {
        panic!()
    };
    assert_eq!(
        w.get(new_id, "PointLight", "intensity").unwrap(),
        Value::Float(2000.0)
    );
    let log = h.log_since_jsonl(1);
    assert!(log.contains(r#""origin":{"agent":"claude"}"#), "{log}");
    assert!(log.contains(new_id.as_str()));
    assert_eq!(log.lines().count(), 1);

    let bad = r#"{"op": "set_feild", "id": "crate"}"#;
    let err = serde_json::from_str::<Op>(bad).unwrap_err();
    assert!(
        err.to_string().starts_with("unknown variant `set_feild`"),
        "{err}"
    );
}

#[test]
fn procedural_sources_overrides_and_regeneration() {
    let (mut w, mut h) = sample();
    h.execute(&mut w, tx(vec![spawn_op(grid_doc(2, 3))]))
        .unwrap();
    assert!(w.update().warnings().is_empty());
    let grid = w.entity("grid").unwrap();
    assert_eq!(children(&w.ecs, grid).len(), 6);
    let cap = w.entity("grid/pillar_1_2/cap").unwrap();
    assert!(w.ecs.get::<GlobalTransform>(cap).unwrap().0.translation.y > 5.0);

    // Generated entities are not written; the source and its params are.
    let text = scene_json(&mut w);
    assert!(!text.contains("pillar_1_2"));
    assert!(text.contains("\"PillarGrid\""));

    // Editing a generated entity records an override on the source.
    let red = Doc::List(vec![
        Doc::F32(1.0),
        Doc::F32(0.0),
        Doc::F32(0.0),
        Doc::F32(1.0),
    ]);
    h.execute(
        &mut w,
        tx(vec![set(
            "grid/pillar_0_0",
            "Primitive",
            "color",
            red.clone(),
        )]),
    )
    .unwrap();
    let text = scene_json(&mut w);
    assert!(text.contains("\"target\": \"pillar_0_0\""), "{text}");
    // Undo removes the override as well as the colour; redo brings both back.
    h.undo(&mut w).unwrap();
    assert!(!scene_json(&mut w).contains("\"overrides\""));
    assert_ne!(
        w.get_doc("grid/pillar_0_0", "Primitive", "color").unwrap(),
        red
    );
    h.redo(&mut w).unwrap();
    assert_eq!(scene_json(&mut w), text);

    // Changing parameters regenerates; the override survives by key, and so
    // does the Entity of every surviving key.
    let kept = w.entity("grid/pillar_0_0").unwrap();
    let kept_cap = w.entity("grid/pillar_1_2/cap").unwrap();
    h.execute(
        &mut w,
        tx(vec![set("grid", "PillarGrid", "cols", Doc::Int(4))]),
    )
    .unwrap();
    assert!(w.update().warnings().is_empty());
    assert_eq!(children(&w.ecs, grid).len(), 8);
    assert_eq!(
        w.get_doc("grid/pillar_0_0", "Primitive", "color").unwrap(),
        red
    );
    assert_eq!(w.entity("grid/pillar_0_0").unwrap(), kept);
    assert_eq!(w.entity("grid/pillar_1_2/cap").unwrap(), kept_cap);

    // Same key, same seed => same height, regardless of grid size.
    let h0 = w.get("grid/pillar_1_2", "Primitive", "size").unwrap();
    h.execute(
        &mut w,
        tx(vec![set("grid", "PillarGrid", "rows", Doc::Int(3))]),
    )
    .unwrap();
    w.update();
    assert_eq!(w.get("grid/pillar_1_2", "Primitive", "size").unwrap(), h0);

    // Shrinking orphans the override (kept, reported).
    h.execute(
        &mut w,
        tx(vec![set("grid", "PillarGrid", "rows", Doc::Int(0))]),
    )
    .unwrap();
    let warnings = w.update().warnings();
    assert_eq!(
        warnings,
        [
            r#"grid: override of Primitive.color on "pillar_0_0" has no target in the new output; kept"#
        ]
    );
    assert!(scene_json(&mut w).contains("\"target\": \"pillar_0_0\""));

    // Generated entities refuse structural edits, with a hint.
    h.execute(
        &mut w,
        tx(vec![set("grid", "PillarGrid", "rows", Doc::Int(1))]),
    )
    .unwrap();
    w.update();
    let err = h
        .execute(
            &mut w,
            tx(vec![Op::Reparent {
                id: "grid/pillar_0_1".into(),
                parent: None,
                index: None,
            }]),
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"command 0: "grid/pillar_0_1" is generated: move it within its source's output with `proc.move`"#
    );
    assert_eq!(err.error.code().to_string(), "RE2003");
    for op in [
        Op::AddComponent {
            id: "grid/pillar_0_1".into(),
            component: "PointLight".into(),
            fields: IndexMap::new(),
        },
        Op::RemoveComponent {
            id: "grid/pillar_0_1".into(),
            component: "RigidBody".into(),
        },
        spawn_op(entity("child", Some("grid/pillar_0_1"), &[])),
    ] {
        let err = h.execute(&mut w, tx(vec![op])).unwrap_err();
        assert_eq!(err.error.code().to_string(), "RE2003", "{err}");
    }

    // Despawning the source takes its generated output with it; undo
    // restores the source (with its override list) and regeneration rebuilds.
    h.execute(&mut w, tx(vec![Op::Despawn { id: "grid".into() }]))
        .unwrap();
    assert!(w.ids().get("grid/pillar_0_0").is_none());
    h.undo(&mut w).unwrap();
    w.update();
    assert_eq!(children(&w.ecs, w.entity("grid").unwrap()).len(), 4);
    assert_eq!(
        w.get_doc("grid/pillar_0_0", "Primitive", "color").unwrap(),
        red
    );
}

#[test]
fn despawn_of_generated_entity_records_deletion_override() {
    let (mut w, mut h) = sample();
    h.execute(&mut w, tx(vec![spawn_op(grid_doc(1, 3))]))
        .unwrap();
    w.update();
    let before = w.state_hash();
    let text = scene_json(&mut w);

    h.execute(
        &mut w,
        tx(vec![Op::Despawn {
            id: "grid/pillar_0_1".into(),
        }]),
    )
    .unwrap();
    assert!(w.entity("grid/pillar_0_1").is_err());
    assert!(w.entity("grid/pillar_0_1/cap").is_err());
    let grid = w.entity("grid").unwrap();
    assert!(
        w.ecs
            .get::<Overrides>(grid)
            .unwrap()
            .is_deleted("pillar_0_1")
    );
    assert!(scene_json(&mut w).contains("\"deleted\": true"));
    // Regeneration does not bring it back.
    w.regenerate(grid);
    assert!(w.entity("grid/pillar_0_1").is_err());
    assert!(w.entity("grid/pillar_0_0").is_ok());

    // Undo removes the override and restores the entity exactly.
    h.undo(&mut w).unwrap();
    assert!(w.ecs.get::<Overrides>(grid).is_none());
    assert!(w.entity("grid/pillar_0_1/cap").is_ok());
    assert_eq!(scene_json(&mut w), text);
    assert_eq!(w.state_hash(), before);
    w.regenerate(grid);
    assert_eq!(w.state_hash(), before);

    // The deletion override can also be set directly.
    h.execute(
        &mut w,
        tx(vec![Op::SetOverride {
            source: "grid".into(),
            target: "pillar_0_2".into(),
            component: None,
            field: None,
            value: None,
            deleted: Some(true),
            index: None,
            moved: None,
            replaced: None,
            insert: None,
        }]),
    )
    .unwrap();
    w.regenerate(grid);
    assert!(w.entity("grid/pillar_0_2").is_err());
    assert!(matches!(
        &w.ecs.get::<Overrides>(grid).unwrap().0[..],
        [OverrideDoc::Deleted { target }] if target == "pillar_0_2"
    ));
}

#[test]
fn dry_run_matches_real_call_and_changes_nothing() {
    let reg = registry();
    let mut s = state();
    let hash = s.world.state_hash();
    let log_len = s.history.log().len();
    let can_undo = s.history.can_undo();

    let args = json!({
        "label": "edit",
        "ops": [
            {"op": "set_field", "id": "crate", "component": "RigidBody", "field": "mass", "value": 12.5},
            {"op": "spawn", "name": "Spotlight", "parent": "crate", "components": {"PointLight": {}}},
            {"op": "despawn", "id": "lamp"}
        ]
    });
    let dry = reg.dispatch(&mut s, "scene.apply", args.clone(), true, &AllowAll);
    assert!(dry.ok, "{:?}", dry.diagnostics);
    assert_eq!(dry.changes.len(), 3);
    assert_eq!(s.world.state_hash(), hash);
    assert_eq!(s.history.log().len(), log_len);
    assert_eq!(s.history.can_undo(), can_undo);
    assert!(!s.history.can_redo());

    // `"dry_run": true` inside the arguments works the same way.
    let mut flagged = args.clone();
    flagged["dry_run"] = json!(true);
    let dry2 = reg.dispatch(&mut s, "scene.apply", flagged, false, &AllowAll);
    assert_eq!(dry2.changes, dry.changes);
    assert_eq!(s.world.state_hash(), hash);

    // The real call makes exactly those changes (same generated id too).
    let real = reg.dispatch(&mut s, "scene.apply", args, false, &AllowAll);
    assert!(real.ok, "{:?}", real.diagnostics);
    assert_eq!(real.changes, dry.changes);
    assert_ne!(s.world.state_hash(), hash);
    assert_eq!(s.history.log().len(), log_len + 1);

    // A dry undo reports and does nothing.
    let after = s.world.state_hash();
    let dry_undo = reg.dispatch(&mut s, "undo", json!({}), true, &AllowAll);
    assert!(dry_undo.ok);
    assert!(!dry_undo.changes.is_empty());
    assert_eq!(s.world.state_hash(), after);
    let undo = reg.dispatch(&mut s, "undo", json!({}), false, &AllowAll);
    assert_eq!(undo.changes, dry_undo.changes);
    assert_eq!(s.world.state_hash(), hash);
}

#[test]
fn scene_apply_is_atomic() {
    let reg = registry();
    let mut s = state();
    let hash = s.world.state_hash();
    let before = scene_json(&mut s.world);
    let out = reg.dispatch(
        &mut s,
        "scene.apply",
        json!({"ops": [
            {"op": "set_field", "id": "crate", "component": "RigidBody", "field": "mass", "value": 3},
            {"op": "spawn", "id": "extra", "parent": "world"},
            {"op": "set_field", "id": "crate", "component": "RigidBody", "field": "mas", "value": 3}
        ]}),
        false,
        &AllowAll,
    );
    assert!(!out.ok);
    let d = &out.diagnostics[0];
    assert_eq!(d.code.to_string(), "RE2001");
    assert!(d.message.starts_with("command 2: "), "{}", d.message);
    assert_eq!(d.related[0].code.to_string(), "RE1002");
    assert_eq!(s.world.state_hash(), hash);
    assert_eq!(scene_json(&mut s.world), before);
    assert_eq!(s.history.log().last().unwrap().kind, LogKind::Rejected);

    // Bad arguments and unknown commands are diagnostics too.
    let out = reg.dispatch(&mut s, "scene.apply", json!({"opz": []}), false, &AllowAll);
    assert_eq!(out.diagnostics[0].code.to_string(), "RE2005");
    let out = reg.dispatch(&mut s, "entity.sett", json!({}), false, &AllowAll);
    assert_eq!(
        out.diagnostics[0].message,
        r#"unknown command "entity.sett" (did you mean "entity.set"?)"#
    );
    assert_eq!(out.diagnostics[0].code.to_string(), "RE2004");
}

#[test]
fn builtin_commands_read_and_edit() {
    let reg = registry();
    let mut s = state();
    let names: Vec<_> = reg.specs().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "describe",
            "query",
            "entity.get",
            "command.describe",
            "project.status",
            "entity.spawn",
            "entity.set",
            "entity.reparent",
            "entity.delete",
            "component.add",
            "component.remove",
            "scene.apply",
            "undo",
            "redo"
        ]
    );
    for spec in reg.specs() {
        assert_eq!(spec.input_schema["type"], "object", "{}", spec.name);
    }
    let d = reg.dispatch(&mut s, "describe", json!(null), false, &AllowAll);
    assert!(d.ok);
    assert_eq!(d.data["commands"].as_array().unwrap().len(), 14);
    assert!(
        d.data["generators"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["name"] == "pillar_grid")
    );

    let q = reg.dispatch(
        &mut s,
        "query",
        json!({"with": ["PointLight"]}),
        false,
        &AllowAll,
    );
    assert_eq!(q.data["entities"][0]["id"], "lamp");
    let g = reg.dispatch(
        &mut s,
        "entity.get",
        json!({"id": "crate"}),
        false,
        &AllowAll,
    );
    assert_eq!(g.data["children"], json!(["lamp"]));
    assert_eq!(g.data["components"]["RigidBody"]["mass"], json!(20.0));

    let sp = reg.dispatch(
        &mut s,
        "entity.spawn",
        json!({"name": "Box", "parent": "world", "components": {"Transform": {}}}),
        false,
        &AllowAll,
    );
    let id = sp.data["id"].as_str().unwrap().to_string();
    let set = reg.dispatch(
        &mut s,
        "entity.set",
        json!({"id": id, "component": "Transform", "field": "translation", "value": [1, 2, 3]}),
        false,
        &AllowAll,
    );
    assert!(set.ok, "{:?}", set.diagnostics);
    let out = reg.dispatch(
        &mut s,
        "component.add",
        json!({"id": id, "component": "PointLight"}),
        false,
        &AllowAll,
    );
    assert!(out.ok);
    let out = reg.dispatch(&mut s, "entity.delete", json!({"id": id}), false, &AllowAll);
    assert!(out.ok);
    assert!(s.world().entity(&id).is_err());
    let out = reg.dispatch(&mut s, "undo", json!({}), false, &AllowAll);
    assert_eq!(out.data["done"], true);
    assert!(s.world().entity(&id).is_ok());
}

#[test]
fn remote_channel_round_trip() {
    let reg = registry();
    let mut s = state();
    let (handle, receiver) = re_command::channel();
    let agent = handle.with_origin(Origin::Agent("mcp".into()));
    drop(handle);
    let client = std::thread::spawn(move || {
        let specs = agent.list_specs().unwrap();
        let out = agent.call(
            "entity.set",
            json!({"id": "crate", "component": "RigidBody", "field": "mass", "value": 7}),
            false,
        );
        (specs.len(), out)
    });
    receiver.serve(&mut s, &reg, &AllowAll);
    let (n, out) = client.join().unwrap();
    assert_eq!(n, 14);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(out.changes.len(), 1);
    assert_eq!(
        s.world.get("crate", "RigidBody", "mass").unwrap(),
        Value::Float(7.0)
    );
    assert_eq!(
        s.history.log().last().unwrap().origin,
        Origin::Agent("mcp".into())
    );

    // With the engine gone, a call is an RE2006 outcome.
    let (handle, receiver) = re_command::channel();
    drop(receiver);
    let out = handle.call("describe", json!({}), false);
    assert!(!out.ok);
    assert_eq!(out.diagnostics[0].code.to_string(), "RE2006");
}

#[test]
fn diagnostics_catalogue_is_complete() {
    let problems = re_core::validate_catalog(re_command::DIAGNOSTICS, re_command::CODE_RANGE);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(re_command::DIAGNOSTICS.len(), 8);
}

// Regression tests from the entity core PR review.

fn overrides_of(w: &World, source: &str) -> Vec<OverrideDoc> {
    w.ecs
        .get::<Overrides>(w.entity(source).unwrap())
        .map(|o| o.0.clone())
        .unwrap_or_default()
}

#[test]
fn restore_from_json_is_validated() {
    let reg = registry();
    let mut s = state();
    let hash = s.world.state_hash();
    let refused = |s: &mut EditState, ops: serde_json::Value| {
        let out = reg.dispatch(s, "scene.apply", json!({ "ops": ops }), false, &AllowAll);
        assert!(!out.ok, "{ops}");
        out.diagnostics[0].related[0].message.clone()
    };
    // An authored id with `/` (a source id like this made rollback panic).
    let m = refused(
        &mut s,
        json!([{"op": "restore", "entities": [{"id": "lvl/grid", "parent": "world", "components": {
            "Procedural": {"generator": "pillar_grid"}, "PillarGrid": {}}}]}]),
    );
    assert!(m.contains("must not contain '/'"), "{m}");
    // Entities that are not one subtree (the inverse despawns only the root).
    let m = refused(
        &mut s,
        json!([{"op": "restore", "entities": [{"id": "a"}, {"id": "b", "parent": "world"}]}]),
    );
    assert!(m.contains("earlier entity of the list"), "{m}");
    // A generated entry whose source is not procedural (save would drop it).
    let m = refused(
        &mut s,
        json!([{"op": "restore", "entities": [{"id": "crate/zz", "parent": "crate"}],
            "generated": [{"source": "crate", "key": "zz"}]}]),
    );
    assert!(m.contains("not a procedural source"), "{m}");
    // An authored child under a generated entity.
    s.world.spawn_docs(&[grid_doc(1, 1)], None).unwrap();
    s.world.update();
    let hash_grid = s.world.state_hash();
    let m = refused(
        &mut s,
        json!([{"op": "restore", "entities": [{"id": "x", "parent": "grid/pillar_0_0"}]}]),
    );
    assert!(m.contains("authored children"), "{m}");
    assert_eq!(s.world.state_hash(), hash_grid);
    assert_ne!(hash, hash_grid);
}

#[test]
fn rollback_of_a_generated_despawn_restores_it_exactly() {
    let (mut w, mut h) = sample();
    h.execute(&mut w, tx(vec![spawn_op(grid_doc(1, 2))]))
        .unwrap();
    w.update();
    let hash = w.state_hash();
    // The key `pillar_0_0/cap` contains `/`.
    let err = h
        .execute(
            &mut w,
            tx(vec![
                Op::Despawn {
                    id: "grid/pillar_0_0/cap".into(),
                },
                Op::Despawn {
                    id: "missing".into(),
                },
            ]),
        )
        .unwrap_err();
    assert_eq!(err.index, 1);
    assert_eq!(w.state_hash(), hash);
    let cap = w.entity("grid/pillar_0_0/cap").unwrap();
    let g = w.ecs.get::<re_scene::Generated>(cap).unwrap();
    assert_eq!(
        (g.source, g.key.as_str()),
        (w.entity("grid").unwrap(), "pillar_0_0/cap")
    );
    assert!(overrides_of(&w, "grid").is_empty());

    // Undo of a procedural source's despawn keeps its generation state.
    h.execute(&mut w, tx(vec![Op::Despawn { id: "grid".into() }]))
        .unwrap();
    h.undo(&mut w).unwrap();
    assert_eq!(w.state_hash(), hash);
    assert!(w.update().is_empty(), "no forced regeneration");
    // So does a dry run.
    History::dry_run(&mut w, &tx(vec![Op::Despawn { id: "grid".into() }])).unwrap();
    assert_eq!(w.state_hash(), hash);
    assert!(w.update().is_empty(), "no forced regeneration");
}

#[test]
fn undo_restores_override_order() {
    let (mut w, mut h) = sample();
    h.execute(&mut w, tx(vec![spawn_op(grid_doc(1, 3))]))
        .unwrap();
    w.update();
    let size = v3(2.0, 2.0, 2.0);
    h.execute(
        &mut w,
        tx(vec![
            set("grid/pillar_0_0", "Primitive", "size", size.clone()),
            set("grid/pillar_0_1", "Primitive", "size", size),
        ]),
    )
    .unwrap();
    let set_override = |target: &str, field: bool, deleted: Option<bool>| Op::SetOverride {
        source: "grid".into(),
        target: target.into(),
        component: field.then(|| "Primitive".into()),
        field: field.then(|| "size".into()),
        value: None,
        deleted,
        index: None,
        moved: None,
        replaced: None,
        insert: None,
    };
    h.execute(
        &mut w,
        tx(vec![
            set_override("pillar_0_2", false, Some(true)),
            set_override("pillar_0_1", false, Some(true)),
        ]),
    )
    .unwrap();
    let before = overrides_of(&w, "grid");
    assert_eq!(before.len(), 4);
    let hash = w.state_hash();
    // Clearing the first field override and the first deletion override.
    let clears = tx(vec![
        set_override("pillar_0_0", true, None),
        set_override("pillar_0_2", false, Some(false)),
    ]);
    History::dry_run(&mut w, &clears).unwrap();
    assert_eq!(overrides_of(&w, "grid"), before);
    assert_eq!(w.state_hash(), hash);
    h.execute(&mut w, clears).unwrap();
    assert_eq!(overrides_of(&w, "grid").len(), 2);
    h.undo(&mut w).unwrap();
    assert_eq!(overrides_of(&w, "grid"), before);
    assert_eq!(w.state_hash(), hash);
    // The inverse names the position, and survives the JSON round trip.
    h.redo(&mut w).unwrap();
    let inverse = h.peek_undo().unwrap().to_vec();
    let json = serde_json::to_value(&inverse).unwrap();
    assert_eq!(
        (&json[0]["index"], &json[1]["index"]),
        (&json!(1), &json!(0)),
        "{json}"
    );
    let back: Vec<Op> = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), json);
}

#[test]
fn despawn_with_a_generated_referrer_undoes_exactly() {
    let (mut w, mut h) = sample();
    w.generators.register(re_scene::GeneratorInfo {
        name: "welder".into(),
        doc: "One entity welded to the crate.".into(),
        params_component: "PillarGrid".into(),
        run: std::sync::Arc::new(|ctx| {
            let target = ctx.world().entity("crate").ok();
            Ok(vec![re_scene::GenNode {
                key: "w".into(),
                name: "W".into(),
                parent_key: None,
                components: vec![("Weld".into(), vec![("other".into(), Value::Entity(target))])],
            }])
        }),
        fingerprint_extra: None,
        version: 1,
        oldest: 1,
    });
    let src = entity(
        "src",
        Some("world"),
        &[
            ("Procedural", &[("generator", Doc::Str("welder".into()))]),
            ("PillarGrid", &[]),
        ],
    );
    h.execute(&mut w, tx(vec![spawn_op(src)])).unwrap();
    w.update();
    assert_eq!(
        w.get_doc("src/w", "Weld", "other").unwrap(),
        Doc::Str("crate".into())
    );
    let hash = w.state_hash();
    h.execute(&mut w, tx(vec![Op::Despawn { id: "crate".into() }]))
        .unwrap();
    assert_eq!(w.get_doc("src/w", "Weld", "other").unwrap(), Doc::Null);
    h.undo(&mut w).unwrap();
    assert!(overrides_of(&w, "src").is_empty());
    assert_eq!(
        w.get_doc("src/w", "Weld", "other").unwrap(),
        Doc::Str("crate".into())
    );
    assert_eq!(w.state_hash(), hash);
    History::dry_run(&mut w, &tx(vec![Op::Despawn { id: "crate".into() }])).unwrap();
    assert!(overrides_of(&w, "src").is_empty());
    assert_eq!(w.state_hash(), hash);
}

#[test]
fn typed_output_commands_carry_an_output_schema() {
    #[derive(serde::Serialize, schemars::JsonSchema)]
    struct Out {
        /// How many.
        count: u32,
    }
    let mut reg: CommandRegistry<EditState> = CommandRegistry::new();
    reg.register_typed::<re_command::NoArgs, _>(
        "plain",
        "No output schema.",
        re_command::Tier::Read,
        true,
        |_t, _a, _c| Ok(json!(null)),
    );
    reg.register_typed_output::<re_command::NoArgs, Out, _>(
        "typed",
        "Has an output schema.",
        re_command::Tier::Read,
        true,
        |_t, _a, _c| Ok(json!({ "count": 2 })),
    );
    assert!(reg.spec("plain").unwrap().output_schema.is_none());
    let typed = reg.spec("typed").unwrap();
    let schema = typed.output_schema.as_ref().unwrap();
    assert_eq!(schema["properties"]["count"]["type"], "integer");
    assert_eq!(schema["required"], json!(["count"]));
    // `specs()` sees it too, and a spec without one serializes without the key.
    assert!(
        reg.specs()
            .iter()
            .any(|s| s.name == "typed" && s.output_schema.is_some())
    );
    let plain = serde_json::to_value(reg.spec("plain").unwrap()).unwrap();
    assert!(plain.get("output_schema").is_none());
    let back: re_command::CommandSpec = serde_json::from_value(plain).unwrap();
    assert!(back.output_schema.is_none());
    let mut s = EditState::new();
    let out = reg.dispatch(&mut s, "typed", json!({}), false, &AllowAll);
    assert_eq!(out.data, json!({ "count": 2 }));
}

/// A drag's live edits share a coalescing key: they are one undo step, the
/// log still has each, and a seal, another transaction or an undo ends the
/// run.
#[test]
fn coalescing_transactions_are_one_undo_step() {
    let (mut w, mut h) = sample();
    let s0 = scene_json(&mut w);
    let drag =
        |v: i64| tx(vec![set("lamp", "PointLight", "intensity", Doc::Int(v))]).coalescing("gizmo");
    let logged = h.log().len();
    for v in [100, 200, 300] {
        h.execute(&mut w, drag(v)).unwrap();
    }
    assert_eq!(h.log().len(), logged + 3, "every edit is logged");
    // Same fields: the step keeps the last value and the first inverse.
    assert_eq!(h.peek_undo().unwrap().len(), 1);
    let s3 = scene_json(&mut w);
    assert!(h.undo(&mut w).unwrap());
    assert_eq!(scene_json(&mut w), s0, "one undo reverts the whole drag");
    assert!(h.redo(&mut w).unwrap());
    assert_eq!(scene_json(&mut w), s3);

    // After a redo the step is sealed: a new drag is a new step.
    h.execute(&mut w, drag(400)).unwrap();
    assert!(h.undo(&mut w).unwrap());
    assert_eq!(scene_json(&mut w), s3);

    // A seal ends the run.
    h.execute(&mut w, drag(500)).unwrap();
    h.seal();
    h.execute(&mut w, drag(600)).unwrap();
    assert!(h.undo(&mut w).unwrap());
    let s5 = scene_json(&mut w);
    assert!(s5.contains("500"), "{s5}");

    // Another key, or another origin, starts a new step.
    h.execute(&mut w, drag(700)).unwrap();
    h.execute(
        &mut w,
        tx(vec![set("lamp", "PointLight", "intensity", Doc::Int(800))]).coalescing("other"),
    )
    .unwrap();
    assert!(h.undo(&mut w).unwrap());
    assert!(scene_json(&mut w).contains("700"));
    let mut by_user = Transaction::new(
        "test",
        Origin::User,
        vec![set("lamp", "PointLight", "intensity", Doc::Int(900))],
    );
    by_user.coalesce = Some("gizmo".into());
    h.execute(&mut w, by_user).unwrap();
    assert!(h.undo(&mut w).unwrap());
    assert!(scene_json(&mut w).contains("700"));

    // Different operations in one run are kept in full and undone together.
    h.seal();
    let before = scene_json(&mut w);
    h.execute(&mut w, drag(1000)).unwrap();
    h.execute(
        &mut w,
        tx(vec![Op::Rename {
            id: "crate".into(),
            name: "Box".into(),
        }])
        .coalescing("gizmo"),
    )
    .unwrap();
    assert_eq!(h.peek_undo().unwrap().len(), 2);
    assert!(h.undo(&mut w).unwrap());
    assert_eq!(scene_json(&mut w), before);
}

/// A coalesced drag on a generated entity records overrides as it goes; when
/// it starts setting a second field, that field's override restore joins
/// the step, so one undo removes every override the drag made.
#[test]
fn a_coalesced_drag_on_a_generated_entity_undoes_its_overrides() {
    let (mut w, mut h) = sample();
    h.execute(&mut w, tx(vec![spawn_op(grid_doc(2, 3))]))
        .unwrap();
    assert!(w.update().warnings().is_empty());
    let before = scene_json(&mut w);
    let id = "grid/pillar_0_0";
    let colour = |r: f32| {
        set(
            id,
            "Primitive",
            "color",
            Doc::List(vec![
                Doc::F32(r),
                Doc::F32(0.0),
                Doc::F32(0.0),
                Doc::F32(1.0),
            ]),
        )
    };
    let moved = |x: f32| {
        set(
            id,
            "Transform",
            "translation",
            Doc::List(vec![Doc::F32(x), Doc::F32(0.0), Doc::F32(0.0)]),
        )
    };
    let key = "gizmo:pillar:1";
    h.execute(&mut w, tx(vec![colour(0.5)]).coalescing(key))
        .unwrap();
    h.execute(&mut w, tx(vec![colour(0.7), moved(2.0)]).coalescing(key))
        .unwrap();
    h.execute(&mut w, tx(vec![colour(0.9), moved(3.0)]).coalescing(key))
        .unwrap();
    assert_eq!(overrides_of(&w, "grid").len(), 2, "one override per field");
    let text = scene_json(&mut w);
    assert!(text.contains("\"translation\""), "{text}");
    assert!(h.undo(&mut w).unwrap());
    assert!(
        overrides_of(&w, "grid").is_empty(),
        "{:?}",
        overrides_of(&w, "grid")
    );
    assert_eq!(scene_json(&mut w), before);
}

/// A drag whose edits set a growing set of fields (a gizmo drag that starts
/// changing a second field) stays one operation per field: each keeps its
/// latest value and its first inverse, and one undo restores every field.
#[test]
fn a_coalesced_run_merges_set_fields_by_field() {
    let (mut w, mut h) = sample();
    let s0 = scene_json(&mut w);
    let intensity = |v: i64| set("lamp", "PointLight", "intensity", Doc::Int(v));
    let range = |v: i64| set("lamp", "PointLight", "range", Doc::Int(v));
    h.execute(&mut w, tx(vec![intensity(100)]).coalescing("gizmo:lamp:1"))
        .unwrap();
    h.execute(
        &mut w,
        tx(vec![intensity(200), range(5)]).coalescing("gizmo:lamp:1"),
    )
    .unwrap();
    h.execute(
        &mut w,
        tx(vec![intensity(300), range(6)]).coalescing("gizmo:lamp:1"),
    )
    .unwrap();
    assert_eq!(h.undo_key(), Some("gizmo:lamp:1"));
    assert_eq!(h.peek_undo().unwrap().len(), 2, "{:?}", h.peek_undo());
    let s3 = scene_json(&mut w);
    assert!(h.undo(&mut w).unwrap());
    assert_eq!(scene_json(&mut w), s0, "one undo restores both fields");
    assert_eq!(h.undo_key(), None);
    assert!(h.redo(&mut w).unwrap());
    assert_eq!(scene_json(&mut w), s3);
}

/// `set_note` sets, replaces and removes an entity's note; each undoes
/// exactly, a despawn's undo brings the note back, and a generated entity
/// refuses one (it is not saved, so its note would be lost).
#[test]
fn set_note_round_trips_through_undo() {
    let (mut w, mut h) = sample();
    let s0 = scene_json(&mut w);
    let note = |text: &str| Op::SetNote {
        id: "crate".into(),
        note: text.into(),
    };
    h.execute(&mut w, tx(vec![note("Heavy: two people lift it.")]))
        .unwrap();
    let s1 = scene_json(&mut w);
    assert!(
        s1.contains("\"note\": \"Heavy: two people lift it.\""),
        "{s1}"
    );
    h.execute(&mut w, tx(vec![Op::Despawn { id: "crate".into() }]))
        .unwrap();
    assert!(h.undo(&mut w).unwrap());
    assert_eq!(scene_json(&mut w), s1, "the despawn's undo keeps the note");
    h.execute(&mut w, tx(vec![note("")])).unwrap();
    assert_eq!(scene_json(&mut w), s0, "an empty note removes it");
    assert!(h.undo(&mut w).unwrap());
    assert_eq!(scene_json(&mut w), s1);
    assert!(h.undo(&mut w).unwrap());
    assert_eq!(scene_json(&mut w), s0);

    h.execute(&mut w, tx(vec![spawn_op(grid_doc(2, 2))]))
        .unwrap();
    w.update();
    let err = h
        .execute(
            &mut w,
            tx(vec![Op::SetNote {
                id: "grid/pillar_0_0".into(),
                note: "x".into(),
            }]),
        )
        .unwrap_err();
    assert!(err.to_string().contains("generated"), "{err}");
}
