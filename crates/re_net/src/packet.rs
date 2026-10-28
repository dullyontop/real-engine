//! The packet layout of transport version 2 (ADR-0050 §1, ADR-0067 §3). All
//! integers are little-endian.
//!
//! ```text
//! handshake header  magic "RENT" (4) | transport version u16 | kind u8 | salt u64
//! Hello       protocol u32 | game u64 | zero padding to MTU                         | CRC-32
//! Challenge   cookie (16) | server key (32) | flags u8 (bit 0: a join code is needed) | CRC-32
//! Response    cookie (16) | protocol u32 | game u64 | msg1 (48) | zero padding to MTU | CRC-32
//! Accept      msg2 (48)                                                             | CRC-32
//! Deny        reason u8 | server protocol u32 | server game u64 | label length u8 | label | CRC-32
//!
//! sealed header     magic "RENT" (4) | transport version u16 | kind u8 | packet number u64
//! Data        sealed: seq u16 | ack u16 | ack_bits u32 | chunk*                    | tag (16)
//! chunk       channel u8 | message id u32 | fragment index u16 | fragment count u16 | len u16 | bytes
//! Disconnect  sealed: reason length u8 | reason (UTF-8)                            | tag (16)
//! ```
//!
//! The magic, the version and the kind come first in every version of the
//! protocol, and a handshake packet ends with the CRC-32 (IEEE) of everything
//! before it, so a Deny can always be read. The CRC drops handshake packets
//! damaged on the way, which the UDP checksum is optional for on IPv4 and too
//! weak to catch reliably. A sealed packet's body is encrypted with
//! ChaCha20-Poly1305 under the packet number, with its 15-byte header as the
//! associated data, so the tag rejects any change to any byte of it.
//!
//! [`decode`], [`data`] and [`disconnect`] read packets as pure functions of
//! their bytes; [`crate::wire`] gathers them for fuzzing.

use crate::conn::Channel;
use crate::noise::{MESSAGE, TAG};

/// The four bytes every packet starts with.
pub(crate) const MAGIC: [u8; 4] = *b"RENT";

/// The version of this packet layout. A peer with another version is denied
/// (`RE8903`).
pub const TRANSPORT_VERSION: u16 = 2;

/// The largest datagram sent: below the IPv6 minimum MTU less the IP and UDP
/// headers, so no datagram is fragmented by IP.
pub const MTU: usize = 1200;

pub(crate) const HEADER: usize = 4 + 2 + 1 + 8;
pub(crate) const DATA_HEADER: usize = 2 + 2 + 4;
pub(crate) const CHUNK_HEADER: usize = 1 + 4 + 2 + 2 + 2;
/// A handshake packet's CRC.
pub(crate) const TRAILER: usize = 4;
/// The bytes of a handshake packet before its CRC.
pub(crate) const BODY_MAX: usize = MTU - TRAILER;
/// The bytes of a sealed packet before its tag.
pub(crate) const SEALED_MAX: usize = MTU - TAG;
/// A handshake cookie: a truncated HMAC-BLAKE2s.
pub(crate) const COOKIE: usize = 16;
/// An X25519 public key.
pub(crate) const KEY: usize = 32;

/// The payload of one fragment of a reliable message, and the largest
/// unreliable message.
pub const FRAGMENT: usize = MTU - HEADER - DATA_HEADER - CHUNK_HEADER - TAG;

/// The largest reliable message.
pub const MAX_MESSAGE: usize = 1 << 20;

/// The most fragments a reliable message can have.
pub(crate) const MAX_FRAGMENTS: usize = MAX_MESSAGE.div_ceil(FRAGMENT);

/// The longest reason a Disconnect carries, in bytes.
pub(crate) const MAX_REASON: usize = 200;

/// The longest engine label a Deny carries, in bytes.
pub(crate) const MAX_LABEL: usize = 64;

