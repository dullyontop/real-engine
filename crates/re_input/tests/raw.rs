//! Raw input frames: events in order with their names, held keys, and raw
//! replay rows played back and recorded.

use re_input::{Button, InputKind, InputPhase, RawInput, Replay, ReplayRecorder};
use re_types::replay::{RawReplayInput, ReplayDoc};
use winit::event::MouseButton;
use winit::keyboard::KeyCode;

#[test]
fn events_keep_their_order_and_names() {
    let mut raw = RawInput::default();
    raw.set_button(Button::Key(KeyCode::KeyW), true);
    raw.set_cursor(10.0, 20.0);
    raw.set_button(Button::Mouse(MouseButton::Left), true);
    raw.add_mouse_delta(3.0, -2.0);
    raw.add_wheel(1.0);
    raw.set_button(Button::Pad(gilrs::Button::South), true);
    // A repeat of a held key is not a new event.
    raw.set_button(Button::Key(KeyCode::KeyW), true);
    let f = raw.raw_frame(7);
    assert_eq!(f.frame, 7);
    let kinds: Vec<(InputKind, &str, InputPhase)> = f
        .events
        .iter()
        .map(|e| (e.kind, e.button.as_str(), e.phase))
        .collect();
    assert_eq!(
        kinds,
        [
            (InputKind::Keyboard, "KeyW", InputPhase::Began),
            (InputKind::MouseMovement, "", InputPhase::Changed),
            (InputKind::MouseButton, "Left", InputPhase::Began),
            (InputKind::MouseMovement, "", InputPhase::Changed),
            (InputKind::MouseWheel, "", InputPhase::Changed),
            (InputKind::Gamepad, "South", InputPhase::Began),
        ]
    );
    assert_eq!(f.events[2].position.x, 10.0);
    assert_eq!(f.events[2].position.y, 20.0);
    assert_eq!(f.events[3].delta.x, 3.0);
    assert_eq!(f.events[4].delta.z, 1.0);
    assert_eq!(f.keys_down, ["KeyW"]);
    assert_eq!(f.mouse_buttons_down, ["Left"]);
    assert_eq!(f.pad_buttons_down, ["South"]);
    assert_eq!(f.mouse_delta.to_array(), [3.0, -2.0]);
    assert!(f.key_down("KeyW") && !f.key_down("KeyS"));
    assert!(f.mouse_button_down("Left") && f.pad_button_down("South"));
    assert!(f.keyboard && f.mouse && !f.gamepad);

    raw.end_frame();
    raw.set_button(Button::Key(KeyCode::KeyW), false);
    let g = raw.raw_frame(8);
    assert_eq!(g.events.len(), 1);
    assert_eq!(g.events[0].phase, InputPhase::Ended);
    assert!(g.keys_down.is_empty());
    assert_eq!(g.mouse_position.to_array(), [10.0, 20.0]);
}

#[test]
fn focus_loss_ends_every_held_key() {
    let mut raw = RawInput::default();
    raw.set_button(Button::Key(KeyCode::KeyA), true);
    raw.set_button(Button::Key(KeyCode::ShiftLeft), true);
    raw.end_frame();
    raw.release_all();
    let f = raw.raw_frame(1);
    assert_eq!(f.events.len(), 2);
    assert!(f.events.iter().all(|e| e.phase == InputPhase::Ended));
    assert!(f.keys_down.is_empty());
}

fn row(frame: u64) -> RawReplayInput {
    RawReplayInput {
        frame,
        ..RawReplayInput::default()
    }
}

