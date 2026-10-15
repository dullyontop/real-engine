//! Physics for parts, as scripts see it: free fall, collision groups, welds,
//! constraints and movers, touches, static cells, and the workspace queries
//! against brute force.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use glam::{Quat, Vec3};
use re_compat::physics;
use re_compat::scene::create_place;
use re_compat::{ClassMarker, CompatSetup, CompatVm, SignalBehavior};
use re_physics::parts::geom::{Placed, gjk_distance, obb_overlap, obb_sphere_overlap};
use re_physics::{PartGeom, PartShape, Pose};
use re_reflect::Value;
use re_scene::{Entity, World};
use re_script::host::{NoServices, WorldHost};
use re_script::{ContextKind, RuntimeConfig};

#[path = "fixtures/physics/bigmap.rs"]
mod bigmap;

const DT: f32 = 1.0 / 60.0;

struct Place {
    setup: Arc<CompatSetup>,
    world: World,
    services: NoServices,
    vm: CompatVm,
    _dir: tempfile::TempDir,
}

fn place() -> Place {
    let setup: Arc<CompatSetup> = CompatSetup::standard().unwrap();
    let mut world = World::new();
    create_place(
        &mut world,
        &setup.classes,
        &[
            "Workspace",
            "ReplicatedStorage",
            "ServerScriptService",
            "Players",
        ],
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let vm = CompatVm::new(
        &setup,
        ContextKind::Server,
        &mut world,
        &RuntimeConfig::new(dir.path()).with_limits(60_000, 512),
        SignalBehavior::Immediate,
    )
    .unwrap();
    physics::prepare_world(&mut world, &setup.classes).unwrap();
    Place {
        setup,
        world,
        services: NoServices::default(),
        vm,
        _dir: dir,
    }
}

impl Place {
    /// Runs a script; fails the test on any error it raises.
    fn run(&mut self, source: &str) {
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = self.vm.exec(&mut host, "test", source);
        assert!(d.is_empty(), "{d:#?}");
    }

    /// One frame: scripts, physics, touches.
    fn frame(&mut self) {
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = self.vm.step(&mut host, f64::from(DT));
        assert!(d.is_empty(), "{d:#?}");
        let d = physics::step(&mut self.world, DT);
        assert!(d.is_empty(), "{d:#?}");
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = self.vm.run(&mut host, physics::dispatch);
        assert!(d.is_empty(), "{d:#?}");
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame();
        }
    }

    /// The printed lines since the last call.
    fn out(&mut self) -> Vec<String> {
        self.vm.take_output().into_iter().map(|l| l.text).collect()
    }

    /// Runs `expr` and returns what it printed.
    fn eval(&mut self, expr: &str) -> String {
        self.run(&format!("print({expr})"));
        self.out().join("\n")
    }

    fn num(&mut self, expr: &str) -> f32 {
        let s = self.eval(expr);
        s.trim()
            .parse()
            .unwrap_or_else(|_| panic!("{expr} printed {s:?}"))
    }

    /// Every instance with its name, by name.
    fn by_name(&mut self) -> BTreeMap<String, Entity> {
        let mut q = self.world.ecs.query::<(Entity, &ClassMarker)>();
        let list: Vec<Entity> = q.iter(&self.world.ecs).map(|(e, _)| e).collect();
        list.into_iter()
            .map(|e| (re_compat::instance::tree::name(&self.world, e), e))
            .collect()
    }

    fn get(&self, e: Entity, prop: &str) -> Value {
        self.setup
            .classes
            .get_property(&self.world, e, prop)
            .unwrap()
    }
}

// Grouped so `cargo test -p re_compat physics` selects them.
mod physics_parts {
    use super::*;

    #[test]
    fn a_part_falls_98_studs_in_a_second() {
        let mut p = place();
        p.run(
            r#"
            local b = Instance.new("Part")
            b.Name = "Ball"
            b.Size = Vector3.new(2, 2, 2)
            b.Position = Vector3.new(0, 500, 0)
            b.Parent = workspace
            "#,
        );
        p.frames(60);
        let y = p.num("workspace.Ball.Position.Y");
        let fell = 500.0 - y;
        assert!((fell - 98.1).abs() <= 0.5, "fell {fell}");
        let v = p.num("workspace.Ball.AssemblyLinearVelocity.Y");
        assert!((v + 196.2).abs() < 2.0, "{v}");
        assert_eq!(p.num("workspace.Gravity"), 196.2);
    }

    #[test]
    fn anchored_parts_stay_and_carry_resting_parts() {
        let mut p = place();
        p.run(
            r#"
            local base = Instance.new("Part")
            base.Name = "Base"
            base.Anchored = true
            base.Size = Vector3.new(100, 2, 100)
            base.Position = Vector3.new(0, 0, 0)
            base.Parent = workspace
            local box = Instance.new("Part")
            box.Name = "Box"
            box.Size = Vector3.new(2, 2, 2)
            box.Position = Vector3.new(0, 10, 0)
            box.Parent = workspace
            "#,
        );
        p.frames(120);
        let y = p.num("workspace.Box.Position.Y");
        assert!((y - 2.0).abs() < 0.1, "rests at {y}");
        assert_eq!(p.num("workspace.Base.Position.Y"), 0.0);
        // The anchored part sits in a static cell.
        let names = p.by_name();
        let cells = p.world.ecs.resource::<physics::StaticCells>();
        assert_eq!(cells.cells().len(), 1);
        assert!(cells.cell_of(names["Base"]).is_some());
        // Moving it takes it out of its cell; the box falls with nothing below.
        p.run("workspace.Base.Position = Vector3.new(0, -50, 300)");
        p.frames(30);
        let cells = p.world.ecs.resource::<physics::StaticCells>();
        assert!(cells.cell_of(names["Base"]).is_none());
        let y = p.num("workspace.Box.Position.Y");
        assert!(y < 0.0, "{y}");
        assert_eq!(p.num("workspace.Base.Position.Z"), 300.0);
    }

