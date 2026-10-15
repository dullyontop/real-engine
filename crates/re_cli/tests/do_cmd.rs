//! `real do`: one command against a project, saved; dry runs change nothing;
//! a second headless writer is refused.
#![cfg(feature = "tooling")]

use std::path::Path;
use std::process::{Command, Output};

fn real(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_real"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("the real binary runs")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn game() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["new", "game"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    dir
}

const SET: &str =
    r#"{"id": "box", "component": "Transform", "field": "translation", "value": [1.0, 7.0, 2.0]}"#;

#[test]
fn do_applies_a_command_and_saves_the_scene() {
    let dir = game();
    let game = dir.path().join("game");
    let o = real(&["do", "entity.set", "--args", SET, "--json"], &game);
    assert!(o.status.success(), "{}", text(&o));
    let out: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(out["via"], "local");
    let scene = std::fs::read_to_string(game.join("scenes/main.scene.json")).unwrap();
    assert!(scene.contains("7.0"), "{scene}");

    // A read command prints its data.
    let o = real(&["do", "entity.get", "--args", r#"{"id": "box"}"#], &game);
    assert!(o.status.success(), "{}", text(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("Transform"));
}

#[test]
fn a_dry_run_changes_nothing() {
    let dir = game();
    let game = dir.path().join("game");
    let before = std::fs::read_to_string(game.join("scenes/main.scene.json")).unwrap();
    let o = real(
        &["do", "entity.set", "--args", SET, "--dry-run", "--json"],
        &game,
    );
    assert!(o.status.success(), "{}", text(&o));
    let out: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(out["changes"].as_array().unwrap().len(), 1);
    let after = std::fs::read_to_string(game.join("scenes/main.scene.json")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn bad_arguments_and_unknown_commands_fail() {
    let dir = game();
    let game = dir.path().join("game");
    let o = real(&["do", "entity.get", "--args", "[1]"], &game);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let o = real(&["do", "entity.get", "--args", "{nope"], &game);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let o = real(&["do", "entity.gte", "--args", r#"{"id": "box"}"#], &game);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let o = real(
        &["do", "entity.get", "--args", r#"{"id": "nobody"}"#],
        &game,
    );
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
}

#[test]
fn a_second_headless_writer_is_re8003() {
    let dir = game();
    let game = dir.path().join("game");
    let _held = re_agent::acquire(&game, re_agent::Holder::headless("test")).unwrap();
    let o = real(&["do", "entity.set", "--args", SET], &game);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("RE8003"), "{}", text(&o));
}

#[test]
fn early_failures_are_json_too() {
    let dir = game();
    let game = dir.path().join("game");
    let o = real(&["do", "entity.get", "--args", "[1]", "--json"], &game);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let out: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(out["ok"], false);
    assert_eq!(out["via"], serde_json::Value::Null);
    assert_eq!(out["diagnostics"][0]["code"], "RE2005");

    let _held = re_agent::acquire(&game, re_agent::Holder::headless("test")).unwrap();
    let o = real(&["do", "entity.set", "--args", SET, "--json"], &game);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let out: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(out["diagnostics"][0]["code"], "RE8003", "{out}");
}

#[cfg(unix)]
#[test]
fn a_failed_save_fails_the_command() {
    use std::os::unix::fs::PermissionsExt;
    let dir = game();
    let game = dir.path().join("game");
    // The scene and its folder read-only: neither a write in place nor a
    // write-then-rename can save it.
    let scenes = game.join("scenes");
    let scene = scenes.join("main.scene.json");
    std::fs::set_permissions(&scene, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::set_permissions(&scenes, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores file modes: nothing to test there.
    let writable = std::fs::OpenOptions::new().write(true).open(&scene).is_ok()
        || std::fs::write(scenes.join("probe"), "").is_ok();
    let o = (!writable).then(|| real(&["do", "entity.set", "--args", SET, "--json"], &game));
    std::fs::set_permissions(&scenes, std::fs::Permissions::from_mode(0o755)).unwrap();
    let Some(o) = o else { return };
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("RE0518"), "{}", text(&o));
}

/// The editor holds the project: `real do` sends the command over its socket.
#[cfg(unix)]
#[test]
fn with_the_editor_open_the_editor_applies_the_command() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use re_agent::lock::{self, Holder};
    use re_agent::remote;
    use re_command::{AllowAll, CommandRegistry, EditState, channel, register_builtin_commands};

    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    let held = lock::acquire(project, Holder::Editor).unwrap();
    let listener = remote::bind(&held).unwrap();
    let (handle, receiver) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let engine = std::thread::spawn(move || {
        let mut registry = CommandRegistry::<EditState>::new();
        register_builtin_commands(&mut registry);
        let mut state = EditState::new();
        while !flag.load(Ordering::SeqCst) {
            if let Ok(Some(request)) = receiver.recv_timeout(Duration::from_millis(5)) {
                request.handle(&mut state, &registry, &AllowAll);
            }
        }
        state
    });
    std::thread::spawn(move || remote::serve(listener, handle));

    let spawn = r#"{"id": "rock", "name": "Rock"}"#;
    let o = real(
        &["do", "entity.spawn", "--args", spawn, "--dry-run", "--json"],
        project,
    );
    assert!(o.status.success(), "{}", text(&o));
    let out: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(out["via"], "editor");
    let o = real(&["do", "entity.spawn", "--args", spawn, "--json"], project);
    assert!(o.status.success(), "{}", text(&o));

    stop.store(true, Ordering::SeqCst);
    let state = engine.join().unwrap();
    assert!(state.world.entity("rock").is_ok());
    // Only the real call reached the editor's history, not the dry run.
    assert_eq!(state.history.log_since_jsonl(0).lines().count(), 1);
    drop(held);
}
