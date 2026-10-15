//! The compat scene conventions: building a place, the fixture project
//! loading and saving byte-identically, and the convention checks.

mod common;

use std::path::Path;

use common::test_registry;
use re_compat::scene::{self, create_place, spawn_instance, validate};
use re_compat::{ClassRegistry, CompatApi, CompatConfig, GAME_ID, script_context};
use re_reflect::Value;
use re_scene::{SaveOptions, ScriptContext, World};

const PLACE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scene/place");

/// The fixture place, built through the conventions.
fn build_place(r: &ClassRegistry) -> World {
    let mut w = World::new();
    let game = create_place(
        &mut w,
        r,
        &["Workspace", "ServerScriptService", "ReplicatedStorage"],
    )
    .unwrap();
    let _ = game;
    let ws = w.entity("Workspace").unwrap();
    let sss = w.entity("ServerScriptService").unwrap();
    let rs = w.entity("ReplicatedStorage").unwrap();
    let tower = spawn_instance(&mut w, r, "Model", Some("tower"), "Tower", Some(ws)).unwrap();
    let base = spawn_instance(
        &mut w,
        r,
        "TestBlock",
        Some("tower-base"),
        "Base",
        Some(tower),
    )
    .unwrap();
    r.set_property(&mut w, base, "Count", Value::Int(4))
        .unwrap();
    let script = |w: &mut World, class: &str, id: &str, name: &str, parent, path: &str, ctx| {
        let e = spawn_instance(w, r, class, Some(id), name, Some(parent)).unwrap();
        w.ecs.entity_mut(e).insert(re_scene::ScriptRef {
            path: path.to_string(),
            context: ctx,
        });
        e
    };
    script(
        &mut w,
        "Script",
        "greeter",
        "Greeter",
        sss,
        "scripts/server/greeter.luau",
        ScriptContext::Server,
    );
    let shared = spawn_instance(&mut w, r, "Folder", Some("shared"), "Shared", Some(rs)).unwrap();
    script(
        &mut w,
        "ModuleScript",
        "strings",
        "Strings",
        shared,
        "scripts/shared/strings.luau",
        ScriptContext::Module,
    );
    let hud = script(
        &mut w,
        "Script",
        "hud",
        "Hud",
        rs,
        "scripts/client/hud.luau",
        ScriptContext::Client,
    );
    w.set(
        "hud",
        "Compat_BaseScript",
        "RunContext",
        Value::Enum("Client".into()),
    )
    .unwrap();
    let _ = hud;
    w
}

fn scene_text(w: &mut World) -> String {
    re_scene::scene::to_json(&w.save(SaveOptions::default()))
}

#[test]
fn fixture_project_loads_validates_and_saves_byte_identically() {
    let r = test_registry();
    let path = Path::new(PLACE).join("scenes/main.scene.json");
    let built = scene_text(&mut build_place(&r));
    if std::env::var_os("RE_COMPAT_BLESS").is_some() {
        std::fs::write(&path, &built).unwrap();
    }
    // A Windows checkout may turn the line ends into CRLF.
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\r\n", "\n");
    assert_eq!(
        text, built,
        "the fixture differs from the conventions (RE_COMPAT_BLESS=1 rewrites it)"
    );

    let mut w = World::new();
    r.register_components(&mut w).unwrap();
    w.load(&re_scene::scene::from_json(&text).unwrap()).unwrap();
    let problems = validate(&mut w, &r);
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(scene_text(&mut w), text);

    // Every script the scene names exists in the project.
    let mut q = w.ecs.query::<&re_scene::ScriptRef>();
    let paths: Vec<String> = q.iter(&w.ecs).map(|s| s.path.clone()).collect();
    assert_eq!(paths.len(), 3);
    for p in paths {
        assert!(Path::new(PLACE).join(&p).is_file(), "{p}");
    }

    let toml = std::fs::read_to_string(Path::new(PLACE).join("real.toml")).unwrap();
    let cfg = CompatConfig::from_project_toml(&toml).unwrap().unwrap();
    assert_eq!(cfg.api, CompatApi::Roblox);
}

