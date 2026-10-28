//! What ADR-0067 defends against, at the endpoint: someone on the path
//! between a client and its server reads every datagram and changes, holds,
//! replays or forges them. Sealed packets that were changed, forged or
//! replayed are dropped and counted; a wrong join code or server key stops
//! the handshake with a diagnostic; and version 1 and version 2 refuse each
//! other clearly. In-process network, simulated 60 Hz clock.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

use re_core::{Code, Diagnostic};
use re_net::{
    Channel, Config, ConnStats, Endpoint, Event, FRAGMENT, Fingerprint, JoinCode, MTU, MemoryNet,
    MemoryTransport, PeerId, ServerKey, Transport, codes,
};

const TICK: Duration = Duration::from_micros(16_667);

// The packet kinds and the header's fields (ADR-0067 §3).
const HELLO: u8 = 0;
const CHALLENGE: u8 = 1;
const RESPONSE: u8 = 2;
const ACCEPT: u8 = 3;
const DENY: u8 = 4;
const DATA: u8 = 5;
const DISCONNECT: u8 = 6;
const HEADER: usize = 15;

fn kind(d: &[u8]) -> u8 {
    d[6]
}

fn number(d: &[u8]) -> u64 {
    u64::from_le_bytes(d[7..15].try_into().unwrap())
}

/// CRC-32 (IEEE), the handshake packets' trailer.
fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// Gives a handshake packet whose bytes were changed a matching CRC, as
/// anyone on the path can.
fn fix_crc(d: &mut [u8]) {
    let n = d.len() - 4;
    let crc = crc32(&d[..n]);
    d[n..].copy_from_slice(&crc.to_le_bytes());
}

/// A handshake packet from its kind, version and salt and the bytes after
/// the header, padded to the MTU when `pad`, with its CRC.
fn handshake(version: u16, kind: u8, salt: u64, body: &[u8], pad: bool) -> Vec<u8> {
    let mut p = b"RENT".to_vec();
    p.extend_from_slice(&version.to_le_bytes());
    p.push(kind);
    p.extend_from_slice(&salt.to_le_bytes());
    p.extend_from_slice(body);
    if pad {
        p.resize(MTU - 4, 0);
    }
    let crc = crc32(&p);
    p.extend_from_slice(&crc.to_le_bytes());
    p
}

/// A rewrite of one datagram on the path: what goes on in its place.
type Rewrite = Box<dyn FnMut(&[u8]) -> Vec<Vec<u8>>>;

/// The path between the client and the server, which the test controls.
#[derive(Default)]
struct Path {
    /// Every datagram the client sent, as it left the client.
    sent: Vec<Vec<u8>>,
    /// Every datagram on its way to the client, as it left the server.
    received: Vec<Vec<u8>>,
    /// What becomes of each datagram the client sends.
    outbound: Option<Rewrite>,
    /// What becomes of each datagram on its way to the client.
    inbound: Option<Rewrite>,
    /// Datagrams to send the server from the client's address.
    to_server: VecDeque<Vec<u8>>,
    /// Datagrams to give the client as from the server.
    to_client: VecDeque<Vec<u8>>,
}

/// The client's transport, with the path in it.
struct OnPath {
    inner: MemoryTransport,
    server: SocketAddr,
    path: Rc<RefCell<Path>>,
    arrived: VecDeque<(SocketAddr, Vec<u8>)>,
}

impl Transport for OnPath {
    fn send_to(&mut self, to: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        let out = {
            let mut path = self.path.borrow_mut();
            path.sent.push(datagram.to_vec());
            match &mut path.outbound {
                Some(f) => f(datagram),
                None => vec![datagram.to_vec()],
            }
        };
        for d in out {
            self.inner.send_to(to, &d)?;
        }
        Ok(())
    }

    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        loop {
            if let Some((from, d)) = self.arrived.pop_front() {
                let n = d.len().min(buf.len());
                buf[..n].copy_from_slice(&d[..n]);
                return Ok(Some((n, from)));
            }
            let forged = self.path.borrow_mut().to_client.pop_front();
            if let Some(d) = forged {
                self.arrived.push_back((self.server, d));
                continue;
            }
            let mut d = vec![0; 65536];
            let Some((n, from)) = self.inner.recv_from(&mut d)? else {
                return Ok(None);
            };
            d.truncate(n);
            let mut path = self.path.borrow_mut();
            path.received.push(d.clone());
            let out = match &mut path.inbound {
                Some(f) => f(&d),
                None => vec![d],
            };
            self.arrived.extend(out.into_iter().map(|d| (from, d)));
        }
    }

    fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr()
    }

    fn advance(&mut self, _now: Duration) {
        let forged: Vec<Vec<u8>> = self.path.borrow_mut().to_server.drain(..).collect();
        for d in forged {
            let _ = self.inner.send_to(self.server, &d);
        }
    }
}

