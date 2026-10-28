//! One connection: its session keys and replay window (ADR-0067 §5), and its
//! reliability: acks, round-trip time, the reliable ordered channel's
//! fragments and resends, the unreliable queue and the token bucket
//! (ADR-0050 §1).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

use crate::noise::Session;
use crate::packet::{
    self, CHUNK_HEADER, FRAGMENT, HEADER, Kind, MAX_MESSAGE, MAX_REASON, MTU, SEALED_MAX, seq_newer,
};
use crate::replay::ReplayWindow;

/// How far past the oldest unacked reliable message a sender may go, and how
/// far ahead of the next expected one a receiver keeps fragments.
const WINDOW: u32 = 1024;
/// The most reliable bytes a receiver holds waiting for earlier fragments
/// (each fragment counts [`FRAGMENT_COST`] more). A packet whose fragments
/// would pass it is not acked, so they all come again; the next expected
/// message's fragments always fit.
const MAX_BUFFERED: usize = 8 * MAX_MESSAGE;
/// What a buffered fragment costs beyond its bytes (its map entry).
pub(crate) const FRAGMENT_COST: usize = 64;
/// The most unreliable messages queued; the oldest are dropped past it.
const MAX_UNRELIABLE: usize = 1024;
/// Packets with chunks received without an ack going back: at this many the
/// receiver acks at once, because one ack covers only 33 packets.
const ACK_EVERY: u32 = 32;
/// The smallest backlog window ([`Conn::window`]).
const MIN_WINDOW: usize = 64 * 1024;
/// Sent packets remembered for acks; older ones count as lost.
const MAX_SENT: usize = 2048;

/// A message channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    /// One datagram, at most [`FRAGMENT`](crate::FRAGMENT) bytes; lost when
    /// the datagram is lost, and delivered in arrival order.
    Unreliable,
    /// Up to [`MAX_MESSAGE`](crate::MAX_MESSAGE) bytes; resent until acked
    /// and delivered in the order sent.
    Reliable,
}

impl Channel {
    fn id(self) -> u8 {
        match self {
            Channel::Unreliable => 0,
            Channel::Reliable => 1,
        }
    }
}

/// Counts for one connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnStats {
    /// Datagrams sent.
    pub packets_sent: u64,
    /// Datagrams received and accepted.
    pub packets_received: u64,
    /// Bytes of datagrams sent.
    pub bytes_sent: u64,
    /// Bytes of datagrams received.
    pub bytes_received: u64,
    /// Reliable fragments sent, resends included.
    pub fragments_sent: u64,
    /// Reliable fragments sent again after their timeout.
    pub fragments_resent: u64,
    /// Datagrams from this peer's address that did not parse and were
    /// dropped: not a packet of ours, or damaged handshake packets, or a
    /// sealed packet that opened but is malformed.
    pub malformed: u64,
    /// Sealed packets dropped because they did not open: altered on the way,
    /// forged, or sealed with other keys (ADR-0067 §5).
    pub rejected: u64,
    /// Sealed packets dropped before opening because their number had opened
    /// already or was too old for the replay window: a duplicate from the
    /// network, or a replay.
    pub replayed: u64,
    /// Messages delivered to the caller.
    pub messages_received: u64,
    /// Packets not acked because their fragments did not fit the receive
    /// buffer; the sender sends them again.
    pub packets_deferred: u64,
    /// Unreliable messages dropped: from a full send queue, or received
    /// in a deferred packet.
    pub unreliable_dropped: u64,
}

struct OutFragment {
    data: Vec<u8>,
    acked: bool,
    last_sent: Option<Duration>,
}

struct OutMessage {
    fragments: Vec<OutFragment>,
    left: usize,
}

struct InMessage {
    count: u16,
    fragments: HashMap<u16, Vec<u8>>,
}

struct SentPacket {
    at: Duration,
    fragments: Vec<(u32, u16)>,
}

/// What [`Conn::receive`] found in one data packet.
pub(crate) enum Received {
    /// Delivered messages, in order.
    Messages(Vec<(Channel, Vec<u8>)>),
    /// The packet did not parse.
    Malformed,
}

pub(crate) struct Conn {
    /// The client's salt for the handshake that made it.
    pub(crate) salt: u64,
    /// The session's ciphers, boxed so the keys stay put when the
    /// connection moves; they wipe themselves when it drops.
    keys: Box<Session>,
    /// The number of the next sealed packet sent, which is its nonce.
    next_number: u64,
    /// The numbers of the peer's sealed packets that opened.
    window: ReplayWindow,
    local_seq: u16,
    remote_seq: u16,
    recv_bits: u32,
    any_received: bool,
    sent: HashMap<u16, SentPacket>,
    srtt: Option<f64>,
    rttvar: f64,
    next_out: u32,
    out: BTreeMap<u32, OutMessage>,
    backlog: usize,
    next_in: u32,
    inbox: BTreeMap<u32, InMessage>,
    buffered: usize,
    unreliable: VecDeque<Vec<u8>>,
    pub(crate) last_recv: Duration,
    last_send: Duration,
    need_ack: bool,
    unacked_received: u32,
    /// Since when sent reliable fragments have waited with no ack progress.
    waiting_since: Option<Duration>,
    /// Reliable fragments sent at least once and not yet acked.
    in_flight: usize,
    tokens: f64,
    last_refill: Duration,
    pub(crate) rate: f64,
    rto_min: Duration,
    keepalive: Duration,
    pub(crate) stats: ConnStats,
}

