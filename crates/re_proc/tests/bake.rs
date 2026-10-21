//! `proc.bake` (ADR-0043 §2): generated entities become authored ones with a
//! `Baked` record, derived assets become files, and undo gives back exactly
//! the world before the bake.

mod support;

use std::path::Path;
use std::sync::Arc;

use re_command::{AllowAll, CommandOutcome, CommandRegistry, EditState};
use re_proc::bake::{Baked, read_heightfield};
use re_proc::{Evaluator, GraphStore, register_commands, register_generator};
use re_reflect::{Value, component};
use re_scene::ecs as bevy_ecs;
use re_scene::{Entity, SaveOptions, World};
use serde_json::{Value as Json, json};
use support::{fixtures, island};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

component! {
    /// An authored entity's reference to another one.
    pub struct Aim {
        /// What it aims at.
        pub target: Option<Entity> = None,
    }
}

/// The island source in a project copied to a temporary directory (bake
/// writes files there), with `Baked` registered, generated once.
fn world(overrides: Json) -> (tempfile::TempDir, EditState) {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&fixtures(), dir.path());
    let mut st = EditState::new();
    register_generator(&mut st.world.generators);
    st.world.registry.register::<Baked>(&mut st.world.ecs);
    st.world.registry.register::<Aim>(&mut st.world.ecs);
    let mut store = GraphStore::with_evaluator(
        Arc::new(Evaluator::default()),
        Some(dir.path().to_path_buf()),
    );
    store.insert("island", island(1));
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
                    "Procedural": { "generator": "graph", "seed": 1 },
                    "Transform": {}
                },
                "overrides": overrides
            }
        ]
    });
    let doc = re_scene::scene::from_json(&scene.to_string()).unwrap();
    st.world.load(&doc).unwrap();
    let report = st.world.regenerate_changed();
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    (dir, st)
}

fn bake(st: &mut EditState, args: Json) -> CommandOutcome {
    let mut r = CommandRegistry::new();
    register_commands(&mut r);
    r.dispatch(st, "proc.bake", args, false, &AllowAll)
}

fn codes(o: &CommandOutcome) -> Vec<String> {
    o.diagnostics.iter().map(|d| d.code.to_string()).collect()
}

fn text(w: &World, id: &str, component: &str, field: &str) -> String {
    match w.get(id, component, field).unwrap() {
        Value::Str(s) => s,
        other => panic!("{other:?}"),
    }
}

fn saved(w: &mut World) -> String {
    re_scene::scene::to_json(&w.save(SaveOptions::default()))
}

