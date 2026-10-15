//! Two worlds: replication, remotes and bindables, players and characters,
//! and the extra services. `cargo test -p re_compat net` runs them.

use std::sync::Arc;

use re_compat::net::pump;
use re_compat::players::{self, PlayerSpec};
use re_compat::scene::create_place;
use re_compat::{CompatSetup, CompatVm, CompatWorlds, SignalBehavior};
use re_scene::{Entity, World};
use re_script::host::{NoServices, WorldHost};
use re_script::{ContextKind, RuntimeConfig};

const DT: f64 = 1.0 / 60.0;

/// A server and a client world with their VMs, run frame by frame in the
/// documented order.
struct Two {
    worlds: CompatWorlds,
    server: CompatVm,
    client: CompatVm,
    services: NoServices,
    errors: Vec<String>,
    _dir: tempfile::TempDir,
}

fn two() -> Two {
    two_with(CompatSetup::standard().unwrap())
}

/// Two worlds with the test `SpawnLocation` and `Humanoid` classes added.
fn two_with_spawns() -> Two {
    let mut b = re_compat::Bindings::standard().unwrap();
    b.classes(|c| {
        c.classes(
            "tests/fixtures/net/classes.json",
            re_compat::data::ClassFile::parse(
                "classes.json",
                include_str!("fixtures/net/classes.json"),
            )
            .unwrap(),
        )
    });
    two_with(CompatSetup::new(b).unwrap())
}

fn two_with(setup: Arc<CompatSetup>) -> Two {
    let mut world = World::new();
    create_place(
        &mut world,
        &setup.classes,
        &[
            "Workspace",
            "ReplicatedStorage",
            "ServerStorage",
            "ServerScriptService",
            "StarterGui",
            "Lighting",
        ],
    )
    .unwrap();
    let mut worlds = CompatWorlds::new(&setup, world).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig::new(dir.path());
    let server = CompatVm::new(
        &setup,
        ContextKind::Server,
        &mut worlds.server,
        &config,
        SignalBehavior::Deferred,
    )
    .unwrap();
    let client = CompatVm::new(
        &setup,
        ContextKind::Client,
        &mut worlds.client,
        &config,
        SignalBehavior::Deferred,
    )
    .unwrap();
    Two {
        worlds,
        server,
        client,
        services: NoServices::default(),
        errors: Vec::new(),
        _dir: dir,
    }
}

impl Two {
    fn keep(&mut self, d: Vec<re_core::Diagnostic>) {
        self.errors.extend(d.into_iter().map(|d| d.message));
    }

    /// Runs a server script.
    fn server(&mut self, source: &str) {
        let mut host = WorldHost {
            world: &mut self.worlds.server,
            services: &mut self.services,
        };
        let d = self.server.exec(&mut host, "server", source);
        self.keep(d);
    }

    /// Runs a client script.
    fn client(&mut self, source: &str) {
        let mut host = WorldHost {
            world: &mut self.worlds.client,
            services: &mut self.services,
        };
        let d = self.client.exec(&mut host, "client", source);
        self.keep(d);
    }

    /// One frame: pump the server, step it, replicate, pump the client, step
    /// it.
    fn frame(&mut self) {
        let mut host = WorldHost {
            world: &mut self.worlds.server,
            services: &mut self.services,
        };
        let mut d = pump(&self.server, &mut host);
        d.extend(self.server.step(&mut host, DT));
        self.keep(d);
        self.worlds.replicate();
        let mut host = WorldHost {
            world: &mut self.worlds.client,
            services: &mut self.services,
        };
        let mut d = pump(&self.client, &mut host);
        d.extend(self.client.step(&mut host, DT));
        self.keep(d);
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame();
        }
    }

    fn join(&mut self, spec: &PlayerSpec) -> Entity {
        let mut host = WorldHost {
            world: &mut self.worlds.server,
            services: &mut self.services,
        };
        let (p, d) = players::add_player(&self.server, &mut host, spec);
        self.keep(d);
        p.expect("the player joined")
    }

    fn leave(&mut self, p: Entity) {
        let mut host = WorldHost {
            world: &mut self.worlds.server,
            services: &mut self.services,
        };
        let (r, d) = players::remove_player(&self.server, &mut host, p);
        self.keep(d);
        r.expect("the player left");
    }

    fn say(&mut self, p: Entity, text: &str) {
        let mut host = WorldHost {
            world: &mut self.worlds.server,
            services: &mut self.services,
        };
        let (r, d) = players::say(&self.server, &mut host, p, text);
        self.keep(d);
        r.expect("the player spoke");
    }

    /// The server's and the client's printed lines since the last call.
    fn out(&mut self) -> (Vec<String>, Vec<String>) {
        let take = |vm: &CompatVm| -> Vec<String> {
            vm.take_output()
                .into_iter()
                .map(|l| {
                    if l.warn {
                        format!("W {}", l.text)
                    } else {
                        l.text
                    }
                })
                .collect()
        };
        (take(&self.server), take(&self.client))
    }

    fn no_errors(&mut self) {
        let e = std::mem::take(&mut self.errors);
        assert!(e.is_empty(), "{e:#?}");
    }
}

mod net {
    use super::*;

