//! Online play for compat projects (ADR-0050 §6): `real run --server`,
//! `--host` and `--connect`, through `re_compat::net::session`. A server
//! takes players over UDP and, unless `[net] websocket = false`, over a
//! WebSocket on TCP at the same port, which is how a browser plays (ADR-0050's
//! amendment for PR "web"). A client connects over UDP to `host:port`, or over
//! a WebSocket to a `ws://` address; a browser always uses a WebSocket.
//!
//! [`App::set_net`](crate::App::set_net) binds the socket before the first
//! frame. A client's session connects from then on (a page polls it before the
//! first frame, [`App::try_frame`](crate::App::try_frame)); a server's starts
//! with the compat game ([`NetPlay`]). In a frame:
//!
//! ```text
//! server: receive  pump(server)  server phases  [in-process replicate]  replicate (sends)
//! client:                                          receive  pump(client)  client phases  send
//! ```

use std::net::SocketAddr;
use std::time::Duration;

use re_compat::net::session::{ClientNet, PROTOCOL, ServerNet};
use re_core::Diagnostic;
use re_core::files::{Form, LayerKind, ProjectFiles};
use re_net::{Config, Endpoint, Transport, UdpTransport, WsTransport};
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
    /// Whether a server also takes players over a WebSocket (browsers), on
    /// TCP at its port.
    pub websocket: bool,
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
            websocket: true,
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
    /// A client of the server at this address: `host:port` (or `host`, on
    /// the configured port) over UDP, or `ws://host:port/` over a WebSocket.
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

/// What [`App::set_net`](crate::App::set_net) bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetBound {
    /// The address of the UDP socket, or a WebSocket client's own.
    pub addr: SocketAddr,
    /// Where a server takes WebSocket players (TCP), when it does.
    pub websocket: Option<SocketAddr>,
}

/// The hash a client and its server must share (`RE8905`): the game's
/// `real.toml`, scenes and scripts as the run reads them (ADR-0050 §5), from
/// a project directory or a packaged game's archive. Each file counts with
/// its path, how it is stored (source, bytecode or processed) and its bytes,
/// so a browser and a desktop server that play one archive agree. Left out:
/// what the engine provides itself, the folder beside a packaged game, and
/// files under `.real/` (data, caches), `tests/` (replays, goldens) and
/// `.git/`.
pub fn game_hash(files: &ProjectFiles) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(b"real-engine/game-hash/2");
    let mut paths: Vec<String> = [".toml", ".json", ".luau", ".lua"]
        .iter()
        .flat_map(|s| files.list("", s))
        .map(|p| p.as_str().to_string())
        .filter(|p| {
            ![".real/", "tests/", ".git/"]
                .iter()
                .any(|d| p.starts_with(d))
        })
        .collect();
    paths.sort();
    paths.dedup();
    for p in paths {
        let Ok(data) = files.read(&p) else {
            continue;
        };
        if matches!(data.layer, LayerKind::Engine | LayerKind::Folder) {
            continue;
        }
        let form: u8 = match data.form {
            Form::Source => 0,
            Form::Bytecode { .. } => 1,
            Form::Processed { .. } => 2,
            _ => 3,
        };
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
        h.update(&[form]);
        h.update(&(data.bytes.len() as u64).to_le_bytes());
        h.update(&data.bytes);
    }
    let out = h.finalize();
    u64::from_le_bytes(out.as_bytes()[..8].try_into().expect("8 bytes"))
}

/// A run's transport: a client's UDP socket or WebSocket, or a server's UDP
/// socket and WebSocket listener.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type Link = Box<dyn Transport + Send>;
/// A run's transport: a browser's WebSocket.
#[cfg(target_arch = "wasm32")]
pub(crate) type Link = Box<dyn Transport>;

/// A run's network side.
pub(crate) enum NetPlay {
    /// A server's socket, bound, waiting for the game to start.
    Bound {
        end: Box<Endpoint<Link>>,
        options: NetOptions,
    },
    /// A client's session, connecting until the game starts and joins.
    Connecting {
        net: Box<ClientNet<Link>>,
        options: NetOptions,
    },
    /// Serving (dedicated or host).
    Server(Box<ServerNet<Link>>),
    /// Playing on a server.
    Client(Box<ClientNet<Link>>),
    /// Over: failed, kicked or the server went away.
    Closed,
}

