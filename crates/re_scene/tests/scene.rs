//! `re_scene` tests: the ported core spike tests that need no undo history
//! (the ones that do live in `re_command`), plus the entity core PR's new
//! requirements (script components in files, suppression, prefabs,
//! `state_hash`, `fork`, a surviving key's `Entity`, no compute pool).

mod support;

use glam::{Quat, Vec3};
use re_reflect::{ComponentOrigin, ComponentSchema, Doc, FieldSchema, FieldType, Value};
use re_scene::ecs::hierarchy::ChildOf;
use re_scene::hierarchy::{self, check_reparent, insert_child, remove_parent};
use re_scene::scene;
use re_scene::{
    GlobalTransform, OverrideDoc, Overrides, PrefabFile, SaveOptions, SceneError, World,
};
use support::*;

fn sample() -> World {
    let mut w = world();
    w.spawn_docs(&sample_docs(), None).unwrap();
    w
}

fn health_schema() -> ComponentSchema {
    ComponentSchema {
        name: "Health".into(),
        doc: "Hit points (declared from a script).".into(),
        fields: vec![
            FieldSchema {
                name: "current".into(),
                ty: FieldType::F32,
                doc: String::new(),
                default: Value::Int(100),
            },
            FieldSchema {
                name: "team".into(),
                ty: FieldType::String,
                doc: String::new(),
                default: Value::Str("red".into()),
            },
        ],
        origin: ComponentOrigin::Script,
    }
}

#[test]
fn script_components_are_real_ecs_components() {
    let mut w = sample();
    let key = w
        .registry
        .register_script(&mut w.ecs, health_schema())
        .unwrap();
    let crate_e = w.entity("crate").unwrap();
    let mut values = w.registry.schema(key).defaults();
    values[1] = Value::Str("blue".into());
    w.registry.insert(&mut w.ecs, crate_e, key, values).unwrap();
    assert_eq!(
        w.get("crate", "Health", "current").unwrap(),
        Value::Float(100.0)
    );
    assert_eq!(
        w.get("crate", "Health", "team").unwrap(),
        Value::Str("blue".into())
    );

    // It is a genuine bevy_ecs component: queryable by ComponentId.
    let id = w.registry.component_id(key);
    assert!(w.ecs.entity(crate_e).contains_id(id));

    w.set("crate", "Health", "current", Value::Int(42)).unwrap();
    assert_eq!(
        w.get("crate", "Health", "current").unwrap(),
        Value::Float(42.0)
    );

    // Saved and loaded like any other component; the file carries its schema.
    let text = scene_json(&mut w);
    assert!(text.contains("\"Health\""));
    let doc = scene::from_json(&text).unwrap();
    assert_eq!(doc.script_components.len(), 1);
    assert_eq!(doc.script_components[0].name, "Health");

    // A world that already declared it (as a script would) loads it...
    let mut w2 = world();
    w2.registry
        .register_script(&mut w2.ecs, w.registry.schema(key).clone())
        .unwrap();
    w2.load(&doc).unwrap();
    assert_eq!(scene_json(&mut w2), text);

    // ...and so does a fresh world with no script run first.
    let mut w3 = world();
    w3.load(&scene::from_json(&text).unwrap()).unwrap();
    assert_eq!(scene_json(&mut w3), text);
    assert_eq!(
        w3.get("crate", "Health", "current").unwrap(),
        Value::Float(42.0)
    );

    // A different declaration of the same name is refused.
    let mut w4 = world();
    let mut other = health_schema();
    other.fields.pop();
    w4.registry.register_script(&mut w4.ecs, other).unwrap();
    let err = w4.load(&doc).unwrap_err();
    assert_eq!(err.code().to_string(), "RE1208");
    assert_eq!(w4.ids().len(), 0);

    // Despawn drops the boxed data; restoring the snapshot brings it back.
    let snapshot = w.snapshot_subtree(crate_e);
    hierarchy::despawn_recursive(&mut w.ecs, crate_e);
    assert!(w.entity("crate").is_err());
    w.restore(&snapshot, Some(1)).unwrap();
    assert_eq!(
        w.get("crate", "Health", "current").unwrap(),
        Value::Float(42.0)
    );
    // A raw despawn does not track references (re_command's despawn does).
    let crate_e = w.entity("crate").unwrap();
    w.set("weld", "Weld", "other", Value::Entity(Some(crate_e)))
        .unwrap();
    assert_eq!(scene_json(&mut w), text);
}

