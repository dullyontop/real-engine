//! Endpoints over the in-process network, on a simulated 60 Hz clock: the
//! handshake and its refusals, delivery under loss, duplication and
//! reordering, timeouts, disconnects and backpressure, all over encrypted
//! links (the attacks they stop are in `secure.rs`).

use std::time::Duration;

use re_core::Code;
use re_net::{
    Channel, Conditioned, Config, Endpoint, Event, LinkConditions, MemoryNet, MemoryTransport,
    PeerId, codes,
};

type Ep = Endpoint<Conditioned<MemoryTransport>>;

const TICK: Duration = Duration::from_micros(16_667);

struct Sim {
    now: Duration,
    server: Option<Ep>,
    clients: Vec<Ep>,
    /// Every event, by endpoint: 0 is the server, then the clients.
    events: Vec<Vec<Event>>,
}

impl Sim {
    fn new(server: Config, clients: Vec<Config>, link: LinkConditions) -> Sim {
        let net = MemoryNet::new();
        let cond = |seed: u64| Conditioned::new(net.bind(), LinkConditions { seed, ..link });
        let server = Endpoint::server(cond(1), server);
        let addr = server.local_addr();
        let clients: Vec<Ep> = clients
            .into_iter()
            .enumerate()
            .map(|(i, c)| Endpoint::client(cond(100 + i as u64), addr, c))
            .collect();
        let n = clients.len();
        Sim {
            now: Duration::ZERO,
            server: Some(server),
            clients,
            events: vec![Vec::new(); n + 1],
        }
    }

    fn server(&mut self) -> &mut Ep {
        self.server.as_mut().expect("server running")
    }

    /// One frame: every endpoint polls, `act` runs, every endpoint flushes.
    fn frame(&mut self, mut act: impl FnMut(&mut Sim)) {
        self.now += TICK;
        let now = self.now;
        if let Some(s) = &mut self.server {
            self.events[0].extend(s.poll(now));
        }
        for (i, c) in self.clients.iter_mut().enumerate() {
            self.events[i + 1].extend(c.poll(now));
        }
        act(self);
        if let Some(s) = &mut self.server {
            self.events[0].extend(s.flush(now));
        }
        for (i, c) in self.clients.iter_mut().enumerate() {
            self.events[i + 1].extend(c.flush(now));
        }
    }

    fn run(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame(|_| {});
        }
    }

    fn connected(&self, who: usize) -> Vec<PeerId> {
        self.events[who]
            .iter()
            .filter_map(|e| match e {
                Event::Connected(p) => Some(*p),
                _ => None,
            })
            .collect()
    }

    fn disconnects(&self, who: usize) -> Vec<Code> {
        self.events[who]
            .iter()
            .filter_map(|e| match e {
                Event::Disconnected { reason, .. } => Some(reason.code),
                _ => None,
            })
            .collect()
    }

    fn messages(&self, who: usize, channel: Channel) -> Vec<Vec<u8>> {
        self.events[who]
            .iter()
            .filter_map(|e| match e {
                Event::Message {
                    channel: c, bytes, ..
                } if *c == channel => Some(bytes.clone()),
                _ => None,
            })
            .collect()
    }
}

fn configs(n: usize) -> (Config, Vec<Config>) {
    (Config::server(7, 99), vec![Config::client(7, 99); n])
}

/// The `i`th test message: sizes from a few bytes to several fragments.
fn message(i: u32) -> Vec<u8> {
    let len = match i % 40 {
        0 => 50_000,
        1 => re_net::FRAGMENT,
        2 => re_net::FRAGMENT + 1,
        3 => 0,
        _ => 10 + (i as usize * 37) % 400,
    };
    (0..len).map(|k| (i as usize * 131 + k * 7) as u8).collect()
}

#[test]
fn a_client_connects_and_both_sides_see_it() {
    let (s, c) = configs(1);
    let mut sim = Sim::new(s, c, LinkConditions::default());
    sim.run(5);
    assert_eq!(sim.connected(0), vec![PeerId(0)]);
    assert_eq!(sim.connected(1), vec![PeerId(0)]);
    assert!(sim.clients[0].is_connected());
    assert_eq!(sim.server().peers(), vec![PeerId(0)]);
    assert!(sim.disconnects(0).is_empty() && sim.disconnects(1).is_empty());
}

