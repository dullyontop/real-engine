//! The WebSocket transports over real sockets on 127.0.0.1, on a simulated
//! 60 Hz clock (each frame also sleeps a millisecond, so TCP moves): a
//! WebSocket client and a UDP client on one server, closes seen at once on
//! both sides, and the listener's limits.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use re_core::Code;
use re_net::ws::{MAX_WS_MESSAGE, is_websocket_peer};
use re_net::{
    Channel, Config, Endpoint, Event, PeerId, ServerTransport, Transport, UdpTransport,
    WsTransport, codes,
};

const TICK: Duration = Duration::from_micros(16_667);

type Client = Endpoint<Box<dyn Transport>>;

struct Net {
    now: Duration,
    server: Endpoint<ServerTransport>,
    clients: Vec<Option<Client>>,
    /// Every event, by endpoint: 0 is the server, then the clients.
    events: Vec<Vec<Event>>,
}

impl Net {
    fn new(max_links: usize) -> Net {
        let transport =
            ServerTransport::with_websocket(([127, 0, 0, 1], 0).into(), max_links).unwrap();
        Net {
            now: Duration::ZERO,
            server: Endpoint::server(transport, Config::server(3, 42)),
            clients: Vec::new(),
            events: vec![Vec::new()],
        }
    }

    fn url(&self) -> String {
        let addr = self.server.transport().websocket_addr().unwrap();
        format!("ws://{addr}/")
    }

    fn add_ws(&mut self) -> usize {
        let ws = WsTransport::connect(&self.url(), Duration::from_secs(5)).unwrap();
        let server = ws.server_addr();
        self.add(Endpoint::client(
            Box::new(ws),
            server,
            Config::client(3, 42),
        ))
    }

    fn add_udp(&mut self) -> usize {
        let server = self.server.local_addr();
        let udp = UdpTransport::for_server(server).unwrap();
        self.add(Endpoint::client(
            Box::new(udp),
            server,
            Config::client(3, 42),
        ))
    }

    fn add(&mut self, client: Client) -> usize {
        self.clients.push(Some(client));
        self.events.push(Vec::new());
        self.clients.len()
    }

    fn client(&mut self, who: usize) -> &mut Client {
        self.clients[who - 1].as_mut().expect("client running")
    }

