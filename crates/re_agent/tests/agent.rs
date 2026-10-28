//! `re_agent`: the policy, sessions and checkpoints, and the agent commands.

#[path = "../../re_scene/tests/support/mod.rs"]
mod support;

use re_agent::policy::{self, Policy};
use re_agent::{Agent, register_commands};
use re_command::{
    AllowAll, CommandOutcome, CommandRegistry, EditState, PolicyCheck, register_builtin_commands,
};
use re_reflect::Doc;
use serde_json::{Value as Json, json};
use support::*;

fn registry(agent: &Agent) -> CommandRegistry<EditState> {
    let mut r = CommandRegistry::new();
    register_builtin_commands(&mut r);
    register_commands(&mut r, agent);
    r
}

/// The sample scene, a procedural grid, and a second root whose weld points
/// into the first root's subtree.
fn state() -> EditState {
    let mut s = EditState {
        world: world(),
        history: Default::default(),
    };
    let mut docs = sample_docs();
    docs.push(grid_doc(2, 2));
    docs.push(entity(
        "outpost",
        None,
        &[("Weld", &[("other", Doc::Str("crate".into()))])],
    ));
    s.world.spawn_docs(&docs, None).unwrap();
    s.world.update();
    s
}

fn code(out: &CommandOutcome) -> String {
    out.diagnostics
        .first()
        .map(|d| d.code.to_string())
        .unwrap_or_default()
}

fn hash(s: &EditState) -> String {
    s.world.state_hash().to_hex().to_string()
}

// --- Policy ---------------------------------------------------------------

#[test]
fn read_policy_refuses_edits_but_allows_dry_runs() {
    let dir = tempfile::tempdir().unwrap();
    let agent = Agent::new(dir.path());
    let reg = registry(&agent);
    let mut s = state();
    // No policy file: read only.
    let policy = Policy::load(dir.path()).unwrap();
    assert_eq!(policy, Policy::read_only().in_project(dir.path()));

    let before = hash(&s);
    let out = reg.dispatch(&mut s, "entity.spawn", json!({ "id": "x" }), false, &policy);
    assert!(!out.ok);
    assert_eq!(code(&out), "RE8001");
    assert!(
        out.diagnostics[0]
            .hint
            .as_deref()
            .unwrap()
            .contains("dry_run")
    );

    let dry = reg.dispatch(&mut s, "entity.spawn", json!({ "id": "x" }), true, &policy);
    assert!(dry.ok, "{:?}", dry.diagnostics);
    assert!(!dry.changes.is_empty());
    // `dry_run` inside the arguments counts too.
    let dry = reg.dispatch(
        &mut s,
        "entity.spawn",
        json!({ "id": "x", "dry_run": true }),
        false,
        &policy,
    );
    assert!(dry.ok, "{:?}", dry.diagnostics);
    assert_eq!(hash(&s), before);

    let read = reg.dispatch(&mut s, "query", json!({}), false, &policy);
    assert!(read.ok);
}

#[test]
fn edit_policy_allows_edits_but_not_destructive() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("old.txt"), "x").unwrap();
    let agent = Agent::new(dir.path());
    let reg = registry(&agent);
    let mut s = state();
    let policy = Policy::init_default();
    let out = reg.dispatch(&mut s, "entity.spawn", json!({ "id": "x" }), false, &policy);
    assert!(out.ok, "{:?}", out.diagnostics);
    let out = reg.dispatch(
        &mut s,
        "file.delete",
        json!({ "path": "assets/old.txt" }),
        false,
        &policy,
    );
    assert_eq!(code(&out), "RE8001");
    assert!(
        out.diagnostics[0]
            .hint
            .as_deref()
            .unwrap()
            .contains("person")
    );
}

