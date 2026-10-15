//! `scene.dump_tree` over a compat place, and `compat.coverage` through the
//! registry.

use re_command::{AllowAll, CommandRegistry, EditState};
use re_compat::ClassRegistry;
use re_scene::EntityDoc;
use serde_json::json;

fn registry() -> CommandRegistry<EditState> {
    let mut r = CommandRegistry::new();
    re_command::register_builtin_commands(&mut r);
    re_agent::compat::register_commands(&mut r);
    r
}

/// `game` with a Folder holding a tagged, attributed Model and a native
/// entity.
fn place() -> EditState {
    let mut s = EditState::new();
    let classes = ClassRegistry::builtin().unwrap();
    let game = re_compat::scene::create_place(&mut s.world, &classes, &[]).unwrap();
    let folder = re_compat::scene::spawn_instance(
        &mut s.world,
        &classes,
        "Folder",
        Some("stuff"),
        "Stuff",
        Some(game),
    )
    .unwrap();
    let _ = folder;
    let docs: Vec<EntityDoc> = serde_json::from_value(json!([
        {
            "id": "crate", "name": "Crate", "parent": "stuff",
            "components": {
                "ClassMarker": { "class": "Model" },
                "Attributes": { "values": { "health": 3 } },
                "Tags": { "tags": ["breakable"] }
            }
        },
        { "id": "lamp", "name": "Lamp", "parent": "stuff" }
    ]))
    .unwrap();
    s.world.spawn_docs(&docs, None).unwrap();
    s.world.update();
    s
}

#[test]
fn the_tree_shows_classes_attributes_and_tags() {
    let reg = registry();
    let mut s = place();
    let out = reg.dispatch(
        &mut s,
        "scene.dump_tree",
        json!({ "root": "game" }),
        false,
        &AllowAll,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let game = &out.data["roots"][0];
    assert_eq!(game["id"], "game");
    assert_eq!(game["class"], "DataModel");
    let stuff = &game["children"][0];
    assert_eq!(stuff["class"], "Folder");
    let kids = stuff["children"].as_array().unwrap();
    assert_eq!(kids.len(), 2);
    let crate_ = kids.iter().find(|k| k["id"] == "crate").unwrap();
    assert_eq!(crate_["class"], "Model");
    assert_eq!(crate_["attributes"]["health"], 3.0);
    assert_eq!(crate_["tags"], json!(["breakable"]));
    assert!(crate_.get("components").is_none());
    let lamp = kids.iter().find(|k| k["id"] == "lamp").unwrap();
    assert!(
        lamp.get("class").is_none(),
        "a native entity has no class: {lamp}"
    );
    assert_eq!(out.data["nodes"], 4);
    assert_eq!(out.data["truncated"], false);
}

#[test]
fn depth_and_max_nodes_cut_the_tree() {
    let reg = registry();
    let mut s = place();
    let out = reg.dispatch(
        &mut s,
        "scene.dump_tree",
        json!({ "depth": 1 }),
        false,
        &AllowAll,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let stuff = &out.data["roots"][0]["children"][0];
    assert_eq!(stuff["children_not_listed"], 2);
    assert!(stuff.get("children").is_none());

    let out = reg.dispatch(
        &mut s,
        "scene.dump_tree",
        json!({ "max_nodes": 3, "components": true }),
        false,
        &AllowAll,
    );
    assert_eq!(out.data["nodes"], 3);
    assert_eq!(out.data["truncated"], true);
    let stuff = &out.data["roots"][0]["children"][0];
    assert_eq!(stuff["children_not_listed"], 1);
    assert!(stuff["components"]["ClassMarker"].is_object());

    let missing = reg.dispatch(
        &mut s,
        "scene.dump_tree",
        json!({ "root": "nope" }),
        false,
        &AllowAll,
    );
    assert!(!missing.ok);
}

#[test]
fn coverage_is_queryable_by_name() {
    let reg = registry();
    let mut s = EditState::new();
    let out = reg.dispatch(
        &mut s,
        "compat.coverage",
        json!({ "name": "Folder" }),
        false,
        &AllowAll,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(out.data["rows"][0]["key"], "Folder");
    assert!(out.data["totals"].is_object());
    let typo = reg.dispatch(
        &mut s,
        "compat.coverage",
        json!({ "name": "Foldr" }),
        false,
        &AllowAll,
    );
    assert_eq!(typo.diagnostics[0].code.to_string(), "RE8018");
    assert!(typo.diagnostics[0].message.contains("\"Folder\""));
}
