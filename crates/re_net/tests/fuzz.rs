//! Seeded fuzzing: random datagrams from strangers, and corrupted, truncated
//! and forged copies of a connected client's real packets. Nothing may
//! panic. Damaged packets fail the CRC and count as lost, so every message
//! still arrives intact; forged packets with a valid CRC (what only someone
//! who knows the connection's salt can make) may break the stream, but never
//! the process.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use re_net::{
    Channel, Config, Endpoint, Event, MTU, MemoryNet, MemoryTransport, PeerId, Transport,
};

/// A seeded xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// CRC-32 (IEEE), as the trailer uses.
fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// Replaces a datagram's trailer with a valid CRC.
fn reseal(d: &mut Vec<u8>) {
    let body = d.len().saturating_sub(4);
    d.truncate(body);
    let crc = crc32(d);
    d.extend_from_slice(&crc.to_le_bytes());
}

/// A client transport that mangles a share of its outgoing datagrams, and
/// with `forge` gives the mangled ones a valid CRC.
struct Mangler {
    inner: MemoryTransport,
    rng: Rng,
    share: u64,
    forge: bool,
    mangled: u64,
}

impl Transport for Mangler {
    fn send_to(&mut self, to: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        if self.rng.below(100) >= self.share {
            return self.inner.send_to(to, datagram);
        }
        self.mangled += 1;
        let mut d = datagram.to_vec();
        match self.rng.below(6) {
            // Truncate anywhere.
            0 => d.truncate(self.rng.below(d.len() as u64 + 1) as usize),
            // Flip bytes after the header.
            1 => {
                for _ in 0..1 + self.rng.below(8) {
                    let at = 15 + self.rng.below(d.len().saturating_sub(15) as u64) as usize;
                    if at < d.len() {
                        d[at] ^= 1 + self.rng.below(255) as u8;
                    }
                }
            }
            // Keep the header, replace the rest with noise.
            2 => {
                d.truncate(15.min(d.len()));
                for _ in 0..self.rng.below(MTU as u64) {
                    d.push(self.rng.next() as u8);
                }
            }
            // A chunk claiming the most fragments, far-ahead ids and the
            // longest length.
            3 => {
                d.truncate(23.min(d.len()));
                for _ in 0..40 {
                    d.push(1);
                    d.extend_from_slice(&((self.rng.next() as u32) % 2048).to_le_bytes());
                    d.extend_from_slice(&((self.rng.next() as u16) % 900).to_le_bytes());
                    d.extend_from_slice(&900u16.to_le_bytes());
                    d.extend_from_slice(&20u16.to_le_bytes());
                    d.extend_from_slice(&[0xAB; 20]);
                }
            }
            // Append garbage.
            4 => d.extend((0..self.rng.below(64)).map(|_| self.rng.next() as u8)),
            // Send it twice, the second one truncated.
            _ => {
                self.inner.send_to(to, &d)?;
                d.truncate(d.len() / 2);
            }
        }
        if self.forge {
            reseal(&mut d);
        }
        self.inner.send_to(to, &d)
    }

    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        self.inner.recv_from(buf)
    }

    fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr()
    }
}

const TICK: Duration = Duration::from_micros(16_667);

#[test]
fn strangers_noise_never_panics_or_connects() {
    let net = MemoryNet::new();
    let mut server = Endpoint::server(net.bind(), Config::server(1, 1));
    let mut strangers: Vec<MemoryTransport> = (0..8).map(|_| net.bind()).collect();
    let to = server.local_addr();
    let mut rng = Rng(0x5eed);
    let mut now = Duration::ZERO;
    for _ in 0..2000 {
        for s in &mut strangers {
            let len = rng.below(MTU as u64 + 200) as usize;
            let mut d: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            // Half of them start like ours, with every kind, and most of
            // those carry a valid CRC.
            if rng.below(2) == 0 && d.len() >= 19 {
                d[..4].copy_from_slice(b"RENT");
                d[4..6].copy_from_slice(&1u16.to_le_bytes());
                d[6] = rng.below(8) as u8;
                if rng.below(4) != 0 {
                    reseal(&mut d);
                }
            }
            s.send_to(to, &d).unwrap();
        }
        now += TICK;
        let events = server.poll(now);
        assert!(events.is_empty(), "{events:?}");
        server.flush(now);
    }
    assert!(server.peers().is_empty());
    assert!(server.stray() > 0);
}

fn mangled_run(seed: u64, forge: bool) -> (Vec<Vec<u8>>, Vec<Vec<u8>>, bool) {
    let net = MemoryNet::new();
    let mut server = Endpoint::server(net.bind(), Config::server(1, 1));
    let mangler = Mangler {
        inner: net.bind(),
        rng: Rng(seed * 7919),
        share: 0,
        forge,
        mangled: 0,
    };
    let mut client = Endpoint::client(mangler, server.local_addr(), Config::client(1, 1));
    let mut now = Duration::ZERO;
    let mut got = Vec::new();
    let mut frame = |server: &mut Endpoint<MemoryTransport>,
                     client: &mut Endpoint<Mangler>,
                     got: &mut Vec<Vec<u8>>| {
        now += TICK;
        client.poll(now);
        for e in server.poll(now) {
            if let Event::Message { bytes, .. } = e {
                got.push(bytes);
            }
        }
        client.flush(now);
        server.flush(now);
    };
    for _ in 0..5 {
        frame(&mut server, &mut client, &mut got);
    }
    assert!(client.is_connected());
    client.transport_mut().share = 40;
    let mut sent = Vec::new();
    for i in 0..1000u32 {
        let m = i.to_le_bytes().repeat(1 + (i % 700) as usize);
        if client.send(PeerId(0), Channel::Reliable, &m).is_ok() {
            sent.push(m);
        }
        frame(&mut server, &mut client, &mut got);
    }
    assert!(client.transport().mangled > 100);
    client.transport_mut().share = 0;
    for _ in 0..1200 {
        frame(&mut server, &mut client, &mut got);
    }
    let alive = server.peers() == vec![PeerId(0)];
    (sent, got, alive)
}

#[test]
fn damaged_packets_count_as_lost_and_every_message_arrives_intact() {
    for seed in [1u64, 2, 3] {
        let (sent, got, alive) = mangled_run(seed, false);
        assert!(alive, "seed {seed}");
        assert_eq!(got.len(), sent.len(), "seed {seed}");
        assert!(
            got == sent,
            "seed {seed}: a message arrived damaged or out of order"
        );
    }
}

#[test]
fn forged_packets_with_a_valid_crc_never_panic() {
    for seed in [4u64, 5, 6] {
        let (_, got, _) = mangled_run(seed, true);
        // Whatever arrived is bounded by what fits through the connection.
        assert!(got.len() <= 100_000);
    }
}