impl Conn {
    pub(crate) fn new(
        salt: u64,
        keys: Session,
        now: Duration,
        rate: f64,
        rto_min: Duration,
        keepalive: Duration,
    ) -> Conn {
        // A rate that is not a positive number would never send.
        let rate = rate.max(1024.0);
        Conn {
            salt,
            keys: Box::new(keys),
            next_number: 0,
            window: ReplayWindow::new(),
            local_seq: 0,
            // Until the peer's first packet arrives, acks name a sequence
            // number the peer has not used (it starts at 0), so they ack
            // nothing.
            remote_seq: u16::MAX,
            recv_bits: 0,
            any_received: false,
            sent: HashMap::new(),
            srtt: None,
            rttvar: 0.0,
            next_out: 0,
            out: BTreeMap::new(),
            backlog: 0,
            next_in: 0,
            inbox: BTreeMap::new(),
            buffered: 0,
            unreliable: VecDeque::new(),
            last_recv: now,
            last_send: now,
            need_ack: false,
            unacked_received: 0,
            waiting_since: None,
            in_flight: 0,
            tokens: burst(rate),
            last_refill: now,
            rate,
            rto_min,
            keepalive: keepalive.max(Duration::from_millis(1)),
            stats: ConnStats::default(),
        }
    }

    /// The smoothed round-trip time, once a packet has been acked.
    pub(crate) fn rtt(&self) -> Option<Duration> {
        self.srtt.and_then(|s| Duration::try_from_secs_f64(s).ok())
    }

    /// Reliable bytes queued and not yet acked.
    pub(crate) fn backlog(&self) -> usize {
        self.backlog
    }

    /// The backlog with a fixed cost per queued message, so empty and tiny
    /// messages still count against a limit.
    pub(crate) fn queued_cost(&self) -> usize {
        self.backlog + self.out.len() * FRAGMENT_COST
    }

    /// The backlog a sender should stay under: the bandwidth-delay product,
    /// `max(64 KiB, 2 × rate × srtt)`.
    pub(crate) fn window(&self) -> usize {
        let bdp = 2.0 * self.rate * self.srtt.unwrap_or(0.0);
        (bdp as usize).max(MIN_WINDOW)
    }

    fn rto(&self) -> Duration {
        let rto = match self.srtt {
            None => Duration::from_millis(200),
            Some(s) => Duration::try_from_secs_f64(s + 4.0 * self.rttvar).unwrap_or(Duration::MAX),
        };
        rto.max(self.rto_min)
            .min(Duration::from_secs(1).max(self.rto_min))
    }

    /// Queues a message. The caller checked its size.
    pub(crate) fn queue(&mut self, channel: Channel, bytes: &[u8]) {
        match channel {
            Channel::Unreliable => {
                if self.unreliable.len() >= MAX_UNRELIABLE {
                    self.unreliable.pop_front();
                    self.stats.unreliable_dropped += 1;
                }
                self.unreliable.push_back(bytes.to_vec());
            }
            Channel::Reliable => {
                let fragments: Vec<OutFragment> = if bytes.is_empty() {
                    vec![OutFragment {
                        data: Vec::new(),
                        acked: false,
                        last_sent: None,
                    }]
                } else {
                    bytes
                        .chunks(FRAGMENT)
                        .map(|c| OutFragment {
                            data: c.to_vec(),
                            acked: false,
                            last_sent: None,
                        })
                        .collect()
                };
                self.backlog += bytes.len();
                let left = fragments.len();
                self.out
                    .insert(self.next_out, OutMessage { fragments, left });
                self.next_out += 1;
            }
        }
    }

    /// Reads one data packet's body (after the header).
    pub(crate) fn receive(&mut self, now: Duration, body: &[u8]) -> Received {
        match self.parse(now, body) {
            Some(m) => {
                self.stats.messages_received += m.len() as u64;
                Received::Messages(m)
            }
            None => {
                self.stats.malformed += 1;
                Received::Malformed
            }
        }
    }

