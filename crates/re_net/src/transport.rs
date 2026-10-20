//! The transport seam (ADR-0050 §1): what an [`Endpoint`](crate::Endpoint)
//! sends datagrams through.
//!
//! * [`UdpTransport`]: a non-blocking `std::net::UdpSocket`.
//! * [`MemoryNet`]: datagrams between endpoints in one process, for tests and
//!   for the in-process two-world run.
//! * [`Conditioned`]: wraps either and drops, duplicates and delays datagrams
//!   from a seed, so loss and reordering are reproducible.
//!
//! The trait has no `Send` bound and no blocking call, so a browser
//! implementation over a WebSocket fits it (ADR-0049 §10).

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::rc::Rc;
use std::time::Duration;

use re_core::Diagnostic;

use crate::codes;

/// Sends and receives datagrams of at most [`MTU`](crate::MTU) bytes.
pub trait Transport {
    /// Sends one datagram. A datagram that cannot be sent is lost, as on a
    /// real network; the error is only for the caller's statistics.
    fn send_to(&mut self, to: SocketAddr, datagram: &[u8]) -> io::Result<()>;

    /// Receives one waiting datagram into `buf`, or `None` when none waits.
    /// Never blocks.
    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>>;

    /// The address peers reach this transport at.
    fn local_addr(&self) -> SocketAddr;

    /// The current time, given once per poll and flush; transports that hold
    /// datagrams back ([`Conditioned`]) release them by it.
    fn advance(&mut self, _now: Duration) {}
}

/// A non-blocking UDP socket.
#[derive(Debug)]
pub struct UdpTransport {
    socket: UdpSocket,
    local: SocketAddr,
}

impl UdpTransport {
    /// Binds a socket to `addr` (port 0 picks a free port).
    ///
    /// # Errors
    ///
    /// `RE8900` when the socket cannot be bound or made non-blocking.
    pub fn bind(addr: SocketAddr) -> Result<UdpTransport, Diagnostic> {
        let fail = |e: io::Error| {
            Diagnostic::error(
                codes::BIND,
                format!("cannot open a UDP socket on {addr}: {e}"),
            )
            .with_hint("another program may be using the port; pick another with --port")
        };
        let socket = UdpSocket::bind(addr).map_err(fail)?;
        socket.set_nonblocking(true).map_err(fail)?;
        let local = socket.local_addr().map_err(fail)?;
        Ok(UdpTransport { socket, local })
    }

    /// A socket for a client of `server`: any free port on the unspecified
    /// address of the server's family.
    ///
    /// # Errors
    ///
    /// `RE8900`, as [`UdpTransport::bind`].
    pub fn for_server(server: SocketAddr) -> Result<UdpTransport, Diagnostic> {
        let any: SocketAddr = if server.is_ipv4() {
            ([0, 0, 0, 0], 0).into()
        } else {
            ([0u16; 8], 0).into()
        };
        UdpTransport::bind(any)
    }
}

