//! WebSocket transports (ADR-0050's amendment for PR "web"): a browser cannot
//! send UDP, so its players reach a server over a WebSocket.
//!
//! Each binary WebSocket message is one datagram, so the endpoint's protocol
//! runs unchanged on top. Its acks and resends cost little over TCP, and both
//! kinds of player go through one code path.
//!
//! * [`ServerTransport`] (native): a server's UDP socket and, on TCP at the
//!   same port, a WebSocket listener. Each WebSocket peer gets an address of
//!   its own in the IPv6 discard prefix `100::/64` (RFC 6666), which no
//!   datagram comes from, so the endpoint tells connections apart by address
//!   as it does on UDP.
//! * [`WsTransport`]: a client's WebSocket to its server. In a browser it is
//!   the page's `WebSocket`; natively it is tungstenite over a `TcpStream`,
//!   for `real run --connect ws://…` and for tests.
//!
//! The listener speaks plain `ws://`. A page served over HTTPS may only open
//! `wss://`, which needs a TLS proxy in front of the server. The traffic is not
//! encrypted either way (ADR-0050 §1).

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(not(target_arch = "wasm32"))]
mod native;

#[cfg(target_arch = "wasm32")]
pub use browser::WsTransport;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{ServerTransport, WsTransport};

/// The largest WebSocket message a peer may send. A datagram is at most
/// [`MTU`](crate::MTU) bytes; the rest is room for a later MTU, so another
/// version's handshake is refused by the endpoint and not by the link.
pub const MAX_WS_MESSAGE: usize = 4096;

/// The most datagrams a WebSocket transport keeps for the endpoint's next
/// poll. A server stops reading its links past it (TCP holds the rest); a
/// browser drops what comes past it, as a full socket buffer would.
const MAX_INBOX: usize = 4096;

/// The most datagrams a client queues while its WebSocket opens.
const MAX_QUEUED: usize = 64;

/// The most bytes a link may have waiting to be written. A datagram past it
/// is lost, as on a congested network; the endpoint's reliable channel sends
/// it again, and a peer that stops reading times out (`RE8906`).
const MAX_WRITE_BUFFER: usize = 1 << 20;

/// True for an address a WebSocket transport gave a peer (`100::/64`).
pub fn is_websocket_peer(addr: SocketAddr) -> bool {
    matches!(addr.ip(), IpAddr::V6(ip) if ip.segments()[..4] == [0x100, 0, 0, 0])
}

/// The `n`th address a transport gives its WebSocket peers.
fn virtual_addr(n: u64) -> SocketAddr {
    let s = |shift: u32| (n >> shift) as u16;
    let ip = Ipv6Addr::new(0x100, 0, 0, 0, s(48), s(32), s(16), s(0));
    SocketAddr::new(IpAddr::V6(ip), 0)
}

/// The address a client's [`WsTransport`] gives its server.
fn server_addr() -> SocketAddr {
    virtual_addr(1)
}

/// The address a client's [`WsTransport`] gives itself.
fn client_addr() -> SocketAddr {
    virtual_addr(0)
}

/// True when `address` names a WebSocket server (`ws://` or `wss://`), not a
/// UDP one (`host:port`).
pub fn is_websocket_url(address: &str) -> bool {
    let lower = address.trim().to_ascii_lowercase();
    lower.starts_with("ws://") || lower.starts_with("wss://")
}

/// `address` as a WebSocket URL: unchanged when it is one, else
/// `ws://host:port/` with `default_port` when it has no port. A browser
/// reaches every server this way.
pub fn websocket_url(address: &str, default_port: u16) -> String {
    let address = address.trim();
    if is_websocket_url(address) {
        return address.to_string();
    }
    let has_port = match address.rsplit_once(':') {
        // `[::1]:47800` or `host:47800`; a bare IPv6 address has more colons.
        Some((host, port)) => {
            port.parse::<u16>().is_ok() && (host.ends_with(']') || !host.contains(':'))
        }
        None => false,
    };
    let host = if !has_port && address.contains(':') && !address.starts_with('[') {
        format!("[{address}]")
    } else {
        address.to_string()
    };
    if has_port {
        format!("ws://{host}/")
    } else {
        format!("ws://{host}:{default_port}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_peers_have_addresses_no_datagram_comes_from() {
        let a = virtual_addr(7);
        assert!(is_websocket_peer(a));
        assert_eq!(a.to_string(), "[100::7]:0");
        assert!(is_websocket_peer(virtual_addr(u64::MAX)));
        assert_ne!(virtual_addr(1 << 32), virtual_addr(1));
        assert!(!is_websocket_peer("127.0.0.1:47800".parse().unwrap()));
        assert!(!is_websocket_peer("[::1]:47800".parse().unwrap()));
        assert!(!is_websocket_peer("[100:0:0:1::7]:0".parse().unwrap()));
    }

    #[test]
    fn addresses_become_websocket_urls() {
        assert!(is_websocket_url("ws://host:1/"));
        assert!(is_websocket_url(" WSS://host/game"));
        assert!(!is_websocket_url("host:47800"));
        assert_eq!(websocket_url("ws://a:1/x", 47800), "ws://a:1/x");
        assert_eq!(
            websocket_url("192.168.1.20", 47800),
            "ws://192.168.1.20:47800/"
        );
        assert_eq!(websocket_url("host:5000", 47800), "ws://host:5000/");
        assert_eq!(websocket_url("[::1]:5000", 47800), "ws://[::1]:5000/");
        assert_eq!(websocket_url("::1", 47800), "ws://[::1]:47800/");
    }
}
