//! The native WebSocket transports, over tungstenite and `std::net`: a
//! server's listener beside its UDP socket, and a client's WebSocket.
//!
//! Every socket is non-blocking and every step runs in
//! [`Transport::advance`], so the frame loop drives them as it drives UDP.
//! What a peer may cost is bounded: 64 handshakes at a time (5 s each), a
//! number of links the caller sets, messages of [`MAX_WS_MESSAGE`] bytes, 1 MiB
//! waiting to be written per link, and 4,096 datagrams waiting to be read.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use re_core::Diagnostic;
use tungstenite::error::ProtocolError;
use tungstenite::handshake::client::ClientHandshake;
use tungstenite::handshake::server::{NoCallback, ServerHandshake};
use tungstenite::handshake::{HandshakeError, MidHandshake};
use tungstenite::protocol::frame::Utf8Bytes;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tungstenite::{Message, WebSocket};

use super::{MAX_INBOX, MAX_QUEUED, MAX_WRITE_BUFFER, MAX_WS_MESSAGE, is_websocket_peer};
use crate::codes;
use crate::transport::{Transport, UdpTransport};

/// The most WebSocket handshakes a server runs at once; a connection past it
/// is closed at once.
const MAX_PENDING: usize = 64;

/// The most connections a server accepts in one step.
const MAX_ACCEPTS: usize = 64;

/// How long a WebSocket handshake may take before the server drops it.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// A link that receives nothing for this long closes, or for the endpoint's
/// timeout when that is longer ([`ServerTransport::set_link_timeout`]). A
/// connected peer sends every 100 ms, so only links that never joined, or were
/// refused, go quiet.
const LINK_IDLE: Duration = Duration::from_secs(15);

/// How long a closing link may take to finish its close handshake.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// The most datagrams one link delivers in a step, so none starves the rest.
const MAX_PER_LINK: usize = 256;

/// The most forged datagrams (from `100::/64`) a server skips in a step: as
/// many as its endpoint reads in one, so crowding out the players takes as
/// large a flood of them as of any other datagram.
const MAX_FORGED: usize = 4096;

/// Each link's limits: a 4 KiB read buffer, eager writes, 1 MiB waiting to be
/// written, and messages of at most [`MAX_WS_MESSAGE`] bytes.
fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_WRITE_BUFFER)
        .max_message_size(Some(MAX_WS_MESSAGE))
        .max_frame_size(Some(MAX_WS_MESSAGE))
}