    #[test]
    fn properties_attributes_and_tags_replicate() {
        let mut t = two();
        t.server(
            r#"
            local f = Instance.new("Folder")
            f.Name = "Shared"
            f:SetAttribute("Score", 3)
            f:AddTag("Loot")
            f.Parent = game.ReplicatedStorage
            local hidden = Instance.new("Folder")
            hidden.Name = "Secret"
            hidden.Parent = game.ServerStorage
            "#,
        );
        t.frame();
        t.client(
            r#"
            local f = game.ReplicatedStorage:FindFirstChild("Shared")
            assert(f, "replicated")
            print(f:GetAttribute("Score"), f:HasTag("Loot"))
            print(#game:GetService("ServerStorage"):GetChildren())
            f.AttributeChanged:Connect(function(n) print("changed " .. n) end)
            f:GetPropertyChangedSignal("Name"):Connect(function() print("renamed " .. f.Name) end)
            "#,
        );
        t.server(
            r#"
            local f = game.ReplicatedStorage.Shared
            f:SetAttribute("Score", 4)
            f:RemoveTag("Loot")
            f.Name = "Shared2"
            "#,
        );
        t.frame();
        t.client(
            r#"
            local f = game.ReplicatedStorage:FindFirstChild("Shared2")
            print(f:GetAttribute("Score"), f:HasTag("Loot"))
            "#,
        );
        t.frame();
        t.no_errors();
        let (_, c) = t.out();
        assert_eq!(
            c,
            [
                "3\ttrue",
                "0",
                "changed Score",
                "renamed Shared2",
                "4\tfalse"
            ]
        );
    }

    #[test]
    fn reparenting_across_replicated_containers_adds_and_removes() {
        let mut t = two();
        t.server(
            r#"
            local f = Instance.new("Folder")
            f.Name = "Box"
            f.Parent = game.ServerStorage
            "#,
        );
        t.frame();
        t.client(
            r#"
            print(workspace:FindFirstChild("Box") == nil)
            workspace.ChildAdded:Connect(function(c) print("added " .. c.Name) end)
            "#,
        );
        t.server("game.ServerStorage.Box.Parent = workspace");
        t.frame();
        t.client(
            r#"
            local box = workspace.Box
            box.Destroying:Connect(function() print("gone " .. box.Name) end)
            "#,
        );
        t.server("workspace.Box.Parent = game.ServerStorage");
        t.frame();
        t.client("print(workspace:FindFirstChild('Box') == nil)");
        t.server("game.ServerStorage.Box.Parent = workspace");
        t.frame();
        t.client("print(workspace:FindFirstChild('Box') ~= nil)");
        t.frame();
        t.no_errors();
        let (_, c) = t.out();
        assert_eq!(
            c,
            ["true", "added Box", "gone Box", "true", "added Box", "true"]
        );
    }

    #[test]
    fn destroy_on_the_server_destroys_on_the_client() {
        let mut t = two();
        t.server(
            r#"
            local m = Instance.new("Model")
            m.Name = "M"
            Instance.new("Folder", m).Name = "Inner"
            m.Parent = workspace
            "#,
        );
        t.frame();
        t.client(
            r#"
            local m = workspace.M
            local inner = m.Inner
            inner.Destroying:Connect(function() print("inner destroying") end)
            workspace.ChildRemoved:Connect(function(c)
                print("removed", c == m, m.Parent, m.Name)
            end)
            "#,
        );
        t.server("workspace.M:Destroy()");
        t.frame();
        t.frame();
        t.no_errors();
        let (_, c) = t.out();
        assert!(c.contains(&"inner destroying".to_string()), "{c:?}");
        assert!(c.contains(&"removed\ttrue\tnil\tM".to_string()), "{c:?}");
        assert!(
            t.worlds.client.entity("M").is_err() || {
                let m = t.worlds.client.entity("M").unwrap();
                re_compat::instance::tree::is_destroyed(&t.worlds.client, m)
            }
        );
    }

    #[test]
    fn client_writes_stay_local_until_the_server_changes_the_property() {
        let mut t = two();
        t.server(
            r#"
            local f = Instance.new("Folder")
            f.Name = "Door"
            f:SetAttribute("Open", false)
            f.Parent = workspace
            "#,
        );
        t.frame();
        t.client(
            r#"
            local f = workspace.Door
            f.Name = "MyDoor"
            f:SetAttribute("Open", true)
            local mine = Instance.new("Folder")
            mine.Name = "ClientOnly"
            mine.Parent = workspace
            "#,
        );
        t.frame();
        t.server(
            r#"
            print(workspace:FindFirstChild("Door") ~= nil, workspace:FindFirstChild("ClientOnly") == nil)
            print(workspace.Door:GetAttribute("Open"))
            "#,
        );
        t.client(
            r#"
            print(workspace:FindFirstChild("MyDoor") ~= nil, workspace.MyDoor:GetAttribute("Open"))
            print(workspace:FindFirstChild("ClientOnly") ~= nil)
            "#,
        );
        t.server(r#"workspace.Door.Name = "Gate""#);
        t.frame();
        t.client(
            r#"
            print(workspace:FindFirstChild("Gate") ~= nil, workspace.Gate:GetAttribute("Open"))
            "#,
        );
        // Writing the value the server already has is no change, as in
        // Roblox; a different value replicates.
        t.server(r#"workspace.Gate:SetAttribute("Open", false)"#);
        t.frame();
        t.client(r#"print(workspace.Gate:GetAttribute("Open"))"#);
        t.server(r#"workspace.Gate:SetAttribute("Open", "jammed")"#);
        t.frame();
        t.client(r#"print(workspace.Gate:GetAttribute("Open"))"#);
        t.no_errors();
        let (s, c) = t.out();
        assert_eq!(s, ["true\ttrue", "false"]);
        assert_eq!(c, ["true\ttrue", "true", "true\ttrue", "true", "jammed"]);
    }

    /// A server with a remote event, a remote function and a joined local
    /// player whose client knows the remotes.
    fn with_remotes() -> (Two, Entity) {
        let mut t = two();
        t.server(
            r#"
            local ev = Instance.new("RemoteEvent")
            ev.Name = "Ev"
            ev.Parent = game.ReplicatedStorage
            local fn = Instance.new("RemoteFunction")
            fn.Name = "Fn"
            fn.Parent = game.ReplicatedStorage
            "#,
        );
        let p = t.join(&PlayerSpec::numbered(1));
        t.frame();
        (t, p)
    }

    #[test]
    fn remote_events_copy_values_both_ways_in_order() {
        let (mut t, _) = with_remotes();
        t.server(
            r#"
            local ev = game.ReplicatedStorage.Ev
            ev.OnServerEvent:Connect(function(player, n, t, inst, nested, mixed)
                print(player.Name, n ~= n, t[1], t[2], t[3], inst == workspace)
                print(nested.a.b[2], mixed[1], mixed[2], mixed.x)
            end)
            "#,
        );
        t.client(
            r#"
            local ev = game.ReplicatedStorage.Ev
            ev.OnClientEvent:Connect(function(...)
                print("client got", ...)
            end)
            for i = 1, 3 do
                ev:FireServer(0/0, {"a", "b", i}, workspace, {a = {b = {1, 2}}}, {10, 20, x = "dropped"})
            end
            local ok, err = pcall(function() ev:FireServer(function() end) end)
            print(ok, err)
            local cyc = {}; cyc.self = cyc
            local ok2, err2 = pcall(function() ev:FireServer(cyc) end)
            print(ok2, err2)
            "#,
        );
        t.frame();
        t.server(
            r#"
            local ev = game.ReplicatedStorage.Ev
            local secret = Instance.new("Folder", game.ServerStorage)
            ev:FireAllClients("one", secret)
            ev:FireClient(game.Players.Player1, "two", workspace)
            "#,
        );
        t.frame();
        t.no_errors();
        let (s, c) = t.out();
        assert_eq!(
            s,
            [
                "Player1\ttrue\ta\tb\t1\ttrue",
                "2\t10\t20\tnil",
                "Player1\ttrue\ta\tb\t2\ttrue",
                "2\t10\t20\tnil",
                "Player1\ttrue\ta\tb\t3\ttrue",
                "2\t10\t20\tnil",
            ]
        );
        assert!(
            c[0].starts_with("false\t") && c[0].contains("[RE9300]") && c[0].contains("argument 1"),
            "{c:?}"
        );
        assert!(
            c[1].contains("[RE9300]") && c[1].contains("contains itself"),
            "{c:?}"
        );
        // A server-only instance arrives as nil.
        assert_eq!(c[2], "client got\tone\tnil");
        assert_eq!(c[3], "client got\ttwo\tWorkspace");
    }

    #[test]
    fn events_wait_for_their_first_listener() {
        let (mut t, _) = with_remotes();
        t.client(r#"game.ReplicatedStorage.Ev:FireServer("early")"#);
        t.frame();
        t.frame();
        t.server(
            r#"
            game.ReplicatedStorage.Ev.OnServerEvent:Connect(function(p, x) print(x) end)
            "#,
        );
        t.frame();
        t.no_errors();
        assert_eq!(t.out().0, ["early"]);
    }

    #[test]
    fn invoke_server_yields_across_frames() {
        let (mut t, _) = with_remotes();
        t.server(
            r#"
            game.ReplicatedStorage.Fn.OnServerInvoke = function(player, a, b)
                task.wait(0.05)
                if a == "boom" then error("no coins") end
                return a + b, player.Name
            end
            "#,
        );
        t.client(
            r#"
            local fn = game.ReplicatedStorage.Fn
            local before = os.clock()
            print("sum", fn:InvokeServer(2, 3))
            local ok, err = pcall(fn.InvokeServer, fn, "boom")
            print(ok, err)
            "#,
        );
        t.frame();
        assert!(t.out().1.is_empty());
        t.frames(10);
        t.no_errors();
        let (_, c) = t.out();
        assert_eq!(c[0], "sum\t5\tPlayer1");
        assert!(
            c[1].starts_with("false\t") && c[1].contains("[RE9301]") && c[1].contains("no coins"),
            "{c:?}"
        );
    }

    #[test]
    fn invoke_client_round_trips_and_fails_when_the_player_leaves() {
        let (mut t, p) = with_remotes();
        t.client(
            r#"
            game.ReplicatedStorage.Fn.OnClientInvoke = function(x)
                return x * 2, game.Players.LocalPlayer.Name
            end
            "#,
        );
        t.server(
            r#"
            local fn = game.ReplicatedStorage.Fn
            print(fn:InvokeClient(game.Players.Player1, 21))
            "#,
        );
        t.frames(3);
        t.client(
            r#"
            game.ReplicatedStorage.Fn.OnClientInvoke = function() task.wait(10) end
            "#,
        );
        t.server(
            r#"
            local fn = game.ReplicatedStorage.Fn
            local p = game.Players.Player1
            print(pcall(fn.InvokeClient, fn, p, 1))
            "#,
        );
        t.frame();
        t.leave(p);
        t.frames(2);
        t.no_errors();
        let (s, _) = t.out();
        assert_eq!(s[0], "42\tPlayer1");
        assert!(
            s[1].starts_with("false\t") && s[1].contains("left"),
            "{s:?}"
        );
    }

    #[test]
    fn bindables_copy_and_return_in_the_same_vm() {
        let mut t = two();
        t.server(
            r#"
            local be = Instance.new("BindableEvent")
            local got
            be.Event:Connect(function(t, inst) got = t; print(t.x, inst == workspace) end)
            local src = {x = 1}
            be:Fire(src, workspace)
            task.wait()
            print(got ~= src)
            local bf = Instance.new("BindableFunction")
            print(pcall(bf.Invoke, bf, 1))
            bf.OnInvoke = function(a) task.wait() return a + 1 end
            print(bf:Invoke(41))
            print(type(bf.OnInvoke))
            bf.OnInvoke = function() error("bad") end
            print(pcall(bf.Invoke, bf))
            "#,
        );
        t.frames(5);
        t.no_errors();
        let (s, _) = t.out();
        assert_eq!(s[0], "1\ttrue");
        assert_eq!(s[1], "true");
        assert!(
            s[2].starts_with("false\t") && s[2].contains("[RE9301]"),
            "{s:?}"
        );
        assert_eq!(s[3], "42");
        assert_eq!(s[4], "function");
        assert!(s[5].starts_with("false\t") && s[5].contains("bad"), "{s:?}");
    }

    #[test]
    fn two_players_join_spawn_leave_and_rejoin() {
        let mut t = two();
        t.server(
            r#"
            local sp = Instance.new("Model")
            sp.Name = "SpawnArea"
            sp.Parent = workspace
            local Players = game:GetService("Players")
            Players.PlayerAdded:Connect(function(p)
                print("added " .. p.Name, p.UserId)
                p.CharacterAdded:Connect(function(c)
                    print("character " .. c.Name, c.Parent == workspace, Players:GetPlayerFromCharacter(c) == p)
                end)
            end)
            Players.PlayerRemoving:Connect(function(p) print("removing " .. p.Name) end)
            "#,
        );
        t.client(
            r#"
            local Players = game:GetService("Players")
            Players.PlayerAdded:Connect(function(p) print("client added " .. p.Name) end)
            Players.PlayerRemoving:Connect(function(p) print("client removing " .. p.Name) end)
            "#,
        );
        let p1 = t.join(&PlayerSpec::numbered(1));
        let p2 = t.join(&PlayerSpec::numbered(2));
        t.frames(2);
        t.client(
            r#"
            local Players = game:GetService("Players")
            local me = Players.LocalPlayer
            print(me.Name, #Players:GetPlayers(), me.Character ~= nil)
            print(me:FindFirstChild("PlayerGui") ~= nil, me:FindFirstChild("PlayerScripts") ~= nil, me:FindFirstChild("Backpack") ~= nil)
            print(Players:GetPlayerByUserId(2).Name)
            "#,
        );
        t.server("print(game.Players.LocalPlayer == nil, #game.Players:GetPlayers())");
        // Both characters exist in both worlds, as the same instances.
        let c1 = players::character_of(&t.worlds.server, p1).expect("character 1");
        let c2 = players::character_of(&t.worlds.server, p2).expect("character 2");
        assert!(t.worlds.client_of(c1).is_some() && t.worlds.client_of(c2).is_some());
        assert_eq!(
            re_compat::net::network_owner(&t.worlds.server, c1),
            Some(p1)
        );
        t.leave(p2);
        t.frame();
        assert!(t.worlds.client_of(c2).is_none());
        let p2b = t.join(&PlayerSpec {
            name: "Player2".into(),
            user_id: 2,
            local: false,
        });
        t.frames(2);
        assert!(players::character_of(&t.worlds.server, p2b).is_some());
        t.no_errors();
        let (s, c) = t.out();
        assert_eq!(
            s,
            [
                "added Player1\t1",
                "added Player2\t2",
                "character Player1\ttrue\ttrue",
                "character Player2\ttrue\ttrue",
                "true\t2",
                "removing Player2",
                "added Player2\t2",
                "character Player2\ttrue\ttrue",
            ]
        );
        assert_eq!(
            c,
            [
                "client added Player1",
                "client added Player2",
                "Player1\t2\ttrue",
                "true\ttrue\ttrue",
                "Player2",
                "client removing Player2",
                "client added Player2",
            ]
        );
    }

    #[test]
    fn load_character_uses_starter_character_scripts_and_pack() {
        let mut t = two();
        t.server(
            r#"
            local Players = game:GetService("Players")
            Players.CharacterAutoLoads = false
            local sc = Instance.new("Folder")
            sc.Name = "CharScript"
            sc.Parent = game.StarterPlayer.StarterCharacterScripts
            local tool = Instance.new("Folder")
            tool.Name = "Sword"
            tool.Parent = game:GetService("StarterPack")
            "#,
        );
        let p = t.join(&PlayerSpec::numbered(1));
        t.frames(2);
        assert!(players::character_of(&t.worlds.server, p).is_none());
        t.server(
            r#"
            local p = game.Players.Player1
            p.CharacterRemoving:Connect(function(c) print("removing", c.Name) end)
            p:LoadCharacter()
            local c = p.Character
            print(c.Name, c:FindFirstChild("CharScript") ~= nil, p.Backpack:FindFirstChild("Sword") ~= nil)
            p:LoadCharacter()
            print(c.Parent == workspace, p.Character ~= c, #p.Backpack:GetChildren())
            task.wait()
            print(c.Parent == nil)
            "#,
        );
        t.frames(2);
        t.client(
            r#"
            local ok, err = pcall(function() game.Players.LocalPlayer:LoadCharacter() end)
            print(ok, err)
            "#,
        );
        t.frame();
        t.no_errors();
        let (s, c) = t.out();
        // Deferred signals: the handler runs after the calling script yields.
        assert_eq!(
            s,
            [
                "Player1\ttrue\ttrue",
                "true\ttrue\t1",
                "removing\tPlayer1",
                "true"
            ]
        );
        assert!(
            c[0].starts_with("false\t") && c[0].contains("server"),
            "{c:?}"
        );
    }

    #[test]
    fn chat_fires_chatted_and_text_chat_commands() {
        let mut t = two();
        t.server(
            r#"
            local cmd = Instance.new("TextChatCommand")
            cmd.PrimaryAlias = "/dance"
            cmd.Parent = game:GetService("TextChatService")
            cmd.Triggered:Connect(function(src, text) print("server cmd", src.UserId, text) end)
            game.Players.PlayerAdded:Connect(function(p)
                p.Chatted:Connect(function(m) print("server chatted", m) end)
            end)
            "#,
        );
        let p = t.join(&PlayerSpec::numbered(1));
        t.frames(2);
        t.client(
            r#"
            local me = game.Players.LocalPlayer
            me.Chatted:Connect(function(m) print("client chatted", m) end)
            game.TextChatService:FindFirstChildOfClass("TextChatCommand").Triggered:Connect(function(src, text)
                print("client cmd", src and src.UserId, text)
            end)
            "#,
        );
        t.say(p, "/dance now");
        t.frame();
        t.no_errors();
        let (s, c) = t.out();
        assert_eq!(
            s,
            ["server chatted\t/dance now", "server cmd\t1\t/dance now"]
        );
        assert_eq!(
            c,
            ["client chatted\t/dance now", "client cmd\t1\t/dance now"]
        );
    }

    #[test]
    fn http_debris_gui_and_offline_services() {
        let mut t = two();
        t.server(
            r#"
            local Http = game:GetService("HttpService")
            local g = Http:GenerateGUID()
            print(#g, g:sub(1, 1), g:sub(-1), #Http:GenerateGUID(false))
            print(Http:JSONEncode({}), Http:JSONEncode({1, 2, "x"}), Http:JSONEncode({a = true}))
            local d = Http:JSONDecode('{"a":[1,2,{"b":null}],"c":"s"}')
            print(d.a[2], d.c, d.a[3].b)
            print(Http:UrlEncode("a b&c"))
            print(pcall(Http.JSONEncode, Http, {f = print}))
            print(pcall(Http.GetAsync, Http, "https://example.com"))
            local ds = game:GetService("DataStoreService")
            print(pcall(ds.GetDataStore, ds, "x"))
            local f = Instance.new("Folder", workspace)
            f.Name = "Temp"
            game:GetService("Debris"):AddItem(f, 0.05)
            "#,
        );
        t.frames(10);
        t.server(r#"print(workspace:FindFirstChild("Temp") == nil)"#);
        t.client(
            r#"
            local tl, br = game:GetService("GuiService"):GetGuiInset()
            print(tl.Y, br.X)
            local sg = game:GetService("StarterGui")
            sg:SetCoreGuiEnabled(Enum.CoreGuiType.Backpack, false)
            print(sg:GetCoreGuiEnabled(Enum.CoreGuiType.Backpack), sg:GetCoreGuiEnabled(Enum.CoreGuiType.Chat))
            sg:SetCore("ChatActive", false)
            print(sg:GetCore("ChatActive"), pcall(sg.GetCore, sg, "Nope"))
            "#,
        );
        t.frame();
        t.no_errors();
        let (s, c) = t.out();
        assert_eq!(s[0], "38\t{\t}\t36");
        assert_eq!(s[1], r#"[]	[1,2,"x"]	{"a":true}"#);
        assert_eq!(s[2], "2\ts\tnil");
        assert_eq!(s[3], "a%20b%26c");
        assert!(s[4].starts_with("false\t"), "{s:?}");
        assert!(
            s[5].starts_with("false\t") && s[5].contains("[RE9002]"),
            "{s:?}"
        );
        assert!(
            s[6].starts_with("false\t") && s[6].contains("[RE9002]"),
            "{s:?}"
        );
        assert_eq!(s[7], "true");
        let hits = t.server.unsupported();
        assert_eq!(
            hits.get("HttpService.GetAsync (RE9002)"),
            Some(&1),
            "{hits:?}"
        );
        assert_eq!(
            hits.get("DataStoreService.GetDataStore (RE9002)"),
            Some(&1),
            "{hits:?}"
        );
        assert_eq!(c[0], "58\t0");
        assert_eq!(c[1], "false\ttrue");
        assert!(c[2].starts_with("false\tfalse\t"), "{c:?}");
    }

    #[test]
    fn characters_load_at_the_first_enabled_spawn_with_a_force_field() {
        let mut t = two_with_spawns();
        t.server(
            r#"
            local off = Instance.new("SpawnLocation")
            off.Name = "Off"
            off.Enabled = false
            off.Duration = 100
            off.Parent = workspace
            local on = Instance.new("SpawnLocation")
            on.Name = "On"
            on.Duration = 0.1
            on.Parent = workspace
            "#,
        );
        let p = t.join(&PlayerSpec::numbered(1));
        t.frame();
        t.server(
            r#"
            local c = game.Players.Player1.Character
            print(c:FindFirstChildOfClass("ForceField") ~= nil)
            "#,
        );
        t.frame();
        t.client(
            "print(game.Players.LocalPlayer.Character:FindFirstChildOfClass('ForceField') ~= nil)",
        );
        t.frames(12);
        t.server(
            r#"
            local c = game.Players.Player1.Character
            print(c:FindFirstChildOfClass("ForceField") == nil)
            "#,
        );
        t.frame();
        t.client(
            "print(game.Players.LocalPlayer.Character:FindFirstChildOfClass('ForceField') == nil)",
        );
        t.frame();
        t.no_errors();
        assert!(players::character_of(&t.worlds.server, p).is_some());
        let (s, c) = t.out();
        assert_eq!(s, ["true", "true"]);
        assert_eq!(c, ["true", "true"]);
    }

    #[test]
    fn prompts_trigger_on_both_sides() {
        let mut t = two();
        t.server(
            r#"
            local quick = Instance.new("ProximityPrompt")
            quick.Name = "Quick"
            quick.Parent = workspace
            local slow = Instance.new("ProximityPrompt")
            slow.Name = "Slow"
            slow.HoldDuration = 0.05
            slow.Parent = workspace
            for _, p in {quick, slow} do
                p.PromptButtonHoldBegan:Connect(function(pl) print(p.Name .. " began " .. pl.Name) end)
                p.Triggered:Connect(function(pl) print(p.Name .. " triggered " .. pl.Name) end)
                p.TriggerEnded:Connect(function(pl) print(p.Name .. " ended " .. pl.Name) end)
            end
            game:GetService("ProximityPromptService").PromptTriggered:Connect(function(p, pl)
                print("service " .. p.Name)
            end)
            print(pcall(quick.InputHoldBegin, quick))
            "#,
        );
        t.join(&PlayerSpec::numbered(1));
        t.frames(2);
        t.client(
            r#"
            local quick = workspace.Quick
            quick.Triggered:Connect(function(pl) print("client triggered " .. pl.Name) end)
            quick:InputHoldBegin()
            quick:InputHoldEnd()
            workspace.Slow:InputHoldBegin()
            "#,
        );
        t.frames(8);
        t.client("workspace.Slow:InputHoldEnd()");
        t.frames(2);
        t.no_errors();
        let (s, c) = t.out();
        assert!(
            s[0].starts_with("false\t") && s[0].contains("client"),
            "{s:?}"
        );
        assert_eq!(
            s[1..],
            [
                "Quick triggered Player1",
                "service Quick",
                "Quick ended Player1",
                "Slow began Player1",
                "Slow triggered Player1",
                "service Slow",
                "Slow ended Player1",
            ]
        );
        assert_eq!(c, ["client triggered Player1"]);
    }

    #[test]
    fn owned_components_flow_from_the_client() {
        use re_compat::instance::tree;
        let mut t = two();
        t.server(
            r#"
            Instance.new("Folder", workspace).Name = "Mine"
            Instance.new("Folder", workspace).Name = "Theirs"
            "#,
        );
        let p = t.join(&PlayerSpec::numbered(1));
        t.frame();
        let ws = t.worlds.server.entity("Workspace").unwrap();
        let mine = tree::find_child(&t.worlds.server, ws, "Mine", false).unwrap();
        let theirs = tree::find_child(&t.worlds.server, ws, "Theirs", false).unwrap();
        for e in [mine, theirs] {
            t.worlds
                .server
                .ecs
                .entity_mut(e)
                .insert(re_scene::Transform::default());
        }
        re_compat::net::set_network_owner(&mut t.worlds.server, mine, Some(p));
        t.frame();
        let at = |x: f32| re_scene::Transform {
            translation: glam::Vec3::new(x, 0.0, 0.0),
            ..Default::default()
        };
        let x_of =
            |w: &World, e: Entity| w.ecs.get::<re_scene::Transform>(e).unwrap().translation.x;
        for e in [mine, theirs] {
            let c = t.worlds.client_of(e).unwrap();
            t.worlds.client.ecs.entity_mut(c).insert(at(3.0));
        }
        t.frame();
        assert_eq!(x_of(&t.worlds.server, mine), 3.0);
        assert_eq!(x_of(&t.worlds.server, theirs), 0.0);
        // The server's own change still reaches the owner.
        t.worlds.server.ecs.entity_mut(mine).insert(at(7.0));
        t.frame();
        let c = t.worlds.client_of(mine).unwrap();
        assert_eq!(x_of(&t.worlds.client, c), 7.0);
        t.no_errors();
    }

    #[test]
    fn a_client_can_kick_only_itself() {
        let mut t = two();
        let p = t.join(&PlayerSpec::numbered(1));
        t.frames(2);
        t.client(r#"game.Players.LocalPlayer:Kick("bye")"#);
        t.frames(2);
        t.no_errors();
        assert!(
            re_compat::instance::tree::is_destroyed(&t.worlds.server, p)
                || !re_compat::instance::tree::alive(&t.worlds.server, p)
        );
        let (_, c) = t.out();
        assert_eq!(c, ["Player1 was kicked: bye"]);
    }

    #[test]
    fn the_host_invokes_bindable_functions_with_json() {
        use re_compat::instance::tree;
        let mut t = two();
        t.server(
            r#"
            local bf = Instance.new("BindableFunction")
            bf.Name = "Api"
            bf.Parent = game.ServerStorage
            bf.OnInvoke = function(a, t) return a + t.n, {ok = true} end
            local bad = Instance.new("BindableFunction")
            bad.Name = "Bad"
            bad.Parent = game.ServerStorage
            bad.OnInvoke = function() error("nope") end
            "#,
        );
        t.frame();
        let ss = t.worlds.server.entity("ServerStorage").unwrap();
        let api = tree::find_child(&t.worlds.server, ss, "Api", false).unwrap();
        let bad = tree::find_child(&t.worlds.server, ss, "Bad", false).unwrap();
        let mut host = WorldHost {
            world: &mut t.worlds.server,
            services: &mut t.services,
        };
        let (r, d) = re_compat::net::invoke_bindable(&t.server, &mut host, api, r#"[2, {"n": 3}]"#);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(r.unwrap(), r#"[5,{"ok":true}]"#);
        let (r, _) = re_compat::net::invoke_bindable(&t.server, &mut host, bad, "[]");
        let e = r.unwrap_err();
        assert!(
            e.code.to_string() == "RE9301" && e.message.contains("nope"),
            "{e:?}"
        );
        let (r, _) = re_compat::net::invoke_bindable(&t.server, &mut host, ss, "[]");
        assert!(r.unwrap_err().message.contains("not a BindableFunction"));
        let (r, _) = players::say(&t.server, &mut host, ss, "hi");
        assert_eq!(r.unwrap_err().code.to_string(), "RE9302");
        t.no_errors();
    }

    #[test]
    fn dead_characters_respawn_after_respawn_time() {
        let mut t = two_with_spawns();
        t.server(
            r#"
            local Players = game:GetService("Players")
            Players.RespawnTime = 0.1
            local model = Instance.new("Model")
            model.Name = "StarterCharacter"
            Instance.new("Humanoid", model)
            model.Parent = game.StarterPlayer
            "#,
        );
        let p = t.join(&PlayerSpec::numbered(1));
        t.frames(2);
        let first = players::character_of(&t.worlds.server, p).unwrap();
        t.server("game.Players.Player1.Character.Humanoid.Health = 0");
        t.frames(3);
        assert_eq!(players::character_of(&t.worlds.server, p), Some(first));
        t.frames(10);
        let second = players::character_of(&t.worlds.server, p).unwrap();
        assert_ne!(second, first);
        t.frames(10);
        assert_eq!(players::character_of(&t.worlds.server, p), Some(second));
        t.no_errors();
    }

    #[test]
    fn owned_attributes_flow_up_but_new_children_do_not() {
        let mut t = two();
        let p = t.join(&PlayerSpec::numbered(1));
        t.frames(3);
        t.client(
            r#"
            local c = game.Players.LocalPlayer.Character
            c:SetAttribute("Emote", "wave")
            Instance.new("Folder", c).Name = "ClientMade"
            "#,
        );
        t.frames(2);
        t.server(
            r#"
            local c = game.Players.Player1.Character
            print(c:GetAttribute("Emote"), c:FindFirstChild("ClientMade") == nil)
            "#,
        );
        t.frame();
        t.no_errors();
        assert!(players::character_of(&t.worlds.server, p).is_some());
        assert_eq!(t.out().0, ["wave\ttrue"]);
    }

    #[test]
    fn removal_handlers_see_the_instance_in_place() {
        let mut t = two();
        t.server(
            r#"
            local Players = game:GetService("Players")
            Players.PlayerRemoving:Connect(function(p)
                print("server removing", p.Parent == Players, p.Character ~= nil)
            end)
            Players.PlayerAdded:Connect(function(p)
                p.CharacterRemoving:Connect(function(c)
                    print("server char removing", c.Parent == workspace)
                end)
            end)
            "#,
        );
        t.client(
            r#"
            local Players = game:GetService("Players")
            Players.PlayerRemoving:Connect(function(p)
                print("client removing", p.Parent == Players)
            end)
            Players.PlayerAdded:Connect(function(p)
                p.CharacterRemoving:Connect(function(c)
                    print("client char removing", c.Parent == workspace)
                end)
            end)
            "#,
        );
        t.join(&PlayerSpec::numbered(1));
        let p2 = t.join(&PlayerSpec::numbered(2));
        t.frames(2);
        t.leave(p2);
        t.frames(2);
        t.no_errors();
        let (s, c) = t.out();
        assert_eq!(
            s,
            ["server removing\ttrue\ttrue", "server char removing\ttrue"]
        );
        assert_eq!(c, ["client removing\ttrue", "client char removing\ttrue"]);
    }

    #[test]
    fn copies_keep_array_holes_and_remotes_keep_order() {
        let (mut t, _) = with_remotes();
        t.server(
            r#"
            local rs = game.ReplicatedStorage
            rs.Ev.OnServerEvent:Connect(function(_, n) print("ev", n) end)
            rs.Fn.OnServerInvoke = function(_, n) print("fn", n) return n end
            local secret = Instance.new("Folder", game.ServerStorage)
            rs.Ev:FireAllClients({secret, "x", "y"}, -0)
            "#,
        );
        t.client(
            r#"
            local rs = game.ReplicatedStorage
            rs.Ev.OnClientEvent:Connect(function(a, z)
                print(a[1] == nil, a[2], a[3], 1 / z)
            end)
            rs.Ev:FireServer(1)
            task.spawn(function() rs.Fn:InvokeServer(2) end)
            rs.Ev:FireServer(3)
            "#,
        );
        t.frames(3);
        t.no_errors();
        let (s, c) = t.out();
        assert_eq!(s, ["ev\t1", "fn\t2", "ev\t3"]);
        assert_eq!(c, ["true\tx\ty\t-inf"]);
    }

    #[test]
    fn client_attributes_survive_the_servers_first_one() {
        let mut t = two();
        t.server(r#"Instance.new("Folder", workspace).Name = "F""#);
        t.frame();
        t.client(
            r#"
            workspace.F:SetAttribute("Mine", 1)
            workspace.F:AddTag("ClientTag")
            "#,
        );
        t.server(
            r#"
            workspace.F:SetAttribute("Srv", 2)
            workspace.F:AddTag("SrvTag")
            "#,
        );
        t.frame();
        t.client(
            r#"
            local f = workspace.F
            print(f:GetAttribute("Mine"), f:GetAttribute("Srv"), f:HasTag("ClientTag"), f:HasTag("SrvTag"))
            "#,
        );
        t.server(r#"workspace.F:SetAttribute("Srv", nil)"#);
        t.frame();
        t.client(r#"print(workspace.F:GetAttribute("Srv"), workspace.F:GetAttribute("Mine"))"#);
        t.no_errors();
        assert_eq!(t.out().1, ["1\t2\ttrue\ttrue", "nil\t1"]);
    }

    #[test]
    fn references_resolve_when_their_target_replicates() {
        let mut t = two();
        t.server(r#"Instance.new("Folder", game.ServerStorage).Name = "Spot""#);
        t.join(&PlayerSpec::numbered(1));
        t.frame();
        t.server(r#"game.Players.Player1.RespawnLocation = game.ServerStorage.Spot"#);
        t.frame();
        t.client(r#"print(game.Players.LocalPlayer.RespawnLocation)"#);
        t.server(r#"game.ServerStorage.Spot.Parent = workspace"#);
        t.frame();
        t.client(r#"print(game.Players.LocalPlayer.RespawnLocation == workspace.Spot)"#);
        t.no_errors();
        assert_eq!(t.out().1, ["nil", "true"]);
    }

    #[test]
    fn ownership_covers_children_added_later() {
        let mut t = two();
        t.join(&PlayerSpec::numbered(1));
        t.frames(2);
        t.server(r#"Instance.new("Folder", game.Players.Player1.Character).Name = "Tool""#);
        t.frame();
        t.client(r#"game.Players.LocalPlayer.Character.Tool:SetAttribute("Swing", 5)"#);
        t.frames(2);
        t.server(r#"print(game.Players.Player1.Character.Tool:GetAttribute("Swing"))"#);
        t.no_errors();
        assert_eq!(t.out().0, ["5"]);
    }

    #[test]
    fn bindables_return_at_once_when_the_handler_does_not_yield() {
        let mut t = two();
        t.server(
            r#"
            local bf = Instance.new("BindableFunction")
            bf.OnInvoke = function(x) return x end
            task.defer(function() print("deferred") end)
            print("got", bf:Invoke(1))
            "#,
        );
        t.frame();
        t.no_errors();
        assert_eq!(t.out().0, ["got\t1", "deferred"]);
    }

    #[test]
    fn core_settings_have_defaults_and_belong_to_the_client() {
        let mut t = two();
        t.client(
            r#"
            local sg = game:GetService("StarterGui")
            print(sg:GetCore("ChatActive"), sg:GetCore("DevConsoleVisible"))
            print(pcall(sg.GetCore, sg, "SendNotification"))
            "#,
        );
        t.server(r#"print(pcall(function() game:GetService("StarterGui"):SetCore("ChatActive", false) end))"#);
        t.frame();
        t.no_errors();
        let (s, c) = t.out();
        assert_eq!(c[0], "true\tfalse");
        assert!(c[1].starts_with("false\t"), "{c:?}");
        assert!(
            s[0].starts_with("false\t") && s[0].contains("client"),
            "{s:?}"
        );
    }

    #[test]
    fn the_harness_runs_a_two_world_play_test() {
        use re_compat::scene::spawn_instance;
        use re_compat::services::core::ensure_service;
        use re_compat::testing::{Harness, HarnessOptions, Side};
        use re_scene::{SaveOptions, ScriptRef};

        let setup = CompatSetup::standard().unwrap();
        let r = &setup.classes;
        let dir = tempfile::tempdir().unwrap();
        let mut w = World::new();
        let game = create_place(
            &mut w,
            r,
            &["Workspace", "ServerScriptService", "ReplicatedStorage"],
        )
        .unwrap();
        let sp = ensure_service(&mut w, r, game, "StarterPlayer").unwrap();
        // StarterPlayer comes with its two script folders.
        let sps =
            re_compat::instance::tree::find_child(&w, sp, "StarterPlayerScripts", false).unwrap();
        let scs = re_compat::instance::tree::find_child(&w, sp, "StarterCharacterScripts", false)
            .unwrap();
        let rs = w.entity("ReplicatedStorage").unwrap();
        spawn_instance(&mut w, r, "RemoteEvent", None, "Hello", Some(rs)).unwrap();
        let script = |w: &mut World, class: &str, name: &str, parent: Entity, src: &str| {
            let e = spawn_instance(w, r, class, None, name, Some(parent)).unwrap();
            let path = format!("scripts/{name}.luau");
            std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
            std::fs::write(dir.path().join(&path), src).unwrap();
            w.ecs.entity_mut(e).insert(ScriptRef {
                path,
                context: re_compat::script_context(class, None).unwrap(),
            });
        };
        let sss = w.entity("ServerScriptService").unwrap();
        script(
            &mut w,
            "Script",
            "Server",
            sss,
            r#"
            local Players = game:GetService("Players")
            Players.PlayerAdded:Connect(function(p) print("joined " .. p.Name) end)
            game.ReplicatedStorage.Hello.OnServerEvent:Connect(function(p, msg)
                print(p.Name .. " says " .. msg)
            end)
            "#,
        );
        script(
            &mut w,
            "LocalScript",
            "Client",
            sps,
            r#"
            local me = game:GetService("Players").LocalPlayer
            print("local " .. me.Name)
            game.ReplicatedStorage.Hello:FireServer("hi")
            "#,
        );
        script(
            &mut w,
            "LocalScript",
            "Char",
            scs,
            r#"print("character " .. script.Parent.Name)"#,
        );
        std::fs::create_dir_all(dir.path().join("scenes")).unwrap();
        std::fs::write(
            dir.path().join("scenes/main.scene.json"),
            re_scene::scene::to_json(&w.save(SaveOptions::default())),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("real.toml"),
            "[project]\nname = \"t\"\nentry_scene = \"scenes/main.scene.json\"\n\n[compat]\napi = \"roblox\"\nplayers = 2\n",
        )
        .unwrap();
        let mut h = Harness::open_with(
            dir.path(),
            &setup,
            HarnessOptions {
                two_worlds: true,
                ..Default::default()
            },
        )
        .unwrap();
        h.start();
        h.step(10);
        assert!(h.uncaught().is_empty(), "{:#?}", h.uncaught());
        let text = |h: &mut Harness, side| -> Vec<String> {
            h.take_output(side).into_iter().map(|l| l.text).collect()
        };
        let s = text(&mut h, Side::Server);
        let c = text(&mut h, Side::Client);
        assert_eq!(
            s,
            ["joined Player1", "joined Player2", "Player1 says hi"],
            "{c:?}"
        );
        assert_eq!(c, ["local Player1", "character Player1"], "{s:?}");
        // A one-world harness has no players.
        let mut one = Harness::open_with(dir.path(), &setup, HarnessOptions::default()).unwrap();
        let (r, _) = one.add_player(&PlayerSpec::numbered(1));
        assert!(r.unwrap_err().message.contains("two_worlds"));
    }
}