#[test]
fn scopes_and_paths() {
    let dir = tempfile::tempdir().unwrap();
    let p = Policy::init_default().in_project(dir.path());
    assert_eq!(
        p.check_path("script.write", "scripts/./game/a.luau")
            .unwrap(),
        "scripts/game/a.luau"
    );
    assert_eq!(
        p.check_path("script.write", "scripts\\b.luau").unwrap(),
        "scripts/b.luau"
    );
    assert_eq!(
        p.check_path("animgraph.create", "anims/walk.animgraph.json")
            .unwrap(),
        "anims/walk.animgraph.json"
    );
    for bad in [
        "../x.luau",
        "scripts/../../x.luau",
        "/etc/passwd",
        "C:/x.luau",
        "",
        "notes/x.md",
        ".real/agent-policy.toml",
        ".git/config",
        "AGENTS.md",
        // Inside the scopes, but spelled as another name on some file system.
        "scripts/a.luau.",
        "scripts/a.luau ",
        "scripts/game./a.luau",
        "scripts/a.luau::$DATA",
        "scripts/PROGRA~1/a.luau",
        "scripts/SECRET~1.LUA",
        "scripts/PASSWO~1.MD",
        "scripts/a~12.b",
        "scripts/con.luau",
        "scripts/NUL",
        "scripts/com1.txt",
        "scripts/LPT9/a.luau",
    ] {
        let err = p.check_path("script.write", bad).unwrap_err();
        assert_eq!(err.code.to_string(), "RE8002", "{bad}");
    }
    // Names that only look like those are fine.
    for good in [
        "scripts/console.luau",
        "scripts/com10.luau",
        "scripts/a~b.luau",
        // Too long for an 8.3 name, so never a short one: the importer
        // names a second script called `Rose` this way.
        "scripts/Rose~2.luau",
        "scripts/Rose~2.server.luau",
        "scripts/Longer~12/a.luau",
        "scripts/.hidden/a.luau",
        "scripts/v1.2/a.luau",
    ] {
        assert!(p.check_path("script.write", good).is_ok(), "{good}");
    }
    // `allow` matches as written: another case fails closed.
    assert!(p.check_path("script.write", "Scripts/a.luau").is_err());

    // Even an allow-all scope never opens `.real/` or `.git/`, however they
    // are spelled.
    let mut wide = Policy::init_default().in_project(dir.path());
    wide.scopes.allow = vec!["**".into()];
    for protected in [
        ".real/agent-policy.toml",
        ".git/config",
        ".Git/hooks/pre-commit",
        ".GIT/config",
        ".REAL/agent-policy.toml",
        ".git./config",
        ".git /config",
        "GIT~1/config",
        "git~12/config",
        "REAL~1/agent-policy.toml",
        ".git::$INDEX_ALLOCATION/config",
        ".real/agent-policy.toml::$DATA",
        ".g\u{200C}it/config",
        "\u{FEFF}.real/agent-policy.toml",
    ] {
        let err = wide.check_path("x", protected).unwrap_err();
        assert_eq!(err.code.to_string(), "RE8002", "{protected:?}");
    }
    assert!(policy::is_protected_dir(".Git"));
    assert!(policy::is_protected_dir("REAL~2"));
    assert!(!policy::is_protected_dir(".github"));
    assert!(!policy::is_protected_dir("git"));
    assert!(!policy::is_protected_dir("real~"));
    assert!(wide.check_path("x", ".github/workflows/ci.yml").is_ok());
    assert!(wide.check_path("x", "notes/x.md").is_ok());
    // `deny` ignores case and the code points HFS+ ignores.
    wide.scopes.deny = vec!["notes/**".into(), "scripts/secret/**".into()];
    for denied in [
        "notes/x.md",
        "NOTES/x.md",
        "scripts/Secret/x.luau",
        "scripts/SECRET/deep/x.luau",
        "scripts/se\u{200D}cret/x.luau",
    ] {
        let err = wide.check_path("x", denied).unwrap_err();
        assert_eq!(err.code.to_string(), "RE8002", "{denied:?}");
        assert!(err.message.contains("deny"), "{}", err.message);
    }
    assert!(wide.check_path("x", "scripts/secrets.luau").is_ok());

    // A policy that does not know its project refuses every write rather
    // than skip the link check.
    let loose = Policy::init_default();
    let err = loose
        .check_path("script.write", "scripts/a.luau")
        .unwrap_err();
    assert_eq!(err.code.to_string(), "RE8002");
    assert!(err.message.contains("project"), "{}", err.message);
}

