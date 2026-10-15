//! Luau-defined tools: loading, validation, one transaction per call, dry
//! runs, tiers and the policy, and direct world writes being reverted.

use std::path::Path;

use re_agent::luau_tools::{TOOLS_DIR, register};
use re_agent::policy::Policy;
use re_command::{
    AllowAll, CommandOutcome, CommandRegistry, EditState, Origin, PolicyCheck,
    register_builtin_commands,
};
use serde_json::{Value as Json, json};

fn write_tool(project: &Path, file: &str, source: &str) {
    let path = project.join(TOOLS_DIR).join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, source).unwrap();
}

const RING: &str = r#"
return {
    name = "test.ring",
    summary = "Spawn markers named ring_1 to ring_N.",
    input = {
        type = "object",
        properties = { count = { type = "integer", minimum = 1, maximum = 50 } },
        required = { "count" },
        additionalProperties = false,
    },
    output = {
        type = "object",
        properties = { placed = { type = "integer" } },
        required = { "placed" },
    },
    run = function(args)
        for i = 1, args.count do
            local out = command.run("entity.spawn", { id = `ring_{i}`, name = `Ring {i}` })
            if not out.ok then
                error(out.diagnostics[1].message)
            end
        end
        print(`placed {args.count}`)
        return { placed = args.count }
    end,
}
"#;

fn registry(project: &Path) -> (CommandRegistry<EditState>, Vec<re_core::Diagnostic>) {
    let mut r = CommandRegistry::new();
    register_builtin_commands(&mut r);
    let (_defs, problems) = register(&mut r, project);
    (r, problems)
}

fn call(
    reg: &CommandRegistry<EditState>,
    s: &mut EditState,
    name: &str,
    args: Json,
    dry_run: bool,
) -> CommandOutcome {
    reg.dispatch(s, name, args, dry_run, &AllowAll)
}

fn code(out: &CommandOutcome) -> String {
    out.diagnostics
        .first()
        .map(|d| d.code.to_string())
        .unwrap_or_default()
}

#[test]
fn a_tool_runs_as_one_undoable_transaction() {
    let dir = tempfile::tempdir().unwrap();
    write_tool(dir.path(), "ring.luau", RING);
    let (reg, problems) = registry(dir.path());
    assert!(problems.is_empty(), "{problems:?}");
    let spec = reg.spec("test.ring").unwrap();
    assert_eq!(spec.tier, re_command::Tier::Edit);
    assert!(spec.summary.contains("scripts/tools/commands/ring.luau"));
    assert!(spec.input_schema["properties"]["dry_run"].is_object());
    assert_eq!(
        spec.output_schema.as_ref().unwrap()["required"],
        json!(["placed"])
    );

    let mut s = EditState::new();
    let before = s.world.state_hash();

    // A dry run reports every change and changes nothing.
    let dry = call(&reg, &mut s, "test.ring", json!({ "count": 3 }), true);
    assert!(dry.ok, "{:?}", dry.diagnostics);
    assert_eq!(dry.changes.len(), 3);
    assert_eq!(s.world.state_hash(), before);
    assert!(!s.history.can_undo());

    let out = call(&reg, &mut s, "test.ring", json!({ "count": 3 }), false);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(out.data, json!({ "placed": 3 }));
    assert_eq!(out.changes.len(), 3);
    assert!(s.world.entity("ring_3").is_ok());
    // What the tool printed comes back as a note.
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.code.to_string() == "RE8017" && d.message == "placed 3"),
        "{:?}",
        out.diagnostics
    );

    // One undo reverts the whole tool.
    assert_eq!(s.history.undo_label(), Some("tool test.ring"));
    s.history.undo(&mut s.world).unwrap();
    assert_eq!(s.world.state_hash(), before);
    assert!(!s.history.can_undo());
}

#[test]
fn arguments_are_checked_against_the_input_schema() {
    let dir = tempfile::tempdir().unwrap();
    write_tool(dir.path(), "ring.luau", RING);
    let (reg, _) = registry(dir.path());
    let mut s = EditState::new();
    for (args, want) in [
        (json!({}), "`count` is required"),
        (json!({ "count": 0 }), "at least 1"),
        (json!({ "count": 2, "cout": 1 }), "did you mean `count`"),
    ] {
        let out = call(&reg, &mut s, "test.ring", args, false);
        assert_eq!(code(&out), "RE2005");
        assert!(
            out.diagnostics[0].message.contains(want),
            "{:?}",
            out.diagnostics
        );
    }
}

#[test]
fn a_failing_tool_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write_tool(dir.path(), "ring.luau", RING);
    let (reg, _) = registry(dir.path());
    let mut s = EditState::new();
    call(&reg, &mut s, "test.ring", json!({ "count": 1 }), false);
    let before = s.world.state_hash();
    // ring_1 exists, so the second call's first spawn fails and the tool
    // raises an error: ring_2 is not left behind.
    let out = call(&reg, &mut s, "test.ring", json!({ "count": 2 }), false);
    assert!(!out.ok);
    assert_eq!(s.world.state_hash(), before);
    assert!(s.world.entity("ring_2").is_err());
    // The history still holds the first call only.
    assert_eq!(s.history.undo_label(), Some("tool test.ring"));
    s.history.undo(&mut s.world).unwrap();
    assert!(!s.history.can_undo());
}

