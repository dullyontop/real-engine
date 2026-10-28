//! An endpoint: a server with many connections or a client with one, over a
//! [`Transport`], polled from the frame (ADR-0050 §1), and the encrypted
//! handshake that opens each connection (ADR-0067 §4).
//!
//! A frame calls [`Endpoint::poll`] first (receive, handshake, timeouts) and
//! [`Endpoint::flush`] last (send what was queued). Time is the caller's: a
//! [`Duration`] since any fixed start, so tests run on a simulated clock and a
//! browser build needs no `Instant`.
//!
//! The handshake, a client's side and then a server's:
//!
//! ```text
//! Hello (salt, padded)            ->  a Challenge for the source: a cookie, the server's key, the flags
//! Response (cookie, msg1, padded) ->  cookie, resend, versions and room, replay, msg1: Accept (msg2) or Deny
//! Accept: msg2 opens              ->  the session keys; every later packet is sealed
//! ```

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::Duration;

use re_core::Diagnostic;
use subtle::ConstantTimeEq;
use x25519_dalek::StaticSecret;
use zeroize::Zeroizing;

use crate::codes;
use crate::conn::{Channel, Conn, ConnStats, Received};
use crate::keys::{self, Fingerprint, JoinCode, ServerKey};
use crate::noise::{self, Initiator, Session};
use crate::packet::{
    self, COOKIE, DenyReason, FRAGMENT, HEADER, KEY, Kind, MAX_MESSAGE, Packet, TRANSPORT_VERSION,
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
/// The most first handshake messages a server remembers answering, for its
/// replay check; past it the oldest are forgotten early.
const MAX_ANSWERED: usize = 65_536;
/// What every handshake's prologue starts with (ADR-0067 §3).
const PROLOGUE: &[u8] = b"real-engine/net/2";

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
    /// The most reliable bytes a connection may have queued and unacked,
    /// each message counting 64 bytes more; a send past it is refused
    /// (`RE8908`), so a sender coalesces or waits.
    /// A connection whose sent fragments see no ack for [`Config::timeout`]
    /// is closed (`RE8906`).
    pub max_backlog: usize,
    /// A server's key (ADR-0067 §7). A server without one makes one when it
    /// starts, so its fingerprint changes each start; a server that keeps its
    /// data passes the one [`ServerKey::load_or_make`] keeps.
    pub key: Option<ServerKey>,
    /// A server's join code: only a client that gives it joins. A client's:
    /// the code it gives. A client stops with `RE8913` when the server's need
    /// for a code differs (ADR-0067 §8).
    pub join_code: Option<JoinCode>,
    /// A client: the fingerprint the server's key must have (`RE8914`
    /// otherwise).
    pub pin: Option<Fingerprint>,
    /// This endpoint's engine, for people: a server names it in a Deny (at
    /// most 64 bytes), and a refused client puts both in its diagnostic. The
    /// defaults set the engine's version, which every crate of the workspace
    /// shares (ADR-0067 §6).
    pub engine: String,
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
            key: None,
            join_code: None,
            pin: None,
            engine: env!("CARGO_PKG_VERSION").to_string(),
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
    /// says why (`RE8902`–`RE8907`, `RE8912`–`RE8914`). Closing a connection
    /// yourself with [`Endpoint::disconnect`] reports nothing.
    Disconnected {
        /// The connection (a failed handshake reports the id the connection
        /// would have had).
        peer: PeerId,
        /// Why.
        reason: Diagnostic,
    },
}

/// A client's handshake.
enum Handshake {
    /// A server, or a client that is connected, gave up or was refused.
    None,
    /// Sending Hello until a Challenge with this salt arrives.
    Hello {
        salt: u64,
        /// The ephemeral key the first handshake message will carry.
        ephemeral: Box<StaticSecret>,
        last: Option<Duration>,
        since: Option<Duration>,
        /// Answers that arrived and failed the handshake.
        failed: u32,
    },
    /// Sending this Response until Accept or Deny.
    Response {
        salt: u64,
        response: Vec<u8>,
        initiator: Box<Initiator>,
        last: Option<Duration>,
        since: Option<Duration>,
        failed: u32,
    },
}