impl NetPlay {
    /// Binds the socket for `options` (a server's listening socket and
    /// WebSocket listener, or a client's connection to its server); the game
    /// hash names the project.
    ///
    /// # Errors
    ///
    /// `RE8900` the socket does not bind, `RE8901` the address does not
    /// resolve, `RE8910` a WebSocket does not open, and `RE8911` a browser
    /// asked to serve.
    pub(crate) fn bind(
        options: NetOptions,
        config: &NetConfig,
        files: &ProjectFiles,
    ) -> Result<(NetPlay, NetBound), Diagnostic> {
        let game = game_hash(files);
        let timeout = Duration::from_millis(config.timeout_ms.max(1));
        let port = options.port.unwrap_or(config.port);
        match &options.role {
            NetRole::Server | NetRole::Host => {
                let max_players = options.max_players.unwrap_or(config.max_players);
                let ip = options.bind.as_deref().unwrap_or("0.0.0.0");
                let (transport, websocket) = serve(ip, port, config.websocket, max_players)?;
                let mut cfg = Config::server(PROTOCOL, game);
                cfg.send_rate = f64::from(config.server_rate.max(1));
                cfg.max_connections = max_players as usize;
                cfg.timeout = timeout;
                let end = Endpoint::server(transport, cfg);
                let bound = NetBound {
                    addr: end.local_addr(),
                    websocket,
                };
                let play = NetPlay::Bound {
                    end: Box::new(end),
                    options,
                };
                Ok((play, bound))
            }
            NetRole::Client(server) => {
                let (transport, addr) = connect(server, port, timeout)?;
                let mut cfg = Config::client(PROTOCOL, game);
                cfg.send_rate = f64::from(config.client_rate.max(1));
                cfg.timeout = timeout;
                let end = Endpoint::client(transport, addr, cfg);
                let bound = NetBound {
                    addr: end.local_addr(),
                    websocket: None,
                };
                let net = ClientNet::new(end, options.name.clone(), options.lockstep.is_some());
                let play = NetPlay::Connecting {
                    net: Box::new(net),
                    options,
                };
                Ok((play, bound))
            }
        }
    }

    /// The compat role this run plays.
    pub(crate) fn role(&self) -> crate::compat::Role {
        match self {
            NetPlay::Bound { options, .. } | NetPlay::Connecting { options, .. } => {
                match options.role {
                    NetRole::Server => crate::compat::Role::Server,
                    NetRole::Host => crate::compat::Role::InProcess,
                    NetRole::Client(_) => crate::compat::Role::Client,
                }
            }
            NetPlay::Server(_) => crate::compat::Role::Server,
            NetPlay::Client(_) => crate::compat::Role::Client,
            NetPlay::Closed => crate::compat::Role::InProcess,
        }
    }

    /// True unless a client must wait for its server: before the game
    /// starts, for the welcome; in lockstep, for the server's frame
    /// ([`ClientNet::ready`]).
    pub(crate) fn ready(&mut self) -> bool {
        match self {
            NetPlay::Connecting { net, .. } | NetPlay::Client(net) => net.ready(),
            NetPlay::Bound { .. } | NetPlay::Server(_) | NetPlay::Closed => true,
        }
    }

    /// Each open connection's counts, by peer: its address, with
    /// `(WebSocket)` after a WebSocket player's, or the server's URL.
    pub(crate) fn stats(&self) -> Vec<(String, re_net::ConnStats)> {
        fn of<T: Transport>(end: &Endpoint<T>) -> Vec<(String, re_net::ConnStats)> {
            end.peers()
                .into_iter()
                .filter_map(|p| Some((end.describe(p)?, end.stats(p)?)))
                .collect()
        }
        match self {
            NetPlay::Bound { end, .. } => of(end),
            NetPlay::Connecting { net, .. } | NetPlay::Client(net) => of(net.endpoint()),
            NetPlay::Server(s) => of(s.endpoint()),
            NetPlay::Closed => Vec::new(),
        }
    }

    /// Leaves or stops serving.
    pub(crate) fn close(&mut self) {
        match self {
            NetPlay::Server(s) => s.close(),
            NetPlay::Connecting { net, .. } | NetPlay::Client(net) => net.close(),
            NetPlay::Bound { .. } | NetPlay::Closed => {}
        }
    }
}

