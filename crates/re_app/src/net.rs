//! Online play for compat projects (ADR-0050 §6): `real run --server`,
//! `--host` and `--connect` over UDP, through `re_compat::net::session`.
//!
//! [`App::set_net`](crate::App::set_net) binds the socket before the first
//! frame; the session starts with the compat game ([`NetPlay`]). In a frame:
//!
//! ```text
//! server: receive  pump(server)  server phases  [in-process replicate]  replicate (sends)
//! client:                                          receive  pump(client)  client phases  send
//! ```

use std::net::SocketAddr;
use std::time::Duration;

use re_compat::net::session::{ClientNet, PROTOCOL, ServerNet};
use re_core::Diagnostic;
use re_core::files::ProjectFiles;
use re_net::{Config, Endpoint, UdpTransport};
use serde::{Deserialize, Serialize};

/// `[net]` in `real.toml`: ports, players, rates and the timeout of online
/// play (ADR-0050 §6). A command-line flag overrides the file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NetConfig {
    /// The port a server listens on and a client connects to without one.
    pub port: u16,
    /// The most network players a server accepts.
    pub max_players: u32,
    /// Bytes per second a server sends each client.
    pub server_rate: u32,
    /// Bytes per second a client sends its server.
    pub client_rate: u32,
    /// A connection that hears nothing for this long closes; a lockstep run
    /// waits this long for a frame.
    pub timeout_ms: u64,
}

/// The default port (ADR-0050 §6).
pub const DEFAULT_PORT: u16 = 47800;

impl Default for NetConfig {
    fn default() -> Self {
        NetConfig {
            port: DEFAULT_PORT,
            max_players: 16,
            server_rate: 256 * 1024,
            client_rate: 64 * 1024,
            timeout_ms: 10_000,
        }
    }
}

impl NetConfig {
    /// True when nothing differs from the defaults.
    pub fn is_default(&self) -> bool {
        *self == NetConfig::default()
    }
}

/// What a run does online.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetRole {
    /// A dedicated server: the server context only.
    Server,
    /// A listen server: both contexts in this process, and remote players.
    Host,
    /// A client of the server at this address (`host:port`, or `host` with
    /// the configured port).
    Client(String),
}

/// How a run goes online; `None` fields come from `[net]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetOptions {
    /// The role.
    pub role: NetRole,
    /// The port to listen on (server and host).
    pub port: Option<u16>,
    /// The address to listen on (default `0.0.0.0`).
    pub bind: Option<String>,
    /// The most network players (server and host).
    pub max_players: Option<u32>,
    /// The player's name (client; default `Player<n>`).
    pub name: Option<String>,
    /// Lockstep (ADR-0050 §7): a server first waits for this many players,
    /// then each frame waits for every player's; a client waits for each
    /// server frame. For CI.
    pub lockstep: Option<u32>,
}

impl NetOptions {
    /// Options for `role` with every other value from `[net]`.
    pub fn new(role: NetRole) -> NetOptions {
        NetOptions {
            role,
            port: None,
            bind: None,
            max_players: None,
            name: None,
            lockstep: None,
        }
    }
}

/// The hash a client and its server must share (`RE8905`): the project's
/// `real.toml`, scenes and scripts, as the run reads them (ADR-0050 §5).
/// Files under `.real/` (data, caches) are left out.
pub fn game_hash(files: &ProjectFiles) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(b"real-engine/game-hash/1");
    let mut paths: Vec<String> = [".toml", ".json", ".luau", ".lua"]
        .iter()
        .flat_map(|s| files.list("", s))
        .map(|p| p.as_str().to_string())
        .filter(|p| !p.starts_with(".real/"))
        .collect();
    paths.sort();
    paths.dedup();
    for p in paths {
        let Ok(bytes) = files.read_source(&p) else {
            continue;
        };
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(&bytes);
    }
    let out = h.finalize();
    u64::from_le_bytes(out.as_bytes()[..8].try_into().expect("8 bytes"))
}