#[test]
fn the_place_has_its_root_and_services() {
    let r = test_registry();
    let w = build_place(&r);
    let game = w.entity(GAME_ID).unwrap();
    assert_eq!(r.class_of(&w, game).unwrap().name, "DataModel");
    for s in ["Workspace", "ServerScriptService", "ReplicatedStorage"] {
        let e = w.entity(s).unwrap();
        assert_eq!(re_scene::hierarchy::parent(&w.ecs, e), Some(game));
        assert!(w.ecs.get::<re_compat::ServiceTag>(e).is_some(), "{s}");
        assert_eq!(w.name_of(e), s);
    }
    let mut w = World::new();
    let err = create_place(&mut w, &r, &["Folder"]).unwrap_err();
    assert_eq!(err.code().to_string(), "RE9004");
}

#[test]
fn script_contexts_follow_class_and_run_context() {
    assert_eq!(script_context("Script", None), Some(ScriptContext::Server));
    assert_eq!(
        script_context("Script", Some("Legacy")),
        Some(ScriptContext::Server)
    );
    assert_eq!(
        script_context("Script", Some("Server")),
        Some(ScriptContext::Server)
    );
    assert_eq!(
        script_context("Script", Some("Client")),
        Some(ScriptContext::Client)
    );
    assert_eq!(script_context("Script", Some("Plugin")), None);
    assert_eq!(
        script_context("LocalScript", Some("Server")),
        Some(ScriptContext::Client)
    );
    assert_eq!(
        script_context("ModuleScript", None),
        Some(ScriptContext::Module)
    );
    assert_eq!(script_context("Folder", None), None);
}

#[test]
fn convention_breaks_are_reported_with_re9004() {
    let r = test_registry();
    let messages = |w: &mut World| -> Vec<String> {
        validate(w, &r)
            .into_iter()
            .map(|d| {
                assert_eq!(d.code.to_string(), "RE9004");
                d.message
            })
            .collect()
    };

    // A script whose context does not match its class.
    let mut w = build_place(&r);
    w.set(
        "greeter",
        "ScriptRef",
        "context",
        Value::Enum("Client".into()),
    )
    .unwrap();
    assert_eq!(
        messages(&mut w),
        ["a Script instance must have script context server, found client"]
    );

    // A service with the wrong id, and one not under game.
    let mut w = build_place(&r);
    let tower = w.entity("tower").unwrap();
    spawn_instance(
        &mut w,
        &r,
        "ServerScriptService",
        Some("extra"),
        "Extra",
        Some(tower),
    )
    .unwrap();
    let m = messages(&mut w);
    assert!(
        m.contains(
            &"a ServerScriptService service must have the id \"ServerScriptService\"".to_string()
        ),
        "{m:?}"
    );
    assert!(
        m.contains(&"the ServerScriptService service must be a child of \"game\"".to_string()),
        "{m:?}"
    );

    // An unknown class and a missing ServiceTag.
    let mut w = build_place(&r);
    w.set("shared", "ClassMarker", "class", Value::Str("Nope".into()))
        .unwrap();
    let ws = w.entity("Workspace").unwrap();
    w.ecs.entity_mut(ws).remove::<re_compat::ServiceTag>();
    let m = messages(&mut w);
    assert!(m.contains(&"unknown class \"Nope\"".to_string()), "{m:?}");
    assert!(
        m.contains(&"Workspace is a service but has no ServiceTag".to_string()),
        "{m:?}"
    );

    // No game root at all.
    let mut w = World::new();
    r.register_components(&mut w).unwrap();
    assert_eq!(
        messages(&mut w),
        ["the scene has no root entity with id \"game\""]
    );
}