/// A server and one client with someone on the path between them.
struct Game {
    net: MemoryNet,
    now: Duration,
    server: Endpoint<MemoryTransport>,
    client: Endpoint<OnPath>,
    path: Rc<RefCell<Path>>,
    server_events: Vec<Event>,
    client_events: Vec<Event>,
}

impl Game {
    fn new(server: Config, client: Config) -> Game {
        let net = MemoryNet::new();
        let server = Endpoint::server(net.bind(), server);
        let path = Rc::new(RefCell::new(Path::default()));
        let transport = OnPath {
            inner: net.bind(),
            server: server.local_addr(),
            path: path.clone(),
            arrived: VecDeque::new(),
        };
        let client = Endpoint::client(transport, server.local_addr(), client);
        Game {
            net,
            now: Duration::ZERO,
            server,
            client,
            path,
            server_events: Vec::new(),
            client_events: Vec::new(),
        }
    }

    /// A game whose client connected.
    fn connected(server: Config, client: Config) -> Game {
        let mut g = Game::new(server, client);
        g.run(10);
        assert!(g.client.is_connected(), "{:?}", g.client_events);
        assert_eq!(g.server.peers(), vec![PeerId(0)]);
        g
    }

    fn frame(&mut self) {
        self.now += TICK;
        self.client_events.extend(self.client.poll(self.now));
        self.server_events.extend(self.server.poll(self.now));
        self.client_events.extend(self.client.flush(self.now));
        self.server_events.extend(self.server.flush(self.now));
    }

    fn run(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame();
        }
    }

    /// Why the client's handshake or connection ended, if it did.
    fn client_failure(&self) -> Option<&Diagnostic> {
        self.client_events.iter().find_map(|e| match e {
            Event::Disconnected { reason, .. } => Some(reason),
            _ => None,
        })
    }

    fn client_failure_code(&self) -> Option<Code> {
        self.client_failure().map(|d| d.code)
    }

    fn server_got(&self) -> Vec<Vec<u8>> {
        messages(&self.server_events)
    }

    fn client_got(&self) -> Vec<Vec<u8>> {
        messages(&self.client_events)
    }

    fn server_stats(&self) -> ConnStats {
        self.server
            .stats(PeerId(0))
            .expect("the client is connected")
    }

    fn sent_of_kind(&self, k: u8) -> Vec<Vec<u8>> {
        let path = self.path.borrow();
        path.sent.iter().filter(|d| kind(d) == k).cloned().collect()
    }

    fn received_of_kind(&self, k: u8) -> Vec<Vec<u8>> {
        let path = self.path.borrow();
        path.received
            .iter()
            .filter(|d| kind(d) == k)
            .cloned()
            .collect()
    }

    /// Runs `f` on every datagram the client sends from now on.
    fn outbound(&self, f: impl FnMut(&[u8]) -> Vec<Vec<u8>> + 'static) {
        self.path.borrow_mut().outbound = Some(Box::new(f));
    }

    /// Runs `f` on every datagram on its way to the client from now on.
    fn inbound(&self, f: impl FnMut(&[u8]) -> Vec<Vec<u8>> + 'static) {
        self.path.borrow_mut().inbound = Some(Box::new(f));
    }

    fn forge_to_server(&self, d: Vec<u8>) {
        self.path.borrow_mut().to_server.push_back(d);
    }

    fn forge_to_client(&self, d: Vec<u8>) {
        self.path.borrow_mut().to_client.push_back(d);
    }
}

fn messages(events: &[Event]) -> Vec<Vec<u8>> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Message { bytes, .. } => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

fn configs() -> (Config, Config) {
    (Config::server(7, 99), Config::client(7, 99))
}

/// Rewrites the first datagram of kind `k` with `f`, and passes the rest.
fn first_of_kind(
    k: u8,
    mut f: impl FnMut(&mut Vec<u8>) + 'static,
) -> impl FnMut(&[u8]) -> Vec<Vec<u8>> {
    let mut done = false;
    move |d| {
        let mut d = d.to_vec();
        if !done && kind(&d) == k {
            done = true;
            f(&mut d);
        }
        vec![d]
    }
}

/// Rewrites every datagram of kind `k` with `f`, and passes the rest.
fn every_of_kind(
    k: u8,
    mut f: impl FnMut(&mut Vec<u8>) + 'static,
) -> impl FnMut(&[u8]) -> Vec<Vec<u8>> {
    move |d| {
        let mut d = d.to_vec();
        if kind(&d) == k {
            f(&mut d);
        }
        vec![d]
    }
}

fn join_code(text: &str) -> JoinCode {
    text.parse().unwrap()
}