#[test]
fn raw_rows_play_back_and_record_the_same() {
    let mut doc = ReplayDoc::new(1, 5);
    doc.raw = vec![
        RawReplayInput {
            key: Some("KeyW".into()),
            down: Some(true),
            ..row(1)
        },
        RawReplayInput {
            mouse_move: Some([4.0, 1.0]),
            ..row(2)
        },
        RawReplayInput {
            mouse_position: Some([100.0, 50.0]),
            ..row(2)
        },
        RawReplayInput {
            mouse_button: Some("Right".into()),
            down: Some(true),
            ..row(3)
        },
        RawReplayInput {
            key: Some("KeyW".into()),
            down: Some(false),
            ..row(3)
        },
        RawReplayInput {
            wheel: Some(-1.0),
            ..row(4)
        },
    ];
    let mut replay = Replay::new(doc.clone(), &Default::default()).unwrap();
    let mut frames = Vec::new();
    let mut recorder = ReplayRecorder::new(1);
    while let Some(actions) = replay.next_frame() {
        frames.push(replay.raw_frame().clone());
        recorder.record_raw(replay.raw_frame());
        recorder.record(&actions);
    }
    assert!(frames[0].events.is_empty());
    assert!(frames[1].key_down("KeyW"));
    assert_eq!(frames[2].mouse_delta.to_array(), [4.0, 1.0]);
    assert_eq!(frames[2].mouse_position.to_array(), [100.0, 50.0]);
    assert!(!frames[3].key_down("KeyW"));
    assert_eq!(frames[3].mouse_buttons_down, ["Right"]);
    assert_eq!(frames[4].events[0].kind, InputKind::MouseWheel);
    // Recording what played back gives a document that plays back the same
    // frames.
    let again = recorder.finish();
    let mut replay2 = Replay::new(again, &Default::default()).unwrap();
    let mut i = 0;
    while replay2.next_frame().is_some() {
        let (a, b) = (&frames[i], replay2.raw_frame());
        assert_eq!(a.keys_down, b.keys_down, "frame {i}");
        assert_eq!(a.mouse_buttons_down, b.mouse_buttons_down, "frame {i}");
        assert_eq!(a.mouse_position, b.mouse_position, "frame {i}");
        assert_eq!(a.mouse_delta, b.mouse_delta, "frame {i}");
        i += 1;
    }
    assert_eq!(i, 5);
}

#[test]
fn bad_raw_rows_are_refused() {
    let mut doc = ReplayDoc::new(1, 3);
    doc.raw = vec![
        RawReplayInput {
            key: Some("NotAKey".into()),
            down: Some(true),
            ..row(0)
        },
        RawReplayInput {
            key: Some("KeyW".into()),
            ..row(1)
        },
        RawReplayInput {
            mouse_button: Some("Thumb".into()),
            down: Some(true),
            ..row(1)
        },
        RawReplayInput {
            wheel: Some(1.0),
            ..row(0)
        },
        RawReplayInput {
            wheel: Some(1.0),
            ..row(5)
        },
    ];
    let problems = Replay::new(doc, &Default::default()).unwrap_err();
    let text: Vec<String> = problems.iter().map(|d| d.message.clone()).collect();
    assert!(
        text.iter().any(|t| t.contains("unknown key `NotAKey`")),
        "{text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains("needs exactly one")),
        "{text:?}"
    );
    assert!(
        text.iter()
            .any(|t| t.contains("unknown mouse button `Thumb`")),
        "{text:?}"
    );
    assert!(text.iter().any(|t| t.contains("frame order")), "{text:?}");
    assert!(text.iter().any(|t| t.contains("past the end")), "{text:?}");
}

#[test]
fn raw_rows_round_trip_as_json() {
    let text = r#"{"version":1,"seed":0,"frames":2,"raw":[{"frame":0,"key":"Space","down":true},{"frame":1,"mouse_move":[1,2]}]}"#;
    let doc: ReplayDoc = serde_json::from_str(text).unwrap();
    assert_eq!(doc.raw.len(), 2);
    let back = serde_json::to_string(&doc).unwrap();
    assert!(
        back.contains(r#""raw":[{"frame":0,"key":"Space","down":true}"#),
        "{back}"
    );
    // A replay without raw rows writes no `raw` key.
    let plain = serde_json::to_string(&ReplayDoc::new(0, 1)).unwrap();
    assert!(!plain.contains("raw"), "{plain}");
}