struct Peer {
    id: PeerId,
    conn: Conn,
    /// A server's Accept, sent again for a resent Response until the client's
    /// first sealed packet shows it has the keys.
    accept: Option<Vec<u8>>,
}

/// A server or a client over transport `T`.
pub struct Endpoint<T: Transport> {
    transport: T,
    config: Config,
    server: bool,
    peers: HashMap<SocketAddr, Peer>,
    by_id: HashMap<PeerId, SocketAddr>,
    next_id: u32,
    /// The key of a server's handshake cookies, random per start.
    cookie_key: Zeroizing<[u8; 32]>,
    /// The ephemeral keys of the first handshake messages a server answered,
    /// with their cookie epochs, oldest first, and as a set: a Response
    /// replayed once its connection is gone is dropped (ADR-0067 §4).
    answered: VecDeque<(u64, [u8; KEY])>,
    answered_keys: HashSet<[u8; KEY]>,
    handshake: Handshake,
    server_addr: Option<SocketAddr>,
    /// A client's server's public key, from the Challenge it answered.
    server_key: Option<[u8; KEY]>,
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

/// The prologue both sides mix into the handshake (ADR-0067 §3), which binds
/// it to this transport version, this protocol and game, and this attempt.
pub(crate) fn prologue(protocol: u32, game: u64, salt: u64) -> Vec<u8> {
    let mut p = PROLOGUE.to_vec();
    p.extend_from_slice(&protocol.to_le_bytes());
    p.extend_from_slice(&game.to_le_bytes());
    p.extend_from_slice(&salt.to_le_bytes());
    p
}

impl<T: Transport> Endpoint<T> {
    /// A server that accepts connections on `transport`.
    ///
    /// # Panics
    ///
    /// When the platform has no secure random numbers for its keys, or in a
    /// browser; [`Endpoint::try_server`] reports those as errors instead.
    pub fn server(transport: T, config: Config) -> Endpoint<T> {
        Endpoint::try_server(transport, config).unwrap_or_else(|d| panic!("{d}"))
    }

    /// A server that accepts connections on `transport`, with
    /// [`Config::key`] or a new key.
    ///
    /// # Errors
    ///
    /// `RE8916` when the platform has no secure random numbers, and `RE8911`
    /// in a browser, which can only join a server (ADR-0049 §10), so its
    /// build leaves out the server's half of the handshake.
    pub fn try_server(transport: T, mut config: Config) -> Result<Endpoint<T>, Diagnostic> {
        if cfg!(target_arch = "wasm32") {
            return Err(Diagnostic::error(
                codes::CANNOT_SERVE,
                "a browser cannot serve: it can only join a server",
            ));
        }
        if config.key.is_none() {
            config.key = Some(ServerKey::generate()?);
        }
        Endpoint::new(transport, config, true)
    }

    /// A client that connects to `server` over `transport`. The handshake
    /// starts at the first flush; [`Event::Connected`] reports success.
    ///
    /// # Panics
    ///
    /// When the platform has no secure random numbers for its keys;
    /// [`Endpoint::try_client`] reports that as `RE8916` instead.
    pub fn client(transport: T, server: SocketAddr, config: Config) -> Endpoint<T> {
        Endpoint::try_client(transport, server, config).unwrap_or_else(|d| panic!("{d}"))
    }

    /// A client that connects to `server` over `transport`.
    ///
    /// # Errors
    ///
    /// `RE8916` when the platform has no secure random numbers.
    pub fn try_client(
        transport: T,
        server: SocketAddr,
        config: Config,
    ) -> Result<Endpoint<T>, Diagnostic> {
        let mut e = Endpoint::new(transport, config, false)?;
        e.server_addr = Some(server);
        let mut salt = [0u8; 8];
        keys::random(&mut salt)?;
        e.handshake = Handshake::Hello {
            salt: u64::from_le_bytes(salt),
            ephemeral: Box::new(keys::random_secret()?),
            last: None,
            since: None,
            failed: 0,
        };
        Ok(e)
    }

