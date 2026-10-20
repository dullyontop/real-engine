//! One connection's reliability: acks, round-trip time, the reliable ordered
//! channel's fragments and resends, the unreliable queue and the token
//! bucket (ADR-0050 §1).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

use crate::packet::{
    self, BODY_MAX, CHUNK_HEADER, FRAGMENT, Kind, MAX_FRAGMENTS, MAX_MESSAGE, MTU, Reader,
    seq_newer,
};

/// How far past the oldest unacked reliable message a sender may go, and how
/// far ahead of the next expected one a receiver keeps fragments.
const WINDOW: u32 = 1024;
/// The most reliable bytes a receiver holds waiting for earlier fragments;
/// fragments past it are dropped and come again.
const MAX_BUFFERED: usize = 8 * MAX_MESSAGE;
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
    /// Datagrams from this peer that did not parse and were dropped.
    pub malformed: u64,
    /// Messages delivered to the caller.
    pub messages_received: u64,
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
    pub(crate) salt: u64,
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
        now: Duration,
        rate: f64,
        rto_min: Duration,
        keepalive: Duration,
    ) -> Conn {
        Conn {
            salt,
            local_seq: 0,
            remote_seq: 0,
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
            tokens: burst(rate),
            last_refill: now,
            rate,
            rto_min,
            keepalive,
            stats: ConnStats::default(),
        }
    }

    /// The smoothed round-trip time, once a packet has been acked.
    pub(crate) fn rtt(&self) -> Option<Duration> {
        self.srtt.map(Duration::from_secs_f64)
    }

    /// Reliable bytes queued and not yet acked.
    pub(crate) fn backlog(&self) -> usize {
        self.backlog
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
            Some(s) => Duration::from_secs_f64(s + 4.0 * self.rttvar),
        };
        rto.clamp(self.rto_min, Duration::from_secs(1))
    }

    /// Queues a message. The caller checked its size.
    pub(crate) fn queue(&mut self, channel: Channel, bytes: &[u8]) {
        match channel {
            Channel::Unreliable => self.unreliable.push_back(bytes.to_vec()),
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
        let mut r = Reader(body);
        let seq = r.u16()?;
        let ack = r.u16()?;
        let ack_bits = r.u32()?;
        // Check every chunk before acting on any, so a malformed packet
        // changes nothing.
        let chunks = {
            let mut chunks = Vec::new();
            while !r.is_empty() {
                let channel = r.u8()?;
                let id = r.u32()?;
                let index = r.u16()?;
                let count = r.u16()?;
                let len = r.u16()? as usize;
                let bytes = r.take(len)?;
                let channel = match channel {
                    0 => Channel::Unreliable,
                    1 => Channel::Reliable,
                    _ => return None,
                };
                if channel == Channel::Reliable
                    && (count == 0
                        || index >= count
                        || count as usize > MAX_FRAGMENTS
                        || len > FRAGMENT)
                {
                    return None;
                }
                chunks.push((channel, id, index, count, bytes));
            }
            chunks
        };
        self.last_recv = now;
        self.note_received(seq);
        self.note_acks(now, ack, ack_bits);
        if !chunks.is_empty() {
            self.need_ack = true;
        }
        let mut delivered = Vec::new();
        for (channel, id, index, count, bytes) in chunks {
            match channel {
                Channel::Unreliable => delivered.push((Channel::Unreliable, bytes.to_vec())),
                Channel::Reliable => self.take_fragment(id, index, count, bytes, &mut delivered),
            }
        }
        Some(delivered)
    }

    fn note_received(&mut self, seq: u16) {
        if !self.any_received {
            self.any_received = true;
            self.remote_seq = seq;
            self.recv_bits = 0;
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
        } else {
            let back = self.remote_seq.wrapping_sub(seq) as u32;
            if (1..=32).contains(&back) {
                self.recv_bits |= 1 << (back - 1);
            }
        }
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
        if self.buffered + bytes.len() > MAX_BUFFERED {
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
        self.buffered += bytes.len();
        while let Some(m) = self.inbox.get(&self.next_in) {
            if m.fragments.len() != m.count as usize {
                break;
            }
            let mut m = self.inbox.remove(&self.next_in).expect("present");
            let mut msg = Vec::new();
            for i in 0..m.count {
                msg.extend_from_slice(&m.fragments.remove(&i).expect("complete"));
            }
            self.buffered -= msg.len();
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
        loop {
            let work = !self.unreliable.is_empty() || !due.is_empty();
            let keepalive = now.saturating_sub(self.last_send) >= self.keepalive;
            let can_send = self.tokens >= MTU as f64;
            if !(work && can_send) && !self.need_ack && !keepalive {
                break;
            }
            let mut p = packet::begin(Kind::Data, self.salt);
            let seq = self.local_seq;
            self.local_seq = self.local_seq.wrapping_add(1);
            p.extend_from_slice(&seq.to_le_bytes());
            p.extend_from_slice(&self.remote_seq.to_le_bytes());
            p.extend_from_slice(&self.recv_bits.to_le_bytes());
            let mut fragments = Vec::new();
            if can_send {
                while let Some(u) = self.unreliable.front() {
                    if p.len() + CHUNK_HEADER + u.len() > BODY_MAX {
                        break;
                    }
                    let u = self.unreliable.pop_front().expect("front");
                    packet::chunk(&mut p, Channel::Unreliable.id(), 0, 0, 1, &u);
                }
                while let Some(&(id, f, resend)) = due.front() {
                    let m = &self.out[&id];
                    let data = &m.fragments[f as usize].data;
                    if p.len() + CHUNK_HEADER + data.len() > BODY_MAX {
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
                    m.fragments[f as usize].last_sent = Some(now);
                }
            }
            self.sent.insert(seq, SentPacket { at: now, fragments });
            if self.sent.len() > MAX_SENT {
                // Packets this old are lost; their fragments resend by timeout.
                let cut = self.local_seq.wrapping_sub(MAX_SENT as u16 / 2);
                self.sent.retain(|s, _| seq_newer(*s, cut));
            }
            let p = packet::seal(p);
            self.tokens -= p.len() as f64;
            self.need_ack = false;
            self.last_send = now;
            self.stats.packets_sent += 1;
            self.stats.bytes_sent += p.len() as u64;
            datagrams.push(p);
            if self.unreliable.is_empty() && due.is_empty() {
                break;
            }
        }
        datagrams
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
        let c = |_| Conn::new(7, Duration::ZERO, 1e9, MS * 30, MS * 100);
        (c(0), c(1))
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
            match to.receive(now, &packet::open(&d).unwrap()[packet::HEADER..]) {
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
    fn duplicates_are_delivered_once() {
        let (mut a, mut b) = pair();
        a.queue(Channel::Reliable, b"once");
        let d = a.flush(MS);
        let mut n = 0;
        for _ in 0..3 {
            if let Received::Messages(m) =
                b.receive(MS, &packet::open(&d[0]).unwrap()[packet::HEADER..])
            {
                n += m.len();
            }
        }
        assert_eq!(n, 1);
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
        let mut d = packet::open(&a.flush(MS).remove(0)).unwrap().to_vec();
        d.push(9); // a chunk with channel 9, truncated
        assert!(matches!(
            b.receive(MS, &d[packet::HEADER..]),
            Received::Malformed
        ));
        assert_eq!(b.stats.malformed, 1);
        d.pop();
        let Received::Messages(m) = b.receive(MS, &d[packet::HEADER..]) else {
            panic!()
        };
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn the_token_bucket_limits_bytes_per_second() {
        let mut a = Conn::new(7, Duration::ZERO, 120_000.0, MS * 30, MS * 100);
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