/// A server's transport on `ip:port`: UDP, and a WebSocket listener on TCP at
/// the same port when `websocket` (for twice the players, so a page that
/// reloads can join while its old link times out).
#[cfg(not(target_arch = "wasm32"))]
fn serve(
    ip: &str,
    port: u16,
    websocket: bool,
    max_players: u32,
) -> Result<(Link, Option<SocketAddr>), Diagnostic> {
    let addr = re_net::resolve(ip, port)?;
    let addr = SocketAddr::new(addr.ip(), port);
    if !websocket {
        return Ok((Box::new(re_net::ServerTransport::udp(addr)?), None));
    }
    let links = 2 * max_players as usize + 16;
    let transport = re_net::ServerTransport::with_websocket(addr, links)?;
    let at = transport.websocket_addr();
    Ok((Box::new(transport), at))
}

/// A browser cannot listen for players (ADR-0049 §10).
#[cfg(target_arch = "wasm32")]
fn serve(
    _ip: &str,
    _port: u16,
    _websocket: bool,
    _max_players: u32,
) -> Result<(Link, Option<SocketAddr>), Diagnostic> {
    Err(Diagnostic::error(
        re_net::codes::CANNOT_SERVE,
        "a game in a browser cannot serve: it can only join a server",
    )
    .with_hint(
        "run the server on a desktop: `real run --server` in the project, or `real run --package DIR --server` for a built game",
    ))
}

/// A client's transport to `server` and the address its endpoint reaches the
/// server at: a WebSocket for a `ws://` address (and always in a browser),
/// else UDP to `host:port` (`port` when it has none).
fn connect(server: &str, port: u16, timeout: Duration) -> Result<(Link, SocketAddr), Diagnostic> {
    if cfg!(target_arch = "wasm32") || re_net::ws::is_websocket_url(server) {
        let ws = WsTransport::connect(&re_net::ws::websocket_url(server, port), timeout)?;
        let addr = ws.server_addr();
        return Ok((Box::new(ws), addr));
    }
    let addr = re_net::resolve(server, port)?;
    Ok((Box::new(UdpTransport::for_server(addr)?), addr))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use re_core::files::{MemLayer, ProjectPath, WritableDirs};

    use super::*;

    fn layer(kind: LayerKind, files: &[(&str, &[u8], Form)]) -> Arc<MemLayer> {
        let mut l = MemLayer::new(kind);
        for (path, bytes, form) in files {
            l.insert(
                ProjectPath::new(path).unwrap(),
                bytes.to_vec(),
                form.clone(),
            );
        }
        Arc::new(l)
    }

    fn hash(layers: Vec<Arc<MemLayer>>) -> u64 {
        let layers = layers
            .into_iter()
            .map(|l| l as Arc<dyn re_core::files::FileLayer>)
            .collect();
        game_hash(&ProjectFiles::new(layers, WritableDirs::memory(), None))
    }

    #[test]
    fn the_game_hash_counts_what_the_game_runs_and_nothing_else() {
        let bytecode = Form::Bytecode {
            compiler: "test".into(),
        };
        let game: &[(&str, &[u8], Form)] = &[
            ("real.toml", b"[project]", Form::Source),
            ("scenes/main.scene.json", b"{}", Form::Source),
            ("scripts/a.luau", b"\x1bbytecode", bytecode.clone()),
        ];
        let base = hash(vec![layer(LayerKind::Archive, game)]);
        // One archive, in a browser or beside a desktop game's folder.
        let folder = layer(
            LayerKind::Folder,
            &[("scripts/mod.luau", b"x", Form::Source)],
        );
        let engine = layer(LayerKind::Engine, &[("engine.toml", b"x", Form::Source)]);
        let tests = layer(
            LayerKind::Archive,
            &[("tests/replays/walk.json", b"{}", Form::Source)],
        );
        assert_eq!(
            hash(vec![engine, layer(LayerKind::Archive, game), folder]),
            base
        );
        assert_eq!(hash(vec![layer(LayerKind::Archive, game), tests]), base);
        // A script's bytecode counts, and so does how a file is stored.
        let changed: &[(&str, &[u8], Form)] = &[
            ("real.toml", b"[project]", Form::Source),
            ("scenes/main.scene.json", b"{}", Form::Source),
            ("scripts/a.luau", b"\x1bbytecode2", bytecode),
        ];
        assert_ne!(hash(vec![layer(LayerKind::Archive, changed)]), base);
        let as_source: &[(&str, &[u8], Form)] = &[
            ("real.toml", b"[project]", Form::Source),
            ("scenes/main.scene.json", b"{}", Form::Source),
            ("scripts/a.luau", b"\x1bbytecode", Form::Source),
        ];
        assert_ne!(hash(vec![layer(LayerKind::Archive, as_source)]), base);
    }
}
