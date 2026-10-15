//! The Animator: built-in clips on a rig, track properties and events, and
//! the pose rule (a script's joint write wins over the Animator).

use re_script::ContextKind;

use crate::place::{Place, character_place};

fn rigged(rig: &str) -> Place {
    let mut p = character_place(ContextKind::Server);
    p.character(rig, "Dummy", 0.0, 0.0);
    p.frames(2);
    p
}

fn shoulder_angle(p: &Place) -> f32 {
    let m = p.find(&["Workspace", "Dummy", "Torso", "Right Shoulder"]);
    let t = p.cframe(m, "Transform");
    let (rx, ry, rz) = t.to_euler_xyz();
    rx.abs() + ry.abs() + rz.abs()
}

#[test]
fn a_built_in_clip_moves_the_joints_and_reports_its_state() {
    let mut p = rigged("R6");
    p.run(
        "local a = Instance.new('Animation')
         a.AnimationId = 'builtin:r6_walk'
         local t = workspace.Dummy.Humanoid.Animator:LoadAnimation(a)
         print(t.IsPlaying, t.Length > 0, t.Looped, t.Priority.Name)
         t.DidLoop:Connect(function() print('looped') end)
         t:Play(0)",
    );
    assert_eq!(p.out(), ["false true true Core"]);
    assert!(shoulder_angle(&p) < 1e-4);
    p.frames(10);
    assert!(shoulder_angle(&p) > 0.05, "the arm swings");
    p.run(
        "local t = workspace.Dummy.Humanoid.Animator:GetPlayingAnimationTracks()[1]
         print(t.IsPlaying, t.WeightCurrent, #workspace.Dummy.Humanoid.Animator:GetPlayingAnimationTracks())",
    );
    assert_eq!(p.out(), ["true 1 1"]);
    let len = 60 * 2;
    p.frames(len);
    assert!(p.out().iter().any(|l| l == "looped"));
}

#[test]
fn stop_fades_out_and_fires_stopped() {
    let mut p = rigged("R15");
    p.run(
        "local a = Instance.new('Animation')
         a.AnimationId = 'builtin:r15_walk'
         local t = workspace.Dummy.Humanoid:LoadAnimation(a)
         t.Stopped:Connect(function() print('stopped', t.IsPlaying) end)
         t:Play(0)",
    );
    p.frames(5);
    p.run("workspace.Dummy.Humanoid.Animator:GetPlayingAnimationTracks()[1]:Stop(0.1)");
    p.frames(12);
    assert_eq!(p.out(), ["stopped false"]);
}

#[test]
fn script_writes_to_track_properties_reach_the_mixer() {
    let mut p = rigged("R6");
    p.run(
        "local a = Instance.new('Animation')
         a.AnimationId = 'builtin:r6_idle'
         local t = workspace.Dummy.Humanoid.Animator:LoadAnimation(a)
         t.Looped = false
         t.Priority = Enum.AnimationPriority.Action
         t.Ended:Connect(function() print('ended') end)
         t:Play(0)
         t.TimePosition = t.Length - 0.05
         t:AdjustSpeed(2)
         print(t.Priority.Name, t.Speed)",
    );
    p.frames(5);
    p.run("print(#workspace.Dummy.Humanoid.Animator:GetPlayingAnimationTracks())");
    assert_eq!(p.out(), ["Action 2", "ended", "0"]);
}

#[test]
fn a_script_write_to_a_joint_wins_over_the_animator() {
    let mut p = rigged("R6");
    p.run(
        "local a = Instance.new('Animation')
         a.AnimationId = 'builtin:r6_walk'
         workspace.Dummy.Humanoid.Animator:LoadAnimation(a):Play(0)",
    );
    p.frames(10);
    assert!(shoulder_angle(&p) > 0.05);
    // Written early in the frame (a render-step binding): kept.
    p.begin_frame();
    p.run("workspace.Dummy.Torso['Right Shoulder'].Transform = CFrame.new()");
    p.finish_frame(1.0 / 60.0);
    assert!(shoulder_angle(&p) < 1e-4, "the script's value is final");
    // Written after the Animator's step (Stepped or Heartbeat): kept too.
    p.begin_frame();
    p.with(|lua| re_compat::anim::step(lua, 1.0 / 60.0));
    p.run("workspace.Dummy.Torso['Right Shoulder'].Transform = CFrame.new()");
    assert!(shoulder_angle(&p) < 1e-4);
    // A frame with no script write: the Animator poses the joint again.
    p.frames(3);
    assert!(shoulder_angle(&p) > 0.01);
}

#[test]
fn unknown_ids_warn_and_play_nothing() {
    let mut p = rigged("R6");
    p.run(
        "local a = Instance.new('Animation')
         a.AnimationId = 'rbxassetid://424242'
         local t = workspace.Dummy.Humanoid.Animator:LoadAnimation(a)
         print(t.Length)
         t:Play()",
    );
    let out = p.out();
    assert!(out[0].starts_with("W [RE9201]"), "{out:?}");
    assert_eq!(out[1], "0");
}

#[test]
fn keyframe_sequences_play_through_substitutes_with_markers() {
    let mut p = rigged("R6");
    p.with(|lua| {
        let mut subs = std::collections::BTreeMap::new();
        subs.insert("777".to_string(), "game.ReplicatedStorage.Wave".to_string());
        re_compat::anim::set_substitutes(lua, subs);
        Ok(())
    });
    p.run(
        "local seq = Instance.new('KeyframeSequence')
         seq.Name = 'Wave'
         seq.Loop = false
         for i, t in {0, 0.5} do
             local k = Instance.new('Keyframe')
             k.Time = t
             k.Name = 'k' .. i
             local root = Instance.new('Pose')
             root.Name = 'HumanoidRootPart'
             root.Parent = k
             local torso = Instance.new('Pose')
             torso.Name = 'Torso'
             torso.Parent = root
             local arm = Instance.new('Pose')
             arm.Name = 'Right Arm'
             arm.CFrame = CFrame.Angles(0, 0, t * 2)
             arm.Parent = torso
             if i == 2 then
                 local m = Instance.new('KeyframeMarker')
                 m.Name = 'Hit'
                 m.Value = 'boom'
                 m.Parent = k
             end
             k.Parent = seq
         end
         seq.Parent = game.ReplicatedStorage
         local a = Instance.new('Animation')
         a.AnimationId = 'rbxassetid://777'
         local t = workspace.Dummy.Humanoid.Animator:LoadAnimation(a)
         t:GetMarkerReachedSignal('Hit'):Connect(function(v) print('marker', v) end)
         print(t.Length, t:GetTimeOfKeyframe('k2'))
         t:Play(0)",
    );
    p.frames(40);
    assert_eq!(p.out(), ["0.5 0.5", "marker boom"]);
}

#[test]
fn ik_bends_an_arm_toward_a_target() {
    let mut p = rigged("R15");
    p.run(
        "local target = Instance.new('Part')
         target.Anchored = true
         target.CFrame = CFrame.new(2.2, 4.5, -0.5)
         target.Parent = workspace
         local ik = Instance.new('IKControl')
         ik.Type = Enum.IKControlType.Position
         ik.ChainRoot = workspace.Dummy.RightUpperArm
         ik.EndEffector = workspace.Dummy.RightHand
         ik.Target = target
         ik.Parent = workspace.Dummy.Humanoid",
    );
    p.frames(3);
    let hand = p.find(&["Workspace", "Dummy", "RightHand"]);
    let target = [2.2f32, 4.5, -0.5];
    let at = p.cframe(hand, "CFrame").p;
    let d =
        ((at[0] - target[0]).powi(2) + (at[1] - target[1]).powi(2) + (at[2] - target[2]).powi(2))
            .sqrt();
    assert!(d < 0.3, "hand at {at:?}");
}
