//! An endpoint: a server with many connections or a client with one, over a
//! [`Transport`], polled from the frame (ADR-0050 §1).
//!
//! A frame calls [`Endpoint::poll`] first (receive, handshake, timeouts) and
//! [`Endpoint::flush`] last (send what was queued). Time is the caller's: a
//! [`Duration`] since any fixed start, so tests run on a simulated clock and a
//! browser build needs no `Instant`.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash, Hasher};
use std::net::SocketAddr;
use std::time::Duration;

use re_core::Diagnostic;

use crate::codes;
use crate::conn::{Channel, Conn, ConnStats, Received};
use crate::packet::{
    self, BODY_MAX, DenyReason, FRAGMENT, HEADER, Kind, MAX_MESSAGE, MAX_REASON, MTU, Reader,
    TRANSPORT_VERSION,
};
use crate::transport::Transport;

/// How often a client repeats an unanswered handshake packet.
const HANDSHAKE_RESEND: Duration = Duration::from_millis(250);
/// How often a Disconnect is sent, since it is never acked.
const DISCONNECT_COPIES: usize = 3;
/// The most datagrams one poll reads, so a flood cannot hold the frame.
const MAX_DATAGRAMS_PER_POLL: usize = 4096;
/// How long a handshake cookie stays valid: it changes every this many
/// seconds, and the previous one is still accepted.
const COOKIE_EPOCH_SECS: u64 = 30;

/// A connection, numbered by its endpoint in the order it connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub u32);

/// What an endpoint is configured with.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The application's protocol version; a client and a server must have
    /// the same (`RE8903`). `re_compat` bumps it with its wire format.
    pub protocol: u32,
    /// The game: a hash of the build or the project's content. A client and
    /// a server must have the same (`RE8905`).
    pub game: u64,
    /// Bytes per second this endpoint sends on each connection.
    pub send_rate: f64,
    /// The most connections a server accepts (`RE8904` past it).
    pub max_connections: usize,
    /// A connection that receives nothing for this long is closed (`RE8906`);
    /// a client whose handshake goes unanswered this long gives up (`RE8902`).
    pub timeout: Duration,
    /// A connection that sent nothing for this long sends an empty packet.
    pub keepalive: Duration,
    /// The shortest retransmission timeout; two frame times is right.
    pub rto_min: Duration,
    /// The most reliable bytes a connection may have queued and unacked; a
    /// send past it is refused (`RE8908`), so a sender coalesces or waits.
    /// A connection whose sent fragments see no ack for [`Config::timeout`]
    /// is closed (`RE8906`).
    pub max_backlog: usize,
}

impl Config {
    /// A server's defaults: 256 KiB/s to each client, 16 clients.
    pub fn server(protocol: u32, game: u64) -> Config {
        Config {
            protocol,
            game,
            send_rate: 256.0 * 1024.0,
            max_connections: 16,
            timeout: Duration::from_secs(10),
            keepalive: Duration::from_millis(100),
            rto_min: Duration::from_millis(34),
            max_backlog: 4 * MAX_MESSAGE,
        }
    }

    /// A client's defaults: 64 KiB/s to the server.
    pub fn client(protocol: u32, game: u64) -> Config {
        Config {
            send_rate: 64.0 * 1024.0,
            max_connections: 1,
            ..Config::server(protocol, game)
        }
    }
}

/// What happened during a poll or flush.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A connection opened: a client joined (server) or the server accepted
    /// this client.
    Connected(PeerId),
    /// A message arrived.
    Message {
        /// From whom.
        peer: PeerId,
        /// On which channel.
        channel: Channel,
        /// Its bytes.
        bytes: Vec<u8>,
    },
    /// A connection closed, or a client's handshake failed; the diagnostic
    /// says why (`RE8902`–`RE8907`). Closing a connection yourself with
    /// [`Endpoint::disconnect`] reports nothing.
    Disconnected {
        /// The connection (a failed handshake reports the id the connection
        /// would have had).
        peer: PeerId,
        /// Why.
        reason: Diagnostic,
    },
}