    fn parse(&mut self, now: Duration, body: &[u8]) -> Option<Vec<(Channel, Vec<u8>)>> {
        // Every chunk is checked before any is used, so a malformed packet
        // changes nothing.
        let packet::DataBody {
            seq,
            ack,
            ack_bits,
            chunks,
        } = packet::data(body)?;
        let chunks: Vec<_> = chunks
            .into_iter()
            .map(|c| (c.channel, c.id, c.index, c.count, c.bytes))
            .collect();
        self.last_recv = now;
        self.note_acks(now, ack, ack_bits);
        // Fragments that would overflow the receive buffer: the packet is
        // not acked, so the sender sends all of it again later.
        let cost: usize = chunks
            .iter()
            .filter(|c| c.0 == Channel::Reliable && self.would_buffer(c.1, c.2))
            .map(|c| c.4.len() + FRAGMENT_COST)
            .sum();
        let mut delivered = Vec::new();
        if self.buffered + cost > MAX_BUFFERED {
            // Fragments that need no room are still taken: the next expected
            // message may complete and drain the buffer, which a resend of
            // this same mix could otherwise never do.
            for &(channel, id, index, count, bytes) in &chunks {
                if channel == Channel::Reliable && !self.would_buffer(id, index) {
                    self.take_fragment(id, index, count, bytes, &mut delivered);
                }
            }
            let cost: usize = chunks
                .iter()
                .filter(|c| c.0 == Channel::Reliable && self.would_buffer(c.1, c.2))
                .map(|c| c.4.len() + FRAGMENT_COST)
                .sum();
            if self.buffered + cost > MAX_BUFFERED {
                self.stats.packets_deferred += 1;
                self.stats.unreliable_dropped +=
                    chunks.iter().filter(|c| c.0 == Channel::Unreliable).count() as u64;
                return Some(delivered);
            }
        }
        if !self.note_received(seq) {
            // A duplicate: its reliable fragments were taken already, and its
            // unreliable messages must not arrive twice.
            if !chunks.is_empty() {
                self.need_ack = true;
            }
            return Some(delivered);
        }
        if !chunks.is_empty() {
            self.need_ack = true;
            self.unacked_received += 1;
        }
        for (channel, id, index, count, bytes) in chunks {
            match channel {
                Channel::Unreliable => delivered.push((Channel::Unreliable, bytes.to_vec())),
                Channel::Reliable => self.take_fragment(id, index, count, bytes, &mut delivered),
            }
        }
        Some(delivered)
    }

    /// True when a fragment would wait in the buffer: it is in the window,
    /// not of the next expected message (whose fragments always fit) and new.
    fn would_buffer(&self, id: u32, index: u16) -> bool {
        id > self.next_in
            && id - self.next_in < WINDOW
            && !self
                .inbox
                .get(&id)
                .is_some_and(|m| m.fragments.contains_key(&index))
    }

    /// Records a received sequence number; false when it was received before.
    fn note_received(&mut self, seq: u16) -> bool {
        if !self.any_received {
            self.any_received = true;
            self.remote_seq = seq;
            self.recv_bits = 0;
            true
        } else if seq_newer(seq, self.remote_seq) {
            let shift = seq.wrapping_sub(self.remote_seq) as u32;
            self.recv_bits = if shift > 32 {
                0
            } else {
                let moved = if shift == 32 {
                    0
                } else {
                    self.recv_bits << shift
                };
                moved | (1 << (shift - 1))
            };
            self.remote_seq = seq;
            true
        } else {
            let back = self.remote_seq.wrapping_sub(seq) as u32;
            match back {
                0 => false,
                1..=32 => {
                    let bit = 1 << (back - 1);
                    let new = self.recv_bits & bit == 0;
                    self.recv_bits |= bit;
                    new
                }
                // Too old to tell; reliable fragments are filtered by id.
                _ => true,
            }
        }
    }

    /// True when so many packets arrived unacked that an ack must go now
    /// ([`Conn::ack`]), before the next flush.
    pub(crate) fn ack_due(&self) -> bool {
        self.unacked_received >= ACK_EVERY
    }

    /// An ack-only packet, sent at once.
    pub(crate) fn ack(&mut self, now: Duration) -> Option<Vec<u8>> {
        let p = self.data_packet(now, Vec::new(), packet::begin(Kind::Data, 0))?;
        self.stats.packets_sent += 1;
        self.stats.bytes_sent += p.len() as u64;
        Some(p)
    }

    /// Opens a sealed packet from the peer in place (ADR-0067 §5): a number
    /// that opened before or is too old for the window is dropped before any
    /// decryption (`replayed`), and one whose tag fails is dropped
    /// (`rejected`) without moving the window. Returns the body's length; the
    /// body is `datagram[HEADER..HEADER + len]`.
    pub(crate) fn open(&mut self, number: u64, datagram: &mut [u8]) -> Option<usize> {
        if !self.window.fresh(number) {
            self.stats.replayed += 1;
            return None;
        }
        match self.keys.recv.open(number, datagram, HEADER) {
            Ok(len) => {
                self.window.mark(number);
                Some(len)
            }
            Err(_) => {
                self.stats.rejected += 1;
                None
            }
        }
    }

