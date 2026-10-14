//! Replays: record, save, load and play back with identical action frames.

use glam::Vec2;
use re_input::{
    ActionFrame, ActionLayout, Button, InputConfig, RawInput, Replay, ReplayDoc, ReplayRecorder,
    load_replay, save_replay,
};
use re_types::replay::ReplayInput;
use winit::keyboard::KeyCode;

/// A scripted run over 12 frames: holds, taps, re-presses, stick and mouse motion.
fn live_frames() -> (ActionLayout, Vec<ActionFrame>) {
    let text = include_str!("fixtures/real.toml");
    let map = InputConfig::from_real_toml(text, "real.toml")
        .unwrap()
        .to_action_map()
        .unwrap();
    let mut raw = RawInput::default();
    let mut frames = Vec::new();
    for f in 0..12u64 {
        match f {
            1 => raw.set_button(Button::Key(KeyCode::Space), true),
            3 => raw.set_button(Button::Key(KeyCode::Space), false),
            4 => {
                // A tap shorter than a frame.
                raw.set_button(Button::Key(KeyCode::KeyF), true);
                raw.set_button(Button::Key(KeyCode::KeyF), false);
            }
            5 => {
                raw.set_button(Button::Key(KeyCode::KeyW), true);
                raw.set_pad_axis(gilrs::Axis::RightStickX, 0.9);
            }
            6 => raw.add_mouse_delta(12.5, -3.0),
            7 => {
                raw.set_button(Button::Pad(gilrs::Button::South), true);
                raw.set_pad_axis(gilrs::Axis::RightStickX, 0.0);
            }
            8 => {
                // Released and pressed again within one frame: a new press, still held.
                raw.set_button(Button::Pad(gilrs::Button::South), false);
                raw.set_button(Button::Pad(gilrs::Button::South), true);
            }
            10 => raw.release_all(),
            _ => {}
        }
        frames.push(map.frame(&raw, f));
        raw.end_frame();
    }
    (map.layout(), frames)
}

#[test]
fn record_save_load_play_gives_identical_frames() {
    let (layout, frames) = live_frames();
    assert!(frames[8].pressed("jump") && frames[8].is_down("jump") && !frames[8].released("jump"));

    let mut rec = ReplayRecorder::new(1234);
    for f in &frames {
        rec.record(f);
    }
    rec.command(
        6,
        "graph.regenerate",
        serde_json::json!({ "source": "island", "seed": 99 }),
    );
    rec.screenshot(11, "tests/goldens/{backend}/island_11.png");
    let doc = rec.finish();
    assert_eq!(doc.frames, 12);
    assert_eq!(doc.seed, 1234);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.replay.json");
    save_replay(&doc, &path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with('\n'));
    let loaded = load_replay(&path).unwrap();
    assert_eq!(loaded, doc);

    let mut replay = Replay::new(loaded, &layout).unwrap();
    let mut played = Vec::new();
    while let Some(f) = replay.next_frame() {
        played.push(f);
    }
    assert_eq!(played.len(), frames.len());
    for (live, play) in frames.iter().zip(&played) {
        assert_eq!(live, play, "frame {}", live.frame);
    }
    assert!(replay.is_finished());
    assert_eq!(replay.commands_at(6).count(), 1);
    assert_eq!(replay.checks_at(11).count(), 1);
}

#[test]
fn hand_written_replay_plays() {
    let text = r#"{ "version": 1, "seed": 1234, "frames": 4,
      "input": [ { "frame": 1, "action": "move", "axis": [0, 1] }, { "frame": 2, "action": "jump", "down": true } ] }"#;
    let doc: ReplayDoc = serde_json::from_str(text).unwrap();
    let mut r = Replay::new(doc, &ActionLayout::default()).unwrap();
    let f: Vec<ActionFrame> = std::iter::from_fn(|| r.next_frame()).collect();
    assert_eq!(f[0].axis("move"), Vec2::ZERO);
    assert_eq!(f[1].axis("move"), Vec2::Y);
    assert_eq!(
        f[3].axis("move"),
        Vec2::Y,
        "an action keeps its value until changed"
    );
    assert!(!f[1].is_down("jump"));
    assert!(f[2].pressed("jump"));
    assert!(f[3].is_down("jump") && !f[3].pressed("jump"));
}

#[test]
fn invalid_replays_are_diagnostics() {
    let (layout, _) = live_frames();
    let input = |frame, action: &str, down, axis| ReplayInput {
        frame,
        action: action.into(),
        down,
        axis,
    };

    let mut doc = ReplayDoc::new(1, 10);
    doc.input = vec![
        input(3, "jump", Some(true), Some([0.0, 1.0])),
        input(1, "jump", Some(false), None),
    ];
    let errs = Replay::new(doc, &layout).unwrap_err();
    let codes: Vec<String> = errs.iter().map(|d| d.code.to_string()).collect();
    assert_eq!(codes, ["RE7204", "RE7204"], "{errs:#?}");

    let mut doc = ReplayDoc::new(1, 10);
    doc.input = vec![
        input(0, "walk", Some(true), None),
        input(0, "move", Some(true), None),
    ];
    let errs = Replay::new(doc, &layout).unwrap_err();
    assert!(
        errs.iter().all(|d| d.code.to_string() == "RE7205"),
        "{errs:#?}"
    );
    assert_eq!(errs.len(), 2);

    let mut doc = ReplayDoc::new(1, 10);
    doc.version = 9;
    assert_eq!(
        Replay::new(doc, &layout).unwrap_err()[0].code.to_string(),
        "RE7204"
    );
}

#[test]
fn bad_replay_files_are_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let err = load_replay(&dir.path().join("missing.replay.json")).unwrap_err();
    assert_eq!(err.code.to_string(), "RE7206");

    let path = dir.path().join("bad.replay.json");
    std::fs::write(
        &path,
        "{ \"version\": 1, \"seed\": 1, \"frames\": 2,\n  \"frame_count\": 3 }",
    )
    .unwrap();
    let err = load_replay(&path).unwrap_err();
    assert_eq!(err.code.to_string(), "RE7207");
    assert!(err.render_human().contains(":2:"), "{}", err.render_human());
}