#[test]
fn images_go_under_real_out_or_to_new_files_in_scope() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join("assets")).unwrap();
    std::fs::write(project.join("assets/hero.png"), b"png").unwrap();
    let read = Policy::read_only().in_project(project);
    let edit = Policy::init_default().in_project(project);
    for p in [&read, &edit] {
        assert_eq!(
            p.check_output("screenshot", ".real/out/shot.png").unwrap(),
            ".real/out/shot.png"
        );
        assert!(p.check_output("screenshot", ".real/out/a/b.PNG").is_ok());
        for bad in [
            "real.toml",
            ".real/agent-policy.toml",
            ".real/out/x.toml",
            ".real/out/../agent-policy.png",
            ".real/out/x.png.",
            ".real/out/x.png::$DATA",
            ".real/shot.png",
            ".git/shot.png",
            ".Real/out/x.png",
            "REAL~1/out/x.png",
            "../x.png",
        ] {
            let err = p.check_output("screenshot", bad).unwrap_err();
            assert_eq!(err.code.to_string(), "RE8002", "{bad}");
            assert!(
                err.hint.as_deref().unwrap().contains(".real/out/"),
                "{:?}",
                err.hint
            );
        }
    }
    // Outside `.real/out/`: a new `.png` inside the scopes, never an
    // existing file, so a screenshot never replaces one.
    assert!(read.check_output("screenshot", "assets/new.png").is_err());
    assert!(edit.check_output("screenshot", "assets/new.png").is_ok());
    let err = edit
        .check_output("screenshot", "assets/hero.png")
        .unwrap_err();
    assert!(err.message.contains("exists"), "{}", err.message);
    assert!(edit.check_output("screenshot", "notes/new.png").is_err());
    #[cfg(unix)]
    {
        // `.real/out/` gets the link check too.
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.join(".real")).unwrap();
        std::os::unix::fs::symlink(outside.path(), project.join(".real/out")).unwrap();
        let err = read
            .check_output("screenshot", ".real/out/shot.png")
            .unwrap_err();
        assert!(err.message.contains("symbolic link"), "{}", err.message);
    }
}