/// What a packet is, from the byte after the version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// A client's first packet, padded to the MTU.
    Hello = 0,
    /// The server's answer to a Hello: a cookie and the server's key.
    Challenge = 1,
    /// The client's answer to a Challenge, padded: the cookie and the first
    /// handshake message.
    Response = 2,
    /// The server's answer to a Response: the second handshake message.
    Accept = 3,
    /// The server refuses the client.
    Deny = 4,
    /// Sealed: acks and chunks.
    Data = 5,
    /// Sealed: the sender closes the connection.
    Disconnect = 6,
}

impl Kind {
    pub(crate) fn from_u8(b: u8) -> Option<Kind> {
        Some(match b {
            0 => Kind::Hello,
            1 => Kind::Challenge,
            2 => Kind::Response,
            3 => Kind::Accept,
            4 => Kind::Deny,
            5 => Kind::Data,
            6 => Kind::Disconnect,
            _ => return None,
        })
    }

    /// True for Data and Disconnect, which are sealed; the rest are the
    /// handshake's, framed as version 1 framed every packet.
    pub fn is_sealed(self) -> bool {
        matches!(self, Kind::Data | Kind::Disconnect)
    }
}

/// Why a server refused a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum DenyReason {
    Version = 1,
    Full = 2,
    Game = 3,
    /// The first handshake message did not open: a wrong join code, or not
    /// encrypted to the server's key.
    Handshake = 4,
}

impl DenyReason {
    pub(crate) fn from_u8(b: u8) -> Option<DenyReason> {
        Some(match b {
            1 => DenyReason::Version,
            2 => DenyReason::Full,
            3 => DenyReason::Game,
            4 => DenyReason::Handshake,
            _ => return None,
        })
    }
}

/// A datagram, read from its bytes alone ([`decode`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packet<'a> {
    /// A Hello of this version that fills the MTU.
    Hello {
        /// The client's salt for this attempt.
        salt: u64,
        /// Its protocol version.
        protocol: u32,
        /// Its game hash.
        game: u64,
    },
    /// A Challenge.
    Challenge {
        /// The client's salt.
        salt: u64,
        /// The cookie the Response must carry back.
        cookie: [u8; COOKIE],
        /// The server's static public key.
        server_key: [u8; KEY],
        /// Flag bit 0: the server takes only clients with its join code.
        code_required: bool,
    },
    /// A Response that fills the MTU.
    Response {
        /// The client's salt.
        salt: u64,
        /// The Challenge's cookie.
        cookie: [u8; COOKIE],
        /// Its protocol version.
        protocol: u32,
        /// Its game hash.
        game: u64,
        /// The first handshake message (`-> psk, e, es`).
        msg1: &'a [u8],
    },
    /// An Accept.
    Accept {
        /// The client's salt.
        salt: u64,
        /// The second handshake message (`<- e, ee`).
        msg2: &'a [u8],
    },
    /// A Deny of any transport version: each starts as version 1's did, and
    /// a version 1 Deny has no label.
    Deny {
        /// The server's transport version.
        version: u16,
        /// The client's salt.
        salt: u64,
        /// Why, when it says.
        reason: Option<u8>,
        /// The server's protocol version.
        protocol: Option<u32>,
        /// The server's game hash.
        game: Option<u64>,
        /// The server's engine, as it names itself.
        label: Option<&'a [u8]>,
    },
    /// A sealed packet of this version (Data or Disconnect), not yet opened:
    /// its tag is checked when it opens.
    Sealed {
        /// Data or Disconnect.
        kind: Kind,
        /// The sender's packet number, the nonce it was sealed under.
        number: u64,
    },
    /// A Hello of another transport version that fills the MTU, which a
    /// server answers with a Deny (`RE8903` on that client).
    OtherHello {
        /// The client's transport version.
        version: u16,
        /// The client's salt.
        salt: u64,
    },
    /// Not a packet of ours, damaged (a CRC that does not match), too short,
    /// of an unknown kind, or another version's packet that needs no answer.
    Stray,
}