#[test]
fn the_traffic_is_encrypted_on_the_wire() {
    let (s, c) = configs();
    let mut g = Game::connected(s, c);
    let secret: Vec<u8> = b"the plans for the castle are under the third stone; "
        .iter()
        .copied()
        .cycle()
        .take(3000)
        .collect();
    g.client
        .send(PeerId(0), Channel::Reliable, &secret)
        .unwrap();
    g.server
        .send(PeerId(0), Channel::Reliable, &secret)
        .unwrap();
    g.client
        .send(PeerId(0), Channel::Unreliable, &secret[..500])
        .unwrap();
    g.run(10);
    let got = g.server_got();
    assert_eq!(got.len(), 2);
    assert!(got.contains(&secret) && got.contains(&secret[..500].to_vec()));
    assert_eq!(g.client_got(), vec![secret.clone()]);
    let path = g.path.borrow();
    let wire: Vec<&Vec<u8>> = path.sent.iter().chain(&path.received).collect();
    assert!(wire.len() > 8);
    for d in wire {
        for w in secret.windows(12).step_by(7).take(50) {
            assert!(
                !d.windows(w.len()).any(|x| x == w),
                "a datagram of kind {} carries the message in the clear",
                kind(d)
            );
        }
        if matches!(kind(d), DATA | DISCONNECT) {
            assert!(d.len() <= MTU);
            assert_eq!(&d[..6], b"RENT\x02\x00", "a sealed packet of version 2");
        }
    }
}

#[test]
fn a_changed_byte_anywhere_in_a_sealed_packet_drops_it_and_counts_it() {
    let (s, mut c) = configs();
    c.send_rate = 1e9;
    let mut g = Game::connected(s, c);
    g.run(5);
    // Someone on the path keeps the client's next packet, which carries an
    // unreliable message, and sends the server every one-bit change of it
    // instead: in the header, the body and the tag.
    let held = Rc::new(RefCell::new(Vec::new()));
    let keep = held.clone();
    g.outbound(move |d| {
        keep.borrow_mut().push(d.to_vec());
        Vec::new()
    });
    g.client
        .send(PeerId(0), Channel::Unreliable, b"exactly once")
        .unwrap();
    g.client_events.extend(g.client.flush(g.now));
    g.path.borrow_mut().outbound = None;
    let held = held.borrow().clone();
    assert_eq!(held.len(), 1, "one packet");
    let real = held[0].clone();
    assert_eq!(kind(&real), DATA);
    let before = g.server_stats();
    let mut changed = 0;
    for i in 0..real.len() {
        for bit in 0..8 {
            let mut d = real.clone();
            d[i] ^= 1 << bit;
            g.forge_to_server(d);
            changed += 1;
        }
    }
    // A packet's number changed to one ahead fails its tag; if the window
    // moved to it anyway, the real packet would be too old to open.
    let mut far = real.clone();
    far[7..15].copy_from_slice(&(u64::MAX - 1).to_le_bytes());
    g.forge_to_server(far);
    changed += 1;
    g.frame();
    let after = g.server_stats();
    let dropped = (after.rejected - before.rejected)
        + (after.replayed - before.replayed)
        + (after.malformed - before.malformed);
    assert_eq!(
        dropped, changed,
        "every changed packet is dropped and counted"
    );
    assert!(after.rejected - before.rejected > 8 * 40, "{after:?}");
    assert!(g.server_got().is_empty());
    // The real packet still opens, once.
    g.forge_to_server(real.clone());
    g.forge_to_server(real);
    g.frame();
    assert_eq!(g.server_got(), vec![b"exactly once".to_vec()]);
    assert_eq!(g.server_stats().replayed, after.replayed + 1);
    assert_eq!(g.server.peers(), vec![PeerId(0)]);
    assert!(
        g.server_events
            .iter()
            .all(|e| !matches!(e, Event::Disconnected { .. }))
    );
}

#[test]
fn a_replayed_packet_is_dropped_and_counted_inside_the_window_and_past_it() {
    let (s, mut c) = configs();
    c.send_rate = 1e9;
    let mut g = Game::connected(s, c);
    g.client
        .send(PeerId(0), Channel::Unreliable, b"once")
        .unwrap();
    let sent = g.path.borrow().sent.len();
    g.client_events.extend(g.client.flush(g.now));
    let real = {
        let path = g.path.borrow();
        assert_eq!(path.sent.len(), sent + 1, "one packet");
        path.sent[sent].clone()
    };
    assert_eq!(kind(&real), DATA);
    g.run(2);
    assert_eq!(g.server_got(), vec![b"once".to_vec()]);
    let before = g.server_stats().replayed;
    for _ in 0..5 {
        g.forge_to_server(real.clone());
    }
    g.frame();
    assert_eq!(g.server_stats().replayed, before + 5);
    // 1,100 packets later the number is below the window: still dropped.
    for _ in 0..40 {
        for _ in 0..30 {
            g.client
                .send(PeerId(0), Channel::Unreliable, &[7; FRAGMENT])
                .unwrap();
        }
        g.frame();
    }
    assert!(number(g.path.borrow().sent.last().unwrap()) > number(&real) + 1100);
    let before = g.server_stats();
    g.forge_to_server(real);
    g.frame();
    let after = g.server_stats();
    assert_eq!(after.replayed, before.replayed + 1);
    assert_eq!(after.rejected, before.rejected);
    assert_eq!(
        g.server_got()
            .iter()
            .filter(|m| m.as_slice() == b"once")
            .count(),
        1
    );
}

