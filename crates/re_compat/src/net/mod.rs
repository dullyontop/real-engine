//! Two worlds in one process: the server and the client, side by side, the
//! way a play test runs them ([`CompatWorlds`]).
//!
//! * The **server world** is authoritative: physics runs there, its scripts
//!   run in the `server` VM, and it is what is saved.
//! * The **client world** is the local player's replica: the replicated
//!   containers ([`ReplicationSet::containers`]) are copied from the server
//!   each frame by [`replicate`], and client-only instances (`PlayerGui`,
//!   `PlayerScripts`, whatever client scripts create) live only there. Its
//!   scripts run in the `client` VM, and rendering and input use it.
//! * An instance is the same instance in both worlds when it has the same
//!   stable id. Entities are never compared across worlds.
//! * The two VMs talk through remotes ([`remotes`]): messages queue in the
//!   [`NetLink`] both worlds share and are delivered by [`pump`].
//! * Both directions go through the wire forms a network uses ([`wire`]):
//!   replication is a server half and a client half that exchange encoded
//!   frames ([`Conn`], [`Replica`]), and remote messages queue as their
//!   encoded bytes. So the in-process run exercises the network code.
//!
//! A frame of a two-world run is:
//!
//! ```text
//! pump(server VM)    remotes from the client; queued character loads;
//!                    scripts that became startable
//! server phases      PreRender .. PostSimulation (scripts, physics)
//! replicate          owned components client → server, then server → client
//! pump(client VM)    player and character events (removals included),
//!                    PlayerScripts and PlayerGui, removals, remotes from
//!                    the server, prompts; scripts that became startable
//! client phases      PreRender .. PostSimulation, then render from the client
//! ```
//!
//! In-process, there is one client world, the local player's. Other players
//! are simulated on the server only (`players::add_player`).

pub mod remotes;
mod replicate;
pub mod session;
pub mod value;
pub mod wire;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use mlua::{Function, Lua, MultiValue, Result};
use re_core::Diagnostic;
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::prelude::Resource;
use re_scene::{Entity, IdGen, World, hierarchy};
use re_script::{ScriptHost, sched};

pub use replicate::{
    Conn, NetworkOwner, Replica, ReplicationReport, ServerReplica, UPSTREAM_ALL, UpCheck, UpReport,
    network_owner, replicate, set_network_owner,
};

use crate::error::CompatError;
use crate::instance::tree;
use crate::runtime::{Bindings, CompatSetup, CompatVm, state, with_world};
use crate::scene::ServiceTag;
use crate::{GAME_ID, ROOT_CLASS};
use value::NetValue;

/// The seed of the client world's id generator, so a seeded run makes the same
/// client ids every time (and never the server's: a clash is resolved anyway).
const CLIENT_ID_SEED: u64 = 0x636c_6965_6e74_0001;

/// What replicates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicationSet {
    /// The service classes whose subtrees replicate from server to client.
    /// Services the place lacks are skipped. `ServerStorage` and
    /// `ServerScriptService` never belong here.
    pub containers: Vec<String>,
    /// The components a client owns for the entities its player is the
    /// network owner of ([`set_network_owner`]; `LoadCharacter` makes the
    /// player own its whole character): the client's changes to them flow to
    /// the server, and the server's own changes still reach the client.
    /// [`UPSTREAM_ALL`] (`"*"`, the default) means every component: own and
    /// aliased properties, attributes and tags. Children a client creates
    /// never flow up.
    pub upstream: Vec<String>,
}

impl Default for ReplicationSet {
    fn default() -> ReplicationSet {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
        ReplicationSet {
            containers: s(&[
                "Workspace",
                "ReplicatedStorage",
                "ReplicatedFirst",
                "Players",
                "Lighting",
                "StarterGui",
                "StarterPlayer",
                "StarterPack",
                "SoundService",
                "TextChatService",
                "Teams",
            ]),
            upstream: s(&[UPSTREAM_ALL]),
        }
    }
}

/// What an owner may change in a networked game unless `[compat.net]
/// upstream` says otherwise (ADR-0050 §2): a part's position and
/// orientation, the joints its animations pose, the `Humanoid`'s movement
/// input, and its animation tracks. A `Component.field` entry lets only that
/// field through.
pub const NETWORK_UPSTREAM: &[&str] = &[
    "Transform",
    "Compat_Motor6D",
    "Compat_AnimationTrack",
    "Compat_Humanoid.MoveDirection",
    "Compat_Humanoid.Jump",
    "Compat_Humanoid.Sit",
    "Compat_Humanoid.PlatformStand",
];