#[test]
fn id_index_follows_hooks() {
    let mut w = sample();
    assert_eq!(w.ids().len(), 5);
    // Despawning a parent despawns its subtree and the hooks unindex every id.
    let crate_e = w.entity("crate").unwrap();
    let snapshot = w.snapshot_subtree(crate_e);
    hierarchy::despawn_recursive(&mut w.ecs, crate_e);
    assert_eq!(w.ids().len(), 3);
    assert!(w.ids().get("lamp").is_none());
    w.restore(&snapshot, Some(1)).unwrap();
    assert_eq!(w.ids().len(), 5);
    assert!(w.ids().get("lamp").is_some());
    // A duplicate id is refused and leaves the index as it was.
    let err = w
        .spawn_docs(&[entity("lamp", None, &[])], None)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"an entity with id "lamp" already exists"#
    );
    assert_eq!(w.ids().len(), 5);
}

#[test]
fn hierarchy_reparent_order_and_cycles() {
    let mut w = sample();
    assert_eq!(ids_under(&w, "world"), ["ground", "crate", "weld"]);
    let world_e = w.entity("world").unwrap();
    let weld = w.entity("weld").unwrap();
    insert_child(&mut w.ecs, world_e, 0, weld);
    assert_eq!(ids_under(&w, "world"), ["weld", "ground", "crate"]);
    insert_child(&mut w.ecs, world_e, 2, weld);
    assert_eq!(ids_under(&w, "world"), ["ground", "crate", "weld"]);

    let crate_e = w.entity("crate").unwrap();
    let lamp = w.entity("lamp").unwrap();
    let err = check_reparent(&w.ecs, crate_e, lamp).unwrap_err();
    assert!(matches!(err, SceneError::HierarchyCycle { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        r#"cannot parent "crate" under "lamp": "lamp" is "crate" or one of its descendants"#
    );
    assert!(check_reparent(&w.ecs, crate_e, crate_e).is_err());

    remove_parent(&mut w.ecs, lamp);
    assert!(w.ecs.get::<ChildOf>(lamp).is_none());
    insert_child(&mut w.ecs, crate_e, 0, lamp);
    assert_eq!(ids_under(&w, "crate"), ["lamp"]);
    assert_eq!(w.find("WORLD/CRATE/LAMP"), Some(lamp));
}

#[test]
fn transform_propagation_incremental_matches_full() {
    let mut w = sample();
    w.update();
    let lamp = w.entity("lamp").unwrap();
    let g = w.ecs.get::<GlobalTransform>(lamp).unwrap().0;
    assert_eq!(Vec3::from(g.translation), Vec3::new(0.0, 6.0, 0.0));

    // Rotate the crate: only its subtree changes.
    let rot = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
    w.set("crate", "Transform", "rotation", Value::Quat(rot))
        .unwrap();
    w.update();
    let g = w.ecs.get::<GlobalTransform>(lamp).unwrap().0;
    assert!(
        (Vec3::from(g.translation) - Vec3::new(-1.0, 5.0, 0.0)).length() < 1e-5,
        "{:?}",
        g.translation
    );

    // Unparenting is seen through RemovedComponents<ChildOf>.
    remove_parent(&mut w.ecs, lamp);
    w.update();
    let g = w.ecs.get::<GlobalTransform>(lamp).unwrap().0;
    assert_eq!(Vec3::from(g.translation), Vec3::new(0.0, 1.0, 0.0));

    // Compare with a full recomputation.
    let mut q = w.ecs.query::<&GlobalTransform>();
    let incremental: Vec<_> = q.iter(&w.ecs).map(|g| g.0).collect();
    w.propagate_all();
    let full: Vec<_> = q.iter(&w.ecs).map(|g| g.0).collect();
    assert_eq!(incremental, full);
}

#[test]
fn scene_round_trip_is_byte_identical() {
    let mut w = sample();
    let json = scene_json(&mut w);
    let mut a = world();
    a.load(&scene::from_json(&json).unwrap()).unwrap();
    assert_eq!(scene_json(&mut a), json);

    // `$schema` is accepted and written back unchanged.
    let with_schema = json.replacen('{', "{\n  \"$schema\": \"../scene.schema.json\",", 1);
    let mut b = world();
    b.load(&scene::from_json(&with_schema).unwrap()).unwrap();
    assert_eq!(scene_json(&mut b), with_schema);

    // omit_defaults output loads back to the same scene.
    let small = scene::to_json(&w.save(SaveOptions {
        omit_defaults: true,
    }));
    assert!(small.len() < json.len());
    let mut c = world();
    c.load(&scene::from_json(&small).unwrap()).unwrap();
    assert_eq!(scene_json(&mut c), json);
}

#[test]
fn load_errors_name_the_place() {
    let mut w = world();
    let bad = r#"{ "format": "real-engine/scene", "version": 1, "entities": [
        { "id": "a", "components": { "Transform": { "translation": [0, 1] } } }
    ] }"#;
    let err = w.load(&scene::from_json(bad).unwrap()).unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"parse error: entities[0] (id "a"): Transform.translation: expected vec3 as [x, y, z], found list of 2"#
    );
    assert_eq!(
        w.ids().len(),
        0,
        "failed load must leave the world untouched"
    );

    let bad = r#"{ "format": "real-engine/scene", "version": 1, "entities": [ { "id": "a", "parnt": "b" } ] }"#;
    let err = scene::from_json(bad).unwrap_err();
    assert!(err.to_string().contains("unknown field `parnt`"), "{err}");

    let bad = r#"{ "format": "real-engine/scene", "version": 1, "entities": [ { "id": "a", "parent": "b" }, { "id": "b" } ] }"#;
    let err = w.load(&scene::from_json(bad).unwrap()).unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"parse error: entities[0] (id "a"): parent "b" is not defined earlier in the file or in the world"#
    );
    assert_eq!(w.ids().len(), 0);

    // A later failure removes the entities already spawned by this load.
    let bad = r#"{ "format": "real-engine/scene", "version": 1, "entities": [ { "id": "a" }, { "id": "b", "components": { "Nope": {} } } ] }"#;
    let err = w.load(&scene::from_json(bad).unwrap()).unwrap_err();
    assert_eq!(err.code().to_string(), "RE1204");
    assert_eq!(err.to_diagnostic().related[0].code.to_string(), "RE1001");
    assert_eq!(w.ids().len(), 0);
}

