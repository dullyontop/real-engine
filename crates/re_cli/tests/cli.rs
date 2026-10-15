//! The `real` binary end to end: `new`, `run --headless`, `verify`, `describe`,
//! `explain` and `schema`.

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

#[test]
fn new_then_run_verify_and_describe() {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["new", "game"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    let game = dir.path().join("game");
    assert!(game.join("real.toml").is_file());

    let o = real(
        &[
            "run",
            "--project",
            "game",
            "--headless",
            "--frames",
            "60",
            "--exit-after",
        ],
        dir.path(),
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("RUN OK"));

    let o = real(&["verify", "--json"], &game);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["stages"].as_array().unwrap().len(), 7);

    let o = real(&["describe", "--json"], &game);
    assert!(o.status.success(), "{}", text(&o));
    let d: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let commands: Vec<&str> = d["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(commands.contains(&"verify"));
    assert!(commands.contains(&"explain"));
    assert!(commands.contains(&"entity.spawn"));

    // `new` refuses to overwrite.
    let o = real(&["new", "game"], dir.path());
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("RE0901"));
}

#[test]
fn verify_outside_a_project_is_re0500() {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["verify"], dir.path());
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("RE0500"));
    // With --json there is still one report on stdout.
    let o = real(&["verify", "--json"], dir.path());
    assert_eq!(o.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["stages"][0]["name"], "schema");
}

#[test]
fn explain_prints_pages_and_suggests_codes() {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["explain", "RE0502"], dir.path());
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("# RE0502"));
    let o = real(&["explain", "RE5004"], dir.path());
    assert!(o.status.success(), "physics codes are linked");
    let o = real(&["explain", "RE0599"], dir.path());
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("RE0900"));
    let o = real(&["explain"], dir.path());
    assert!(String::from_utf8_lossy(&o.stdout).contains("RE1200"));
}

#[test]
fn schema_writes_every_schema() {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["schema", "--out", "s"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    for name in [
        "graph.schema.json",
        "prefab.schema.json",
        "replay.schema.json",
        "scene.schema.json",
        "verify-report.schema.json",
    ] {
        let t = std::fs::read_to_string(dir.path().join("s").join(name)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
        assert!(v.is_object(), "{name}");
    }
}

#[test]
fn usage_errors_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    real(&["new", "g"], dir.path());
    let o = real(&["run", "--project", "g", "--frames", "1"], dir.path());
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let o = real(&["frobnicate"], dir.path());
    assert_eq!(o.status.code(), Some(2));
}