#[test]
fn policy_checks_the_declared_paths_of_every_call() {
    use re_command::{PathAccess, PathArg};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("taken.png"), b"png").unwrap();
    let p = Policy::init_default().in_project(dir.path());
    let spec = |tier| re_command::CommandSpec {
        name: "files".into(),
        summary: String::new(),
        tier,
        input_schema: json!({}),
        annotations: re_command::CommandAnnotations::for_tier(tier, false),
        output_schema: None,
        paths: vec![
            PathArg {
                pointer: "/target".into(),
                access: PathAccess::Write,
            },
            PathArg {
                pointer: "/ops/*/doc".into(),
                access: PathAccess::Write,
            },
            PathArg {
                pointer: "/shot".into(),
                access: PathAccess::Output,
            },
            PathArg {
                pointer: "/source".into(),
                access: PathAccess::Read,
            },
        ],
    };
    let code = |r: Result<(), re_core::Diagnostic>| r.unwrap_err().code.to_string();
    for tier in [re_command::Tier::Read, re_command::Tier::Edit] {
        let s = spec(tier);
        // Whatever the tier, every write is checked.
        assert_eq!(
            code(p.check(&s, &json!({ "target": "../x.luau" }), false)),
            "RE8002"
        );
        assert!(
            p.check(&s, &json!({ "target": "scripts/x.luau" }), false)
                .is_ok()
        );
        assert_eq!(
            code(p.check(
                &s,
                &json!({ "ops": [{ "op": "spawn" }, { "op": "set_doc", "doc": "levels/x.graph.json" }] }),
                false
            )),
            "RE8002"
        );
        assert!(
            p.check(
                &s,
                &json!({ "ops": [{ "op": "set_doc", "doc": "graphs/x.graph.json" }] }),
                false
            )
            .is_ok()
        );
        assert_eq!(
            code(p.check(&s, &json!({ "shot": "real.toml" }), false)),
            "RE8002"
        );
        assert_eq!(
            code(p.check(&s, &json!({ "shot": "taken.png" }), false)),
            "RE8002"
        );
        assert!(
            p.check(&s, &json!({ "shot": ".real/out/a.png" }), false)
                .is_ok()
        );
        // Reads are not checked, and nor is an argument the spec does not
        // declare, whatever its name.
        assert!(
            p.check(
                &s,
                &json!({ "source": "/etc/passwd", "dest": "../x", "out_path": ".git/x" }),
                false
            )
            .is_ok()
        );
        // A dry run writes nothing, so it is not path-checked.
        assert!(
            p.check(
                &s,
                &json!({ "target": "../x.luau", "shot": "real.toml" }),
                true
            )
            .is_ok()
        );
    }
}

#[cfg(unix)]
#[test]
fn file_commands_refuse_symbolic_links() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    let agent = Agent::new(project);
    let reg = registry(&agent);
    let mut s = state();
    agent.start_session(&s.world).unwrap();
    let policy = Policy::init_default().in_project(project);
    std::fs::create_dir_all(project.join(".real")).unwrap();
    std::fs::write(
        project.join(".real/agent-policy.toml"),
        "max_tier = \"edit\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(project.join("scripts")).unwrap();
    // A link to the policy, and a linked directory leading out of the project.
    std::os::unix::fs::symlink(
        "../.real/agent-policy.toml",
        project.join("scripts/cfg.luau"),
    )
    .unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), project.join("scripts/out")).unwrap();

    let write = |s: &mut EditState, path: &str| {
        reg.dispatch(
            s,
            "script.write",
            json!({ "path": path, "source": "max_tier = \"destructive\"\n" }),
            false,
            &policy,
        )
    };
    let out = write(&mut s, "scripts/cfg.luau");
    assert_eq!(code(&out), "RE8002", "{:?}", out.diagnostics);
    let out = write(&mut s, "scripts/out/x.luau");
    assert_eq!(code(&out), "RE8002", "{:?}", out.diagnostics);
    assert_eq!(
        std::fs::read_to_string(project.join(".real/agent-policy.toml")).unwrap(),
        "max_tier = \"edit\"\n"
    );
    assert!(!outside.path().join("x.luau").exists());

    let mut all = Policy::init_default().in_project(project);
    all.max_tier = re_command::Tier::Destructive;
    let out = reg.dispatch(
        &mut s,
        "file.delete",
        json!({ "path": "scripts/cfg.luau" }),
        false,
        &all,
    );
    assert_eq!(code(&out), "RE8002", "{:?}", out.diagnostics);
    assert!(project.join(".real/agent-policy.toml").exists());
    // Protected directories are refused whatever their case.
    let out = write(&mut s, ".REAL/x.luau");
    assert_eq!(code(&out), "RE8002", "{:?}", out.diagnostics);
}