/// How a link ended.
enum Ending {
    /// The peer closed it, with this close frame's code and reason.
    Closed(Option<CloseFrame>),
    /// The peer broke the protocol: the link is closed with this code and
    /// reason, and nothing more is read from it.
    Refused(CloseCode, &'static str, String),
    /// The connection broke.
    Broke(String),
    /// It sent nothing for the listener's idle time.
    Idle,
}

impl Ending {
    /// What a close frame said, for a message: ` (code 1001: going away)`.
    fn detail(frame: Option<&CloseFrame>) -> String {
        match frame {
            Some(f) if f.reason.is_empty() => format!(" (code {})", f.code),
            Some(f) => format!(" (code {}: {})", f.code, f.reason),
            None => String::new(),
        }
    }
}

/// What one read of a link gave.
enum Read {
    Datagram(Vec<u8>),
    /// A ping or pong: the link is alive.
    Control,
    /// Nothing waiting.
    Empty,
    Over(Ending),
}

fn read_link(ws: &mut WebSocket<TcpStream>) -> Read {
    use tungstenite::Error;
    match ws.read() {
        Ok(Message::Binary(b)) => Read::Datagram(b.to_vec()),
        Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => Read::Control,
        Ok(Message::Text(_)) => Read::Over(Ending::Refused(
            CloseCode::Unsupported,
            "only binary messages carry datagrams",
            "it sent a text message, and only binary messages carry datagrams".into(),
        )),
        Ok(Message::Close(frame)) => Read::Over(Ending::Closed(frame)),
        Err(Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => Read::Empty,
        Err(Error::Io(e)) if e.kind() == ErrorKind::Interrupted => Read::Control,
        Err(Error::ConnectionClosed | Error::AlreadyClosed) => Read::Over(Ending::Closed(None)),
        Err(e @ Error::Protocol(ProtocolError::ResetWithoutClosingHandshake)) => {
            Read::Over(Ending::Broke(e.to_string()))
        }
        // The codec is left mid-frame: the link must not be read again.
        Err(e @ Error::Capacity(_)) => Read::Over(Ending::Refused(
            CloseCode::Size,
            "message too big",
            e.to_string(),
        )),
        Err(e @ (Error::Protocol(_) | Error::Utf8(_))) => Read::Over(Ending::Refused(
            CloseCode::Protocol,
            "protocol error",
            e.to_string(),
        )),
        Err(e) => Read::Over(Ending::Broke(e.to_string())),
    }
}

/// Writes one datagram, at once ([`config`]'s eager writes). A full write
/// buffer loses it; another failure ends the link.
fn write_link(ws: &mut WebSocket<TcpStream>, datagram: &[u8]) -> Result<(), io::Error> {
    match ws.write(Message::binary(datagram.to_vec())) {
        Ok(()) => Ok(()),
        // Queued: a later write or flush sends it.
        Err(tungstenite::Error::Io(e))
            if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
        {
            Ok(())
        }
        Err(tungstenite::Error::WriteBufferFull(_)) => Err(io::Error::new(
            ErrorKind::WouldBlock,
            "the link has 1 MiB waiting to be written",
        )),
        Err(e) => Err(io::Error::other(e.to_string())),
    }
}

/// Sends what a link has waiting; `Err` when the link broke.
fn flush_link(ws: &mut WebSocket<TcpStream>) -> Result<(), String> {
    match ws.flush() {
        Ok(()) => Ok(()),
        Err(tungstenite::Error::Io(e))
            if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
        {
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Starts a close: a close frame (1000, normal) and whatever was waiting.
fn start_close(ws: &mut WebSocket<TcpStream>) {
    close_with(ws, CloseCode::Normal, "");
}

/// Starts a close with `code` and `reason` (at most 123 bytes).
fn close_with(ws: &mut WebSocket<TcpStream>, code: CloseCode, reason: &'static str) {
    let _ = ws.close(Some(CloseFrame {
        code,
        reason: Utf8Bytes::from_static(reason),
    }));
}

/// One step of a close: sends what is waiting and, when `read`, reads until
/// the peer's close. True while it is not over.
fn step_close(ws: &mut WebSocket<TcpStream>, read: bool) -> bool {
    let blocked = |e: &tungstenite::Error| matches!(e, tungstenite::Error::Io(e) if e.kind() == ErrorKind::WouldBlock);
    if !read {
        // Only the close frame goes out; then the connection is dropped.
        return ws.flush().is_err_and(|e| blocked(&e));
    }
    for _ in 0..MAX_PER_LINK {
        match ws.read() {
            Ok(_) => {}
            Err(e) => return blocked(&e),
        }
    }
    true
}

// ------------------------------------------------------------------ server

/// A server's transport: a UDP socket and, when asked for, a WebSocket
/// listener on TCP at the same address and port for players in a browser.
///
/// Receives alternate between the two, so neither starves the other. A UDP
/// datagram that claims to come from `100::/64` is dropped: that prefix is
/// the listener's.
pub struct ServerTransport {
    udp: UdpTransport,
    ws: Option<Listener>,
    ws_first: bool,
    /// The forged datagrams skipped in this step.
    forged: usize,
}

impl fmt::Debug for ServerTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerTransport")
            .field("udp", &self.udp.local_addr())
            .field("websocket", &self.websocket_addr())
            .field("links", &self.websocket_links())
            .finish()
    }
}

impl ServerTransport {
    /// UDP only, on `addr` ([`UdpTransport::bind`]).
    ///
    /// # Errors
    ///
    /// `RE8900` when the socket cannot be bound.
    pub fn udp(addr: SocketAddr) -> Result<ServerTransport, Diagnostic> {
        Ok(ServerTransport {
            udp: UdpTransport::bind(addr)?,
            ws: None,
            ws_first: false,
            forged: 0,
        })
    }

    /// UDP on `addr` and a WebSocket listener on TCP at the same address and
    /// port, for at most `max_links` WebSocket peers at a time (playing or
    /// joining). Port 0 picks a port free for both.
    ///
    /// # Errors
    ///
    /// `RE8900` when either socket cannot be bound.
    pub fn with_websocket(
        addr: SocketAddr,
        max_links: usize,
    ) -> Result<ServerTransport, Diagnostic> {
        let pick = addr.port() == 0;
        let tries = if pick { 16 } else { 1 };
        let mut failed = None;
        for i in 0..tries {
            // With port 0, every other try lets TCP pick the port and binds
            // UDP to it: Windows keeps blocks of TCP ports from use (a bind
            // there fails with WSAEACCES), and the UDP ports it hands out
            // one after another can all fall in one block.
            let bound = if pick && i % 2 == 1 {
                match Listener::bind(addr, max_links) {
                    Ok(listener) => {
                        let at = SocketAddr::new(addr.ip(), listener.local.port());
                        UdpTransport::bind(at).ok().map(|udp| (udp, listener))
                    }
                    Err(e) => {
                        failed = Some((addr, e));
                        None
                    }
                }
            } else {
                let udp = UdpTransport::bind(addr)?;
                let at = SocketAddr::new(addr.ip(), udp.local_addr().port());
                match Listener::bind(at, max_links) {
                    Ok(listener) => Some((udp, listener)),
                    Err(e) => {
                        failed = Some((at, e));
                        None
                    }
                }
            };
            if let Some((udp, listener)) = bound {
                return Ok(ServerTransport {
                    udp,
                    ws: Some(listener),
                    ws_first: false,
                    forged: 0,
                });
            }
        }
        let (at, e) = failed.expect("bound at least once");
        Err(Diagnostic::error(
            codes::BIND,
            format!("cannot open a WebSocket listener on TCP {at}: {e}"),
        )
        .with_hint(
            "another program may be using the TCP port; pick another with --port, or serve UDP only with `[net] websocket = false`",
        ))
    }

    /// The address WebSocket peers connect to, when the listener is on.
    pub fn websocket_addr(&self) -> Option<SocketAddr> {
        self.ws.as_ref().map(|l| l.local)
    }

    /// The WebSocket links open now, playing or joining.
    pub fn websocket_links(&self) -> usize {
        self.ws.as_ref().map_or(0, |l| l.links.len())
    }

    /// Lets a WebSocket link stay quiet for `timeout`, when that is longer
    /// than 15 s, before it closes. Given the endpoint's timeout, a quiet
    /// player (a hidden browser tab) stays as long as one on UDP.
    pub fn set_link_timeout(&mut self, timeout: Duration) {
        if let Some(l) = &mut self.ws {
            l.idle = timeout.max(LINK_IDLE);
        }
    }

    fn recv_udp(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        while self.forged < MAX_FORGED {
            match self.udp.recv_from(buf)? {
                Some((_, from)) if is_websocket_peer(from) => self.forged += 1,
                got => return Ok(got),
            }
        }
        Ok(None)
    }
}

impl Transport for ServerTransport {
    fn send_to(&mut self, to: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        if !is_websocket_peer(to) {
            return self.udp.send_to(to, datagram);
        }
        match &mut self.ws {
            Some(l) => l.send(to, datagram),
            None => Err(ErrorKind::NotConnected.into()),
        }
    }

    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        let mut udp_failed = None;
        for _ in 0..2 {
            self.ws_first = !self.ws_first;
            if self.ws_first {
                if let Some(got) = self.ws.as_mut().and_then(|l| l.recv(buf)) {
                    return Ok(Some(got));
                }
            } else {
                match self.recv_udp(buf) {
                    Ok(Some(got)) => return Ok(Some(got)),
                    Ok(None) => {}
                    Err(e) => udp_failed = Some(e),
                }
            }
        }
        udp_failed.map_or(Ok(None), Err)
    }

    fn local_addr(&self) -> SocketAddr {
        self.udp.local_addr()
    }

    fn advance(&mut self, now: Duration) {
        self.forged = 0;
        if let Some(l) = &mut self.ws {
            l.advance(now);
        }
    }

    fn take_lost(&mut self) -> Vec<(SocketAddr, Diagnostic)> {
        self.ws
            .as_mut()
            .map(Listener::take_lost)
            .unwrap_or_default()
    }

    fn release(&mut self, peer: SocketAddr) {
        if let Some(l) = &mut self.ws {
            l.release(peer);
        }
    }

    fn describe_peer(&self, peer: SocketAddr) -> String {
        match self.ws.as_ref().and_then(|l| l.tcp_peer(peer)) {
            Some(tcp) => format!("{tcp} (WebSocket)"),
            None => peer.to_string(),
        }
    }
}

/// An open link.
struct Link {
    ws: WebSocket<TcpStream>,
    /// Its TCP peer.
    peer: SocketAddr,
    /// When it last sent anything.
    heard: Duration,
}

/// A connection whose WebSocket handshake is under way.
struct Pending {
    handshake: MidHandshake<ServerHandshake<TcpStream, NoCallback>>,
    peer: SocketAddr,
    since: Duration,
}

/// A link whose close is finishing; what it sends is dropped.
struct Closing {
    addr: SocketAddr,
    peer: SocketAddr,
    ws: WebSocket<TcpStream>,
    since: Duration,
    /// Whether it is read until the peer's close (not after the peer broke
    /// the protocol).
    read: bool,
}

/// A link that ended, until the endpoint is told.
struct Lost {
    addr: SocketAddr,
    peer: SocketAddr,
    reason: Diagnostic,
}

/// The WebSocket half of a [`ServerTransport`].
struct Listener {
    listener: TcpListener,
    local: SocketAddr,
    max_links: usize,
    /// How long a link may receive nothing before it closes.
    idle: Duration,
    now: Duration,
    /// The last link's number (its address, [`super::virtual_addr`]).
    next: u64,
    pending: Vec<Pending>,
    links: HashMap<SocketAddr, Link>,
    /// The links in the order they opened; reads start one further each step.
    order: Vec<SocketAddr>,
    cursor: usize,
    closing: Vec<Closing>,
    inbox: VecDeque<(SocketAddr, Vec<u8>)>,
    lost: Vec<Lost>,
}

impl Listener {
    fn bind(addr: SocketAddr, max_links: usize) -> io::Result<Listener> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let local = listener.local_addr()?;
        Ok(Listener {
            listener,
            local,
            max_links: max_links.max(1),
            idle: LINK_IDLE,
            now: Duration::ZERO,
            next: 1,
            pending: Vec::new(),
            links: HashMap::new(),
            order: Vec::new(),
            cursor: 0,
            closing: Vec::new(),
            inbox: VecDeque::new(),
            lost: Vec::new(),
        })
    }

    fn advance(&mut self, now: Duration) {
        self.now = now;
        self.accept();
        self.handshake();
        self.read();
        self.flush();
        self.expire();
        self.finish_closing();
    }

    fn accept(&mut self) {
        for _ in 0..MAX_ACCEPTS {
            let (stream, peer) = match self.listener.accept() {
                Ok(got) => got,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::Interrupted
                            | ErrorKind::ConnectionAborted
                            | ErrorKind::ConnectionReset
                    ) =>
                {
                    continue;
                }
                // Out of file descriptors and the like: the next step tries again.
                Err(_) => return,
            };
            // Dropping the stream closes it.
            if self.pending.len() >= MAX_PENDING
                || self.links.len() + self.pending.len() >= self.max_links
                || stream.set_nonblocking(true).is_err()
            {
                continue;
            }
            let _ = stream.set_nodelay(true);
            match tungstenite::accept_with_config(stream, Some(config())) {
                Ok(ws) => self.open(ws, peer),
                Err(HandshakeError::Interrupted(handshake)) => self.pending.push(Pending {
                    handshake,
                    peer,
                    since: self.now,
                }),
                Err(HandshakeError::Failure(_)) => {}
            }
        }
    }