    /// Seals a packet [`packet::begin`] started: numbers it with the next
    /// packet number and encrypts its body with its header as the associated
    /// data. `None` once the numbers ran out ([`Conn::spent`]).
    fn seal(&mut self, mut p: Vec<u8>) -> Option<Vec<u8>> {
        let number = self.next_number;
        packet::set_number(&mut p, number);
        self.keys.send.seal(number, &mut p, HEADER).ok()?;
        self.next_number += 1;
        Some(p)
    }

    /// True when the connection has sealed 2^64 − 1 packets and can seal no
    /// more, which Noise's reserved nonce forbids (it cannot happen: a million
    /// packets a second for half a million years).
    pub(crate) fn spent(&self) -> bool {
        self.next_number == u64::MAX
    }

    /// A Disconnect with `reason` (at most 200 bytes of it).
    pub(crate) fn disconnect(&mut self, reason: &str) -> Option<Vec<u8>> {
        let reason = packet::cut(reason, MAX_REASON);
        let mut p = packet::begin(Kind::Disconnect, 0);
        p.push(reason.len() as u8);
        p.extend_from_slice(reason.as_bytes());
        let p = self.seal(p)?;
        self.stats.packets_sent += 1;
        self.stats.bytes_sent += p.len() as u64;
        Some(p)
    }

    /// True when sent reliable fragments have had no ack progress for
    /// `timeout`: the peer stopped acknowledging.
    pub(crate) fn stalled(&mut self, now: Duration, timeout: Duration) -> bool {
        let Some(since) = self.waiting_since else {
            return false;
        };
        if now.saturating_sub(since) <= timeout {
            return false;
        }
        self.in_flight > 0
    }

    fn note_acks(&mut self, now: Duration, ack: u16, bits: u32) {
        for i in 0..=32u16 {
            if i > 0 && bits & (1 << (i - 1)) == 0 {
                continue;
            }
            let Some(p) = self.sent.remove(&ack.wrapping_sub(i)) else {
                continue;
            };
            let sample = now.saturating_sub(p.at).as_secs_f64();
            match self.srtt {
                None => {
                    self.srtt = Some(sample);
                    self.rttvar = sample / 2.0;
                }
                Some(s) => {
                    self.rttvar = 0.75 * self.rttvar + 0.25 * (s - sample).abs();
                    self.srtt = Some(0.875 * s + 0.125 * sample);
                }
            }
            for (id, f) in p.fragments {
                let Some(m) = self.out.get_mut(&id) else {
                    continue;
                };
                let frag = &mut m.fragments[f as usize];
                if !frag.acked {
                    frag.acked = true;
                    m.left -= 1;
                    self.backlog -= frag.data.len();
                    self.in_flight -= 1;
                    // Progress; with nothing left in flight there is nothing
                    // to wait for until the next send.
                    self.waiting_since = (self.in_flight > 0).then_some(now);
                }
                if m.left == 0 {
                    self.out.remove(&id);
                }
            }
        }
    }

    fn take_fragment(
        &mut self,
        id: u32,
        index: u16,
        count: u16,
        bytes: &[u8],
        delivered: &mut Vec<(Channel, Vec<u8>)>,
    ) {
        // Already delivered (a resend whose ack was lost), or too far ahead.
        if id < self.next_in || id - self.next_in >= WINDOW {
            return;
        }
        let m = self.inbox.entry(id).or_insert_with(|| InMessage {
            count,
            fragments: HashMap::new(),
        });
        if m.count != count || m.fragments.contains_key(&index) {
            return;
        }
        m.fragments.insert(index, bytes.to_vec());
        self.buffered += bytes.len() + FRAGMENT_COST;
        while let Some(m) = self.inbox.get(&self.next_in) {
            if m.fragments.len() != m.count as usize {
                break;
            }
            let mut m = self.inbox.remove(&self.next_in).expect("present");
            let mut msg = Vec::new();
            for i in 0..m.count {
                msg.extend_from_slice(&m.fragments.remove(&i).expect("complete"));
            }
            self.buffered -= msg.len() + m.count as usize * FRAGMENT_COST;
            delivered.push((Channel::Reliable, msg));
            self.next_in += 1;
        }
    }

