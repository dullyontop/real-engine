//! WP15: Sound as scripts see it, in a client VM.

use re_compat::sound::{RollOffMode, SoundEventKind, SoundSettings, roll_off};
use re_types::audio::AudioCmd;

use re_compat::SignalBehavior;
use wp15::{Place, place, place_with};

mod wp15;

fn sound_place() -> Place {
    configured(place())
}

fn configured(mut p: Place) -> Place {
    p.lua(|lua| {
        re_compat::sound::configure(
            lua,
            SoundSettings {
                assets: std::collections::BTreeMap::from([
                    ("1".to_string(), "assets/two.wav".to_string()),
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

#[test]
fn length_and_loaded_follow_the_id_before_play() {
    let mut p = sound_place();
    p.run(
        r#"
        local s = Instance.new("Sound", workspace)
        s.Loaded:Connect(function(id) print("loaded " .. id) end)
        s.SoundId = "rbxassetid://1"
        assert(not s.IsLoaded and s.TimeLength == 0)
        "#,
    );
    p.frame(0.0);
    p.run("local s = workspace.Sound assert(s.IsLoaded and s.TimeLength == 2 and not s.IsPlaying)");
    assert_eq!(p.out(), vec!["loaded rbxassetid://1"]);
    assert!(commands(&mut p).is_empty(), "nothing plays");
    p.run("workspace.Sound.SoundId = '3'");
    p.frame(0.0);
    p.run("assert(workspace.Sound.TimeLength == 0.5)");
}

#[test]
fn an_immediate_ended_handler_can_play_again() {
    let mut p = configured(place_with(SignalBehavior::Immediate));
    p.run(
        r#"
        local s = Instance.new("Sound", workspace)
        s.SoundId = "3"
        S.n = 0
        s.Ended:Connect(function()
            S.n += 1
            if S.n < 2 then s:Play() end
        end)
        s:Play()
        "#,
    );
    commands(&mut p);
    p.frame(0.6);
    p.run("local s = workspace.Sound assert(S.n == 1 and s.IsPlaying and s.Playing)");
    assert_eq!(
        commands(&mut p),
        vec!["stop", "start assets/half.wav loop=false 3d=false vol=0.5"]
    );
    p.frame(0.6);
    p.run("local s = workspace.Sound assert(S.n == 2 and not s.IsPlaying)");
    assert_eq!(commands(&mut p), vec!["stop"]);
}

// 3D sounds need a part or attachment parent; those classes come with WP13
// and WP14, and the 3D tests come with them.

#[test]
fn changing_the_id_while_playing_restarts_it() {
    let mut p = sound_place();
    p.run("local s = Instance.new('Sound', workspace) s.SoundId = '1' s.Looped = true s:Play()");
    commands(&mut p);
    p.run("workspace.Sound.SoundId = '3' workspace.Sound.PlaybackSpeed = 3");
    p.frame(0.0);
    let c = commands(&mut p);
    assert!(
        c.contains(&"start assets/half.wav loop=true 3d=false vol=0.5".to_string()),
        "{c:?}"
    );
    assert!(c.contains(&"rate 3".to_string()), "{c:?}");
    p.run("assert(workspace.Sound.TimeLength == 0.5 and workspace.Sound.IsPlaying)");
}

mod config {
    use re_compat::CompatConfig;
    use re_compat::scene::DEFAULT_EFFECT_BUDGET;

    const HEAD: &str = "[compat]\napi = \"roblox\"\n";

    fn load(extra: &str) -> Result<CompatConfig, String> {
        CompatConfig::from_project_toml(&format!("{HEAD}{extra}"))
            .map(Option::unwrap)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn assets_and_budget_round_trip() {
        let cfg = load(
            "effect_budget = 500\n\n[compat.assets]\n\"rbxassetid://12\" = \"assets/hit.wav\"\n\"7\" = \"sfx/./wind.ogg\"\n",
        )
        .unwrap();
        assert_eq!(cfg.effect_budget(), 500);
        let keys: Vec<&str> = cfg.assets.keys().map(String::as_str).collect();
        assert_eq!(keys, ["12", "7"], "both forms normalise to the bare id");
        let text = cfg.to_toml();
        assert_eq!(
            text,
            "[compat]\napi = \"roblox\"\neffect_budget = 500\n\n[compat.assets]\n\"12\" = \"assets/hit.wav\"\n\"7\" = \"sfx/./wind.ogg\"\n"
        );
        assert_eq!(
            CompatConfig::from_project_toml(&text).unwrap().unwrap(),
            cfg
        );

        let s = re_compat::sound::SoundSettings::from_config(&cfg, None);
        assert_eq!(s.resolve("rbxassetid://12"), Some("assets/hit.wav"));
        assert_eq!(s.resolve("12"), Some("assets/hit.wav"));
        assert_eq!(s.resolve("rbxassetid://7"), Some("sfx/./wind.ogg"));
        assert_eq!(s.resolve("8"), None);
    }

    #[test]
    fn absent_fields_are_not_written() {
        let cfg = load("").unwrap();
        assert!(cfg.assets.is_empty());
        assert_eq!(cfg.effect_budget, None);
        assert_eq!(cfg.effect_budget(), DEFAULT_EFFECT_BUDGET);
        assert_eq!(cfg.to_toml(), HEAD);
    }

    #[test]
    fn refused_values() {
        let e = load("effect_budget = 0\n").unwrap_err();
        assert!(e.contains("effect_budget"), "{e}");
        let e = load("[compat.assets]\n\"rbxassetid://5\" = \"a.wav\"\n\"5\" = \"b.wav\"\n")
            .unwrap_err();
        assert!(e.contains("same id"), "{e}");
        for path in [
            "/etc/a.wav",
            "../a.wav",
            "assets/../../a.wav",
            "a\\\\..\\\\b.wav",
            "",
        ] {
            let e = load(&format!("[compat.assets]\n\"5\" = \"{path}\"\n")).unwrap_err();
            assert!(e.contains("inside the project"), "{path}: {e}");
        }
    }
}
