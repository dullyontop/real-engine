//! Online play (ADR-0050 §6): the network session around a compat game.

use re_core::{Diagnostic, Location};

use re_script::ContextKind;

use super::App;

impl App {
    /// True unless a network client must wait for its server: for the
    /// welcome before the game starts, or in lockstep for the server's frame.
    /// Polls the connection; never waits.
    pub fn net_ready(&mut self) -> bool {
        self.net.as_mut().is_none_or(crate::net::NetPlay::ready)
    }

    /// Puts a compat game online (ADR-0050 §6): runs the server side or the
    /// client side of the network session that [`App::set_net`] bound, and
    /// starts the scripts in the order the in-process game does.
    ///
    /// - A server binds the scene as loaded, then starts its scripts. In
    ///   lockstep it waits for its network players, then joins the
    ///   `[compat] players` they do not fill, so its game matches the
    ///   in-process one (whose first player is the local one).
    /// - A host starts as the in-process game does, with remote players
    ///   joining after its `[compat] players`.
    /// - A client joins (the server's welcome binds its copy of the scene),
    ///   starts its scripts and sends its frame 0.
    pub(super) fn start_online(&mut self, cfg: &re_compat::CompatConfig) {
        use crate::net::{NetPlay, NetRole};
        let (end, options) = match self.net.take() {
            Some(NetPlay::Bound { end, options }) => (end, options),
            Some(NetPlay::Connecting { net, .. }) => return self.join_online(net),
            other => {
                self.net = other;
                return;
            }
        };
        let lockstep = options.lockstep;
        match options.role {
            NetRole::Server | NetRole::Host => {
                let host = options.role == NetRole::Host;
                let server = re_compat::net::session::ServerNet::new(
                    *end,
                    &mut self.world,
                    cfg.net.clone(),
                    lockstep.is_some(),
                );
                self.net = Some(NetPlay::Server(Box::new(server)));
                let kinds: &[ContextKind] = if host {
                    &[ContextKind::Server, ContextKind::Client]
                } else {
                    &[ContextKind::Server]
                };
                for kind in kinds {
                    self.compat_call(*kind, |vm, h| crate::compat::start_scripts(vm, h));
                }
                if let Some(n) = lockstep.filter(|_| !host) {
                    let mut players = 0;
                    let mut failed = None;
                    let mut net = self.net.take();
                    if let Some(NetPlay::Server(s)) = &mut net {
                        self.compat_call(ContextKind::Server, |vm, h| {
                            match s.wait_for_players(n as usize, vm, h, &mut players) {
                                Ok(d) => d,
                                Err(e) => {
                                    failed = Some(e);
                                    Vec::new()
                                }
                            }
                        });
                    }
                    self.net = net;
                    if let Some(c) = self.compat.as_mut() {
                        c.players = players;
                    }
                    if let Some(d) = failed {
                        self.net_failed(d);
                        return;
                    }
                }
                if host || lockstep.is_some() {
                    let have = self.compat.as_ref().map_or(0, |c| c.players);
                    for _ in have..cfg.players {
                        if let Err(d) = self.join_player() {
                            self.diagnostics.push(d);
                        }
                    }
                }
            }
            NetRole::Client(_) => unreachable!("a client's session connects from set_net"),
        }
    }

    /// Joins a network client's game (ADR-0050 §5): waits for the server's
    /// welcome (which binds the client world), then starts the client's
    /// scripts and sends its frame 0.
    fn join_online(
        &mut self,
        mut client: Box<re_compat::net::session::ClientNet<crate::net::Link>>,
    ) {
        let joined = match self.compat.as_mut() {
            Some(c) => client.join(&mut c.client),
            None => return,
        };
        self.net = Some(crate::net::NetPlay::Client(client));
        if let Err(d) = joined {
            self.net_failed(d);
            return;
        }
        self.compat_call(ContextKind::Client, |vm, h| {
            crate::compat::start_scripts(vm, h)
        });
        // Frame 0: what the client's scripts did as they started.
        self.net_send_client();
    }