/// A packet's header: its version and kind, and its salt (handshake packets)
/// or packet number (sealed ones).
struct Header {
    version: u16,
    kind: Kind,
    word: u64,
}

/// Reads a header; `None` when the bytes are not one of ours.
fn header(r: &mut Reader<'_>) -> Option<Header> {
    if r.take(4)? != MAGIC {
        return None;
    }
    let version = r.u16()?;
    let kind = Kind::from_u8(r.u8()?)?;
    let word = r.u64()?;
    Some(Header {
        version,
        kind,
        word,
    })
}

/// Reads a datagram: what it is and its fields, with the CRC of a handshake
/// packet checked. A sealed packet is only classified here; it opens with
/// its connection's key.
pub fn decode(datagram: &[u8]) -> Packet<'_> {
    let Some(h) = header(&mut Reader(datagram)) else {
        return Packet::Stray;
    };
    if h.version == TRANSPORT_VERSION && h.kind.is_sealed() {
        return if datagram.len() >= HEADER + TAG {
            Packet::Sealed {
                kind: h.kind,
                number: h.word,
            }
        } else {
            Packet::Stray
        };
    }
    let Some(body) = open_crc(datagram) else {
        return Packet::Stray;
    };
    // Hello and Response are padded, so a forged source address never gets
    // back more than it sent.
    let padded = datagram.len() >= MTU;
    let mut r = Reader(&body[HEADER..]);
    let salt = h.word;
    if h.version != TRANSPORT_VERSION {
        return match h.kind {
            Kind::Deny => deny(h.version, salt, &mut r),
            Kind::Hello if padded => Packet::OtherHello {
                version: h.version,
                salt,
            },
            _ => Packet::Stray,
        };
    }
    let packet = match h.kind {
        Kind::Hello if padded => (|| {
            Some(Packet::Hello {
                salt,
                protocol: r.u32()?,
                game: r.u64()?,
            })
        })(),
        Kind::Challenge => (|| {
            Some(Packet::Challenge {
                salt,
                cookie: r.array()?,
                server_key: r.array()?,
                code_required: r.u8()? & 1 != 0,
            })
        })(),
        Kind::Response if padded => (|| {
            Some(Packet::Response {
                salt,
                cookie: r.array()?,
                protocol: r.u32()?,
                game: r.u64()?,
                msg1: r.take(MESSAGE)?,
            })
        })(),
        Kind::Accept => r.take(MESSAGE).map(|msg2| Packet::Accept { salt, msg2 }),
        Kind::Deny => Some(deny(h.version, salt, &mut r)),
        _ => None,
    };
    packet.unwrap_or(Packet::Stray)
}

fn deny<'a>(version: u16, salt: u64, r: &mut Reader<'a>) -> Packet<'a> {
    Packet::Deny {
        version,
        salt,
        reason: r.u8(),
        protocol: r.u32(),
        game: r.u64(),
        label: r.u8().and_then(|n| r.take(n.into())),
    }
}

/// A Data packet's body, opened ([`data`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataBody<'a> {
    /// The sender's sequence number for acks.
    pub seq: u16,
    /// The newest sequence number the sender received.
    pub ack: u16,
    /// Which of the 32 before `ack` it received too.
    pub ack_bits: u32,
    /// The messages and fragments.
    pub chunks: Vec<Chunk<'a>>,
}

/// A chunk of a Data packet: an unreliable message, or one fragment of a
/// reliable one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk<'a> {
    /// Its channel.
    pub channel: Channel,
    /// A reliable message's number (0 for an unreliable one).
    pub id: u32,
    /// Which fragment.
    pub index: u16,
    /// Of how many.
    pub count: u16,
    /// Its bytes.
    pub bytes: &'a [u8],
}