#[test]
fn a_held_packet_opens_at_the_windows_edge_and_not_past_it() {
    let (s, mut c) = configs();
    c.send_rate = 1e9;
    let mut g = Game::connected(s, c);
    // Hold the client's next two packets, numbers n and n + 1, and pass the
    // rest until the server has opened n + 1024: n + 1 is then 1,023 below
    // the highest number, the oldest the window keeps, and n is 1,024 below.
    let held = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
    let keep = held.clone();
    let mut edge = None;
    g.outbound(move |d| {
        if kind(d) != DATA {
            return vec![d.to_vec()];
        }
        let mut held = keep.borrow_mut();
        if held.len() < 2 {
            held.push(d.to_vec());
            return Vec::new();
        }
        let n = number(&held[0]);
        let top = *edge.get_or_insert(n + 1024);
        if number(d) <= top {
            vec![d.to_vec()]
        } else {
            // Past the edge: held back too, so the highest stays n + 1024.
            held.push(d.to_vec());
            Vec::new()
        }
    });
    for _ in 0..50 {
        for _ in 0..30 {
            g.client
                .send(PeerId(0), Channel::Unreliable, &[9; FRAGMENT])
                .unwrap();
        }
        g.frame();
    }
    let held = held.borrow().clone();
    let (n, n1) = (number(&held[0]), number(&held[1]));
    assert_eq!(n1, n + 1);
    assert!(
        held.len() > 2,
        "the client sent past n + 1024, so the server's highest is n + 1024"
    );
    let before = g.server_stats();
    g.forge_to_server(held[1].clone());
    g.forge_to_server(held[0].clone());
    g.frame();
    let after = g.server_stats();
    assert_eq!(
        after.packets_received,
        before.packets_received + 1,
        "n + 1 opens"
    );
    assert_eq!(after.replayed, before.replayed + 1, "n is too old");
    assert_eq!(after.rejected, before.rejected);
}

#[test]
fn a_forged_disconnect_closes_nothing() {
    let (s, c) = configs();
    let mut g = Game::connected(s, c);
    // Keep the client's next packet from the server.
    let held = Rc::new(RefCell::new(Vec::new()));
    let keep = held.clone();
    g.outbound(move |d| {
        keep.borrow_mut().push(d.to_vec());
        Vec::new()
    });
    g.client
        .send(PeerId(0), Channel::Unreliable, b"still here")
        .unwrap();
    g.client_events.extend(g.client.flush(g.now));
    g.path.borrow_mut().outbound = None;
    let real = held.borrow()[0].clone();
    let before = g.server_stats();
    // That real packet turned into a Disconnect: the kind is in the
    // associated data, so it fails its tag.
    let mut turned = real.clone();
    turned[6] = DISCONNECT;
    g.forge_to_server(turned);
    // One made up, with a number ahead.
    let mut made_up = real[..HEADER].to_vec();
    made_up[6] = DISCONNECT;
    made_up[7..15].copy_from_slice(&(u64::MAX - 5).to_le_bytes());
    made_up.extend_from_slice(&[0; 1 + 16]);
    g.forge_to_server(made_up.clone());
    // Version 1's plaintext Disconnect, with the salt every header carried.
    g.forge_to_server(handshake(1, DISCONNECT, 0, b"\x04left", false));
    // And a made-up one to the client.
    g.forge_to_client(made_up);
    g.run(2);
    let after = g.server_stats();
    assert_eq!(after.rejected, before.rejected + 2);
    assert_eq!(after.malformed, before.malformed + 1);
    assert_eq!(g.client.stats(PeerId(0)).unwrap().rejected, 1);
    // The real packet, which the failed one did not use up, still opens.
    g.forge_to_server(real);
    g.run(30);
    assert_eq!(g.server_got(), vec![b"still here".to_vec()]);
    assert_eq!(g.server.peers(), vec![PeerId(0)]);
    assert!(g.client.is_connected());
    assert!(g.client_failure().is_none());
    assert!(
        g.server_events
            .iter()
            .all(|e| !matches!(e, Event::Disconnected { .. }))
    );
}

#[test]
fn a_real_disconnect_still_closes_with_its_reason() {
    let (s, c) = configs();
    let mut g = Game::connected(s, c);
    g.server.disconnect(g.now, PeerId(0), "the round is over");
    g.run(2);
    let reason = g.client_failure().unwrap();
    assert_eq!(reason.code, codes::CLOSED);
    assert!(
        reason
            .message
            .ends_with("closed the connection: the round is over"),
        "{}",
        reason.message
    );
    // Three copies with three numbers: the two the client did not need are
    // dropped as strays, since its connection is gone.
    assert_eq!(g.received_of_kind(DISCONNECT).len(), 3);
    let numbers: Vec<u64> = g
        .received_of_kind(DISCONNECT)
        .iter()
        .map(|d| number(d))
        .collect();
    assert!(numbers.windows(2).all(|w| w[1] == w[0] + 1), "{numbers:?}");
}

