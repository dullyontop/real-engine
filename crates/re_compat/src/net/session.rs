//! A networked game (ADR-0050 §4 to §7): a server's sessions with its
//! network players ([`ServerNet`]) and a client's session with its server
//! ([`ClientNet`]), over a `re_net` endpoint.
//!
//! * **Join.** After the transport handshake (which checks the protocol and
//!   the game hash), the client sends `Join` with its player's name. The
//!   server adds the `Player` (named `Player<n>` unless the client gave a
//!   name, made unique with a number, `UserId` `n`), whose client queue
//!   exists before `PlayerAdded` fires, and answers `Welcome`: the player's
//!   stable id and the frame that binds the client's own copy of the scene
//!   (the server bound it at boot). Its first frame then carries everything
//!   that changed since boot, so a client that joins a game in progress gets
//!   the current state.
//! * **Frames.** Each side sends one reliable [`Frame`] per frame: the
//!   server, a connection's replication ops and the messages for its player;
//!   a client, its owned changes and its messages. An `UnreliableRemoteEvent`
//!   message goes alone in an unreliable datagram (`RE9454` when too large).
//! * **Limits** on what clients send: a message over 64 KiB or that does not
//!   decode is dropped (`RE9450`), more than `remote_rate` a second are
//!   dropped (`RE9453`), only `upstream` components of owned instances are
//!   taken, and an owned part that moves too fast stays put (`RE9452`).
//!   Every message's player is the connection's, whatever the client wrote.
//! * **Backpressure.** A connection over its backlog gets no ops that frame
//!   (its mirror keeps what it was sent, so the next diff carries the
//!   coalesced difference); messages are never coalesced, and a connection
//!   whose send is refused (4 MiB unacked) is closed.
//! * **Leave.** A disconnect or a timeout removes the player as
//!   `Players:remove_player` does; a kick (`Player:Kick` on the server)
//!   closes the connection with the kick message.
//! * **Lockstep** (§7): server frame N runs once every network player's
//!   frame N arrived, and a client frame N once the server's frame N arrived;
//!   a client's frame 0 is sent once its scripts started. A side that waits
//!   longer than the timeout with no traffic moving fails the run.
//! * **Parts.** A message larger than a transport message (a large game's
//!   first frame) crosses in pieces that the other end joins.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use re_core::Diagnostic;
use re_core::time::Instant;
use re_net::{Channel, Endpoint, Event, PeerId, Transport};
use re_scene::{Entity, World};
use re_script::ScriptHost;
use serde::{Deserialize, Serialize};

use super::replicate::{Conn, Replica, UpCheck};
use super::wire::{self, Frame};
use super::{Msg, NetLink, NetSettings, ReplicationSet};
use crate::codes;
use crate::players::{self, PlayerSpec};
use crate::runtime::{CompatVm, with_world};

/// The largest remote message a client may send.
pub const MAX_REMOTE_MESSAGE: usize = 64 * 1024;

/// The protocol version of a session: the wire format's.
pub const PROTOCOL: u32 = wire::WIRE_VERSION;

/// How often a lockstep side looks for what it waits for.
const WAIT_STEP: Duration = Duration::from_millis(1);

/// A reliable message of a session.
#[derive(Debug, Serialize, Deserialize)]
enum NetMsg {
    /// Client to server, once connected: the player's name, if it chose one.
    Join { name: Option<String> },
    /// Server to client: its `Player`'s stable id, the frame that binds
    /// the client's copy of the scene, and the server frame it joins at (its
    /// own frames count from there, and in lockstep it waits for that one).
    Welcome {
        player: String,
        bind: Frame,
        tick: u64,
    },
    /// Either way, once a frame.
    Frame(Frame),
    /// A piece of a message too large for one transport message (a large
    /// game's first frame); the pieces up to `last` join into it.
    Part { last: bool, bytes: Vec<u8> },
}

/// The most bytes of a message one [`NetMsg::Part`] carries.
const PART: usize = re_net::MAX_MESSAGE - 1024;

/// The largest message a server takes from a client in parts.
const MAX_CLIENT_JOINED: usize = 4 * re_net::MAX_MESSAGE;

/// The largest message a client takes from its server in parts: as much as
/// a server may have unacknowledged (`re_net::Config::max_backlog`).
const MAX_SERVER_JOINED: usize = 4 * re_net::MAX_MESSAGE;

/// The most frames of a lockstep player that may wait, ahead of the server.
const MAX_AHEAD: usize = 8;

/// The most unreliable messages of a player that may wait for a frame.
const MAX_UNRELIABLE_WAITING: usize = 4096;

/// Joins [`NetMsg::Part`]s back into the message they carry.
#[derive(Default)]
struct Parts(Vec<u8>);