    /// Every ordered pair of nine groups: a part of the first group dropped on
    /// an anchored part of the second rests on it exactly when the two collide.
    #[test]
    fn the_collision_matrix_holds_for_every_ordered_pair() {
        let mut p = place();
        let groups = ["Default", "G1", "G2", "G3", "G4", "G5", "G6", "G7", "G8"];
        let mut rng = Rng(7);
        let mut collides = BTreeMap::new();
        let mut script = String::from("local PS = game:GetService(\"PhysicsService\")\n");
        for g in &groups[1..] {
            script += &format!("PS:RegisterCollisionGroup({g:?})\n");
        }
        for (i, a) in groups.iter().enumerate() {
            for b in &groups[i..] {
                let c = rng.next() < 0.5;
                collides.insert((*a, *b), c);
                collides.insert((*b, *a), c);
                script += &format!("PS:CollisionGroupSetCollidable({a:?}, {b:?}, {c})\n");
            }
        }
        for (i, a) in groups.iter().enumerate() {
            for (j, b) in groups.iter().enumerate() {
                let (x, z) = (i as f32 * 20.0, j as f32 * 20.0);
                script += &format!(
                    r#"
                    do
                        local floor = Instance.new("Part")
                        floor.Name = "F_{i}_{j}"
                        floor.Anchored = true
                        floor.Size = Vector3.new(8, 1, 8)
                        floor.Position = Vector3.new({x}, 0, {z})
                        floor.CollisionGroup = {b:?}
                        floor.Parent = workspace
                        local box = Instance.new("Part")
                        box.Name = "B_{i}_{j}"
                        box.Size = Vector3.new(2, 2, 2)
                        box.Position = Vector3.new({x}, 3, {z})
                        box.CollisionGroup = {a:?}
                        box.Parent = workspace
                    end
                    "#
                );
            }
        }
        p.run(&script);
        // The service agrees with the table, both ways round.
        for (i, a) in groups.iter().enumerate() {
            for b in groups.iter() {
                let got = p.eval(&format!(
                    "game:GetService(\"PhysicsService\"):CollisionGroupsAreCollidable({a:?}, {b:?})"
                ));
                assert_eq!(got, collides[&(*a, *b)].to_string(), "{a} {b}");
            }
            let _ = i;
        }
        p.frames(60);
        for (i, a) in groups.iter().enumerate() {
            for (j, b) in groups.iter().enumerate() {
                let y = p.num(&format!("workspace.B_{i}_{j}.Position.Y"));
                let rests = (y - 1.5).abs() < 0.2;
                let fell = y < -10.0;
                assert!(rests || fell, "{a} on {b}: y {y}");
                assert_eq!(rests, collides[&(*a, *b)], "{a} on {b}: y {y}");
                let ccw = p.eval(&format!(
                    "workspace.B_{i}_{j}:CanCollideWith(workspace.F_{i}_{j})"
                ));
                assert_eq!(ccw, collides[&(*a, *b)].to_string());
            }
        }
    }

