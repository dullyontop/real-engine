//! Several clients of one server, each with its own world, talking through
//! `re_net` endpoints over the in-memory transport: the split replication
//! pass as a network runs it.

use std::time::Duration;

use re_net::{Channel, Config, Endpoint, Event, MemoryNet, MemoryTransport, PeerId};
use re_reflect::Value;
use re_scene::{Entity, World};

use super::wire::{Frame, Op, WireValue};
use super::{Conn, Replica, ReplicationSet, set_network_owner};
use crate::CompatSetup;
use crate::scene::{create_place, spawn_instance};

const TICK: Duration = Duration::from_micros(16_667);
const SERVICES: &[&str] = &["Workspace", "Players", "ReplicatedStorage"];

fn place(setup: &CompatSetup) -> World {
    let mut w = World::new();
    create_place(&mut w, &setup.classes, SERVICES).unwrap();
    w
}

fn child(w: &World, parent: &str, name: &str) -> Option<Entity> {
    let game = w.entity(crate::GAME_ID).ok()?;
    let p = crate::services::core::find_service(w, game, parent)?;
    re_scene::hierarchy::children(&w.ecs, p)
        .iter()
        .copied()
        .find(|c| w.name_of(*c) == name)
}

/// A part's `Transform` translation.
fn position(w: &World, e: Entity) -> Value {
    let k = w.registry.try_key("Transform").unwrap();
    let i = translation(w);
    w.registry.get_all(&w.ecs, e, k).unwrap()[i].clone()
}

fn translation(w: &World) -> usize {
    let k = w.registry.try_key("Transform").unwrap();
    w.registry
        .schema(k)
        .fields
        .iter()
        .position(|f| f.name == "translation")
        .unwrap()
}

fn move_to(w: &mut World, e: Entity, x: f32) {
    let k = w.registry.try_key("Transform").unwrap();
    let i = translation(w);
    w.registry
        .set_field(
            &mut w.ecs,
            e,
            k,
            i,
            Value::Vec3(glam::Vec3::new(x, 1.0, 0.0)),
        )
        .unwrap();
}

struct Client {
    world: World,
    replica: Replica,
    end: Endpoint<MemoryTransport>,
}

struct Server {
    world: World,
    end: Endpoint<MemoryTransport>,
    /// Per connection: its replication state and its `Player`.
    conns: Vec<(Conn, Entity)>,
}

/// One tick: each client sends its frame, the server applies them and sends
/// each client its own, and the clients apply theirs.
fn tick(now: Duration, server: &mut Server, clients: &mut [Client], set: &ReplicationSet) {
    for c in clients.iter_mut() {
        receive(now, c);
        if c.end.is_connected() {
            let f = c.replica.diff_up(&mut c.world, set);
            c.end
                .send(PeerId(0), Channel::Reliable, &f.encode())
                .unwrap();
        }
        c.end.flush(now);
    }
    for e in server.end.poll(now) {
        if let Event::Message { peer, bytes, .. } = e {
            let (conn, player) = &mut server.conns[peer.0 as usize];
            let f = Frame::decode(&bytes).expect("a frame");
            conn.apply_up(&mut server.world, set, Some(*player), &f, None);
        }
    }
    for peer in server.end.peers() {
        let (conn, player) = &mut server.conns[peer.0 as usize];
        let f = conn.diff(&mut server.world, set, Some(*player));
        server
            .end
            .send(peer, Channel::Reliable, &f.encode())
            .unwrap();
    }
    server.world.ecs.increment_change_tick();
    server.end.flush(now);
    for c in clients.iter_mut() {
        receive(now, c);
    }
}

/// Applies the frames that arrived for a client.
fn receive(now: Duration, c: &mut Client) {
    for e in c.end.poll(now) {
        if let Event::Message { bytes, .. } = e {
            let f = Frame::decode(&bytes).expect("a frame");
            c.replica.apply(&mut c.world, &f);
        }
    }
}