    fn new(transport: T, config: Config, server: bool) -> Result<Endpoint<T>, Diagnostic> {
        let mut cookie_key = Zeroizing::new([0u8; 32]);
        if server {
            keys::random(&mut cookie_key[..])?;
        }
        Ok(Endpoint {
            transport,
            config,
            server,
            peers: HashMap::new(),
            by_id: HashMap::new(),
            next_id: 0,
            cookie_key,
            answered: VecDeque::new(),
            answered_keys: HashSet::new(),
            handshake: Handshake::None,
            server_addr: None,
            server_key: None,
            events: Vec::new(),
            stray: 0,
            buf: vec![0; 65536],
        })
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

    /// A server's own key's fingerprint, which it prints for players; a
    /// client's server's, once its Challenge arrived (ADR-0067 §7).
    pub fn fingerprint(&self) -> Option<Fingerprint> {
        if self.server {
            self.config.key.as_ref().map(ServerKey::fingerprint)
        } else {
            self.server_key.as_ref().map(Fingerprint::of)
        }
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

    /// A connection's peer for people: its address, and how it is reached
    /// when that is not plain UDP ([`Transport::describe_peer`]).
    pub fn describe(&self, peer: PeerId) -> Option<String> {
        self.addr_of(peer).map(|a| self.transport.describe_peer(a))
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
    /// belong to one: strangers, damaged or replayed handshake packets, and
    /// sealed packets from an address with no connection.
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
                    "connection {} already has {backlog} reliable bytes waiting (64 per message included), and {} more would pass the {max_backlog}-byte limit",
                    peer.0,
                    bytes.len() + crate::conn::FRAGMENT_COST
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
                Ok(Some((n, from))) => {
                    let n = n.min(buf.len());
                    self.on_datagram(now, from, &mut buf[..n]);
                }
                Ok(None) | Err(_) => {
                    self.buf = buf;
                    break;
                }
            }
            self.buf = buf;
        }
        // A link the transport lost (a closed WebSocket) closes its
        // connection now rather than at the timeout.
        for (addr, reason) in self.transport.take_lost() {
            self.lost(now, addr, reason);
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
            let at = self.transport.describe_peer(a);
            self.close(
                now,
                a,
                Some(Diagnostic::error(
                    codes::TIMED_OUT,
                    format!(
                        "{what} at {at} sent nothing for {} s",
                        timeout.as_secs_f64()
                    ),
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
            let at = self.transport.describe_peer(a);
            let closing = if peer.conn.stalled(now, timeout) {
                let backlog = peer.conn.backlog();
                Some((
                    format!(
                        "{at} acknowledged nothing for {} s ({backlog} bytes waiting)",
                        timeout.as_secs_f64()
                    ),
                    "stopped acknowledging",
                ))
            } else if peer.conn.spent() {
                Some((
                    format!("the connection to {at} sealed 2^64 - 1 packets, all it may"),
                    "out of packet numbers",
                ))
            } else {
                None
            };
            if let Some((message, reason)) = closing {
                let event = Diagnostic::error(codes::TIMED_OUT, message);
                self.close(now, a, Some(event), reason);
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

    /// Closes the connection to `addr` and releases its link (a WebSocket).
    fn close(&mut self, now: Duration, addr: SocketAddr, event: Option<Diagnostic>, reason: &str) {
        if self.drop_peer(now, addr, event, reason) {
            self.transport.release(addr);
        }
    }

    /// Closes the connection to `addr`, keeping its link; true if there was
    /// one. Its keys are wiped as it drops, after its sealed Disconnects.
    fn drop_peer(
        &mut self,
        now: Duration,
        addr: SocketAddr,
        event: Option<Diagnostic>,
        reason: &str,
    ) -> bool {
        let Some(mut p) = self.peers.remove(&addr) else {
            return false;
        };
        self.by_id.remove(&p.id);
        self.transport.advance(now);
        for _ in 0..DISCONNECT_COPIES {
            if let Some(d) = p.conn.disconnect(reason) {
                let _ = self.transport.send_to(addr, &d);
            }
        }
        if let Some(reason) = event {
            self.events.push(Event::Disconnected { peer: p.id, reason });
        }
        true
    }

    /// The transport lost its link to `addr`: closes that connection, or
    /// fails a client's handshake with its server.
    fn lost(&mut self, now: Duration, addr: SocketAddr, reason: Diagnostic) {
        if self.peers.contains_key(&addr) {
            self.close(now, addr, Some(reason), "the link closed");
        } else if Some(addr) == self.server_addr && !matches!(self.handshake, Handshake::None) {
            self.fail(reason);
        }
    }

    /// Ends a client's handshake with `reason`.
    fn fail(&mut self, reason: Diagnostic) {
        self.handshake = Handshake::None;
        self.events.push(Event::Disconnected {
            peer: PeerId(self.next_id),
            reason,
        });
    }

    /// A server's key (it always has one).
    fn own_key(&self) -> &ServerKey {
        self.config.key.as_ref().expect("a server has a key")
    }

    /// The cookie for `addr` and `salt` in cookie epoch `epoch`: the first
    /// 16 bytes of an HMAC-BLAKE2s of them under the cookie key, so only this
    /// server makes them, and only for a source that receives its Challenges.
    fn cookie(&self, addr: SocketAddr, salt: u64, epoch: u64) -> [u8; COOKIE] {
        let ip = match addr.ip() {
            std::net::IpAddr::V4(ip) => ip.to_ipv6_mapped().octets(),
            std::net::IpAddr::V6(ip) => ip.octets(),
        };
        let mac = noise::hmac(
            &self.cookie_key,
            &[
                &ip,
                &addr.port().to_le_bytes(),
                &salt.to_le_bytes(),
                &epoch.to_le_bytes(),
            ],
        );
        let mut cookie = [0u8; COOKIE];
        cookie.copy_from_slice(&mac[..COOKIE]);
        cookie
    }

    fn open(&mut self, now: Duration, addr: SocketAddr, salt: u64, keys: Session) -> PeerId {
        let id = PeerId(self.next_id);
        self.next_id += 1;
        let c = &self.config;
        let conn = Conn::new(salt, keys, now, c.send_rate, c.rto_min, c.keepalive);
        self.peers.insert(
            addr,
            Peer {
                id,
                conn,
                accept: None,
            },
        );
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
        let mut give_up = None;
        let mut send = None;
        match &mut self.handshake {
            Handshake::None => {}
            Handshake::Hello {
                salt,
                last,
                since,
                failed,
                ..
            } => {
                let started = *since.get_or_insert(now);
                if now.saturating_sub(started) > timeout {
                    give_up = Some(*failed);
                } else if last.is_none_or(|t| now.saturating_sub(t) >= HANDSHAKE_RESEND) {
                    *last = Some(now);
                    send = Some(packet::hello(*salt, protocol, game));
                }
            }
            Handshake::Response {
                response,
                last,
                since,
                failed,
                ..
            } => {
                let started = *since.get_or_insert(now);
                if now.saturating_sub(started) > timeout {
                    give_up = Some(*failed);
                } else if last.is_none_or(|t| now.saturating_sub(t) >= HANDSHAKE_RESEND) {
                    *last = Some(now);
                    send = Some(response.clone());
                }
            }
        }
        if let Some(failed) = give_up {
            let at = self.transport.describe_peer(server);
            let failed = match failed {
                0 => String::new(),
                1 => "; one answer came that failed the handshake (changed on the way, or from another server)".into(),
                n => format!("; {n} answers came that failed the handshake (changed on the way, or from another server)"),
            };
            self.fail(
                Diagnostic::error(
                    codes::NO_ANSWER,
                    format!(
                        "no answer from a server at {at} after {} s{failed}",
                        timeout.as_secs_f64()
                    ),
                )
                .with_hint(
                    "check the address and port, and that the server is running and reachable",
                ),
            );
            return;
        }
        if let Some(p) = send {
            let _ = self.transport.send_to(server, &p);
        }
    }

    /// A datagram from `from`: decrypted in place when it is a sealed one.
    fn on_datagram(&mut self, now: Duration, from: SocketAddr, datagram: &mut [u8]) {
        let packet = packet::decode(datagram);
        match packet {
            Packet::Sealed { kind, number } => self.on_sealed(now, from, kind, number, datagram),
            Packet::Stray => {
                self.stray += 1;
                if let Some(p) = self.peers.get_mut(&from) {
                    p.conn.stats.malformed += 1;
                }
            }
            Packet::OtherHello { salt, .. } if self.serves() => {
                self.deny(from, salt, DenyReason::Version);
            }
            Packet::Hello {
                salt,
                protocol,
                game,
            } if self.serves() => self.on_hello(now, from, salt, protocol, game),
            Packet::Response {
                salt,
                cookie,
                protocol,
                game,
                msg1,
            } if self.serves() => self.on_response(now, from, salt, &cookie, protocol, game, msg1),
            Packet::Challenge {
                salt,
                cookie,
                server_key,
                code_required,
            } if self.is_from_server(from) => {
                self.on_challenge(from, salt, &cookie, &server_key, code_required);
            }
            Packet::Accept { salt, msg2 } if self.is_from_server(from) => {
                self.on_accept(now, from, salt, msg2);
            }
            Packet::Deny {
                version,
                salt,
                reason,
                protocol,
                game,
                label,
            } if self.is_from_server(from) => {
                let ours = match &self.handshake {
                    Handshake::Hello { salt, .. } | Handshake::Response { salt, .. } => *salt,
                    Handshake::None => return,
                };
                if ours != salt {
                    self.stray += 1;
                    return;
                }
                let reason = self.denied(version, reason, protocol, game, label, from);
                self.fail(reason);
            }
            _ => self.stray += 1,
        }
    }

    /// True for a server. Never in a browser, so its build leaves the
    /// server's half of the handshake out.
    fn serves(&self) -> bool {
        !cfg!(target_arch = "wasm32") && self.server
    }

    /// True when a client hears from the address it connects to.
    fn is_from_server(&self, from: SocketAddr) -> bool {
        !self.server && Some(from) == self.server_addr
    }

    /// A server's Hello: a Challenge for the source, which costs one HMAC
    /// and keeps nothing.
    fn on_hello(&mut self, now: Duration, from: SocketAddr, salt: u64, protocol: u32, game: u64) {
        // From a connected address, a resend unless its salt is new (the
        // client restarted on the same address).
        if self.peers.get(&from).is_some_and(|p| p.conn.salt == salt) {
            return;
        }
        if let Some(reason) = self.refuse(protocol, game, None) {
            self.deny(from, salt, reason);
            return;
        }
        let cookie = self.cookie(from, salt, now.as_secs() / COOKIE_EPOCH_SECS);
        let p = packet::challenge(
            salt,
            &cookie,
            &self.own_key().public_key(),
            self.config.join_code.is_some(),
        );
        let _ = self.transport.send_to(from, &p);
    }

    /// A server's Response, checked in order of cost (ADR-0067 §4).
    #[expect(
        clippy::too_many_arguments,
        reason = "the Response's fields, as the decoder gives them"
    )]
    fn on_response(
        &mut self,
        now: Duration,
        from: SocketAddr,
        salt: u64,
        cookie: &[u8; COOKIE],
        protocol: u32,
        game: u64,
        msg1: &[u8],
    ) {
        // 1. The cookie: the source received this epoch's Challenge or the
        //    last one's.
        let epoch = now.as_secs() / COOKIE_EPOCH_SECS;
        let current = cookie.ct_eq(&self.cookie(from, salt, epoch));
        let previous = cookie.ct_eq(&self.cookie(from, salt, epoch.saturating_sub(1)));
        if !bool::from(current | previous) {
            self.stray += 1;
            return;
        }
        // 2. A resend for a connection that opened: its Accept was lost.
        if let Some(p) = self.peers.get(&from)
            && p.conn.salt == salt
        {
            if let Some(accept) = &p.accept {
                let _ = self.transport.send_to(from, accept);
            }
            return;
        }
        // 3. The versions, the game and room for one more (a connection from
        //    this address will be replaced, so it does not count).
        if let Some(reason) = self.refuse(protocol, game, Some(from)) {
            self.deny(from, salt, reason);
            return;
        }
        // 4. A first message answered within the cookie's life, whose
        //    connection is gone: a replay.
        self.forget_answered(epoch);
        let ephemeral: [u8; KEY] = msg1[..KEY].try_into().expect("msg1 holds a key");
        if self.answered_keys.contains(&ephemeral) {
            self.stray += 1;
            return;
        }
        // 5. The Diffie-Hellman work: msg1 must open under this server's key
        //    and its join code's.
        let Ok(e) = keys::random_secret() else {
            self.stray += 1;
            return;
        };
        let prologue = prologue(protocol, game, salt);
        let answer = {
            let key = self.own_key();
            let public = key.public_key();
            let psk = keys::psk(&public, self.config.join_code.as_ref());
            noise::respond(key.secret(), &public, &psk, &prologue, msg1, &e, &[])
        };
        let Ok((_, msg2, session, _)) = answer else {
            self.deny(from, salt, DenyReason::Handshake);
            return;
        };
        self.remember_answered(epoch, ephemeral);
        // 6. A client that restarted on the same address replaces its old
        //    connection, whose link the new one keeps.
        if self.peers.contains_key(&from) {
            let at = self.transport.describe_peer(from);
            self.drop_peer(
                now,
                from,
                Some(Diagnostic::error(
                    codes::CLOSED,
                    format!("the client at {at} connected again"),
                )),
                "replaced by a new connection",
            );
        }
        let accept = packet::accept(salt, &msg2);
        let _ = self.transport.send_to(from, &accept);
        self.open(now, from, salt, session);
        self.peers.get_mut(&from).expect("opened").accept = Some(accept);
    }

    /// Forgets the answered first messages whose cookies have expired.
    fn forget_answered(&mut self, epoch: u64) {
        while let Some(&(e, key)) = self.answered.front() {
            if e + 2 > epoch && self.answered.len() <= MAX_ANSWERED {
                break;
            }
            self.answered.pop_front();
            self.answered_keys.remove(&key);
        }
    }

    fn remember_answered(&mut self, epoch: u64, key: [u8; KEY]) {
        if self.answered_keys.insert(key) {
            self.answered.push_back((epoch, key));
        }
    }

    /// A client's Challenge: checks the server's key and its need for a join
    /// code, then writes the first handshake message.
    fn on_challenge(
        &mut self,
        from: SocketAddr,
        salt: u64,
        cookie: &[u8; COOKIE],
        server_key: &[u8; KEY],
        code_required: bool,
    ) {
        let Handshake::Hello {
            salt: ours,
            since,
            failed,
            ..
        } = self.handshake
        else {
            // A Challenge after the first one: its Response is on its way.
            return;
        };
        if ours != salt {
            self.stray += 1;
            return;
        }
        let at = self.transport.describe_peer(from);
        let fingerprint = Fingerprint::of(server_key);
        if let Some(pin) = self.config.pin
            && pin != fingerprint
        {
            self.fail(
                Diagnostic::error(
                    codes::KEY_MISMATCH,
                    format!("the server at {at} has the key {fingerprint}, not the pinned {pin}"),
                )
                .with_hint("if the server's key changed (its server.key was deleted), pin the fingerprint it prints now; if not, something on the way is posing as the server"),
            );
            return;
        }
        match (code_required, self.config.join_code.is_some()) {
            (true, false) => {
                self.fail(
                    Diagnostic::error(
                        codes::JOIN_CODE_MISMATCH,
                        format!("the server at {at} needs a join code"),
                    )
                    .with_hint("ask the host for the code the server printed when it started"),
                );
                return;
            }
            (false, true) => {
                self.fail(
                    Diagnostic::error(
                        codes::JOIN_CODE_MISMATCH,
                        format!(
                            "the server at {at} takes no join code, and this client was given one"
                        ),
                    )
                    .with_hint("check the address, or join without the code"),
                );
                return;
            }
            _ => {}
        }
        let started = {
            let Handshake::Hello { ephemeral, .. } = &self.handshake else {
                unreachable!("matched above")
            };
            let psk = keys::psk(server_key, self.config.join_code.as_ref());
            let prologue = prologue(self.config.protocol, self.config.game, salt);
            Initiator::start(server_key, &psk, &prologue, ephemeral, &[])
        };
        match started {
            Ok((initiator, msg1)) => {
                let response =
                    packet::response(salt, cookie, self.config.protocol, self.config.game, &msg1);
                self.server_key = Some(*server_key);
                self.handshake = Handshake::Response {
                    salt,
                    response,
                    initiator: Box::new(initiator),
                    last: None,
                    since,
                    failed,
                };
            }
            // A key of low order is no server's: keep waiting.
            Err(_) => {
                if let Handshake::Hello { failed, .. } = &mut self.handshake {
                    *failed += 1;
                }
            }
        }
    }

    /// A client's Accept: its second handshake message gives the session.
    fn on_accept(&mut self, now: Duration, from: SocketAddr, salt: u64, msg2: &[u8]) {
        let Handshake::Response {
            salt: ours,
            initiator,
            failed,
            ..
        } = &mut self.handshake
        else {
            self.stray += 1;
            return;
        };
        if *ours != salt {
            self.stray += 1;
            return;
        }
        match initiator.finish(msg2) {
            Ok((_, session, _)) => {
                self.handshake = Handshake::None;
                self.open(now, from, salt, session);
            }
            // Changed on the way or forged: the handshake waits for the real
            // one, and RE8902 says it came if none does.
            Err(_) => *failed += 1,
        }
    }

    /// A sealed packet: opened in place with its connection's keys.
    fn on_sealed(
        &mut self,
        now: Duration,
        from: SocketAddr,
        kind: Kind,
        number: u64,
        datagram: &mut [u8],
    ) {
        let Some(peer) = self.peers.get_mut(&from) else {
            self.stray += 1;
            return;
        };
        let Some(len) = peer.conn.open(number, datagram) else {
            return;
        };
        // The client has its keys, so its Accept arrived.
        peer.accept = None;
        let body = &datagram[HEADER..HEADER + len];
        if kind == Kind::Data {
            peer.conn.stats.packets_received += 1;
            peer.conn.stats.bytes_received += datagram.len() as u64;
            let received = peer.conn.receive(now, body);
            if peer.conn.ack_due()
                && let Some(ack) = peer.conn.ack(now)
            {
                let _ = self.transport.send_to(from, &ack);
            }
            if let Received::Messages(m) = received {
                let id = peer.id;
                self.events
                    .extend(m.into_iter().map(|(channel, bytes)| Event::Message {
                        peer: id,
                        channel,
                        bytes,
                    }));
            }
            return;
        }
        // A Disconnect, which only the peer can seal.
        let reason = packet::disconnect(body);
        let id = peer.id;
        let who = if self.server {
            "the client"
        } else {
            "the server"
        };
        let at = self.transport.describe_peer(from);
        self.peers.remove(&from);
        self.by_id.remove(&id);
        self.transport.release(from);
        let message = if reason.is_empty() {
            format!("{who} at {at} closed the connection")
        } else {
            format!("{who} at {at} closed the connection: {reason}")
        };
        self.events.push(Event::Disconnected {
            peer: id,
            reason: Diagnostic::error(codes::CLOSED, message),
        });
    }

    /// Why a server refuses a client with `protocol` and `game`, if it does;
    /// with `joining`, also when it has no room for one more connection from
    /// that address.
    fn refuse(&self, protocol: u32, game: u64, joining: Option<SocketAddr>) -> Option<DenyReason> {
        let open =
            self.peers.len() - joining.map_or(0, |a| usize::from(self.peers.contains_key(&a)));
        if protocol != self.config.protocol {
            Some(DenyReason::Version)
        } else if game != self.config.game {
            Some(DenyReason::Game)
        } else if joining.is_some() && open >= self.config.max_connections {
            Some(DenyReason::Full)
        } else {
            None
        }
    }

    fn deny(&mut self, to: SocketAddr, salt: u64, reason: DenyReason) {
        let c = &self.config;
        let p = packet::deny_packet(salt, reason, c.protocol, c.game, &c.engine);
        let _ = self.transport.send_to(to, &p);
    }

    /// A client's diagnostic for a server's Deny. A Deny of another
    /// transport version is a version mismatch, whatever its reason.
    fn denied(
        &self,
        version: u16,
        reason: Option<u8>,
        protocol: Option<u32>,
        game: Option<u64>,
        label: Option<&[u8]>,
        from: SocketAddr,
    ) -> Diagnostic {
        let at = self.transport.describe_peer(from);
        let engines = match label.filter(|l| !l.is_empty()) {
            Some(l) => format!(
                " (the server runs Real Engine {}, this client {})",
                packet::text(l),
                self.config.engine
            ),
            None => String::new(),
        };
        let reason = reason
            .and_then(DenyReason::from_u8)
            .filter(|_| version == TRANSPORT_VERSION);
        match reason {
            Some(DenyReason::Full) => Diagnostic::error(codes::SERVER_FULL, format!("the server at {at} is full"))
                .with_hint("try again when a player has left"),
            Some(DenyReason::Game) => Diagnostic::error(
                codes::GAME_MISMATCH,
                format!(
                    "the server at {at} runs a different game (its game is {:016x}, this one is {:016x}){engines}",
                    game.unwrap_or(0),
                    self.config.game
                ),
            )
            .with_hint("run the same build or project content on both machines"),
            Some(DenyReason::Handshake) => match &self.config.join_code {
                Some(_) => Diagnostic::error(
                    codes::HANDSHAKE_REFUSED,
                    format!("the server at {at} refused the join code"),
                )
                .with_hint("check the code with the host: a server makes a new one each time it starts"),
                None => Diagnostic::error(
                    codes::HANDSHAKE_REFUSED,
                    format!("the server at {at} refused the handshake: it may need a join code that its answer did not ask for, or something on the way changed that answer"),
                )
                .with_hint("ask the host whether the game needs a join code"),
            },
            Some(DenyReason::Version) | None => {
                let hint = if version < TRANSPORT_VERSION {
                    "the server runs an engine from before online play was encrypted, which this client cannot join: run the same engine version on both machines"
                } else {
                    "run the same engine version on both machines"
                };
                Diagnostic::error(
                    codes::VERSION_MISMATCH,
                    format!(
                        "the server at {at} speaks network version {version}.{}, this client {TRANSPORT_VERSION}.{}{engines}",
                        protocol.unwrap_or(0),
                        self.config.protocol
                    ),
                )
                .with_hint(hint)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prologue_is_the_one_the_snow_vectors_were_made_with() {
        let vs: serde_json::Value =
            serde_json::from_str(include_str!("../tests/data/nkpsk0-snow.json")).unwrap();
        let hex: String = prologue(7, 0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(vs[0]["init_prologue"].as_str().unwrap(), hex);
    }
}