#[test]
fn a_changed_accept_is_ignored_and_the_real_one_joins() {
    for at in [HEADER, HEADER + 31, HEADER + 32, HEADER + 47] {
        let (s, c) = configs();
        let mut g = Game::new(s, c);
        g.inbound(first_of_kind(ACCEPT, move |d| {
            d[at] ^= 1;
            fix_crc(d);
        }));
        g.run(30);
        assert!(
            g.client.is_connected(),
            "msg2 byte {}: {:?}",
            at - HEADER,
            g.client_events
        );
        assert_eq!(
            g.received_of_kind(ACCEPT).len(),
            2,
            "the server sent its Accept again"
        );
        g.client.send(PeerId(0), Channel::Reliable, b"hi").unwrap();
        g.run(3);
        assert_eq!(g.server_got(), vec![b"hi".to_vec()]);
    }
}

#[test]
fn a_forged_accept_never_connects_and_says_so_at_the_timeout() {
    let (s, c) = configs();
    let mut g = Game::new(s, c);
    let mut flip = 0u8;
    g.inbound(every_of_kind(ACCEPT, move |d| {
        flip = flip.wrapping_add(1);
        d[HEADER + usize::from(flip % 48)] ^= 0x80;
        fix_crc(d);
    }));
    g.run(10 * 60 + 30);
    assert!(!g.client.is_connected());
    let reason = g.client_failure().unwrap();
    assert_eq!(reason.code, codes::NO_ANSWER);
    assert!(
        reason
            .message
            .contains("answers came that failed the handshake"),
        "{}",
        reason.message
    );
}

#[test]
fn a_changed_first_message_is_refused_and_nothing_is_kept() {
    for at in [0, 31, 32, 47] {
        let (s, c) = configs();
        let mut g = Game::new(s, c);
        // msg1 starts after the cookie, the protocol and the game.
        g.outbound(first_of_kind(RESPONSE, move |d| {
            d[HEADER + 16 + 4 + 8 + at] ^= 1;
            fix_crc(d);
        }));
        g.run(30);
        // Someone on the path can stop a join (ADR-0067 §11), but never get
        // a connection opened.
        assert_eq!(
            g.client_failure_code(),
            Some(codes::HANDSHAKE_REFUSED),
            "msg1 byte {at}"
        );
        assert!(g.server.peers().is_empty());
        assert!(g.server_events.is_empty());
    }
}

#[test]
fn a_changed_cookie_is_dropped_and_the_resent_response_joins() {
    let (s, c) = configs();
    let mut g = Game::new(s, c);
    g.outbound(first_of_kind(RESPONSE, |d| {
        d[HEADER] ^= 1;
        fix_crc(d);
    }));
    g.run(30);
    assert!(g.client.is_connected());
    assert_eq!(g.sent_of_kind(RESPONSE).len(), 2);
}

#[test]
fn a_client_of_another_protocol_cannot_be_passed_off_as_this_one() {
    // The Hello and the Response say protocol 7 on the way, but the
    // handshake is bound to the client's protocol 8 by its prologue.
    let mut g = Game::new(Config::server(7, 99), Config::client(8, 99));
    let to_7 = |d: &mut Vec<u8>| {
        let at = match kind(d) {
            HELLO => HEADER,
            RESPONSE => HEADER + 16,
            _ => return,
        };
        d[at..at + 4].copy_from_slice(&7u32.to_le_bytes());
        fix_crc(d);
    };
    g.outbound(move |d| {
        let mut d = d.to_vec();
        to_7(&mut d);
        vec![d]
    });
    g.run(30);
    assert_eq!(g.client_failure_code(), Some(codes::HANDSHAKE_REFUSED));
    assert!(g.server.peers().is_empty());
}

#[test]
fn a_client_with_the_right_join_code_joins_and_a_wrong_one_is_refused() {
    let code = join_code("KD4RT-7XWQP");
    let mut s = Config::server(7, 99);
    s.join_code = Some(code.clone());
    let mut c = Config::client(7, 99);
    c.join_code = Some(join_code("kd4rt 7xwqp"));
    Game::connected(s.clone(), c);

    let mut wrong = Config::client(7, 99);
    wrong.join_code = Some(join_code("KD4RT-7XWQQ"));
    let mut g = Game::new(s.clone(), wrong);
    g.run(30);
    let reason = g.client_failure().unwrap();
    assert_eq!(reason.code, codes::HANDSHAKE_REFUSED);
    assert!(
        reason.message.ends_with("refused the join code"),
        "{}",
        reason.message
    );
    assert!(!reason.message.contains("KD4RT"), "a code is never printed");
    assert!(g.server.peers().is_empty());
    assert!(g.server_events.is_empty());

    let mut g = Game::new(s, Config::client(7, 99));
    g.run(30);
    let reason = g.client_failure().unwrap();
    assert_eq!(reason.code, codes::JOIN_CODE_MISMATCH);
    assert!(
        reason.message.ends_with("needs a join code"),
        "{}",
        reason.message
    );
    assert!(
        g.sent_of_kind(RESPONSE).is_empty(),
        "it stops before the handshake"
    );

    let mut c = Config::client(7, 99);
    c.join_code = Some(code);
    let mut g = Game::new(Config::server(7, 99), c);
    g.run(30);
    let reason = g.client_failure().unwrap();
    assert_eq!(reason.code, codes::JOIN_CODE_MISMATCH);
    assert!(
        reason.message.contains("takes no join code"),
        "{}",
        reason.message
    );
}