enum Handshake {
    /// A server, or a client that is connected, gave up or was denied.
    None,
    Hello {
        salt: u64,
        last: Option<Duration>,
        since: Option<Duration>,
    },
    Response {
        salt: u64,
        cookie: u64,
        last: Option<Duration>,
        since: Option<Duration>,
    },
}

struct Peer {
    id: PeerId,
    conn: Conn,
}

/// A server or a client over transport `T`.
pub struct Endpoint<T: Transport> {
    transport: T,
    config: Config,
    server: bool,
    peers: HashMap<SocketAddr, Peer>,
    by_id: HashMap<PeerId, SocketAddr>,
    next_id: u32,
    key: RandomState,
    handshake: Handshake,
    server_addr: Option<SocketAddr>,
    events: Vec<Event>,
    /// Datagrams from unknown or wrong senders, dropped.
    stray: u64,
    buf: Vec<u8>,
}

impl<T: Transport> std::fmt::Debug for Endpoint<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("server", &self.server)
            .field("local_addr", &self.transport.local_addr())
            .field("connections", &self.peers.len())
            .finish_non_exhaustive()
    }
}

impl<T: Transport> Endpoint<T> {
    /// A server that accepts connections on `transport`.
    pub fn server(transport: T, config: Config) -> Endpoint<T> {
        Endpoint::new(transport, config, true)
    }

    /// A client that connects to `server` over `transport`. The handshake
    /// starts at the first flush; [`Event::Connected`] reports success.
    pub fn client(transport: T, server: SocketAddr, config: Config) -> Endpoint<T> {
        let mut e = Endpoint::new(transport, config, false);
        e.server_addr = Some(server);
        let mut h = e.key.build_hasher();
        e.transport.local_addr().hash(&mut h);
        server.hash(&mut h);
        e.handshake = Handshake::Hello {
            salt: h.finish(),
            last: None,
            since: None,
        };
        e
    }

    fn new(transport: T, config: Config, server: bool) -> Endpoint<T> {
        Endpoint {
            transport,
            config,
            server,
            peers: HashMap::new(),
            by_id: HashMap::new(),
            next_id: 0,
            // Keyed randomly per process: the cookie key is never a constant.
            key: RandomState::new(),
            handshake: Handshake::None,
            server_addr: None,
            events: Vec::new(),
            stray: 0,
            buf: vec![0; 65536],
        }
    }

    /// The configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// The transport, to change it (a [`Conditioned`](crate::Conditioned)
    /// transport's conditions, for example).
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// The address peers reach this endpoint at.
    pub fn local_addr(&self) -> SocketAddr {
        self.transport.local_addr()
    }

    /// The open connections, in connection order.
    pub fn peers(&self) -> Vec<PeerId> {
        let mut v: Vec<PeerId> = self.by_id.keys().copied().collect();
        v.sort();
        v
    }

    /// True while a client's connection is open.
    pub fn is_connected(&self) -> bool {
        !self.peers.is_empty()
    }

    /// The address of a connection's peer.
    pub fn addr_of(&self, peer: PeerId) -> Option<SocketAddr> {
        self.by_id.get(&peer).copied()
    }

    fn peer(&self, peer: PeerId) -> Option<&Conn> {
        self.by_id
            .get(&peer)
            .and_then(|a| self.peers.get(a))
            .map(|p| &p.conn)
    }

    /// A connection's smoothed round-trip time, once known.
    pub fn rtt(&self, peer: PeerId) -> Option<Duration> {
        self.peer(peer).and_then(Conn::rtt)
    }

    /// Reliable bytes queued on a connection and not yet acked.
    pub fn backlog(&self, peer: PeerId) -> usize {
        self.peer(peer).map_or(0, Conn::backlog)
    }

    /// True when a connection's backlog is past its window (the
    /// bandwidth-delay product, at least 64 KiB): a sender that can coalesce
    /// (the replication pass) should skip this connection for now.
    pub fn congested(&self, peer: PeerId) -> bool {
        self.peer(peer).is_some_and(|c| c.backlog() >= c.window())
    }

    /// A connection's counts.
    pub fn stats(&self, peer: PeerId) -> Option<ConnStats> {
        self.peer(peer).map(|c| c.stats)
    }