impl Parts {
    /// The message `bytes` is or completes: `Ok(None)` while parts are
    /// missing, `Err` for a message that does not decode or would be larger
    /// than `max`.
    fn take(&mut self, bytes: &[u8], max: usize) -> Result<Option<NetMsg>, ()> {
        match NetMsg::decode(bytes) {
            Some(NetMsg::Part { last, bytes }) => {
                if self.0.len() + bytes.len() > max {
                    return Err(());
                }
                self.0.extend_from_slice(&bytes);
                if !last {
                    return Ok(None);
                }
                match NetMsg::decode(&std::mem::take(&mut self.0)) {
                    Some(NetMsg::Part { .. }) | None => Err(()),
                    Some(m) => Ok(Some(m)),
                }
            }
            Some(m) if self.0.is_empty() => Ok(Some(m)),
            _ => Err(()),
        }
    }
}

/// Sends `msg` reliably to `peer`, in parts when it is too large for one
/// transport message.
///
/// # Errors
///
/// Why the endpoint refused it (the connection is gone, or too much is
/// unacknowledged); the connection must close, since the other end may have
/// some of the parts.
fn send_msg<T: Transport>(end: &mut Endpoint<T>, peer: PeerId, msg: &NetMsg) -> Result<(), String> {
    let bytes = msg.encode();
    let refused = |e: Diagnostic| e.message;
    if bytes.len() <= PART {
        return end.send(peer, Channel::Reliable, &bytes).map_err(refused);
    }
    let count = bytes.len().div_ceil(PART);
    for (i, chunk) in bytes.chunks(PART).enumerate() {
        let part = NetMsg::Part {
            last: i + 1 == count,
            bytes: chunk.to_vec(),
        };
        end.send(peer, Channel::Reliable, &part.encode())
            .map_err(refused)?;
    }
    Ok(())
}

impl NetMsg {
    fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("a session message always encodes")
    }

    fn decode(bytes: &[u8]) -> Option<NetMsg> {
        postcard::from_bytes(bytes).ok()
    }
}

fn warn(code: re_core::Code, msg: impl Into<String>) -> Diagnostic {
    Diagnostic::warning(code, msg)
}

/// Sends the messages of `queue` with `frame` on `peer`: unreliable ones in
/// their own datagrams, the rest in the frame. Returns the warnings and
/// whether the reliable send was refused.
fn send_frame<T: Transport>(
    end: &mut Endpoint<T>,
    peer: PeerId,
    mut frame: Frame,
    msgs: Vec<(bool, Vec<u8>)>,
    out: &mut Vec<Diagnostic>,
) -> Result<(), String> {
    for (unreliable, bytes) in msgs {
        if !unreliable {
            frame.msgs.push(bytes);
            continue;
        }
        if bytes.len() > re_net::FRAGMENT {
            out.push(warn(
                codes::UNRELIABLE_SIZE,
                format!(
                    "an UnreliableRemoteEvent message of {} bytes was dropped: more than fits in one datagram ({})",
                    bytes.len(),
                    re_net::FRAGMENT
                ),
            ));
            continue;
        }
        let _ = end.send(peer, Channel::Unreliable, &bytes);
    }
    send_msg(end, peer, &NetMsg::Frame(frame))
}

// ------------------------------------------------------------------ server

/// A network player's connection.
struct Remote {
    conn: Conn,
    /// Its `Player`'s stable id and entity, once it joined.
    player: Option<(String, Entity)>,
    /// Frames that arrived and were not taken yet.
    frames: VecDeque<Frame>,
    /// Frames whose messages were delivered; their ops wait for the
    /// replication step.
    ops: Vec<Frame>,
    /// Unreliable messages that arrived.
    unreliable: Vec<Vec<u8>>,
    /// The newest client frame taken.
    tick: Option<u64>,
    /// Simulation seconds since its last frame was applied.
    since: f32,
    /// Messages taken this second, and how many were dropped.
    rate: (u64, u32, u32),
    /// The second `RE9452` was last reported.
    speed_reported: Option<u64>,
    /// A message arriving in parts.
    parts: Parts,
}

impl Remote {
    fn new(conn: Conn) -> Remote {
        Remote {
            conn,
            player: None,
            frames: VecDeque::new(),
            ops: Vec::new(),
            unreliable: Vec::new(),
            tick: None,
            since: 0.0,
            rate: (0, 0, 0),
            speed_reported: None,
            parts: Parts::default(),
        }
    }
}

/// A server's sessions with its network players.
pub struct ServerNet<T: Transport> {
    end: Endpoint<T>,
    remotes: BTreeMap<PeerId, Remote>,
    /// A connection bound to the scene at boot; each join gets a copy.
    template: Conn,
    /// The frame that binds a client's copy of the scene to `template`.
    bind: Frame,
    settings: NetSettings,
    set: ReplicationSet,
    lockstep: bool,
    timeout: Duration,
    clock: Instant,
    /// The server frame being run.
    tick: u64,
    /// Simulation seconds since the server started.
    time: f64,
    /// Players whose connection the replication step closed, removed from
    /// the game at the next receive (which has the VM).
    lost: Vec<(String, Entity)>,
    /// Connections the replication step saw close.
    dropped: Vec<(PeerId, Diagnostic)>,
}

