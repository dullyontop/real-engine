//! Humanoid movement, jumping, health and state, on a flat floor.

use re_script::ContextKind;

use crate::place::{Place, character_place};

fn root_y(p: &Place, model: &str) -> f32 {
    let hrp = p.find(&["Workspace", model, "HumanoidRootPart"]);
    p.cframe(hrp, "CFrame").p[1]
}

fn root_pos(p: &Place, model: &str) -> [f32; 3] {
    let hrp = p.find(&["Workspace", model, "HumanoidRootPart"]);
    p.cframe(hrp, "CFrame").p
}

fn settled(rig: &str) -> Place {
    let mut p = character_place(ContextKind::Server);
    p.character(rig, "Dummy", 0.0, 0.0);
    p.frames(30);
    p
}

#[test]
fn both_rigs_stand_at_their_root_height() {
    for (rig, height) in [("R6", 3.0), ("R15", 3.0)] {
        let p = settled(rig);
        let y = root_y(&p, "Dummy");
        assert!(
            (y - height).abs() <= 0.1,
            "{rig}: root at {y}, expected {height}"
        );
    }
}

#[test]
fn a_walking_character_moves_at_walk_speed() {
    for (rig, speed) in [("R15", 16.0), ("R6", 16.0), ("R15", 30.0)] {
        let mut p = settled(rig);
        p.run(&format!(
            "local h = workspace.Dummy.Humanoid
             h.WalkSpeed = {speed}
             h:Move(Vector3.new(0, 0, -1))"
        ));
        p.frames(30);
        let a = root_pos(&p, "Dummy");
        p.frames(60);
        let b = root_pos(&p, "Dummy");
        let dist = ((b[0] - a[0]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
        assert!(
            (dist - speed).abs() <= 0.5,
            "{rig} at WalkSpeed {speed}: {dist} studs in 1 s"
        );
        assert!(b[2] < a[2], "walks toward -Z");
        p.run("local d = workspace.Dummy.Humanoid.MoveDirection print(d.X, d.Y, d.Z)");
        assert_eq!(p.out(), ["0 0 -1"]);
    }
}

fn peak_rise(p: &mut Place) -> f32 {
    let start = root_y(p, "Dummy");
    let mut peak = start;
    for _ in 0..120 {
        p.frame(1.0 / 60.0);
        peak = peak.max(root_y(p, "Dummy"));
    }
    peak - start
}

#[test]
fn jumps_rise_as_the_gravity_formula_says() {
    // JumpPower: rises v² / 2g.
    let mut p = settled("R15");
    p.run("workspace.Dummy.Humanoid.Jump = true");
    let rise = peak_rise(&mut p);
    let want = 50.0f32 * 50.0 / (2.0 * 196.2);
    assert!(
        (rise - want).abs() <= want * 0.05,
        "JumpPower 50: rose {rise}, expected {want}"
    );
    // JumpHeight, with UseJumpPower off.
    let mut p = settled("R6");
    p.run(
        "local h = workspace.Dummy.Humanoid
         h.UseJumpPower = false
         h.JumpHeight = 10
         h.Jump = true",
    );
    let rise = peak_rise(&mut p);
    assert!((rise - 10.0).abs() <= 0.5, "JumpHeight 10: rose {rise}");
    // The jump consumed the request, and the character is back on the floor.
    p.run("print(workspace.Dummy.Humanoid.Jump, workspace.Dummy.Humanoid:GetState())");
    assert_eq!(p.out(), ["false Enum.HumanoidStateType.Running"]);
}

#[test]
fn a_jump_goes_through_jumping_freefall_and_landed() {
    let mut p = settled("R15");
    p.run(
        "local h = workspace.Dummy.Humanoid
         h.StateChanged:Connect(function(old, new) print(old.Name, '->', new.Name) end)
         h.Jumping:Connect(function(on) print('Jumping', on) end)
         h.FreeFalling:Connect(function(on) print('FreeFalling', on) end)
         h.Jump = true",
    );
    p.frames(90);
    assert_eq!(
        p.out(),
        [
            "Running -> Jumping",
            "Jumping true",
            "Jumping -> Freefall",
            "Jumping false",
            "FreeFalling true",
            "Freefall -> Landed",
            "FreeFalling false",
            "Landed -> Running",
        ]
    );
}

#[test]
fn disabled_states_are_never_entered() {
    let mut p = settled("R15");
    p.run(
        "local h = workspace.Dummy.Humanoid
         h.StateEnabledChanged:Connect(function(s, on) print(s.Name, on) end)
         h:SetStateEnabled(Enum.HumanoidStateType.Jumping, false)
         print(h:GetStateEnabled(Enum.HumanoidStateType.Jumping))
         h.Jump = true",
    );
    let y = root_y(&p, "Dummy");
    p.frames(30);
    assert!((root_y(&p, "Dummy") - y).abs() < 0.05, "did not jump");
    assert_eq!(p.out(), ["false", "Jumping false"]);
}

#[test]
fn move_to_walks_there_and_reports_arrival() {
    let mut p = settled("R15");
    p.run(
        "local h = workspace.Dummy.Humanoid
         h.MoveToFinished:Connect(function(ok) print('finished', ok) end)
         h:MoveTo(Vector3.new(8, 0, 0))",
    );
    p.frames(60);
    let pos = root_pos(&p, "Dummy");
    assert!((pos[0] - 8.0).abs() <= 1.0, "stopped at {pos:?}");
    assert_eq!(p.out(), ["finished true"]);
    // A goal it cannot reach times out after 8 s.
    p.run(
        "local h = workspace.Dummy.Humanoid
         h.WalkSpeed = 0
         h:MoveTo(Vector3.new(100, 0, 0))",
    );
    p.frames(60 * 8 + 5);
    assert_eq!(p.out(), ["finished false"]);
}

#[test]
fn damage_kills_and_breaks_the_joints() {
    let mut p = settled("R6");
    p.run(
        "local h = workspace.Dummy.Humanoid
         h.HealthChanged:Connect(function(v) print('health', v) end)
         h.Died:Connect(function() print('died', h:GetState().Name) end)
         h:TakeDamage(30)
         h:TakeDamage(80)",
    );
    p.frames(2);
    assert_eq!(p.out(), ["health 70", "health 0", "died Dead"]);
    p.run(
        "local n = 0
         for _, d in workspace.Dummy:GetDescendants() do
             if d:IsA('Motor6D') then n += 1 end
         end
         print(n)",
    );
    assert_eq!(p.out(), ["0"]);
}

#[test]
fn a_force_field_blocks_damage_and_health_is_capped() {
    let mut p = settled("R15");
    p.run(
        "local h = workspace.Dummy.Humanoid
         Instance.new('ForceField').Parent = workspace.Dummy
         h:TakeDamage(50)
         h.Health = 500
         print(h.Health)",
    );
    p.frames(1);
    p.run("print(workspace.Dummy.Humanoid.Health)");
    assert_eq!(p.out(), ["500", "100"]);
}

#[test]
fn losing_the_neck_kills_when_the_neck_is_required() {
    let mut p = settled("R15");
    p.run(
        "local h = workspace.Dummy.Humanoid
         h.BreakJointsOnDeath = false
         h.Died:Connect(function() print('died') end)
         workspace.Dummy.Head.Neck:Destroy()",
    );
    p.frames(2);
    assert_eq!(p.out(), ["died"]);
}

#[test]
fn rig_queries_and_descriptions() {
    let mut p = settled("R6");
    p.run(
        "local h = workspace.Dummy.Humanoid
         print(h:GetLimb(workspace.Dummy['Left Arm']).Name, h.RigType.Name, h.RootPart.Name)
         local d = Instance.new('HumanoidDescription')
         d.HeightScale = 2
         d.TorsoColor = Color3.new(1, 0, 0)
         h:ApplyDescription(d)
         local s = workspace.Dummy.Torso.Size
         print(s.X, s.Y, s.Z, workspace.Dummy.Torso.Color)
         print(h:GetAppliedDescription().HeightScale)",
    );
    assert_eq!(
        p.out(),
        ["LeftArm R6 HumanoidRootPart", "2 4 1 1, 0, 0", "2"]
    );
    // The capsule is rebuilt for the taller rig: the root stands twice as high.
    p.frames(30);
    let y = root_y(&p, "Dummy");
    assert!((y - 6.0).abs() <= 0.1, "root at {y}");
}

#[test]
fn change_state_jumping_in_the_air_does_not_queue_a_jump() {
    let mut p = settled("R15");
    p.run("workspace.Dummy.Humanoid.Jump = true");
    p.frames(5);
    p.run("workspace.Dummy.Humanoid:ChangeState(Enum.HumanoidStateType.Jumping)");
    p.frames(90);
    let y = root_y(&p, "Dummy");
    p.frames(30);
    assert!(
        (root_y(&p, "Dummy") - y).abs() < 0.05,
        "it jumped again after landing"
    );
}