#[test]
fn budgets() {
    let p = Policy::init_default();
    let spec = |name: &str| re_command::CommandSpec {
        name: name.into(),
        summary: String::new(),
        tier: re_command::Tier::Read,
        input_schema: json!({}),
        annotations: re_command::CommandAnnotations::for_tier(re_command::Tier::Read, true),
        output_schema: None,
        paths: Vec::new(),
    };
    assert!(
        p.check(&spec("play.run"), &json!({ "frames": 600 }), false)
            .is_ok()
    );
    let err = p
        .check(&spec("play.run"), &json!({ "frames": 601 }), false)
        .unwrap_err();
    assert_eq!(err.code.to_string(), "RE8008");
    let err = p
        .check(&spec("graph.generate"), &json!({ "frames": 10_000 }), true)
        .unwrap_err();
    assert_eq!(err.code.to_string(), "RE8008");
    // A frame count that isn't a whole number can't slip past the budget.
    for frames in [json!(1e9), json!(-1), json!("600")] {
        let err = p
            .check(&spec("play.run"), &json!({ "frames": frames }), false)
            .unwrap_err();
        assert_eq!(err.code.to_string(), "RE8008");
    }

    let dir = tempfile::tempdir().unwrap();
    let agent = Agent::new(dir.path());
    let reg = registry(&agent);
    let mut s = state();
    let mut small = Policy::init_default();
    small.budgets.entities_per_transaction = 2;
    let ops: Vec<Json> = (0..3)
        .map(|i| json!({ "op": "spawn", "id": format!("b{i}") }))
        .collect();
    let out = reg.dispatch(&mut s, "scene.apply", json!({ "ops": ops }), false, &small);
    assert_eq!(code(&out), "RE8008");
}

#[test]
fn policy_file_round_trips_and_errors_are_re8009() {
    let p = Policy::init_default();
    let text = p.to_toml();
    assert_eq!(Policy::parse(&text).unwrap(), p);
    assert!(text.contains("max_tier = \"edit\""));
    assert!(!text.contains("destructive\"\n"));

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".real")).unwrap();
    std::fs::write(policy::policy_path(dir.path()), "max_tier = \"admin\"\n").unwrap();
    let err = Policy::load(dir.path()).unwrap_err();
    assert_eq!(err.code.to_string(), "RE8009");
    assert!(err.message.contains("admin"), "{}", err.message);

    // Partial files take defaults for the rest.
    let p = Policy::parse("max_tier = \"destructive\"\n").unwrap();
    assert_eq!(p.max_tier, re_command::Tier::Destructive);
    assert!(p.scopes.allow.is_empty());
    assert_eq!(p.budgets, policy::Budgets::default());
}

// --- Sessions, checkpoint.restore and file.delete -------------------------