/// Reads an opened Data packet's body (after the header): `None` when any
/// part of it is malformed, so a packet is taken whole or not at all. Every
/// reliable fragment but a message's last is full, so a message's fragments
/// cannot be made to cost more than its bytes.
pub fn data(body: &[u8]) -> Option<DataBody<'_>> {
    let mut r = Reader(body);
    let seq = r.u16()?;
    let ack = r.u16()?;
    let ack_bits = r.u32()?;
    let mut chunks = Vec::new();
    while !r.is_empty() {
        let channel = match r.u8()? {
            0 => Channel::Unreliable,
            1 => Channel::Reliable,
            _ => return None,
        };
        let id = r.u32()?;
        let index = r.u16()?;
        let count = r.u16()?;
        let len = r.u16()? as usize;
        let bytes = r.take(len)?;
        if channel == Channel::Reliable
            && (count == 0
                || index >= count
                || count as usize > MAX_FRAGMENTS
                || len > FRAGMENT
                || (index + 1 < count && len != FRAGMENT))
        {
            return None;
        }
        chunks.push(Chunk {
            channel,
            id,
            index,
            count,
            bytes,
        });
    }
    Some(DataBody {
        seq,
        ack,
        ack_bits,
        chunks,
    })
}

/// Reads an opened Disconnect's body: the reason, empty when there is none
/// or it does not parse, as [`text`] shows it.
pub fn disconnect(body: &[u8]) -> String {
    let mut r = Reader(body);
    r.u8()
        .and_then(|n| r.take(n.into()))
        .map(text)
        .unwrap_or_default()
}

/// Text a peer sent (a Disconnect's reason, a Deny's engine label) for a
/// diagnostic's one line: line breaks and tabs become spaces, and invalid
/// UTF-8 and other control characters U+FFFD, so a peer cannot write escape
/// sequences to a terminal.
pub(crate) fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            c if c.is_control() => '\u{fffd}',
            c => c,
        })
        .collect()
}

/// Starts a packet with its header; `word` is a handshake packet's salt, and
/// a sealed packet's number is written when it is sealed.
pub(crate) fn begin(kind: Kind, word: u64) -> Vec<u8> {
    let mut p = Vec::with_capacity(MTU);
    p.extend_from_slice(&MAGIC);
    p.extend_from_slice(&TRANSPORT_VERSION.to_le_bytes());
    p.push(kind as u8);
    p.extend_from_slice(&word.to_le_bytes());
    p
}

/// Writes a sealed packet's number into its header.
pub(crate) fn set_number(p: &mut [u8], number: u64) {
    p[HEADER - 8..HEADER].copy_from_slice(&number.to_le_bytes());
}

/// Ends a handshake packet with its CRC.
fn with_crc(mut p: Vec<u8>) -> Vec<u8> {
    let crc = crc32(&p);
    p.extend_from_slice(&crc.to_le_bytes());
    p
}

/// A handshake packet without its CRC, if the CRC matches.
fn open_crc(d: &[u8]) -> Option<&[u8]> {
    let (body, crc) = d.split_at_checked(d.len().checked_sub(TRAILER)?)?;
    (crc32(body).to_le_bytes() == crc).then_some(body)
}

/// A Hello, padded to the MTU.
pub(crate) fn hello(salt: u64, protocol: u32, game: u64) -> Vec<u8> {
    let mut p = begin(Kind::Hello, salt);
    p.extend_from_slice(&protocol.to_le_bytes());
    p.extend_from_slice(&game.to_le_bytes());
    p.resize(BODY_MAX, 0);
    with_crc(p)
}

/// A Challenge.
pub(crate) fn challenge(
    salt: u64,
    cookie: &[u8; COOKIE],
    server_key: &[u8; KEY],
    code_required: bool,
) -> Vec<u8> {
    let mut p = begin(Kind::Challenge, salt);
    p.extend_from_slice(cookie);
    p.extend_from_slice(server_key);
    p.push(u8::from(code_required));
    with_crc(p)
}