impl<T: Transport> ServerNet<T> {
    /// Serves `server`, which holds the scene as loaded (before any script
    /// ran), on `end`. `settings` are the project's `[compat.net]`; with
    /// `lockstep`, each frame waits for every player's (§7).
    pub fn new(
        end: Endpoint<T>,
        server: &mut World,
        settings: NetSettings,
        lockstep: bool,
    ) -> ServerNet<T> {
        let set = settings.replication_set();
        let mut template = Conn::default();
        let bind = template.bind(server, &set);
        let timeout = end.config().timeout;
        ServerNet {
            end,
            remotes: BTreeMap::new(),
            template,
            bind,
            settings,
            set,
            lockstep,
            timeout,
            clock: Instant::now(),
            tick: 0,
            time: 0.0,
            lost: Vec::new(),
            dropped: Vec::new(),
        }
    }

    /// The endpoint.
    pub fn endpoint(&self) -> &Endpoint<T> {
        &self.end
    }

    /// What replicates.
    pub fn set(&self) -> &ReplicationSet {
        &self.set
    }

    /// How many network players have joined and are connected.
    pub fn players(&self) -> usize {
        self.remotes.values().filter(|r| r.player.is_some()).count()
    }

    fn now(&self) -> Duration {
        self.clock.elapsed()
    }

    /// What changes while traffic moves: each connection's unacked backlog
    /// and the parts of a message arriving from it (bounded, so a client
    /// cannot keep a lockstep wait alive by itself).
    fn progress(&self) -> Vec<(usize, usize)> {
        self.remotes
            .iter()
            .map(|(p, r)| (self.end.backlog(*p), r.parts.0.len()))
            .collect()
    }

    /// Waits, in real time, until `n` network players joined (a lockstep
    /// server before its first frame). `players` counts the players joined
    /// so far, in this process too, and names the next.
    ///
    /// # Errors
    ///
    /// `RE8906` when they do not all join within the timeout.
    pub fn wait_for_players(
        &mut self,
        n: usize,
        vm: &CompatVm,
        host: &mut dyn ScriptHost,
        players: &mut u32,
    ) -> Result<Vec<Diagnostic>, Diagnostic> {
        let start = Instant::now();
        let mut out = Vec::new();
        while self.players() < n {
            out.extend(self.poll(vm, host, players));
            if self.players() >= n {
                break;
            }
            if start.elapsed() > self.timeout {
                return Err(Diagnostic::error(
                    re_net::codes::TIMED_OUT,
                    format!(
                        "{} of {n} players joined within {:.0} s",
                        self.players(),
                        self.timeout.as_secs_f64()
                    ),
                ));
            }
            sleep();
        }
        Ok(out)
    }

    /// The start of a server frame, before its remotes are delivered: takes
    /// what arrived (joins, leaves, frames) and queues the frames' messages
    /// for the server VM. In lockstep, waits until every player's frame of
    /// this tick arrived.
    ///
    /// # Errors
    ///
    /// `RE8906` when a lockstep player's frame does not arrive in time.
    pub fn receive(
        &mut self,
        vm: &CompatVm,
        host: &mut dyn ScriptHost,
        players: &mut u32,
    ) -> Result<Vec<Diagnostic>, Diagnostic> {
        let mut out = self.poll(vm, host, players);
        if self.lockstep {
            // The timeout counts from the last progress: a large frame takes
            // a while to cross, and its acks show it moving.
            let mut start = Instant::now();
            let mut mark = self.progress();
            loop {
                let tick = self.tick;
                let missing = self
                    .remotes
                    .values()
                    .any(|r| r.player.is_some() && !r.frames.iter().any(|f| f.tick >= tick));
                if !missing {
                    break;
                }
                if start.elapsed() > self.timeout {
                    return Err(Diagnostic::error(
                        re_net::codes::TIMED_OUT,
                        format!("a player's frame {tick} did not arrive in lockstep"),
                    ));
                }
                sleep();
                out.extend(self.poll(vm, host, players));
                let now = self.progress();
                if now != mark {
                    mark = now;
                    start = Instant::now();
                }
            }
        }
        let tick = self.tick;
        let lockstep = self.lockstep;
        let rate = self.settings.remote_rate;
        let second = self.time as u64;
        let mut queued: Vec<(Option<String>, Msg)> = Vec::new();
        for r in self.remotes.values_mut() {
            let Some((id, _)) = r.player.clone() else {
                continue;
            };
            let mut take = Vec::new();
            while let Some(f) = r.frames.front() {
                if lockstep && f.tick > tick {
                    break;
                }
                take.push(r.frames.pop_front().expect("front"));
            }
            let mut bytes: Vec<Vec<u8>> = Vec::new();
            for f in &mut take {
                r.tick = Some(f.tick);
                bytes.extend(std::mem::take(&mut f.msgs));
            }
            bytes.append(&mut r.unreliable);
            for b in bytes {
                if r.rate.0 != second {
                    if r.rate.2 > 0 {
                        out.push(rate_warning(&id, rate, r.rate.2));
                    }
                    r.rate = (second, 0, 0);
                }
                if r.rate.1 >= rate {
                    r.rate.2 += 1;
                    continue;
                }
                r.rate.1 += 1;
                match checked(&id, &b) {
                    Ok(m) => queued.push((Some(id.clone()), m)),
                    Err(d) => out.push(d),
                }
            }
            r.ops.extend(take);
        }
        if !queued.is_empty() {
            let link = with_world_link(vm, host);
            if let Some(link) = link {
                let mut s = link.lock();
                for (_, m) in queued {
                    s.to_server.push_back(m);
                }
            }
        }
        Ok(out)
    }

