//! Real Engine networking: encrypted connections between a server and its
//! clients over UDP, with our own reliability, polled from the frame loop
//! (ADR-0050, ADR-0067).
//!
//! * [`Endpoint`]: a server with many connections or a client with one. A
//!   frame calls [`Endpoint::poll`] first and [`Endpoint::flush`] last; time
//!   is a [`Duration`](std::time::Duration) the caller gives, so tests run on
//!   a simulated clock.
//! * Two channels ([`Channel`]): unreliable (one datagram) and reliable
//!   ordered (fragmented, resent until acked, at most [`MAX_MESSAGE`] bytes).
//! * A handshake that answers a forged source with nothing larger than what
//!   it sent and keeps no state until the client proves its address, and that
//!   refuses another transport or protocol version (`RE8903`), a full server
//!   (`RE8904`) or another game (`RE8905`).
//! * Encryption and authentication (ADR-0067): the handshake is
//!   `Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s` to the server's key
//!   ([`ServerKey`], which players see as a [`Fingerprint`]), with a game's
//!   [`JoinCode`] as its pre-shared key, and every later packet is sealed
//!   with ChaCha20-Poly1305 under its packet number. A packet changed,
//!   forged or replayed on the way is dropped and counted
//!   ([`ConnStats::rejected`], [`ConnStats::replayed`]).
//! * Liveness: keepalives, a timeout (`RE8906`), and Disconnect with a reason
//!   (`RE8907`).
//! * A token bucket per connection, and [`Endpoint::congested`] for senders
//!   that can coalesce what they send.
//! * The [`Transport`] seam: [`UdpTransport`], the in-process [`MemoryNet`],
//!   and [`Conditioned`] for seeded loss, duplication, delay and reordering.
//! * WebSocket transports ([`ws`]) for players in a browser: a server's
//!   [`ServerTransport`] takes them on TCP at its UDP port, and a client's
//!   [`WsTransport`] is the page's `WebSocket` (or tungstenite's, natively).
//!   The packets inside are sealed the same way.
//! * [`wire`]: the packet and handshake decoders as pure functions of bytes,
//!   for fuzzing.
//!
//! The crate knows nothing about scenes or scripts: `re_compat` puts its wire
//! messages on it. Without a join code, anyone who reaches a server's port
//! can join it; the encryption keeps what they send private and whole.
//!
//! # Example
//!
//! ```
//! use std::time::Duration;
//! use re_net::{Channel, Config, Endpoint, Event, MemoryNet};
//!
//! let net = MemoryNet::new();
//! let mut server = Endpoint::server(net.bind(), Config::server(1, 42));
//! let mut client = Endpoint::client(net.bind(), server.local_addr(), Config::client(1, 42));
//! let mut got = None;
//! for frame in 0..20u64 {
//!     let now = Duration::from_millis(frame * 16);
//!     for e in client.poll(now) {
//!         if let Event::Connected(peer) = e {
//!             client.send(peer, Channel::Reliable, b"hello").unwrap();
//!         }
//!     }
//!     for e in server.poll(now) {
//!         if let Event::Message { bytes, .. } = e {
//!             got = Some(bytes);
//!         }
//!     }
//!     client.flush(now);
//!     server.flush(now);
//! }
//! assert_eq!(got.as_deref(), Some(&b"hello"[..]));
//! ```
//!
//! Every diagnostic this crate raises has a code from `RE8900`–`RE8999` and an
//! explain page listed in [`DIAGNOSTICS`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod codes;
mod conn;
mod endpoint;
mod keys;
mod noise;
mod packet;
mod replay;
mod transport;
pub mod wire;
pub mod ws;

pub use conn::{Channel, ConnStats};
pub use endpoint::{Config, Endpoint, Event, PeerId};
pub use keys::{Fingerprint, JoinCode, KEY_FILE, ServerKey};
pub use packet::{FRAGMENT, MAX_MESSAGE, MTU, TRANSPORT_VERSION};
pub use transport::{
    Conditioned, ConditionerStats, LinkConditions, MemoryNet, MemoryTransport, Transport,
    UdpTransport, resolve,
};
#[cfg(not(target_arch = "wasm32"))]
pub use ws::ServerTransport;
pub use ws::WsTransport;

use re_core::DiagInfo;

/// Every diagnostic code `re_net` raises, with its explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::BIND,
        title: "Cannot open a network socket",
        page: include_str!("../explain/RE8900.md"),
    },
    DiagInfo {
        code: codes::ADDRESS,
        title: "Address does not resolve",
        page: include_str!("../explain/RE8901.md"),
    },
    DiagInfo {
        code: codes::NO_ANSWER,
        title: "No answer from the server",
        page: include_str!("../explain/RE8902.md"),
    },
    DiagInfo {
        code: codes::VERSION_MISMATCH,
        title: "Network version mismatch",
        page: include_str!("../explain/RE8903.md"),
    },
    DiagInfo {
        code: codes::SERVER_FULL,
        title: "Server full",
        page: include_str!("../explain/RE8904.md"),
    },
    DiagInfo {
        code: codes::GAME_MISMATCH,
        title: "Server runs a different game",
        page: include_str!("../explain/RE8905.md"),
    },
    DiagInfo {
        code: codes::TIMED_OUT,
        title: "Connection timed out",
        page: include_str!("../explain/RE8906.md"),
    },
    DiagInfo {
        code: codes::CLOSED,
        title: "Connection closed by the other end",
        page: include_str!("../explain/RE8907.md"),
    },
    DiagInfo {
        code: codes::SEND,
        title: "Message cannot be sent",
        page: include_str!("../explain/RE8908.md"),
    },
    DiagInfo {
        code: codes::NOT_ONLINE,
        title: "This run cannot go online",
        page: include_str!("../explain/RE8909.md"),
    },
    DiagInfo {
        code: codes::WEBSOCKET,
        title: "WebSocket connection failed",
        page: include_str!("../explain/RE8910.md"),
    },
    DiagInfo {
        code: codes::CANNOT_SERVE,
        title: "This platform cannot serve",
        page: include_str!("../explain/RE8911.md"),
    },
    DiagInfo {
        code: codes::HANDSHAKE_REFUSED,
        title: "Server refused the handshake",
        page: include_str!("../explain/RE8912.md"),
    },
    DiagInfo {
        code: codes::JOIN_CODE_MISMATCH,
        title: "Join code does not fit the server",
        page: include_str!("../explain/RE8913.md"),
    },
    DiagInfo {
        code: codes::KEY_MISMATCH,
        title: "Server key does not match",
        page: include_str!("../explain/RE8914.md"),
    },
    DiagInfo {
        code: codes::BAD_CODE,
        title: "Join code or fingerprint does not parse",
        page: include_str!("../explain/RE8915.md"),
    },
    DiagInfo {
        code: codes::SECRETS,
        title: "No server key or random numbers",
        page: include_str!("../explain/RE8916.md"),
    },
];

#[cfg(test)]
mod tests {
    #[test]
    fn the_catalogue_is_valid() {
        let problems = re_core::validate_catalog(super::DIAGNOSTICS, 8900..=8999);
        assert!(problems.is_empty(), "{problems:?}");
    }
}
