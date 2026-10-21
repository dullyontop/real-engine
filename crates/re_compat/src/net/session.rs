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
//!   longer than the timeout fails the run.

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
    /// Server to client: its `Player`'s stable id, and the frame that binds
    /// the client's copy of the scene.
    Welcome { player: String, bind: Frame },
    /// Either way, once a frame.
    Frame(Frame),
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
) -> bool {
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
    end.send(peer, Channel::Reliable, &NetMsg::Frame(frame).encode())
        .is_err()
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
            let start = Instant::now();
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
        for e in self.end.poll(now) {
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
                    {
                        r.unreliable.push(bytes);
                    }
                }
                Event::Message { peer, bytes, .. } => match NetMsg::decode(&bytes) {
                    Some(NetMsg::Join { name }) => {
                        out.extend(self.join(peer, name, vm, host, players));
                    }
                    Some(NetMsg::Frame(f)) => {
                        if let Some(r) = self.remotes.get_mut(&peer)
                            && r.player.is_some()
                        {
                            r.frames.push_back(f);
                        }
                    }
                    Some(NetMsg::Welcome { .. }) | None => {
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
        };
        let r = self.remotes.get_mut(&peer).expect("checked above");
        r.player = Some((id, p));
        let _ = self.end.send(peer, Channel::Reliable, &welcome.encode());
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
            if send_frame(&mut self.end, peer, frame, msgs, &mut out) {
                close.push((
                    peer,
                    "the connection fell too far behind (4 MiB unacknowledged)".to_string(),
                ));
                out.push(warn(
                    re_net::codes::TIMED_OUT,
                    format!(
                        "{} was disconnected: 4 MiB of its messages went unacknowledged",
                        server.name_of(p)
                    ),
                ));
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
        out.extend(self.end.flush(now).into_iter().filter_map(|e| match e {
            Event::Disconnected { reason, .. } => Some(reason),
            _ => None,
        }));
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
    welcome: Option<(String, Frame)>,
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
                Event::Message { bytes, .. } => match NetMsg::decode(&bytes) {
                    Some(NetMsg::Welcome { player, bind }) => {
                        self.welcome = Some((player, bind));
                    }
                    Some(NetMsg::Frame(f)) => self.frames.push_back(f),
                    Some(NetMsg::Join { .. }) | None => {
                        self.end
                            .disconnect(now, PeerId(0), "not a message a server sends");
                        self.state = ClientState::Closed(Diagnostic::error(
                            re_net::codes::CLOSED,
                            "the server sent a message this client does not read",
                        ));
                    }
                },
                Event::Disconnected { reason, .. } => {
                    if !matches!(self.state, ClientState::Closed(_)) {
                        self.state = ClientState::Closed(reason);
                    }
                }
            }
        }
        self.end.flush(now);
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
        let start = Instant::now();
        while !self.poll_join(client)? {
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
        let Some((player, bind)) = self.welcome.take() else {
            return Ok(false);
        };
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
            let start = Instant::now();
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
        send_frame(&mut self.end, PeerId(0), frame, msgs, &mut out);
        let now = self.now();
        for e in self.end.flush(now) {
            if let Event::Disconnected { reason, .. } = e {
                self.state = ClientState::Closed(reason);
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