    /// Takes the endpoint's events: joins, frames, leaves.
    fn poll(
        &mut self,
        vm: &CompatVm,
        host: &mut dyn ScriptHost,
        players: &mut u32,
    ) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for (id, p) in std::mem::take(&mut self.lost) {
            out.extend(remove_remote(vm, host, &id, p));
        }
        let now = self.now();
        let mut events: Vec<Event> = std::mem::take(&mut self.dropped)
            .into_iter()
            .map(|(peer, reason)| Event::Disconnected { peer, reason })
            .collect();
        events.extend(self.end.poll(now));
        let mut flushed = false;
        while !events.is_empty() || !flushed {
            if events.is_empty() {
                // What the joins queued (a welcome) and the acks go out now:
                // a lockstep server waits in `receive` for frames that need
                // them.
                flushed = true;
                events = self.end.flush(now);
                continue;
            }
            let e = events.remove(0);
            match e {
                Event::Connected(peer) => {
                    self.remotes
                        .insert(peer, Remote::new(self.template.clone()));
                }
                Event::Message {
                    peer,
                    channel: Channel::Unreliable,
                    bytes,
                } => {
                    if let Some(r) = self.remotes.get_mut(&peer)
                        && r.player.is_some()
                        && r.unreliable.len() < MAX_UNRELIABLE_WAITING
                    {
                        r.unreliable.push(bytes);
                    }
                }
                Event::Message { peer, bytes, .. } => match self
                    .remotes
                    .get_mut(&peer)
                    .map(|r| r.parts.take(&bytes, MAX_CLIENT_JOINED))
                {
                    Some(Ok(None)) => {}
                    Some(Ok(Some(NetMsg::Join { name }))) => {
                        out.extend(self.join(peer, name, vm, host, players));
                    }
                    Some(Ok(Some(NetMsg::Frame(f)))) => {
                        let tick = self.tick;
                        let lockstep = self.lockstep;
                        let mut ahead = false;
                        if let Some(r) = self.remotes.get_mut(&peer)
                            && r.player.is_some()
                            // In lockstep a frame from the past is dropped.
                            && !(lockstep && f.tick < tick)
                        {
                            ahead = lockstep && r.frames.len() >= MAX_AHEAD;
                            if !ahead {
                                r.frames.push_back(f);
                            }
                        }
                        if ahead {
                            // A lockstep player never runs this far ahead.
                            self.end
                                .disconnect(now, peer, "too many frames ahead of the server");
                            out.extend(self.leave(peer, vm, host));
                        }
                    }
                    None => {}
                    Some(_) => {
                        self.end
                            .disconnect(now, peer, "not a message a client sends");
                        out.extend(self.leave(peer, vm, host));
                    }
                },
                Event::Disconnected { peer, reason } => {
                    let name = self.name_of(peer, vm, host);
                    out.extend(self.leave(peer, vm, host));
                    if let Some(name) = name {
                        out.push(Diagnostic::note(
                            re_net::codes::CLOSED,
                            format!("{name} left: {}", reason.message),
                        ));
                    }
                }
            }
        }
        out
    }

    fn name_of(&self, peer: PeerId, vm: &CompatVm, host: &mut dyn ScriptHost) -> Option<String> {
        let (_, e) = self.remotes.get(&peer)?.player.clone()?;
        let mut name = None;
        vm.run(host, |lua| {
            name = with_world(lua, |w| w.name_of(e).to_string()).ok();
            Ok(())
        });
        name
    }

    fn join(
        &mut self,
        peer: PeerId,
        name: Option<String>,
        vm: &CompatVm,
        host: &mut dyn ScriptHost,
        players: &mut u32,
    ) -> Vec<Diagnostic> {
        let Some(r) = self.remotes.get(&peer) else {
            return Vec::new();
        };
        if r.player.is_some() {
            return Vec::new();
        }
        let n = *players + 1;
        let mut joined = None;
        let diags = vm.run(host, |lua| {
            let taken: Vec<String> = with_world(lua, |w| {
                let g = w.entity(crate::GAME_ID).ok();
                g.map(|g| players::players_in(w, g))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| w.name_of(p).to_string())
                    .collect()
            })?;
            let base = name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("Player{n}"));
            let mut unique = base.clone();
            let mut k = 2;
            while taken.contains(&unique) {
                unique = format!("{base}{k}");
                k += 1;
            }
            let spec = PlayerSpec {
                name: unique,
                user_id: i64::from(n),
                local: false,
            };
            let p = players::add_player_lua(lua, &spec, true)?;
            let id = with_world(lua, |w| w.id_of(p).map(str::to_string))?.unwrap_or_default();
            joined = Some((id, p));
            Ok(())
        });
        let Some((id, p)) = joined else {
            self.end
                .disconnect(self.now(), peer, "the server could not add the player");
            self.remotes.remove(&peer);
            return diags;
        };
        *players = n;
        let welcome = NetMsg::Welcome {
            player: id.clone(),
            bind: self.bind.clone(),
            tick: self.tick,
        };
        let r = self.remotes.get_mut(&peer).expect("checked above");
        r.player = Some((id, p));
        let mut diags = diags;
        if let Err(why) = send_msg(&mut self.end, peer, &welcome) {
            let why = format!("the server could not send the scene: {why}");
            self.end.disconnect(self.now(), peer, &why);
            diags.push(warn(re_net::codes::SEND, why));
            diags.extend(self.leave(peer, vm, host));
        }
        diags
    }

    /// Removes a connection's player, if it joined.
    fn leave(&mut self, peer: PeerId, vm: &CompatVm, host: &mut dyn ScriptHost) -> Vec<Diagnostic> {
        let Some(r) = self.remotes.remove(&peer) else {
            return Vec::new();
        };
        let Some((id, p)) = r.player else {
            return Vec::new();
        };
        remove_remote(vm, host, &id, p)
    }

    /// The replication step of a server frame, after the server's phases:
    /// applies what each player's frames changed, sends each its frame, and
    /// closes the connections of players that left or were kicked. `dt` is
    /// the simulation seconds this frame ran. Increments the server world's
    /// change tick once every connection had its pass.
    pub fn replicate(&mut self, server: &mut World, dt: f64) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        self.time += dt;
        let second = self.time as u64;
        let now = self.now();
        let max_speed = self.settings.max_owned_speed as f32;
        for r in self.remotes.values_mut() {
            let Some((id, p)) = r.player.clone() else {
                continue;
            };
            r.since += dt as f32;
            for f in std::mem::take(&mut r.ops) {
                let check = UpCheck {
                    max_speed,
                    dt: r.since.max(dt as f32),
                };
                r.since = 0.0;
                let report = r.conn.apply_up(server, &self.set, Some(p), &f, Some(check));
                if let Some(&e) = report.too_fast.first()
                    && r.speed_reported != Some(second)
                {
                    r.speed_reported = Some(second);
                    out.push(warn(
                        codes::OWNED_SPEED,
                        format!(
                            "{} ({id}) moved {} faster than {max_speed} studs/s; it stays where the server has it",
                            server.name_of(p),
                            server.name_of(e)
                        ),
                    ));
                }
            }
        }
        let link = server.ecs.get_resource::<NetLink>().cloned();
        let (mut kicks, mut queues) = match &link {
            Some(l) => {
                let mut s = l.lock();
                let kicks = std::mem::take(&mut s.kicks);
                let queues: BTreeMap<String, Vec<(bool, Vec<u8>)>> = s
                    .remote
                    .iter_mut()
                    .map(|(k, q)| (k.clone(), q.take_bytes()))
                    .collect();
                (kicks, queues)
            }
            None => (Vec::new(), BTreeMap::new()),
        };
        let mut close: Vec<(PeerId, String)> = Vec::new();
        let peers: Vec<PeerId> = self.remotes.keys().copied().collect();
        for peer in peers {
            let r = self.remotes.get_mut(&peer).expect("listed");
            let Some((id, p)) = r.player.clone() else {
                continue;
            };
            let Some(msgs) = queues.remove(&id) else {
                // Removed: kicked, or a script destroyed the Player.
                let why = kicks
                    .iter()
                    .position(|(k, _)| *k == id)
                    .map(|i| kicks.remove(i).1)
                    .unwrap_or_else(|| "you were removed from the game".to_string());
                close.push((peer, why));
                continue;
            };
            let frame = if self.end.congested(peer) {
                Frame {
                    tick: self.tick,
                    ..Frame::default()
                }
            } else {
                let mut f = r.conn.diff(server, &self.set, Some(p));
                f.tick = self.tick;
                f
            };
            if let Err(why) = send_frame(&mut self.end, peer, frame, msgs, &mut out) {
                out.push(warn(
                    re_net::codes::TIMED_OUT,
                    format!("{} was disconnected: {why}", server.name_of(p)),
                ));
                close.push((peer, format!("the connection fell too far behind: {why}")));
            }
        }
        server.ecs.increment_change_tick();
        for (peer, why) in close {
            self.end.disconnect(now, peer, &why);
            if let Some(r) = self.remotes.remove(&peer)
                && let Some(player) = r.player
            {
                // Still in the game when it fell behind: removed next frame.
                self.lost.push(player);
            }
        }
        for e in self.end.flush(now) {
            if let Event::Disconnected { peer, reason } = e {
                // Removed from the game at the next receive (which has the
                // VM), as a disconnect seen there is.
                self.dropped.push((peer, reason));
            }
        }
        self.tick += 1;
        out
    }

    /// Closes every connection, telling the players the server stopped.
    pub fn close(&mut self) {
        let now = self.now();
        self.end.disconnect_all(now, "the server stopped");
        self.end.flush(now);
    }
}

