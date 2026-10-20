//! The data model as scripts see it: instance handles, properties, methods,
//! attributes and tags, signals (deferred and immediate), `WaitForChild`,
//! `Clone`, `Destroy` and `GetService`.

use std::sync::Arc;

use re_compat::scene::create_place;
use re_compat::{CompatSetup, CompatVm, SignalBehavior};
use re_scene::{TagIndex, World};
use re_script::host::{NoServices, WorldHost};
use re_script::{ContextKind, RuntimeConfig};

struct Place {
    world: World,
    services: NoServices,
    vm: CompatVm,
    _dir: tempfile::TempDir,
}

fn place(behavior: SignalBehavior) -> Place {
    let setup: Arc<CompatSetup> = CompatSetup::standard().unwrap();
    let mut world = World::new();
    create_place(
        &mut world,
        &setup.classes,
        &["Workspace", "ReplicatedStorage", "ServerScriptService"],
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let vm = CompatVm::new(
        &setup,
        ContextKind::Server,
        &mut world,
        &RuntimeConfig::new(dir.path()),
        behavior,
    )
    .unwrap();
    Place {
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

    fn step(&mut self, dt: f64) {
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = self.vm.step(&mut host, dt);
        assert!(d.is_empty(), "{d:#?}");
    }

    /// The printed lines since the last call; warnings start with `W `.
    fn out(&mut self) -> Vec<String> {
        self.vm
            .take_output()
            .into_iter()
            .map(|l| {
                if l.warn {
                    format!("W {}", l.text)
                } else {
                    l.text
                }
            })
            .collect()
    }
}

// Grouped so `cargo test -p re_compat model` selects them.
mod model {
    use super::*;

    #[test]
    fn properties_read_write_and_unknown_members_raise_re9001() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local f = Instance.new("Folder")
            assert(f.Name == "Folder" and f.ClassName == "Folder" and f.Parent == nil)
            f.Name = "Stuff"
            f.Parent = workspace
            assert(workspace.Stuff == f)
            assert(f.Parent == workspace and f.Archivable == true)
            assert(typeof(f) == "Instance", typeof(f))
            assert(tostring(f) == "Stuff")
            local ok, err = pcall(function() return f.Nmae end)
            assert(not ok)
            print(err)
            local ok2, err2 = pcall(function() f.ClassName = "Model" end)
            assert(not ok2)
            local ok3, err3 = pcall(function() return workspace.Missing end)
            print(err3)
            "#,
        );
        let out = p.out();
        assert!(
            out[0].contains("[RE9001]")
                && out[0].contains("Nmae is not a valid member of Folder \"Workspace.Stuff\""),
            "{out:?}"
        );
        assert!(
            out[1].contains("[RE9001]")
                && out[1].contains("Missing is not a valid member of Workspace \"Workspace\""),
            "{out:?}"
        );
    }

    #[test]
    fn tree_methods_find_and_list_instances() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local a = Instance.new("Folder", workspace); a.Name = "A"
            local b = Instance.new("Model", a); b.Name = "B"
            local c = Instance.new("Configuration", b); c.Name = "C"
            local d = Instance.new("Folder", a); d.Name = "D"
            assert(a:FindFirstChild("C") == nil)
            assert(a:FindFirstChild("C", true) == c)
            assert(a:FindFirstDescendant("C") == c)
            assert(a:FindFirstChildOfClass("Model") == b)
            assert(a:FindFirstChildWhichIsA("PVInstance") == b)
            assert(c:FindFirstAncestor("A") == a)
            assert(c:FindFirstAncestorOfClass("Folder") == a)
            assert(c:FindFirstAncestorWhichIsA("PVInstance") == b)
            local kids = a:GetChildren()
            assert(#kids == 2 and kids[1] == b and kids[2] == d)
            local all = a:GetDescendants()
            assert(#all == 3 and all[1] == b and all[2] == c and all[3] == d)
            assert(c:GetFullName() == "Workspace.A.B.C", c:GetFullName())
            assert(c:IsA("Instance") and c:IsA("Configuration") and not c:IsA("Folder"))
            assert(c:IsDescendantOf(a) and a:IsAncestorOf(c) and not a:IsDescendantOf(c))
            assert(#a:children() == 2)
            local ok, err = pcall(Instance.new, "Workspace")
            assert(not ok); print(err)
            local ok2, err2 = pcall(Instance.new, "Nope")
            assert(not ok2); print(err2)
            local ok3, err3 = pcall(function() a.Parent = c end)
            assert(not ok3); print(err3)
            "#,
        );
        let out = p.out();
        assert!(out[0].contains("[RE3006]"), "{out:?}");
        assert!(out[1].contains("[RE9001]"), "{out:?}");
        assert!(out[2].contains("[RE9008]"), "{out:?}");
    }

    #[test]
    fn get_service_finds_or_makes_services() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            assert(game:GetService("Workspace") == workspace)
            assert(game.Workspace == workspace and Workspace == workspace and Game == game)
            assert(game:GetService("ReplicatedStorage") == game.ReplicatedStorage)
            assert(game:FindService("ServerStorage") == nil)
            local ss = game:GetService("ServerStorage")
            assert(ss.Parent == game and ss.ClassName == "ServerStorage")
            assert(game:GetService("ServerStorage") == ss)
            assert(game:FindService("ServerStorage") == ss)
            local sp = game:GetService("StarterPlayer")
            assert(sp:FindFirstChild("StarterPlayerScripts") and sp:FindFirstChild("StarterCharacterScripts"))
            local cs = game:GetService("CollectionService")
            assert(game.CollectionService == cs)
            local ok, err = pcall(function() return game:GetService("Folder") end)
            assert(not ok); print(err)
            "#,
        );
        let out = p.out();
        assert!(out[0].contains("[RE9001]"), "{out:?}");
    }

    #[test]
    fn user_settings_has_its_own_game_settings() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local us = UserSettings()
            assert(us == UserSettings() and us.ClassName == "UserSettings" and us.Parent == nil)
            print(us:IsUserFeatureEnabled("SomeFeature"))
            local gs = us:GetService("UserGameSettings")
            assert(gs == us:GetService("UserGameSettings") and gs.Parent == us)
            print(gs.SavedQualityLevel == Enum.SavedQualitySetting.Automatic)
            print(pcall(function() return game:GetService("UserGameSettings") end))
            print(pcall(function() return us:GetService("Workspace") end))
            print(pcall(function() us.Parent = workspace end))
            print(pcall(function() us:Destroy() end))
            assert(UserSettings() == us and us.Parent == nil)
            "#,
        );
        let out = p.out();
        assert_eq!(out[..2], ["false", "true"], "{out:?}");
        assert!(
            out[2].starts_with("false") && out[2].contains("[RE9001]"),
            "{out:?}"
        );
        assert!(
            out[3].starts_with("false") && out[3].contains("[RE9001]"),
            "{out:?}"
        );
    }

    #[test]
    fn deferred_signals_run_after_the_writing_code() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local f = Instance.new("Folder", workspace)
            f.ChildAdded:Connect(function(c) print("added " .. c.Name) end)
            local x = Instance.new("Folder"); x.Name = "X"
            x.Parent = f
            print("after write")
            "#,
        );
        assert_eq!(p.out(), ["after write", "added X"]);
    }

    #[test]
    fn immediate_signals_run_inside_the_write() {
        let mut p = place(SignalBehavior::Immediate);
        p.run(
            r#"
            local f = Instance.new("Folder", workspace)
            f.ChildAdded:Connect(function(c) print("added " .. c.Name) end)
            local x = Instance.new("Folder"); x.Name = "X"
            x.Parent = f
            print("after write")
            "#,
        );
        assert_eq!(p.out(), ["added X", "after write"]);
    }

    #[test]
    fn disconnect_while_firing_stops_later_handlers() {
        for behavior in [SignalBehavior::Deferred, SignalBehavior::Immediate] {
            let mut p = place(behavior);
            p.run(
                r#"
                local f = Instance.new("Folder", workspace)
                local second
                f.ChildAdded:Connect(function()
                    print("first")
                    second:Disconnect()
                end)
                second = f.ChildAdded:Connect(function() print("second") end)
                local once = 0
                f.ChildAdded:Once(function() once += 1 end)
                Instance.new("Folder", f)
                Instance.new("Folder", f)
                task.wait()
                print("once " .. once, second.Connected)
                "#,
            );
            p.step(1.0 / 60.0);
            assert_eq!(p.out(), ["first", "first", "once 1\tfalse"], "{behavior:?}");
        }
    }

    #[test]
    fn wait_resumes_across_steps() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local f = Instance.new("Folder", workspace); f.Name = "F"
            local child = f.ChildAdded:Wait()
            print("got " .. child.Name)
            "#,
        );
        assert_eq!(p.vm.parked().len(), 1);
        p.step(1.0);
        assert!(p.out().is_empty());
        p.run(
            r#"
            local c = Instance.new("Folder"); c.Name = "Late"
            c.Parent = workspace.F
            "#,
        );
        assert_eq!(p.out(), ["got Late"]);
        assert!(p.vm.parked().is_empty());
    }

    #[test]
    fn wait_for_child_warns_once_after_five_seconds() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local x = workspace:WaitForChild("Late")
            print("found " .. x.Name)
            "#,
        );
        p.step(4.9);
        assert!(p.out().is_empty());
        p.step(0.2);
        assert_eq!(
            p.out(),
            ["W Infinite yield possible on 'Workspace:WaitForChild(\"Late\")'"]
        );
        p.step(10.0);
        assert!(p.out().is_empty());
        p.run(r#"local f = Instance.new("Folder"); f.Name = "Late"; f.Parent = workspace"#);
        assert_eq!(p.out(), ["found Late"]);
    }

    #[test]
    fn wait_for_child_returns_at_once_times_out_and_sees_same_frame_children() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local here = Instance.new("Folder", workspace); here.Name = "Here"
            assert(workspace:WaitForChild("Here") == here)
            print("timeout", workspace:WaitForChild("Never", 2))
            "#,
        );
        p.run(
            r#"
            print("soon", workspace:WaitForChild("Soon").Name)
            "#,
        );
        p.run(r#"local f = Instance.new("Folder"); f.Name = "Soon"; f.Parent = workspace"#);
        assert_eq!(p.out(), ["soon\tSoon"]);
        p.step(1.0);
        assert!(p.out().is_empty());
        p.step(1.5);
        assert_eq!(p.out(), ["timeout\tnil"]);
    }

    #[test]
    fn attributes_and_property_signals() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local f = Instance.new("Folder", workspace)
            f:SetAttribute("Speed", 10)
            f:SetAttribute("Label", "fast")
            assert(f:GetAttribute("Speed") == 10 and f:GetAttribute("Nope") == nil)
            local all = f:GetAttributes()
            assert(all.Speed == 10 and all.Label == "fast")
            f.AttributeChanged:Connect(function(n) print("attr " .. n) end)
            f:GetAttributeChangedSignal("Speed"):Connect(function() print("speed " .. f:GetAttribute("Speed")) end)
            f.Changed:Connect(function(n) print("changed " .. n) end)
            f:GetPropertyChangedSignal("Name"):Connect(function() print("name " .. f.Name) end)
            f:SetAttribute("Speed", 20)
            f.Name = "Renamed"
            local ok, err = pcall(function() f:SetAttribute("RBX_bad", 1) end)
            assert(not ok); print(err)
            local ok2, err2 = pcall(function() f:SetAttribute("Fn", function() end) end)
            assert(not ok2); print(err2)
            "#,
        );
        let out = p.out();
        assert!(out[0].contains("[RE9007]"), "{out:?}");
        assert!(out[1].contains("[RE9007]"), "{out:?}");
        let rest: Vec<&str> = out[2..].iter().map(String::as_str).collect();
        assert_eq!(
            rest,
            ["attr Speed", "speed 20", "changed Name", "name Renamed"]
        );
    }

    #[test]
    fn tree_signals_fire_for_descendants_and_ancestry() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local root = Instance.new("Folder", workspace); root.Name = "Root"
            local mid = Instance.new("Folder", root); mid.Name = "Mid"
            root.DescendantAdded:Connect(function(d) print("desc+ " .. d.Name) end)
            root.DescendantRemoving:Connect(function(d) print("desc- " .. d.Name) end)
            mid.ChildRemoved:Connect(function(c) print("child- " .. c.Name) end)
            local box = Instance.new("Model"); box.Name = "Box"
            local inner = Instance.new("Folder", box); inner.Name = "Inner"
            inner.AncestryChanged:Connect(function(c, p) print("anc " .. c.Name .. " " .. tostring(p)) end)
            box.Parent = mid
            task.wait()
            box.Parent = nil
            "#,
        );
        p.step(1.0 / 60.0);
        assert_eq!(
            p.out(),
            [
                "desc+ Box",
                "desc+ Inner",
                "anc Box Mid",
                "child- Box",
                "desc- Box",
                "desc- Inner",
                "anc Box nil",
            ]
        );
    }

    #[test]
    fn clone_and_destroy_keep_tags_and_attributes_exact() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local cs = game:GetService("CollectionService")
            cs:GetInstanceAddedSignal("Coin"):Connect(function(i) print("coin+ " .. i.Name) end)
            cs:GetInstanceRemovedSignal("Coin"):Connect(function(i) print("coin- " .. i.Name) end)
            local m = Instance.new("Model", workspace); m.Name = "M"
            m:SetAttribute("Value", 5)
            m:AddTag("Coin")
            local part = Instance.new("Folder", m); part.Name = "Part"
            part:AddTag("Coin"); part:AddTag("Shiny")
            local secret = Instance.new("Folder", m); secret.Name = "Secret"
            secret.Archivable = false
            task.wait()
            local copy = m:Clone()
            assert(copy.Parent == nil and copy ~= m)
            assert(copy:GetAttribute("Value") == 5 and copy:HasTag("Coin"))
            assert(copy:FindFirstChild("Secret") == nil)
            local cp = copy:FindFirstChild("Part")
            assert(cp:HasTag("Shiny") and #cp:GetTags() == 2)
            copy.Name = "Copy"; cp.Name = "CopyPart"
            copy.Parent = workspace
            task.wait()
            print("tagged " .. #cs:GetTagged("Coin"))
            m.Destroying:Connect(function() print("destroying " .. m.Name) end)
            workspace.ChildRemoved:Connect(function(c) print("removed " .. c.Name .. " " .. c.ClassName) end)
            m:Destroy()
            print("parent", m.Parent, m.Name)
            local ok, err = pcall(function() m.Parent = workspace end)
            assert(not ok); print(err)
            task.wait()
            print("tagged " .. #cs:GetTagged("Coin"))
            assert(cs:HasTag(copy, "Coin") and not cs:HasTag(m, "Coin"))
            "#,
        );
        for _ in 0..3 {
            p.step(1.0 / 60.0);
        }
        let out = p.out();
        assert_eq!(
            out[..5],
            [
                "coin+ M",
                "coin+ Part",
                "coin+ Copy",
                "coin+ CopyPart",
                "tagged 4"
            ],
            "{out:?}"
        );
        assert_eq!(out[5], "parent\tnil\tM", "{out:?}");
        assert!(out[6].contains("[RE9008]"), "{out:?}");
        let mut rest: Vec<&str> = out[7..].iter().map(String::as_str).collect();
        let tail = rest.split_off(rest.len() - 1);
        assert_eq!(tail, ["tagged 2"], "{out:?}");
        assert_eq!(rest[0], "destroying M", "{out:?}");
        let mut events = rest[1..].to_vec();
        events.sort_unstable();
        assert_eq!(
            events,
            ["coin- M", "coin- Part", "removed M Model"],
            "{out:?}"
        );

        // The world's tag index agrees with what scripts saw.
        let index = p.world.ecs.resource::<TagIndex>();
        assert_eq!(index.tagged("Coin").count(), 2);
        assert_eq!(index.tagged("Shiny").count(), 1);
    }

    #[test]
    fn print_shows_instances_and_tables() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(r#"print(workspace, game.ReplicatedStorage) print(type({}))"#);
        assert_eq!(p.out(), ["Workspace\tReplicatedStorage", "table"]);
    }

    #[test]
    fn deferred_handlers_still_read_a_destroyed_instance() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local f = Instance.new("Folder", workspace)
            local c = Instance.new("Folder", f); c.Name = "C"
            c:SetAttribute("Id", 7)
            f.ChildRemoved:Connect(function(x)
                print(x.Name, x:GetAttribute("Id"), x.Archivable, x.Parent)
            end)
            c:Destroy()
            c:Destroy() -- a second Destroy does nothing
            task.wait()
            print(c.Name, c.ClassName)
            "#,
        );
        p.step(1.0 / 60.0);
        assert_eq!(p.out(), ["C\t7\ttrue\tnil", "C\tFolder"]);
    }

    #[test]
    fn game_and_services_are_locked() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            for _, f in {
                function() workspace.Parent = nil end,
                function() workspace.Parent = game.ReplicatedStorage end,
                function() workspace:Destroy() end,
                function() game:Destroy() end,
                function() game.Parent = workspace end,
            } do
                local ok, err = pcall(f)
                assert(not ok)
                assert(string.find(tostring(err), "RE9008"), tostring(err))
            end
            workspace.Parent = game -- unchanged: allowed
            assert(game.Workspace == workspace and workspace.Parent == game)
            print("ok")
            "#,
        );
        assert_eq!(p.out(), ["ok"]);
    }

    #[test]
    fn wait_for_child_with_an_infinite_timeout_never_times_out_or_warns() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(r#"print("got", workspace:WaitForChild("X", math.huge).Name)"#);
        p.step(1.0);
        p.step(100.0);
        assert!(p.out().is_empty());
        p.run(r#"local x = Instance.new("Folder"); x.Name = "X"; x.Parent = workspace"#);
        assert_eq!(p.out(), ["got\tX"]);
    }

    #[test]
    fn collection_service_counts_only_instances_in_the_place() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local cs = game:GetService("CollectionService")
            cs:GetInstanceAddedSignal("T"):Connect(function(i) print("+" .. i.Name) end)
            cs:GetInstanceRemovedSignal("T"):Connect(function(i) print("-" .. i.Name) end)
            local a = Instance.new("Folder"); a.Name = "A"
            a:AddTag("T")
            local inner = Instance.new("Folder", a); inner.Name = "Inner"
            inner:AddTag("T")
            task.wait()
            print("tagged " .. #cs:GetTagged("T"))
            a.Parent = workspace
            task.wait()
            print("tagged " .. #cs:GetTagged("T"))
            local b = Instance.new("Folder", workspace); b.Name = "B"
            a.Parent = b -- moving inside the place changes nothing
            task.wait()
            a.Parent = nil
            task.wait()
            print("tagged " .. #cs:GetTagged("T"))
            "#,
        );
        for _ in 0..4 {
            p.step(1.0 / 60.0);
        }
        assert_eq!(
            p.out(),
            [
                "tagged 0", "+A", "+Inner", "tagged 2", "-A", "-Inner", "tagged 0"
            ]
        );
    }

    #[test]
    fn moving_an_instance_fires_on_both_parents() {
        let mut p = place(SignalBehavior::Deferred);
        p.run(
            r#"
            local a = Instance.new("Folder", workspace); a.Name = "A"
            local b = Instance.new("Folder", workspace); b.Name = "B"
            local x = Instance.new("Folder", a); x.Name = "X"
            a.ChildRemoved:Connect(function(c) print("A- " .. c.Name) end)
            b.ChildAdded:Connect(function(c) print("B+ " .. c.Name) end)
            x.Parent = b
            "#,
        );
        assert_eq!(p.out(), ["A- X", "B+ X"]);
    }
}