    /// Builds the datagrams to send now: queued unreliable messages, reliable
    /// fragments never sent or past their timeout (oldest first), acks and
    /// keepalives, within the token bucket.
    pub(crate) fn flush(&mut self, now: Duration) -> Vec<Vec<u8>> {
        let dt = now.saturating_sub(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + dt * self.rate).min(burst(self.rate));
        let rto = self.rto();
        let oldest = self.out.keys().next().copied().unwrap_or(self.next_out);
        let mut due: VecDeque<(u32, u16, bool)> = VecDeque::new();
        for (&id, m) in &self.out {
            if id - oldest >= WINDOW {
                break;
            }
            for (i, f) in m.fragments.iter().enumerate() {
                match f.last_sent {
                    _ if f.acked => {}
                    None => due.push_back((id, i as u16, false)),
                    Some(t) if now.saturating_sub(t) >= rto => due.push_back((id, i as u16, true)),
                    Some(_) => {}
                }
            }
        }
        let mut datagrams = Vec::new();
        let mut keepalive = now.saturating_sub(self.last_send) >= self.keepalive;
        loop {
            let work = !self.unreliable.is_empty() || !due.is_empty();
            let can_send = self.tokens >= MTU as f64;
            if !(work && can_send) && !self.need_ack && !keepalive {
                break;
            }
            keepalive = false;
            let mut p = packet::begin(Kind::Data, 0);
            // The data header goes in by `data_packet`, after the chunks are
            // chosen; reserve its bytes.
            p.extend_from_slice(&[0; packet::DATA_HEADER]);
            let mut fragments = Vec::new();
            if can_send {
                while let Some(u) = self.unreliable.front() {
                    if p.len() + CHUNK_HEADER + u.len() > SEALED_MAX {
                        break;
                    }
                    let u = self.unreliable.pop_front().expect("front");
                    packet::chunk(&mut p, Channel::Unreliable.id(), 0, 0, 1, &u);
                }
                while let Some(&(id, f, resend)) = due.front() {
                    let m = &self.out[&id];
                    let data = &m.fragments[f as usize].data;
                    if p.len() + CHUNK_HEADER + data.len() > SEALED_MAX {
                        break;
                    }
                    due.pop_front();
                    let count = m.fragments.len() as u16;
                    packet::chunk(&mut p, Channel::Reliable.id(), id, f, count, data);
                    fragments.push((id, f));
                    self.stats.fragments_sent += 1;
                    if resend {
                        self.stats.fragments_resent += 1;
                    }
                }
            }
            for &(id, f) in &fragments {
                if let Some(m) = self.out.get_mut(&id) {
                    let frag = &mut m.fragments[f as usize];
                    if frag.last_sent.is_none() {
                        self.in_flight += 1;
                    }
                    frag.last_sent = Some(now);
                }
            }
            if !fragments.is_empty() && self.waiting_since.is_none() {
                self.waiting_since = Some(now);
            }
            let empty = p.len() == HEADER + packet::DATA_HEADER;
            let Some(p) = self.data_packet(now, fragments, p) else {
                break;
            };
            // Acks and keepalives are small and must flow even when the
            // bucket is empty; only packets with data spend tokens.
            if !empty {
                self.tokens -= p.len() as f64;
            }
            self.stats.packets_sent += 1;
            self.stats.bytes_sent += p.len() as u64;
            datagrams.push(p);
            if self.unreliable.is_empty() && due.is_empty() {
                break;
            }
        }
        datagrams
    }

    /// Finishes a data packet: its sequence number and acks go into the
    /// reserved header bytes (or are appended when `p` is only the packet
    /// header), the packet is remembered for acks, and sealed.
    fn data_packet(
        &mut self,
        now: Duration,
        fragments: Vec<(u32, u16)>,
        mut p: Vec<u8>,
    ) -> Option<Vec<u8>> {
        if self.spent() {
            return None;
        }
        let seq = self.local_seq;
        self.local_seq = self.local_seq.wrapping_add(1);
        let mut head = [0u8; packet::DATA_HEADER];
        head[0..2].copy_from_slice(&seq.to_le_bytes());
        head[2..4].copy_from_slice(&self.remote_seq.to_le_bytes());
        head[4..8].copy_from_slice(&self.recv_bits.to_le_bytes());
        if p.len() == HEADER {
            p.extend_from_slice(&head);
        } else {
            p[HEADER..HEADER + packet::DATA_HEADER].copy_from_slice(&head);
        }
        self.sent.insert(seq, SentPacket { at: now, fragments });
        if self.sent.len() > MAX_SENT {
            // Packets this old are lost; their fragments resend by timeout.
            let cut = self.local_seq.wrapping_sub(MAX_SENT as u16 / 2);
            self.sent.retain(|s, _| seq_newer(*s, cut));
        }
        self.need_ack = false;
        self.unacked_received = 0;
        self.last_send = now;
        self.seal(p)
    }
}