/// Removes a network player whose connection closed, unless a kick or a
/// script already did: `PlayerRemoving` fires, its character goes, and what
/// it owned goes back to the server.
fn remove_remote(vm: &CompatVm, host: &mut dyn ScriptHost, id: &str, p: Entity) -> Vec<Diagnostic> {
    vm.run(host, |lua| {
        let link = super::link(lua)?;
        if let Some(link) = link
            && link.lock().remote.remove(id).is_none()
        {
            return Ok(());
        }
        players::remove_player_lua(lua, p)?;
        with_world(lua, |w| super::replicate::disown(w, p))?;
        Ok(())
    })
}

fn rate_warning(player: &str, rate: u32, dropped: u32) -> Diagnostic {
    warn(
        codes::REMOTE_RATE,
        format!(
            "player {player} sent more than {rate} remote messages in a second; {dropped} were dropped"
        ),
    )
}

/// A client's message, checked: its size, that it decodes, and its player,
/// which becomes `player` whatever the client wrote.
fn checked(player: &str, bytes: &[u8]) -> Result<Msg, Diagnostic> {
    if bytes.len() > MAX_REMOTE_MESSAGE {
        return Err(warn(
            codes::REMOTE_DROPPED,
            format!(
                "a remote message from player {player} was dropped: {} bytes, more than {MAX_REMOTE_MESSAGE}",
                bytes.len()
            ),
        ));
    }
    let Some(mut m) = wire::decode_msg(bytes) else {
        return Err(warn(
            codes::REMOTE_DROPPED,
            format!("a remote message from player {player} was dropped: it does not decode"),
        ));
    };
    let me = player.to_string();
    match &mut m {
        Msg::Event { player, .. } | Msg::Invoke { player, .. } | Msg::Reply { player, .. } => {
            *player = Some(me)
        }
        Msg::Prompt { player, .. } | Msg::Kick { player } => *player = me,
        Msg::Chat { .. } => {
            return Err(warn(
                codes::REMOTE_DROPPED,
                format!(
                    "a chat message from player {player} was dropped: clients do not send chat"
                ),
            ));
        }
    }
    Ok(m)
}

