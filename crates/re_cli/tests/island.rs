//! The island demo (`examples/island`) through the `real` binary: its scripts
//! type-check clean with luau-lsp 1.70.1, and a copy with a deliberate type
//! error is reported.
#![cfg(feature = "tooling")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn island() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/island")
}

fn real(args: &[&str], project: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_real"))
        .arg("--project")
        .arg(project)
        .args(args)
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

/// luau-lsp is an external tool (ADR-0026): without it on PATH these tests
/// say so and pass; `verify-demo` and the acceptance commands install it.
fn have_checker() -> bool {
    let found = Command::new("luau-lsp").arg("--version").output().is_ok();
    if !found {
        eprintln!("luau-lsp is not on PATH; skipping the type check");
    }
    found
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn the_island_type_checks_clean() {
    if !have_checker() {
        return;
    }
    let o = real(&["check", "--types"], &island());
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("types: 0 errors"), "{}", text(&o));
}

#[test]
fn a_type_error_in_the_island_is_reported() {
    if !have_checker() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    copy_dir(&island().join("scripts"), &project.join("scripts"));
    std::fs::copy(island().join("real.toml"), project.join("real.toml")).unwrap();
    std::fs::write(
        project.join("scripts/game/broken.luau"),
        "--!strict\nlocal hud = script.entity :: Entity\nlocal count: number = hud.name\nprint(count)\n",
    )
    .unwrap();
    let o = real(&["check", "--types"], &project);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let out = text(&o);
    assert!(out.contains("RE3081"), "{out}");
    assert!(out.contains("scripts/game/broken.luau:3"), "{out}");
}