fn grid_world(rows: i64, cols: i64) -> World {
    let mut w = sample();
    w.spawn_docs(&[grid_doc(rows, cols)], None).unwrap();
    let report = w.update();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    w
}

#[test]
fn surviving_keys_keep_their_entity() {
    let mut w = grid_world(2, 3);
    let grid = w.entity("grid").unwrap();
    assert_eq!(hierarchy::children(&w.ecs, grid).len(), 6);
    let before = w.entity("grid/pillar_1_2").unwrap();
    let cap_before = w.entity("grid/pillar_1_2/cap").unwrap();

    w.set("grid", "PillarGrid", "cols", Value::Int(4)).unwrap();
    let report = w.update();
    assert_eq!(report.spawned.len(), 4, "{report:?}");
    assert_eq!(report.updated.len(), 12);
    assert!(report.despawned.is_empty());
    assert_eq!(w.entity("grid/pillar_1_2").unwrap(), before);
    assert_eq!(w.entity("grid/pillar_1_2/cap").unwrap(), cap_before);

    w.set("grid", "PillarGrid", "cols", Value::Int(2)).unwrap();
    let report = w.update();
    assert_eq!(report.despawned.len(), 8);
    assert!(w.entity("grid/pillar_1_2").is_err());

    // Nothing changed: the generator does not run again.
    assert!(w.update().is_empty());

    // A failing generator keeps the old output.
    let kept = w.entity("grid/pillar_0_0").unwrap();
    w.set("grid", "PillarGrid", "rows", Value::Int(5000))
        .unwrap();
    let report = w.update();
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].code.to_string(), "RE1207");
    assert_eq!(w.entity("grid/pillar_0_0").unwrap(), kept);
}

