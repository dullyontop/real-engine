//! Structural overrides (ADR-0043 §3): `proc.move`, `proc.replace`,
//! `proc.insert` and `proc.clear_structure` change a source's output, the
//! change survives regeneration, and undo gives back the world it had.

use std::sync::Arc;

use re_command::{AllowAll, CommandOutcome, CommandRegistry, EditState};
use re_core::files::ProjectFiles;
use re_proc::register_commands;
use re_reflect::{Value, component};
use re_scene::ecs as bevy_ecs;
use re_scene::structure::SceneFiles;
use re_scene::{GenNode, GeneratorInfo, SaveOptions, World, hierarchy};
use serde_json::{Value as Json, json};

component! {
    /// A component replacements and inserts use.
    pub struct Glow {
        /// How bright.
        pub intensity: f64 = 1.0,
    }
}

component! {
    /// The test generator's parameters.
    pub struct Rows {
        /// Whether `a` (and its child `a/x`) is generated.
        pub with_a: bool = true,
    }
}

fn at(x: f32) -> (String, Vec<(String, Value)>) {
    (
        "Transform".into(),
        vec![("translation".into(), Value::Vec3([x, 0.0, 0.0].into()))],
    )
}

fn node(key: &str, parent: Option<&str>, x: f32) -> GenNode {
    GenNode {
        key: key.into(),
        name: key.to_uppercase(),
        parent_key: parent.map(Into::into),
        components: vec![at(x)],
    }
}

const ROCK: &str = r#"{
  "format": "real-engine/prefab",
  "version": 1,
  "entities": [
    { "id": "rock", "name": "Rock", "components": { "Transform": { "translation": [0, 5, 0] }, "Glow": {} } },
    { "id": "pebble", "name": "Pebble", "parent": "rock", "components": { "Transform": {} } }
  ]
}"#;

const TWO_ROOTS: &str = r#"{
  "format": "real-engine/prefab",
  "version": 1,
  "entities": [
    { "id": "one", "components": {} },
    { "id": "two", "components": {} }
  ]
}"#;

/// A source `src` whose generator gives `a` (with child `a/x`), `b` and
/// `c`, in a project holding two prefabs.
fn world() -> (tempfile::TempDir, EditState) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("prefabs")).unwrap();
    std::fs::write(dir.path().join("prefabs/rock.prefab.json"), ROCK).unwrap();
    std::fs::write(dir.path().join("prefabs/two.prefab.json"), TWO_ROOTS).unwrap();
    let mut st = EditState::new();
    st.world.registry.register::<Rows>(&mut st.world.ecs);
    st.world.registry.register::<Glow>(&mut st.world.ecs);
    st.world.generators.register(GeneratorInfo {
        name: "rows".into(),
        doc: String::new(),
        params_component: "Rows".into(),
        run: Arc::new(|ctx| {
            let mut out = Vec::new();
            if ctx.param::<bool>("with_a")? {
                out.push(node("a", None, 0.0));
                out.push(node("a/x", Some("a"), 0.5));
            }
            out.push(node("b", None, 1.0));
            out.push(node("c", None, 2.0));
            Ok(out)
        }),
        fingerprint_extra: None,
        version: 1,
        oldest: 1,
    });
    st.world
        .ecs
        .insert_resource(SceneFiles(ProjectFiles::project(dir.path())));
    let scene = json!({
        "format": "real-engine/scene",
        "version": 1,
        "entities": [{
            "id": "src",
            "components": {
                "Procedural": { "generator": "rows" },
                "Rows": {},
                "Transform": {}
            }
        }]
    });
    st.world
        .load(&re_scene::scene::from_json(&scene.to_string()).unwrap())
        .unwrap();
    let report = st.world.regenerate_changed();
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    (dir, st)
}

fn run(st: &mut EditState, command: &str, args: Json) -> CommandOutcome {
    let mut r = CommandRegistry::new();
    re_command::register_builtin_commands(&mut r);
    register_commands(&mut r);
    r.dispatch(st, command, args, false, &AllowAll)
}