/// A Response, padded to the MTU.
pub(crate) fn response(
    salt: u64,
    cookie: &[u8; COOKIE],
    protocol: u32,
    game: u64,
    msg1: &[u8],
) -> Vec<u8> {
    debug_assert_eq!(msg1.len(), MESSAGE);
    let mut p = begin(Kind::Response, salt);
    p.extend_from_slice(cookie);
    p.extend_from_slice(&protocol.to_le_bytes());
    p.extend_from_slice(&game.to_le_bytes());
    p.extend_from_slice(msg1);
    p.resize(BODY_MAX, 0);
    with_crc(p)
}

/// An Accept.
pub(crate) fn accept(salt: u64, msg2: &[u8]) -> Vec<u8> {
    debug_assert_eq!(msg2.len(), MESSAGE);
    let mut p = begin(Kind::Accept, salt);
    p.extend_from_slice(msg2);
    with_crc(p)
}

/// A Deny, with the server's engine label cut to [`MAX_LABEL`] bytes.
pub(crate) fn deny_packet(
    salt: u64,
    reason: DenyReason,
    protocol: u32,
    game: u64,
    label: &str,
) -> Vec<u8> {
    let mut p = begin(Kind::Deny, salt);
    p.push(reason as u8);
    p.extend_from_slice(&protocol.to_le_bytes());
    p.extend_from_slice(&game.to_le_bytes());
    let label = cut(label, MAX_LABEL);
    p.push(label.len() as u8);
    p.extend_from_slice(label.as_bytes());
    with_crc(p)
}

/// `s` cut to at most `max` bytes, at a character boundary.
pub(crate) fn cut(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// CRC-32 (IEEE 802.3, reflected, as zlib computes it).
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// Appends one chunk.
pub(crate) fn chunk(p: &mut Vec<u8>, channel: u8, id: u32, index: u16, count: u16, data: &[u8]) {
    debug_assert!(data.len() <= FRAGMENT);
    p.push(channel);
    p.extend_from_slice(&id.to_le_bytes());
    p.extend_from_slice(&index.to_le_bytes());
    p.extend_from_slice(&count.to_le_bytes());
    p.extend_from_slice(&(data.len() as u16).to_le_bytes());
    p.extend_from_slice(data);
}

/// Reads little-endian integers off the front of a packet. Every read checks
/// the length, so a truncated packet reads as `None`.
pub(crate) struct Reader<'a>(pub &'a [u8]);

impl<'a> Reader<'a> {
    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.array()?))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// True when sequence number `a` is newer than `b`, with wrap-around.
