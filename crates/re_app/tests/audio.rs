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

const MIX: &str = r#"time.on_frame(function()
	audio.set_bus_volume("music", -6, 0.5)
	local l = audio.levels("footsteps")
	print(`levels {l.peak_db} {l.rms_db}`)
	print(`missing {pcall(audio.levels, "nowhere")}`)
	-- Checked against the project graph: sfx has one effect and no snapshots.
	print(`effect {pcall(audio.set_effect, "sfx", 1, { ratio = 4 })} {pcall(audio.set_effect, "sfx", 2, { mix = 1 })}`)
	print(`bus {pcall(audio.set_bus_volume, "musik", 0)} snapshot {pcall(audio.snapshot, "default")} {pcall(audio.snapshot, "underwater")}`)
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
        [
            "[game] levels -60 -60",
            "[game] missing false",
            "[game] effect true false",
            "[game] bus false snapshot true false",
        ],
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

const ENGINE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "engine", "components": {
        "AudioSource": { "clip": "tone", "looping": true, "spatial": false, "autoplay": true, "rate": 1.5 },
        "ScriptRef": { "path": "scripts/pitch.luau" } } },
    { "id": "music", "components": {
        "AudioSource": { "clip": "tone", "looping": true, "spatial": false, "autoplay": true } } },
    { "id": "lamp", "components": { "Transform": {} } }
  ]
}
"#;

const PITCH: &str = r#"local frames = 0
time.on_frame(function()
	frames += 1
	local engine = world.get("engine")
	if frames == 2 then
		audio.set_rate(engine, 0.75)
		local zero, why_zero = pcall(audio.set_rate, engine, 0)
		local lamp, why_lamp = pcall(audio.set_rate, world.get("lamp"), 1)
		local zero_why = string.find(tostring(why_zero), "above 0", 1, true) ~= nil
		local lamp_why = string.find(tostring(why_lamp), "no AudioSource", 1, true) ~= nil
		print(`rate {engine:get("AudioSource", "rate")} {zero} {zero_why} {lamp} {lamp_why}`)
	elseif frames == 4 then
		engine:set("AudioSource", "bus", "music")
	elseif frames == 5 then
		engine:set("AudioSource", "rate", 0)
	end
end)
"#;

/// The rate of each command that sets `entity`'s playback rate.
fn rates(cmds: &[AudioCmd], entity: re_types::EntityId) -> Vec<f32> {
    cmds.iter()
        .filter_map(|c| match c {
            AudioCmd::SetSourceRate { entity: e, rate } if *e == entity => Some(*rate),
            _ => None,
        })
        .collect()
}

#[test]
fn a_source_plays_at_its_rate_and_scripts_change_it() {
    let dir = project(AUDIO);
    std::fs::write(dir.path().join("scenes/main.scene.json"), ENGINE).unwrap();
    std::fs::write(dir.path().join("scripts/pitch.luau"), PITCH).unwrap();
    let mut app = open(dir.path());
    let id = |app: &App, name: &str| re_scene::entity_id(app.world().entity(name).unwrap());
    let (engine, music) = (id(&app, "engine"), id(&app, "music"));

    // The rate follows the start; a source at rate 1 sends none.
    let report = app.run_headless(Some(1), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let cmds = app.audio_commands();
    let start = cmds
        .iter()
        .position(|c| matches!(c, AudioCmd::StartSource { entity, .. } if *entity == engine))
        .expect("the engine starts");
    assert_eq!(
        cmds[start + 1],
        AudioCmd::SetSourceRate {
            entity: engine,
            rate: 1.5
        },
        "{cmds:?}"
    );
    assert!(rates(cmds, music).is_empty(), "{cmds:?}");

    // A script sets it in the frame its sound follows; a bad rate or an entity
    // without an AudioSource is refused.
    let report = app.run_headless(Some(1), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.output, ["[game] rate 0.75 false true false true"]);
    assert_eq!(rates(app.audio_commands(), engine), [0.75]);
    // An unchanged rate is not sent again.
    let report = app.run_headless(Some(1), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(rates(app.audio_commands(), engine).is_empty());

    // A change of bus starts the sound again, and the rate follows the start.
    let report = app.run_headless(Some(1), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let cmds = app.audio_commands();
    let start = cmds
        .iter()
        .position(|c| matches!(c, AudioCmd::StartSource { entity, .. } if *entity == engine))
        .expect("the engine starts again");
    assert!(
        cmds[..start].contains(&AudioCmd::StopSource { entity: engine }),
        "{cmds:?}"
    );
    assert_eq!(
        cmds[start - 1],
        AudioCmd::SetSourceBus {
            entity: engine,
            bus: "music".into()
        },
        "{cmds:?}"
    );
    assert_eq!(
        cmds[start + 1],
        AudioCmd::SetSourceRate {
            entity: engine,
            rate: 0.75
        },
        "{cmds:?}"
    );

    // A rate the mixer refuses (0, set with `e:set`) is sent and refused once.
    let report = app.run_headless(Some(1), None).unwrap();
    assert_eq!(rates(app.audio_commands(), engine), [0.0]);
    let codes: Vec<String> = report
        .diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect();
    assert_eq!(codes, ["RE7405"], "{:?}", report.diagnostics);
    let report = app.run_headless(Some(1), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(rates(app.audio_commands(), engine).is_empty());
}