fn ok(st: &mut EditState, command: &str, args: Json) -> Json {
    let o = run(st, command, args);
    assert!(o.ok, "{command}: {:#?}", o.diagnostics);
    o.data.clone()
}

fn codes(o: &CommandOutcome) -> Vec<String> {
    o.diagnostics.iter().map(|d| d.code.to_string()).collect()
}

fn saved(w: &mut World) -> String {
    re_scene::scene::to_json(&w.save(SaveOptions::default()))
}

/// The ids of `id`'s children, in order.
fn children(w: &World, id: &str) -> Vec<String> {
    let e = w.entity(id).unwrap();
    hierarchy::children(&w.ecs, e)
        .iter()
        .map(|c| w.id_of(*c).unwrap().to_string())
        .collect()
}

fn overrides(w: &mut World) -> Json {
    let text = saved(w);
    let doc: Json = serde_json::from_str(&text).unwrap();
    doc["entities"][0]["overrides"].clone()
}

/// Undo, regenerate, and check the world is what it was.
fn undo_restores(st: &mut EditState, before: &str, hash: blake3::Hash) {
    assert!(st.history.undo(&mut st.world).unwrap());
    st.world.regenerate_changed();
    assert_eq!(saved(&mut st.world), before);
    assert_eq!(st.world.state_hash(), hash);
}

#[test]
fn a_move_reparents_and_survives_regeneration_and_undo_restores() {
    let (_dir, mut st) = world();
    let before = saved(&mut st.world);
    let hash = st.world.state_hash();
    ok(
        &mut st,
        "proc.move",
        json!({ "id": "src/b", "parent": "src/a", "index": 0 }),
    );
    assert_eq!(children(&st.world, "src/a"), ["src/b", "src/a/x"]);
    assert_eq!(children(&st.world, "src"), ["src/a", "src/c"]);
    assert_eq!(
        overrides(&mut st.world),
        json!([{ "target": "b", "moved": { "parent": "a", "index": 0 } }])
    );
    // Same entity, moved in place.
    let b = st.world.entity("src/b").unwrap();
    st.world
        .set("src", "Procedural", "seed", Value::Int(7))
        .unwrap();
    st.world.regenerate_changed();
    assert_eq!(st.world.entity("src/b").unwrap(), b);
    assert_eq!(children(&st.world, "src/a"), ["src/b", "src/a/x"]);
    st.world
        .set("src", "Procedural", "seed", Value::Int(0))
        .unwrap();
    st.world.regenerate_changed();

    undo_restores(&mut st, &before, hash);
    assert!(st.history.redo(&mut st.world).unwrap());
    st.world.regenerate_changed();
    assert_eq!(children(&st.world, "src/a"), ["src/b", "src/a/x"]);

    // Back under the source.
    ok(
        &mut st,
        "proc.move",
        json!({ "id": "src/b", "parent": "src" }),
    );
    assert_eq!(children(&st.world, "src"), ["src/a", "src/c", "src/b"]);
}

#[test]
fn a_replacement_keeps_the_id_and_transform_and_drops_children() {
    let (_dir, mut st) = world();
    let before = saved(&mut st.world);
    let hash = st.world.state_hash();
    let a = st.world.entity("src/a").unwrap();
    ok(
        &mut st,
        "proc.replace",
        json!({ "id": "src/a", "components": { "Glow": { "intensity": 3.0 } } }),
    );
    assert_eq!(st.world.entity("src/a").unwrap(), a);
    assert_eq!(
        st.world.get("src/a", "Glow", "intensity").unwrap(),
        Value::Float(3.0)
    );
    // The generated Transform stays, the generated child goes.
    assert_eq!(
        st.world.get("src/a", "Transform", "translation").unwrap(),
        Value::Vec3([0.0, 0.0, 0.0].into())
    );
    assert!(st.world.entity("src/a/x").is_err());
    undo_restores(&mut st, &before, hash);

    ok(
        &mut st,
        "proc.replace",
        json!({ "id": "src/a", "components": { "Glow": {} }, "keep_children": true }),
    );
    assert!(st.world.entity("src/a/x").is_ok());
}

