//! Prints what an agent or the editor sees: an agent's edit as JSON
//! operations, the resulting scene, the transaction log, typical error
//! messages and a dry run. With an output directory it also writes
//! `scene.json`, `registry.json`, `scene.schema.json` and `log.jsonl` there.
//!
//! ```sh
//! cargo run --locked -p re_command --example agent_demo -- out/demo
//! ```
//!
//! The demo's `Primitive` and `PillarGrid` components and its `pillar_grid`
//! generator are test fixtures shared with the integration tests.

#[path = "../../re_scene/tests/support/mod.rs"]
mod support;

use std::path::PathBuf;

use re_command::{
    AllowAll, CommandRegistry, EditState, Op, Origin, Transaction, register_builtin_commands,
};
use re_scene::scene::{self, SaveOptions, collapse_scalar_lists, to_pretty_json};
use serde_json::json;

fn main() {
    let out_dir = std::env::args().nth(1).map(PathBuf::from);
    let mut state = EditState {
        world: support::world(),
        history: Default::default(),
    };
    let mut commands = CommandRegistry::new();
    register_builtin_commands(&mut commands);

    // An agent's edit, exactly as it would arrive over a tool call.
    let agent_json = r#"[
      {"op": "spawn", "id": "level", "name": "Level"},
      {"op": "spawn", "id": "sun", "name": "Sun", "parent": "level",
       "components": {"Transform": {"translation": [0, 30, 0]},
                      "Light": {"kind": "Directional", "intensity": 5000, "shadows": true}}},
      {"op": "spawn", "id": "yard", "name": "Yard", "parent": "level",
       "components": {"Transform": {},
                      "Procedural": {"generator": "pillar_grid", "seed": 42},
                      "PillarGrid": {"rows": 2, "cols": 2, "spacing": 6.5}}},
      {"op": "spawn", "id": "crate", "name": "Crate", "parent": "level",
       "components": {"Transform": {"translation": {"x": 0.1, "y": 2, "z": -3}},
                      "MeshRef": {"asset": "meshes/crate.glb"},
                      "RigidBody": {"kind": "Dynamic", "mass": 12.5},
                      "Collider": {"half_extents": [0.5, 0.5, 0.5]}}}
    ]"#;
    let ops: Vec<Op> = serde_json::from_str(agent_json).expect("demo ops parse");
    state
        .history
        .execute(
            &mut state.world,
            Transaction::new("block out level", Origin::Agent("claude".into()), ops),
        )
        .expect("demo edit applies");
    let report = state.world.update();
    println!(
        "=== regeneration ===\nspawned {} generated entities from {:?}",
        report.spawned.len(),
        report.sources
    );

    // An editor tweak to one generated pillar becomes an override.
    let tweak = commands.dispatch(
        &mut state,
        "entity.set",
        json!({"id": "yard/pillar_1_0", "component": "Primitive", "field": "color", "value": [0.9, 0.1, 0.1, 1]}),
        false,
        &AllowAll,
    );
    assert!(tweak.ok, "{:?}", tweak.diagnostics);

    // A dry run reports the changes without making them.
    let before = state.world.state_hash();
    let dry = commands.dispatch(
        &mut state,
        "scene.apply",
        json!({"label": "raise sun", "dry_run": true, "ops": [
            {"op": "set_field", "id": "sun", "component": "Transform", "field": "translation", "value": [0, 40, 0]}
        ]}),
        false,
        &AllowAll,
    );
    assert_eq!(state.world.state_hash(), before);
    println!(
        "=== dry run ===\n{}",
        serde_json::to_string(&dry).expect("outcome serializes")
    );

    let json = scene::to_json(&state.world.save(SaveOptions::default()));
    println!("=== scene.json ===\n{json}");
    let log = state.history.log_since_jsonl(0);
    println!("=== log.jsonl ===\n{log}");

    println!("=== errors ===");
    for bad in [
        r#"{"op": "set_field", "id": "crate", "component": "RigidBody", "field": "mas", "value": 3}"#,
        r#"{"op": "set_field", "id": "crate", "component": "Rigidbody", "field": "mass", "value": 3}"#,
        r#"{"op": "set_field", "id": "crate", "component": "RigidBody", "field": "kind", "value": "Sleeping"}"#,
        r#"{"op": "set_field", "id": "crate", "component": "Transform", "field": "translation", "value": [1, 2]}"#,
        r#"{"op": "reparent", "id": "level", "parent": "crate"}"#,
        r#"{"op": "reparent", "id": "yard/pillar_0_0", "parent": null}"#,
        r#"{"op": "despawn", "id": "crates"}"#,
        r#"{"op": "spawn", "id": "sun"}"#,
        r#"{"op": "add_component", "id": "crate", "component": "RigidBody"}"#,
    ] {
        let op: Op = serde_json::from_str(bad).expect("error case parses");
        let err = state
            .history
            .execute(
                &mut state.world,
                Transaction::new("bad", Origin::Agent("claude".into()), vec![op]),
            )
            .expect_err("error case fails");
        println!("{bad}\n  -> {err}");
    }
    let err = serde_json::from_str::<Op>(r#"{"op": "set_feild", "id": "crate"}"#)
        .expect_err("typo fails");
    println!("set_feild -> {err}");
    let unknown = commands.dispatch(&mut state, "entity.sett", json!({}), false, &AllowAll);
    println!("entity.sett -> {}", unknown.diagnostics[0]);

    let describe = commands.dispatch(&mut state, "describe", json!({}), false, &AllowAll);
    let registry_json = collapse_scalar_lists(&to_pretty_json(&describe.data));
    let schema_json = collapse_scalar_lists(&to_pretty_json(&state.world.scene_json_schema()));
    let log = state.history.log_since_jsonl(0);
    match out_dir {
        Some(dir) => {
            std::fs::create_dir_all(&dir).expect("create output directory");
            for (name, text) in [
                ("scene.json", &json),
                ("registry.json", &registry_json),
                ("scene.schema.json", &schema_json),
                ("log.jsonl", &log),
            ] {
                std::fs::write(dir.join(name), text).expect("write output file");
            }
            println!(
                "=== wrote scene.json, registry.json, scene.schema.json, log.jsonl to {} ===",
                dir.display()
            );
        }
        None => println!("=== registry.json ===\n{registry_json}"),
    }
}