    fn handshake(&mut self) {
        for p in std::mem::take(&mut self.pending) {
            if self.now.saturating_sub(p.since) > HANDSHAKE_TIMEOUT {
                continue;
            }
            match p.handshake.handshake() {
                Ok(ws) => self.open(ws, p.peer),
                Err(HandshakeError::Interrupted(handshake)) => {
                    self.pending.push(Pending { handshake, ..p });
                }
                Err(HandshakeError::Failure(_)) => {}
            }
        }
    }

    fn open(&mut self, ws: WebSocket<TcpStream>, peer: SocketAddr) {
        self.next += 1;
        let addr = super::virtual_addr(self.next);
        self.links.insert(
            addr,
            Link {
                ws,
                peer,
                heard: self.now,
            },
        );
        self.order.push(addr);
    }

    fn read(&mut self) {
        let n = self.order.len();
        if n == 0 {
            return;
        }
        self.cursor = (self.cursor + 1) % n;
        let mut over = Vec::new();
        for k in 0..n {
            if self.inbox.len() >= MAX_INBOX {
                break;
            }
            let addr = self.order[(self.cursor + k) % n];
            let Some(link) = self.links.get_mut(&addr) else {
                continue;
            };
            for _ in 0..MAX_PER_LINK {
                if self.inbox.len() >= MAX_INBOX {
                    break;
                }
                match read_link(&mut link.ws) {
                    Read::Datagram(d) => {
                        link.heard = self.now;
                        self.inbox.push_back((addr, d));
                    }
                    Read::Control => link.heard = self.now,
                    Read::Empty => break,
                    Read::Over(why) => {
                        over.push((addr, why));
                        break;
                    }
                }
            }
        }
        for (addr, why) in over {
            self.lose(addr, why);
        }
    }