    /// Ends the network session after `d` (reported) and stops the run.
    pub(super) fn net_failed(&mut self, d: Diagnostic) {
        if let Some(net) = &mut self.net {
            net.close();
        }
        self.net = Some(crate::net::NetPlay::Closed);
        self.diagnostics.push(d);
        self.stopped = true;
    }

    /// A server's receive, before its remotes are delivered.
    pub(super) fn net_receive_server(&mut self) {
        let mut net = self.net.take();
        let mut failed = None;
        if let Some(crate::net::NetPlay::Server(s)) = &mut net {
            let mut players = self.compat.as_ref().map_or(0, |c| c.players);
            self.compat_call(ContextKind::Server, |vm, h| {
                match s.receive(vm, h, &mut players) {
                    Ok(d) => d,
                    Err(e) => {
                        failed = Some(e);
                        Vec::new()
                    }
                }
            });
            if let Some(c) = self.compat.as_mut() {
                c.players = players;
            }
        }
        self.net = net;
        if let Some(d) = failed {
            self.net_failed(d);
        }
    }

    /// A server's replication to its network players, after the
    /// in-process one.
    pub(super) fn net_replicate(&mut self, dt: f64) {
        if let Some(crate::net::NetPlay::Server(s)) = &mut self.net {
            let diags = s.replicate(&mut self.world, dt);
            self.report_script_errors(diags);
        }
    }

    /// A client's receive: the server's frames into the client world, and
    /// their messages for the client VM.
    pub(super) fn net_receive_client(&mut self) {
        let Some(crate::net::NetPlay::Client(net)) = &mut self.net else {
            return;
        };
        let Some(c) = self.compat.as_mut() else {
            return;
        };
        if let Err(d) = net.receive(&mut c.client) {
            self.net_failed(d);
        }
    }

    /// A client's send: its owned changes and messages, to the server.
    pub(super) fn net_send_client(&mut self) {
        let Some(crate::net::NetPlay::Client(net)) = &mut self.net else {
            return;
        };
        let Some(c) = self.compat.as_mut() else {
            return;
        };
        let diags = net.send(&mut c.client, &c.set);
        self.report_script_errors(diags);
    }

    /// Puts the game online (ADR-0050 §6) before its first frame: binds the
    /// socket now, so a port in use fails here, and starts the session with
    /// the compat game. Returns the address bound.
    ///
    /// # Errors
    ///
    /// The project is not a compat project (`RE8909`), the game already
    /// started, or the socket does not bind (`RE8900`, `RE8901`).
    pub fn set_net(
        &mut self,
        options: crate::net::NetOptions,
    ) -> Result<crate::net::NetBound, Diagnostic> {
        if self.compat_config.is_none() {
            return Err(Diagnostic::error(
                re_net::codes::NOT_ONLINE,
                "online play needs a Roblox-style project ([compat] in real.toml)",
            )
            .at(Location::file(crate::config::CONFIG_FILE)));
        }
        if self.compat.is_some() {
            return Err(Diagnostic::error(
                re_net::codes::NOT_ONLINE,
                "the game already started; go online before its first frame",
            ));
        }
        let (net, bound) = crate::net::NetPlay::bind(options, &self.config.net, &self.files)?;
        self.net = Some(net);
        Ok(bound)
    }

    /// True once the run should end early: a network client was kicked, its
    /// server went away, or a lockstep peer stopped sending.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Each open network connection's counts, by peer: its address, with
    /// `(WebSocket)` after a WebSocket player's, or the server's URL.
    pub fn net_stats(&self) -> Vec<(String, re_net::ConnStats)> {
        self.net.as_ref().map(|n| n.stats()).unwrap_or_default()
    }

    /// Leaves the network game, or stops serving it, at the end of a run.
    pub fn close_net(&mut self) {
        if let Some(net) = &mut self.net {
            net.close();
        }
    }
}
