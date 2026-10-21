//! The mixer graph in the running game (ADR-0041): `real.toml [audio]` builds
//! the buses, an `AudioSource` with a `bus` plays on it, and scripts reach the
//! mixer through the `audio` library.

use std::path::Path;

use re_app::{App, AppConfig};
use re_types::AudioCmd;

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "steps", "components": {
        "AudioSource": { "clip": "tone", "looping": true, "spatial": false, "autoplay": true, "bus": "footsteps" },
        "ScriptRef": { "path": "scripts/mix.luau" } } }
  ]
}
"#;

const MIX: &str = r#"time.on_step(function()
	audio.set_bus_volume("music", -6, 0.5)
	local l = audio.levels("footsteps")
	print(`levels {l.peak_db} {l.rms_db}`)
	print(`missing {pcall(audio.levels, "nowhere")}`)
end)
"#;

const AUDIO: &str = r#"
[audio]
default_bus = "sfx"

[audio.buses.music]
volume_db = -6.0

[audio.buses.sfx]
effects = [{ kind = "compressor", threshold_db = -18.0, ratio = 3.0 }]

[audio.buses.footsteps]
parent = "sfx"
effects = [{ kind = "filter", mode = "high_pass", cutoff_hz = 120.0 }]
"#;

fn project(audio: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("scenes")).unwrap();
    std::fs::create_dir_all(d.join("scripts")).unwrap();
    std::fs::write(
        d.join("real.toml"),
        format!("[project]\nname = \"audio\"\n{audio}"),
    )
    .unwrap();
    std::fs::write(d.join("scenes/main.scene.json"), SCENE).unwrap();
    std::fs::write(d.join("scripts/mix.luau"), MIX).unwrap();
    dir
}

fn open(dir: &Path) -> App {
    let mut app = App::open(dir, AppConfig::load(dir).unwrap()).unwrap();
    app.audio_mut()
        .insert_clip("tone", re_audio::sine(440.0, 1.0, 48_000, 0.5));
    app
}

#[test]
fn the_project_graph_is_built_and_a_source_plays_on_its_bus() {
    let dir = project(AUDIO);
    let mut app = open(dir.path());
    let buses: Vec<&str> = app
        .audio_mut()
        .graph()
        .buses
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(buses, ["footsteps", "music", "sfx"]);
    let report = app.run_headless(Some(1), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);

    let cmds = app.audio_commands();
    let start = cmds
        .iter()
        .position(|c| matches!(c, AudioCmd::StartSource { clip, .. } if clip == "tone"))
        .expect("the source starts");
    assert!(
        matches!(&cmds[start - 1], AudioCmd::SetSourceBus { bus, .. } if bus == "footsteps"),
        "{cmds:?}"
    );
    assert!(
        cmds.iter().any(|c| matches!(
            c,
            AudioCmd::SetBusVolume { bus, volume_db, seconds } if bus == "music" && *volume_db == -6.0 && *seconds == 0.5
        )),
        "{cmds:?}"
    );
    // Nothing renders headless between frames, so the bus is silent.
    assert_eq!(
        report.output,
        ["[game] levels -60 -60", "[game] missing false"],
        "{:?}",
        report.output
    );
}

#[test]
fn an_invalid_graph_is_reported_and_the_default_graph_plays() {
    let dir = project("\n[audio.buses.footsteps]\nparent = \"sxf\"\n");
    let mut app = open(dir.path());
    let problems = app.take_diagnostics();
    assert!(
        problems.iter().any(|d| d.code.to_string() == "RE7407"),
        "{problems:?}"
    );
    assert!(app.audio_mut().graph().is_default());
    // The source's bus is unknown on the default graph: one warning, and it
    // plays on the default bus.
    let report = app.run_headless(Some(2), None).unwrap();
    let warnings: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code.to_string() == "RE7408")
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}