#[test]
fn a_cleared_code_flag_never_makes_a_client_leave_its_code_out() {
    let mut s = Config::server(7, 99);
    s.join_code = Some(join_code("KD4RT-7XWQP"));
    let clear = || {
        every_of_kind(CHALLENGE, |d| {
            d[HEADER + 16 + 32] = 0;
            fix_crc(d);
        })
    };
    // A client without the code goes on, and the server refuses it.
    let mut g = Game::new(s.clone(), Config::client(7, 99));
    g.inbound(clear());
    g.run(30);
    let reason = g.client_failure().unwrap();
    assert_eq!(reason.code, codes::HANDSHAKE_REFUSED);
    assert!(
        reason.message.contains("it may need a join code"),
        "{}",
        reason.message
    );
    assert!(g.server.peers().is_empty());
    // A client with the code stops: it would sooner not join than send
    // its first message without the code.
    let mut c = Config::client(7, 99);
    c.join_code = s.join_code.clone();
    let mut g = Game::new(s, c);
    g.inbound(clear());
    g.run(30);
    assert_eq!(g.client_failure_code(), Some(codes::JOIN_CODE_MISMATCH));
    assert!(g.sent_of_kind(RESPONSE).is_empty());
}

#[test]
fn a_pinned_client_joins_only_the_server_with_that_key() {
    let key = ServerKey::from_bytes([42; 32]);
    let mut s = Config::server(7, 99);
    s.key = Some(key.clone());
    let mut c = Config::client(7, 99);
    c.pin = Some(key.fingerprint());
    let g = Game::connected(s.clone(), c.clone());
    assert_eq!(g.server.fingerprint(), Some(key.fingerprint()));
    assert_eq!(g.client.fingerprint(), Some(key.fingerprint()));

    // Another server key than the pinned one.
    let other: Fingerprint = ServerKey::from_bytes([43; 32]).fingerprint();
    let mut pinned_other = Config::client(7, 99);
    pinned_other.pin = Some(other);
    let mut g = Game::new(s.clone(), pinned_other);
    g.run(30);
    let reason = g.client_failure().unwrap();
    assert_eq!(reason.code, codes::KEY_MISMATCH);
    assert!(
        reason.message.contains(&key.fingerprint().to_string()),
        "{}",
        reason.message
    );
    assert!(
        reason.message.contains(&other.to_string()),
        "{}",
        reason.message
    );

    // Someone on the path puts its own key in the Challenge.
    let impostor = ServerKey::from_bytes([44; 32]).public_key();
    let swap = move || {
        every_of_kind(CHALLENGE, move |d| {
            d[HEADER + 16..HEADER + 48].copy_from_slice(&impostor);
            fix_crc(d);
        })
    };
    let mut g = Game::new(s.clone(), c);
    g.inbound(swap());
    g.run(30);
    assert_eq!(g.client_failure_code(), Some(codes::KEY_MISMATCH));
    assert!(g.sent_of_kind(RESPONSE).is_empty());
    // A client without a pin encrypts to the impostor's key, which the
    // real server cannot read: it refuses, and nothing opens.
    let mut g = Game::new(s, Config::client(7, 99));
    g.inbound(swap());
    g.run(30);
    assert_eq!(g.client_failure_code(), Some(codes::HANDSHAKE_REFUSED));
    assert!(g.server.peers().is_empty());
}