    fn flush(&mut self) {
        let broken: Vec<(SocketAddr, String)> = self
            .links
            .iter_mut()
            .filter_map(|(a, l)| flush_link(&mut l.ws).err().map(|e| (*a, e)))
            .collect();
        for (addr, e) in broken {
            self.lose(addr, Ending::Broke(e));
        }
    }

    fn expire(&mut self) {
        let (now, limit) = (self.now, self.idle);
        let idle: Vec<SocketAddr> = self
            .links
            .iter()
            .filter(|(_, l)| now.saturating_sub(l.heard) > limit)
            .map(|(a, _)| *a)
            .collect();
        for addr in idle {
            self.lose(addr, Ending::Idle);
        }
    }

    fn finish_closing(&mut self) {
        let now = self.now;
        self.closing.retain_mut(|c| {
            now.saturating_sub(c.since) <= CLOSE_TIMEOUT && step_close(&mut c.ws, c.read)
        });
    }

    /// Ends a link the peer closed or broke, and reports it lost once the
    /// endpoint read what it sent first (a Disconnect says why it left).
    fn lose(&mut self, addr: SocketAddr, why: Ending) {
        let Some(mut link) = self.links.remove(&addr) else {
            return;
        };
        self.order.retain(|a| *a != addr);
        let at = format!("{} (WebSocket)", link.peer);
        // The close goes on reading the link, or only sends its close frame,
        // or there is nothing left to close.
        let (reason, read) = match why {
            Ending::Closed(frame) => (
                Diagnostic::error(
                    codes::CLOSED,
                    format!(
                        "the client at {at} closed the connection{}",
                        Ending::detail(frame.as_ref())
                    ),
                ),
                Some(true),
            ),
            Ending::Refused(code, short, e) => {
                close_with(&mut link.ws, code, short);
                (
                    Diagnostic::error(
                        codes::WEBSOCKET,
                        format!("the client at {at} broke the WebSocket protocol: {e}"),
                    ),
                    Some(false),
                )
            }
            Ending::Broke(e) => (
                Diagnostic::error(
                    codes::WEBSOCKET,
                    format!("the WebSocket of the client at {at} broke: {e}"),
                ),
                None,
            ),
            Ending::Idle => {
                close_with(&mut link.ws, CloseCode::Normal, "sent nothing");
                (
                    Diagnostic::error(
                        codes::TIMED_OUT,
                        format!(
                            "the client at {at} sent nothing for {} s",
                            self.idle.as_secs_f64()
                        ),
                    ),
                    Some(true),
                )
            }
        };
        self.lost.push(Lost {
            addr,
            peer: link.peer,
            reason,
        });
        if let Some(read) = read {
            self.closing.push(Closing {
                addr,
                peer: link.peer,
                ws: link.ws,
                since: self.now,
                read,
            });
        }
    }

