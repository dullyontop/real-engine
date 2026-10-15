//! `real import` and `real new --template roblox` (WP16). The importer's own
//! behaviour is tested in `re_import`; these check the command line around it.
#![cfg(feature = "tooling")]

use std::path::{Path, PathBuf};
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

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../re_import/tests/fixtures")
        .join(name)
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(o)))
}

#[test]
fn new_roblox_makes_a_compat_project() {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["new", "place", "--template", "roblox"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    let toml = std::fs::read_to_string(dir.path().join("place/real.toml")).unwrap();
    assert!(
        re_compat::CompatConfig::from_project_toml(&toml)
            .unwrap()
            .is_some()
    );
    // The runtime reads the same real.toml.
    re_app::AppConfig::parse(&toml).unwrap();
    let scene = std::fs::read_to_string(dir.path().join("place/scenes/main.scene.json")).unwrap();
    assert!(scene.contains("\"id\": \"game\""), "{scene}");
    // Never over an existing project.
    let o = real(&["new", "place", "--template", "roblox"], dir.path());
    assert!(!o.status.success());
    assert!(text(&o).contains("RE0901"), "{}", text(&o));
}

#[test]
fn import_reports_as_json_and_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture("loader_basic.luau");
    let input = input.to_str().unwrap();
    let o = real(
        &["import", input, "--project", "dry", "--dry-run", "--json"],
        dir.path(),
    );
    let dry = json(&o);
    assert_eq!(dry["dry_run"], true);
    assert!(!dir.path().join("dry").exists());

    let o = real(
        &["import", input, "--project", "game", "--json"],
        dir.path(),
    );
    let report = json(&o);
    assert_eq!(report["format"], "loader");
    assert_eq!(report["unrecognised"].as_array().unwrap().len(), 3);
    // Seven instances are declared; each is created or reported as failed
    // (until the class layer defines the services they go into).
    let declared: u64 = report["classes"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(declared, 7);
    let created = report["created"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["path"].as_array().unwrap().len() > 1)
        .count();
    assert!(
        created + report["failed"].as_array().unwrap().len() >= 7,
        "{report:#}"
    );
    assert_eq!(o.status.success(), report["ok"] == true, "{}", text(&o));
    assert_eq!(dry["created"], report["created"]);
    assert!(dir.path().join("game/real.toml").exists());

    // Human output.
    let o = real(
        &["import", input, "--project", "game", "--dry-run"],
        dir.path(),
    );
    assert!(text(&o).contains("would import"), "{}", text(&o));
}

#[test]
fn an_unreadable_building_block_fails_with_its_code() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture("loader_bad_block.luau");
    let o = real(
        &["import", input.to_str().unwrap(), "--project", "p"],
        dir.path(),
    );
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("RE9803"), "{}", text(&o));
    assert!(!dir.path().join("p").exists());
    let o = real(&["explain", "RE9803"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("Loader statement not recognised"),
        "{}",
        text(&o)
    );
}

#[test]
fn import_refuses_a_native_project() {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["new", "native"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    let input = fixture("loader_basic.luau");
    let o = real(
        &["import", input.to_str().unwrap(), "--project", "native"],
        dir.path(),
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("RE9804"), "{}", text(&o));
}