#[test]
fn a_full_bake_makes_authored_entities_and_undo_restores_the_world() {
    let crate_key = "crates/65538";
    let overrides = json!([
        { "target": format!("{crate_key}/crate"), "component": "RigidBody", "field": "mass", "value": 99.0 },
        { "target": "crates/196611", "deleted": true }
    ]);
    let (dir, mut st) = world(overrides);
    let before_scene = saved(&mut st.world);
    let before_hash = st.world.state_hash();
    let generated = st
        .world
        .ecs
        .resource::<GraphStore>()
        .report("island")
        .unwrap();
    let entities = generated.entities.unwrap();

    let out = bake(&mut st, json!({ "source": "island" }));
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(
        out.data["baked"],
        json!(entities - 3),
        "one crate is deleted"
    );
    assert_eq!(out.data["full"], json!(true));

    // Baked ids, provenance, the override's value, remapped parents.
    let w = &mut st.world;
    let body = format!("island.{}.crate", crate_key.replace('/', "."));
    assert_eq!(
        w.get(&body, "RigidBody", "mass").unwrap(),
        Value::Float(99.0)
    );
    assert_eq!(text(w, &body, "Baked", "source"), "island");
    assert_eq!(text(w, &body, "Baked", "key"), format!("{crate_key}/crate"));
    assert_eq!(text(w, &body, "Baked", "generator"), "graph");
    assert_eq!(w.get(&body, "Baked", "seed").unwrap(), Value::Int(1));
    assert_eq!(text(w, &body, "Baked", "stamp").len(), 64);
    let parent = re_scene::hierarchy::parent(&w.ecs, w.entity(&body).unwrap()).unwrap();
    assert_eq!(
        w.id_of(parent),
        Some(format!("island.{}", crate_key.replace('/', ".")).as_str())
    );
    assert!(
        w.entity("island.crates.196611").is_err(),
        "deleted stays deleted"
    );

    // Derived assets are files now, and they load.
    let mesh = text(w, "island.ground.terrain", "MeshRef", "asset");
    assert!(
        mesh.starts_with("assets/baked/island/") && mesh.ends_with(".glb"),
        "{mesh}"
    );
    assert!(dir.path().join(&mesh).is_file());
    let hf = text(w, "island.ground.terrain", "Collider", "source");
    assert!(hf.ends_with(".heightfield"), "{hf}");
    let bytes = std::fs::read(dir.path().join(&hf)).unwrap();
    let field = read_heightfield(&bytes).unwrap();
    assert_eq!(field.n, 33);

    // The source is an ordinary entity; its generated output goes at the
    // next regeneration.
    assert!(w.get("island", "Procedural", "seed").is_err());
    assert!(w.get("island", "GraphSource", "builder").is_err());
    let regen = w.regenerate_changed();
    assert!(regen.diagnostics.is_empty(), "{:#?}", regen.diagnostics);
    assert!(w.entity("island/ground/terrain").is_err());
    let after = saved(w);
    assert!(after.contains("\"Baked\""));
    assert!(!after.contains("derived:"));

    // Undo: the same saved scene and state hash as before.
    assert!(st.history.undo(&mut st.world).unwrap());
    let regen = st.world.regenerate_changed();
    assert!(regen.diagnostics.is_empty(), "{:#?}", regen.diagnostics);
    assert_eq!(saved(&mut st.world), before_scene);
    assert_eq!(st.world.state_hash(), before_hash);
}

#[test]
fn a_partial_bake_keeps_the_source_generating_the_rest() {
    let (_dir, mut st) = world(json!([]));
    let before_scene = saved(&mut st.world);
    let before_hash = st.world.state_hash();
    let key = "crates/65537";
    let out = bake(&mut st, json!({ "source": "island", "keys": [key] }));
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(out.data["full"], json!(false));
    assert_eq!(
        out.data["baked"],
        json!(3),
        "the crate and its two children"
    );
    let w = &mut st.world;
    w.regenerate_changed();
    assert!(w.entity("island.crates.65537").is_ok());
    assert!(w.entity("island.crates.65537.lid").is_ok());
    assert!(
        w.entity(&format!("island/{key}")).is_err(),
        "deleted by override"
    );
    assert!(w.entity("island/crates/65538").is_ok());
    assert!(w.entity("island/ground/terrain").is_ok());
    assert_eq!(
        w.get("island", "Procedural", "seed").unwrap(),
        Value::Int(1)
    );

    assert!(st.history.undo(&mut st.world).unwrap());
    st.world.regenerate_changed();
    assert_eq!(saved(&mut st.world), before_scene);
    assert_eq!(st.world.state_hash(), before_hash);
}

#[test]
fn undo_right_after_a_full_bake_restores_the_world() {
    let (_dir, mut st) = world(json!([]));
    let before_hash = st.world.state_hash();
    let out = bake(&mut st, json!({ "source": "island" }));
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert!(st.history.undo(&mut st.world).unwrap());
    st.world.regenerate_changed();
    assert_eq!(st.world.state_hash(), before_hash);
    // Redo bakes again, to the same ids.
    assert!(st.history.redo(&mut st.world).unwrap());
    st.world.regenerate_changed();
    assert!(st.world.entity("island.ground.terrain").is_ok());
    assert!(st.world.entity("island/ground/terrain").is_err());
}

#[test]
fn bake_refuses_what_it_cannot_bake() {
    let (_dir, mut st) = world(json!([]));
    let before = st.world.state_hash();
    for args in [
        json!({ "source": "island", "keys": ["crates/nope"] }),
        json!({ "source": "island", "keys": ["crates/65537/lid"] }),
        json!({ "source": "world" }),
    ] {
        let out = bake(&mut st, args.clone());
        assert!(!out.ok, "{args}");
        assert_eq!(codes(&out), ["RE4021"], "{args}: {:#?}", out.diagnostics);
    }
    assert_eq!(st.world.state_hash(), before);
}