/// The low-order X25519 points: 0, 1, the two of order 8, and p - 1, p and
/// p + 1, which reduce to the same.
fn low_order_points() -> Vec<[u8; 32]> {
    let hex = |s: &str| -> [u8; 32] {
        let mut b = [0u8; 32];
        for (i, out) in b.iter_mut().enumerate() {
            *out = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        b
    };
    vec![
        [0; 32],
        hex("0100000000000000000000000000000000000000000000000000000000000000"),
        hex("e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800"),
        hex("5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157"),
        hex("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
        hex("edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
        hex("eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ]
}

/// A client written by hand on its own address: its Hello, and what the
/// server answered.
struct Raw {
    t: MemoryTransport,
    server: SocketAddr,
}

impl Raw {
    fn send(&mut self, d: &[u8]) {
        self.t.send_to(self.server, d).unwrap();
    }

    fn take(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = vec![0; 65536];
        while let Some((n, _)) = self.t.recv_from(&mut buf).unwrap() {
            out.push(buf[..n].to_vec());
        }
        out
    }
}

fn hello_body(protocol: u32, game: u64) -> Vec<u8> {
    [&protocol.to_le_bytes()[..], &game.to_le_bytes()].concat()
}

#[test]
fn a_low_order_key_from_a_client_is_refused_and_opens_nothing() {
    let net = MemoryNet::new();
    let mut server = Endpoint::server(net.bind(), Config::server(7, 99));
    let mut raw = Raw {
        t: net.bind(),
        server: server.local_addr(),
    };
    let mut now = Duration::ZERO;
    for (i, point) in low_order_points().into_iter().enumerate() {
        let salt = 1000 + i as u64;
        raw.send(&handshake(2, HELLO, salt, &hello_body(7, 99), true));
        now += TICK;
        assert!(server.poll(now).is_empty());
        let answer = raw.take();
        assert_eq!(answer.len(), 1);
        assert_eq!(kind(&answer[0]), CHALLENGE);
        let cookie = &answer[0][HEADER..HEADER + 16];
        let mut body = cookie.to_vec();
        body.extend_from_slice(&hello_body(7, 99));
        body.extend_from_slice(&point);
        body.extend_from_slice(&[0; 16]);
        raw.send(&handshake(2, RESPONSE, salt, &body, true));
        now += TICK;
        assert!(server.poll(now).is_empty(), "point {i}");
        let answer = raw.take();
        assert_eq!(answer.len(), 1);
        assert_eq!(kind(&answer[0]), DENY);
        assert_eq!(answer[0][HEADER], 4, "Deny Handshake");
        assert!(server.peers().is_empty());
    }
}

#[test]
fn a_low_order_key_in_a_challenge_is_ignored_and_the_real_one_joins() {
    for point in low_order_points() {
        let (s, c) = configs();
        let mut g = Game::new(s, c);
        g.inbound(first_of_kind(CHALLENGE, move |d| {
            d[HEADER + 16..HEADER + 48].copy_from_slice(&point);
            fix_crc(d);
        }));
        g.run(40);
        assert!(
            g.client.is_connected(),
            "{point:02x?}: {:?}",
            g.client_events
        );
    }
    // Every Challenge changed: the client gives up, and says answers came.
    let (s, c) = configs();
    let mut g = Game::new(s, c);
    g.inbound(every_of_kind(CHALLENGE, |d| {
        d[HEADER + 16..HEADER + 48].fill(0);
        fix_crc(d);
    }));
    g.run(10 * 60 + 30);
    let reason = g.client_failure().unwrap();
    assert_eq!(reason.code, codes::NO_ANSWER);
    assert!(
        reason.message.contains("failed the handshake"),
        "{}",
        reason.message
    );
}

#[test]
fn a_replayed_first_message_never_opens_a_second_connection() {
    let (s, c) = configs();
    let mut g = Game::connected(s, c);
    let response = g.sent_of_kind(RESPONSE)[0].clone();
    // While the connection is open: a resend, which the server answers with
    // its Accept again only until the client's first sealed packet arrives.
    g.forge_to_server(response.clone());
    g.run(3);
    assert_eq!(g.received_of_kind(ACCEPT).len(), 2);
    g.client
        .send(PeerId(0), Channel::Reliable, b"keys work")
        .unwrap();
    g.run(3);
    assert_eq!(g.server_got(), vec![b"keys work".to_vec()]);
    let accepts = g.received_of_kind(ACCEPT).len();
    g.forge_to_server(response.clone());
    g.run(3);
    assert_eq!(g.received_of_kind(ACCEPT).len(), accepts);
    assert_eq!(g.server.peers(), vec![PeerId(0)]);
    // From another address: the cookie is the client's address's.
    let mut raw = Raw {
        t: g.net.bind(),
        server: g.server.local_addr(),
    };
    raw.send(&response);
    g.run(3);
    assert!(raw.take().is_empty());
    // After the client left, inside the cookie's life: a replay.
    let now = g.now;
    g.client.disconnect(now, PeerId(0), "");
    g.run(3);
    assert!(g.server.peers().is_empty());
    let stray = g.server.stray();
    let answers = g.path.borrow().received.len();
    g.forge_to_server(response.clone());
    g.run(3);
    assert!(g.server.peers().is_empty());
    assert_eq!(g.path.borrow().received.len(), answers, "no answer");
    assert_eq!(g.server.stray(), stray + 1);
    // A minute later the cookie has expired.
    g.run(62 * 60);
    g.forge_to_server(response);
    g.run(3);
    assert!(g.server.peers().is_empty());
    assert_eq!(g.server.stray(), stray + 2);
    let connected = g
        .server_events
        .iter()
        .filter(|e| matches!(e, Event::Connected(_)))
        .count();
    assert_eq!(connected, 1);
}

#[test]
fn a_version_1_client_reads_the_servers_deny_as_a_version_mismatch() {
    let net = MemoryNet::new();
    let mut s = Config::server(7, 99);
    s.engine = "0.4.0-test".into();
    let mut server = Endpoint::server(net.bind(), s);
    let mut raw = Raw {
        t: net.bind(),
        server: server.local_addr(),
    };
    raw.send(&handshake(1, HELLO, 55, &hello_body(2, 99), true));
    assert!(server.poll(TICK).is_empty());
    let answer = raw.take();
    assert_eq!(answer.len(), 1);
    let deny = &answer[0];
    // What version 1 reads: the header, then reason, protocol and game, and
    // a CRC over everything before it.
    assert_eq!(&deny[..4], b"RENT");
    assert_eq!(
        u16::from_le_bytes([deny[4], deny[5]]),
        2,
        "the server's version"
    );
    assert_eq!(kind(deny), DENY);
    assert_eq!(number(deny), 55, "the client's salt");
    assert_eq!(deny[HEADER], 1, "Version");
    assert_eq!(u32::from_le_bytes(deny[16..20].try_into().unwrap()), 7);
    assert_eq!(u64::from_le_bytes(deny[20..28].try_into().unwrap()), 99);
    assert_eq!(&deny[29..29 + usize::from(deny[28])], b"0.4.0-test");
    let n = deny.len() - 4;
    assert_eq!(crc32(&deny[..n]).to_le_bytes(), deny[n..]);
    assert!(deny.len() < MTU, "smaller than the Hello it answers");
    assert!(server.peers().is_empty());
}

#[test]
fn a_version_2_client_says_a_version_1_server_cannot_be_joined() {
    let net = MemoryNet::new();
    let mut raw = Raw {
        t: net.bind(),
        server: SocketAddr::from(([127, 0, 0, 1], 0)),
    };
    let mut c = Config::client(2, 99);
    c.engine = "0.4.0".into();
    let mut client = Endpoint::client(net.bind(), raw.t.local_addr(), c);
    raw.server = client.local_addr();
    client.flush(TICK);
    let hello = raw.take();
    assert_eq!(hello.len(), 1);
    assert_eq!(hello[0].len(), MTU);
    // A version 1 server's Deny: no label.
    let mut body = vec![1];
    body.extend_from_slice(&3u32.to_le_bytes());
    body.extend_from_slice(&99u64.to_le_bytes());
    raw.send(&handshake(1, DENY, number(&hello[0]), &body, false));
    let events = client.poll(TICK * 2);
    let [Event::Disconnected { reason, .. }] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(reason.code, codes::VERSION_MISMATCH);
    assert!(
        reason
            .message
            .ends_with("speaks network version 1.3, this client 2.2"),
        "{}",
        reason.message
    );
    assert!(
        reason
            .hint
            .as_deref()
            .unwrap()
            .contains("before online play was encrypted"),
        "{:?}",
        reason.hint
    );
}

#[test]
fn a_refusal_names_both_engines() {
    let mut s = Config::server(7, 99);
    s.engine = "0.4.0-dev (cbbdbf0c0cf8 2026-10-28)".into();
    let mut c = Config::client(7, 98);
    c.engine = "0.3.0".into();
    let mut g = Game::new(s, c);
    g.run(10);
    let reason = g.client_failure().unwrap();
    assert_eq!(reason.code, codes::GAME_MISMATCH);
    assert!(
        reason.message.ends_with(
            "(the server runs Real Engine 0.4.0-dev (cbbdbf0c0cf8 2026-10-28), this client 0.3.0)"
        ),
        "{}",
        reason.message
    );
}

#[test]
fn every_handshake_answer_is_smaller_than_what_it_answers() {
    let mut s = Config::server(7, 99);
    s.join_code = Some(join_code("KD4RT-7XWQP"));
    s.engine = "x".repeat(200);
    let mut c = Config::client(7, 99);
    c.join_code = s.join_code.clone();
    let g = Game::connected(s, c);
    let path = g.path.borrow();
    let largest_answer = path
        .received
        .iter()
        .filter(|d| matches!(kind(d), CHALLENGE | ACCEPT | DENY))
        .map(Vec::len)
        .max()
        .unwrap();
    assert!(largest_answer < 100, "{largest_answer}");
    for d in path
        .sent
        .iter()
        .filter(|d| matches!(kind(d), HELLO | RESPONSE))
    {
        assert_eq!(d.len(), MTU);
    }
}

#[test]
fn secrets_stay_out_of_debug_output() {
    let key = ServerKey::from_bytes([0xa7; 32]);
    let mut c = Config::server(7, 99);
    c.key = Some(key.clone());
    c.join_code = Some(join_code("KD4RT-7XWQP"));
    let text = format!("{c:?}");
    assert!(
        text.contains(&format!("ServerKey({})", key.fingerprint())),
        "{text}"
    );
    assert!(text.contains("JoinCode(..)"), "{text}");
    assert!(!text.contains("KD4RT"), "{text}");
    assert!(!text.to_lowercase().contains("a7a7"), "{text}");
    assert!(!text.contains("167, 167"), "{text}");
}
