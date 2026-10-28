//! Transport version 2 read as pure functions of bytes (ADR-0067 §3), for
//! fuzzing and tools: nothing here does I/O, keeps state or draws a random
//! number, so the same bytes always give the same answer.
//!
//! [`decode`] says what any datagram is; [`open`] opens a sealed one with one
//! direction's session key, and [`data`] and [`disconnect`] read its body.
//! [`respond`] reads the handshake's first message as a server does and
//! [`finish`] the second as a client does; [`initiate`] writes the first, as
//! a seed for a fuzzer's corpus.
//!
//! ```
//! use re_net::ServerKey;
//! use re_net::wire::{finish, initiate, respond};
//!
//! let server = ServerKey::from_bytes([7; 32]);
//! let (protocol, game, salt) = (2, 42, 99);
//! let msg1 = initiate(&server.public_key(), None, protocol, game, salt, &[1; 32]).unwrap();
//! let msg2 = respond(&server, None, protocol, game, salt, &msg1, &[2; 32]).unwrap();
//! assert!(finish(&server.public_key(), None, protocol, game, salt, &[1; 32], &msg2));
//! // Another salt is another handshake.
//! assert!(respond(&server, None, protocol, game, salt + 1, &msg1, &[2; 32]).is_none());
//! ```

use x25519_dalek::StaticSecret;

use crate::endpoint::prologue;
use crate::keys::{self, JoinCode, ServerKey};
use crate::noise::{self, Cipher, Initiator};
use crate::packet::HEADER;

pub use crate::packet::{Chunk, DataBody, Kind, Packet, data, decode, disconnect};

/// Opens a sealed packet in place with `key`, one direction's session key:
/// its body, or `None` when the datagram is not a sealed packet or fails its
/// tag (and is then left as it was).
pub fn open<'a>(key: &[u8; 32], datagram: &'a mut [u8]) -> Option<&'a [u8]> {
    let Packet::Sealed { number, .. } = decode(datagram) else {
        return None;
    };
    let len = Cipher::new(key).open(number, datagram, HEADER).ok()?;
    Some(&datagram[HEADER..HEADER + len])
}

/// A client's first handshake message (`-> psk, e, es`) to the server key
/// `server_key` with `code`, from the ephemeral key `ephemeral`; `None` when
/// `server_key` is of low order.
pub fn initiate(
    server_key: &[u8; 32],
    code: Option<&JoinCode>,
    protocol: u32,
    game: u64,
    salt: u64,
    ephemeral: &[u8; 32],
) -> Option<Vec<u8>> {
    let psk = keys::psk(server_key, code);
    let e = StaticSecret::from(*ephemeral);
    let prologue = prologue(protocol, game, salt);
    let (_, msg1) = Initiator::start(server_key, &psk, &prologue, &e, &[]).ok()?;
    Some(msg1)
}

/// A server's reading of a Response's first handshake message: its answer
/// (`<- e, ee`) from the ephemeral key `ephemeral`, or `None` when `msg1` was
/// not written to `server` with `code` for this protocol, game and salt.
pub fn respond(
    server: &ServerKey,
    code: Option<&JoinCode>,
    protocol: u32,
    game: u64,
    salt: u64,
    msg1: &[u8],
    ephemeral: &[u8; 32],
) -> Option<Vec<u8>> {
    let public = server.public_key();
    let psk = keys::psk(&public, code);
    let e = StaticSecret::from(*ephemeral);
    let prologue = prologue(protocol, game, salt);
    let (_, msg2, _, _) =
        noise::respond(server.secret(), &public, &psk, &prologue, msg1, &e, &[]).ok()?;
    Some(msg2)
}

/// A client's reading of an Accept's second handshake message: true when
/// `msg2` answers the first message [`initiate`] writes from the same
/// arguments.
pub fn finish(
    server_key: &[u8; 32],
    code: Option<&JoinCode>,
    protocol: u32,
    game: u64,
    salt: u64,
    ephemeral: &[u8; 32],
    msg2: &[u8],
) -> bool {
    let psk = keys::psk(server_key, code);
    let e = StaticSecret::from(*ephemeral);
    let prologue = prologue(protocol, game, salt);
    Initiator::start(server_key, &psk, &prologue, &e, &[])
        .and_then(|(initiator, _)| initiator.finish(msg2))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{self, Kind};

    #[test]
    fn a_join_code_changes_every_message() {
        let server = ServerKey::from_bytes([7; 32]);
        let code: JoinCode = "KD4RT-7XWQP".parse().unwrap();
        let other: JoinCode = "KD4RT-7XWQQ".parse().unwrap();
        let public = server.public_key();
        let msg1 = initiate(&public, Some(&code), 2, 42, 99, &[1; 32]).unwrap();
        assert!(respond(&server, None, 2, 42, 99, &msg1, &[2; 32]).is_none());
        assert!(respond(&server, Some(&other), 2, 42, 99, &msg1, &[2; 32]).is_none());
        let msg2 = respond(&server, Some(&code), 2, 42, 99, &msg1, &[2; 32]).unwrap();
        assert!(finish(&public, Some(&code), 2, 42, 99, &[1; 32], &msg2));
        assert!(!finish(&public, None, 2, 42, 99, &[1; 32], &msg2));
        assert!(!finish(&public, Some(&code), 3, 42, 99, &[1; 32], &msg2));
    }

    #[test]
    fn a_sealed_packet_opens_only_whole_and_with_its_key() {
        let key = [5u8; 32];
        let mut p = packet::begin(Kind::Data, 0);
        packet::set_number(&mut p, 17);
        p.extend_from_slice(b"body");
        Cipher::new(&key).seal(17, &mut p, HEADER).unwrap();
        let sealed = p.clone();
        assert_eq!(open(&key, &mut p), Some(&b"body"[..]));
        assert_eq!(open(&[6; 32], &mut sealed.clone()), None);
        for i in 0..sealed.len() {
            let mut d = sealed.clone();
            d[i] ^= 0x10;
            let changed = d.clone();
            assert_eq!(open(&key, &mut d), None, "byte {i} changed");
            assert_eq!(d, changed, "a packet that fails is left as it was");
        }
    }
}