    #[test]
    fn collision_group_errors_carry_re9250() {
        let mut p = place();
        p.run(
            r#"
            local PS = game:GetService("PhysicsService")
            print(pcall(function() PS:CollisionGroupSetCollidable("Nope", "Default", false) end))
            print(pcall(function() PS:UnregisterCollisionGroup("Default") end))
            for i = 1, 31 do PS:RegisterCollisionGroup("G" .. i) end
            print(pcall(function() PS:RegisterCollisionGroup("Extra") end))
            print(PS:GetMaxCollisionGroups(), #PS:GetRegisteredCollisionGroups())
            print(PS:IsCollisionGroupRegistered("G5"), PS:IsCollisionGroupRegistered("Extra"))
            "#,
        );
        let out = p.out();
        assert!(
            out[0].starts_with("false") && out[0].contains("[RE9250]"),
            "{out:?}"
        );
        assert!(out[1].contains("[RE9250]"), "{out:?}");
        assert!(out[2].contains("[RE9250]"), "{out:?}");
        assert_eq!(out[3], "32\t32");
        assert_eq!(out[4], "true\tfalse");
    }

    #[test]
    fn network_ownership_is_recorded_on_the_assembly_and_refused_when_grounded() {
        let mut p = place();
        let mut host = WorldHost {
            world: &mut p.world,
            services: &mut p.services,
        };
        let (player, d) = re_compat::players::add_player(
            &p.vm,
            &mut host,
            &re_compat::players::PlayerSpec::numbered(1),
        );
        assert!(d.is_empty(), "{d:#?}");
        let player = player.unwrap();
        p.run(
            r#"
            local a = Instance.new("Part")
            a.Name = "A"
            a.Position = Vector3.new(0, 50, 0)
            a.Parent = workspace
            local b = Instance.new("Part")
            b.Name = "B"
            b.Position = Vector3.new(0, 51, 0)
            b.Parent = workspace
            local w = Instance.new("WeldConstraint")
            w.Part0 = a
            w.Part1 = b
            w.Parent = a
            local g = Instance.new("Part")
            g.Name = "G"
            g.Anchored = true
            g.Position = Vector3.new(20, 0, 0)
            g.Parent = workspace
            "#,
        );
        p.frame();
        let me = "game.Players:GetPlayers()[1]";
        assert_eq!(p.eval("workspace.A:CanSetNetworkOwnership()"), "true\tnil");
        p.run(&format!("workspace.A:SetNetworkOwner({me})"));
        assert_eq!(
            p.eval(&format!("workspace.B:GetNetworkOwner() == {me}")),
            "true"
        );
        let parts = p.by_name();
        assert_eq!(
            re_compat::net::network_owner(&p.world, parts["A"]),
            Some(player)
        );
        assert_eq!(
            re_compat::net::network_owner(&p.world, parts["B"]),
            Some(player)
        );
        p.run("workspace.B:SetNetworkOwnershipAuto()");
        assert_eq!(p.eval("workspace.A:GetNetworkOwner()"), "nil");
        let refused = p.eval("workspace.G:CanSetNetworkOwnership()");
        assert!(refused.starts_with("false\t"), "{refused}");
        assert_eq!(
            p.eval(&format!(
                "(pcall(workspace.G.SetNetworkOwner, workspace.G, {me}))"
            )),
            "false"
        );
        assert_eq!(
            p.eval("(pcall(workspace.A.SetNetworkOwner, workspace.A, workspace.B))"),
            "false"
        );
        assert_eq!(re_compat::net::network_owner(&p.world, parts["G"]), None);
    }

    #[test]
    fn every_member_without_its_own_test_is_called_from_the_fixture() {
        let mut p = place();
        let src = include_str!("fixtures/physics/members.luau");
        p.run(&format!("(function() {src} end)().setup()"));
        p.frames(120);
        p.run(&format!("(function() {src} end)().check()"));
        assert_eq!(p.out(), ["members ok"]);
    }

    #[test]
    fn a_parts_surfaces_and_lock_are_stored() {
        let mut p = place();
        p.run(
            r#"
            local floor = Instance.new("Part")
            floor.Name = "Floor"
            floor.TopSurface = Enum.SurfaceType.Studs
            floor.BottomSurface = "Inlet"
            floor.Locked = true
            floor.Parent = workspace
            "#,
        );
        assert_eq!(p.eval("workspace.Floor.TopSurface.Name"), "Studs");
        assert_eq!(p.eval("workspace.Floor.BottomSurface.Name"), "Inlet");
        assert_eq!(p.eval("workspace.Floor.Locked"), "true");
        for face in ["Front", "Back", "Left", "Right"] {
            assert_eq!(
                p.eval(&format!("workspace.Floor.{face}Surface.Name")),
                "Smooth"
            );
        }
    }

    #[test]
    fn a_weld_holds_the_offset_its_parts_had_when_it_was_joined() {
        let mut p = place();
        p.run(
            r#"
            local function gun(name, x)
                local m = Instance.new("Model")
                m.Name = name
                local body = Instance.new("Part")
                body.Name = "Body"
                body.Anchored = true
                body.Size = Vector3.new(0.2, 0.2, 0.2)
                body.Parent = m
                for i = 1, 3 do
                    local q = Instance.new("Part")
                    q.Name = "Bit" .. i
                    q.Size = Vector3.new(0.2, 0.2, 0.5)
                    q.CFrame = body.CFrame * CFrame.new(0, 0, -i)
                    q.Parent = m
                    local weld = Instance.new("WeldConstraint")
                    weld.Part0 = body
                    weld.Part1 = q
                    weld.Parent = q
                end
                m.Parent = workspace
                return m
            end
            -- Joined at the origin, then moved far away in the same frame:
            -- the welded parts go with it.
            local a = gun("A")
            a.Body.CFrame = CFrame.new(3000, 50, 2000)
            -- Moved first, then joined there.
            local m = Instance.new("Model")
            m.Name = "B"
            local body = Instance.new("Part")
            body.Name = "Body"
            body.Anchored = true
            body.CFrame = CFrame.new(-1000, 50, 800)
            body.Parent = m
            local q = Instance.new("Part")
            q.Name = "Bit1"
            q.CFrame = body.CFrame * CFrame.new(0, 0, -1)
            q.Parent = m
            local weld = Instance.new("WeldConstraint")
            weld.Part0 = body
            weld.Part1 = q
            weld.Parent = q
            m.Parent = workspace
            "#,
        );
        p.frame();
        assert!((p.num("workspace.A.Bit3.Position.X") - 3000.0).abs() < 1e-3);
        assert!((p.num("workspace.A.Bit3.Position.Z") - 1997.0).abs() < 1e-3);
        assert!((p.num("workspace.B.Bit1.Position.Z") - 799.0).abs() < 1e-3);
        // The body moves every frame while the assembly is rebuilt (a part
        // joins it): the offsets stay.
        for i in 1..=5 {
            p.run(&format!(
                r#"
                workspace.A.Body.CFrame = CFrame.new({x}, 50, 2000)
                local extra = Instance.new("Part")
                extra.CFrame = workspace.A.Body.CFrame * CFrame.new(0, 1, 0)
                extra.Parent = workspace.A
                local weld = Instance.new("WeldConstraint")
                weld.Part0 = workspace.A.Body
                weld.Part1 = extra
                weld.Parent = extra
                "#,
                x = 3000 - i * 100
            ));
            p.frame();
        }
        assert!((p.num("workspace.A.Bit3.Position.X") - 2500.0).abs() < 1e-3);
        assert!((p.num("workspace.A.Bit3.Position.Z") - 1997.0).abs() < 1e-3);
        assert_eq!(p.eval("#workspace.A:GetChildren()"), "9");
        assert_eq!(
            p.eval("workspace.A.Bit3:GetRootPart() == workspace.A.Body"),
            "true"
        );
    }

    #[test]
    fn welded_parts_move_as_one_and_split_when_the_weld_goes() {
        let mut p = place();
        p.run(
            r#"
            local a = Instance.new("Part")
            a.Name = "A"
            a.Size = Vector3.new(4, 1, 3)
            a.Position = Vector3.new(0, 100, 0)
            a.Parent = workspace
            local b = Instance.new("Part")
            b.Name = "B"
            b.Size = Vector3.new(2, 2, 2)
            b.CFrame = CFrame.new(3, 101, 0) * CFrame.Angles(0, 0.5, 0)
            b.Parent = workspace
            local w = Instance.new("WeldConstraint")
            w.Name = "W"
            w.Part0 = a
            w.Part1 = b
            w.Parent = a
            assert(b:GetRootPart() == a, "the larger part is the root")
            assert(#a:GetConnectedParts(true) == 2)
            assert(math.abs(a.AssemblyMass - (a:GetMass() + b:GetMass())) < 1e-3)
            a:ApplyAngularImpulse(Vector3.new(0, 2000, 0))
            "#,
        );
        let before = p.eval("tostring(workspace.A.CFrame:ToObjectSpace(workspace.B.CFrame))");
        p.frames(30);
        let after = p.eval("tostring(workspace.A.CFrame:ToObjectSpace(workspace.B.CFrame))");
        let diff = p.num(
            "(workspace.A.CFrame:ToObjectSpace(workspace.B.CFrame).Position - Vector3.new(3, 1, 0)).Magnitude",
        );
        assert!(diff < 1e-3, "{before} -> {after}");
        let spin = p.num("workspace.B.AssemblyAngularVelocity.Y");
        assert!(spin.abs() > 0.1, "the weld carries the spin: {spin}");
        assert_eq!(
            p.eval("workspace.A.AssemblyRootPart == workspace.B.AssemblyRootPart"),
            "true"
        );
        // Breaking the weld splits the assembly.
        p.run(
            r#"
            workspace.A.W:Destroy()
            local a, b = workspace.A, workspace.B
            assert(b:GetRootPart() == b and a:GetRootPart() == a)
            b:ApplyImpulse(Vector3.new(0, 0, 50) * b:GetMass())
            "#,
        );
        p.frames(30);
        let dz = p.num("workspace.B.Position.Z - workspace.A.Position.Z");
        assert!(dz > 10.0, "{dz}");
    }

    #[test]
    fn a_weld_places_part1_from_c0_and_c1() {
        let mut p = place();
        p.run(
            r#"
            local a = Instance.new("Part")
            a.Name = "A"
            a.Anchored = true
            a.CFrame = CFrame.new(10, 5, 0) * CFrame.Angles(0, math.pi / 2, 0)
            a.Parent = workspace
            local b = Instance.new("Part")
            b.Name = "B"
            b.Parent = workspace
            local w = Instance.new("Weld")
            w.Part0 = a
            w.Part1 = b
            w.C0 = CFrame.new(0, 2, 0)
            w.C1 = CFrame.new(1, 0, 0)
            w.Parent = a
            "#,
        );
        p.frames(10);
        let d = p.num(
            "((workspace.A.CFrame * CFrame.new(0, 2, 0) * CFrame.new(1, 0, 0):Inverse()).Position - workspace.B.Position).Magnitude",
        );
        assert!(d < 1e-3, "{d}");
        assert_eq!(p.eval("workspace.B:IsGrounded()"), "true");
    }

    #[test]
    fn touched_and_touch_ended_fire_on_both_parts() {
        let mut p = place();
        p.run(
            r#"
            local base = Instance.new("Part")
            base.Name = "Base"
            base.Anchored = true
            base.Size = Vector3.new(50, 1, 50)
            base.Parent = workspace
            local box = Instance.new("Part")
            box.Name = "Box"
            box.Size = Vector3.new(2, 2, 2)
            box.Position = Vector3.new(0, 3, 0)
            box.CustomPhysicalProperties = PhysicalProperties.new(0.35, 0.8, 0.03, 1, 100)
            box.Parent = workspace
            base.Touched:Connect(function(o) print("base touched " .. o.Name) end)
            box.Touched:Connect(function(o) print("box touched " .. o.Name) end)
            box.TouchEnded:Connect(function(o) print("box ended " .. o.Name) end)
            "#,
        );
        p.frames(60);
        let out = p.out();
        // A landing may bounce once; it ends touching.
        assert!(out.contains(&"box touched Base".to_string()), "{out:?}");
        assert!(out.contains(&"base touched Box".to_string()), "{out:?}");
        assert_eq!(
            out.last().map(String::as_str),
            Some("base touched Box"),
            "{out:?}"
        );
        assert_eq!(p.eval("#workspace.Box:GetTouchingParts()"), "1");
        assert_eq!(
            p.eval("workspace.Box.CustomPhysicalProperties.Density"),
            "0.3499999940395355"
        );
        // Changing the group lets it fall through; the touch ends.
        p.run(
            r#"
            local PS = game:GetService("PhysicsService")
            PS:RegisterCollisionGroup("Debris")
            PS:CollisionGroupSetCollidable("Debris", "Default", false)
            workspace.Box.CollisionGroup = "Debris"
            "#,
        );
        p.frames(30);
        let out = p.out();
        assert!(out.contains(&"box ended Base".to_string()), "{out:?}");
        assert!(p.num("workspace.Box.Position.Y") < 0.0);
    }

    #[test]
    fn parts_below_the_destroy_height_are_destroyed() {
        let mut p = place();
        p.run(
            r#"
            workspace.FallenPartsDestroyHeight = -20
            local b = Instance.new("Part")
            b.Name = "Faller"
            b.Parent = workspace
            b.Destroying:Connect(function() print("gone") end)
            "#,
        );
        p.frames(40);
        assert_eq!(p.out(), ["gone"]);
        assert_eq!(p.eval("workspace:FindFirstChild('Faller')"), "nil");
    }

    #[test]
    fn a_part_outside_the_world_limits_is_reported_once_and_kept() {
        let mut p = place();
        p.run(
            r#"
            local a = Instance.new("Part")
            a.Name = "Near"
            a.Anchored = true
            a.Parent = workspace
            local b = Instance.new("Part")
            b.Name = "Far"
            b.Position = Vector3.new(12000, 100, 0)
            b.Parent = workspace
            "#,
        );
        let d = physics::step(&mut p.world, DT);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code.to_string(), "RE5006");
        assert_eq!(d[0].severity, re_core::Severity::Warning);
        // It stays where it is, without a body, and is not reported again
        // however often a script parks it out there.
        assert!(physics::step(&mut p.world, DT).is_empty());
        assert_eq!(p.num("workspace.Far.Position.Y"), 100.0);
        for y in [10000, 20000, 10000] {
            p.run(&format!("workspace.Far.CFrame = CFrame.new(0, {y}, 0)"));
            assert!(physics::step(&mut p.world, DT).is_empty());
            assert_eq!(p.num("workspace.Far.Position.Y"), y as f32);
        }
        assert_eq!(
            p.eval("workspace:Raycast(Vector3.new(0, 10050, 0), Vector3.new(0, -100, 0))"),
            "nil"
        );
        // Back inside, it gets its body and falls.
        p.run("workspace.Far.CFrame = CFrame.new(20, 100, 0)");
        assert_eq!(
            p.eval(
                "workspace:Raycast(Vector3.new(20, 150, 0), Vector3.new(0, -100, 0)).Instance.Name"
            ),
            "Far"
        );
        for _ in 0..30 {
            assert!(physics::step(&mut p.world, DT).is_empty());
        }
        assert!(p.num("workspace.Far.Position.Y") < 99.0);
    }

    #[test]
    fn an_anchored_map_part_outside_the_world_limits_is_reported() {
        let mut p = place();
        p.run(
            r#"
            for i, x in {-4900, 12000} do
                local a = Instance.new("Part")
                a.Name = "P" .. i
                a.Anchored = true
                a.Position = Vector3.new(x, 0, 0)
                a.Parent = workspace
            end
            "#,
        );
        let d = physics::step(&mut p.world, DT);
        assert!(d.iter().any(|d| d.code.to_string() == "RE5006"), "{d:?}");
    }

    #[test]
    fn a_character_stands_where_its_joints_put_it_and_is_found_by_queries() {
        let mut p = place();
        p.run(
            r#"
            local m = Instance.new("Model")
            m.Name = "Dummy"
            local root = Instance.new("Part")
            root.Name = "HumanoidRootPart"
            root.Size = Vector3.new(2, 2, 1)
            root.Position = Vector3.new(0, 10, 0)
            root.Parent = m
            local arm = Instance.new("Part")
            arm.Name = "Arm"
            arm.Size = Vector3.new(1, 2, 1)
            arm.Position = Vector3.new(1.5, 10, 0)
            arm.Parent = m
            local j = Instance.new("Motor6D")
            j.Name = "Shoulder"
            j.Part0 = root
            j.Part1 = arm
            j.C0 = CFrame.new(1.5, 0, 0)
            j.Parent = root
            Instance.new("Humanoid").Parent = m
            m.Parent = workspace
            "#,
        );
        p.frames(60);
        assert_eq!(p.num("workspace.Dummy.HumanoidRootPart.Position.Y"), 10.0);
        assert_eq!(p.num("workspace.Dummy.Arm.Position.Y"), 10.0);
        assert_eq!(
            p.eval(
                "workspace:Raycast(Vector3.new(1.5, 50, 0), Vector3.new(0, -100, 0)).Instance.Name"
            ),
            "Arm"
        );
        // An animated joint moves the arm with it.
        p.run("workspace.Dummy.HumanoidRootPart.Shoulder.Transform = CFrame.new(0, 1, 0)");
        p.frames(2);
        assert!((p.num("workspace.Dummy.Arm.Position.Y") - 11.0).abs() < 1e-4);
        // Without its Humanoid the model is ordinary parts again, and falls.
        p.run("workspace.Dummy.Humanoid:Destroy()");
        p.frames(30);
        assert!(p.num("workspace.Dummy.HumanoidRootPart.Position.Y") < 9.0);
    }

    #[test]
    fn an_animated_joint_keeps_the_assembly_and_its_touches() {
        let mut p = place();
        p.run(
            r#"
            local base = Instance.new("Part")
            base.Name = "Base"
            base.Anchored = true
            base.Size = Vector3.new(50, 1, 50)
            base.Parent = workspace
            local a = Instance.new("Part")
            a.Name = "A"
            a.Size = Vector3.new(4, 2, 4)
            a.Position = Vector3.new(0, 1.5, 0)
            a.Parent = workspace
            local b = Instance.new("Part")
            b.Name = "B"
            b.Size = Vector3.new(1, 1, 1)
            b.Position = Vector3.new(0, 3, 0)
            b.Parent = workspace
            local j = Instance.new("Motor6D")
            j.Name = "Neck"
            j.Part0 = a
            j.Part1 = b
            j.C0 = CFrame.new(0, 1.5, 0)
            j.Parent = a
            a.TouchEnded:Connect(function(o) print("ended " .. o.Name) end)
            "#,
        );
        p.frames(30);
        p.out();
        let t0 = p.eval("workspace.A.AssemblyLinearVelocity.Magnitude < 0.5");
        assert_eq!(t0, "true");
        for i in 0..30 {
            p.run(&format!(
                "workspace.A.Neck.Transform = CFrame.Angles(0, {}, 0)",
                f32::from(i as u8) * 0.1
            ));
            p.frame();
        }
        assert!(p.out().is_empty(), "the touch with Base ended");
        assert_eq!(p.eval("#workspace.A:GetTouchingParts()"), "1");
    }

    #[test]
    fn a_map_wider_than_the_limits_around_the_origin_still_simulates() {
        let mut p = place();
        let add = |i: usize, x: i32| {
            format!(
                "local floor = Instance.new('Part') floor.Name = 'Floor{i}' floor.Anchored = true floor.Size = Vector3.new(20, 1, 20) floor.Position = Vector3.new({x}, 0, 0) floor.Parent = workspace \
                 local box = Instance.new('Part') box.Name = 'Box{i}' box.Size = Vector3.new(2, 2, 2) box.Position = Vector3.new({x}, 10, 0) box.Parent = workspace"
            )
        };
        // One end of the map first, the other after the simulation started:
        // the simulation's origin moves to hold both.
        p.run(&add(1, -4800));
        assert!(physics::step(&mut p.world, DT).is_empty());
        p.run(&add(2, 5000));
        for _ in 0..90 {
            let d = physics::step(&mut p.world, DT);
            assert!(d.is_empty(), "{d:?}");
        }
        for i in 1..=2 {
            let y = p.num(&format!("workspace.Box{i}.Position.Y"));
            assert!((y - 1.5).abs() < 0.05, "box {i} at {y}");
        }
        let hit = p.eval(
            "workspace:Raycast(Vector3.new(5000, 50, 0), Vector3.new(0, -100, 0)).Instance.Name",
        );
        assert_eq!(hit, "Box2");
        // A script moves a box to the far end: it lands there.
        p.run("workspace.Box1.Position = Vector3.new(5000, 10, 5)");
        for _ in 0..90 {
            assert!(physics::step(&mut p.world, DT).is_empty());
        }
        let y = p.num("workspace.Box1.Position.Y");
        assert!((y - 1.5).abs() < 0.05, "moved box at {y}");
    }

    #[test]
    fn hinge_motor_rope_and_movers_act() {
        let mut p = place();
        p.run(
            r#"
            local function part(name, pos, anchored, size)
                local x = Instance.new("Part")
                x.Name = name
                x.Size = size or Vector3.new(2, 2, 2)
                x.Position = pos
                x.Anchored = anchored
                x.Parent = workspace
                return x
            end
            local function att(p, cf)
                local a = Instance.new("Attachment")
                a.CFrame = cf or CFrame.new()
                a.Parent = p
                return a
            end
            -- A wheel on a motor hinge about X.
            local axle = part("Axle", Vector3.new(0, 50, 0), true)
            local wheel = part("Wheel", Vector3.new(3, 50, 0), false, Vector3.new(1, 4, 4))
            local h = Instance.new("HingeConstraint")
            h.Name = "Hinge"
            h.Attachment0 = att(axle, CFrame.new(1.5, 0, 0))
            h.Attachment1 = att(wheel, CFrame.new(-1.5, 0, 0))
            h.ActuatorType = Enum.ActuatorType.Motor
            h.AngularVelocity = 3
            h.MotorMaxTorque = 1e9
            h.Parent = workspace
            -- A part hanging on a rope.
            local hook = part("Hook", Vector3.new(50, 50, 0), true)
            local bob = part("Bob", Vector3.new(50, 45, 0), false)
            local r = Instance.new("RopeConstraint")
            r.Name = "Rope"
            r.Attachment0 = att(hook)
            r.Attachment1 = att(bob)
            r.Length = 8
            r.Parent = workspace
            -- A part held up by a force equal to its weight.
            local hover = part("Hover", Vector3.new(-50, 50, 0), false)
            local f = Instance.new("VectorForce")
            f.Attachment0 = att(hover)
            f.Force = Vector3.new(0, hover.AssemblyMass * workspace.Gravity, 0)
            f.ApplyAtCenterOfMass = true
            f.Parent = hover
            -- A part pulled to a point.
            local seek = part("Seek", Vector3.new(-100, 50, 0), false)
            local ap = Instance.new("AlignPosition")
            ap.Mode = Enum.PositionAlignmentMode.OneAttachment
            ap.Attachment0 = att(seek)
            ap.Position = Vector3.new(-100, 60, 20)
            ap.MaxForce = 1e9
            ap.Responsiveness = 50
            ap.Parent = seek
            -- A part driven sideways.
            local slide = part("Slide", Vector3.new(-150, 50, 0), false)
            local lv = Instance.new("LinearVelocity")
            lv.Attachment0 = att(slide)
            lv.VectorVelocity = Vector3.new(10, 0, 0)
            lv.MaxForce = 1e9
            lv.Parent = slide
            -- A part spun in place.
            local spin = part("Spin", Vector3.new(-200, 50, 0), true)
            local top = part("Top", Vector3.new(-200, 55, 0), false)
            local bs = Instance.new("BallSocketConstraint")
            bs.Attachment0 = att(spin, CFrame.new(0, 2.5, 0))
            bs.Attachment1 = att(top, CFrame.new(0, -2.5, 0))
            bs.Parent = workspace
            local av = Instance.new("AngularVelocity")
            av.Attachment0 = att(top)
            av.AngularVelocity = Vector3.new(0, 4, 0)
            av.MaxTorque = 1e9
            av.Parent = top
            "#,
        );
        assert_eq!(p.eval("workspace.Hinge.Active"), "true");
        p.frames(90);
        let a = p.num("workspace.Hinge.CurrentAngle");
        assert!(a.abs() > 10.0, "the motor turned the wheel: {a}");
        let d = p.num("(workspace.Wheel.Position - Vector3.new(3, 50, 0)).Magnitude");
        assert!(d < 0.1, "the hinge holds the wheel: {d}");
        let rope = p.num("workspace.Rope.CurrentDistance");
        assert!((rope - 8.0).abs() < 0.2, "{rope}");
        let hy = p.num("workspace.Hover.Position.Y");
        assert!((hy - 50.0).abs() < 0.1, "hover {hy}");
        let seek = p.num("(workspace.Seek.Position - Vector3.new(-100, 60, 20)).Magnitude");
        assert!(seek < 0.5, "seek {seek}");
        let vx = p.num("workspace.Slide.AssemblyLinearVelocity.X");
        let vy = p.num("workspace.Slide.AssemblyLinearVelocity.Y");
        assert!((vx - 10.0).abs() < 0.1 && vy.abs() < 0.1, "{vx} {vy}");
        let wy = p.num("workspace.Top.AssemblyAngularVelocity.Y");
        assert!((wy - 4.0).abs() < 0.2, "{wy}");
        let top = p.num("(workspace.Top.Position - Vector3.new(-200, 55, 0)).Magnitude");
        assert!(top < 0.1, "the ball socket holds: {top}");
    }

    #[test]
    fn pivots_models_and_attachments() {
        let mut p = place();
        p.run(
            r#"
            local m = Instance.new("Model")
            m.Name = "M"
            for i = 1, 3 do
                local x = Instance.new("Part")
                x.Name = "P" .. i
                x.Anchored = true
                x.Size = Vector3.new(2, 2, 2)
                x.Position = Vector3.new(i * 4, 0, 0)
                x.Parent = m
            end
            m.PrimaryPart = m.P1
            m.Parent = workspace
            local cf, size = m:GetBoundingBox()
            assert((cf.Position - Vector3.new(8, 0, 0)).Magnitude < 1e-4, tostring(cf))
            assert((size - Vector3.new(10, 2, 2)).Magnitude < 1e-4, tostring(size))
            m:PivotTo(CFrame.new(0, 10, 0))
            assert((m.P1.Position - Vector3.new(0, 10, 0)).Magnitude < 1e-4)
            assert((m.P3.Position - Vector3.new(8, 10, 0)).Magnitude < 1e-4)
            m:TranslateBy(Vector3.new(0, 0, 5))
            assert((m:GetPivot().Position - Vector3.new(0, 10, 5)).Magnitude < 1e-4)
            local a = Instance.new("Attachment")
            a.Position = Vector3.new(0, 1, 0)
            a.Parent = m.P2
            assert((a.WorldPosition - Vector3.new(4, 11, 5)).Magnitude < 1e-4, tostring(a.WorldPosition))
            a.WorldPosition = Vector3.new(4, 13, 5)
            assert((a.Position - Vector3.new(0, 3, 0)).Magnitude < 1e-4)
            a.Axis = Vector3.new(0, 0, 1)
            assert((a.WorldAxis - Vector3.new(0, 0, 1)).Magnitude < 1e-4)
            local ok, err = pcall(function() Instance.new("Model"):SetPrimaryPartCFrame(CFrame.new()) end)
            assert(not ok and string.find(tostring(err), "RE9251"), tostring(err))
            assert(workspace.Terrain.ClassName == "Terrain")
            assert(workspace.CurrentCamera.ClassName == "Camera")
            "#,
        );
    }

    /// A small deterministic generator.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32) / (1u64 << 24) as f32
        }
        fn range(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * self.next()
        }
        fn vec(&mut self, lo: f32, hi: f32) -> Vec3 {
            Vec3::new(self.range(lo, hi), self.range(lo, hi), self.range(lo, hi))
        }
        fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
            &xs[((self.next() * xs.len() as f32) as usize).min(xs.len() - 1)]
        }
    }

    const GROUPS: [&str; 4] = ["Default", "A", "B", "C"];
    /// The pairs that do not collide.
    const APART: [(&str, &str); 3] = [("A", "B"), ("C", "C"), ("Default", "C")];

    fn collide(a: &str, b: &str) -> bool {
        !APART.contains(&(a, b)) && !APART.contains(&(b, a))
    }

    struct Part {
        name: String,
        folder: usize,
        geom: PartGeom,
        pose: Pose,
        group: &'static str,
        can_collide: bool,
        can_query: bool,
    }

    /// Builds a scene of random parts in four folders and returns them as
    /// placed (read back from the world).
    fn scattered(p: &mut Place, rng: &mut Rng, n: usize) -> Vec<Part> {
        let mut s = String::from(
            r#"
            local PS = game:GetService("PhysicsService")
            for _, g in {"A", "B", "C"} do PS:RegisterCollisionGroup(g) end
            "#,
        );
        for (a, b) in APART {
            s += &format!("PS:CollisionGroupSetCollidable({a:?}, {b:?}, false)\n");
        }
        s += "local folders = {}\nfor i = 0, 3 do local f = Instance.new('Folder') f.Name = 'F' .. i f.Parent = workspace folders[i] = f end\n";
        for i in 0..n {
            let shape = *rng.pick(&["Block", "Ball", "Cylinder", "Wedge", "CornerWedge"]);
            let size = rng.vec(0.5, 6.0);
            let pos = rng.vec(-25.0, 25.0);
            let axis = rng.vec(-1.0, 1.0).normalize_or_zero();
            let angle = rng.range(0.0, std::f32::consts::TAU);
            let group = *rng.pick(&GROUPS);
            let can_collide = rng.next() < 0.8;
            let can_query = rng.next() < 0.7;
            let anchored = rng.next() < 0.7;
            let folder = (rng.next() * 4.0) as usize % 4;
            let class = if shape == "CornerWedge" {
                "CornerWedgePart"
            } else {
                "Part"
            };
            s += &format!(
                "do local x = Instance.new({class:?}) x.Name = 'P{i}' {shape} x.Size = Vector3.new({}, {}, {}) x.CFrame = CFrame.new({}, {}, {}) * CFrame.fromAxisAngle(Vector3.new({}, {}, {}), {angle}) x.CollisionGroup = {group:?} x.CanCollide = {can_collide} x.CanQuery = {can_query} x.Anchored = {anchored} x.Parent = folders[{folder}] end\n",
                size.x,
                size.y,
                size.z,
                pos.x,
                pos.y,
                pos.z,
                if axis == Vec3::ZERO { 1.0 } else { axis.x },
                axis.y,
                axis.z,
                shape = if class == "Part" {
                    format!("x.Shape = Enum.PartType.{shape}")
                } else {
                    String::new()
                },
            );
        }
        p.run(&s);
        let names = p.by_name();
        (0..n)
            .map(|i| {
                let name = format!("P{i}");
                let e = names[&name];
                let shape = if p.get(e, "ClassName") == Value::Str("CornerWedgePart".into()) {
                    PartShape::CornerWedge
                } else {
                    match p.get(e, "Shape") {
                        Value::Enum(s) => match s.as_str() {
                            "Ball" => PartShape::Ball,
                            "Cylinder" => PartShape::Cylinder,
                            "Wedge" => PartShape::Wedge,
                            _ => PartShape::Block,
                        },
                        _ => PartShape::Block,
                    }
                };
                let Value::Vec3(size) = p.get(e, "Size") else {
                    panic!()
                };
                let pose = physics::pose_of(&p.get(e, "CFrame")).unwrap();
                let Value::Str(group) = p.get(e, "CollisionGroup") else {
                    panic!()
                };
                let folder = match p.get(e, "Parent") {
                    Value::Entity(Some(f)) => re_compat::instance::tree::name(&p.world, f)[1..]
                        .parse()
                        .unwrap(),
                    _ => panic!(),
                };
                Part {
                    name,
                    folder,
                    geom: PartGeom::new(shape, size).sanitized(),
                    pose,
                    group: GROUPS.iter().find(|g| **g == group).unwrap(),
                    can_collide: p.get(e, "CanCollide") == Value::Bool(true),
                    can_query: p.get(e, "CanQuery") == Value::Bool(true),
                }
            })
            .collect()
    }

    /// A random filter: (Lua that makes `params`, the predicate it means).
    struct Params {
        lua: String,
        folders: BTreeSet<usize>,
        include: bool,
        group: &'static str,
        respect: bool,
    }

    impl Params {
        fn random(rng: &mut Rng, ty: &str) -> Params {
            let folders: BTreeSet<usize> = (0..4).filter(|_| rng.next() < 0.3).collect();
            let include = rng.next() < 0.3;
            let group = *rng.pick(&GROUPS);
            let respect = rng.next() < 0.3;
            let list = folders
                .iter()
                .map(|f| format!("workspace.F{f}"))
                .collect::<Vec<_>>()
                .join(", ");
            let lua = format!(
                "local params = {ty}.new() params.FilterDescendantsInstances = {{{list}}} params.FilterType = Enum.RaycastFilterType.{} params.CollisionGroup = {group:?} params.RespectCanCollide = {respect}",
                if include { "Include" } else { "Exclude" }
            );
            Params {
                lua,
                folders,
                include,
                group,
                respect,
            }
        }

        fn sees(&self, part: &Part) -> bool {
            (part.can_collide || part.can_query)
                && collide(self.group, part.group)
                && !(self.respect && !part.can_collide)
                && self.folders.contains(&part.folder) == self.include
        }
    }

    #[test]
    fn raycasts_match_brute_force() {
        let mut p = place();
        let mut rng = Rng(11);
        let parts = scattered(&mut p, &mut rng, 400);
        let mut checked = 0;
        let mut hits = 0;
        let mut grazing = 0;
        for chunk in 0..10 {
            let mut s = String::new();
            let mut cases = Vec::new();
            for i in 0..100 {
                let params = Params::random(&mut rng, "RaycastParams");
                let origin = rng.vec(-35.0, 35.0);
                // Most rays aim at a part; the rest go anywhere.
                let dir = if i % 3 == 0 {
                    rng.vec(-1.0, 1.0).normalize_or_zero() * rng.range(10.0, 120.0)
                } else {
                    let target = rng.pick(&parts).pose.pos;
                    (target - origin) * 1.5
                };
                s += &format!(
                    "do {} local r = workspace:Raycast(Vector3.new({}, {}, {}), Vector3.new({}, {}, {}), params) if r then print({i}, r.Instance.Name, r.Distance) else print({i}, 'nil', 0) end end\n",
                    params.lua, origin.x, origin.y, origin.z, dir.x, dir.y, dir.z
                );
                cases.push((params, origin, dir));
            }
            p.run(&s);
            let out = p.out();
            assert_eq!(out.len(), 100, "chunk {chunk}");
            for (line, (params, origin, dir)) in out.iter().zip(&cases) {
                let f: Vec<&str> = line.split('\t').collect();
                let (name, dist): (&str, f32) = (f[1], f[2].parse().unwrap());
                let len = dir.length();
                let unit = *dir / len;
                let mut best: Option<(f32, &str)> = None;
                for part in parts.iter().filter(|x| params.sees(x)) {
                    let o = part.pose.inv_point(*origin);
                    let d = part.pose.rot.inverse() * unit;
                    if let Some((t, _)) = part.geom.ray_entry(o, d, len)
                        && best.is_none_or(|(b, _)| t < b)
                    {
                        best = Some((t, &part.name));
                    }
                }
                // A ray that grazes an edge, or touches a slope at one point,
                // may round either way.
                let grazes = |name: &str, t: f32| {
                    parts.iter().find(|x| x.name == name).is_some_and(|x| {
                        let at = Pose::new(*origin + unit * t, Quat::IDENTITY);
                        let speck =
                            Placed::new(PartGeom::new(PartShape::Ball, Vec3::splat(2e-3)), &at);
                        params.sees(x)
                            && gjk_distance(&Placed::new(x.geom, &x.pose), &speck).distance < 2e-3
                    })
                };
                match best {
                    None if name != "nil" && grazes(name, dist) => {
                        grazing += 1;
                        continue;
                    }
                    Some((t, want)) if name == "nil" && grazes(want, t) => {
                        grazing += 1;
                        continue;
                    }
                    _ => {}
                }
                match best {
                    None => assert_eq!(name, "nil", "{line}"),
                    Some((t, want)) => {
                        assert!((t - dist).abs() < 1e-3, "{line}: brute force {want} at {t}");
                        if name != want {
                            // A tie between two surfaces at the same distance.
                            assert!((t - dist).abs() < 1e-4, "{line}: brute force {want}");
                        }
                        hits += 1;
                    }
                }
                checked += 1;
            }
        }
        assert_eq!(checked + grazing, 1000);
        assert!(grazing <= 5, "{grazing} grazing rays");
        assert!(hits > 300, "{hits} hits");
    }

    #[test]
    fn blockcasts_match_brute_force() {
        use re_physics::parts::geom::shape_cast;
        let mut p = place();
        let mut rng = Rng(31);
        let parts = scattered(&mut p, &mut rng, 400);
        let mut hits = 0;
        let mut grazing = 0;
        for chunk in 0..3 {
            let mut s = String::new();
            let mut cases = Vec::new();
            for i in 0..100 {
                let params = Params::random(&mut rng, "RaycastParams");
                let from = rng.vec(-35.0, 35.0);
                let size = rng.vec(0.5, 4.0);
                let axis = rng.vec(-1.0, 1.0).normalize_or(Vec3::Y);
                let angle = rng.range(0.0, std::f32::consts::TAU);
                let target = rng.pick(&parts).pose.pos;
                let dir = (target - from) * 1.5;
                s += &format!(
                    "do {} local r = workspace:Blockcast(CFrame.new({}, {}, {}) * CFrame.fromAxisAngle(Vector3.new({}, {}, {}), {angle}), Vector3.new({}, {}, {}), Vector3.new({}, {}, {}), params) if r then print({i}, r.Instance.Name, r.Distance) else print({i}, 'nil', 0) end end\n",
                    params.lua,
                    from.x,
                    from.y,
                    from.z,
                    axis.x,
                    axis.y,
                    axis.z,
                    size.x,
                    size.y,
                    size.z,
                    dir.x,
                    dir.y,
                    dir.z
                );
                let pose = Pose::new(from, Quat::from_axis_angle(axis.normalize(), angle));
                cases.push((params, pose, size, dir));
            }
            p.run(&s);
            let out = p.out();
            assert_eq!(out.len(), 100, "chunk {chunk}");
            for (line, (params, pose, size, dir)) in out.iter().zip(&cases) {
                let f: Vec<&str> = line.split('\t').collect();
                let (name, dist): (&str, f32) = (f[1], f[2].parse().unwrap());
                let block = Placed::new(PartGeom::new(PartShape::Block, *size).sanitized(), pose);
                let mut best: Option<(f32, &str)> = None;
                for part in parts.iter().filter(|x| params.sees(x)) {
                    let target = Placed::new(part.geom, &part.pose);
                    if let Some((frac, _, _)) = shape_cast(&block, &target, dir.as_dvec3(), 1e-4) {
                        let t = (frac * dir.as_dvec3().length()) as f32;
                        if best.is_none_or(|(b, _)| t < b) {
                            best = Some((t, &part.name));
                        }
                    }
                }
                match (best, name) {
                    (None, "nil") => {}
                    (Some((t, want)), got) if got == want || (t - dist).abs() < 1e-3 => {
                        assert!((t - dist).abs() < 1e-2, "{line}: brute force {want} at {t}");
                        hits += 1;
                    }
                    // A cast that only grazes a face or edge may round either way.
                    _ => grazing += 1,
                }
            }
        }
        assert!(grazing <= 3, "{grazing} grazing casts");
        assert!(hits > 100, "{hits} hits");
    }

    #[test]
    fn region_queries_match_brute_force() {
        let mut p = place();
        let mut rng = Rng(23);
        let parts = scattered(&mut p, &mut rng, 400);
        let mut found = 0;
        for chunk in 0..10 {
            let mut s = String::new();
            let mut cases = Vec::new();
            for i in 0..100 {
                let params = Params::random(&mut rng, "OverlapParams");
                let center = rng.vec(-45.0, 45.0);
                let is_box = i % 2 == 0;
                if is_box {
                    let size = rng.vec(1.0, 20.0);
                    let axis = rng.vec(-1.0, 1.0).normalize_or(Vec3::Y);
                    let angle = rng.range(0.0, std::f32::consts::TAU);
                    s += &format!(
                        "do {} local l = workspace:GetPartBoundsInBox(CFrame.new({}, {}, {}) * CFrame.fromAxisAngle(Vector3.new({}, {}, {}), {angle}), Vector3.new({}, {}, {}), params) local n = {{}} for _, x in l do table.insert(n, x.Name) end table.sort(n) print(table.concat(n, ',')) end\n",
                        params.lua,
                        center.x,
                        center.y,
                        center.z,
                        axis.x,
                        axis.y,
                        axis.z,
                        size.x,
                        size.y,
                        size.z
                    );
                    // The script's CFrame is rebuilt from the same numbers.
                    let pose = Pose::new(center, Quat::from_axis_angle(axis.normalize(), angle));
                    cases.push((params, Shape::Box(pose, size)));
                } else {
                    let r = rng.range(0.5, 12.0);
                    s += &format!(
                        "do {} local l = workspace:GetPartBoundsInRadius(Vector3.new({}, {}, {}), {r}, params) local n = {{}} for _, x in l do table.insert(n, x.Name) end table.sort(n) print(table.concat(n, ',')) end\n",
                        params.lua, center.x, center.y, center.z
                    );
                    cases.push((params, Shape::Sphere(center, r)));
                }
            }
            p.run(&s);
            let out = p.out();
            assert_eq!(out.len(), 100, "chunk {chunk}");
            for (line, (params, shape)) in out.iter().zip(&cases) {
                let got: BTreeSet<&str> = line.split(',').filter(|x| !x.is_empty()).collect();
                let want: BTreeSet<&str> = parts
                    .iter()
                    .filter(|x| params.sees(x))
                    .filter(|x| match shape {
                        Shape::Box(pose, size) => {
                            obb_overlap(&x.pose, x.geom.half(), pose, *size * 0.5)
                        }
                        Shape::Sphere(c, r) => obb_sphere_overlap(&x.pose, x.geom.half(), *c, *r),
                    })
                    .map(|x| x.name.as_str())
                    .collect();
                // Rounding at a touching face may differ; nothing else may.
                let diff: Vec<_> = got.symmetric_difference(&want).collect();
                assert!(diff.len() <= 1, "{line} vs {want:?}");
                found += want.len();
            }
        }
        assert!(found > 500, "{found}");
    }

    enum Shape {
        Box(Pose, Vec3),
        Sphere(Vec3, f32),
    }

    #[test]
    fn casts_and_parts_in_part_see_the_scene() {
        let mut p = place();
        p.run(
            r#"
            local wall = Instance.new("Part")
            wall.Name = "Wall"
            wall.Anchored = true
            wall.Size = Vector3.new(1, 10, 10)
            wall.Position = Vector3.new(20, 0, 0)
            wall.Parent = workspace
            local r = workspace:Blockcast(CFrame.new(0, 0, 0), Vector3.new(2, 2, 2), Vector3.new(50, 0, 0))
            print(r.Instance.Name, r.Distance, r.Normal, r.Material)
            r = workspace:Spherecast(Vector3.new(0, 0, 0), 1, Vector3.new(50, 0, 0))
            print(r.Instance.Name, r.Distance)
            local probe = Instance.new("Part")
            probe.Size = Vector3.new(2, 2, 2)
            probe.Position = Vector3.new(0, 0, 0)
            probe.Anchored = true
            probe.Parent = workspace
            r = workspace:Shapecast(probe, Vector3.new(50, 0, 0))
            print(r.Instance.Name, r.Distance)
            probe.Position = Vector3.new(20.5, 0, 0)
            local inside = workspace:GetPartsInPart(probe)
            print(#inside, inside[1].Name)
            print(typeof(r), workspace:Raycast(Vector3.new(0, 0, 0), Vector3.new(0, 50, 0)))
            "#,
        );
        let out: Vec<Vec<String>> = p
            .out()
            .iter()
            .map(|l| l.split('\t').map(String::from).collect())
            .collect();
        for line in &out[..3] {
            assert_eq!(line[0], "Wall", "{out:?}");
            let d: f32 = line[1].parse().unwrap();
            assert!((d - 18.5).abs() < 1e-3, "{out:?}");
        }
        assert!(out[0][2].starts_with("vector(-1,"), "{out:?}");
        assert_eq!(out[0][3], "Enum.Material.Plastic", "{out:?}");
        assert_eq!(out[3].join(" "), "1 Wall", "{out:?}");
        assert_eq!(out[4].join(" "), "RaycastResult nil", "{out:?}");
    }
}

