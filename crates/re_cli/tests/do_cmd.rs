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