#[test]
fn two_clients_see_each_others_characters_move() {
    let setup = CompatSetup::standard().unwrap();
    let mut server_world = place(&setup);
    let game = server_world.entity(crate::GAME_ID).unwrap();
    let players = crate::services::core::find_service(&server_world, game, "Players").unwrap();
    let workspace = crate::services::core::find_service(&server_world, game, "Workspace").unwrap();
    let mut owners = Vec::new();
    for name in ["A", "B"] {
        let p = spawn_instance(
            &mut server_world,
            &setup.classes,
            "Player",
            None,
            name,
            Some(players),
        )
        .unwrap();
        let part = spawn_instance(
            &mut server_world,
            &setup.classes,
            "Part",
            None,
            &format!("Char{name}"),
            Some(workspace),
        )
        .unwrap();
        set_network_owner(&mut server_world, part, Some(p));
        owners.push(p);
    }
    let net = MemoryNet::new();
    let mut server = Server {
        world: server_world,
        end: Endpoint::server(net.bind(), Config::server(1, 1)),
        conns: owners.iter().map(|p| (Conn::default(), *p)).collect(),
    };
    let set = ReplicationSet::default();
    let mut clients: Vec<Client> = (0..2)
        .map(|_| Client {
            world: place(&setup),
            replica: Replica::default(),
            end: Endpoint::client(net.bind(), server.end.local_addr(), Config::client(1, 1)),
        })
        .collect();
    let mut now = Duration::ZERO;
    let mut run = |n: usize, server: &mut Server, clients: &mut [Client]| {
        for _ in 0..n {
            now += TICK;
            tick(now, server, clients, &set);
        }
    };
    // Client 0 connects first, so it is connection 0 and player A.
    run(1, &mut server, &mut clients[..1]);
    run(20, &mut server, &mut clients);
    assert_eq!(server.end.peers(), vec![PeerId(0), PeerId(1)]);
    for c in &clients {
        for name in ["CharA", "CharB"] {
            assert!(child(&c.world, "Workspace", name).is_some(), "{name}");
        }
        assert!(child(&c.world, "Players", "A").is_some());
    }

    // Each client moves its own character; client 0 also moves B's, which
    // it does not own.
    let a0 = child(&clients[0].world, "Workspace", "CharA").unwrap();
    let b0 = child(&clients[0].world, "Workspace", "CharB").unwrap();
    let b1 = child(&clients[1].world, "Workspace", "CharB").unwrap();
    move_to(&mut clients[0].world, a0, 5.0);
    move_to(&mut clients[0].world, b0, -9.0);
    move_to(&mut clients[1].world, b1, 7.0);
    run(10, &mut server, &mut clients);

    let want = |x: f32| Value::Vec3(glam::Vec3::new(x, 1.0, 0.0));
    let a_s = child(&server.world, "Workspace", "CharA").unwrap();
    let b_s = child(&server.world, "Workspace", "CharB").unwrap();
    assert_eq!(position(&server.world, a_s), want(5.0));
    assert_eq!(
        position(&server.world, b_s),
        want(7.0),
        "only B's owner moves it"
    );
    let a1 = child(&clients[1].world, "Workspace", "CharA").unwrap();
    assert_eq!(position(&clients[1].world, a1), want(5.0), "B sees A move");
    assert_eq!(position(&clients[0].world, b0), want(7.0), "A sees B move");
}

/// One pass between a server and one client, frames through their bytes.
fn pass(
    server: &mut World,
    conn: &mut Conn,
    player: Option<Entity>,
    client: &mut World,
    rep: &mut Replica,
    set: &ReplicationSet,
) {
    let up = Frame::decode(&rep.diff_up(client, set).encode()).unwrap();
    conn.apply_up(server, set, player, &up, None);
    let down = Frame::decode(&conn.diff(server, set, player).encode()).unwrap();
    server.ecs.increment_change_tick();
    rep.apply(client, &down);
}

fn spawn(w: &mut World, setup: &CompatSetup, class: &str, name: &str, parent: Entity) -> Entity {
    spawn_instance(w, &setup.classes, class, None, name, Some(parent)).unwrap()
}