#[test]
fn deletion_overrides_and_suppression_stop_spawning() {
    let mut w = grid_world(1, 3);
    let grid = w.entity("grid").unwrap();

    // A deletion override (what despawning a generated entity records).
    let mut ov = Overrides::default();
    ov.set_deleted("pillar_0_1", true);
    w.ecs.entity_mut(grid).insert(ov);
    w.regenerate(grid);
    assert!(w.entity("grid/pillar_0_1").is_err());
    assert!(w.entity("grid/pillar_0_1/cap").is_err());
    assert!(w.entity("grid/pillar_0_0").is_ok());
    let text = scene_json(&mut w);
    assert!(text.contains("\"deleted\": true"), "{text}");

    // A suppression (what a game script's despawn records): play state only.
    w.suppress("grid/pillar_0_2").unwrap();
    w.regenerate(grid);
    assert!(w.entity("grid/pillar_0_2").is_err());
    assert!(w.entity("grid/pillar_0_0").is_ok());
    assert_eq!(scene_json(&mut w), text, "suppression is not saved");

    // despawn_runtime suppresses a generated entity by key.
    let p00 = w.entity("grid/pillar_0_0").unwrap();
    w.despawn_runtime(p00).unwrap();
    w.regenerate(grid);
    assert!(w.entity("grid/pillar_0_0").is_err());
    assert_eq!(w.suppressions().len(), 2);

    let err = w.suppress("crate").unwrap_err();
    assert_eq!(err.code().to_string(), "RE1210");

    // A loaded copy has the deletion override but no suppressions.
    let mut copy = world();
    copy.load(&scene::from_json(&text).unwrap()).unwrap();
    copy.update();
    assert!(copy.entity("grid/pillar_0_1").is_err());
    assert!(copy.entity("grid/pillar_0_0").is_ok());
    assert!(copy.entity("grid/pillar_0_2").is_ok());
}

#[test]
fn prefab_instantiates_with_prefixed_keys() {
    let text = r#"{
  "format": "real-engine/prefab",
  "version": 1,
  "entities": [
    { "id": "frame", "name": "Frame", "components": { "Transform": {}, "Primitive": { "size": [2, 3, 0.2] } } },
    { "id": "panel", "name": "Panel", "parent": "frame", "components": { "PointLight": { "intensity": 5 } } }
  ]
}"#;
    let prefab = PrefabFile::from_json(text).unwrap();
    let w = world();
    let fragment = prefab.instantiate(&w.registry, "door_1").unwrap();
    let keys: Vec<_> = fragment.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(keys, ["door_1/frame", "door_1/panel"]);
    assert_eq!(fragment[0].parent_key, None);
    assert_eq!(fragment[1].parent_key.as_deref(), Some("door_1/frame"));
    assert_eq!(fragment[1].name, "Panel");
    let light = &fragment[1].components[0];
    assert_eq!(light.0, "PointLight");
    assert!(
        light
            .1
            .contains(&("intensity".to_string(), Value::Float(5.0)))
    );

    let bad = text.replace("real-engine/prefab", "real-engine/scene");
    assert_eq!(
        PrefabFile::from_json(&bad).unwrap_err().code().to_string(),
        "RE1206"
    );
}

fn built() -> World {
    let mut w = grid_world(2, 2);
    let key = w
        .registry
        .register_script(&mut w.ecs, health_schema())
        .unwrap();
    let crate_e = w.entity("crate").unwrap();
    let values = w.registry.schema(key).defaults();
    w.registry.insert(&mut w.ecs, crate_e, key, values).unwrap();
    let grid = w.entity("grid").unwrap();
    let mut ov = Overrides::default();
    ov.set(
        "pillar_0_0",
        "Primitive",
        "color",
        Some(Doc::List(vec![Doc::F32(1.0), Doc::F32(0.0), Doc::F32(0.0)])),
    );
    w.ecs.entity_mut(grid).insert(ov);
    w.regenerate(grid);
    w.suppress("grid/pillar_1_1").unwrap();
    w.regenerate(grid);
    w.update();
    w
}