    fn frame(&mut self) {
        self.now += TICK;
        let now = self.now;
        self.events[0].extend(self.server.poll(now));
        for (i, c) in self.clients.iter_mut().enumerate() {
            if let Some(c) = c {
                self.events[i + 1].extend(c.poll(now));
            }
        }
        self.events[0].extend(self.server.flush(now));
        for (i, c) in self.clients.iter_mut().enumerate() {
            if let Some(c) = c {
                self.events[i + 1].extend(c.flush(now));
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    /// Runs frames until `done`, failing after `frames` of them.
    fn until(&mut self, frames: usize, what: &str, mut done: impl FnMut(&Net) -> bool) {
        for _ in 0..frames {
            if done(self) {
                return;
            }
            self.frame();
        }
        assert!(done(self), "{what} did not happen in {frames} frames");
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

    fn messages(&self, who: usize, from: Option<PeerId>) -> Vec<Vec<u8>> {
        self.events[who]
            .iter()
            .filter_map(|e| match e {
                Event::Message {
                    peer,
                    channel: Channel::Reliable,
                    bytes,
                } if from.is_none_or(|f| f == *peer) => Some(bytes.clone()),
                _ => None,
            })
            .collect()
    }
}

/// The `i`th test message: a few bytes to several fragments.
fn message(i: u32) -> Vec<u8> {
    let len = if i.is_multiple_of(25) {
        6000
    } else {
        10 + (i as usize * 37) % 400
    };
    (0..len).map(|k| (i as usize * 131 + k * 7) as u8).collect()
}

#[test]
fn a_websocket_client_and_a_udp_client_play_on_one_server() {
    let mut net = Net::new(8);
    let tcp = net.server.transport().websocket_addr().unwrap();
    assert_eq!(tcp.port(), net.server.local_addr().port());
    let ws = net.add_ws();
    let udp = net.add_udp();
    net.until(300, "both clients connecting", |n| {
        n.connected(0).len() == 2 && n.connected(ws).len() == 1 && n.connected(udp).len() == 1
    });
    let peers = net.server.peers();
    let labels: Vec<String> = peers
        .iter()
        .map(|p| net.server.describe(*p).unwrap())
        .collect();
    let ws_peer = peers
        .iter()
        .copied()
        .find(|p| is_websocket_peer(net.server.addr_of(*p).unwrap()))
        .expect("one peer is a WebSocket");
    assert!(
        labels.iter().any(|l| l.ends_with("(WebSocket)")),
        "{labels:?}"
    );
    assert_eq!(net.server.transport().websocket_links(), 1);

    // Each client sends 100 reliable messages and gets its own back.
    let server_ws = net.client(ws).peers()[0];
    let server_udp = net.client(udp).peers()[0];
    for i in 0..100 {
        net.client(ws)
            .send(server_ws, Channel::Reliable, &message(i))
            .unwrap();
        net.client(udp)
            .send(server_udp, Channel::Reliable, &message(1000 + i))
            .unwrap();
    }
    let mut echoed = 0;
    for _ in 0..2000 {
        net.frame();
        let arrived: Vec<(PeerId, Vec<u8>)> = net.events[0]
            .drain(..)
            .filter_map(|e| match e {
                Event::Message { peer, bytes, .. } => Some((peer, bytes)),
                _ => None,
            })
            .collect();
        for (peer, bytes) in arrived {
            net.server.send(peer, Channel::Reliable, &bytes).unwrap();
            echoed += 1;
        }
        if net.messages(ws, None).len() == 100 && net.messages(udp, None).len() == 100 {
            break;
        }
    }
    assert_eq!(echoed, 200);
    let want_ws: Vec<Vec<u8>> = (0..100).map(message).collect();
    let want_udp: Vec<Vec<u8>> = (1000..1100).map(message).collect();
    assert_eq!(net.messages(ws, None), want_ws);
    assert_eq!(net.messages(udp, None), want_udp);
    // The WebSocket peer's messages came over its link.
    let stats = net.server.stats(ws_peer).unwrap();
    assert!(stats.messages_received >= 100, "{stats:?}");
}

#[test]
fn a_closed_websocket_closes_its_connection_at_once() {
    let mut net = Net::new(8);
    let ws = net.add_ws();
    net.until(300, "the client connecting", |n| n.connected(0).len() == 1);
    // The client goes away without a Disconnect: its WebSocket closes.
    net.clients[ws - 1] = None;
    // Far sooner than the 10 s timeout (600 frames).
    net.until(300, "the server seeing the close", |n| {
        !n.disconnects(0).is_empty()
    });
    let code = net.disconnects(0)[0];
    assert!(code == codes::CLOSED || code == codes::WEBSOCKET, "{code}");
    assert!(net.server.peers().is_empty());
    net.until(300, "the link closing", |n| {
        n.server.transport().websocket_links() == 0
    });
}

#[test]
fn a_server_disconnect_reaches_the_websocket_client_with_its_reason() {
    let mut net = Net::new(8);
    let ws = net.add_ws();
    net.until(300, "the client connecting", |n| n.connected(ws).len() == 1);
    let peer = net.server.peers()[0];
    let now = net.now;
    net.server.disconnect(now, peer, "kicked: AFK");
    net.until(300, "the client seeing the disconnect", |n| {
        !n.disconnects(ws).is_empty()
    });
    let reason = net.events[ws]
        .iter()
        .find_map(|e| match e {
            Event::Disconnected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(reason.code, codes::CLOSED);
    assert!(reason.message.contains("kicked: AFK"), "{}", reason.message);
    assert!(
        reason.message.contains("ws://127.0.0.1:"),
        "{}",
        reason.message
    );
    net.until(300, "the link closing", |n| {
        n.server.transport().websocket_links() == 0
    });
}

#[test]
fn a_websocket_client_leaving_says_so() {
    let mut net = Net::new(8);
    let ws = net.add_ws();
    net.until(300, "the client connecting", |n| n.connected(0).len() == 1);
    let now = net.now;
    net.client(ws).disconnect_all(now, "the player left");
    net.client(ws).flush(now);
    net.until(300, "the server seeing the leave", |n| {
        !n.disconnects(0).is_empty()
    });
    let reason = net.events[0]
        .iter()
        .find_map(|e| match e {
            Event::Disconnected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(reason.code, codes::CLOSED);
    assert!(
        reason.message.contains("the player left"),
        "{}",
        reason.message
    );
    assert!(reason.message.contains("(WebSocket)"), "{}", reason.message);
}

#[test]
fn a_websocket_to_nothing_or_to_a_bad_address_fails_at_once() {
    // A port nothing listens on: bind one, then close it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let refused = WsTransport::connect(&format!("ws://127.0.0.1:{port}/"), Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(refused.code, codes::WEBSOCKET);
    let tls = WsTransport::connect("wss://127.0.0.1:1/", Duration::from_secs(1)).unwrap_err();
    assert_eq!(tls.code, codes::WEBSOCKET);
    assert!(tls.message.contains("TLS"), "{}", tls.message);
    for bad in ["http://127.0.0.1:1/", "127.0.0.1:1", "ws://"] {
        let e = WsTransport::connect(bad, Duration::from_secs(1)).unwrap_err();
        assert_eq!(e.code, codes::ADDRESS, "{bad}: {}", e.message);
    }
}

#[test]
fn a_websocket_that_never_joins_fails_its_client_handshake() {
    // A WebSocket server that is not a game server: it accepts the WebSocket
    // and then closes it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        FakeWs::accept(stream).close();
    });
    let ws = WsTransport::connect(&format!("ws://{addr}/"), Duration::from_secs(5)).unwrap();
    let to = ws.server_addr();
    let mut client: Client = Endpoint::client(Box::new(ws), to, Config::client(3, 42));
    let mut now = Duration::ZERO;
    let mut failed = None;
    // Fewer frames than the 10 s the handshake waits for an answer.
    for _ in 0..500 {
        now += TICK;
        for e in client.poll(now).into_iter().chain(client.flush(now)) {
            if let Event::Disconnected { reason, .. } = e {
                failed = Some(reason);
            }
        }
        if failed.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    server.join().unwrap();
    let failed = failed.expect("the handshake fails when the link closes");
    assert!(
        failed.code == codes::CLOSED || failed.code == codes::WEBSOCKET,
        "{}: {}",
        failed.code,
        failed.message
    );
}

/// Just enough of a WebSocket server to accept and close: the RFC 6455
/// handshake by hand, so the test needs no WebSocket crate of its own.
struct FakeWs(TcpStream);

impl FakeWs {
    fn accept(mut stream: TcpStream) -> FakeWs {
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        let text = String::from_utf8(request).unwrap();
        let key = text
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("sec-websocket-key")
                    .then(|| value.trim().to_string())
            })
            .unwrap();
        let accept = accept_key(&key);
        write!(
            stream,
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .unwrap();
        FakeWs(stream)
    }

    /// Sends a close frame (1000, unmasked: server to client) and hangs up.
    fn close(mut self) {
        let _ = self.0.write_all(&[0x88, 0x02, 0x03, 0xe8]);
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// `Sec-WebSocket-Accept` for `key`: base64 of SHA-1 of the key and the
/// RFC 6455 GUID (a small SHA-1 here, test-only).
fn accept_key(key: &str) -> String {
    let mut data = key.as_bytes().to_vec();
    data.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64(&sha1(&data))
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn the_test_sha1_is_sha1() {
    // RFC 6455's example.
    assert_eq!(
        accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

/// A raw TCP client of the listener.
fn raw(net: &Net) -> TcpStream {
    let addr: SocketAddr = net.server.transport().websocket_addr().unwrap();
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

/// True once the server closed `s` (a read returns 0 or fails).
fn closed_by_server(net: &mut Net, s: &mut TcpStream, frames: usize) -> bool {
    s.set_nonblocking(true).unwrap();
    let mut buf = [0u8; 4096];
    for _ in 0..frames {
        net.frame();
        match s.read(&mut buf) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return true,
        }
    }
    false
}

/// A client-side handshake by hand: the request, and the 101 read back.
fn raw_websocket(net: &mut Net) -> TcpStream {
    let mut s = raw(net);
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    write!(
        s,
        "GET / HTTP/1.1\r\nHost: test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n"
    )
    .unwrap();
    s.set_nonblocking(true).unwrap();
    let mut response = Vec::new();
    let mut buf = [0u8; 1024];
    for _ in 0..300 {
        net.frame();
        if let Ok(n) = s.read(&mut buf) {
            response.extend_from_slice(&buf[..n]);
        }
        if response.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&response);
    assert!(text.starts_with("HTTP/1.1 101"), "{text}");
    s.set_nonblocking(false).unwrap();
    s
}

/// A masked client frame with `opcode` and `payload`.
fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0x80 | opcode];
    let mask = [1u8, 2, 3, 4];
    match payload.len() {
        n if n < 126 => f.push(0x80 | n as u8),
        n if n < 65536 => {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

#[test]
fn what_is_not_a_websocket_is_closed() {
    let mut net = Net::new(8);
    // A plain HTTP request.
    let mut s = raw(&net);
    s.write_all(b"GET / HTTP/1.1\r\nHost: test\r\n\r\n")
        .unwrap();
    assert!(closed_by_server(&mut net, &mut s, 250));
    // Silence: the handshake times out after 5 s (300 frames).
    let mut s = raw(&net);
    assert!(!closed_by_server(&mut net, &mut s, 200));
    assert!(closed_by_server(&mut net, &mut s, 400));
    assert_eq!(net.server.transport().websocket_links(), 0);
}

#[test]
fn text_and_oversized_messages_close_the_link() {
    let mut net = Net::new(8);
    let mut s = raw_websocket(&mut net);
    net.until(30, "the link opening", |n| {
        n.server.transport().websocket_links() == 1
    });
    s.write_all(&frame(0x1, b"hello")).unwrap();
    net.until(300, "the text closing the link", |n| {
        n.server.transport().websocket_links() == 0
    });
    let mut s = raw_websocket(&mut net);
    net.until(30, "the link opening", |n| {
        n.server.transport().websocket_links() == 1
    });
    s.write_all(&frame(0x2, &vec![0; MAX_WS_MESSAGE + 1]))
        .unwrap();
    net.until(300, "the oversized message closing the link", |n| {
        n.server.transport().websocket_links() == 0
    });
    // Neither was ever a connection.
    assert!(net.disconnects(0).is_empty());
}

#[test]
fn links_past_the_limit_are_closed_and_quiet_ones_expire() {
    let mut net = Net::new(1);
    let _first = raw_websocket(&mut net);
    net.until(30, "the first link opening", |n| {
        n.server.transport().websocket_links() == 1
    });
    let mut second = raw(&net);
    assert!(closed_by_server(&mut net, &mut second, 250));
    assert_eq!(net.server.transport().websocket_links(), 1);
    // The first link never joins and sends nothing: it expires after 15 s.
    net.until(1200, "the quiet link expiring", |n| {
        n.server.transport().websocket_links() == 0
    });
}

#[test]
fn the_listener_can_be_left_out() {
    let udp = ServerTransport::udp(([127, 0, 0, 1], 0).into()).unwrap();
    assert_eq!(udp.websocket_addr(), None);
    assert!(udp.describe_peer("127.0.0.1:5".parse().unwrap()) == "127.0.0.1:5");
}