    /// Closes the link to `addr` for the endpoint, which closed its
    /// connection; what it sent and was not read yet is dropped.
    fn release(&mut self, addr: SocketAddr) {
        self.inbox.retain(|(a, _)| *a != addr);
        self.lost.retain(|l| l.addr != addr);
        let Some(mut link) = self.links.remove(&addr) else {
            return;
        };
        self.order.retain(|a| *a != addr);
        start_close(&mut link.ws);
        self.closing.push(Closing {
            addr,
            peer: link.peer,
            ws: link.ws,
            since: self.now,
            read: true,
        });
    }

    /// The links lost whose datagrams were all read.
    fn take_lost(&mut self) -> Vec<(SocketAddr, Diagnostic)> {
        let (ready, waiting): (Vec<Lost>, Vec<Lost>) = std::mem::take(&mut self.lost)
            .into_iter()
            .partition(|l| !self.inbox.iter().any(|(a, _)| *a == l.addr));
        self.lost = waiting;
        ready.into_iter().map(|l| (l.addr, l.reason)).collect()
    }

    fn send(&mut self, to: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        let Some(link) = self.links.get_mut(&to) else {
            return Err(ErrorKind::NotConnected.into());
        };
        let sent = write_link(&mut link.ws, datagram);
        if let Err(e) = &sent
            && e.kind() != ErrorKind::WouldBlock
        {
            self.lose(to, Ending::Broke(e.to_string()));
        }
        sent
    }