#[test]
fn state_hash_is_deterministic_and_bit_sensitive() {
    let a = built();
    let b = built();
    assert_eq!(a.state_hash(), b.state_hash());

    let mut c = built();
    let Value::Float(mass) = c.get("crate", "RigidBody", "mass").unwrap() else {
        panic!()
    };
    let next = f32::from_bits((mass as f32).to_bits() + 1);
    c.set("crate", "RigidBody", "mass", Value::Float(next as f64))
        .unwrap();
    assert_ne!(a.state_hash(), c.state_hash());
    c.set("crate", "RigidBody", "mass", Value::Float(mass))
        .unwrap();
    assert_eq!(a.state_hash(), c.state_hash());
}

/// Every NaN hashes as one: which NaN an operation returns differs between
/// x86, arm64 and wasm (ADR-0036). Reflected writes refuse NaN, so this goes
/// through the component, as a physics write-back could.
#[test]
fn state_hash_treats_every_nan_as_one() {
    let quiet = f32::NAN;
    let other = f32::from_bits(quiet.to_bits() | 1 | (1 << 31));
    assert!(other.is_nan() && other.to_bits() != quiet.to_bits());
    let with_x = |x: f32| {
        let mut w = built();
        let e = w.entity("crate").unwrap();
        w.ecs
            .get_mut::<re_scene::Transform>(e)
            .unwrap()
            .translation
            .x = x;
        w.state_hash()
    };
    assert_eq!(with_x(quiet), with_x(other));
    assert_ne!(with_x(quiet), with_x(0.0));
}

#[test]
fn a_new_defaulted_field_leaves_state_hash_unchanged() {
    // The same world, with Health declared as today and with one more field.
    let with = |extra: Option<FieldSchema>, armor: Option<Value>| {
        let mut w = grid_world(2, 2);
        let mut schema = health_schema();
        schema.fields.extend(extra);
        let key = w.registry.register_script(&mut w.ecs, schema).unwrap();
        let e = w.entity("crate").unwrap();
        let mut values = w.registry.schema(key).defaults();
        values[0] = Value::Float(42.0);
        if let Some(armor) = armor {
            values[2] = armor;
        }
        w.registry.insert(&mut w.ecs, e, key, values).unwrap();
        w.update();
        w.state_hash()
    };
    let armor = || FieldSchema {
        name: "armor".into(),
        ty: FieldType::F32,
        doc: String::new(),
        default: Value::Float(0.0),
    };
    let today = with(None, None);
    assert_eq!(with(Some(armor()), None), today);
    assert_ne!(with(Some(armor()), Some(Value::Float(5.0))), today);
}

#[test]
fn fork_copies_everything_and_is_independent() {
    let mut original = built();
    assert!(original.ids().get("grid/pillar_0_0").is_some());
    assert!(!original.suppressions().is_empty());
    let hash = original.state_hash();

    let mut fork = original.fork();
    assert_eq!(fork.state_hash(), hash);
    assert_eq!(scene_json(&mut fork), scene_json(&mut original));
    assert_eq!(fork.ids().len(), original.ids().len());
    assert!(fork.suppressions().contains("grid/pillar_1_1"));
    let grid = fork.entity("grid").unwrap();
    assert!(matches!(
        &fork.ecs.get::<Overrides>(grid).unwrap().0[0],
        OverrideDoc::Field { target, .. } if target == "pillar_0_0"
    ));
    // Weld references are remapped to the fork's entities.
    let fork_crate = fork.entity("crate").unwrap();
    assert_eq!(
        fork.get("weld", "Weld", "other").unwrap(),
        Value::Entity(Some(fork_crate))
    );
    // An up-to-date fork does not regenerate.
    assert!(fork.update().is_empty());
    assert_eq!(fork.state_hash(), hash);

    // Editing the fork leaves the original alone.
    fork.set("crate", "Health", "current", Value::Int(1))
        .unwrap();
    fork.set("grid", "PillarGrid", "rows", Value::Int(3))
        .unwrap();
    fork.update();
    assert_ne!(fork.state_hash(), hash);
    assert_eq!(original.state_hash(), hash);
}

