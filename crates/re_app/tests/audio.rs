//! The mixer graph in the running game (ADR-0041): `real.toml [audio]` builds
//! the buses, an `AudioSource` with a `bus` plays on it, and scripts reach the
//! mixer through the `audio` library.

use std::path::Path;

use re_app::sync::audio::AudioSync;
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
    // Scripts read silence on the headless backend (see below).
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
    // Then the frame renders its audio (headless, nothing else does), and the
    // bus meters the tone: a 0.5 sine peaks at -6 dB.
    let steps = app
        .bus_levels()
        .iter()
        .find(|l| l.bus == "footsteps")
        .expect("the footsteps bus is metered");
    assert!(
        steps.peak_db > -7.0 && steps.rms_db > -12.0,
        "{:?}",
        app.bus_levels()
    );
    // The script still reads silence: what it writes from `on_frame` is in the
    // state hash, and a rendered mix depends on the platform's maths.
    let report = app.run_headless(Some(1), None).unwrap();
    assert_eq!(
        report.output.first().map(String::as_str),
        Some("[game] levels -60 -60"),
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

const WALKER: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "walker", "components": { "ScriptRef": { "path": "scripts/steps.luau" } } }
  ]
}
"#;

const STEPS: &str = r#"time.on_frame(function()
	audio.play("step", vector.create(1, 0, 0), 1, "footsteps")
end)
"#;

#[test]
fn one_shots_end_and_free_their_voices_in_a_headless_run() {
    // A spatial one-shot holds one of its bus's 256 voices until it ends, and
    // it ends only as the mixer renders.
    let dir = project(AUDIO);
    std::fs::write(dir.path().join("scenes/main.scene.json"), WALKER).unwrap();
    std::fs::write(dir.path().join("scripts/steps.luau"), STEPS).unwrap();
    let mut app = open(dir.path());
    app.audio_mut()
        .insert_clip("step", re_audio::sine(440.0, 0.1, 48_000, 0.5));
    // A step every frame for 5 s, each 0.1 s long: 6 or 7 at a time.
    let report = app.run_headless(Some(300), None).unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let playing = app.audio_mut().playing();
    assert!((5..=8).contains(&playing), "{playing} playing");
}

// The audio sync on its own, from a scene, with the commands a frame's scripts
// queued before it.

fn world(json: &str) -> re_scene::World {
    let mut w = re_scene::World::new();
    w.load(&re_scene::scene::from_json(json).unwrap()).unwrap();
    w.propagate_all();
    w
}

fn sync(w: &mut re_scene::World, s: &mut AudioSync, mut cmds: Vec<AudioCmd>) -> Vec<AudioCmd> {
    re_app::sync::audio::build(w, s, &mut cmds);
    cmds
}

fn listeners(cmds: &[AudioCmd]) -> Vec<glam::Vec3> {
    cmds.iter()
        .filter_map(|c| match c {
            AudioCmd::SetListener { position, .. } => Some(*position),
            _ => None,
        })
        .collect()
}

const LISTENING: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "cam", "components": { "Camera": { "active": true }, "Transform": { "translation": [0, 5, 10] } } },
    { "id": "head", "components": { "AudioListener": {}, "Transform": { "translation": [1, 2, 3] } } },
    { "id": "spare", "components": { "AudioListener": { "active": false }, "Transform": { "translation": [7, 7, 7] } } }
  ]
}
"#;

#[test]
fn an_audio_listener_hears_in_place_of_the_camera() {
    let mut w = world(LISTENING);
    let mut s = AudioSync::new();
    assert_eq!(
        listeners(&sync(&mut w, &mut s, Vec::new())),
        [glam::Vec3::new(1.0, 2.0, 3.0)]
    );
    // Unchanged, it is not sent again.
    assert!(listeners(&sync(&mut w, &mut s, Vec::new())).is_empty());
    // Without an active listener, the active camera hears.
    w.set(
        "head",
        "AudioListener",
        "active",
        re_reflect::Value::Bool(false),
    )
    .unwrap();
    assert_eq!(
        listeners(&sync(&mut w, &mut s, Vec::new())),
        [glam::Vec3::new(0.0, 5.0, 10.0)]
    );
    w.set(
        "spare",
        "AudioListener",
        "active",
        re_reflect::Value::Bool(true),
    )
    .unwrap();
    assert_eq!(
        listeners(&sync(&mut w, &mut s, Vec::new())),
        [glam::Vec3::new(7.0, 7.0, 7.0)]
    );
}