fn with_world_link(vm: &CompatVm, host: &mut dyn ScriptHost) -> Option<NetLink> {
    let mut link = None;
    vm.run(host, |lua| {
        link = super::link(lua)?;
        Ok(())
    });
    link
}

fn sleep() {
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::sleep(WAIT_STEP);
}

// ------------------------------------------------------------------ client

/// Where a client's session is.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientState {
    /// Connecting or joining.
    Joining,
    /// In the game.
    Joined,
    /// Closed: kicked, timed out, refused or the server stopped.
    Closed(Diagnostic),
}

/// A client's session with its server.
pub struct ClientNet<T: Transport> {
    end: Endpoint<T>,
    name: Option<String>,
    state: ClientState,
    sent_join: bool,
    welcome: Option<(String, Frame, u64)>,
    /// Server frames that arrived and were not applied.
    frames: VecDeque<Frame>,
    unreliable: Vec<Vec<u8>>,
    lockstep: bool,
    timeout: Duration,
    clock: Instant,
    /// The next client frame's number.
    tick: u64,
    /// The newest server frame applied.
    applied: Option<u64>,
    /// A message arriving in parts.
    parts: Parts,
}

impl<T: Transport> ClientNet<T> {
    /// A session that connects through `end` (a client endpoint) and joins
    /// as `name` (the server names it `Player<n>` without one).
    pub fn new(end: Endpoint<T>, name: Option<String>, lockstep: bool) -> ClientNet<T> {
        let timeout = end.config().timeout;
        ClientNet {
            end,
            name,
            state: ClientState::Joining,
            sent_join: false,
            welcome: None,
            frames: VecDeque::new(),
            unreliable: Vec::new(),
            lockstep,
            timeout,
            clock: Instant::now(),
            tick: 0,
            applied: None,
            parts: Parts::default(),
        }
    }

    /// The endpoint.
    pub fn endpoint(&self) -> &Endpoint<T> {
        &self.end
    }

    /// Where the session is.
    pub fn state(&self) -> &ClientState {
        &self.state
    }