#[test]
fn a_prefab_replacement_brings_its_children_and_transform() {
    let (_dir, mut st) = world();
    let before = saved(&mut st.world);
    let hash = st.world.state_hash();
    ok(
        &mut st,
        "proc.replace",
        json!({ "id": "src/c", "prefab": "prefabs/rock.prefab.json" }),
    );
    assert_eq!(children(&st.world, "src/c"), ["src/c/~pebble"]);
    // The prefab root sets a Transform, so it wins over the generated one.
    assert_eq!(
        st.world.get("src/c", "Transform", "translation").unwrap(),
        Value::Vec3([0.0, 5.0, 0.0].into())
    );
    // The generator's name stays.
    let c = st.world.entity("src/c").unwrap();
    assert_eq!(st.world.name_of(c), "C");
    undo_restores(&mut st, &before, hash);
}

#[test]
fn an_insert_adds_an_entity_that_takes_field_overrides() {
    let (_dir, mut st) = world();
    let before = saved(&mut st.world);
    let hash = st.world.state_hash();
    let r = ok(
        &mut st,
        "proc.insert",
        json!({
            "source": "src", "key": "lamp", "parent": "src/a", "index": 1,
            "components": { "Transform": { "translation": [0, 2, 0] }, "Glow": {} }
        }),
    );
    assert_eq!(r["id"], "src/+lamp");
    assert_eq!(children(&st.world, "src/a"), ["src/a/x", "src/+lamp"]);
    let lamp = st.world.entity("src/+lamp").unwrap();
    assert_eq!(st.world.name_of(lamp), "lamp");
    assert_eq!(
        overrides(&mut st.world),
        json!([{
            "insert": "lamp", "parent": "a", "index": 1,
            "components": { "Transform": { "translation": [0, 2, 0] }, "Glow": {} }
        }])
    );
    // An edit of the inserted entity is an ordinary field override.
    ok(
        &mut st,
        "entity.set",
        json!({ "id": "src/+lamp", "component": "Glow", "field": "intensity", "value": 9.0 }),
    );
    st.world
        .set("src", "Procedural", "seed", Value::Int(3))
        .unwrap();
    st.world.regenerate_changed();
    assert_eq!(
        st.world.get("src/+lamp", "Glow", "intensity").unwrap(),
        Value::Float(9.0)
    );
    st.world
        .set("src", "Procedural", "seed", Value::Int(0))
        .unwrap();
    st.world.regenerate_changed();

    assert!(st.history.undo(&mut st.world).unwrap());
    undo_restores(&mut st, &before, hash);

    // A prefab insert under the source, named after the prefab's root.
    ok(
        &mut st,
        "proc.insert",
        json!({ "source": "src", "key": "rock", "prefab": "prefabs/rock.prefab.json" }),
    );
    let rock = st.world.entity("src/+rock").unwrap();
    assert_eq!(st.world.name_of(rock), "Rock");
    assert_eq!(children(&st.world, "src/+rock"), ["src/+rock/~pebble"]);
}

#[test]
fn overrides_whose_target_or_parent_is_gone_are_kept_as_orphans() {
    let (_dir, mut st) = world();
    ok(
        &mut st,
        "proc.move",
        json!({ "id": "src/b", "parent": "src/a" }),
    );
    ok(
        &mut st,
        "proc.insert",
        json!({ "source": "src", "key": "lamp", "parent": "src/a/x", "components": {} }),
    );
    ok(
        &mut st,
        "proc.replace",
        json!({ "id": "src/a/x", "components": {} }),
    );
    let o = run(
        &mut st,
        "entity.set",
        json!({ "id": "src", "component": "Rows", "field": "with_a", "value": false }),
    );
    assert!(o.ok, "{:#?}", o.diagnostics);
    let report = st.world.regenerate_changed();
    let mut orphans: Vec<(String, String)> = report
        .orphans
        .iter()
        .map(|o| (o.target.clone(), o.structural.unwrap().to_string()))
        .collect();
    orphans.sort();
    assert_eq!(
        orphans,
        [
            ("+lamp".to_string(), "insert".to_string()),
            ("a/x".to_string(), "replacement".to_string()),
            ("b".to_string(), "move".to_string()),
        ]
    );
    // `b` stays where it was generated; the overrides stay in the file.
    assert_eq!(children(&st.world, "src"), ["src/b", "src/c"]);
    assert_eq!(overrides(&mut st.world).as_array().unwrap().len(), 3);
}