#[test]
fn the_server_takes_only_upstream_components_that_fit() {
    let setup = CompatSetup::standard().unwrap();
    let mut server = place(&setup);
    let game = server.entity(crate::GAME_ID).unwrap();
    let players = crate::services::core::find_service(&server, game, "Players").unwrap();
    let workspace = crate::services::core::find_service(&server, game, "Workspace").unwrap();
    let p = spawn(&mut server, &setup, "Player", "A", players);
    let part = spawn(&mut server, &setup, "Part", "CharA", workspace);
    set_network_owner(&mut server, part, Some(p));
    let mut client = place(&setup);
    let (mut conn, mut rep) = (Conn::default(), Replica::default());
    let all = ReplicationSet::default();
    for _ in 0..2 {
        pass(&mut server, &mut conn, Some(p), &mut client, &mut rep, &all);
    }
    let c = child(&client, "Workspace", "CharA").unwrap();
    move_to(&mut client, c, 5.0);
    // A client that sends every component it changed.
    let up = Frame::decode(&rep.diff_up(&mut client, &all).encode()).unwrap();
    assert!(up.ops.iter().any(|o| matches!(o, Op::Up { .. })));

    let only = ReplicationSet {
        upstream: vec!["Velocity".into()],
        ..ReplicationSet::default()
    };
    assert_eq!(
        conn.apply_up(&mut server, &only, Some(p), &up, None)
            .written,
        0
    );
    assert_ne!(
        position(&server, part),
        Value::Vec3(glam::Vec3::new(5.0, 1.0, 0.0))
    );

    // One value too many: refused, not a panic.
    let mut long = up.clone();
    for op in &mut long.ops {
        if let Op::Up { values, .. } = op {
            values.push(WireValue::Bool(true));
        }
    }
    assert_eq!(
        conn.apply_up(&mut server, &all, Some(p), &long, None)
            .written,
        0
    );

    assert_eq!(
        conn.apply_up(&mut server, &all, Some(p), &up, None).written,
        1
    );
    assert_eq!(
        position(&server, part),
        Value::Vec3(glam::Vec3::new(5.0, 1.0, 0.0))
    );
}

#[test]
fn instances_waiting_under_a_destroyed_parent_arrive_with_their_children() {
    let setup = CompatSetup::standard().unwrap();
    let mut server = place(&setup);
    let game = server.entity(crate::GAME_ID).unwrap();
    let workspace = crate::services::core::find_service(&server, game, "Workspace").unwrap();
    let f = spawn(&mut server, &setup, "Folder", "F", workspace);
    let mut client = place(&setup);
    let (mut conn, mut rep) = (Conn::default(), Replica::default());
    let set = ReplicationSet::default();
    pass(&mut server, &mut conn, None, &mut client, &mut rep, &set);
    // A client script destroys F.
    let fc = child(&client, "Workspace", "F").unwrap();
    client.despawn_runtime(fc).unwrap();

    let x = spawn(&mut server, &setup, "Folder", "X", f);
    spawn(&mut server, &setup, "Folder", "Y", x);
    pass(&mut server, &mut conn, None, &mut client, &mut rep, &set);
    assert!(child(&client, "Workspace", "X").is_none());

    crate::instance::tree::set_parent(&mut server, x, Some(workspace)).unwrap();
    pass(&mut server, &mut conn, None, &mut client, &mut rep, &set);
    let xc = child(&client, "Workspace", "X").expect("X arrives");
    let kids = re_scene::hierarchy::children(&client.ecs, xc);
    assert!(
        kids.iter().any(|k| client.name_of(*k) == "Y"),
        "Y arrives under X"
    );
}

// ------------------------------------------------------------- sessions

mod sessions {
    use re_net::{Config, Endpoint, MemoryNet, MemoryTransport};
    use re_script::host::{NoServices, WorldHost};
    use re_script::{ContextKind, RuntimeConfig};

    use super::super::session::{ClientNet, ClientState, ServerNet};
    use super::super::{
        CompatWorlds, NetSettings, ReplicationSet, client_world, link_client, pump,
    };
    use crate::{CompatSetup, CompatVm, SignalBehavior};
    use re_scene::World;

    const DT: f64 = 1.0 / 60.0;
    const PLACE: &[&str] = &[
        "Workspace",
        "Players",
        "ReplicatedStorage",
        "ServerScriptService",
    ];

    struct Server {
        world: World,
        vm: CompatVm,
        net: ServerNet<MemoryTransport>,
        players: u32,
        diags: Vec<String>,
    }

    struct Client {
        world: World,
        vm: CompatVm,
        net: ClientNet<MemoryTransport>,
        set: ReplicationSet,
        started: bool,
    }