/// The `[compat.net]` section of `real.toml`: what a networked game lets
/// its clients do (ADR-0050 §2). The in-process two-world run ignores it.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(default, deny_unknown_fields)]
pub struct NetSettings {
    /// The components (or `Component.field`s) an owner's changes send to the
    /// server; [`NETWORK_UPSTREAM`] when unset, `["*"]` for everything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<Vec<String>>,
    /// The fastest an owned part may move, in studs per second (`RE9452`).
    pub max_owned_speed: u32,
    /// The remote messages a player may send a second, all remotes together
    /// (`RE9453`).
    pub remote_rate: u32,
}

impl Default for NetSettings {
    fn default() -> NetSettings {
        NetSettings {
            upstream: None,
            max_owned_speed: 1000,
            remote_rate: 600,
        }
    }
}

impl NetSettings {
    /// True when nothing is set.
    pub fn is_default(&self) -> bool {
        *self == NetSettings::default()
    }

    /// The replication set of a networked game: the default containers,
    /// and [`NetSettings::upstream`].
    pub fn replication_set(&self) -> ReplicationSet {
        ReplicationSet {
            upstream: self
                .upstream
                .clone()
                .unwrap_or_else(|| NETWORK_UPSTREAM.iter().map(|s| s.to_string()).collect()),
            ..ReplicationSet::default()
        }
    }
}

/// Which side of the two worlds a world is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The authoritative world.
    Server,
    /// The local player's replica.
    Client,
}

/// A message between the two VMs.
#[derive(Clone, Debug)]
pub(crate) enum Msg {
    /// A remote event: `FireServer` (with the sender), `FireClient` or
    /// `FireAllClients`. An `UnreliableRemoteEvent`'s crosses a network
    /// on the unreliable channel.
    Event {
        remote: String,
        player: Option<String>,
        args: Vec<NetValue>,
        unreliable: bool,
    },
    /// A remote function call, answered by a [`Msg::Reply`] with `call`.
    Invoke {
        remote: String,
        player: Option<String>,
        call: u64,
        args: Vec<NetValue>,
    },
    /// The results of an invocation, or its error message. On the server,
    /// `player` is the client it came from (set by the network session; an
    /// in-process client's is `None`), which must be the one invoked.
    Reply {
        call: u64,
        player: Option<String>,
        result: std::result::Result<Vec<NetValue>, String>,
    },
    /// A proximity prompt action of the local player, client to server.
    Prompt {
        prompt: String,
        player: String,
        action: PromptAction,
    },
    /// A chat message a player sent, server to client.
    Chat { player: String, text: String },
    /// The local player kicked itself (`Player:Kick` on the client).
    Kick { player: String },
}

/// What a player did with a proximity prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum PromptAction {
    HoldBegan,
    HoldEnded,
    Triggered,
    TriggerEnded,
}

/// Messages on their way to one side, as their wire bytes
/// ([`wire::encode_msg`]), so the in-process run sends what a network would;
/// each is marked when it goes on the unreliable channel.
#[derive(Debug, Default)]
pub(crate) struct MsgQueue(VecDeque<(bool, Vec<u8>)>);

impl MsgQueue {
    /// Queues `m`.
    pub(crate) fn push_back(&mut self, m: Msg) {
        let unreliable = matches!(
            m,
            Msg::Event {
                unreliable: true,
                ..
            }
        );
        self.0.push_back((unreliable, wire::encode_msg(&m)));
    }

    /// Takes every queued message, in order.
    pub(crate) fn take_all(&mut self) -> Vec<Msg> {
        self.0
            .drain(..)
            .map(|(_, b)| wire::decode_msg(&b).expect("a message this build encoded decodes"))
            .collect()
    }

    /// Takes every queued message as its bytes, with whether it is
    /// unreliable, in order.
    pub(crate) fn take_bytes(&mut self) -> Vec<(bool, Vec<u8>)> {
        self.0.drain(..).collect()
    }
}

/// The state both worlds share.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    /// Messages for the server: from the in-process client, or from the
    /// network session's clients.
    pub(crate) to_server: MsgQueue,
    /// Messages for the client: the in-process client's, or, in a client
    /// process, the ones the server sent.
    pub(crate) to_client: MsgQueue,
    /// The stable id of the local player's `Player`.
    pub(crate) local_player: Option<String>,
    /// On a server: the messages for each player connected over the
    /// network, by the stable id of its `Player`.
    pub(crate) remote: BTreeMap<String, MsgQueue>,
    /// On a server: network players a script kicked, with the message.
    pub(crate) kicks: Vec<(String, String)>,
    pub(crate) next_call: u64,
    /// Messages for players that have no client in this process.
    pub(crate) undelivered: u64,
}