impl Transport for UdpTransport {
    fn send_to(&mut self, to: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        self.socket.send_to(datagram, to).map(|_| ())
    }

    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        loop {
            match self.socket.recv_from(buf) {
                Ok((n, from)) => return Ok(Some((n, from))),
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(None),
                // Windows reports an earlier send's ICMP "port unreachable"
                // on the next receive; the connection timeout decides instead.
                Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn local_addr(&self) -> SocketAddr {
        self.local
    }
}

/// Parses or resolves `text` (`host:port`, or `host` with `default_port`).
///
/// # Errors
///
/// `RE8901` when it does not parse or resolve.
pub fn resolve(text: &str, default_port: u16) -> Result<SocketAddr, Diagnostic> {
    if let Ok(a) = text.parse::<SocketAddr>() {
        return Ok(a);
    }
    let with_port = if text
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
        && !text.contains('[')
        && text.matches(':').count() == 1
    {
        text.to_string()
    } else {
        format!("{text}:{default_port}")
    };
    with_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
        .ok_or_else(|| {
            Diagnostic::error(
                codes::ADDRESS,
                format!("cannot resolve the address `{text}`"),
            )
            .with_hint("write an address as host:port, for example 192.168.1.20:47800")
        })
}

type Queue = VecDeque<(SocketAddr, Vec<u8>)>;

/// Datagrams between transports in one process. Each [`MemoryNet::bind`]
/// gets its own loopback address; a datagram to an address nobody bound is
/// lost.
#[derive(Clone, Debug, Default)]
pub struct MemoryNet {
    inboxes: Rc<RefCell<HashMap<SocketAddr, Queue>>>,
    next_port: Rc<RefCell<u16>>,
}

impl MemoryNet {
    /// An empty network.
    pub fn new() -> MemoryNet {
        MemoryNet::default()
    }

    /// A transport on a fresh address (`127.0.0.1:1`, `:2`, ...).
    pub fn bind(&self) -> MemoryTransport {
        let addr = {
            let mut port = self.next_port.borrow_mut();
            *port += 1;
            SocketAddr::from(([127, 0, 0, 1], *port))
        };
        self.bind_at(addr)
    }

    /// A transport on `addr`, which must be free: a restarted process that
    /// gets its old address back, as behind a NAT.
    ///
    /// # Panics
    ///
    /// When `addr` is bound already.
    pub fn bind_at(&self, addr: SocketAddr) -> MemoryTransport {
        let fresh = self
            .inboxes
            .borrow_mut()
            .insert(addr, VecDeque::new())
            .is_none();
        assert!(fresh, "{addr} is bound already");
        MemoryTransport {
            net: self.clone(),
            addr,
        }
    }
}

/// One endpoint's side of a [`MemoryNet`]. Dropping it unbinds the address.
#[derive(Debug)]
pub struct MemoryTransport {
    net: MemoryNet,
    addr: SocketAddr,
}

impl Transport for MemoryTransport {
    fn send_to(&mut self, to: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        if let Some(q) = self.net.inboxes.borrow_mut().get_mut(&to) {
            q.push_back((self.addr, datagram.to_vec()));
        }
        Ok(())
    }

    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        let mut inboxes = self.net.inboxes.borrow_mut();
        let Some((from, d)) = inboxes.get_mut(&self.addr).and_then(|q| q.pop_front()) else {
            return Ok(None);
        };
        let n = d.len().min(buf.len());
        buf[..n].copy_from_slice(&d[..n]);
        Ok(Some((n, from)))
    }

    fn local_addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for MemoryTransport {
    fn drop(&mut self) {
        self.net.inboxes.borrow_mut().remove(&self.addr);
    }
}

/// What a [`Conditioned`] transport does to outgoing datagrams.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinkConditions {
    /// The chance that a datagram is lost, from 0 to 1.
    pub loss: f64,
    /// The chance that a datagram is sent twice.
    pub duplicate: f64,
    /// The delay every datagram gets.
    pub latency: Duration,
    /// A further random delay, from zero to this; it reorders datagrams.
    pub jitter: Duration,
    /// The seed of the random choices.
    pub seed: u64,
}

impl Default for LinkConditions {
    fn default() -> LinkConditions {
        LinkConditions {
            loss: 0.0,
            duplicate: 0.0,
            latency: Duration::ZERO,
            jitter: Duration::ZERO,
            seed: 1,
        }
    }
}

/// Counts of what a [`Conditioned`] transport did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConditionerStats {
    /// Datagrams dropped.
    pub dropped: u64,
    /// Datagrams sent twice.
    pub duplicated: u64,
}

/// A transport whose outgoing datagrams are lost, duplicated and delayed per
/// [`LinkConditions`], reproducibly from the seed.
#[derive(Debug)]
pub struct Conditioned<T> {
    inner: T,
    conditions: LinkConditions,
    rng: u64,
    now: Duration,
    held: Vec<(Duration, u64, SocketAddr, Vec<u8>)>,
    order: u64,
    /// What it did so far.
    pub stats: ConditionerStats,
}

impl<T: Transport> Conditioned<T> {
    /// Wraps `inner`.
    pub fn new(inner: T, conditions: LinkConditions) -> Conditioned<T> {
        Conditioned {
            inner,
            conditions,
            rng: conditions.seed.max(1),
            now: Duration::ZERO,
            held: Vec::new(),
            order: 0,
            stats: ConditionerStats::default(),
        }
    }