pub(crate) fn seq_newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_fragment_fills_a_datagram_exactly() {
        assert_eq!(HEADER + DATA_HEADER + CHUNK_HEADER + FRAGMENT + TAG, MTU);
        assert_eq!(FRAGMENT, 1150);
        assert!(MAX_FRAGMENTS <= u16::MAX as usize);
    }

    #[test]
    fn handshake_packets_have_the_sizes_of_the_adr() {
        let (cookie, key) = ([3; COOKIE], [4; KEY]);
        assert_eq!(hello(1, 2, 3).len(), MTU);
        assert_eq!(challenge(1, &cookie, &key, true).len(), 68);
        assert_eq!(response(1, &cookie, 2, 3, &[5; MESSAGE]).len(), MTU);
        assert_eq!(accept(1, &[6; MESSAGE]).len(), 67);
        let label = "x".repeat(100);
        let d = deny_packet(1, DenyReason::Game, 2, 3, &label);
        assert_eq!(d.len(), HEADER + 1 + 4 + 8 + 1 + MAX_LABEL + TRAILER);
        // Smaller than what it answers: a forged source gets no amplification.
        assert!(d.len() < MTU / 10);
    }

    #[test]
    fn every_handshake_packet_reads_back() {
        let (cookie, key) = ([3; COOKIE], [4; KEY]);
        assert_eq!(
            decode(&hello(9, 2, 3)),
            Packet::Hello {
                salt: 9,
                protocol: 2,
                game: 3
            }
        );
        assert_eq!(
            decode(&challenge(9, &cookie, &key, true)),
            Packet::Challenge {
                salt: 9,
                cookie,
                server_key: key,
                code_required: true
            }
        );
        let msg1 = [5; MESSAGE];
        assert_eq!(
            decode(&response(9, &cookie, 2, 3, &msg1)),
            Packet::Response {
                salt: 9,
                cookie,
                protocol: 2,
                game: 3,
                msg1: &msg1
            }
        );
        let msg2 = [6; MESSAGE];
        assert_eq!(
            decode(&accept(9, &msg2)),
            Packet::Accept {
                salt: 9,
                msg2: &msg2
            }
        );
        assert_eq!(
            decode(&deny_packet(9, DenyReason::Handshake, 2, 3, "0.4.0 ü")),
            Packet::Deny {
                version: TRANSPORT_VERSION,
                salt: 9,
                reason: Some(4),
                protocol: Some(2),
                game: Some(3),
                label: Some("0.4.0 ü".as_bytes())
            }
        );
    }

    #[test]
    fn a_label_is_cut_at_a_character() {
        let label = "é".repeat(40);
        let p = deny_packet(1, DenyReason::Full, 2, 3, &label);
        let Packet::Deny {
            label: Some(got), ..
        } = decode(&p)
        else {
            panic!()
        };
        assert_eq!(got.len(), 64);
        assert!(std::str::from_utf8(got).is_ok());
    }

    /// Version 1's packets, as its builds send them.
    fn v1(kind: Kind, salt: u64, body: &[u8], pad: bool) -> Vec<u8> {
        let mut p = b"RENT".to_vec();
        p.extend_from_slice(&1u16.to_le_bytes());
        p.push(kind as u8);
        p.extend_from_slice(&salt.to_le_bytes());
        p.extend_from_slice(body);
        if pad {
            p.resize(BODY_MAX, 0);
        }
        with_crc(p)
    }

    #[test]
    fn version_1_is_read_only_for_its_deny_and_its_hello() {
        let mut hello_body = 2u32.to_le_bytes().to_vec();
        hello_body.extend_from_slice(&3u64.to_le_bytes());
        assert_eq!(
            decode(&v1(Kind::Hello, 5, &hello_body, true)),
            Packet::OtherHello {
                version: 1,
                salt: 5
            }
        );
        assert_eq!(
            decode(&v1(Kind::Hello, 5, &hello_body, false)),
            Packet::Stray
        );
        let mut deny_body = vec![1];
        deny_body.extend_from_slice(&2u32.to_le_bytes());
        deny_body.extend_from_slice(&3u64.to_le_bytes());
        assert_eq!(
            decode(&v1(Kind::Deny, 5, &deny_body, false)),
            Packet::Deny {
                version: 1,
                salt: 5,
                reason: Some(1),
                protocol: Some(2),
                game: Some(3),
                label: None
            }
        );
        for kind in [
            Kind::Challenge,
            Kind::Response,
            Kind::Accept,
            Kind::Data,
            Kind::Disconnect,
        ] {
            assert_eq!(
                decode(&v1(kind, 5, &[0; 64], true)),
                Packet::Stray,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_version_1_client_reads_this_deny() {
        // As version 1's code reads it: CRC over all but the last 4 bytes,
        // then reason, protocol and game; the label after them is ignored.
        let d = deny_packet(5, DenyReason::Version, 2, 3, "0.4.0");
        let body = &d[..d.len() - 4];
        assert_eq!(crc32(body).to_le_bytes(), d[d.len() - 4..]);
        assert_eq!(&body[4..6], &2u16.to_le_bytes(), "the version it reports");
        assert_eq!(body[6], Kind::Deny as u8);
        assert_eq!(body[15], DenyReason::Version as u8);
        assert_eq!(&body[16..20], &2u32.to_le_bytes());
        assert_eq!(&body[20..28], &3u64.to_le_bytes());
    }

    #[test]
    fn sealed_packets_are_classified_by_their_header_alone() {
        let mut p = begin(Kind::Data, 0);
        set_number(&mut p, 77);
        p.extend_from_slice(&[0; TAG]);
        assert_eq!(
            decode(&p),
            Packet::Sealed {
                kind: Kind::Data,
                number: 77
            }
        );
        assert_eq!(
            decode(&p[..HEADER + TAG - 1]),
            Packet::Stray,
            "no room for a tag"
        );
        p[6] = Kind::Disconnect as u8;
        assert_eq!(
            decode(&p),
            Packet::Sealed {
                kind: Kind::Disconnect,
                number: 77
            }
        );
    }

    #[test]
    fn damage_and_strangers_read_as_stray() {
        let c = challenge(9, &[3; COOKIE], &[4; KEY], false);
        for i in 0..c.len() {
            for bit in 0..8 {
                let mut d = c.clone();
                d[i] ^= 1 << bit;
                // A kind changed to a sealed one reads as a sealed packet,
                // which then fails its tag where it is opened.
                let expected = match d[6] {
                    5 if i == 6 => Packet::Sealed {
                        kind: Kind::Data,
                        number: 9,
                    },
                    6 if i == 6 => Packet::Sealed {
                        kind: Kind::Disconnect,
                        number: 9,
                    },
                    _ => Packet::Stray,
                };
                assert_eq!(decode(&d), expected, "byte {i} bit {bit}");
            }
        }
        for n in 0..c.len() {
            assert_eq!(decode(&c[..n]), Packet::Stray, "{n} bytes");
        }
        assert_eq!(decode(b""), Packet::Stray);
        assert_eq!(decode(b"RENX\x02\x00\x05"), Packet::Stray);
        let mut unknown = begin(Kind::Hello, 1);
        unknown[6] = 7;
        assert_eq!(decode(&with_crc(unknown)), Packet::Stray);
        // A Response or Hello that does not fill the MTU gets no answer.
        let mut short = response(1, &[3; COOKIE], 2, 3, &[5; MESSAGE]);
        short.truncate(BODY_MAX - 1);
        assert_eq!(decode(&with_crc(short)), Packet::Stray);
    }

    #[test]
    fn the_crc_is_zlibs() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn data_bodies_read_whole_or_not_at_all() {
        let mut p = vec![1, 0, 2, 0, 4, 0, 0, 0];
        chunk(&mut p, 0, 0, 0, 1, b"u");
        chunk(&mut p, 1, 7, 0, 1, b"r");
        let body = data(&p).unwrap();
        assert_eq!((body.seq, body.ack, body.ack_bits), (1, 2, 4));
        assert_eq!(body.chunks.len(), 2);
        assert_eq!(body.chunks[1].channel, Channel::Reliable);
        assert_eq!(body.chunks[1].bytes, b"r");
        p.push(9);
        assert_eq!(data(&p), None, "a truncated chunk on another channel");
        p.pop();
        // A reliable fragment that is not its message's last must be full.
        let mut q = vec![0; DATA_HEADER];
        chunk(&mut q, 1, 7, 0, 2, b"short");
        assert_eq!(data(&q), None);
        assert_eq!(data(&[0; DATA_HEADER - 1]), None);
        assert_eq!(data(&[0; DATA_HEADER]).unwrap().chunks, vec![]);
    }

    #[test]
    fn a_disconnect_reason_reads_back() {
        assert_eq!(disconnect(b"\x03bye"), "bye");
        assert_eq!(disconnect(b"\x09bye"), "");
        assert_eq!(disconnect(b""), "");
        assert_eq!(disconnect(b"\x02\xff\xfe"), "\u{fffd}\u{fffd}");
        assert_eq!(disconnect(b"\x0b\x1b[2Jbye\nnow"), "\u{fffd}[2Jbye now");
    }

    #[test]
    fn sequence_order_wraps() {
        assert!(seq_newer(1, 0));
        assert!(seq_newer(0, 65535));
        assert!(!seq_newer(65535, 0));
        assert!(!seq_newer(7, 7));
    }
}