    /// Datagrams dropped because they came from no connection or did not
    /// belong to one.
    pub fn stray(&self) -> u64 {
        self.stray
    }

    /// Queues a message for `peer`; it goes out at the next flush.
    ///
    /// # Errors
    ///
    /// `RE8908` when the connection does not exist, or the message is larger
    /// than its channel allows ([`FRAGMENT`] bytes unreliable,
    /// [`MAX_MESSAGE`] reliable).
    pub fn send(&mut self, peer: PeerId, channel: Channel, bytes: &[u8]) -> Result<(), Diagnostic> {
        let limit = match channel {
            Channel::Unreliable => FRAGMENT,
            Channel::Reliable => MAX_MESSAGE,
        };
        if bytes.len() > limit {
            return Err(Diagnostic::error(
                codes::SEND,
                format!(
                    "{} message of {} bytes is larger than the {limit} bytes the channel allows",
                    match channel {
                        Channel::Unreliable => "an unreliable",
                        Channel::Reliable => "a reliable",
                    },
                    bytes.len()
                ),
            ));
        }
        let max_backlog = self.config.max_backlog;
        let conn = self
            .by_id
            .get(&peer)
            .and_then(|a| self.peers.get_mut(a))
            .ok_or_else(|| {
                Diagnostic::error(codes::SEND, format!("connection {} is not open", peer.0))
            })?;
        // Each message counts a fixed cost too, so many tiny ones stay bounded.
        let backlog = conn.conn.queued_cost();
        if channel == Channel::Reliable
            && backlog + bytes.len() + crate::conn::FRAGMENT_COST > max_backlog
        {
            return Err(Diagnostic::error(
                codes::SEND,
                format!(
                    "connection {} already has {backlog} reliable bytes waiting, and {} more would pass the {max_backlog}-byte limit",
                    peer.0,
                    bytes.len()
                ),
            )
            .with_hint("send less at once, or wait until Endpoint::congested is false"));
        }
        conn.conn.queue(channel, bytes);
        Ok(())
    }

    /// Receives every waiting datagram, runs the handshake and closes
    /// connections that timed out. Returns what happened.
    pub fn poll(&mut self, now: Duration) -> Vec<Event> {
        self.transport.advance(now);
        for _ in 0..MAX_DATAGRAMS_PER_POLL {
            let mut buf = std::mem::take(&mut self.buf);
            let got = self.transport.recv_from(&mut buf);
            match got {
                Ok(Some((n, from))) => self.on_datagram(now, from, &buf[..n.min(buf.len())]),
                Ok(None) | Err(_) => {
                    self.buf = buf;
                    break;
                }
            }
            self.buf = buf;
        }
        let timeout = self.config.timeout;
        let dead: Vec<SocketAddr> = self
            .peers
            .iter()
            .filter(|(_, p)| now.saturating_sub(p.conn.last_recv) > timeout)
            .map(|(a, _)| *a)
            .collect();
        for a in dead {
            let what = if self.server {
                "the client"
            } else {
                "the server"
            };
            self.close(
                now,
                a,
                Some(Diagnostic::error(
                    codes::TIMED_OUT,
                    format!("{what} at {a} sent nothing for {} s", timeout.as_secs_f64()),
                )),
                "timed out",
            );
        }
        std::mem::take(&mut self.events)
    }

    /// Sends this frame's datagrams on every connection, and a client's
    /// handshake. Returns what happened (a handshake that gave up, a
    /// connection closed for its backlog).
    pub fn flush(&mut self, now: Duration) -> Vec<Event> {
        self.transport.advance(now);
        self.flush_handshake(now);
        let addrs: Vec<SocketAddr> = self.peers.keys().copied().collect();
        for a in addrs {
            let timeout = self.config.timeout;
            let peer = self.peers.get_mut(&a).expect("listed");
            if peer.conn.stalled(now, timeout) {
                let backlog = peer.conn.backlog();
                self.close(
                    now,
                    a,
                    Some(Diagnostic::error(
                        codes::TIMED_OUT,
                        format!(
                            "{a} acknowledged nothing for {} s ({backlog} bytes waiting)",
                            timeout.as_secs_f64()
                        ),
                    )),
                    "stopped acknowledging",
                );
                continue;
            }
            for d in peer.conn.flush(now) {
                let _ = self.transport.send_to(a, &d);
            }
        }
        std::mem::take(&mut self.events)
    }