mod physics_bigmap_load {
    use super::*;

    /// The large-map path at a small size: the scene file round-trips, every
    /// anchored part lands in a static cell, and stepping moves nothing.
    #[test]
    fn a_small_map_loads_into_static_cells_and_stays_put() {
        let setup = CompatSetup::standard().unwrap();
        let map = bigmap::generate(5_000, 3);
        let text = re_scene::scene::to_json(&bigmap::scene(&map, &setup.classes).unwrap());
        let doc = re_scene::scene::from_json(&text).unwrap();
        let mut world = World::new();
        setup.classes.register_components(&mut world).unwrap();
        world.load(&doc).unwrap();
        physics::prepare_world(&mut world, &setup.classes).unwrap();
        assert!(physics::step(&mut world, 0.0).is_empty());
        let cells = world.ecs.resource::<physics::StaticCells>().cells().len();
        assert!(cells > 1, "{cells} cells");
        let simulated = physics::with_physics(&mut world, |p| p.part_count()).unwrap_or_default();
        assert_eq!(simulated, 5_000);
        let mut q = world.ecs.query::<(Entity, &re_scene::Transform)>();
        let parts: Vec<Entity> = q.iter(&world.ecs).map(|(e, _)| e).collect();
        let before: Vec<_> = parts
            .iter()
            .map(|e| physics::world_pose(&world, *e))
            .collect();
        for _ in 0..30 {
            assert!(physics::step(&mut world, DT).is_empty());
        }
        let after: Vec<_> = parts
            .iter()
            .map(|e| physics::world_pose(&world, *e))
            .collect();
        assert_eq!(before, after);
    }
}