    /// What changes while traffic moves (see [`ServerNet`]'s).
    fn progress(&self) -> (usize, usize, usize) {
        (
            self.end.backlog(PeerId(0)),
            self.parts.0.len(),
            self.frames.len(),
        )
    }

    fn now(&self) -> Duration {
        self.clock.elapsed()
    }

    fn poll(&mut self) {
        let now = self.now();
        let events = self.end.poll(now);
        for e in events {
            match e {
                Event::Connected(_) => {
                    if !self.sent_join {
                        self.sent_join = true;
                        let join = NetMsg::Join {
                            name: self.name.clone(),
                        };
                        let _ = self.end.send(PeerId(0), Channel::Reliable, &join.encode());
                    }
                }
                Event::Message {
                    channel: Channel::Unreliable,
                    bytes,
                    ..
                } => self.unreliable.push(bytes),
                Event::Message { bytes, .. } => match self.parts.take(&bytes, MAX_SERVER_JOINED) {
                    Ok(None) => {}
                    Ok(Some(NetMsg::Welcome { player, bind, tick })) => {
                        self.welcome = Some((player, bind, tick));
                    }
                    Ok(Some(NetMsg::Frame(f))) => self.frames.push_back(f),
                    Ok(Some(_)) | Err(()) => {
                        self.end
                            .disconnect(now, PeerId(0), "not a message a server sends");
                        self.state = ClientState::Closed(Diagnostic::error(
                            re_net::codes::CLOSED,
                            "the server sent a message this client does not read",
                        ));
                    }
                },
                Event::Disconnected { reason, .. } => self.closed(reason),
            }
        }
        for e in self.end.flush(now) {
            if let Event::Disconnected { reason, .. } = e {
                self.closed(reason);
            }
        }
    }

    /// The session ended for `reason`, unless it already had.
    fn closed(&mut self, reason: Diagnostic) {
        if !matches!(self.state, ClientState::Closed(_)) {
            self.state = ClientState::Closed(reason);
        }
    }

    /// Connects and joins, waiting in real time for the server's welcome,
    /// then binds `client` (the client world made from this client's copy
    /// of the scene, [`super::client_world`], with [`super::link_client`])
    /// and makes its player the local one. Scripts start after this.
    ///
    /// # Errors
    ///
    /// The connection failed (`RE8902` to `RE8907`) or no welcome came
    /// within the timeout (`RE8906`).
    pub fn join(&mut self, client: &mut World) -> Result<(), Diagnostic> {
        let mut start = Instant::now();
        let mut mark = self.progress();
        while !self.poll_join(client)? {
            let now = self.progress();
            if now != mark {
                mark = now;
                start = Instant::now();
            }
            if start.elapsed() > self.timeout {
                return Err(Diagnostic::error(
                    re_net::codes::TIMED_OUT,
                    format!(
                        "the server did not welcome this player within {:.0} s",
                        self.timeout.as_secs_f64()
                    ),
                ));
            }
            sleep();
        }
        Ok(())
    }

    /// [`ClientNet::join`] without waiting: true once joined.
    ///
    /// # Errors
    ///
    /// The connection failed (`RE8902` to `RE8907`).
    pub fn poll_join(&mut self, client: &mut World) -> Result<bool, Diagnostic> {
        if self.state == ClientState::Joined {
            return Ok(true);
        }
        self.poll();
        if let ClientState::Closed(d) = &self.state {
            return Err(d.clone());
        }
        let Some((player, bind, tick)) = self.welcome.take() else {
            return Ok(false);
        };
        // The server's frames from here on, and this client's, are
        // numbered from the frame it joined at.
        self.applied = tick.checked_sub(1);
        self.tick = tick;
        let mut rep = client.ecs.remove_resource::<Replica>().unwrap_or_default();
        rep.apply(client, &bind);
        client.ecs.insert_resource(rep);
        if let Some(link) = client.ecs.get_resource::<NetLink>() {
            link.lock().local_player = Some(player);
        }
        self.state = ClientState::Joined;
        Ok(true)
    }

