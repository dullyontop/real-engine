//! A project handed over file by file, as the boot script does, plays its
//! replays to the same `state_hash` as the desktop runtime.

use std::path::Path;

use re_web::Session;

fn add_dir(session: &mut Session, root: &Path, dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");
        if rel.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            add_dir(session, root, &path);
        } else {
            session
                .add_file(&rel, &std::fs::read(&path).unwrap())
                .unwrap();
        }
    }
}

#[test]
fn island_replays_match_the_desktop_runtime() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/island");
    let mut session = Session::new();
    add_dir(&mut session, &root, &root);
    for replay in [
        "tests/replays/walk_collect.replay.json",
        "tests/replays/full_game.replay.json",
        "tests/replays/regenerate.replay.json",
    ] {
        let web: serde_json::Value =
            serde_json::from_str(&session.run_replay(replay).unwrap()).unwrap();
        let doc = serde_json::from_slice(&std::fs::read(root.join(replay)).unwrap()).unwrap();
        let mut app = re_app::App::open_project(&root).unwrap();
        let desktop = app.run_headless(None, Some(doc)).unwrap();
        assert_eq!(web["state_hash"], desktop.state_hash, "{replay}");
        assert_eq!(web["errors"], serde_json::json!([]), "{replay}");
    }
}

#[test]
fn a_gpu_run_draws_without_errors_and_keeps_the_state_hash() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/island");
    let mut session = Session::new();
    add_dir(&mut session, &root, &root);
    let replay = "tests/replays/walk_collect.replay.json";
    let plain: serde_json::Value =
        serde_json::from_str(&session.run_replay(replay).unwrap()).unwrap();
    let backends = re_render::backends_from_env().expect("RE_BACKENDS");
    let gpu: serde_json::Value = serde_json::from_str(
        &pollster::block_on(session.run_replay_gpu(replay, backends, 120)).unwrap(),
    )
    .unwrap();
    assert_eq!(gpu["errors"], serde_json::json!([]));
    assert_eq!(gpu["state_hash"], plain["state_hash"]);
    let colors = gpu["last_frame"]["distinct_colors"].as_u64().unwrap();
    assert!(
        colors > 100,
        "the last frame shows the island: {}",
        gpu["last_frame"]
    );
    assert_eq!(gpu["drawn"], 6, "frames 0, 120, ..., 480 and the last");
}