#[test]
fn baked_meshes_load_through_the_gltf_importer_bit_for_bit() {
    let (dir, mut st) = world(json!([]));
    let store = st.world.ecs.resource::<GraphStore>();
    let derived = text(&st.world, "island/ground/terrain", "MeshRef", "asset");
    let mesh = store.mesh(&derived).unwrap();
    let out = bake(&mut st, json!({ "source": "island" }));
    assert!(out.ok, "{:#?}", out.diagnostics);
    let path = text(&st.world, "island.ground.terrain", "MeshRef", "asset");
    let bytes = std::fs::read(dir.path().join(&path)).unwrap();
    let imported =
        re_assets::import_gltf(&bytes, None, re_assets::AssetId::new_source(), &path).unwrap();
    assert!(imported.warnings.is_empty(), "{:#?}", imported.warnings);
    assert_eq!(imported.meshes.len(), 1);
    let back = &imported.meshes[0].mesh;
    assert_eq!(back.indices, mesh.indices);
    assert_eq!(back.vertices.len(), mesh.vertices.len());
    let bits = |v: &re_types::mesh::Vertex| {
        v.pos
            .iter()
            .chain(&v.normal)
            .chain(&v.uv)
            .chain(&v.color)
            .map(|x| x.to_bits())
            .collect::<Vec<_>>()
    };
    for (a, b) in back.vertices.iter().zip(&mesh.vertices) {
        assert_eq!(bits(a), bits(b));
    }
}

#[test]
fn references_to_the_output_follow_it_and_undo_puts_them_back() {
    let (_dir, mut st) = world(json!([]));
    let mut r = CommandRegistry::new();
    re_command::register_builtin_commands(&mut r);
    let out = r.dispatch(
        &mut st,
        "scene.apply",
        json!({ "ops": [
            { "op": "spawn", "id": "cam", "components": { "Aim": { "target": "island/crates/65537" } } }
        ] }),
        false,
        &AllowAll,
    );
    assert!(out.ok, "{:#?}", out.diagnostics);
    let before_scene = saved(&mut st.world);
    let before_hash = st.world.state_hash();

    let out = bake(&mut st, json!({ "source": "island" }));
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert!(
        st.world.entity("island/crates/65537").is_err(),
        "gone in the bake itself"
    );
    let aim = st.world.get_doc("cam", "Aim", "target").unwrap();
    assert_eq!(aim, re_reflect::Doc::Str("island.crates.65537".into()));
    let regen = st.world.regenerate_changed();
    assert!(regen.diagnostics.is_empty(), "{:#?}", regen.diagnostics);

    assert!(st.history.undo(&mut st.world).unwrap());
    st.world.regenerate_changed();
    assert_eq!(saved(&mut st.world), before_scene);
    assert_eq!(st.world.state_hash(), before_hash);
}

#[test]
fn the_source_can_be_despawned_right_after_a_bake_and_brought_back() {
    let (_dir, mut st) = world(json!([]));
    let out = bake(&mut st, json!({ "source": "island" }));
    assert!(out.ok, "{:#?}", out.diagnostics);
    let after_bake = st.world.state_hash();
    let mut r = CommandRegistry::new();
    re_command::register_builtin_commands(&mut r);
    let out = r.dispatch(
        &mut st,
        "scene.apply",
        json!({ "ops": [{ "op": "despawn", "id": "island" }] }),
        false,
        &AllowAll,
    );
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert!(st.history.undo(&mut st.world).unwrap());
    assert_eq!(st.world.state_hash(), after_bake);
}

#[test]
fn an_empty_key_list_is_refused() {
    let (_dir, mut st) = world(json!([]));
    let before = st.world.state_hash();
    let out = bake(&mut st, json!({ "source": "island", "keys": [] }));
    assert_eq!(codes(&out), ["RE4021"]);
    assert_eq!(st.world.state_hash(), before);
}
