//! A recorded key sequence replayed twice gives the same character path and
//! the same world state hash.

use re_input::Replay;
use re_script::ContextKind;
use re_types::replay::{RawReplayInput, ReplayDoc};

use crate::place::character_place;

fn key(frame: u64, name: &str, down: bool) -> RawReplayInput {
    RawReplayInput {
        frame,
        key: Some(name.into()),
        down: Some(down),
        ..RawReplayInput::default()
    }
}

fn recording() -> ReplayDoc {
    let mut doc = ReplayDoc::new(7, 150);
    doc.raw = vec![
        key(10, "KeyW", true),
        key(40, "Space", true),
        key(42, "Space", false),
        key(60, "KeyD", true),
        key(90, "KeyW", false),
        key(120, "KeyD", false),
    ];
    doc
}

/// A small control script (the default one needs the player runtime):
/// W and D walk, Space jumps.
const CONTROLS: &str = "
local UIS = game:GetService('UserInputService')
local h = workspace.Dummy.Humanoid
local held = {}
local function update()
    local v = Vector3.zero
    if held.W then v += Vector3.new(0, 0, -1) end
    if held.D then v += Vector3.new(1, 0, 0) end
    h:Move(v)
end
UIS.InputBegan:Connect(function(input)
    held[input.KeyCode.Name] = true
    if input.KeyCode == Enum.KeyCode.Space then h.Jump = true end
    update()
end)
UIS.InputEnded:Connect(function(input)
    held[input.KeyCode.Name] = nil
    update()
end)
";

fn run() -> (Vec<[f32; 3]>, String) {
    let mut p = character_place(ContextKind::Client);
    p.character("R15", "Dummy", 0.0, 0.0);
    p.run(CONTROLS);
    let mut replay = Replay::new(recording(), &Default::default()).unwrap();
    let hrp = p.find(&["Workspace", "Dummy", "HumanoidRootPart"]);
    let mut path = Vec::new();
    while replay.next_frame().is_some() {
        p.services.raw = Some(replay.raw_frame().clone());
        p.with(re_compat::input::frame);
        p.frame(1.0 / 60.0);
        path.push(p.cframe(hrp, "CFrame").p);
    }
    (path, p.world.state_hash().to_hex().to_string())
}

#[test]
fn a_replayed_key_sequence_gives_the_same_path_twice() {
    let (a, ha) = run();
    let (b, hb) = run();
    assert_eq!(a.len(), 150);
    assert_eq!(a, b, "the paths differ");
    assert_eq!(ha, hb, "the world states differ");
    // The keys did move it: forward, up in a jump, then right.
    let end = a[149];
    assert!(end[2] < -5.0 && end[0] > 5.0, "ended at {end:?}");
    let peak = a.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
    assert!(peak > 6.0, "never jumped (peak {peak})");
}