    /// Changes the conditions from now on.
    pub fn set_conditions(&mut self, conditions: LinkConditions) {
        self.conditions = conditions;
    }

    fn random(&mut self) -> f64 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn release(&mut self) {
        if self.held.is_empty() {
            return;
        }
        let now = self.now;
        let mut due: Vec<_> = Vec::new();
        self.held.retain(|h| {
            if h.0 <= now {
                due.push(h.clone());
                false
            } else {
                true
            }
        });
        due.sort_by_key(|h| (h.0, h.1));
        for (_, _, to, d) in due {
            let _ = self.inner.send_to(to, &d);
        }
    }
}

impl<T: Transport> Transport for Conditioned<T> {
    fn send_to(&mut self, to: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        if self.random() < self.conditions.loss {
            self.stats.dropped += 1;
            return Ok(());
        }
        let copies = if self.random() < self.conditions.duplicate {
            self.stats.duplicated += 1;
            2
        } else {
            1
        };
        for _ in 0..copies {
            let jitter = self.conditions.jitter.mul_f64(self.random());
            let at = self.now + self.conditions.latency + jitter;
            if at <= self.now {
                self.inner.send_to(to, datagram)?;
            } else {
                self.order += 1;
                self.held.push((at, self.order, to, datagram.to_vec()));
            }
        }
        Ok(())
    }

    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        self.inner.recv_from(buf)
    }

    fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr()
    }

    fn advance(&mut self, now: Duration) {
        self.now = now;
        self.inner.advance(now);
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_datagrams_reach_the_bound_address_only() {
        let net = MemoryNet::new();
        let mut a = net.bind();
        let mut b = net.bind();
        a.send_to(b.local_addr(), b"hi").unwrap();
        a.send_to(([127, 0, 0, 1], 999).into(), b"lost").unwrap();
        let mut buf = [0; 16];
        assert_eq!(b.recv_from(&mut buf).unwrap(), Some((2, a.local_addr())));
        assert_eq!(&buf[..2], b"hi");
        assert_eq!(b.recv_from(&mut buf).unwrap(), None);
        assert_eq!(a.recv_from(&mut buf).unwrap(), None);
    }

    #[test]
    fn the_conditioner_holds_datagrams_until_their_time() {
        let net = MemoryNet::new();
        let b = net.bind();
        let to = b.local_addr();
        let conditions = LinkConditions {
            latency: Duration::from_millis(50),
            ..LinkConditions::default()
        };
        let mut a = Conditioned::new(net.bind(), conditions);
        let mut b = b;
        a.advance(Duration::ZERO);
        a.send_to(to, b"x").unwrap();
        let mut buf = [0; 4];
        assert_eq!(b.recv_from(&mut buf).unwrap(), None);
        a.advance(Duration::from_millis(49));
        assert_eq!(b.recv_from(&mut buf).unwrap(), None);
        a.advance(Duration::from_millis(50));
        assert!(b.recv_from(&mut buf).unwrap().is_some());
    }

    #[test]
    fn the_conditioner_repeats_with_its_seed() {
        let run = || {
            let net = MemoryNet::new();
            let b = net.bind();
            let mut a = Conditioned::new(
                net.bind(),
                LinkConditions {
                    loss: 0.3,
                    duplicate: 0.1,
                    seed: 42,
                    ..LinkConditions::default()
                },
            );
            for i in 0..200u8 {
                a.send_to(b.local_addr(), &[i]).unwrap();
            }
            a.stats
        };
        let (x, y) = (run(), run());
        assert_eq!(x, y);
        assert!(x.dropped > 30 && x.dropped < 90, "{x:?}");
        assert!(x.duplicated > 3, "{x:?}");
    }

    #[test]
    fn addresses_resolve_with_a_default_port() {
        assert_eq!(
            resolve("127.0.0.1:9", 47800).unwrap(),
            ([127, 0, 0, 1], 9).into()
        );
        assert_eq!(
            resolve("127.0.0.1", 47800).unwrap(),
            ([127, 0, 0, 1], 47800).into()
        );
        assert_eq!(resolve("localhost", 5).unwrap().port(), 5);
        assert_eq!(
            resolve("not an address at all", 1).unwrap_err().code,
            codes::ADDRESS
        );
    }
}