#[test]
fn checkpoint_restore_returns_scene_and_files_and_can_be_undone() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join("scripts")).unwrap();
    std::fs::write(project.join("scripts/keep.luau"), "-- original\n").unwrap();
    let agent = Agent::new(project);
    let reg = registry(&agent);
    let mut s = state();
    let start_hash = hash(&s);
    let session = agent.start_session(&s.world).unwrap();
    assert_eq!(agent.session_id().as_deref(), Some(session.as_str()));

    // The agent edits the scene...
    let policy = Policy::init_default();
    for (cmd, args) in [
        ("entity.spawn", json!({ "id": "rock", "name": "Rock" })),
        (
            "entity.set",
            json!({ "id": "ground", "component": "Transform", "field": "translation", "value": [0, 2, 0] }),
        ),
        ("entity.delete", json!({ "id": "crate" })),
    ] {
        let out = reg.dispatch(&mut s, cmd, args, false, &policy);
        assert!(out.ok, "{cmd}: {:?}", out.diagnostics);
    }
    // ...and files.
    re_agent::commands::write_file(
        &agent,
        &s.world,
        "test",
        "scripts/keep.luau",
        b"-- changed\n",
    )
    .unwrap();
    re_agent::commands::write_file(&agent, &s.world, "test", "scripts/new.luau", b"-- new\n")
        .unwrap();
    let edited_hash = hash(&s);
    assert_ne!(edited_hash, start_hash);

    // A dry run reports and changes nothing.
    let dry = reg.dispatch(&mut s, "checkpoint.restore", json!({}), true, &policy);
    assert!(dry.ok, "{:?}", dry.diagnostics);
    assert_eq!(dry.data["restores"], "start");
    assert_eq!(dry.data["files"].as_array().unwrap().len(), 2);
    assert_eq!(hash(&s), edited_hash);

    let out = reg.dispatch(&mut s, "checkpoint.restore", json!({}), false, &policy);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(out.data["restored"], "start");
    assert_eq!(out.data["saved"], "cp1");
    assert_eq!(out.data["state_hash"], start_hash);
    assert_eq!(hash(&s), start_hash);
    assert_eq!(
        std::fs::read_to_string(project.join("scripts/keep.luau")).unwrap(),
        "-- original\n"
    );
    assert!(!project.join("scripts/new.luau").exists());
    // The cross-root reference survived the round trip.
    assert!(s.world.entity("crate").is_ok());

    // Restoring the saved checkpoint brings the edits back.
    let out = reg.dispatch(
        &mut s,
        "checkpoint.restore",
        json!({ "checkpoint": "cp1" }),
        false,
        &policy,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(out.data["saved"], "cp2");
    assert_eq!(hash(&s), edited_hash);
    assert_eq!(
        std::fs::read_to_string(project.join("scripts/new.luau")).unwrap(),
        "-- new\n"
    );

    // The scene part is one transaction: `undo` reverts it.
    let out = reg.dispatch(&mut s, "undo", json!({}), false, &policy);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(hash(&s), start_hash);

    // Unknown checkpoints are RE8011.
    let out = reg.dispatch(
        &mut s,
        "checkpoint.restore",
        json!({ "checkpoint": "cp99" }),
        false,
        &policy,
    );
    assert_eq!(code(&out), "RE8011");
    let out = reg.dispatch(
        &mut s,
        "checkpoint.restore",
        json!({ "checkpoint": "../start" }),
        false,
        &policy,
    );
    assert_eq!(code(&out), "RE8011");

    // The session is on disk and can be reopened.
    assert_eq!(
        re_agent::session::list_sessions(project),
        vec![session.clone()]
    );
    let reopened = re_agent::Session::open(project, &session).unwrap();
    assert_eq!(
        reopened.load_checkpoint("start").unwrap().state_hash,
        start_hash
    );
    let err = re_agent::Session::open(project, "nope").unwrap_err();
    assert_eq!(err.code.to_string(), "RE8011");
}

#[test]
fn restore_without_a_session_is_re8012() {
    let dir = tempfile::tempdir().unwrap();
    let agent = Agent::new(dir.path());
    let reg = registry(&agent);
    let mut s = state();
    let out = reg.dispatch(&mut s, "checkpoint.restore", json!({}), false, &AllowAll);
    assert_eq!(code(&out), "RE8012");
}

