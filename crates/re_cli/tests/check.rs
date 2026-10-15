//! `real check`, verify's `scan` stage and `real describe --luau-defs`.
#![cfg(feature = "tooling")]

use std::path::Path;
use std::process::{Command, Output};

fn real(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_real"))
        .args(args)
        .current_dir(cwd)
        // The type checker is looked up on PATH; tests that need it not to be
        // found clear PATH themselves.
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

/// A starter project with `scripts` written into it.
fn project(scripts: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["new", "game"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    for (path, source) in scripts {
        let p = dir.path().join("game").join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, source).unwrap();
    }
    dir
}

#[test]
fn describe_prints_the_luau_definitions() {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["describe", "--luau-defs"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    let defs = String::from_utf8_lossy(&o.stdout);
    assert!(defs.starts_with("--!strict"));
    assert!(defs.contains("declare physics:"));
}

#[test]
fn check_passes_good_scripts_and_fails_on_a_compile_error() {
    let dir = project(&[("scripts/ok.luau", "print(\"hi\")\n")]);
    let game = dir.path().join("game");
    let o = real(&["check", "--json"], &game);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["compile"]["ok"], 1);

    std::fs::write(game.join("scripts/bad.luau"), "local x = = 1\n").unwrap();
    let o = real(&["check"], &game);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let out = text(&o);
    assert!(out.contains("RE3001"), "{out}");
    assert!(out.contains("scripts/bad.luau"), "{out}");
}

#[test]
fn a_name_the_engine_does_not_provide_is_a_warning() {
    let dir = project(&[(
        "scripts/roblox.luau",
        "local p = game:GetService(\"Players\")\nphysics.overlap()\n",
    )]);
    let game = dir.path().join("game");
    let o = real(&["check"], &game);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let out = text(&o);
    assert!(out.contains("RE0903"), "{out}");
    assert!(out.contains("physics.overlap"), "{out}");

    // Verify's scan stage reports the same, as warnings.
    let o = real(&["verify", "--json"], &game);
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let scan = report["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "scan")
        .unwrap();
    assert_eq!(scan["ok"], true, "{scan}");
    assert!(
        scan["data"]["missing"]["physics.overlap"].is_number(),
        "{scan}"
    );
    let compile = report["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "compile")
        .unwrap();
    assert_eq!(compile["data"]["scripts"], 1);
}

#[test]
fn check_types_without_a_checker_is_re3090() {
    let dir = project(&[("scripts/ok.luau", "print(\"hi\")\n")]);
    let game = dir.path().join("game");
    let o = real(
        &["check", "--types", "--checker", "no-such-luau-lsp"],
        &game,
    );
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("RE3090"), "{}", text(&o));
}

#[test]
fn check_types_and_agent_init_write_the_engine_definitions() {
    let dir = project(&[("scripts/ok.luau", "print(\"hi\")\n")]);
    let game = dir.path().join("game");
    let defs = game.join(".real/types/engine.d.luau");
    let expected = real(&["describe", "--luau-defs"], &game);
    assert!(expected.status.success(), "{}", text(&expected));

    // Written even when the checker is missing: editors use the file too.
    real(
        &["check", "--types", "--checker", "no-such-luau-lsp"],
        &game,
    );
    assert_eq!(std::fs::read(&defs).unwrap(), expected.stdout);

    std::fs::remove_file(&defs).unwrap();
    let o = real(&["agent", "init"], &game);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(std::fs::read(&defs).unwrap(), expected.stdout);
}

#[test]
fn check_corpus_needs_directories() {
    let dir = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_real"))
        .args(["check", "--corpus"])
        .current_dir(dir.path())
        .env_remove("REAL_SCENE_SCRIPTS")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));

    std::fs::create_dir_all(dir.path().join("corpus")).unwrap();
    std::fs::write(dir.path().join("corpus/a.lua"), "local t = {}\n").unwrap();
    let o = real(
        &["check", "--corpus", "--scripts-dir", "corpus", "--json"],
        dir.path(),
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(report["files"], 1);

    // A directory alone needs no project either.
    let o = real(&["check", "--scripts-dir", "corpus"], dir.path());
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
}