impl Shared {
    /// The queue of `player`'s client: the in-process client's or a network
    /// player's; `None` for a player simulated on the server only.
    pub(crate) fn client_queue(&mut self, player: &str) -> Option<&mut MsgQueue> {
        if self.local_player.as_deref() == Some(player) {
            Some(&mut self.to_client)
        } else {
            self.remote.get_mut(player)
        }
    }

    /// Queues `m` for `player`'s client, or counts it undelivered when that
    /// player has none.
    pub(crate) fn send_to(&mut self, player: &str, m: Msg) {
        match self.client_queue(player) {
            Some(q) => q.push_back(m),
            None => self.undelivered += 1,
        }
    }

    /// Queues `m` for every client: the in-process one and the network
    /// players'.
    pub(crate) fn send_to_all(&mut self, m: Msg) {
        if self.local_player.is_some() {
            self.to_client.push_back(m.clone());
        }
        for q in self.remote.values_mut() {
            q.push_back(m.clone());
        }
    }
}

/// The link between the two worlds, a resource of each: which side the world
/// is, and the queues and local player they share.
#[derive(Resource, Clone, Debug)]
pub struct NetLink {
    /// This world's side.
    pub side: Side,
    shared: Arc<Mutex<Shared>>,
}

impl NetLink {
    pub(crate) fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The stable id of the local player's `Player`, if one has joined.
    pub fn local_player(&self) -> Option<String> {
        self.lock().local_player.clone()
    }

    /// How many messages were addressed to players without a client here.
    pub fn undelivered(&self) -> u64 {
        self.lock().undelivered
    }
}

/// Makes `client` (from [`client_world`]) the world of a network client:
/// its own link, and an empty replica the server's welcome binds.
pub fn link_client(client: &mut World) {
    client.ecs.insert_resource(NetLink {
        side: Side::Client,
        shared: Arc::new(Mutex::new(Shared::default())),
    });
    client.ecs.insert_resource(Replica::default());
}

/// The link of the VM's world, if it is one of two worlds.
pub(crate) fn link(lua: &Lua) -> Result<Option<NetLink>> {
    with_world(lua, |w| w.ecs.get_resource::<NetLink>().cloned())
}

/// The server and the client world, and what replicates between them.
pub struct CompatWorlds {
    /// The authoritative world.
    pub server: World,
    /// The local player's replica.
    pub client: World,
    /// What replicates.
    pub set: ReplicationSet,
}

impl std::fmt::Debug for CompatWorlds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompatWorlds")
            .field("set", &self.set)
            .finish_non_exhaustive()
    }
}

impl CompatWorlds {
    /// Makes the client world from `server`, which holds a compat scene,
    /// with the default [`ReplicationSet`].
    ///
    /// # Errors
    ///
    /// The server world has no `game` root, or the class layer's components
    /// do not register (`RE9004`, `RE9003`).
    pub fn new(
        setup: &CompatSetup,
        server: World,
    ) -> std::result::Result<CompatWorlds, CompatError> {
        CompatWorlds::with_set(setup, server, ReplicationSet::default())
    }

    /// Makes the client world from `server` with replication set `set`. The
    /// client starts as a copy of the server (same stable ids) in which the
    /// services outside `set` are empty; the services themselves stay, as
    /// they do on a Roblox client.
    pub fn with_set(
        setup: &CompatSetup,
        mut server: World,
        set: ReplicationSet,
    ) -> std::result::Result<CompatWorlds, CompatError> {
        let mut client = client_world(setup, &mut server, &set)?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        server.ecs.insert_resource(NetLink {
            side: Side::Server,
            shared: shared.clone(),
        });
        client.ecs.insert_resource(NetLink {
            side: Side::Client,
            shared,
        });
        replicate::init(&mut server, &mut client, &set);
        Ok(CompatWorlds {
            server,
            client,
            set,
        })
    }

    /// One replication pass ([`replicate`]).
    pub fn replicate(&mut self) -> ReplicationReport {
        replicate(&mut self.server, &mut self.client, &self.set)
    }

