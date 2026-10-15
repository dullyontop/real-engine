//! `real do` against an open editor: the command goes through the editor's
//! socket, is applied to the editor's World and saved by the editor.
#![cfg(all(feature = "tooling", unix))]

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use re_app::{App, AppConfig};
use re_command::AllowAll;
use re_editor::EditorSocket;

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

/// Runs `real` in `cwd` while the editor (`socket`, `app`) answers its
/// socket, as the editor does between frames.
fn real_with_editor(args: &[&str], cwd: &Path, socket: &EditorSocket, app: &mut App) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_real"))
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the real binary runs");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        socket.serve_pending(app, &AllowAll);
        assert!(Instant::now() < deadline, "`real do` did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn do_goes_through_an_open_editor() {
    let dir = tempfile::tempdir().unwrap();
    let o = real(&["new", "game"], dir.path());
    assert!(o.status.success(), "{}", text(&o));
    let game = dir.path().join("game");

    // What `real editor` does: the lock and socket first, then the project
    // with edits saved.
    let socket = EditorSocket::open(&game).unwrap();
    assert!(socket.path().is_some(), "{:?}", socket.warning());
    let config = AppConfig {
        persist_edits: true,
        ..AppConfig::load(&game).unwrap()
    };
    let mut app = App::open(&game, config).unwrap();

    let spawn = r#"{"id": "proxied", "name": "Proxied", "components": {"Transform": {"translation": [0.0, 3.5, 0.0]}}}"#;
    let o = real_with_editor(
        &["do", "entity.spawn", "--args", spawn, "--json"],
        &game,
        &socket,
        &mut app,
    );
    assert!(o.status.success(), "{}", text(&o));
    let out: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["via"], "editor", "{out}");

    // Applied to the editor's World, and saved by the editor.
    assert!(app.world().entity("proxied").is_ok());
    let scene = std::fs::read_to_string(game.join("scenes/main.scene.json")).unwrap();
    assert!(scene.contains("\"proxied\""), "{scene}");

    // A read answers from the editor's World.
    let o = real_with_editor(
        &[
            "do",
            "entity.get",
            "--args",
            r#"{"id": "proxied"}"#,
            "--json",
        ],
        &game,
        &socket,
        &mut app,
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("3.5"),
        "{}",
        text(&o)
    );

    // A dry run through the editor changes nothing.
    let before = std::fs::read_to_string(game.join("scenes/main.scene.json")).unwrap();
    let o = real_with_editor(
        &[
            "do",
            "entity.delete",
            "--args",
            r#"{"id": "proxied"}"#,
            "--dry-run",
            "--json",
        ],
        &game,
        &socket,
        &mut app,
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(app.world().entity("proxied").is_ok());
    let after = std::fs::read_to_string(game.join("scenes/main.scene.json")).unwrap();
    assert_eq!(before, after);

    // Once the editor closes, `real do` takes the project itself.
    drop(socket);
    let o = real(
        &[
            "do",
            "entity.get",
            "--args",
            r#"{"id": "proxied"}"#,
            "--json",
        ],
        &game,
    );
    assert!(o.status.success(), "{}", text(&o));
    let out: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(out["via"], "local", "{out}");
}