/// The token bucket's capacity: a tenth of a second at `rate`, and at least
/// four datagrams.
fn burst(rate: f64) -> f64 {
    (rate / 10.0).max(MTU as f64 * 4.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn pair() -> (Conn, Conn) {
        let (a, b) = crate::noise::test_pair();
        let c = |keys| Conn::new(7, keys, Duration::ZERO, 1e9, MS * 30, MS * 100);
        (c(a), c(b))
    }

    /// A connection with the client's end of a session nobody answers.
    fn alone(rate: f64, rto_min: Duration) -> Conn {
        Conn::new(
            1,
            crate::noise::test_pair().0,
            Duration::ZERO,
            rate,
            rto_min,
            MS * 100,
        )
    }

    /// Opens a datagram `to`'s peer sealed, as the endpoint does: its body,
    /// or `None` when it does not open.
    fn open(to: &mut Conn, d: &[u8]) -> Option<Vec<u8>> {
        let packet::Packet::Sealed { number, .. } = packet::decode(d) else {
            panic!("not a sealed packet: {d:?}")
        };
        let mut d = d.to_vec();
        let len = to.open(number, &mut d)?;
        Some(d[HEADER..HEADER + len].to_vec())
    }

    /// Moves every datagram of `from` to `to` (bodies only), returning what
    /// `to` delivered.
    fn carry(
        from: &mut Conn,
        to: &mut Conn,
        now: Duration,
        keep: impl Fn(usize) -> bool,
    ) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for (i, d) in from.flush(now).into_iter().enumerate() {
            if !keep(i) {
                continue;
            }
            let body = open(to, &d).expect("opens");
            match to.receive(now, &body) {
                Received::Messages(m) => out.extend(m.into_iter().map(|(_, b)| b)),
                Received::Malformed => panic!("malformed"),
            }
        }
        out
    }

    #[test]
    fn a_large_message_fragments_and_reassembles() {
        let (mut a, mut b) = pair();
        let msg: Vec<u8> = (0..10_000u32).map(|i| (i * 7) as u8).collect();
        a.queue(Channel::Reliable, &msg);
        assert_eq!(a.backlog(), 10_000);
        let got = carry(&mut a, &mut b, MS, |_| true);
        assert_eq!(got, vec![msg]);
        carry(&mut b, &mut a, MS * 2, |_| true);
        assert_eq!(a.backlog(), 0, "acked");
    }

    #[test]
    fn a_lost_fragment_is_resent_after_the_timeout_and_order_holds() {
        let (mut a, mut b) = pair();
        a.queue(Channel::Reliable, &[1; 3000]);
        a.queue(Channel::Reliable, b"second");
        // Lose the first datagram: nothing can be delivered yet.
        let got = carry(&mut a, &mut b, MS, |i| i != 0);
        assert!(got.is_empty());
        carry(&mut b, &mut a, MS * 2, |_| true);
        // Before the timeout nothing is resent.
        assert!(carry(&mut a, &mut b, MS * 10, |_| true).is_empty());
        let got = carry(&mut a, &mut b, MS * 250, |_| true);
        assert_eq!(got, vec![vec![1; 3000], b"second".to_vec()]);
        assert_eq!(a.stats.fragments_resent, 1);
    }

    #[test]
    fn a_duplicate_is_dropped_before_it_opens_and_counted() {
        let (mut a, mut b) = pair();
        a.queue(Channel::Reliable, b"once");
        let d = a.flush(MS);
        let mut n = 0;
        for _ in 0..3 {
            if let Some(body) = open(&mut b, &d[0])
                && let Received::Messages(m) = b.receive(MS, &body)
            {
                n += m.len();
            }
        }
        assert_eq!(n, 1);
        assert_eq!((b.stats.replayed, b.stats.rejected), (2, 0));
    }

    #[test]
    fn a_body_received_twice_delivers_once() {
        // Behind the replay window, the sequence numbers still keep a
        // packet's messages from arriving twice.
        let (mut a, mut b) = pair();
        a.queue(Channel::Reliable, b"once");
        a.queue(Channel::Unreliable, b"u");
        let body = open(&mut b, &a.flush(MS)[0]).unwrap();
        let mut n = 0;
        for _ in 0..3 {
            if let Received::Messages(m) = b.receive(MS, &body) {
                n += m.len();
            }
        }
        assert_eq!(n, 2);
    }

    #[test]
    fn each_packet_has_its_own_number_and_opens_only_with_the_peers_keys() {
        let (mut a, mut b) = pair();
        for i in 0..20u8 {
            a.queue(Channel::Reliable, &[i; 1000]);
        }
        let sent = a.flush(MS);
        let numbers: Vec<u64> = sent
            .iter()
            .map(|d| match packet::decode(d) {
                packet::Packet::Sealed { number, .. } => number,
                p => panic!("{p:?}"),
            })
            .collect();
        assert_eq!(numbers, (0..sent.len() as u64).collect::<Vec<_>>());
        // The sender cannot open its own packets: each direction has its key.
        assert_eq!(open(&mut a, &sent[0]), None);
        assert_eq!(a.stats.rejected, 1);
        for d in &sent {
            assert!(open(&mut b, d).is_some());
        }
        assert!(sent.iter().all(|d| d.len() <= MTU));
    }

    #[test]
    fn a_packet_that_fails_its_tag_never_moves_the_window() {
        let (mut a, mut b) = pair();
        for i in 0..3u8 {
            a.queue(Channel::Unreliable, &[i]);
            a.flush(MS * (u32::from(i) * 200 + 1));
        }
        a.queue(Channel::Unreliable, b"late");
        let late = a.flush(MS * 700);
        // A forgery with a number far ahead: the tag fails, and the window
        // must not move to it, or the real packets after it would all be
        // too old.
        let mut forged = late[0].clone();
        packet::set_number(&mut forged, 1_000_000);
        assert_eq!(open(&mut b, &forged), None);
        assert_eq!(b.stats.rejected, 1);
        assert!(
            open(&mut b, &late[0]).is_some(),
            "the real packet still opens"
        );
        assert_eq!((b.stats.rejected, b.stats.replayed), (1, 0));
    }

    #[test]
    fn a_connection_that_used_up_its_numbers_sends_nothing() {
        let (mut a, mut b) = pair();
        a.next_number = u64::MAX - 1;
        a.queue(Channel::Reliable, b"last");
        let sent = a.flush(MS);
        assert_eq!(sent.len(), 1);
        assert!(open(&mut b, &sent[0]).is_some());
        assert!(a.spent());
        a.queue(Channel::Reliable, b"never");
        assert!(a.flush(MS * 300).is_empty());
        assert!(a.ack(MS * 300).is_none());
        assert!(a.disconnect("bye").is_none());
    }

    #[test]
    fn rtt_follows_acks() {
        let (mut a, mut b) = pair();
        a.queue(Channel::Reliable, b"x");
        carry(&mut a, &mut b, Duration::ZERO, |_| true);
        carry(&mut b, &mut a, MS * 40, |_| true);
        assert_eq!(a.rtt(), Some(MS * 40));
        assert!(a.window() >= MIN_WINDOW);
    }

    #[test]
    fn a_packet_with_one_bad_chunk_changes_nothing() {
        let (mut a, mut b) = pair();
        a.queue(Channel::Reliable, b"ok");
        let mut d = open(&mut b, &a.flush(MS).remove(0)).unwrap();
        d.push(9); // a chunk with channel 9, truncated
        assert!(matches!(b.receive(MS, &d), Received::Malformed));
        assert_eq!(b.stats.malformed, 1);
        d.pop();
        let Received::Messages(m) = b.receive(MS, &d) else {
            panic!()
        };
        assert_eq!(m.len(), 1);
    }

    /// Delivers datagrams to `to`, acking at once when `to` says an ack is
    /// due (the endpoint's behaviour); returns delivered messages and the
    /// immediate acks.
    fn deliver(
        to: &mut Conn,
        now: Duration,
        datagrams: &[Vec<u8>],
    ) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let (mut got, mut acks) = (Vec::new(), Vec::new());
        for d in datagrams {
            if let Some(body) = open(to, d)
                && let Received::Messages(m) = to.receive(now, &body)
            {
                got.extend(m.into_iter().map(|(_, b)| b));
            }
            if to.ack_due() {
                acks.push(to.ack(now).unwrap());
            }
        }
        (got, acks)
    }

    #[test]
    fn a_peer_that_has_received_nothing_acks_nothing() {
        let (mut a, mut b) = pair();
        a.queue(Channel::Reliable, b"hello");
        let lost = a.flush(MS); // seq 0, lost
        assert_eq!(lost.len(), 1);
        // b sends before anything of a's arrived (a keepalive).
        let keepalive = b.flush(MS * 200);
        assert_eq!(keepalive.len(), 1);
        deliver(&mut a, MS * 201, &keepalive);
        assert_eq!(a.backlog(), 5, "the lost packet was not taken as acked");
        let (got, _) = deliver(&mut b, MS * 400, &a.flush(MS * 400));
        assert_eq!(got, vec![b"hello".to_vec()]);
        assert_eq!(a.stats.fragments_resent, 1);
    }

    #[test]
    fn a_full_receive_buffer_defers_packets_instead_of_losing_them() {
        let (mut a, mut b) = pair();
        let big: Vec<Vec<u8>> = (0..12u8).map(|i| vec![i; MAX_MESSAGE]).collect();
        for m in &big {
            a.queue(Channel::Reliable, m);
        }
        let mut now = MS;
        let first = a.flush(now);
        // Lose the datagram with message 0's sixth fragment; deliver the rest.
        let (mut got, acks) = deliver(&mut b, now, &[&first[..5], &first[6..]].concat());
        assert!(got.is_empty());
        assert!(b.stats.packets_deferred > 0, "the buffer filled");
        deliver(&mut a, now, &acks);
        for _ in 0..200 {
            now += MS * 50;
            let (g, acks) = deliver(&mut b, now, &a.flush(now));
            got.extend(g);
            deliver(&mut a, now, &acks);
            deliver(&mut a, now, &b.flush(now));
            if a.backlog() == 0 {
                break;
            }
        }
        assert!(got == big, "all twelve arrive, in order");
        assert_eq!(a.backlog(), 0);
    }

    #[test]
    fn a_lost_short_fragment_still_completes_when_the_buffer_is_full() {
        // The small messages share their resend packet with a later
        // message's short last fragment, which never fits while the buffer
        // is full.
        let (mut a, mut b) = pair();
        let mut sent = vec![vec![1; 10], vec![2; 1141]];
        sent.extend((0..10u8).map(|i| vec![i + 3; MAX_MESSAGE]));
        for m in &sent {
            a.queue(Channel::Reliable, m);
        }
        let mut now = MS;
        let first = a.flush(now);
        let (mut got, acks) = deliver(&mut b, now, &first[1..]);
        assert!(b.stats.packets_deferred > 0, "the buffer filled");
        deliver(&mut a, now, &acks);
        for _ in 0..400 {
            now += MS * 50;
            let (g, acks) = deliver(&mut b, now, &a.flush(now));
            got.extend(g);
            deliver(&mut a, now, &acks);
            deliver(&mut a, now, &b.flush(now));
            if a.backlog() == 0 {
                break;
            }
        }
        assert!(got == sent, "{} of {} arrived", got.len(), sent.len());
    }

    #[test]
    fn a_send_after_a_quiet_spell_is_not_stalled() {
        let (mut a, mut b) = pair();
        let timeout = Duration::from_secs(10);
        a.queue(Channel::Reliable, b"x");
        deliver(&mut b, MS, &a.flush(MS));
        deliver(&mut a, MS * 50, &b.flush(MS * 50));
        let mut now = MS * 50;
        while now < MS * 10_040 {
            now += MS * 16;
            a.flush(now);
            assert!(!a.stalled(now, timeout));
        }
        a.queue(Channel::Reliable, b"y");
        a.flush(now);
        assert!(
            !a.stalled(now + MS * 16, timeout),
            "the ack is one round trip away"
        );
        assert!(a.stalled(now + timeout + MS, timeout), "but never coming");
    }

    #[test]
    fn a_burst_of_packets_is_acked_in_full() {
        let (mut a, mut b) = pair();
        for i in 0..100u8 {
            a.queue(Channel::Reliable, &[i; 1000]);
        }
        let burst = a.flush(MS);
        assert!(burst.len() > 60, "{}", burst.len());
        let (got, acks) = deliver(&mut b, MS, &burst);
        assert_eq!(got.len(), 100);
        deliver(&mut a, MS * 2, &acks);
        deliver(&mut a, MS * 3, &b.flush(MS * 3));
        assert_eq!(a.backlog(), 0, "every packet acked, none to resend");
        a.flush(MS * 500);
        assert_eq!(a.stats.fragments_resent, 0);
    }

    #[test]
    fn a_duplicated_datagram_delivers_its_unreliable_message_once() {
        let (mut a, mut b) = pair();
        a.queue(Channel::Unreliable, b"u");
        let d = a.flush(MS);
        let (got, _) = deliver(&mut b, MS, &[d[0].clone(), d[0].clone()]);
        assert_eq!(got, vec![b"u".to_vec()]);
    }

    #[test]
    fn a_peer_that_never_acks_is_stalled_after_the_timeout() {
        let (mut a, mut b) = pair();
        let timeout = Duration::from_secs(10);
        a.queue(Channel::Reliable, b"x");
        a.flush(MS);
        assert!(!a.stalled(Duration::from_secs(5), timeout));
        assert!(a.stalled(Duration::from_secs(11), timeout));
        // Once acked, nothing waits.
        let (mut c, _) = pair();
        c.queue(Channel::Reliable, b"y");
        let d = c.flush(MS);
        deliver(&mut b, MS, &d);
        deliver(&mut c, MS * 2, &b.flush(MS * 2));
        assert!(!c.stalled(Duration::from_secs(60), timeout));
    }

    #[test]
    fn odd_settings_neither_hang_nor_panic() {
        let mut a = Conn::new(
            1,
            crate::noise::test_pair().0,
            Duration::ZERO,
            f64::NAN,
            Duration::from_secs(5),
            Duration::ZERO,
        );
        for _ in 0..50 {
            a.queue(Channel::Reliable, &[0; 1000]);
        }
        for t in 0..10 {
            a.flush(Duration::from_secs(t));
        }
        assert!(a.rto() >= Duration::from_secs(5));
    }

    #[test]
    fn the_unreliable_queue_is_bounded() {
        let mut a = alone(1e9, MS);
        for _ in 0..MAX_UNRELIABLE + 10 {
            a.queue(Channel::Unreliable, b"z");
        }
        assert_eq!(a.stats.unreliable_dropped, 10);
    }

    #[test]
    fn the_token_bucket_limits_bytes_per_second() {
        let mut a = alone(120_000.0, MS * 30);
        for _ in 0..100 {
            a.queue(Channel::Reliable, &[0; 1000]);
        }
        let mut sent = 0;
        for t in 0..60 {
            sent += a
                .flush(MS * (t * 1000 / 60))
                .iter()
                .map(Vec::len)
                .sum::<usize>();
        }
        // About one second at 120 KB/s, plus the initial burst.
        assert!(sent > 100_000 && sent < 140_000, "{sent}");
    }
}