#[test]
fn bad_structural_edits_are_refused_and_change_nothing() {
    let (_dir, mut st) = world();
    let before = saved(&mut st.world);
    for (command, args) in [
        ("proc.move", json!({ "id": "src/a", "parent": "src/a/x" })),
        ("proc.move", json!({ "id": "src", "parent": "src/a" })),
        ("proc.move", json!({ "id": "src/a", "parent": "elsewhere" })),
        ("proc.replace", json!({ "id": "src/a" })),
        (
            "proc.replace",
            json!({ "id": "src/a", "prefab": "prefabs/two.prefab.json" }),
        ),
        (
            "proc.replace",
            json!({ "id": "src/a", "prefab": "prefabs/missing.prefab.json" }),
        ),
        (
            "proc.replace",
            json!({ "id": "src/a", "components": { "NoSuchThing": {} } }),
        ),
        (
            "proc.insert",
            json!({ "source": "src", "key": "+x", "components": {} }),
        ),
        (
            "proc.insert",
            json!({ "source": "src", "key": "a/b", "components": {} }),
        ),
        ("proc.clear_structure", json!({ "source": "src" })),
    ] {
        let o = run(&mut st, command, args.clone());
        assert!(!o.ok, "{command} {args}");
        assert_eq!(
            codes(&o)[0],
            "RE4023",
            "{command} {args}: {:#?}",
            o.diagnostics
        );
        assert_eq!(saved(&mut st.world), before, "{command} {args}");
    }
    ok(
        &mut st,
        "proc.insert",
        json!({ "source": "src", "key": "lamp", "components": {} }),
    );
    let o = run(
        &mut st,
        "proc.insert",
        json!({ "source": "src", "key": "lamp", "components": {} }),
    );
    assert_eq!(codes(&o), ["RE4023"]);
    // Reparenting a generated entity points at proc.move.
    let o = run(
        &mut st,
        "entity.reparent",
        json!({ "id": "src/b", "parent": "src/a" }),
    );
    assert!(!o.ok);
    assert!(
        o.diagnostics[0].message.contains("proc.move"),
        "{:#?}",
        o.diagnostics
    );
}

#[test]
fn clear_structure_removes_them_and_undo_puts_them_back_in_order() {
    let (_dir, mut st) = world();
    ok(
        &mut st,
        "proc.move",
        json!({ "id": "src/b", "parent": "src/a" }),
    );
    ok(
        &mut st,
        "entity.set",
        json!({ "id": "src/c", "component": "Transform", "field": "translation", "value": [9, 0, 0] }),
    );
    ok(
        &mut st,
        "proc.insert",
        json!({ "source": "src", "key": "lamp", "components": {} }),
    );
    let before = saved(&mut st.world);
    let hash = st.world.state_hash();
    let r = ok(&mut st, "proc.clear_structure", json!({ "source": "src" }));
    assert_eq!(
        r["cleared"],
        json!([{ "key": "b", "kind": "move" }, { "key": "+lamp", "kind": "insert" }])
    );
    assert_eq!(overrides(&mut st.world).as_array().unwrap().len(), 1);
    assert!(st.world.entity("src/+lamp").is_err());
    assert_eq!(children(&st.world, "src"), ["src/a", "src/b", "src/c"]);
    undo_restores(&mut st, &before, hash);

    let r = ok(
        &mut st,
        "proc.clear_structure",
        json!({ "source": "src", "key": "+lamp" }),
    );
    assert_eq!(r["cleared"], json!([{ "key": "+lamp", "kind": "insert" }]));
}