    fn scene(setup: &CompatSetup) -> World {
        let mut w = World::new();
        crate::scene::create_place(&mut w, &setup.classes, PLACE).unwrap();
        let game = w.entity(crate::GAME_ID).unwrap();
        let rs = crate::services::core::find_service(&w, game, "ReplicatedStorage").unwrap();
        crate::scene::spawn_instance(
            &mut w,
            &setup.classes,
            "RemoteEvent",
            None,
            "Ping",
            Some(rs),
        )
        .unwrap();
        w
    }

    fn vm(
        setup: &std::sync::Arc<CompatSetup>,
        kind: ContextKind,
        w: &mut World,
        dir: &std::path::Path,
    ) -> CompatVm {
        CompatVm::new(
            setup,
            kind,
            w,
            &RuntimeConfig::new(dir),
            SignalBehavior::Deferred,
        )
        .unwrap()
    }

    impl Server {
        fn exec(&mut self, src: &str) {
            let mut s = NoServices::default();
            let mut h = WorldHost {
                world: &mut self.world,
                services: &mut s,
            };
            let d = self.vm.exec(&mut h, "server", src);
            assert!(d.is_empty(), "{d:?}");
        }

        fn frame(&mut self) {
            let mut s = NoServices::default();
            let mut h = WorldHost {
                world: &mut self.world,
                services: &mut s,
            };
            let d = self
                .net
                .receive(&self.vm, &mut h, &mut self.players)
                .unwrap();
            self.diags.extend(d.into_iter().map(|d| d.message));
            let mut d = pump(&self.vm, &mut h);
            d.extend(self.vm.step(&mut h, DT));
            assert!(d.iter().all(|d| !d.is_error()), "{d:?}");
            let d = self.net.replicate(&mut self.world, DT);
            self.diags.extend(d.into_iter().map(|d| d.message));
        }
    }

    impl Client {
        fn exec(&mut self, src: &str) {
            let mut s = NoServices::default();
            let mut h = WorldHost {
                world: &mut self.world,
                services: &mut s,
            };
            let d = self.vm.exec(&mut h, "client", src);
            assert!(d.is_empty(), "{d:?}");
        }

        fn frame(&mut self) {
            if !self.started {
                if !self.net.poll_join(&mut self.world).unwrap() {
                    return;
                }
                self.started = true;
                // Frame 0: what the scripts did when they started.
                self.net.send(&mut self.world, &self.set);
                return;
            }
            if matches!(self.net.state(), ClientState::Closed(_)) {
                return;
            }
            // An end (a kick) still delivers the frame that carried it.
            let _ = self.net.receive(&mut self.world);
            let mut s = NoServices::default();
            let mut h = WorldHost {
                world: &mut self.world,
                services: &mut s,
            };
            let mut d = pump(&self.vm, &mut h);
            d.extend(self.vm.step(&mut h, DT));
            assert!(d.iter().all(|d| !d.is_error()), "{d:?}");
            self.net.send(&mut self.world, &self.set);
        }

        fn out(&self) -> Vec<String> {
            self.vm.take_output().into_iter().map(|l| l.text).collect()
        }
    }

    fn game(names: &[Option<&str>]) -> (Server, Vec<Client>, tempfile::TempDir) {
        let setup = CompatSetup::standard().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let scene = scene(&setup);
        let settings = NetSettings::default();
        let CompatWorlds { mut server, .. } = CompatWorlds::new(&setup, scene.fork()).unwrap();
        let vm_s = vm(&setup, ContextKind::Server, &mut server, dir.path());
        let net = MemoryNet::new();
        // These tests count frames, and a frame waits for nothing. A join
        // sends more than a connection's burst at the default rate (the
        // second player's first frames are about 30 KB), and the rest goes
        // once the bucket refills in real time: a fast machine runs the
        // frames a join has before that, so the rate here is high enough
        // that the bucket never waits. `re_net` tests the rates.
        let rate = 64.0 * 1024.0 * 1024.0;
        let mut config = Config::server(1, 7);
        config.send_rate = rate;
        let end = Endpoint::server(net.bind(), config);
        let addr = end.local_addr();
        let snet = ServerNet::new(end, &mut server, settings.clone(), false);
        let s = Server {
            world: server,
            vm: vm_s,
            net: snet,
            players: 0,
            diags: Vec::new(),
        };
        let clients = names
            .iter()
            .map(|n| {
                let mut copy = scene.fork();
                let set = settings.replication_set();
                let mut w = client_world(&setup, &mut copy, &set).unwrap();
                link_client(&mut w);
                let vm_c = vm(&setup, ContextKind::Client, &mut w, dir.path());
                let mut config = Config::client(1, 7);
                config.send_rate = rate;
                let end = Endpoint::client(net.bind(), addr, config);
                Client {
                    world: w,
                    vm: vm_c,
                    net: ClientNet::new(end, n.map(str::to_string), false),
                    set,
                    started: false,
                }
            })
            .collect();
        (s, clients, dir)
    }