#[test]
fn compat_config_reads_and_writes_the_section() {
    assert_eq!(
        CompatConfig::from_project_toml("[project]\nname = \"x\"\n").unwrap(),
        None
    );
    let cfg = CompatConfig::from_project_toml("[compat]\napi = \"roblox\"\n")
        .unwrap()
        .unwrap();
    assert_eq!(cfg.to_toml(), "[compat]\napi = \"roblox\"\n");
    let err = CompatConfig::from_project_toml("[compat]\napi = \"other\"\n").unwrap_err();
    assert_eq!(err.code().to_string(), "RE9004");
    let err =
        CompatConfig::from_project_toml("[compat]\napi = \"roblox\"\nnope = 1\n").unwrap_err();
    assert!(err.to_string().contains("nope"), "{err}");
}

#[test]
fn compat_config_animations_round_trip_and_are_checked() {
    let text = "[compat]\napi = \"roblox\"\n\n[compat.animations]\n\"rbxassetid://0042\" = \"builtin:r15_walk\"\n\"7\" = \"game.ReplicatedStorage.Wave\"\n";
    let cfg = CompatConfig::from_project_toml(text).unwrap().unwrap();
    assert_eq!(cfg.animations["42"], "builtin:r15_walk");
    assert_eq!(cfg.animations["7"], "game.ReplicatedStorage.Wave");
    let again = CompatConfig::from_project_toml(&cfg.to_toml())
        .unwrap()
        .unwrap();
    assert_eq!(again, cfg);
    for bad in [
        "\"42\" = \"builtin:r15_walk\"\n\"rbxassetid://42\" = \"builtin:r6_walk\"",
        "\"walk\" = \"builtin:r15_walk\"",
        "\"42\" = \"builtin:nope\"",
        "\"42\" = \"game..Wave\"",
        "\"42\" = \"https://example.com/a\"",
    ] {
        let err = CompatConfig::from_project_toml(&format!(
            "[compat]\napi = \"roblox\"\n[compat.animations]\n{bad}\n"
        ))
        .unwrap_err();
        assert_eq!(err.code().to_string(), "RE9202", "{bad}: {err}");
    }
}

#[test]
fn compat_config_round_trips_the_session_values() {
    // An existing section without the new keys writes back unchanged.
    let toml = std::fs::read_to_string(Path::new(PLACE).join("real.toml")).unwrap();
    let doc: toml::Table = toml.parse().unwrap();
    let cfg = CompatConfig::from_project_toml(&toml).unwrap().unwrap();
    assert_eq!(cfg.game, re_compat::scene::GameInfo::default());
    assert_eq!(cfg.seed, 0);
    let section: toml::Table = cfg.to_toml().parse().unwrap();
    assert_eq!(section["compat"], doc["compat"]);
    // Non-default values survive a write and a read.
    let text = "[compat]\napi = \"roblox\"\nseed = 7\n\n[compat.game]\nplace_id = 42\n\
                game_id = 0\ncreator_id = 0\njob_id = \"job\"\nprivate_server_id = \"\"\n\
                private_server_owner_id = 0\nstudio = false\n";
    let cfg = CompatConfig::from_project_toml(text).unwrap().unwrap();
    assert_eq!(cfg.seed, 7);
    assert_eq!(cfg.game.place_id, 42);
    assert!(!cfg.game.studio);
    assert_eq!(cfg.to_toml(), text);
    assert_eq!(
        CompatConfig::from_project_toml(&cfg.to_toml())
            .unwrap()
            .unwrap(),
        cfg
    );
}

#[test]
fn instance_doc_lists_the_class_components() {
    let r = test_registry();
    let d = scene::instance_doc(&r, "Workspace", "Workspace", "Workspace", Some("game")).unwrap();
    let names: Vec<&str> = d.components.keys().map(String::as_str).collect();
    assert_eq!(names, ["ClassMarker", "Compat_Instance", "ServiceTag"]);
}