    /// Closes a connection, telling the peer `reason` (at most 200 bytes are
    /// sent). Reports no event.
    pub fn disconnect(&mut self, now: Duration, peer: PeerId, reason: &str) {
        if let Some(a) = self.by_id.get(&peer).copied() {
            self.close(now, a, None, reason);
        }
    }

    /// Closes every connection (a server shutting down, a client leaving).
    pub fn disconnect_all(&mut self, now: Duration, reason: &str) {
        for p in self.peers() {
            self.disconnect(now, p, reason);
        }
        self.handshake = Handshake::None;
    }

    fn close(&mut self, now: Duration, addr: SocketAddr, event: Option<Diagnostic>, reason: &str) {
        let Some(p) = self.peers.remove(&addr) else {
            return;
        };
        self.by_id.remove(&p.id);
        let mut d = packet::begin(Kind::Disconnect, p.conn.salt);
        let mut cut = reason.len().min(MAX_REASON);
        while !reason.is_char_boundary(cut) {
            cut -= 1;
        }
        d.push(cut as u8);
        d.extend_from_slice(&reason.as_bytes()[..cut]);
        let d = packet::seal(d);
        self.transport.advance(now);
        for _ in 0..DISCONNECT_COPIES {
            let _ = self.transport.send_to(addr, &d);
        }
        if let Some(reason) = event {
            self.events.push(Event::Disconnected { peer: p.id, reason });
        }
    }

    fn cookie(&self, addr: SocketAddr, salt: u64, epoch: u64) -> u64 {
        let mut h = self.key.build_hasher();
        addr.hash(&mut h);
        salt.hash(&mut h);
        epoch.hash(&mut h);
        h.finish()
    }

    fn open(&mut self, now: Duration, addr: SocketAddr, salt: u64) -> PeerId {
        let id = PeerId(self.next_id);
        self.next_id += 1;
        let c = &self.config;
        let conn = Conn::new(salt, now, c.send_rate, c.rto_min, c.keepalive);
        self.peers.insert(addr, Peer { id, conn });
        self.by_id.insert(id, addr);
        self.events.push(Event::Connected(id));
        id
    }

    fn flush_handshake(&mut self, now: Duration) {
        let Some(server) = self.server_addr else {
            return;
        };
        let timeout = self.config.timeout;
        let (protocol, game) = (self.config.protocol, self.config.game);
        let mut give_up = false;
        let mut send = None;
        match &mut self.handshake {
            Handshake::None => {}
            Handshake::Hello { salt, last, since }
            | Handshake::Response {
                salt, last, since, ..
            } => {
                let started = *since.get_or_insert(now);
                if now.saturating_sub(started) > timeout {
                    give_up = true;
                } else if last.is_none_or(|t| now.saturating_sub(t) >= HANDSHAKE_RESEND) {
                    *last = Some(now);
                    send = Some(*salt);
                }
            }
        }
        if give_up {
            self.handshake = Handshake::None;
            let id = PeerId(self.next_id);
            self.events.push(Event::Disconnected {
                peer: id,
                reason: Diagnostic::error(
                    codes::NO_ANSWER,
                    format!(
                        "no answer from a server at {server} after {} s",
                        timeout.as_secs_f64()
                    ),
                )
                .with_hint(
                    "check the address and port, and that the server is running and reachable",
                ),
            });
            return;
        }
        let Some(salt) = send else { return };
        let mut p = match &self.handshake {
            Handshake::Hello { .. } => packet::begin(Kind::Hello, salt),
            Handshake::Response { cookie, .. } => {
                let mut p = packet::begin(Kind::Response, salt);
                p.extend_from_slice(&cookie.to_le_bytes());
                p
            }
            Handshake::None => return,
        };
        p.extend_from_slice(&protocol.to_le_bytes());
        p.extend_from_slice(&game.to_le_bytes());
        p.resize(BODY_MAX, 0);
        let _ = self.transport.send_to(server, &packet::seal(p));
    }