    /// The link the two worlds share.
    pub fn link(&self) -> NetLink {
        self.server
            .ecs
            .get_resource::<NetLink>()
            .cloned()
            .expect("a CompatWorlds server has a link")
    }

    /// The local player's `Player` in the server world.
    pub fn local_player(&self) -> Option<Entity> {
        let id = self.link().local_player()?;
        self.server.entity(&id).ok()
    }

    /// The client entity of a server entity (same stable id), if the client
    /// has it.
    pub fn client_of(&self, server_entity: Entity) -> Option<Entity> {
        let id = self.server.id_of(server_entity)?;
        self.client.entity(id).ok()
    }

    /// The server entity of a client entity (same stable id), if the server
    /// has it.
    pub fn server_of(&self, client_entity: Entity) -> Option<Entity> {
        let id = self.client.id_of(client_entity)?;
        self.server.entity(id).ok()
    }
}

/// The client world a play starts from: a copy of `server` (same stable
/// ids) in which the services outside `set` are empty; the services
/// themselves stay, as they do on a Roblox client. A network client makes it
/// from its own copy of the scene, which the server's welcome then binds
/// ([`session::ClientNet::join`]). Registers the class layer's components
/// in `server` first.
///
/// # Errors
///
/// `server` has no `game` root, or the class layer's components do not
/// register (`RE9004`, `RE9003`).
pub fn client_world(
    setup: &CompatSetup,
    server: &mut World,
    set: &ReplicationSet,
) -> std::result::Result<World, CompatError> {
    setup.classes.register_components(server)?;
    server
        .entity(GAME_ID)
        .ok()
        .filter(|g| tree::class(server, *g) == Some(ROOT_CLASS))
        .ok_or_else(|| {
            CompatError::Scene(format!(
                "the server world has no {ROOT_CLASS} root with id {GAME_ID:?}"
            ))
        })?;
    let mut client = server.fork();
    let game_c = client.entity(GAME_ID).expect("the fork keeps ids");
    for child in hierarchy::children(&client.ecs, game_c).to_vec() {
        let keep = client.ecs.get::<ServiceTag>(child).is_some();
        let replicated = keep
            && tree::class(&client, child).is_some_and(|c| set.containers.iter().any(|x| x == c));
        if !keep {
            let _ = client.despawn_runtime(child);
        } else if !replicated {
            for c in hierarchy::children(&client.ecs, child).to_vec() {
                let _ = client.despawn_runtime(c);
            }
        }
    }
    client.ecs.insert_resource(IdGen::seeded(CLIENT_ID_SEED));
    Ok(client)
}

/// Delivers what arrived for `vm` and runs the per-frame work of its side
/// (see the module docs for where it sits in a frame). A VM whose world is
/// not one of two worlds has nothing to pump. Returns the diagnostics of the
/// errors raised meanwhile (handlers included).
pub fn pump(vm: &CompatVm, host: &mut dyn ScriptHost) -> Vec<Diagnostic> {
    let mut out = vm.run(host, pump_lua);
    // Scripts disabled by a write the VM did not make (replicated, or from
    // the editor) stop; the ones that became startable since the last pump
    // (copied to a player or a character, cloned into the tree, enabled
    // again) start now, each on its own.
    let mut new = Vec::new();
    out.extend(vm.run(host, |lua| {
        if link(lua)?.is_some() {
            for e in crate::runtime::scripts::disabled_running(lua)? {
                crate::runtime::scripts::stop(lua, e)?;
            }
            new = crate::runtime::scripts::unstarted(lua)?;
        }
        Ok(())
    }));
    for e in new {
        out.extend(vm.start_script(host, e));
    }
    out
}

fn pump_lua(lua: &Lua) -> Result<()> {
    let Some(link) = link(lua)? else {
        return Ok(());
    };
    match link.side {
        Side::Server => {
            crate::players::server_frame(lua)?;
            remotes::deliver(lua, &link)?;
        }
        Side::Client => {
            crate::players::client_frame(lua, &link)?;
            destroy_removed(lua)?;
            remotes::deliver(lua, &link)?;
            crate::services::extra::prompts::client_frame(lua, &link)?;
        }
    }
    Ok(())
}

/// Destroys, in the client VM, the subtrees the server removed, as
/// `Destroy` would (`Destroying` fires, the handles keep their name and
/// class), after the handlers already queued this pump.
fn destroy_removed(lua: &Lua) -> Result<()> {
    let roots = with_world(lua, replicate::take_removed)?;
    for root in roots {
        crate::players::destroy_after_handlers(lua, root)?;
    }
    Ok(())
}