#[test]
fn direct_world_writes_are_reverted() {
    let dir = tempfile::tempdir().unwrap();
    write_tool(
        dir.path(),
        "sneaky.luau",
        r#"
return {
    name = "test.sneaky",
    summary = "Spawn without command.run.",
    run = function()
        world.spawn({ id = "sneaky" })
        return true
    end,
}
"#,
    );
    let (reg, problems) = registry(dir.path());
    assert!(problems.is_empty(), "{problems:?}");
    let mut s = EditState::new();
    let before = s.world.state_hash();
    let out = call(&reg, &mut s, "test.sneaky", json!({}), false);
    assert_eq!(code(&out), "RE8016", "{:?}", out.diagnostics);
    assert_eq!(s.world.state_hash(), before);
    assert!(s.world.entity("sneaky").is_err());
}

#[test]
fn read_tools_cannot_edit_and_tools_cannot_undo() {
    let dir = tempfile::tempdir().unwrap();
    write_tool(
        dir.path(),
        "peek.luau",
        r#"
return {
    name = "test.peek",
    summary = "Try to edit from a read tool.",
    tier = "read",
    run = function()
        local spawn = command.run("entity.spawn", { id = "x" })
        local preview = command.run("entity.spawn", { id = "x" }, true)
        local undo = command.run("undo")
        local found = command.run("query", {})
        return {
            spawn = spawn.diagnostics[1].code,
            preview = preview.ok,
            undo = undo.diagnostics[1].code,
            query = found.ok,
        }
    end,
}
"#,
    );
    let (reg, _) = registry(dir.path());
    assert_eq!(reg.spec("test.peek").unwrap().tier, re_command::Tier::Read);
    assert!(
        reg.spec("test.peek").unwrap().input_schema["properties"]["dry_run"].is_null(),
        "read tools take no dry_run"
    );
    let mut s = EditState::new();
    let out = call(&reg, &mut s, "test.peek", json!({}), false);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(
        out.data,
        json!({ "spawn": "RE8001", "preview": true, "undo": "RE8001", "query": true })
    );
    assert!(out.changes.is_empty());
    assert!(!s.history.can_undo());
}

#[test]
fn bad_tool_scripts_are_reported_and_skipped() {
    let dir = tempfile::tempdir().unwrap();
    write_tool(dir.path(), "ring.luau", RING);
    let bad = [
        (
            "a_destructive.luau",
            r#"return { name = "test.wipe", summary = "x", tier = "destructive", run = function() end }"#,
            "destructive",
        ),
        (
            "b_taken.luau",
            r#"return { name = "entity.spawn", summary = "x", run = function() end }"#,
            "already taken",
        ),
        (
            "c_dupe.luau",
            r#"return { name = "test.ring", summary = "x", run = function() end }"#,
            "already used by",
        ),
        (
            "d_norun.luau",
            r#"return { name = "test.norun", summary = "x" }"#,
            "no `run` function",
        ),
        (
            "e_badname.luau",
            r#"return { name = "Test Ring", summary = "x", run = function() end }"#,
            "dotted lowercase",
        ),
        ("f_syntax.luau", "return {", "could not be loaded"),
        (
            "g_nosummary.luau",
            r#"return { name = "test.nosummary", run = function() end }"#,
            "no `summary`",
        ),
    ];
    for (file, source, _) in &bad {
        // After ring.luau in path order, so the duplicate is the one refused.
        write_tool(dir.path(), &format!("zz/{file}"), source);
    }
    let (reg, problems) = registry(dir.path());
    assert!(reg.spec("test.ring").is_some());
    assert_eq!(problems.len(), bad.len(), "{problems:?}");
    for (d, (file, _, want)) in problems.iter().zip(&bad) {
        assert_eq!(d.code.to_string(), "RE8015");
        assert!(d.message.contains(file), "{d:?}");
        assert!(d.message.contains(want), "{file}: {d:?}");
    }
    assert!(reg.spec("test.wipe").is_none());
}

#[test]
fn an_agents_call_is_held_to_the_agent_policy() {
    let dir = tempfile::tempdir().unwrap();
    write_tool(dir.path(), "ring.luau", RING);
    // A policy that grants edit but allows only two operations per
    // transaction.
    std::fs::create_dir_all(dir.path().join(".real")).unwrap();
    std::fs::write(
        dir.path().join(".real/agent-policy.toml"),
        "max_tier = \"edit\"\n[scopes]\nallow = [\"scripts/**\"]\n[budgets]\nentities_per_transaction = 2\n",
    )
    .unwrap();
    let (reg, _) = registry(dir.path());
    let policy = Policy::load(dir.path()).unwrap();
    let mut s = EditState::new();
    let before = s.world.state_hash();
    let agent = |s: &mut EditState, n: u64| {
        reg.dispatch_as(
            s,
            "test.ring",
            json!({ "count": n }),
            false,
            &policy as &dyn PolicyCheck,
            Origin::Agent("test".into()),
        )
    };
    let out = agent(&mut s, 3);
    assert_eq!(code(&out), "RE8008", "{:?}", out.diagnostics);
    assert_eq!(s.world.state_hash(), before);
    let out = agent(&mut s, 2);
    assert!(out.ok, "{:?}", out.diagnostics);
    s.history.undo(&mut s.world).unwrap();
    // A person (`real do`) is not held to the agent budgets.
    let out = call(&reg, &mut s, "test.ring", json!({ "count": 3 }), false);
    assert!(out.ok, "{:?}", out.diagnostics);
    // A read-only agent policy refuses the edit tool before it runs.
    let read = Policy::read_only();
    let out = reg.dispatch_as(
        &mut s,
        "test.ring",
        json!({ "count": 1 }),
        false,
        &read,
        Origin::Agent("test".into()),
    );
    assert_eq!(code(&out), "RE8001");
}