    fn recv(&mut self, buf: &mut [u8]) -> Option<(usize, SocketAddr)> {
        let (from, d) = self.inbox.pop_front()?;
        let n = d.len().min(buf.len());
        buf[..n].copy_from_slice(&d[..n]);
        Some((n, from))
    }

    /// The TCP peer of the link at `addr`: open, closing, or lost and not
    /// yet reported.
    fn tcp_peer(&self, addr: SocketAddr) -> Option<SocketAddr> {
        self.links
            .get(&addr)
            .map(|l| l.peer)
            .or_else(|| self.lost.iter().find(|l| l.addr == addr).map(|l| l.peer))
            .or_else(|| self.closing.iter().find(|c| c.addr == addr).map(|c| c.peer))
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // Every link closes, and each close gets its handshake (for at most
        // CLOSE_TIMEOUT) with what the peer sends read meanwhile, so the
        // socket closes with a FIN and not a reset that could lose what the
        // server sent last: its last frame and its Disconnect.
        for (addr, mut link) in self.links.drain() {
            start_close(&mut link.ws);
            self.closing.push(Closing {
                addr,
                peer: link.peer,
                ws: link.ws,
                since: self.now,
                read: true,
            });
        }
        let start = re_core::time::Instant::now();
        loop {
            self.closing.retain_mut(|c| step_close(&mut c.ws, c.read));
            if self.closing.is_empty() || start.elapsed() > CLOSE_TIMEOUT {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

// ------------------------------------------------------------------ client

/// A client's WebSocket to its server, natively: `ws://` only (a native
/// build has no TLS). The endpoint reaches the server at
/// [`WsTransport::server_addr`].
///
/// The TCP connection opens in [`WsTransport::connect`]; the WebSocket
/// handshake then runs in the frame loop, and the datagrams sent meanwhile
/// wait for it (at most 64).
pub struct WsTransport {
    url: String,
    link: ClientLink,
    now: Duration,
    inbox: VecDeque<Vec<u8>>,
    queued: Vec<Vec<u8>>,
    lost: Option<Diagnostic>,
}

enum ClientLink {
    Opening(MidHandshake<ClientHandshake<TcpStream>>),
    Open(WebSocket<TcpStream>),
    /// Its close is finishing ([`step_close`]); what the server sends is
    /// dropped.
    Closing {
        ws: WebSocket<TcpStream>,
        since: Duration,
        read: bool,
    },
    Closed,
}

impl fmt::Debug for WsTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.link {
            ClientLink::Opening(_) => "opening",
            ClientLink::Open(_) => "open",
            ClientLink::Closing { .. } => "closing",
            ClientLink::Closed => "closed",
        };
        f.debug_struct("WsTransport")
            .field("url", &self.url)
            .field("state", &state)
            .finish_non_exhaustive()
    }
}

impl WsTransport {
    /// Connects to the server at `url` (`ws://host:port/path`), waiting at
    /// most `timeout` for TCP to connect.
    ///
    /// # Errors
    ///
    /// `RE8901` when `url` is not a `ws://` address or does not resolve,
    /// `RE8910` when it does not connect (or is `wss://`).
    pub fn connect(url: &str, timeout: Duration) -> Result<WsTransport, Diagnostic> {
        let url = url.trim();
        let not_an_address = |why: String| {
            Diagnostic::error(
                codes::ADDRESS,
                format!("`{url}` is not a WebSocket address: {why}"),
            )
            .with_hint("write it as ws://host:port/, for example ws://192.168.1.20:47800/")
        };
        let uri: tungstenite::http::Uri =
            url.parse().map_err(|e| not_an_address(format!("{e}")))?;
        match uri.scheme_str().map(str::to_ascii_lowercase).as_deref() {
            Some("ws") => {}
            Some("wss") => {
                return Err(Diagnostic::error(
                    codes::WEBSOCKET,
                    format!("cannot connect to {url}: wss:// needs TLS, which a native build does not have"),
                )
                .with_hint("connect to the server's ws:// address, or over UDP with host:port"));
            }
            _ => return Err(not_an_address("it does not start with ws://".into())),
        }
        let host = uri
            .host()
            .map(|h| h.trim_start_matches('[').trim_end_matches(']'))
            .filter(|h| !h.is_empty())
            .ok_or_else(|| not_an_address("it names no host".into()))?;
        let port = uri.port_u16().unwrap_or(80);
        let addrs: Vec<SocketAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|_| {
                Diagnostic::error(codes::ADDRESS, format!("cannot resolve the host of {url}"))
                    .with_hint("check the host name, or use the server's IP address")
            })?
            .collect();
        let timeout = timeout.max(Duration::from_millis(1));
        let mut failed = None;
        let stream = addrs
            .iter()
            .find_map(|a| match TcpStream::connect_timeout(a, timeout) {
                Ok(s) => Some(s),
                Err(e) => {
                    failed = Some(e);
                    None
                }
            })
            .ok_or_else(|| {
                let why = failed.map_or_else(|| "no address".to_string(), |e| e.to_string());
                Diagnostic::error(codes::WEBSOCKET, format!("cannot connect to {url}: {why}"))
                    .with_hint("check that the server is running and takes WebSocket players on that TCP port")
            })?;
        stream.set_nonblocking(true).map_err(|e| {
            Diagnostic::error(codes::WEBSOCKET, format!("cannot connect to {url}: {e}"))
        })?;
        let _ = stream.set_nodelay(true);
        let link = match tungstenite::client::client_with_config(url, stream, Some(config())) {
            Ok((ws, _)) => ClientLink::Open(ws),
            Err(HandshakeError::Interrupted(handshake)) => ClientLink::Opening(handshake),
            Err(HandshakeError::Failure(e)) => {
                return Err(Diagnostic::error(
                    codes::WEBSOCKET,
                    format!("the WebSocket handshake with {url} failed: {e}"),
                ));
            }
        };
        Ok(WsTransport {
            url: url.to_string(),
            link,
            now: Duration::ZERO,
            inbox: VecDeque::new(),
            queued: Vec::new(),
            lost: None,
        })
    }

    /// The address the endpoint reaches the server at.
    pub fn server_addr(&self) -> SocketAddr {
        super::server_addr()
    }

    /// The link is reported lost once, after the datagrams it delivered.
    fn report(&mut self, reason: Diagnostic) {
        self.queued.clear();
        if self.lost.is_none() {
            self.lost = Some(reason);
        }
    }

    /// The open link `ws` ended `why`: its close finishes in later steps.
    fn end(&mut self, mut ws: WebSocket<TcpStream>, why: Ending) {
        let url = &self.url;
        let (reason, read) = match why {
            Ending::Closed(frame) => (
                Diagnostic::error(
                    codes::CLOSED,
                    format!(
                        "the server at {url} closed the connection{}",
                        Ending::detail(frame.as_ref())
                    ),
                ),
                Some(true),
            ),
            Ending::Refused(code, short, e) => {
                close_with(&mut ws, code, short);
                (
                    Diagnostic::error(
                        codes::WEBSOCKET,
                        format!("the server at {url} broke the WebSocket protocol: {e}"),
                    ),
                    Some(false),
                )
            }
            Ending::Broke(e) => (
                Diagnostic::error(
                    codes::WEBSOCKET,
                    format!("the WebSocket to {url} broke: {e}"),
                ),
                None,
            ),
            Ending::Idle => {
                start_close(&mut ws);
                (
                    Diagnostic::error(
                        codes::TIMED_OUT,
                        format!("the server at {url} sent nothing"),
                    ),
                    Some(true),
                )
            }
        };
        self.link = match read {
            Some(read) => ClientLink::Closing {
                ws,
                since: self.now,
                read,
            },
            None => ClientLink::Closed,
        };
        self.report(reason);
    }
}

impl Transport for WsTransport {
    fn send_to(&mut self, to: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        if to != super::server_addr() {
            return Err(ErrorKind::NotConnected.into());
        }
        match &mut self.link {
            ClientLink::Open(ws) => {
                let sent = write_link(ws, datagram);
                if let Err(e) = &sent
                    && e.kind() != ErrorKind::WouldBlock
                    && let ClientLink::Open(ws) =
                        std::mem::replace(&mut self.link, ClientLink::Closed)
                {
                    self.end(ws, Ending::Broke(e.to_string()));
                }
                sent
            }
            ClientLink::Opening(_) => {
                if self.queued.len() < MAX_QUEUED {
                    self.queued.push(datagram.to_vec());
                }
                Ok(())
            }
            ClientLink::Closing { .. } | ClientLink::Closed => Err(ErrorKind::NotConnected.into()),
        }
    }

    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        let Some(d) = self.inbox.pop_front() else {
            return Ok(None);
        };
        let n = d.len().min(buf.len());
        buf[..n].copy_from_slice(&d[..n]);
        Ok(Some((n, super::server_addr())))
    }