/// A run's network side.
pub(crate) enum NetPlay {
    /// Bound, waiting for the game to start.
    Bound {
        end: Box<Endpoint<UdpTransport>>,
        options: NetOptions,
    },
    /// Serving (dedicated or host).
    Server(Box<ServerNet<UdpTransport>>),
    /// Playing on a server.
    Client(Box<ClientNet<UdpTransport>>),
    /// Over: failed, kicked or the server went away.
    Closed,
}

impl NetPlay {
    /// Binds the socket for `options` (a server's listening socket, or a
    /// client's to its server); the game hash names the project.
    ///
    /// # Errors
    ///
    /// `RE8900` the socket does not bind, `RE8901` the address does not
    /// resolve.
    pub(crate) fn bind(
        options: NetOptions,
        config: &NetConfig,
        files: &ProjectFiles,
    ) -> Result<NetPlay, Diagnostic> {
        let game = game_hash(files);
        let timeout = Duration::from_millis(config.timeout_ms.max(1));
        let end = match &options.role {
            NetRole::Server | NetRole::Host => {
                let port = options.port.unwrap_or(config.port);
                let ip = options.bind.as_deref().unwrap_or("0.0.0.0");
                let addr = re_net::resolve(ip, port)?;
                let addr = SocketAddr::new(addr.ip(), port);
                let mut cfg = Config::server(PROTOCOL, game);
                cfg.send_rate = f64::from(config.server_rate.max(1));
                cfg.max_connections = options.max_players.unwrap_or(config.max_players) as usize;
                cfg.timeout = timeout;
                Endpoint::server(UdpTransport::bind(addr)?, cfg)
            }
            NetRole::Client(server) => {
                let addr = re_net::resolve(server, options.port.unwrap_or(config.port))?;
                let mut cfg = Config::client(PROTOCOL, game);
                cfg.send_rate = f64::from(config.client_rate.max(1));
                cfg.timeout = timeout;
                Endpoint::client(UdpTransport::for_server(addr)?, addr, cfg)
            }
        };
        Ok(NetPlay::Bound {
            end: Box::new(end),
            options,
        })
    }

    /// The address the socket is bound to.
    pub(crate) fn local_addr(&self) -> Option<SocketAddr> {
        match self {
            NetPlay::Bound { end, .. } => Some(end.local_addr()),
            NetPlay::Server(s) => Some(s.endpoint().local_addr()),
            NetPlay::Client(c) => Some(c.endpoint().local_addr()),
            NetPlay::Closed => None,
        }
    }

    /// The compat role this run plays.
    pub(crate) fn role(&self) -> crate::compat::Role {
        match self {
            NetPlay::Bound { options, .. } => match options.role {
                NetRole::Server => crate::compat::Role::Server,
                NetRole::Host => crate::compat::Role::InProcess,
                NetRole::Client(_) => crate::compat::Role::Client,
            },
            NetPlay::Server(_) => crate::compat::Role::Server,
            NetPlay::Client(_) => crate::compat::Role::Client,
            NetPlay::Closed => crate::compat::Role::InProcess,
        }
    }

    /// Each open connection's counts.
    pub(crate) fn stats(&self) -> Vec<(SocketAddr, re_net::ConnStats)> {
        let end = match self {
            NetPlay::Bound { end, .. } => end,
            NetPlay::Server(s) => s.endpoint(),
            NetPlay::Client(c) => c.endpoint(),
            NetPlay::Closed => return Vec::new(),
        };
        end.peers()
            .into_iter()
            .filter_map(|p| Some((end.addr_of(p)?, end.stats(p)?)))
            .collect()
    }

    /// Leaves or stops serving.
    pub(crate) fn close(&mut self) {
        match self {
            NetPlay::Server(s) => s.close(),
            NetPlay::Client(c) => c.close(),
            _ => {}
        }
    }
}