fn exchange(link: LinkConditions, messages: u32) -> (Sim, PeerId, PeerId) {
    let (s, c) = configs(1);
    let mut sim = Sim::new(s, c, link);
    let mut sent = 0;
    for _ in 0..3000 {
        sim.frame(|sim| {
            let (Some(&sp), Some(&cp)) = (sim.connected(0).first(), sim.connected(1).first())
            else {
                return;
            };
            // 8 per frame each way, plus an unreliable one.
            for _ in 0..8 {
                if sent < messages {
                    sim.server()
                        .send(sp, Channel::Reliable, &message(sent))
                        .unwrap();
                    sim.clients[0]
                        .send(cp, Channel::Reliable, &message(sent + 1))
                        .unwrap();
                    sent += 1;
                }
            }
            sim.server().send(sp, Channel::Unreliable, b"ping").unwrap();
        });
        if sent == messages
            && sim.messages(1, Channel::Reliable).len() == messages as usize
            && sim.messages(0, Channel::Reliable).len() == messages as usize
        {
            break;
        }
    }
    let (sp, cp) = (sim.connected(0)[0], sim.connected(1)[0]);
    (sim, sp, cp)
}

fn assert_all_arrived(sim: &Sim, messages: u32) {
    let to_client = sim.messages(1, Channel::Reliable);
    let to_server = sim.messages(0, Channel::Reliable);
    assert_eq!(to_client.len(), messages as usize, "server to client");
    assert_eq!(to_server.len(), messages as usize, "client to server");
    for i in 0..messages {
        assert!(
            to_client[i as usize] == message(i),
            "message {i} to the client"
        );
        assert!(
            to_server[i as usize] == message(i + 1),
            "message {i} to the server"
        );
    }
    assert!(sim.disconnects(0).is_empty() && sim.disconnects(1).is_empty());
}

#[test]
fn reliable_messages_arrive_intact_and_in_order_on_a_clean_link() {
    let (sim, sp, _) = exchange(LinkConditions::default(), 400);
    assert_all_arrived(&sim, 400);
    assert_eq!(
        sim.server
            .as_ref()
            .unwrap()
            .stats(sp)
            .unwrap()
            .fragments_resent,
        0
    );
    assert!(sim.messages(1, Channel::Unreliable).len() > 30);
}

#[test]
fn reliable_messages_survive_loss_duplication_and_reordering() {
    for (loss, seed) in [(0.1, 3), (0.3, 4)] {
        let link = LinkConditions {
            loss,
            duplicate: 0.05,
            latency: Duration::from_millis(40),
            jitter: Duration::from_millis(30),
            seed,
        };
        let (sim, sp, _) = exchange(link, 400);
        assert_all_arrived(&sim, 400);
        let server = sim.server.as_ref().unwrap();
        let stats = server.stats(sp).unwrap();
        assert!(stats.fragments_resent > 0, "{loss}: {stats:?}");
        let rtt = server.rtt(sp).unwrap();
        assert!(rtt >= Duration::from_millis(80), "{loss}: rtt {rtt:?}");
    }
}

#[test]
fn a_server_refuses_another_protocol_another_game_and_one_too_many() {
    let mut s = Config::server(7, 99);
    s.max_connections = 1;
    let mut version = Config::client(8, 99);
    version.timeout = Duration::from_secs(30);
    let clients = vec![Config::client(7, 99), version, Config::client(7, 98)];
    let mut sim = Sim::new(s, clients, LinkConditions::default());
    sim.run(5);
    // The first client fills the server.
    assert_eq!(sim.connected(1).len(), 1);
    assert_eq!(sim.disconnects(2), vec![codes::VERSION_MISMATCH]);
    assert_eq!(sim.disconnects(3), vec![codes::GAME_MISMATCH]);
    let full = Sim::new(
        {
            let mut s = Config::server(7, 99);
            s.max_connections = 1;
            s
        },
        vec![Config::client(7, 99); 2],
        LinkConditions::default(),
    );
    let mut full = full;
    full.run(5);
    let codes_seen: Vec<Code> = [full.disconnects(1), full.disconnects(2)].concat();
    assert_eq!(codes_seen, vec![codes::SERVER_FULL]);
    assert_eq!(full.server().peers().len(), 1);
}

