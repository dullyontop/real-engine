//! WP15: Sound as scripts see it, in a client VM.

use std::collections::HashMap;

use re_compat::sound::{RollOffMode, SoundEventKind, SoundSettings, roll_off};
use re_types::audio::AudioCmd;

use wp15::{Place, place};

mod wp15;

fn sound_place() -> Place {
    let mut p = place();
    p.lua(|lua| {
        re_compat::sound::configure(
            lua,
            SoundSettings {
                assets: HashMap::from([
                    ("rbxassetid://1".to_string(), "assets/two.wav".to_string()),
                    ("3".to_string(), "assets/half.wav".to_string()),
                ]),
                clip_seconds: Some(std::sync::Arc::new(|clip| match clip {
                    "assets/two.wav" => Some(2.0),
                    "assets/half.wav" => Some(0.5),
                    _ => None,
                })),
            },
        )
    });
    p
}

fn commands(p: &mut Place) -> Vec<String> {
    p.lua(re_compat::sound::take_commands)
        .into_iter()
        .map(|c| match c {
            AudioCmd::StartSource {
                clip,
                looping,
                spatial,
                volume,
                ..
            } => format!("start {clip} loop={looping} 3d={spatial} vol={volume}"),
            AudioCmd::StopSource { .. } => "stop".into(),
            AudioCmd::SeekSource { seconds, .. } => format!("seek {seconds}"),
            AudioCmd::SetSourceRate { rate, .. } => format!("rate {rate}"),
            AudioCmd::SetSourceVolume { volume, .. } => format!("volume {volume}"),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn plays_to_its_end_and_fires_ended() {
    let mut p = sound_place();
    p.run(
        r#"
        local s = Instance.new("Sound", workspace)
        s.SoundId = "rbxassetid://1"
        s.Volume = 0.8
        s.Played:Connect(function(id) print("played " .. id) end)
        s.Ended:Connect(function(id) print("ended " .. id) end)
        s:Play()
        assert(s.IsPlaying and s.Playing and s.TimeLength == 2 and s.IsLoaded)
        "#,
    );
    assert_eq!(
        commands(&mut p),
        vec!["start assets/two.wav loop=false 3d=false vol=0.8"]
    );
    assert_eq!(p.out(), vec!["played rbxassetid://1"]);
    p.frame(1.5);
    p.run(
        "local s = workspace.Sound assert(s.IsPlaying and math.abs(s.TimePosition - 1.5) < 1e-9)",
    );
    p.frame(0.6);
    p.run("local s = workspace.Sound assert(not s.IsPlaying and not s.Playing and s.TimePosition == 0)");
    assert_eq!(p.out(), vec!["ended rbxassetid://1"]);
    assert_eq!(commands(&mut p), vec!["stop"]);
    let events: Vec<SoundEventKind> = p
        .lua(re_compat::sound::take_events)
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(events, vec![SoundEventKind::Played, SoundEventKind::Ended]);
}

#[test]
fn loops_speed_pause_and_seek() {
    let mut p = sound_place();
    p.run(
        r#"
        local s = Instance.new("Sound", workspace)
        s.SoundId = "3"
        s.Looped = true
        s.PlaybackSpeed = 2
        s.DidLoop:Connect(function(id, n) print("loop " .. n) end)
        s:Play()
        "#,
    );
    assert_eq!(
        commands(&mut p),
        vec!["start assets/half.wav loop=true 3d=false vol=0.5", "rate 2"]
    );
    // At double speed the half-second clip loops every quarter second.
    p.frame(0.3);
    p.frame(0.3);
    assert_eq!(p.out(), vec!["loop 1", "loop 2"]);
    p.run("local s = workspace.Sound s:Pause() assert(s.IsPaused and not s.IsPlaying)");
    let at: f64 = p.lua(|lua| {
        lua.load("return workspace.Sound.TimePosition")
            .eval()
            .unwrap()
    });
    assert!((at - 0.2).abs() < 1e-9, "{at}");
    p.frame(5.0);
    p.run("local s = workspace.Sound s:Resume() s.TimePosition = 0.1");
    p.frame(0.0);
    let c = commands(&mut p);
    assert_eq!(c[0], "stop");
    assert!(c.contains(&"seek 0.1".to_string()), "{c:?}");
    p.run("workspace.Sound.Playing = false");
    p.frame(0.1);
    p.run("local s = workspace.Sound assert(not s.IsPlaying and math.abs(s.TimePosition - 0.1) < 1e-9)");
    // A local: Luau may resolve a chain of globals like workspace.Sound.X
    // when the chunk loads, before the call before it has run.
    p.run("local s = workspace.Sound s:Stop() assert(s.TimePosition == 0 and not s.IsPaused)");
}

#[test]
fn writing_playing_starts_the_sound() {
    let mut p = sound_place();
    p.run(
        "local s = Instance.new('Sound', workspace) s.SoundId = 'rbxassetid://1' s.Playing = true",
    );
    p.frame(0.0);
    assert_eq!(
        commands(&mut p),
        vec!["start assets/two.wav loop=false 3d=false vol=0.5"]
    );
    p.run("assert(workspace.Sound.IsPlaying)");
    p.run("workspace.Sound:Destroy()");
    p.frame(0.1);
    assert_eq!(commands(&mut p), vec!["stop"]);
}

#[test]
fn an_unknown_id_is_silent_warns_once_and_ends() {
    let mut p = sound_place();
    p.run(
        r#"
        for i = 1, 2 do
            local s = Instance.new("Sound", workspace)
            s.SoundId = "rbxassetid://999"
            s.Ended:Connect(function() print("ended " .. i) end)
            s:Play()
        end
        "#,
    );
    let out = p.out();
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(
        out[0].starts_with("W [RE9350] sound rbxassetid://999"),
        "{out:?}"
    );
    assert!(commands(&mut p).is_empty());
    p.frame(1.0 / 60.0);
    let mut ended = p.out();
    ended.sort();
    assert_eq!(ended, vec!["ended 1", "ended 2"]);
    p.run("assert(not workspace.Sound.IsLoaded and workspace.Sound.TimeLength == 0)");
}

#[test]
fn roll_off_curves() {
    let near = |a: f32, b: f32| (a - b).abs() < 1e-6;
    for mode in [
        RollOffMode::Inverse,
        RollOffMode::Linear,
        RollOffMode::LinearSquare,
        RollOffMode::InverseTapered,
    ] {
        assert_eq!(roll_off(mode, 10.0, 100.0, 5.0), 1.0, "{mode:?}");
        assert_eq!(roll_off(mode, 10.0, 100.0, 100.0), 0.0, "{mode:?}");
        assert_eq!(roll_off(mode, 10.0, 100.0, 1e9), 0.0, "{mode:?}");
    }
    assert!(near(roll_off(RollOffMode::Inverse, 10.0, 100.0, 20.0), 0.5));
    assert!(near(
        roll_off(RollOffMode::Inverse, 10.0, 100.0, 40.0),
        0.25
    ));
    assert!(near(roll_off(RollOffMode::Linear, 10.0, 100.0, 55.0), 0.5));
    assert!(near(
        roll_off(RollOffMode::LinearSquare, 10.0, 100.0, 55.0),
        0.25
    ));
    // Near the minimum distance the tapered curve is the inverse one; near
    // the maximum it is the linear square one.
    assert!(near(
        roll_off(RollOffMode::InverseTapered, 10.0, 100.0, 12.5),
        0.8
    ));
    let far = roll_off(RollOffMode::InverseTapered, 10.0, 100.0, 90.0);
    assert!(near(far, (10.0f32 / 90.0).powi(2)), "{far}");
}