#[cfg(target_os = "linux")]
#[test]
fn no_compute_task_pool_threads() {
    let mut w = grid_world(3, 3);
    w.update();
    let text = scene_json(&mut w);
    let mut again = world();
    again.load(&scene::from_json(&text).unwrap()).unwrap();
    again.update();
    let mut names = Vec::new();
    for entry in std::fs::read_dir("/proc/self/task").unwrap() {
        let path = entry.unwrap().path().join("comm");
        if let Ok(name) = std::fs::read_to_string(path) {
            names.push(name.trim().to_string());
        }
    }
    assert!(
        !names.iter().any(|n| n.starts_with("TaskPool")),
        "bevy's compute pool started: {names:?}"
    );
}

#[test]
fn diagnostics_catalogue_is_complete() {
    let problems = re_core::validate_catalog(re_scene::DIAGNOSTICS, re_scene::CODE_RANGE);
    assert!(problems.is_empty(), "{problems:?}");
    for code in [
        re_scene::codes::UNKNOWN_ID,
        re_scene::codes::GENERATOR_FAILED,
        re_scene::codes::INVALID_CONTENT,
    ] {
        assert!(re_scene::DIAGNOSTICS.iter().any(|d| d.code == code));
    }
}

// Regression tests from the entity core PR review.

#[test]
fn authored_ids_with_a_slash_are_refused_on_load_and_restore() {
    // A source id with `/` would make generated ids ambiguous.
    let mut w = world();
    let mut docs = sample_docs();
    let mut grid = grid_doc(1, 1);
    grid.id = "lvl/grid".into();
    docs.push(grid);
    let text = scene::to_json(&scene::SceneDoc {
        schema: None,
        format: scene::FORMAT.into(),
        version: scene::VERSION,
        script_components: vec![],
        entities: docs,
    });
    let err = w.load(&scene::from_json(&text).unwrap()).unwrap_err();
    assert_eq!(err.code().to_string(), "RE1204");
    assert_eq!(err.to_diagnostic().related[0].code.to_string(), "RE1202");
    assert!(err.to_string().contains("\"lvl/grid\""), "{err}");
    assert_eq!(w.ids().len(), 0);

    // `restore` refuses an authored `/` id, a generated entry whose source
    // is not procedural, and one placed outside its source's output.
    let mut w = sample();
    let snap = re_scene::Snapshot {
        entities: vec![entity("crate/zz", Some("world"), &[])],
        ..Default::default()
    };
    assert_eq!(
        w.restore(&snap, None).unwrap_err().code().to_string(),
        "RE1204"
    );
    let snap = re_scene::Snapshot {
        entities: vec![entity("crate/zz", Some("crate"), &[])],
        generated: vec![re_scene::GeneratedDoc {
            source: "crate".into(),
            key: "zz".into(),
        }],
        fresh: vec![],
    };
    let err = w.restore(&snap, None).unwrap_err();
    assert!(err.to_string().contains("not a procedural source"), "{err}");
    w.spawn_docs(&[grid_doc(1, 1)], None).unwrap();
    let snap = re_scene::Snapshot {
        entities: vec![entity("grid/zz", Some("crate"), &[])],
        generated: vec![re_scene::GeneratedDoc {
            source: "grid".into(),
            key: "zz".into(),
        }],
        fresh: vec![],
    };
    let err = w.restore(&snap, None).unwrap_err();
    assert!(
        err.to_string().contains("parent must be its source"),
        "{err}"
    );
    assert_eq!(w.ids().len(), 6);
}

