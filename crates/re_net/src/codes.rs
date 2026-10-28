//! The diagnostic codes `re_net` owns (range `RE8900`–`RE8999`).
//!
//! Each code has an explain page in `crates/re_net/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// A socket could not be opened or bound to the address asked for.
pub const BIND: Code = Code::new("RE8900");

/// An address does not parse or does not resolve.
pub const ADDRESS: Code = Code::new("RE8901");

/// The server did not answer the handshake in time.
pub const NO_ANSWER: Code = Code::new("RE8902");

/// The server runs another network protocol version.
pub const VERSION_MISMATCH: Code = Code::new("RE8903");

/// The server already has as many players as it allows.
pub const SERVER_FULL: Code = Code::new("RE8904");

/// The server runs a different game (another build or other content).
pub const GAME_MISMATCH: Code = Code::new("RE8905");

/// A connection went silent past the timeout, or stopped acknowledging what
/// it was sent.
pub const TIMED_OUT: Code = Code::new("RE8906");

/// The other end closed the connection (a kick, a server shutting down, a
/// player leaving).
pub const CLOSED: Code = Code::new("RE8907");

/// A message cannot be sent: it is too large for its channel, or the
/// connection does not exist.
pub const SEND: Code = Code::new("RE8908");

/// A run cannot go online: the project is not a Roblox-style project, or
/// the game already started.
pub const NOT_ONLINE: Code = Code::new("RE8909");

/// A WebSocket did not connect, its handshake failed, or the link broke.
pub const WEBSOCKET: Code = Code::new("RE8910");

/// This platform cannot serve: a browser build is always a client.
pub const CANNOT_SERVE: Code = Code::new("RE8911");

/// The server refused the handshake: the join code is wrong, or the client
/// encrypted to a key that is not the server's.
pub const HANDSHAKE_REFUSED: Code = Code::new("RE8912");

/// The join code does not fit the server: it needs one and none was given,
/// or one was given to a server that uses none.
pub const JOIN_CODE_MISMATCH: Code = Code::new("RE8913");

/// The server's key is not the one pinned with its fingerprint.
pub const KEY_MISMATCH: Code = Code::new("RE8914");

/// A join code or a server key's fingerprint does not parse.
pub const BAD_CODE: Code = Code::new("RE8915");

/// The platform has no secure random numbers, or the server key file cannot
/// be read, parsed or written.
pub const SECRETS: Code = Code::new("RE8916");