    /// The start of a client frame: applies the server frames that arrived
    /// to `client` and queues their messages for the client VM. In lockstep,
    /// waits for the server's frame of this tick.
    ///
    /// # Errors
    ///
    /// `RE8906` when a lockstep server frame does not arrive in time.
    pub fn receive(&mut self, client: &mut World) -> Result<(), Diagnostic> {
        self.poll();
        if self.lockstep && self.state == ClientState::Joined {
            let want = self.applied.map_or(0, |t| t + 1);
            let mut start = Instant::now();
            let mut mark = self.progress();
            while !self.frames.iter().any(|f| f.tick >= want) {
                if let ClientState::Closed(d) = &self.state {
                    return Err(d.clone());
                }
                if start.elapsed() > self.timeout {
                    return Err(Diagnostic::error(
                        re_net::codes::TIMED_OUT,
                        format!("the server's frame {want} did not arrive in lockstep"),
                    ));
                }
                sleep();
                self.poll();
                let now = self.progress();
                if now != mark {
                    mark = now;
                    start = Instant::now();
                }
            }
        }
        let mut rep = client.ecs.remove_resource::<Replica>().unwrap_or_default();
        let link = client.ecs.get_resource::<NetLink>().cloned();
        let lockstep_want = self.applied.map_or(0, |t| t + 1);
        while let Some(f) = self.frames.front() {
            if self.lockstep && f.tick > lockstep_want {
                break;
            }
            let mut f = self.frames.pop_front().expect("front");
            rep.apply(client, &f);
            self.applied = Some(f.tick);
            if let Some(l) = &link {
                let mut s = l.lock();
                for b in f.msgs.drain(..) {
                    if let Some(m) = wire::decode_msg(&b) {
                        s.to_client.push_back(m);
                    }
                }
            }
            if self.lockstep {
                break;
            }
        }
        if let Some(l) = &link {
            let mut s = l.lock();
            for b in self.unreliable.drain(..) {
                if let Some(m) = wire::decode_msg(&b) {
                    s.to_client.push_back(m);
                }
            }
        }
        client.ecs.insert_resource(rep);
        Ok(())
    }

    /// The end of a client frame: sends the client's owned changes and its
    /// messages to the server. Returns the warnings (`RE9451`, `RE9454`).
    pub fn send(&mut self, client: &mut World, set: &ReplicationSet) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if self.state != ClientState::Joined {
            return out;
        }
        let mut rep = client.ecs.remove_resource::<Replica>().unwrap_or_default();
        let mut frame = rep.diff_up(client, set);
        out.extend(rep.take_warnings());
        client.ecs.insert_resource(rep);
        frame.tick = self.tick;
        self.tick += 1;
        let msgs = client
            .ecs
            .get_resource::<NetLink>()
            .map(|l| l.lock().to_server.take_bytes())
            .unwrap_or_default();
        let now = self.now();
        if let Err(why) = send_frame(&mut self.end, PeerId(0), frame, msgs, &mut out) {
            // The server may have part of it; the session cannot go on.
            self.end
                .disconnect(now, PeerId(0), "the client could not send its frame");
            self.closed(Diagnostic::error(
                re_net::codes::SEND,
                format!("this client could not send its frame to the server: {why}"),
            ));
        }
        for e in self.end.flush(now) {
            if let Event::Disconnected { reason, .. } = e {
                self.closed(reason);
            }
        }
        out
    }

    /// Leaves the game.
    pub fn close(&mut self) {
        let now = self.now();
        self.end.disconnect_all(now, "the player left");
        self.end.flush(now);
        if !matches!(self.state, ClientState::Closed(_)) {
            self.state = ClientState::Closed(Diagnostic::note(
                re_net::codes::CLOSED,
                "this player left the game",
            ));
        }
    }
}

#[cfg(test)]
mod part_tests {
    use super::*;
    use re_net::{Config, MemoryNet};

    #[test]
    fn a_message_larger_than_the_transport_takes_crosses_in_parts() {
        let net = MemoryNet::new();
        let mut config = Config::server(1, 7);
        config.send_rate = 64.0 * 1024.0 * 1024.0;
        let mut server = Endpoint::server(net.bind(), config);
        let mut client = Endpoint::client(net.bind(), server.local_addr(), Config::client(1, 7));
        let clock = Instant::now();
        let mut peer = None;
        while peer.is_none() {
            let now = clock.elapsed();
            client.poll(now);
            client.flush(now);
            for e in server.poll(now) {
                if let Event::Connected(p) = e {
                    peer = Some(p);
                }
            }
            server.flush(now);
        }
        let peer = peer.expect("connected");
        let frame = Frame {
            tick: 3,
            msgs: vec![vec![7; 900 * 1024], vec![9; 900 * 1024]],
            ..Frame::default()
        };
        send_msg(&mut server, peer, &NetMsg::Frame(frame)).unwrap();
        let mut parts = Parts::default();
        let mut got = None;
        let mut pieces = 0;
        for _ in 0..100_000 {
            let now = clock.elapsed();
            server.poll(now);
            server.flush(now);
            for e in client.poll(now) {
                if let Event::Message { bytes, .. } = e {
                    pieces += 1;
                    if let Ok(Some(m)) = parts.take(&bytes, MAX_SERVER_JOINED) {
                        got = Some(m);
                    }
                }
            }
            client.flush(now);
            if got.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        let Some(NetMsg::Frame(f)) = got else {
            panic!("the frame did not arrive");
        };
        assert_eq!(pieces, 2);
        assert_eq!(f.tick, 3);
        assert_eq!(f.msgs[1].len(), 900 * 1024);
        // A joined message past the limit is refused.
        let mut small = Parts::default();
        let part = NetMsg::Part {
            last: false,
            bytes: vec![0; 2048],
        };
        assert!(small.take(&part.encode(), 1024).is_err());
    }
}
