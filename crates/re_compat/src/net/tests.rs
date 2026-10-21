//! Several clients of one server, each with its own world, talking through
//! `re_net` endpoints over the in-memory transport: the split replication
//! pass as a network runs it.

use std::time::Duration;

use re_net::{Channel, Config, Endpoint, Event, MemoryNet, MemoryTransport, PeerId};
use re_reflect::Value;
use re_scene::{Entity, World};

use super::wire::Frame;
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
            conn.apply_up(&mut server.world, Some(*player), &f);
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