#[test]
fn file_delete_is_destructive_and_restorable() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join("assets")).unwrap();
    std::fs::write(project.join("assets/old.png"), [1u8, 2, 3, 0, 255]).unwrap();
    let agent = Agent::new(project);
    let reg = registry(&agent);
    let mut s = state();
    agent.start_session(&s.world).unwrap();
    let mut policy = Policy::init_default().in_project(project);
    policy.max_tier = re_command::Tier::Destructive;

    let spec = reg.spec("file.delete").unwrap();
    assert_eq!(spec.tier, re_command::Tier::Destructive);
    assert!(spec.annotations.destructive_hint);

    let dry = reg.dispatch(
        &mut s,
        "file.delete",
        json!({ "path": "assets/old.png" }),
        true,
        &policy,
    );
    assert!(dry.ok);
    assert_eq!(dry.data["would_delete"], "assets/old.png");
    assert!(project.join("assets/old.png").exists());

    for (path, want) in [
        ("assets/missing.png", "RE8013"),
        ("assets", "RE8013"), // a directory
        ("../outside.png", "RE8002"),
        (".real/agent-policy.toml", "RE8002"),
    ] {
        let out = reg.dispatch(
            &mut s,
            "file.delete",
            json!({ "path": path }),
            false,
            &policy,
        );
        assert_eq!(code(&out), want, "{path}");
    }
    // A person (AllowAll) still can't reach outside the project or `.real/`.
    let out = reg.dispatch(
        &mut s,
        "file.delete",
        json!({ "path": "../x" }),
        false,
        &AllowAll,
    );
    assert_eq!(code(&out), "RE8002");
    let out = reg.dispatch(
        &mut s,
        "file.delete",
        json!({ "path": "assets" }),
        false,
        &AllowAll,
    );
    assert_eq!(code(&out), "RE8013");

    let out = reg.dispatch(
        &mut s,
        "file.delete",
        json!({ "path": "assets/old.png" }),
        false,
        &policy,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    assert!(!project.join("assets/old.png").exists());

    let out = reg.dispatch(&mut s, "checkpoint.restore", json!({}), false, &policy);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(
        std::fs::read(project.join("assets/old.png")).unwrap(),
        vec![1u8, 2, 3, 0, 255]
    );
}

#[test]
fn script_write_is_compile_gated_and_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    let agent = Agent::new(project);
    let reg = registry(&agent);
    let mut s = state();
    agent.start_session(&s.world).unwrap();
    let policy = Policy::init_default().in_project(project);
    let call = |s: &mut EditState, path: &str, source: &str, dry: bool| {
        reg.dispatch(
            s,
            "script.write",
            json!({ "path": path, "source": source }),
            dry,
            &policy,
        )
    };

    // A syntax error writes nothing and returns RE3xxx.
    let out = call(&mut s, "scripts/bad.luau", "local x = = 1", false);
    assert!(!out.ok);
    assert!(code(&out).starts_with("RE3"), "{:?}", out.diagnostics);
    assert!(
        format!("{:?}", out.diagnostics[0].location).contains("scripts/bad.luau"),
        "{:?}",
        out.diagnostics[0]
    );
    assert!(!project.join("scripts/bad.luau").exists());

    // A dry run checks and writes nothing.
    let out = call(&mut s, "scripts/game/ok.luau", "return 1\n", true);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(out.data["would_write"], "scripts/game/ok.luau");
    assert!(!project.join("scripts/game/ok.luau").exists());

    let out = call(&mut s, "scripts/game/ok.luau", "return 1\n", false);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(out.data["written"], "scripts/game/ok.luau");
    assert_eq!(
        std::fs::read_to_string(project.join("scripts/game/ok.luau")).unwrap(),
        "return 1\n"
    );

    // Outside the scopes, or not a script.
    let out = call(&mut s, "notes/x.luau", "return 1", false);
    assert_eq!(code(&out), "RE8002");
    let out = call(&mut s, "../x.luau", "return 1", false);
    assert_eq!(code(&out), "RE8002");
    let out = call(&mut s, "scripts/x.txt", "return 1", false);
    assert_eq!(code(&out), "RE8013");
    // A read-only policy refuses the write but allows the dry run.
    let read = Policy::read_only().in_project(project);
    let out = reg.dispatch(
        &mut s,
        "script.write",
        json!({ "path": "scripts/y.luau", "source": "return 2" }),
        false,
        &read,
    );
    assert_eq!(code(&out), "RE8001");

    // Restoring the session start removes the new script.
    let out = reg.dispatch(&mut s, "checkpoint.restore", json!({}), false, &policy);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert!(!project.join("scripts/game/ok.luau").exists());
}

#[test]
fn diagnostics_catalogue_is_complete() {
    let problems = re_core::validate_catalog(re_agent::DIAGNOSTICS, re_agent::CODE_RANGE);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(re_agent::DIAGNOSTICS.len(), 27);
}