#[test]
fn a_client_with_no_server_gives_up_after_the_timeout() {
    let (s, c) = configs(1);
    let mut sim = Sim::new(s, c, LinkConditions::default());
    sim.server = None;
    sim.run(590);
    assert!(sim.disconnects(1).is_empty(), "still trying before 10 s");
    sim.run(20);
    assert_eq!(sim.disconnects(1), vec![codes::NO_ANSWER]);
}

#[test]
fn a_silent_peer_times_out_on_both_sides() {
    let (s, c) = configs(2);
    let mut sim = Sim::new(s, c, LinkConditions::default());
    sim.run(5);
    // Client 2's process stops: it neither polls nor flushes.
    let gone = sim.clients.remove(1);
    sim.run(590);
    assert!(sim.disconnects(0).is_empty());
    sim.run(20);
    assert_eq!(sim.disconnects(0), vec![codes::TIMED_OUT]);
    assert_eq!(
        sim.server().peers(),
        vec![PeerId(0)],
        "the other client stays"
    );
    // The server stops: the remaining client times out.
    sim.server = None;
    sim.run(610);
    assert_eq!(sim.disconnects(1), vec![codes::TIMED_OUT]);
    drop(gone);
}

#[test]
fn a_disconnect_carries_its_reason() {
    let (s, c) = configs(2);
    let mut sim = Sim::new(s, c, LinkConditions::default());
    sim.run(5);
    let now = sim.now;
    sim.server()
        .disconnect(now, PeerId(1), "you were kicked: AFK");
    let cp = sim.connected(1)[0];
    sim.clients[0].disconnect(now, cp, "");
    sim.run(2);
    let reason = |who: usize| {
        sim.events[who]
            .iter()
            .find_map(|e| match e {
                Event::Disconnected { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .unwrap()
    };
    let kicked = reason(2);
    assert_eq!(kicked.code, codes::CLOSED);
    assert!(
        kicked
            .message
            .ends_with("closed the connection: you were kicked: AFK"),
        "{}",
        kicked.message
    );
    let left = reason(0);
    assert_eq!(left.code, codes::CLOSED);
    assert!(sim.server().peers().is_empty());
    assert!(!sim.clients[1].is_connected());
}

#[test]
fn sending_too_much_or_to_nobody_is_refused() {
    let (s, c) = configs(1);
    let mut sim = Sim::new(s, c, LinkConditions::default());
    sim.run(5);
    let err = sim
        .server()
        .send(PeerId(0), Channel::Unreliable, &[0; re_net::FRAGMENT + 1])
        .unwrap_err();
    assert_eq!(err.code, codes::SEND);
    let err = sim
        .server()
        .send(
            PeerId(0),
            Channel::Reliable,
            &vec![0; re_net::MAX_MESSAGE + 1],
        )
        .unwrap_err();
    assert_eq!(err.code, codes::SEND);
    assert_eq!(
        sim.server()
            .send(PeerId(5), Channel::Reliable, b"x")
            .unwrap_err()
            .code,
        codes::SEND
    );
}

#[test]
fn a_backlog_marks_the_connection_congested_and_drains() {
    let (mut s, c) = configs(1);
    s.send_rate = 64.0 * 1024.0;
    let mut sim = Sim::new(s, c, LinkConditions::default());
    sim.run(5);
    for i in 0..200 {
        sim.server()
            .send(PeerId(0), Channel::Reliable, &message(i % 39 + 1))
            .unwrap();
    }
    for _ in 0..100 {
        sim.server()
            .send(PeerId(0), Channel::Reliable, &[1; 1000])
            .unwrap();
    }
    assert!(sim.server().congested(PeerId(0)));
    let mut frames = 0;
    while sim.server().backlog(PeerId(0)) > 0 {
        sim.run(1);
        frames += 1;
        assert!(frames < 600, "drains in well under 10 s at 64 KiB/s");
    }
    assert!(!sim.server().congested(PeerId(0)));
    assert_eq!(sim.messages(1, Channel::Reliable).len(), 300);
}

#[test]
fn a_send_past_the_backlog_limit_is_refused_and_the_connection_stays() {
    let (mut s, c) = configs(1);
    s.max_backlog = 200_000;
    let mut sim = Sim::new(s, c, LinkConditions::default());
    sim.run(5);
    let mut refused = None;
    let mut accepted = 0;
    for _ in 0..250 {
        match sim.server().send(PeerId(0), Channel::Reliable, &[2; 1000]) {
            Ok(()) => accepted += 1,
            Err(e) => {
                refused = Some(e);
                break;
            }
        }
    }
    assert_eq!(refused.unwrap().code, codes::SEND);
    // Each message counts its bytes plus a fixed cost.
    assert!((150..200).contains(&accepted), "{accepted}");
    sim.run(120);
    assert!(sim.disconnects(0).is_empty());
    assert_eq!(sim.messages(1, Channel::Reliable).len(), accepted);
}

#[test]
fn a_client_restarting_on_the_same_address_replaces_its_old_connection() {
    let net = MemoryNet::new();
    let mut server = Endpoint::server(net.bind(), Config::server(1, 1));
    let addr = server.local_addr();
    let old = Endpoint::client(net.bind(), addr, Config::client(1, 1));
    let client_addr = old.local_addr();
    let mut old = old;
    let mut now = Duration::ZERO;
    let mut server_events = Vec::new();
    for _ in 0..5 {
        now += TICK;
        old.poll(now);
        server_events.extend(server.poll(now));
        old.flush(now);
        server.flush(now);
    }
    assert!(old.is_connected());
    // The old process dies; a new one gets the same address (as a NAT
    // mapping would give it).
    drop(old);
    let transport = net.bind_at(client_addr);
    let mut new = Endpoint::client(transport, addr, Config::client(1, 1));
    for _ in 0..10 {
        now += TICK;
        new.poll(now);
        server_events.extend(server.poll(now));
        new.flush(now);
        server.flush(now);
    }
    assert!(
        new.is_connected(),
        "connected without waiting for the timeout"
    );
    assert_eq!(server.peers(), vec![PeerId(1)]);
    assert!(server_events.iter().any(|e| matches!(e,
        Event::Disconnected { peer: PeerId(0), reason } if reason.code == codes::CLOSED)));
}

#[test]
fn odd_configurations_do_not_hang() {
    let mut s = Config::server(1, 1);
    s.keepalive = Duration::ZERO;
    s.rto_min = Duration::from_secs(3);
    s.send_rate = 0.0;
    let mut c = Config::client(1, 1);
    c.keepalive = Duration::ZERO;
    let mut sim = Sim::new(s, vec![c], LinkConditions::default());
    sim.run(5);
    // A rate of zero counts as the floor, 1 KiB/s: this takes about a second.
    sim.server()
        .send(PeerId(0), Channel::Reliable, &[1; 5000])
        .unwrap();
    sim.run(300);
    assert_eq!(sim.messages(1, Channel::Reliable).len(), 1);
}

#[test]
fn several_clients_each_get_their_own_stream() {
    let (s, c) = configs(4);
    let mut sim = Sim::new(
        s,
        c,
        LinkConditions {
            loss: 0.05,
            seed: 9,
            ..LinkConditions::default()
        },
    );
    sim.run(60);
    assert_eq!(sim.server().peers().len(), 4);
    for p in sim.server().peers() {
        for k in 0..20u8 {
            sim.server()
                .send(p, Channel::Reliable, &[p.0 as u8, k])
                .unwrap();
        }
    }
    sim.run(120);
    for who in 1..=4 {
        let got = sim.messages(who, Channel::Reliable);
        assert_eq!(got.len(), 20);
        let first = got[0][0];
        assert!(got.iter().enumerate().all(|(k, m)| m == &[first, k as u8]));
    }
}