const SOURCES: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "radio", "components": {
        "AudioSource": { "clip": "tone", "looping": true, "autoplay": false, "bus": "music", "rate": 1.5,
                         "rolloff": "Inverse", "min_distance": 2, "max_distance": 40, "doppler": 1 },
        "Transform": { "translation": [0, 0, -4] } } },
    { "id": "hum", "components": {
        "AudioSource": { "clip": "tone", "looping": true, "autoplay": true },
        "Transform": { "translation": [0, 0, -8] } } }
  ]
}
"#;

#[test]
fn scripts_start_and_fade_sources_through_the_sync() {
    use re_types::{Rolloff, SoundOp, Spatial};
    let mut w = world(SOURCES);
    let id = |w: &re_scene::World, name: &str| re_scene::entity_id(w.entity(name).unwrap());
    let (radio, hum) = (id(&w, "radio"), id(&w, "hum"));
    let mut s = AudioSync::new();
    // The hum autoplays with the default settings, so none are sent.
    let cmds = sync(&mut w, &mut s, Vec::new());
    assert!(
        cmds.iter()
            .any(|c| matches!(c, AudioCmd::StartSource { entity, .. } if *entity == hum)),
        "{cmds:?}"
    );
    assert!(
        !cmds
            .iter()
            .any(|c| matches!(c, AudioCmd::SetSourceSpatial { .. })),
        "{cmds:?}"
    );

    // A script's `start` becomes the radio's start in its place, as autoplay
    // would send it, settings first; the commands around it keep their order.
    let before = AudioCmd::ApplySnapshot {
        name: "default".into(),
        seconds: 0.0,
    };
    let seek = AudioCmd::SourceControl {
        entity: radio,
        op: SoundOp::Seek { seconds: 1.0 },
    };
    let cmds = sync(
        &mut w,
        &mut s,
        vec![
            before.clone(),
            AudioCmd::SourceControl {
                entity: radio,
                op: SoundOp::Start,
            },
            seek.clone(),
        ],
    );
    let spatial = Spatial {
        rolloff: Rolloff::Inverse,
        min_distance: 2.0,
        max_distance: 40.0,
        doppler: 1.0,
    };
    assert_eq!(
        cmds,
        [
            before,
            AudioCmd::SetSourceSpatial {
                entity: radio,
                spatial,
            },
            AudioCmd::SetSourceBus {
                entity: radio,
                bus: "music".into(),
            },
            AudioCmd::StartSource {
                entity: radio,
                clip: "tone".into(),
                volume: 1.0,
                looping: true,
                spatial: true,
                position: glam::Vec3::new(0.0, 0.0, -4.0),
            },
            AudioCmd::SetSourceRate {
                entity: radio,
                rate: 1.5,
            },
            seek,
        ]
    );

    // A script fading the volume writes the field and sends the fade in the
    // same frame: the sync does not cut the fade with an instant change.
    let value = |v: f64| re_reflect::Value::Float(v);
    w.set("radio", "AudioSource", "volume", value(0.25))
        .unwrap();
    w.set("radio", "AudioSource", "rate", value(0.5)).unwrap();
    let fades = vec![
        AudioCmd::SourceControl {
            entity: radio,
            op: SoundOp::Volume {
                volume: 0.25,
                fade: 2.0,
            },
        },
        AudioCmd::SourceControl {
            entity: radio,
            op: SoundOp::Rate {
                rate: 0.5,
                fade: 2.0,
            },
        },
    ];
    assert_eq!(sync(&mut w, &mut s, fades.clone()), fades);
    // A write without a fade is sent as it is.
    w.set("radio", "AudioSource", "volume", value(1.0)).unwrap();
    assert_eq!(
        sync(&mut w, &mut s, Vec::new()),
        [AudioCmd::SetSourceVolume {
            entity: radio,
            volume: 1.0,
        }]
    );

    // Settings changed while it plays follow it; unchanged, nothing is sent.
    w.set(
        "radio",
        "AudioSource",
        "rolloff",
        re_reflect::Value::Enum("Off".into()),
    )
    .unwrap();
    let cmds = sync(&mut w, &mut s, Vec::new());
    assert_eq!(
        cmds,
        [AudioCmd::SetSourceSpatial {
            entity: radio,
            spatial: Spatial {
                rolloff: Rolloff::Off,
                ..spatial
            },
        }]
    );
    assert!(sync(&mut w, &mut s, Vec::new()).is_empty());

    // A `start` on an entity without an AudioSource is left for the mixer,
    // which ignores it.
    let stray = AudioCmd::SourceControl {
        entity: re_types::EntityId(u64::MAX),
        op: SoundOp::Start,
    };
    assert_eq!(sync(&mut w, &mut s, vec![stray.clone()]), [stray]);
}
