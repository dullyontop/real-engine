//! Seats and vehicle seats, and the bodies of dead characters.

use re_script::ContextKind;

use crate::place::{Place, character_place};

fn settled(rig: &str) -> Place {
    let mut p = character_place(ContextKind::Server);
    p.character(rig, "Dummy", 0.0, 0.0);
    p.frames(30);
    p
}

fn pos(p: &Place, path: &[&str]) -> [f32; 3] {
    let e = p.find(path);
    p.cframe(e, "CFrame").p
}

/// An anchored seat, 2 by 1 by 2, whose top is at y = 1.
fn seat(p: &mut Place, class: &str, z: f32) {
    p.run(&format!(
        "local s = Instance.new('{class}')
         s.Name = 'Chair'
         s.Anchored = true
         s.Size = Vector3.new(2, 1, 2)
         s.CFrame = CFrame.new(0, 0.5, {z})
         s.Parent = workspace"
    ));
}

#[test]
fn walking_into_a_seat_sits_in_it() {
    let mut p = settled("R15");
    seat(&mut p, "Seat", -6.0);
    p.run(
        "local h = workspace.Dummy.Humanoid
         h.Seated:Connect(function(on, s) print('seated', on, s and s.Name) end)
         h:Move(Vector3.new(0, 0, -1))",
    );
    p.frames(40);
    p.run(
        "local h = workspace.Dummy.Humanoid
         local s = workspace.Chair
         print(h.Sit, h.SeatPart == s, s.Occupant == h, h:GetState().Name)
         local w = s:FindFirstChild('SeatWeld')
         print(w.Part0 == s, w.Part1 == workspace.Dummy.HumanoidRootPart)",
    );
    assert_eq!(
        p.out(),
        ["seated true Chair", "true true true Seated", "true true"]
    );
    // The root part's bottom rests on the seat's top, at the seat's middle.
    let [x, y, z] = pos(&p, &["Workspace", "Dummy", "HumanoidRootPart"]);
    assert!(
        x.abs() < 1e-3 && (y - 2.0).abs() < 1e-3 && (z + 6.0).abs() < 1e-3,
        "root at {x} {y} {z}"
    );
    // It stays put while seated, though it still asks to walk.
    p.frames(20);
    let [_, _, z2] = pos(&p, &["Workspace", "Dummy", "HumanoidRootPart"]);
    assert!((z2 + 6.0).abs() < 1e-3, "drifted to {z2}");
}

#[test]
fn jumping_stands_up_and_frees_the_seat() {
    let mut p = settled("R15");
    seat(&mut p, "Seat", -6.0);
    p.run("workspace.Chair:Sit(workspace.Dummy.Humanoid)");
    p.frames(2);
    p.run(
        "local h = workspace.Dummy.Humanoid
         h.Seated:Connect(function(on, s) print('seated', on, s) end)
         h.Jump = true",
    );
    p.frames(6);
    p.run(
        "local h = workspace.Dummy.Humanoid
         print(h.Sit, h.SeatPart, workspace.Chair.Occupant, workspace.Chair:FindFirstChild('SeatWeld'))
         print(h:GetState().Name)",
    );
    assert_eq!(
        p.out(),
        ["seated false nil", "false nil nil nil", "Jumping"]
    );
    // It rises off the seat.
    let [_, y, _] = pos(&p, &["Workspace", "Dummy", "HumanoidRootPart"]);
    assert!(y > 4.5, "root at {y}");
}

#[test]
fn a_seated_character_rides_its_seat() {
    let mut p = settled("R6");
    seat(&mut p, "Seat", -6.0);
    p.run("workspace.Chair:Sit(workspace.Dummy.Humanoid)");
    p.frames(2);
    p.run("workspace.Chair.CFrame = CFrame.new(10, 5.5, 3) * CFrame.Angles(0, math.pi / 2, 0)");
    p.frames(2);
    let [x, y, z] = pos(&p, &["Workspace", "Dummy", "HumanoidRootPart"]);
    assert!(
        (x - 10.0).abs() < 1e-3 && (y - 7.0).abs() < 1e-3 && (z - 3.0).abs() < 1e-3,
        "root at {x} {y} {z}"
    );
    // The limbs follow the root part through their joints.
    let [hx, hy, _] = pos(&p, &["Workspace", "Dummy", "Head"]);
    assert!((hx - 10.0).abs() < 1e-3 && hy > y, "head at {hx} {hy}");
    // Destroying the weld stands it up.
    p.run(
        "workspace.Chair.SeatWeld:Destroy()
         task.wait()
         print(workspace.Dummy.Humanoid.Sit, workspace.Chair.Occupant)",
    );
    p.frames(2);
    assert_eq!(p.out(), ["false nil"]);
}

#[test]
fn a_vehicle_seat_reports_its_occupants_input() {
    let mut p = settled("R15");
    seat(&mut p, "VehicleSeat", -6.0);
    p.run(
        "local h = workspace.Dummy.Humanoid
         workspace.Chair:Sit(h)
         h:Move(Vector3.new(0, 0, -1))",
    );
    p.frames(2);
    p.run(
        "local s = workspace.Chair
         print(s.Throttle, s.Steer, s.ThrottleFloat)
         workspace.Dummy.Humanoid:Move(Vector3.new(-1, 0, 0))",
    );
    p.frames(2);
    p.run(
        "local s = workspace.Chair
         print(s.Throttle, s.Steer, s.SteerFloat)
         s.Disabled = true",
    );
    p.frames(2);
    p.run(
        "local s = workspace.Chair
         print(s.Occupant, s.Throttle, s.Steer, workspace.Dummy.Humanoid.Sit)",
    );
    assert_eq!(p.out(), ["1 0 1", "0 -1 -1", "nil 0 0 false"]);
}

#[test]
fn a_disabled_seat_is_not_sat_in() {
    let mut p = settled("R15");
    seat(&mut p, "Seat", -6.0);
    p.run(
        "workspace.Chair.Disabled = true
         workspace.Dummy.Humanoid:Move(Vector3.new(0, 0, -1))",
    );
    p.frames(40);
    p.run("print(workspace.Dummy.Humanoid.Sit, workspace.Chair.Occupant)");
    assert_eq!(p.out(), ["false nil"]);
}

#[test]
fn a_dead_character_falls_apart() {
    let mut p = settled("R15");
    let [_, head_y, _] = pos(&p, &["Workspace", "Dummy", "Head"]);
    p.run("workspace.Dummy.Humanoid.Health = 0");
    p.frames(90);
    let [_, y, _] = pos(&p, &["Workspace", "Dummy", "Head"]);
    assert!(y < head_y - 2.0, "head stayed at {y} (was {head_y})");
    // Nothing falls through the floor.
    for part in ["Head", "UpperTorso", "LeftFoot", "HumanoidRootPart"] {
        let [_, y, _] = pos(&p, &["Workspace", "Dummy", part]);
        assert!(y > -0.5, "{part} at {y}");
    }
}

#[test]
fn a_dead_seated_character_leaves_its_seat() {
    let mut p = settled("R6");
    seat(&mut p, "Seat", -6.0);
    p.run("workspace.Chair:Sit(workspace.Dummy.Humanoid)");
    p.frames(2);
    p.run("workspace.Dummy.Humanoid.Health = 0");
    p.frames(2);
    p.run("print(workspace.Chair.Occupant, workspace.Chair:FindFirstChild('SeatWeld'))");
    assert_eq!(p.out(), ["nil nil"]);
}
