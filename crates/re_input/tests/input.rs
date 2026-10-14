//! The `[input]` config, the action map over injected input, and gilrs start-up.

use glam::Vec2;
use re_input::{ActionMap, AxisSource, Button, InputConfig, RawInput, Stick};
use winit::event::MouseButton;
use winit::keyboard::KeyCode;

fn fixture_map() -> ActionMap<String> {
    let text = include_str!("fixtures/real.toml");
    InputConfig::from_real_toml(text, "real.toml")
        .unwrap()
        .to_action_map()
        .unwrap()
}

fn s(a: &str) -> String {
    a.to_owned()
}

#[test]
fn fixture_parses() {
    let text = include_str!("fixtures/real.toml");
    let cfg = InputConfig::from_real_toml(text, "real.toml").unwrap();
    assert_eq!(cfg.buttons["jump"], ["key:Space", "pad:South"]);
    assert_eq!(cfg.axes["move"].len(), 2);
    let map = cfg.to_action_map().unwrap();
    assert_eq!(
        map.button_bindings(&s("jump")),
        [
            Button::Key(KeyCode::Space),
            Button::Pad(gilrs::Button::South)
        ]
    );
    assert_eq!(
        map.axis_sources(&s("move"))[1],
        AxisSource::Stick {
            stick: Stick::Left,
            dead_zone: 0.2
        }
    );
    assert_eq!(
        map.axis_sources(&s("look")),
        [
            AxisSource::MouseDelta { scale: 0.002 },
            AxisSource::Stick {
                stick: Stick::Right,
                dead_zone: re_input::DEFAULT_DEAD_ZONE
            }
        ]
    );
    let layout = map.layout();
    assert_eq!(layout.buttons, ["attack", "jump", "regenerate"]);
    assert_eq!(layout.axes, ["look", "move"]);
}

#[test]
fn missing_input_section_is_empty() {
    let cfg = InputConfig::from_real_toml("[project]\nname = \"x\"\n", "real.toml").unwrap();
    assert_eq!(cfg, InputConfig::default());
}

#[test]
fn unknown_key_name_is_a_diagnostic() {
    let text = "[input.buttons]\njump = [\"key:Spcae\", \"pad:South\"]\nfire = [\"mouse:Laft\"]\n";
    let errs = InputConfig::from_real_toml(text, "real.toml")
        .unwrap()
        .to_action_map()
        .unwrap_err();
    assert_eq!(errs.len(), 2, "every problem is reported: {errs:#?}");
    let jump = errs
        .iter()
        .find(|d| d.message.contains("key:Spcae"))
        .unwrap();
    assert_eq!(jump.code.to_string(), "RE7201");
    assert!(
        jump.message.contains("[input.buttons] jump"),
        "{}",
        jump.message
    );
    assert!(
        jump.hint.as_deref().unwrap().contains("`key:Space`"),
        "{:?}",
        jump.hint
    );
    let fire = errs
        .iter()
        .find(|d| d.message.contains("mouse:Laft"))
        .unwrap();
    assert!(fire.hint.as_deref().unwrap().contains("`mouse:Left`"));
}

#[test]
fn unknown_toml_key_is_located() {
    let text = "[project]\nname = \"x\"\n\n[input]\nbutton = {}\n";
    let err = InputConfig::from_real_toml(text, "real.toml").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7200");
    let rendered = err.render_human();
    assert!(rendered.contains("real.toml:5:"), "{rendered}");
    assert!(err.message.contains("button"), "{}", err.message);
}

#[test]
fn bad_axis_sources_are_diagnostics() {
    let bad = [
        "move = [{ stick = \"middle\" }]",
        "move = [{ stick = \"left\", dead_zone = 1.5 }]",
        "move = [{ mouse_delta = 0.1, stick = \"left\" }]",
        "move = [{ composite = [\"key:KeyW\", \"key:KeyS\", \"key:KeyA\", \"key:KeyD\"], dead_zone = 0.1 }]",
        "move = [{}]",
    ];
    for line in bad {
        let text = format!("[input.axes]\n{line}\n");
        let errs = InputConfig::from_real_toml(&text, "real.toml")
            .unwrap()
            .to_action_map()
            .unwrap_err();
        assert_eq!(errs[0].code.to_string(), "RE7202", "{line}: {errs:#?}");
    }
    let text =
        "[input.buttons]\nmove = [\"key:KeyW\"]\n[input.axes]\nmove = [{ mouse_delta = 1.0 }]\n";
    let errs = InputConfig::from_real_toml(text, "real.toml")
        .unwrap()
        .to_action_map()
        .unwrap_err();
    assert_eq!(errs[0].code.to_string(), "RE7203");
}

