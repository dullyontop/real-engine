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

    std::fs::create_dir_all(game.join("scripts")).unwrap();
    std::fs::write(game.join("scripts/hello.luau"), "print(\"hi\")\n").unwrap();
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
    let nodes: Vec<&str> = d["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap())
        .collect();
    assert!(nodes.contains(&"terrain.heightfield"), "{nodes:?}");
    assert_eq!(
        d["scripts"],
        serde_json::json!([{ "path": "scripts/hello.luau" }])
    );

    // `new` refuses to overwrite.
    let o = real(&["new", "game"], dir.path());
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("RE0901"));
}

/// ADR-0065 §8: a new project's default backends (`["primary", "gl"]`)
/// find a GPU adapter on every platform without `RE_BACKENDS`. With
/// `["vulkan", "gl"]`, a Mac found none (`RE6002`). A screenshot needs the
/// device, so the run takes one.
#[test]
fn a_new_project_draws_with_the_default_backends() {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["new", "game"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    let o = Command::new(env!("CARGO_BIN_EXE_real"))
        .args(["run", "--project", "game", "--headless", "--frames", "2"])
        .args(["--screenshot", "shot.png", "--json"])
        .env_remove("RE_BACKENDS")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(report["screenshot"].is_string(), "{report}");
    // A path on the command line is relative to the current directory.
    assert!(dir.path().join("shot.png").is_file());
}

/// `real new` writes `schemas/` beside the files that name it, says what to
/// do next, and refuses `--project`, which it would not use.
#[test]
fn new_writes_schemas_and_next_steps() {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["new", "game"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("next steps:\n  cd game\n"), "{out}");
    assert!(dir.path().join("game/schemas/scene.schema.json").is_file());
    assert!(
        dir.path()
            .join("game/schemas/material.schema.json")
            .is_file()
    );

    let o = real(&["new", "other", "--project", "game"], dir.path());
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("takes no `--project`"), "{}", text(&o));
    assert!(!dir.path().join("other").exists());
}

/// Paths typed on the command line are relative to the current directory;
/// a project file named there is also found in the project, and default
/// outputs go in the project.
#[test]
fn command_line_paths_have_one_base() {
    let dir = tempfile::tempdir().unwrap();
    real(&["new", "game"], dir.path());
    let o = real(&["schema", "--project", "game"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    assert!(!dir.path().join("schemas").exists());
    assert!(dir.path().join("game/schemas/graph.schema.json").is_file());

    // A replay named as the project names it, from outside the project.
    let replay = serde_json::json!({ "version": 1, "seed": 1, "frames": 3 });
    std::fs::create_dir_all(dir.path().join("game/tests/replays")).unwrap();
    std::fs::write(
        dir.path().join("game/tests/replays/idle.replay.json"),
        replay.to_string(),
    )
    .unwrap();
    let o = real(
        &[
            "run",
            "--project",
            "game",
            "--headless",
            "--replay",
            "tests/replays/idle.replay.json",
            "--json",
        ],
        dir.path(),
    );
    assert!(o.status.success(), "{}", text(&o));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(report["frames"], 3, "{report}");
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
        "material.schema.json",
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
    let o = real(&["run", "--project", "g", "--frmaes", "1"], dir.path());
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let o = real(&["frobnicate"], dir.path());
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn run_headless_writes_a_screenshot() {
    let dir = tempfile::tempdir().unwrap();
    real(&["new", "g"], dir.path());
    let o = real(
        &[
            "run",
            "--project",
            "g",
            "--headless",
            "--frames",
            "30",
            "--screenshot",
            "out/f30.png",
        ],
        dir.path(),
    );
    assert!(o.status.success(), "{}", text(&o));
    let png = std::fs::read(dir.path().join("out/f30.png")).unwrap();
    assert_eq!(&png[1..4], b"PNG");
}

#[test]
fn graph_eval_reports_each_node() {
    let dir = tempfile::tempdir().unwrap();
    let graph = serde_json::json!({
        "format": "real-engine/graph", "version": 1, "name": "flat", "seed": 3,
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 9, "cell": 4.0 } },
            { "id": "ground", "kind": "terrain.mesh", "version": 3, "params": { "collider": true } }
        ],
        "edges": [ { "from": ["h", "field"], "to": ["ground", "field"] } ]
    });
    std::fs::write(dir.path().join("flat.graph.json"), graph.to_string()).unwrap();
    let o = real(&["graph", "eval", "flat.graph.json", "--json"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(report["seed"], 3);
    assert_eq!(report["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(report["entities"], 1);

    std::fs::write(
        dir.path().join("bad.graph.json"),
        graph.to_string().replace("terrain.mesh", "terrain.nope"),
    )
    .unwrap();
    let o = real(&["graph", "eval", "bad.graph.json"], dir.path());
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("RE4001"), "{}", text(&o));
}

#[test]
fn run_debug_reports_a_port_in_use() {
    let dir = tempfile::tempdir().unwrap();
    real(&["new", "g"], dir.path());
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let o = real(
        &[
            "run",
            "--project",
            "g",
            "--headless",
            "--frames",
            "1",
            "--debug",
            &port,
        ],
        dir.path(),
    );
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("RE3071"), "{}", text(&o));
}

#[test]
fn run_debug_serves_dap_on_localhost() {
    let dir = tempfile::tempdir().unwrap();
    real(&["new", "g"], dir.path());
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port().to_string();
    drop(free);
    let o = real(
        &[
            "run",
            "--project",
            "g",
            "--headless",
            "--frames",
            "5",
            "--exit-after",
            "--debug",
            &port,
        ],
        dir.path(),
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains(&format!("debugger: DAP on 127.0.0.1:{port}")),
        "{}",
        text(&o)
    );
}