    fn local_addr(&self) -> SocketAddr {
        super::client_addr()
    }

    fn advance(&mut self, now: Duration) {
        self.now = now;
        match std::mem::replace(&mut self.link, ClientLink::Closed) {
            ClientLink::Opening(handshake) => match handshake.handshake() {
                Ok((ws, _)) => {
                    self.link = ClientLink::Open(ws);
                    for d in std::mem::take(&mut self.queued) {
                        if self.send_to(super::server_addr(), &d).is_err() {
                            break;
                        }
                    }
                }
                Err(HandshakeError::Interrupted(handshake)) => {
                    self.link = ClientLink::Opening(handshake);
                }
                Err(HandshakeError::Failure(e)) => {
                    self.report(
                        Diagnostic::error(
                            codes::WEBSOCKET,
                            format!("the WebSocket handshake with {} failed: {e}", self.url),
                        )
                        .with_hint("check that the address is a Real Engine server's (it answers WebSocket on its UDP port's number)"),
                    );
                }
            },
            ClientLink::Closing {
                mut ws,
                since,
                read,
            } => {
                if now.saturating_sub(since) <= CLOSE_TIMEOUT && step_close(&mut ws, read) {
                    self.link = ClientLink::Closing { ws, since, read };
                }
            }
            link => self.link = link,
        }
        let ClientLink::Open(ws) = &mut self.link else {
            return;
        };
        let mut ending = None;
        while self.inbox.len() < MAX_INBOX {
            match read_link(ws) {
                Read::Datagram(d) => self.inbox.push_back(d),
                Read::Control => {}
                Read::Empty => break,
                Read::Over(why) => {
                    ending = Some(why);
                    break;
                }
            }
        }
        if ending.is_none()
            && let Err(e) = flush_link(ws)
        {
            ending = Some(Ending::Broke(e));
        }
        if let Some(why) = ending
            && let ClientLink::Open(ws) = std::mem::replace(&mut self.link, ClientLink::Closed)
        {
            self.end(ws, why);
        }
    }

