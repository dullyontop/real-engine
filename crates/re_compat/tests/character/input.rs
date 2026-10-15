//! UserInputService, InputObject and ContextActionService, driven by raw
//! input frames from the host.

use re_script::ContextKind;
use re_types::glam::{Vec2, Vec3};
use re_types::input::{InputEvent, InputPhase, MouseLock, RawInputFrame};

use crate::place::{Place, place};

fn ev(phase: InputPhase, ty: &str, key: &str) -> InputEvent {
    InputEvent {
        phase,
        input_type: ty.into(),
        key: key.into(),
        position: Vec3::ZERO,
        delta: Vec3::ZERO,
    }
}

/// Feeds one frame of raw input and runs the input step, then lets deferred
/// handlers run.
fn feed(p: &mut Place, frame: RawInputFrame) {
    p.services.raw = Some(frame);
    p.with(re_compat::input::frame);
    p.step(1.0 / 60.0);
}

fn keys(held: &[&str], events: Vec<InputEvent>) -> RawInputFrame {
    RawInputFrame {
        events,
        keys_down: held.iter().map(|s| s.to_string()).collect(),
        keyboard: true,
        mouse: true,
        ..RawInputFrame::default()
    }
}

#[test]
fn input_began_and_ended_reuse_one_object_per_key() {
    let mut p = place(ContextKind::Client);
    p.run(
        r#"
        local UIS = game:GetService("UserInputService")
        local seen = {}
        UIS.InputBegan:Connect(function(input, processed)
            print("began", input.KeyCode.Name, input.UserInputType.Name, input.UserInputState.Name, processed)
            seen.began = input
        end)
        UIS.InputEnded:Connect(function(input)
            print("ended", input.KeyCode.Name, input.UserInputState.Name, input == seen.began)
        end)
        "#,
    );
    feed(
        &mut p,
        keys(&["W"], vec![ev(InputPhase::Began, "Keyboard", "W")]),
    );
    p.run(r#"print(game:GetService("UserInputService"):IsKeyDown(Enum.KeyCode.W), game:GetService("UserInputService"):IsKeyDown(Enum.KeyCode.S))"#);
    feed(
        &mut p,
        keys(&[], vec![ev(InputPhase::Ended, "Keyboard", "W")]),
    );
    p.run(r#"print(game:GetService("UserInputService"):IsKeyDown(Enum.KeyCode.W))"#);
    assert_eq!(
        p.out(),
        [
            "began W Keyboard Begin false",
            "true false",
            "ended W End true",
            "false"
        ]
    );
}

#[test]
fn mouse_location_delta_buttons_and_last_input_type() {
    let mut p = place(ContextKind::Client);
    p.run(
        r#"
        local UIS = game:GetService("UserInputService")
        UIS.MouseDeltaSensitivity = 2
        UIS.LastInputTypeChanged:Connect(function(t) print("last", t.Name) end)
        UIS.InputChanged:Connect(function(input)
            print("changed", input.UserInputType.Name, input.Position.X, input.Delta.Y)
        end)
        "#,
    );
    let mut moved = ev(InputPhase::Changed, "MouseMovement", "Unknown");
    moved.position = Vec3::new(40.0, 30.0, 0.0);
    moved.delta = Vec3::new(4.0, -3.0, 0.0);
    feed(
        &mut p,
        RawInputFrame {
            events: vec![moved, ev(InputPhase::Began, "MouseButton1", "Unknown")],
            mouse_buttons_down: vec![1],
            mouse_position: Vec2::new(40.0, 30.0),
            mouse_delta: Vec2::new(4.0, -3.0),
            keyboard: true,
            mouse: true,
            ..RawInputFrame::default()
        },
    );
    p.run(
        r#"
        local UIS = game:GetService("UserInputService")
        local at, d = UIS:GetMouseLocation(), UIS:GetMouseDelta()
        print(at.X, at.Y, d.X, d.Y, UIS:IsMouseButtonPressed(Enum.UserInputType.MouseButton1),
              UIS:IsMouseButtonPressed(Enum.UserInputType.MouseButton2), UIS:GetLastInputType().Name,
              #UIS:GetMouseButtonsPressed())
        "#,
    );
    assert_eq!(
        p.out(),
        [
            "last MouseMovement",
            "changed MouseMovement 40 -3",
            "last MouseButton1",
            "40 30 8 -6 true false MouseButton1 1"
        ]
    );
}

#[test]
fn device_flags_follow_the_host_and_read_only() {
    let mut p = place(ContextKind::Client);
    p.run(r#"local _ = game:GetService("UserInputService")"#);
    feed(
        &mut p,
        RawInputFrame {
            keyboard: true,
            mouse: false,
            gamepad: true,
            ..RawInputFrame::default()
        },
    );
    p.run(
        r#"
        local UIS = game:GetService("UserInputService")
        print(UIS.KeyboardEnabled, UIS.MouseEnabled, UIS.GamepadEnabled, UIS.TouchEnabled,
              UIS:GetGamepadConnected(Enum.UserInputType.Gamepad1), #UIS:GetConnectedGamepads())
        print(pcall(function() UIS.TouchEnabled = true end))
        print(UIS:GetFocusedTextBox())
        "#,
    );
    let out = p.out();
    assert_eq!(out[0], "true false true false true 1");
    assert!(
        out[1].starts_with("false") && out[1].contains("RE9005"),
        "{out:?}"
    );
    assert_eq!(out[2], "nil");
}

#[test]
fn mouse_behavior_is_sent_to_the_host() {
    let mut p = place(ContextKind::Client);
    p.run(
        r#"
        local UIS = game:GetService("UserInputService")
        UIS.MouseBehavior = Enum.MouseBehavior.LockCenter
        UIS.MouseIconEnabled = false
        "#,
    );
    feed(&mut p, RawInputFrame::default());
    assert_eq!(p.services.mouse, Some((MouseLock::LockCenter, false)));
}

#[test]
fn the_server_sees_no_input() {
    let mut p = place(ContextKind::Server);
    p.run(
        r#"
        game:GetService("UserInputService").InputBegan:Connect(function() print("no") end)
        "#,
    );
    feed(
        &mut p,
        keys(&["W"], vec![ev(InputPhase::Began, "Keyboard", "W")]),
    );
    p.run(r#"print(game:GetService("UserInputService"):IsKeyDown(Enum.KeyCode.W))"#);
    assert_eq!(p.out(), ["false"]);
}

#[test]
fn jump_request_and_modifier_keys() {
    let mut p = place(ContextKind::Client);
    p.run(
        r#"
        local UIS = game:GetService("UserInputService")
        UIS.JumpRequest:Connect(function() print("jump") end)
        UIS.InputBegan:Connect(function(input)
            print(input.KeyCode.Name, input:IsModifierKeyDown(Enum.ModifierKey.Shift))
        end)
        "#,
    );
    feed(
        &mut p,
        keys(
            &["LeftShift", "Space"],
            vec![
                ev(InputPhase::Began, "Keyboard", "LeftShift"),
                ev(InputPhase::Began, "Keyboard", "Space"),
            ],
        ),
    );
    let out = p.out();
    assert!(out.contains(&"jump".to_string()), "{out:?}");
    assert!(out.contains(&"Space true".to_string()), "{out:?}");
}

#[test]
fn actions_go_by_priority_and_sink_or_pass() {
    let mut p = place(ContextKind::Client);
    p.run(
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("low", function(name, state, input)
            print(name, state.Name, input.KeyCode.Name)
        end, false, Enum.KeyCode.E)
        CAS:BindActionAtPriority("high", function(name, state)
            print(name, state.Name)
            return Enum.ContextActionResult.Pass
        end, false, 3000, Enum.KeyCode.E)
        CAS:BindActionAtPriority("top", function(name, state)
            print(name, state.Name)
            if state == Enum.UserInputState.End then
                return Enum.ContextActionResult.Sink
            end
            return Enum.ContextActionResult.Pass
        end, false, 4000, Enum.KeyCode.E, Enum.UserInputType.MouseButton2)
        CAS:SetTitle("low", "Use")
        local info = CAS:GetBoundActionInfo("high")
        print(info.priorityLevel, #info.inputTypes, CAS:GetBoundActionInfo("low").title)
        "#,
    );
    feed(
        &mut p,
        keys(&["E"], vec![ev(InputPhase::Began, "Keyboard", "E")]),
    );
    feed(
        &mut p,
        keys(&[], vec![ev(InputPhase::Ended, "Keyboard", "E")]),
    );
    assert_eq!(
        p.out(),
        [
            "3000 1 Use",
            "top Begin",
            "high Begin",
            "low Begin E",
            "top End",
        ]
    );
}

#[test]
fn unbinding_a_held_action_cancels_it() {
    let mut p = place(ContextKind::Client);
    p.run(
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("run", function(name, state) print(name, state.Name) end, false, Enum.KeyCode.LeftShift)
        "#,
    );
    feed(
        &mut p,
        keys(
            &["LeftShift"],
            vec![ev(InputPhase::Began, "Keyboard", "LeftShift")],
        ),
    );
    p.run(r#"game:GetService("ContextActionService"):UnbindAction("run")"#);
    feed(
        &mut p,
        keys(&[], vec![ev(InputPhase::Ended, "Keyboard", "LeftShift")]),
    );
    assert_eq!(p.out(), ["run Begin", "run Cancel"]);
}

#[test]
fn a_bad_input_argument_is_an_error() {
    let mut p = place(ContextKind::Client);
    p.run(
        r#"
        local ok, err = pcall(function()
            game:GetService("ContextActionService"):BindAction("x", function() end, false, "E")
        end)
        print(ok, string.find(tostring(err), "Enum.KeyCode") ~= nil)
        "#,
    );
    assert_eq!(p.out(), ["false true"]);
}