    fn run(s: &mut Server, cs: &mut [Client], n: usize) {
        for _ in 0..n {
            s.frame();
            for c in cs.iter_mut() {
                c.frame();
            }
        }
    }

    #[test]
    fn players_join_talk_and_are_kicked() {
        let (mut s, mut cs, _dir) = game(&[Some("Ann"), None]);
        s.exec(
            r#"
            local rs = game:GetService("ReplicatedStorage")
            rs.Ping.OnServerEvent:Connect(function(p, text)
                rs.Ping:FireAllClients(p.Name .. ":" .. text)
            end)
            game:GetService("Players").PlayerAdded:Connect(function(p)
                print("joined " .. p.Name .. " " .. p.UserId)
            end)
            "#,
        );
        run(&mut s, &mut cs[..1], 5);
        run(&mut s, &mut cs, 5);
        let joined: Vec<String> = s.vm.take_output().into_iter().map(|l| l.text).collect();
        assert_eq!(joined, ["joined Ann 1", "joined Player2 2"]);
        for c in cs.iter_mut() {
            c.exec(
                r#"
                local rs = game:GetService("ReplicatedStorage")
                rs.Ping.OnClientEvent:Connect(function(t) print("got " .. t) end)
                print("me " .. game:GetService("Players").LocalPlayer.Name)
                "#,
            );
        }
        cs[1].exec(r#"game:GetService("ReplicatedStorage").Ping:FireServer("hi")"#);
        run(&mut s, &mut cs, 5);
        assert_eq!(cs[0].out(), ["me Ann", "got Player2:hi"]);
        assert_eq!(cs[1].out(), ["me Player2", "got Player2:hi"]);

        s.exec(r#"game:GetService("Players").Ann:Kick("bye")"#);
        run(&mut s, &mut cs, 5);
        let ClientState::Closed(why) = cs[0].net.state() else {
            panic!("Ann is still connected");
        };
        assert!(
            why.message.contains("Ann was kicked: bye"),
            "{}",
            why.message
        );
        assert_eq!(s.net.players(), 1);
        cs[1].exec(r#"print(#game:GetService("Players"):GetPlayers())"#);
        assert_eq!(cs[1].out(), ["1"]);
    }

    #[test]
    fn a_client_that_leaves_is_removed_and_its_messages_are_its_own() {
        let (mut s, mut cs, _dir) = game(&[None, None]);
        s.exec(
            r#"
            local rs = game:GetService("ReplicatedStorage")
            rs.Ping.OnServerEvent:Connect(function(p) print("from " .. p.Name) end)
            game:GetService("Players").PlayerRemoving:Connect(function(p)
                print("left " .. p.Name)
            end)
            "#,
        );
        run(&mut s, &mut cs[..1], 5);
        run(&mut s, &mut cs, 5);
        // A client that writes another player into its message still sends
        // as itself.
        cs[1].exec(
            r#"
            local rs = game:GetService("ReplicatedStorage")
            rs.Ping:FireServer()
            "#,
        );
        run(&mut s, &mut cs, 3);
        assert_eq!(
            s.vm.take_output()
                .into_iter()
                .map(|l| l.text)
                .collect::<Vec<_>>(),
            ["from Player2"]
        );
        cs[0].net.close();
        run(&mut s, &mut cs, 3);
        assert_eq!(
            s.vm.take_output()
                .into_iter()
                .map(|l| l.text)
                .collect::<Vec<_>>(),
            ["left Player1"]
        );
        assert_eq!(s.net.players(), 1);
    }
}
