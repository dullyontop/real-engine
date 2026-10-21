//! The packet layout (ADR-0050 §1). All integers are little-endian.
//!
//! ```text
//! header     magic "RENT" (4) | transport version u16 | kind u8 | salt u64
//! Hello      protocol u32 | game u64 | zero padding to MTU
//! Challenge  cookie u64
//! Response   cookie u64 | protocol u32 | game u64 | zero padding to MTU
//! Accept     (nothing)
//! Deny       reason u8 | server protocol u32 | server game u64
//! Data       seq u16 | ack u16 | ack_bits u32 | chunk*
//! chunk      channel u8 | message id u32 | fragment index u16 | fragment count u16 | len u16 | bytes
//! Disconnect reason length u8 | reason (UTF-8)
//! trailer    CRC-32 (IEEE) of everything before it, on every packet
//! ```
//!
//! The magic, the version and the kind come first, and the CRC last, in every
//! version of the protocol, so a Deny can always be read. The CRC drops
//! datagrams damaged on the way, which the UDP checksum is optional for on
//! IPv4 and too weak to catch reliably.

/// The four bytes every packet starts with.
pub(crate) const MAGIC: [u8; 4] = *b"RENT";

/// The version of this packet layout. A peer with another version is denied
/// (`RE8903`).
pub const TRANSPORT_VERSION: u16 = 1;

/// The largest datagram sent: below the IPv6 minimum MTU less the IP and UDP
/// headers, so no datagram is fragmented by IP.
pub const MTU: usize = 1200;

pub(crate) const HEADER: usize = 4 + 2 + 1 + 8;
pub(crate) const DATA_HEADER: usize = 2 + 2 + 4;
pub(crate) const CHUNK_HEADER: usize = 1 + 4 + 2 + 2 + 2;
pub(crate) const TRAILER: usize = 4;
/// The bytes of a packet before its trailer.
pub(crate) const BODY_MAX: usize = MTU - TRAILER;

/// The payload of one fragment of a reliable message, and the largest
/// unreliable message.
pub const FRAGMENT: usize = MTU - HEADER - DATA_HEADER - CHUNK_HEADER - TRAILER;

/// The largest reliable message.
pub const MAX_MESSAGE: usize = 1 << 20;

/// The most fragments a reliable message can have.
pub(crate) const MAX_FRAGMENTS: usize = MAX_MESSAGE.div_ceil(FRAGMENT);

/// The longest reason a Disconnect carries, in bytes.
pub(crate) const MAX_REASON: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Kind {
    Hello = 0,
    Challenge = 1,
    Response = 2,
    Accept = 3,
    Deny = 4,
    Data = 5,
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
}

/// Why a server refused a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum DenyReason {
    Version = 1,
    Full = 2,
    Game = 3,
}

impl DenyReason {
    pub(crate) fn from_u8(b: u8) -> Option<DenyReason> {
        Some(match b {
            1 => DenyReason::Version,
            2 => DenyReason::Full,
            3 => DenyReason::Game,
            _ => return None,
        })
    }
}

/// A packet's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub version: u16,
    pub kind: Kind,
    pub salt: u64,
}

/// Starts a packet with its header.
pub(crate) fn begin(kind: Kind, salt: u64) -> Vec<u8> {
    let mut p = Vec::with_capacity(MTU);
    p.extend_from_slice(&MAGIC);
    p.extend_from_slice(&TRANSPORT_VERSION.to_le_bytes());
    p.push(kind as u8);
    p.extend_from_slice(&salt.to_le_bytes());
    p
}

/// Ends a packet with its CRC.
pub(crate) fn seal(mut p: Vec<u8>) -> Vec<u8> {
    let crc = crc32(&p);
    p.extend_from_slice(&crc.to_le_bytes());
    p
}

/// A received datagram without its trailer, if the CRC matches.
pub(crate) fn open(d: &[u8]) -> Option<&[u8]> {
    let (body, crc) = d.split_at_checked(d.len().checked_sub(TRAILER)?)?;
    (crc32(body).to_le_bytes() == crc).then_some(body)
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

/// Reads a header; `None` when the packet is not ours.
pub(crate) fn header(r: &mut Reader<'_>) -> Option<Header> {
    if r.take(4)? != MAGIC {
        return None;
    }
    let version = r.u16()?;
    let kind = Kind::from_u8(r.u8()?)?;
    let salt = r.u64()?;
    Some(Header {
        version,
        kind,
        salt,
    })
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

    pub(crate) fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
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
        assert_eq!(
            HEADER + DATA_HEADER + CHUNK_HEADER + FRAGMENT + TRAILER,
            MTU
        );
        assert!(MAX_FRAGMENTS <= u16::MAX as usize);
    }

    #[test]
    fn headers_round_trip_and_reject_strangers() {
        let p = begin(Kind::Data, 0x0123_4567_89ab_cdef);
        let h = header(&mut Reader(&p)).unwrap();
        assert_eq!(h.kind, Kind::Data);
        assert_eq!(h.salt, 0x0123_4567_89ab_cdef);
        assert_eq!(h.version, TRANSPORT_VERSION);
        assert!(header(&mut Reader(b"RENX\x01\x00\x05")).is_none());
        assert!(header(&mut Reader(&p[..10])).is_none());
    }

    #[test]
    fn the_crc_is_zlibs_and_catches_damage() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let p = seal(begin(Kind::Accept, 5));
        assert_eq!(open(&p).unwrap().len(), HEADER);
        let mut bad = p.clone();
        bad[3] ^= 1;
        assert!(open(&bad).is_none());
        assert!(open(&p[..p.len() - 1]).is_none());
        assert!(open(&[]).is_none());
    }

    #[test]
    fn sequence_order_wraps() {
        assert!(seq_newer(1, 0));
        assert!(seq_newer(0, 65535));
        assert!(!seq_newer(65535, 0));
        assert!(!seq_newer(7, 7));
    }
}