#[test]
fn regeneration_never_reuses_a_taken_id() {
    let mut w = sample();
    w.spawn_docs(&[grid_doc(1, 1)], None).unwrap();
    // An entity already holds a generated id (only raw ECS access can do
    // this now that authored ids are validated).
    let squatter = w
        .ecs
        .spawn(re_scene::StableId("grid/pillar_0_0".into()))
        .id();
    let report = w.update();
    assert_eq!(report.diagnostics.len(), 1, "{report:?}");
    assert_eq!(report.diagnostics[0].code.to_string(), "RE1207");
    assert!(report.spawned.is_empty());
    assert_eq!(w.entity("grid/pillar_0_0").unwrap(), squatter);
    let with_id = w.ecs.query::<&re_scene::StableId>().iter(&w.ecs).count();
    assert_eq!(w.ids().len(), with_id, "the id index stays one-to-one");

    // An empty key is refused too.
    w.generators.register(re_scene::GeneratorInfo {
        name: "empty_key".into(),
        doc: String::new(),
        params_component: "PillarGrid".into(),
        run: std::sync::Arc::new(|_| {
            Ok(vec![re_scene::GenNode {
                key: String::new(),
                name: String::new(),
                parent_key: None,
                components: vec![],
            }])
        }),
        fingerprint_extra: None,
    });
    w.set(
        "grid",
        "Procedural",
        "generator",
        Value::Str("empty_key".into()),
    )
    .unwrap();
    let report = w.update();
    assert_eq!(report.diagnostics[0].code.to_string(), "RE1207");
    assert!(w.entity("grid/").is_err());
}

#[test]
fn full_propagation_covers_transformless_ancestors() {
    // The sample's `world` root has no Transform.
    let mut w = sample();
    w.propagate_all();
    let lamp = w.entity("lamp").unwrap();
    let g = w.ecs.get::<GlobalTransform>(lamp).unwrap().0;
    assert_eq!(Vec3::from(g.translation), Vec3::new(0.0, 6.0, 0.0));

    // Both systems agree on every entity.
    let mut a = sample();
    a.spawn_docs(&[grid_doc(2, 2)], None).unwrap();
    a.regenerate_changed();
    let mut b = a.fork();
    a.propagate();
    b.propagate_all();
    let globals = |w: &mut World| {
        let mut q = w.ecs.query::<(&re_scene::StableId, &GlobalTransform)>();
        let mut v: Vec<(String, glam::Affine3A)> =
            q.iter(&w.ecs).map(|(id, g)| (id.0.clone(), g.0)).collect();
        v.sort_by(|x, y| x.0.cmp(&y.0));
        v
    };
    assert_eq!(globals(&mut a), globals(&mut b));
}

#[test]
fn incremental_regeneration_matches_a_fresh_one() {
    let mut w = grid_world(2, 2);
    w.set("grid", "PillarGrid", "cols", Value::Int(3)).unwrap();
    w.update();
    assert_eq!(
        ids_under(&w, "grid"),
        [
            "grid/pillar_0_0",
            "grid/pillar_0_1",
            "grid/pillar_0_2",
            "grid/pillar_1_0",
            "grid/pillar_1_1",
            "grid/pillar_1_2",
        ]
    );
    let mut fresh = world();
    fresh.load(&w.save(SaveOptions::default())).unwrap();
    fresh.update();
    assert_eq!(ids_under(&fresh, "grid"), ids_under(&w, "grid"));
    assert_eq!(fresh.state_hash(), w.state_hash());

    // Nothing to reorder: an unchanged output marks nothing changed.
    let before = w.state_hash();
    w.regenerate(w.entity("grid").unwrap());
    assert_eq!(w.state_hash(), before);
}

#[test]
fn find_takes_a_shared_world() {
    let w = sample();
    let shared: &World = &w;
    assert_eq!(shared.find("WORLD/CRATE/LAMP"), w.entity("lamp").ok());
    assert_eq!(shared.find("WORLD/NOPE"), None);
}

#[test]
fn a_declared_script_component_no_authored_entity_uses_is_saved() {
    // A prefab's generated entities may be the only users of a script
    // component: the saved scene still declares it, so it loads again.
    let text = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "script_components": [
    { "name": "Collectible", "fields": [ { "name": "value", "type": { "kind": "i32" }, "default": 1 } ] }
  ],
  "entities": [ { "id": "a", "components": {} } ]
}"#;
    let mut w = world();
    w.load(&scene::from_json(text).unwrap()).unwrap();
    let saved = scene_json(&mut w);
    let doc = scene::from_json(&saved).unwrap();
    assert_eq!(doc.script_components.len(), 1, "{saved}");
    assert_eq!(doc.script_components[0].name, "Collectible");
    // And the round trip is stable.
    let mut w2 = world();
    w2.load(&doc).unwrap();
    assert_eq!(scene_json(&mut w2), saved);
}