    fn take_lost(&mut self) -> Vec<(SocketAddr, Diagnostic)> {
        // Not before the endpoint read what came first (a Disconnect says why).
        if !self.inbox.is_empty() {
            return Vec::new();
        }
        self.lost
            .take()
            .map(|d| vec![(super::server_addr(), d)])
            .unwrap_or_default()
    }

    fn release(&mut self, peer: SocketAddr) {
        if peer != super::server_addr() {
            return;
        }
        self.link = match std::mem::replace(&mut self.link, ClientLink::Closed) {
            ClientLink::Open(mut ws) => {
                start_close(&mut ws);
                ClientLink::Closing {
                    ws,
                    since: self.now,
                    read: true,
                }
            }
            closing @ ClientLink::Closing { .. } => closing,
            ClientLink::Opening(_) | ClientLink::Closed => ClientLink::Closed,
        };
        self.inbox.clear();
        self.queued.clear();
        self.lost = None;
    }

    fn describe_peer(&self, peer: SocketAddr) -> String {
        if peer == super::server_addr() {
            self.url.clone()
        } else {
            peer.to_string()
        }
    }
}

impl Drop for WsTransport {
    fn drop(&mut self) {
        // The close frame goes out, and what the server sent is read, so the
        // socket closes with a FIN and not a reset that could lose the frame.
        match &mut self.link {
            ClientLink::Open(ws) => {
                start_close(ws);
                step_close(ws, true);
            }
            ClientLink::Closing { ws, read, .. } => {
                step_close(ws, *read);
            }
            ClientLink::Opening(_) | ClientLink::Closed => {}
        }
    }
}