    fn on_datagram(&mut self, now: Duration, from: SocketAddr, datagram: &[u8]) {
        let Some(d) = packet::open(datagram) else {
            self.stray += 1;
            if let Some(p) = self.peers.get_mut(&from) {
                p.conn.stats.malformed += 1;
            }
            return;
        };
        // Padded handshake packets are measured on the wire.
        let padded = datagram.len() >= MTU;
        let mut r = Reader(d);
        let Some(h) = packet::header(&mut r) else {
            self.stray += 1;
            return;
        };
        if h.version != TRANSPORT_VERSION && h.kind != Kind::Deny {
            if self.server && h.kind == Kind::Hello && padded {
                self.deny(from, h.salt, DenyReason::Version);
            } else {
                self.stray += 1;
            }
            return;
        }
        match (self.server, h.kind) {
            (true, Kind::Hello) => {
                // Unpadded: no answer, so a forged source gets no amplification.
                // A Hello from a connected address is a resend unless its salt
                // is new (the client restarted on the same address).
                if !padded || self.peers.get(&from).is_some_and(|p| p.conn.salt == h.salt) {
                    return;
                }
                let (Some(protocol), Some(game)) = (r.u32(), r.u64()) else {
                    return;
                };
                if let Some(reason) = self.refuse(protocol, game, false) {
                    self.deny(from, h.salt, reason);
                    return;
                }
                let mut p = packet::begin(Kind::Challenge, h.salt);
                let epoch = now.as_secs() / COOKIE_EPOCH_SECS;
                p.extend_from_slice(&self.cookie(from, h.salt, epoch).to_le_bytes());
                let _ = self.transport.send_to(from, &packet::seal(p));
            }
            (true, Kind::Response) => {
                if !padded {
                    return;
                }
                let (Some(cookie), Some(protocol), Some(game)) = (r.u64(), r.u32(), r.u64()) else {
                    return;
                };
                let epoch = now.as_secs() / COOKIE_EPOCH_SECS;
                let fresh = cookie == self.cookie(from, h.salt, epoch)
                    || (epoch > 0 && cookie == self.cookie(from, h.salt, epoch - 1));
                if !fresh {
                    self.stray += 1;
                    return;
                }
                // The same address with a new salt: the old client is gone.
                if self.peers.get(&from).is_some_and(|p| p.conn.salt != h.salt) {
                    self.close(
                        now,
                        from,
                        Some(Diagnostic::error(
                            codes::CLOSED,
                            format!("the client at {from} connected again"),
                        )),
                        "replaced by a new connection",
                    );
                }
                if !self.peers.contains_key(&from) {
                    if let Some(reason) = self.refuse(protocol, game, true) {
                        self.deny(from, h.salt, reason);
                        return;
                    }
                    self.open(now, from, h.salt);
                }
                // Repeated for a client whose Accept was lost.
                let accept = packet::seal(packet::begin(Kind::Accept, h.salt));
                let _ = self.transport.send_to(from, &accept);
            }
            (false, Kind::Challenge) if Some(from) == self.server_addr => {
                if let Handshake::Hello { salt, since, .. } = self.handshake
                    && salt == h.salt
                    && let Some(cookie) = r.u64()
                {
                    self.handshake = Handshake::Response {
                        salt,
                        cookie,
                        last: None,
                        since,
                    };
                }
            }
            (false, Kind::Accept) if Some(from) == self.server_addr => {
                if let Handshake::Response { salt, .. } = self.handshake
                    && salt == h.salt
                {
                    self.handshake = Handshake::None;
                    self.open(now, from, salt);
                }
            }
            (false, Kind::Deny) if Some(from) == self.server_addr => {
                let salt = match self.handshake {
                    Handshake::Hello { salt, .. } | Handshake::Response { salt, .. } => salt,
                    Handshake::None => return,
                };
                if salt != h.salt {
                    return;
                }
                self.handshake = Handshake::None;
                let reason = self.denied(h.version, &mut r, from);
                self.events.push(Event::Disconnected {
                    peer: PeerId(self.next_id),
                    reason,
                });
            }
            (_, Kind::Data) => {
                // A data packet from the server while a client waits for a
                // lost Accept means the server accepted it.
                if !self.server
                    && Some(from) == self.server_addr
                    && let Handshake::Response { salt, .. } = self.handshake
                    && salt == h.salt
                {
                    self.handshake = Handshake::None;
                    self.open(now, from, salt);
                }
                let Some(peer) = self.peers.get_mut(&from) else {
                    self.stray += 1;
                    return;
                };
                if peer.conn.salt != h.salt {
                    self.stray += 1;
                    return;
                }
                peer.conn.stats.packets_received += 1;
                peer.conn.stats.bytes_received += datagram.len() as u64;
                let received = peer.conn.receive(now, &d[HEADER..]);
                if peer.conn.ack_due() {
                    let ack = peer.conn.ack(now);
                    let _ = self.transport.send_to(from, &ack);
                }
                if let Received::Messages(m) = received {
                    let id = self.peers[&from].id;
                    self.events
                        .extend(m.into_iter().map(|(channel, bytes)| Event::Message {
                            peer: id,
                            channel,
                            bytes,
                        }));
                }
            }
            (_, Kind::Disconnect) => {
                let Some(peer) = self.peers.get(&from) else {
                    return;
                };
                if peer.conn.salt != h.salt {
                    return;
                }
                let id = peer.id;
                let reason = r
                    .u8()
                    .and_then(|n| r.take(n as usize))
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
                self.peers.remove(&from);
                self.by_id.remove(&id);
                let who = if self.server {
                    "the client"
                } else {
                    "the server"
                };
                let message = if reason.is_empty() {
                    format!("{who} at {from} closed the connection")
                } else {
                    format!("{who} at {from} closed the connection: {reason}")
                };
                self.events.push(Event::Disconnected {
                    peer: id,
                    reason: Diagnostic::error(codes::CLOSED, message),
                });
            }
            _ => self.stray += 1,
        }
    }