#[test]
fn injected_press_is_just_pressed_for_exactly_one_frame() {
    let map = fixture_map();
    let mut raw = RawInput::default();
    let mut pressed_frames = Vec::new();
    for frame in 0..5u64 {
        if frame == 1 {
            raw.set_button(Button::Key(KeyCode::Space), true);
        }
        let f = map.frame(&raw, frame);
        if f.pressed("jump") {
            pressed_frames.push(frame);
        }
        if frame >= 1 {
            assert!(f.is_down("jump"));
        }
        raw.end_frame();
    }
    assert_eq!(pressed_frames, [1]);
    raw.set_button(Button::Key(KeyCode::Space), false);
    let f = map.frame(&raw, 5);
    assert!(f.released("jump") && !f.is_down("jump") && !f.pressed("jump"));
    raw.end_frame();
    assert!(!map.frame(&raw, 6).released("jump"));
}

#[test]
fn button_edges_across_multiple_bindings() {
    let m = fixture_map();
    let attack = s("attack");
    let mut raw = RawInput::default();
    raw.set_button(Button::Key(KeyCode::KeyF), true);
    assert!(m.just_pressed(&raw, &attack) && m.held(&raw, &attack));
    raw.end_frame();
    assert!(!m.just_pressed(&raw, &attack) && m.held(&raw, &attack));

    // A second binding pressed while the first is held is not a new press.
    raw.set_button(Button::Mouse(MouseButton::Left), true);
    assert!(!m.just_pressed(&raw, &attack));
    raw.end_frame();
    // Releasing one of two held bindings does not release the action.
    raw.set_button(Button::Key(KeyCode::KeyF), false);
    assert!(!m.just_released(&raw, &attack) && m.held(&raw, &attack));
    raw.end_frame();
    raw.set_button(Button::Mouse(MouseButton::Left), false);
    assert!(m.just_released(&raw, &attack) && !m.held(&raw, &attack));
}

#[test]
fn tap_within_one_frame_is_still_seen() {
    let m = fixture_map();
    let mut raw = RawInput::default();
    raw.set_button(Button::Pad(gilrs::Button::South), true);
    raw.set_button(Button::Pad(gilrs::Button::South), false);
    let f = m.frame(&raw, 0);
    assert!(f.pressed("jump") && f.released("jump") && !f.is_down("jump"));
}

#[test]
fn axes_combine_keys_sticks_and_mouse() {
    let m = fixture_map();
    let mut raw = RawInput::default();
    raw.set_button(Button::Key(KeyCode::KeyW), true);
    raw.set_button(Button::Key(KeyCode::KeyD), true);
    let v = m.frame(&raw, 0).axis("move");
    assert!(
        (v - Vec2::splat(std::f32::consts::FRAC_1_SQRT_2)).length() < 1e-6,
        "diagonal normalised: {v}"
    );
    raw.release_all();

    raw.set_pad_axis(gilrs::Axis::LeftStickX, 0.15);
    assert_eq!(
        m.frame(&raw, 1).axis("move"),
        Vec2::ZERO,
        "inside dead zone"
    );
    raw.set_pad_axis(gilrs::Axis::LeftStickX, 0.6);
    assert!(
        (m.frame(&raw, 2).axis("move").x - 0.5).abs() < 1e-6,
        "rescaled from the dead-zone edge"
    );

    raw.add_mouse_delta(100.0, 50.0);
    assert_eq!(m.frame(&raw, 3).axis("look"), Vec2::new(0.2, -0.1));
    raw.end_frame();
    assert_eq!(
        m.frame(&raw, 4).axis("look"),
        Vec2::ZERO,
        "mouse motion is per frame"
    );
}

#[test]
fn every_bound_action_is_listed_even_idle() {
    let f = fixture_map().frame(&RawInput::default(), 7);
    assert_eq!(f.frame, 7);
    assert_eq!(
        f.buttons.keys().collect::<Vec<_>>(),
        ["attack", "jump", "regenerate"]
    );
    assert_eq!(f.axes.keys().collect::<Vec<_>>(), ["look", "move"]);
}

#[test]
fn gamepads_start_without_devices() {
    let mut pads = re_input::Gamepads::new();
    let mut raw = RawInput::default();
    pads.poll(&mut raw);
    println!("gamepads connected: {}", pads.connected());
}