/// The client instances whose removal by the server is pending this pump.
pub(crate) fn removing(lua: &Lua) -> Result<std::collections::HashSet<Entity>> {
    with_world(lua, |w| replicate::removing(w))
}

/// Registry key of the function that sleeps through a park with a timeout.
const SLEEP_KEY: &str = "re_compat.net.sleep";
/// Registry key of the Luau runner of [`after`].
const AFTER_KEY: &str = "re_compat.net.after";

/// Runs `f` once `secs` seconds of simulation time have passed, in a script
/// thread labelled `label` (it shows in `parked()` while it waits).
pub(crate) fn after(lua: &Lua, secs: f64, label: &str, f: Function) -> Result<()> {
    let runner: Function = match lua.named_registry_value::<Option<Function>>(AFTER_KEY)? {
        Some(r) => r,
        None => {
            let raw = lua.create_function(|lua, (label, secs): (String, f64)| {
                let (values, _) = sched::park(lua, &label, Some((secs, MultiValue::new())))?;
                Ok(values)
            })?;
            let sleep = sched::yielding(lua, "sleep", raw)?;
            lua.set_named_registry_value(SLEEP_KEY, &sleep)?;
            let r: Function = lua
                .load(
                    "local sleep = ...\nreturn function(label, secs, f) sleep(label, secs) f() end",
                )
                .set_name("=after")
                .call(sleep)?;
            lua.set_named_registry_value(AFTER_KEY, &r)?;
            r
        }
    };
    sched::spawn(
        lua,
        runner,
        MultiValue::from_iter([
            mlua::Value::String(lua.create_string(label)?),
            mlua::Value::Number(secs),
            mlua::Value::Function(f),
        ]),
        label,
    )?;
    Ok(())
}

/// True when the VM runs on the server side (or alone, with no client).
pub(crate) fn is_server(lua: &Lua) -> Result<bool> {
    Ok(state(lua)?.kind == re_script::ContextKind::Server)
}

/// Registers the remotes' methods and callbacks.
pub fn register(b: &mut Bindings) {
    remotes::register(b);
}

/// Calls `BindableFunction` `entity`'s `OnInvoke` in `vm` from the host (a
/// harness's `invoke` step), with the arguments of JSON array `args_json`,
/// and returns its results as a JSON array. Fails with `RE9301` when the
/// entity is not a `BindableFunction`, the arguments are not a JSON array,
/// the handler raises or is not set, or it yields past the end of the call.
/// The diagnostics are those of other errors raised meanwhile.
pub fn invoke_bindable(
    vm: &CompatVm,
    host: &mut dyn ScriptHost,
    entity: Entity,
    args_json: &str,
) -> (std::result::Result<String, Diagnostic>, Vec<Diagnostic>) {
    let fail = |m: String| Diagnostic::error(crate::codes::REMOTE_CALL, format!("invoke: {m}"));
    let mut out = Err(fail("the call did not run".into()));
    let diags = vm.run(host, |lua| {
        let is_bindable = with_world(lua, |w| {
            tree::alive(w, entity) && tree::class(w, entity) == Some("BindableFunction")
        })?;
        if !is_bindable {
            out = Err(fail("the entity is not a BindableFunction".into()));
            return Ok(());
        }
        let args: Vec<serde_json::Value> = match serde_json::from_str(args_json) {
            Ok(serde_json::Value::Array(a)) => a,
            Ok(_) => {
                out = Err(fail("the arguments must be a JSON array".into()));
                return Ok(());
            }
            Err(e) => {
                out = Err(fail(format!("the arguments are not JSON: {e}")));
                return Ok(());
            }
        };
        let mut values = MultiValue::new();
        for a in args {
            values.push_back(crate::services::extra::from_json(lua, a)?);
        }
        out = match remotes::invoke_bindable_now(lua, entity, values)? {
            Ok(results) => {
                let mut list = Vec::new();
                for r in results {
                    match crate::services::extra::to_json(r, 0, &mut Vec::new()) {
                        Ok(j) => list.push(j),
                        Err(e) => {
                            out = Err(fail(format!("a result has no JSON form: {e}")));
                            return Ok(());
                        }
                    }
                }
                Ok(serde_json::Value::Array(list).to_string())
            }
            Err(e) => Err(fail(e)),
        };
        Ok(())
    });
    (out, diags)
}