    /// Why a server refuses a client with `protocol` and `game`, if it does.
    fn refuse(&self, protocol: u32, game: u64, opening: bool) -> Option<DenyReason> {
        if protocol != self.config.protocol {
            Some(DenyReason::Version)
        } else if game != self.config.game {
            Some(DenyReason::Game)
        } else if opening && self.peers.len() >= self.config.max_connections {
            Some(DenyReason::Full)
        } else {
            None
        }
    }

    fn deny(&mut self, to: SocketAddr, salt: u64, reason: DenyReason) {
        let mut p = packet::begin(Kind::Deny, salt);
        p.push(reason as u8);
        p.extend_from_slice(&self.config.protocol.to_le_bytes());
        p.extend_from_slice(&self.config.game.to_le_bytes());
        let _ = self.transport.send_to(to, &packet::seal(p));
    }

    fn denied(&self, version: u16, r: &mut Reader<'_>, from: SocketAddr) -> Diagnostic {
        let reason = r.u8().and_then(DenyReason::from_u8);
        let (protocol, game) = (r.u32(), r.u64());
        match reason {
            Some(DenyReason::Full) => Diagnostic::error(codes::SERVER_FULL, format!("the server at {from} is full"))
                .with_hint("try again when a player has left"),
            Some(DenyReason::Game) => Diagnostic::error(
                codes::GAME_MISMATCH,
                format!(
                    "the server at {from} runs a different game (its game is {:016x}, this one is {:016x})",
                    game.unwrap_or(0),
                    self.config.game
                ),
            )
            .with_hint("run the same build or project content on both machines"),
            _ => Diagnostic::error(
                codes::VERSION_MISMATCH,
                format!(
                    "the server at {from} speaks network version {version}.{}, this client {TRANSPORT_VERSION}.{}",
                    protocol.unwrap_or(0),
                    self.config.protocol
                ),
            )
            .with_hint("run the same engine version on both machines"),
        }
    }
}
