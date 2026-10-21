//! The replication pass, split in two halves that talk only through wire
//! [`Frame`]s (ADR-0050 §3): server world → client world, plus the
//! components a client owns, client → server.
//!
//! * The **server** keeps, per connection, a mirror of what that client was
//!   sent ([`Conn`]): for each replicated entity, its handle and the values
//!   last sent (its name, parent and every registry component).
//! * The **client** keeps its own mirror ([`Replica`], a resource of the
//!   client world): for each handle, its client entity and the values it
//!   last received or sent, so a change can travel as just the fields that
//!   changed.
//!
//! A pass is:
//! 1. **Up** ([`Replica::diff_up`], on the client). For each instance the
//!    server said the client owns ([`Op::Own`]), the
//!    [`ReplicationSet::upstream`] components the client changed since the
//!    last pass travel as [`Op::Up`]. The server ([`Conn::apply_up`]) writes
//!    them to instances its player still owns, merged against what the
//!    server last sent ([`write_changes`]).
//! 2. **Walk** ([`Conn::diff`], on the server). The replicated containers'
//!    subtrees are walked in tree order. An entity the mirror lacks is new;
//!    one whose components changed since the last pass (bevy change ticks; a
//!    move or a rename changes `ChildOf` or `Name`) is diffed against the
//!    mirror.
//! 3. **Create**, parents first: same stable id, name and parent, then every
//!    registry component, with entity references as handles (a reference to
//!    an instance the client was not sent reads as `nil`, and is sent again
//!    once it resolves).
//! 4. **Diff.** Only what the server changed since the last pass is sent,
//!    and the client merges it per field, per attribute key and per tag
//!    ([`Replica::apply`]). This is the client-local write rule: a client's
//!    own write to a replicated instance stays until the server next changes
//!    that same field.
//! 5. **Own and remove.** The owned set is sent as changes. A mirrored
//!    entity the walk did not reach (destroyed, or moved under a container
//!    that does not replicate) is taken out of the client tree, locked as
//!    destroyed and queued; the client VM's pump fires `Destroying` and
//!    despawns it (`net::pump`), or the next pass despawns it when no VM
//!    runs.
//!
//! [`replicate`] runs both halves in one process, through the wire encoding,
//! for the in-process two-world run. Entities without a stable id are
//! runtime-only and are not replicated.

use std::collections::{BTreeSet, HashMap, HashSet};

use re_core::Diagnostic;
use re_reflect::{ComponentKey, FieldType, Value};
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::change_detection::Tick;
use re_scene::ecs::component::Component;
use re_scene::ecs::prelude::Resource;
use re_scene::{Entity, Name, StableId, World, hierarchy};

use super::wire::{Frame, NameTable, Names, Op, Ref, WireValue, from_wire, to_wire};
use super::{NetLink, ReplicationSet};
use crate::codes;
use crate::instance::tree;
use crate::scene::ServiceTag;
use crate::{GAME_ID, ROOT_CLASS};

/// Marks a server entity as simulated by a player's client: the
/// [`ReplicationSet::upstream`] components flow from that client to the
/// server. A plain component of the server world: not reflected, never saved.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkOwner(pub Entity);

/// Records `player` (a server `Player` instance) as the network owner of
/// `entity` and its descendants (present and future, unless one has its own
/// owner) in the server world; `None` gives it back to the server.
pub fn set_network_owner(server: &mut World, entity: Entity, player: Option<Entity>) {
    let Ok(mut e) = server.ecs.get_entity_mut(entity) else {
        return;
    };
    match player {
        Some(p) => {
            e.insert(NetworkOwner(p));
        }
        None => {
            e.remove::<NetworkOwner>();
        }
    }
}

/// Gives everything `player` owned back to the server (its connection
/// closed).
pub(crate) fn disown(server: &mut World, player: Entity) {
    let owned: Vec<Entity> = server
        .ecs
        .query::<(Entity, &NetworkOwner)>()
        .iter(&server.ecs)
        .filter(|(_, o)| o.0 == player)
        .map(|(e, _)| e)
        .collect();
    for e in owned {
        server.ecs.entity_mut(e).remove::<NetworkOwner>();
    }
}

/// The player that owns `entity`, or `None` for the server.
pub fn network_owner(server: &World, entity: Entity) -> Option<Entity> {
    server.ecs.get::<NetworkOwner>(entity).map(|o| o.0)
}

/// In [`ReplicationSet::upstream`]: every component of an owned instance.
pub const UPSTREAM_ALL: &str = "*";

/// What one replication pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplicationReport {
    /// Instances created on the client.
    pub created: usize,
    /// Instances whose name, parent or fields were written on the client.
    pub updated: usize,
    /// Instances (subtree roots) removed from the client.
    pub removed: usize,
    /// Owned instances whose components were copied to the server.
    pub upstream: usize,
}

/// The server entities `player` owns: the owned instances and their
/// descendants (including ones added later, a tool or an `Animator`),
/// except those another player owns, in entity order.
fn owned_by(server: &mut World, player: Entity) -> Vec<Entity> {
    let mut q = server.ecs.query::<(Entity, &NetworkOwner)>();
    let roots: Vec<Entity> = q
        .iter(&server.ecs)
        .filter(|(_, o)| o.0 == player)
        .map(|(e, _)| e)
        .collect();
    let mut out: Vec<Entity> = Vec::new();
    for r in roots {
        for x in hierarchy::subtree(&server.ecs, r) {
            let other = server
                .ecs
                .get::<NetworkOwner>(x)
                .is_some_and(|o| o.0 != player);
            if !other {
                out.push(x);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

// ------------------------------------------------------------------ server

/// One replicated entity, as the server last sent it.
#[derive(Clone)]
struct Entry {
    handle: u32,
    name: String,
    /// The server parent last sent.
    parent: Option<Entity>,
    /// The server values last sent, by component key.
    comps: Vec<(ComponentKey, Vec<Value>)>,
    /// The components last sent with a reference to an instance the client
    /// was not sent yet (sent as nil), as sent; they are sent again once the
    /// reference resolves.
    unresolved: Vec<(ComponentKey, Vec<WireValue>)>,
}

impl Entry {
    fn comp(&self, k: ComponentKey) -> Option<&Vec<Value>> {
        self.comps.iter().find(|(x, _)| *x == k).map(|(_, v)| v)
    }

    fn set_comp(&mut self, k: ComponentKey, v: Vec<Value>) {
        match self.comps.iter_mut().find(|(x, _)| *x == k) {
            Some(slot) => slot.1 = v,
            None => self.comps.push((k, v)),
        }
    }
}

/// How [`ReplicationSet::upstream`] lets a component flow up: an entry
/// names a component (every field) or `Component.field` (that field).
#[derive(Debug, PartialEq, Eq)]
enum Up<'a> {
    All,
    Fields(Vec<&'a str>),
    No,
}

fn up_rule<'a>(upstream: &'a [String], name: &str) -> Up<'a> {
    if upstream.iter().any(|u| u == UPSTREAM_ALL || u == name) {
        return Up::All;
    }
    let fields: Vec<&str> = upstream
        .iter()
        .filter_map(|u| u.strip_prefix(name)?.strip_prefix('.'))
        .collect();
    if fields.is_empty() {
        Up::No
    } else {
        Up::Fields(fields)
    }
}

/// The components `upstream` names, each once, in order.
fn up_components(upstream: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in upstream {
        let c = u.split('.').next().unwrap_or(u);
        if !out.iter().any(|x| x == c) {
            out.push(c.to_string());
        }
    }
    out
}

/// The server's check of an owner's moves in [`Conn::apply_up`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UpCheck {
    /// The fastest an owned part may move, in studs per second.
    pub max_speed: f32,
    /// The seconds since the connection's previous frame was applied.
    pub dt: f32,
}

/// What [`Conn::apply_up`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpReport {
    /// Owned instances whose components were written.
    pub written: usize,
    /// Owned parts that moved faster than [`UpCheck::max_speed`]: not
    /// moved, and sent back where the server has them.
    pub too_fast: Vec<Entity>,
}

/// The server's replication state for one connection: its mirror of what
/// the client was sent, the handles it gave, and the names it numbered.
/// A clone of a connection bound at boot ([`Conn::bind`]) serves a client
/// that joins later from the same scene: its first diff is everything that
/// changed since.
#[derive(Clone, Default)]
pub struct Conn {
    mirror: HashMap<Entity, Entry>,
    by_handle: HashMap<u32, Entity>,
    next_handle: u32,
    last: Option<Tick>,
    /// The handles the client was told it owns.
    owned: BTreeSet<u32>,
    /// Names sent to the client.
    names: Names,
    /// Names the client sent.
    names_in: NameTable,
    /// Entities the next diff looks at although they did not change: a move
    /// the server refused is sent back.
    recheck: HashSet<Entity>,
}

impl Conn {
    /// How many instances the client was sent and still has.
    pub fn len(&self) -> usize {
        self.mirror.len()
    }

    /// True when the client has no instance from this connection.
    pub fn is_empty(&self) -> bool {
        self.mirror.is_empty()
    }

    /// The server entity of a handle on this connection.
    pub fn entity_of(&self, handle: u32) -> Option<Entity> {
        self.by_handle.get(&handle).copied()
    }

    fn new_handle(&mut self) -> u32 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }

    /// For a client that already holds the replicated instances (made from
    /// the same scene, with the same stable ids): gives each a handle and
    /// records the server's current values as sent. The frame tells the
    /// client which handle is which.
    pub fn bind(&mut self, server: &mut World, set: &ReplicationSet) -> Frame {
        let mut ops = Vec::new();
        if let Some(game) = game_of(server) {
            let order = replicated_order(server, game, set);
            // Every handle first, so references among them encode.
            for &s in &order {
                let handle = self.new_handle();
                let comps = server
                    .registry
                    .components_of(&server.ecs, s)
                    .into_iter()
                    .filter_map(|k| server.registry.get_all(&server.ecs, s, k).map(|v| (k, v)))
                    .collect();
                self.mirror.insert(
                    s,
                    Entry {
                        handle,
                        name: server.name_of(s).to_string(),
                        parent: hierarchy::parent(&server.ecs, s),
                        comps,
                        unresolved: Vec::new(),
                    },
                );
                self.by_handle.insert(handle, s);
            }
            let Conn { mirror, names, .. } = self;
            let refs = |x: Entity| down_ref(mirror, game, x);
            for &s in &order {
                let entry = &mirror[&s];
                let comps = entry
                    .comps
                    .iter()
                    .map(|(k, v)| {
                        let name = names.id(&server.registry.schema(*k).name);
                        (name, v.iter().map(|x| to_wire(x, names, &refs)).collect())
                    })
                    .collect();
                ops.push(Op::Bind {
                    handle: entry.handle,
                    id: server.id_of(s).unwrap_or_default().to_string(),
                    comps,
                });
            }
        }
        self.last = Some(server.ecs.change_tick());
        server.ecs.increment_change_tick();
        Frame {
            tick: 0,
            names: self.names.take_fresh(),
            ops,
            msgs: Vec::new(),
        }
    }

    /// Applies a client's frame: its owned [`ReplicationSet::upstream`]
    /// components ([`Op::Up`]) are written to the instances `player` (a
    /// server `Player`) owns now; anything else is ignored. With `check`, a
    /// part that moved too fast is not moved.
    pub fn apply_up(
        &mut self,
        server: &mut World,
        set: &ReplicationSet,
        player: Option<Entity>,
        frame: &Frame,
        check: Option<UpCheck>,
    ) -> UpReport {
        self.names_in.extend(&frame.names);
        let mut report = UpReport::default();
        let Some(game) = game_of(server) else {
            return report;
        };
        let moves = check.and_then(|c| {
            let k = server.registry.try_key("Transform")?;
            let i = server.registry.schema(k).field_index("translation").ok()?;
            Some((c, k, i))
        });
        let owned: HashSet<Entity> = match player {
            Some(p) => owned_by(server, p).into_iter().collect(),
            None => HashSet::new(),
        };
        let mut wrote: BTreeSet<u32> = BTreeSet::new();
        for op in &frame.ops {
            let Op::Up {
                handle,
                comp,
                values,
            } = op
            else {
                continue;
            };
            let Some(&s) = self.by_handle.get(handle) else {
                continue;
            };
            if !owned.contains(&s) {
                continue;
            }
            let Some(name) = self.names_in.get(*comp) else {
                continue;
            };
            let rule = up_rule(&set.upstream, name);
            if rule == Up::No {
                continue;
            }
            let Some(sk) = server.registry.try_key(name) else {
                continue;
            };
            let by_handle = &self.by_handle;
            let refs = |r: Ref| match r {
                Ref::Game => Some(game),
                Ref::Handle(h) => by_handle.get(&h).copied(),
            };
            let Some(mut cv) = values
                .iter()
                .map(|v| from_wire(v, &self.names_in, &refs))
                .collect::<Option<Vec<Value>>>()
            else {
                continue;
            };
            if cv.len() != server.registry.schema(sk).fields.len() {
                continue;
            }
            let Some(entry) = self.mirror.get_mut(&s) else {
                continue;
            };
            let old = entry.comp(sk).cloned();
            if let Up::Fields(fields) = &rule {
                // Only those fields come from the client; the rest stay as
                // the client was last known to have them.
                let Some(base) = old
                    .clone()
                    .or_else(|| server.registry.get_all(&server.ecs, s, sk))
                else {
                    continue;
                };
                let schema = server.registry.schema(sk);
                let mut kept = base;
                for f in fields {
                    if let Ok(i) = schema.field_index(f) {
                        kept[i] = cv[i].clone();
                    }
                }
                cv = kept;
            }
            if old.as_ref() == Some(&cv) {
                continue;
            }
            if let Some((c, k, i)) = moves
                && k == sk
                && let (Some(Value::Vec3(from)), Some(Value::Vec3(to))) =
                    (server.registry.get_field(&server.ecs, s, sk, i), cv.get(i))
                && from.distance(*to) > c.max_speed * c.dt.max(f32::EPSILON)
            {
                // The client has it there; the next diff puts it back.
                entry.set_comp(sk, cv);
                self.recheck.insert(s);
                report.too_fast.push(s);
                continue;
            }
            let kinds = kinds_of(server, sk);
            if write_changes(server, s, sk, &kinds, old.as_ref(), &cv) || old.is_none() {
                wrote.insert(*handle);
            }
            entry.set_comp(sk, cv);
        }
        report.written = wrote.len();
        report
    }

    /// The server half of a pass for this connection: what changed since its
    /// last pass, as a frame for the client. `player` is the connection's
    /// `Player`, whose owned instances the client is told about. The caller
    /// increments the server world's change tick once every connection had
    /// its pass.
    pub fn diff(
        &mut self,
        server: &mut World,
        set: &ReplicationSet,
        player: Option<Entity>,
    ) -> Frame {
        let mut ops = Vec::new();
        let Some(game) = game_of(server) else {
            return Frame::default();
        };
        let this = server.ecs.change_tick();

        // Walk the replicated subtrees.
        let order = replicated_order(server, game, set);
        let mut seen: HashSet<Entity> = HashSet::with_capacity(order.len());
        let mut new = Vec::new();
        let mut changed = Vec::new();
        for &s in &order {
            seen.insert(s);
            match self.mirror.get(&s) {
                None => new.push(s),
                Some(_)
                    if self
                        .last
                        .is_none_or(|last| changed_since(server, s, last, this))
                        || self.recheck.contains(&s) =>
                {
                    changed.push(s)
                }
                // A reference it holds may resolve to an instance created now.
                Some(e) if !e.unresolved.is_empty() => changed.push(s),
                Some(_) => {}
            }
        }

        // Create, parents first.
        let mut fill = Vec::new();
        for s in new {
            let parent_s = hierarchy::parent(&server.ecs, s);
            let Some(parent) = parent_s.and_then(|p| down_ref(&self.mirror, game, p)) else {
                continue;
            };
            let service = (parent_s == Some(game) && server.ecs.get::<ServiceTag>(s).is_some())
                .then(|| tree::class(server, s).map(str::to_string))
                .flatten();
            let handle = self.new_handle();
            let name = server.name_of(s).to_string();
            ops.push(Op::Create {
                handle,
                parent,
                id: server.id_of(s).unwrap_or_default().to_string(),
                name: name.clone(),
                service,
            });
            self.mirror.insert(
                s,
                Entry {
                    handle,
                    name,
                    parent: parent_s,
                    comps: Vec::new(),
                    unresolved: Vec::new(),
                },
            );
            self.by_handle.insert(handle, s);
            fill.push(s);
        }

        // Components of the new entities, then the changes of the others.
        // References encode once every new entity has its handle.
        for s in fill.into_iter().chain(changed) {
            let Conn { mirror, names, .. } = self;
            let Some(entry) = mirror.get(&s) else {
                continue;
            };
            let handle = entry.handle;
            let name = server.name_of(s).to_string();
            if name != entry.name {
                ops.push(Op::Name {
                    handle,
                    name: name.clone(),
                });
            }
            let parent_s = hierarchy::parent(&server.ecs, s);
            if parent_s != entry.parent
                && let Some(parent) = parent_s.and_then(|p| down_ref(mirror, game, p))
            {
                ops.push(Op::Parent { handle, parent });
            }
            let refs = |x: Entity| down_ref(mirror, game, x);
            let current = server.registry.components_of(&server.ecs, s);
            let mut updates: Vec<(ComponentKey, Vec<Value>)> = Vec::new();
            let mut unresolved: Vec<(ComponentKey, Vec<WireValue>)> = Vec::new();
            for &sk in &current {
                let Some(sv) = server.registry.get_all(&server.ecs, s, sk) else {
                    continue;
                };
                let old = entry.comp(sk);
                let pending = entry
                    .unresolved
                    .iter()
                    .find(|(k, _)| *k == sk)
                    .map(|(_, v)| v);
                if old == Some(&sv) && pending.is_none() {
                    continue;
                }
                let mapped: Vec<WireValue> = sv.iter().map(|v| to_wire(v, names, &refs)).collect();
                let old_mapped: Option<Vec<WireValue>> = match pending {
                    Some(sent) if old == Some(&sv) => Some(sent.clone()),
                    _ => old.map(|o| o.iter().map(|v| to_wire(v, names, &refs)).collect()),
                };
                if old_mapped.as_ref() != Some(&mapped) {
                    let comp = names.id(&server.registry.schema(sk).name);
                    ops.push(match old_mapped {
                        None => Op::Insert {
                            handle,
                            comp,
                            values: mapped.clone(),
                        },
                        Some(o) => Op::Set {
                            handle,
                            comp,
                            fields: mapped
                                .iter()
                                .enumerate()
                                .filter(|(i, v)| o.get(*i) != Some(v))
                                .map(|(i, v)| (i as u32, v.clone()))
                                .collect(),
                        },
                    });
                }
                if mapped.iter().map(WireValue::refs).sum::<usize>()
                    < sv.iter().map(refs_of).sum::<usize>()
                {
                    unresolved.push((sk, mapped));
                }
                if old != Some(&sv) {
                    updates.push((sk, sv));
                }
            }
            let dropped: Vec<ComponentKey> = entry
                .comps
                .iter()
                .map(|(k, _)| *k)
                .filter(|k| !current.contains(k))
                .collect();
            for sk in &dropped {
                ops.push(Op::Drop {
                    handle,
                    comp: names.id(&server.registry.schema(*sk).name),
                });
            }
            let entry = mirror.get_mut(&s).expect("present above");
            entry.name = name;
            entry.parent = parent_s;
            for (k, v) in updates {
                entry.set_comp(k, v);
            }
            entry.comps.retain(|(k, _)| !dropped.contains(k));
            // What was re-checked this pass replaces what was pending.
            entry.unresolved.retain(|(k, _)| !current.contains(k));
            entry.unresolved.extend(unresolved);
        }

        // Remove what the walk did not reach.
        let mut gone: Vec<u32> = Vec::new();
        self.mirror.retain(|s, e| {
            let keep = seen.contains(s);
            if !keep {
                gone.push(e.handle);
            }
            keep
        });
        gone.sort_unstable();
        for h in &gone {
            self.by_handle.remove(h);
            self.owned.remove(h);
        }
        if !gone.is_empty() {
            ops.push(Op::Remove { handles: gone });
        }

        // Ownership, as changes.
        let owned: BTreeSet<u32> = match player {
            Some(p) => owned_by(server, p)
                .into_iter()
                .filter_map(|s| self.mirror.get(&s).map(|e| e.handle))
                .collect(),
            None => BTreeSet::new(),
        };
        for &handle in owned.difference(&self.owned) {
            ops.push(Op::Own {
                handle,
                owned: true,
            });
        }
        for &handle in self.owned.difference(&owned) {
            ops.push(Op::Own {
                handle,
                owned: false,
            });
        }
        self.owned = owned;

        self.recheck.clear();
        self.last = Some(this);
        Frame {
            tick: 0,
            names: self.names.take_fresh(),
            ops,
            msgs: Vec::new(),
        }
    }
}

/// How a server entity is referred to on a connection: the `game` root, or
/// its handle once it was sent.
fn down_ref(mirror: &HashMap<Entity, Entry>, game: Entity, x: Entity) -> Option<Ref> {
    if x == game {
        Some(Ref::Game)
    } else {
        mirror.get(&x).map(|e| Ref::Handle(e.handle))
    }
}

/// The server world's replication state for the in-process client.
#[derive(Resource, Default)]
pub struct ServerReplica {
    /// The local client's connection.
    pub local: Conn,
}

// ------------------------------------------------------------------ client

/// One replicated instance, as the client has it.
struct ClientEntry {
    /// Its client entity; `None` when its parent was gone when it arrived.
    client: Option<Entity>,
    /// Its server parent as last sent; `None` for a bound instance.
    parent: Option<Ref>,
    id: String,
    name: String,
    /// The values last received or sent, by client component key, as the
    /// client read them (references to instances it did not have are null).
    comps: Vec<(ComponentKey, Vec<Value>)>,
}

impl ClientEntry {
    fn comp(&self, k: ComponentKey) -> Option<&Vec<Value>> {
        self.comps.iter().find(|(x, _)| *x == k).map(|(_, v)| v)
    }

    fn set_comp(&mut self, k: ComponentKey, v: Vec<Value>) {
        match self.comps.iter_mut().find(|(x, _)| *x == k) {
            Some(slot) => slot.1 = v,
            None => self.comps.push((k, v)),
        }
    }
}

/// The client's mirror of the replicated instances, a resource of the
/// client world.
#[derive(Resource, Default)]
pub struct Replica {
    entries: HashMap<u32, ClientEntry>,
    by_client: HashMap<Entity, u32>,
    last: Option<Tick>,
    /// The handles the server said this client owns.
    owned: BTreeSet<u32>,
    /// Names sent to the server.
    names: Names,
    /// Names the server sent.
    names_in: NameTable,
    /// Client subtree roots the server removed, waiting for the client VM.
    pub(crate) pending: Vec<Entity>,
    /// Components outside the upstream set whose owned changes were
    /// reported ([`codes::OWNED_WRITE`]), and the reports not taken yet.
    warned: HashSet<String>,
    warnings: Vec<Diagnostic>,
}

impl Replica {
    /// The handle of a replicated client entity, if it is one.
    pub fn handle_of(&self, client: Entity) -> Option<u32> {
        self.by_client.get(&client).copied()
    }

    /// The client entity of a handle.
    pub fn client_of(&self, handle: u32) -> Option<Entity> {
        self.entries.get(&handle).and_then(|e| e.client)
    }

    /// How many instances are replicated.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Takes the reports of owned changes that stay on this client
    /// ([`codes::OWNED_WRITE`]), each component once per run.
    pub fn take_warnings(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.warnings)
    }

    /// True when nothing is replicated.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The client half of step 1: the changed upstream components of the
    /// instances this client owns, as a frame for the server.
    pub fn diff_up(&mut self, client: &mut World, set: &ReplicationSet) -> Frame {
        let mut ops = Vec::new();
        let Some(game) = game_of(client) else {
            return Frame::default();
        };
        let this = client.ecs.change_tick();
        let all = set.upstream.iter().any(|n| n == UPSTREAM_ALL);
        for handle in self.owned.iter().copied().collect::<Vec<_>>() {
            let Some(c) = self.entries.get(&handle).and_then(|e| e.client) else {
                continue;
            };
            if !tree::alive(client, c)
                || self
                    .last
                    .is_some_and(|last| !changed_since(client, c, last, this))
            {
                continue;
            }
            let names: Vec<String> = if all {
                client
                    .registry
                    .components_of(&client.ecs, c)
                    .into_iter()
                    .map(|k| client.registry.schema(k).name.clone())
                    .collect()
            } else {
                up_components(&set.upstream)
            };
            if !all {
                self.report_kept(client, handle, c, &set.upstream);
            }
            let Replica {
                entries,
                by_client,
                names: out,
                ..
            } = self;
            let refs = |x: Entity| {
                if x == game {
                    Some(Ref::Game)
                } else {
                    by_client.get(&x).map(|h| Ref::Handle(*h))
                }
            };
            let entry = entries.get_mut(&handle).expect("present above");
            for name in &names {
                let Some(ck) = client.registry.try_key(name) else {
                    continue;
                };
                let Some(cv) = client.registry.get_all(&client.ecs, c, ck) else {
                    continue;
                };
                if entry.comp(ck) == Some(&cv) {
                    continue;
                }
                ops.push(Op::Up {
                    handle,
                    comp: out.id(name),
                    values: cv.iter().map(|v| to_wire(v, out, &refs)).collect(),
                });
                entry.set_comp(ck, cv);
            }
        }
        Frame {
            tick: 0,
            names: self.names.take_fresh(),
            ops,
            msgs: Vec::new(),
        }
    }

    /// Reports, once per component, an owned instance's component outside
    /// `upstream` that differs from what the server sent: the change stays
    /// here.
    fn report_kept(&mut self, client: &World, handle: u32, c: Entity, upstream: &[String]) {
        let Some(entry) = self.entries.get(&handle) else {
            return;
        };
        for k in client.registry.components_of(&client.ecs, c) {
            let schema = client.registry.schema(k);
            let name = &schema.name;
            let rule = up_rule(upstream, name);
            if rule == Up::All || self.warned.contains(name) {
                continue;
            }
            let Some(sent) = entry.comp(k) else {
                continue;
            };
            let Some(now) = client.registry.get_all(&client.ecs, c, k) else {
                continue;
            };
            let kept = |i: usize| match &rule {
                Up::Fields(f) => !f.contains(&schema.fields[i].name.as_str()),
                _ => true,
            };
            let fields: Vec<&str> = now
                .iter()
                .zip(sent)
                .enumerate()
                .filter(|(i, (a, b))| a != b && kept(*i))
                .map(|(i, _)| schema.fields[i].name.as_str())
                .collect();
            if !fields.is_empty() {
                self.warned.insert(name.clone());
                self.warnings.push(Diagnostic::warning(
                    codes::OWNED_WRITE,
                    format!(
                        "a client change to {name} ({}) on an owned instance stays on this \
                         client: networked games send only [compat.net] upstream components \
                         to the server",
                        fields.join(", ")
                    ),
                ));
            }
        }
    }

    fn decode(&self, game: Entity, values: &[WireValue]) -> Option<Vec<Value>> {
        let refs = |r: Ref| match r {
            Ref::Game => Some(game),
            Ref::Handle(h) => self.entries.get(&h).and_then(|e| e.client),
        };
        values
            .iter()
            .map(|v| from_wire(v, &self.names_in, &refs))
            .collect()
    }

    /// The client key of a component name number.
    fn key(&self, client: &World, comp: u32) -> Option<ComponentKey> {
        client.registry.try_key(self.names_in.get(comp)?)
    }

    /// Makes the client entity of `handle` under `parent`: same stable id
    /// and name. A client-made instance holding the id gives it up.
    fn spawn(&mut self, client: &mut World, handle: u32, parent: Entity) -> Entity {
        let entry = &self.entries[&handle];
        let (id, name) = (entry.id.clone(), entry.name.clone());
        if let Ok(other) = client.entity(&id) {
            let fresh = client.new_id();
            client.ecs.entity_mut(other).insert(StableId(fresh));
        }
        let c = client.ecs.spawn((Name(name), StableId(id))).id();
        hierarchy::add_child(&mut client.ecs, parent, c);
        c
    }

    /// Spawns the waiting `handle` under `parent`, with its components, then
    /// the instances that waited under it. Returns the handles spawned.
    fn arrive(&mut self, client: &mut World, handle: u32, parent: Entity) -> Vec<u32> {
        let mut done = Vec::new();
        let mut todo = vec![(handle, parent)];
        while let Some((h, p)) = todo.pop() {
            let c = self.spawn(client, h, p);
            self.entries.get_mut(&h).expect("present").client = Some(c);
            self.by_client.insert(c, h);
            for (ck, v) in self.entries[&h].comps.clone() {
                let kinds = kinds_of(client, ck);
                write_changes(client, c, ck, &kinds, None, &v);
            }
            done.push(h);
            let mut kids: Vec<u32> = self
                .entries
                .iter()
                .filter(|(_, e)| e.client.is_none() && e.parent == Some(Ref::Handle(h)))
                .map(|(k, _)| *k)
                .collect();
            // Lowest handle first: in the order the server created them.
            kids.sort_unstable_by(|a, b| b.cmp(a));
            todo.extend(kids.into_iter().map(|k| (k, c)));
        }
        done
    }

    /// The client half of steps 2 to 5: applies a server frame to the
    /// client world.
    pub fn apply(&mut self, client: &mut World, frame: &Frame) -> ReplicationReport {
        self.names_in.extend(&frame.names);
        let mut report = ReplicationReport::default();
        let Some(game) = game_of(client) else {
            return report;
        };
        // Subtrees removed in an earlier pass that no client VM took: despawn.
        for root in std::mem::take(&mut self.pending) {
            if tree::alive(client, root) {
                let _ = client.despawn_runtime(root);
            }
        }
        let mut wrote: Vec<u32> = Vec::new();
        let mut mark = |h: u32| {
            if !wrote.contains(&h) {
                wrote.push(h);
            }
        };
        let alive = |client: &World, c: Option<Entity>| c.filter(|c| tree::alive(client, *c));
        for op in &frame.ops {
            if let Op::Bind { handle, id, .. } = op {
                let c = client.entity(id).ok();
                let name = c.map(|c| client.name_of(c).to_string()).unwrap_or_default();
                self.entries.insert(
                    *handle,
                    ClientEntry {
                        client: c,
                        parent: None,
                        id: id.clone(),
                        name,
                        comps: Vec::new(),
                    },
                );
                if let Some(c) = c {
                    self.by_client.insert(c, *handle);
                }
            }
        }
        for op in &frame.ops {
            match op {
                Op::Bind { handle, comps, .. } => {
                    // The entries were made above; references among them
                    // resolve now.
                    let comps = comps
                        .iter()
                        .filter_map(|(k, v)| Some((self.key(client, *k)?, self.decode(game, v)?)))
                        .collect();
                    if let Some(e) = self.entries.get_mut(handle) {
                        e.comps = comps;
                    }
                }
                Op::Create {
                    handle,
                    parent,
                    id,
                    name,
                    service,
                } => {
                    self.entries.insert(
                        *handle,
                        ClientEntry {
                            client: None,
                            parent: Some(*parent),
                            id: id.clone(),
                            name: name.clone(),
                            comps: Vec::new(),
                        },
                    );
                    let parent = match parent {
                        Ref::Game => Some(game),
                        Ref::Handle(p) => self.entries.get(p).and_then(|e| e.client),
                    };
                    // Its parent is gone (destroyed by a client script): it
                    // waits for a move to a live parent.
                    let Some(parent) = alive(client, parent) else {
                        continue;
                    };
                    // A service the client made itself before the server
                    // had it: adopt it.
                    let adopted = service
                        .as_deref()
                        .filter(|_| parent == game)
                        .and_then(|class| service_in(client, game, class));
                    let c = match adopted {
                        Some(c) => {
                            if let Some(old) = self.by_client.remove(&c)
                                && let Some(e) = self.entries.get_mut(&old)
                            {
                                e.client = None;
                            }
                            if client.id_of(c) != Some(id.as_str()) {
                                client.ecs.entity_mut(c).insert(StableId(id.clone()));
                            }
                            c
                        }
                        None => {
                            report.created += 1;
                            self.spawn(client, *handle, parent)
                        }
                    };
                    self.entries.get_mut(handle).expect("inserted").client = Some(c);
                    self.by_client.insert(c, *handle);
                }
                Op::Name { handle, name } => {
                    let Some(e) = self.entries.get_mut(handle) else {
                        continue;
                    };
                    e.name = name.clone();
                    if let Some(c) = alive(client, e.client) {
                        tree::set_name(client, c, name);
                        mark(*handle);
                    }
                }
                Op::Parent { handle, parent } => {
                    let Some(e) = self.entries.get_mut(handle) else {
                        continue;
                    };
                    e.parent = Some(*parent);
                    let pc = match parent {
                        Ref::Game => Some(game),
                        Ref::Handle(p) => self.entries.get(p).and_then(|e| e.client),
                    };
                    let Some(pc) = alive(client, pc) else {
                        continue;
                    };
                    match self.entries[handle].client {
                        // It waited for a live parent: it arrives now, with
                        // what waited under it.
                        None => {
                            for h in self.arrive(client, *handle, pc) {
                                report.created += 1;
                                mark(h);
                            }
                        }
                        Some(c) => {
                            if tree::alive(client, c)
                                && !hierarchy::is_self_or_ancestor(&client.ecs, c, pc)
                                && hierarchy::parent(&client.ecs, c) != Some(pc)
                            {
                                hierarchy::add_child(&mut client.ecs, pc, c);
                                mark(*handle);
                            }
                        }
                    }
                }
                Op::Insert {
                    handle,
                    comp,
                    values,
                } => {
                    let Some(ck) = self.key(client, *comp) else {
                        continue;
                    };
                    let Some(new) = self.decode(game, values) else {
                        continue;
                    };
                    let Some(e) = self.entries.get_mut(handle) else {
                        continue;
                    };
                    e.set_comp(ck, new.clone());
                    if let Some(c) = alive(client, e.client) {
                        let kinds = kinds_of(client, ck);
                        if write_changes(client, c, ck, &kinds, None, &new) {
                            mark(*handle);
                        }
                    }
                }
                Op::Set {
                    handle,
                    comp,
                    fields,
                } => {
                    let Some(ck) = self.key(client, *comp) else {
                        continue;
                    };
                    let Some(old) = self.entries.get(handle).and_then(|e| e.comp(ck)).cloned()
                    else {
                        continue;
                    };
                    let mut new = old.clone();
                    let mut ok = true;
                    for (i, v) in fields {
                        match (
                            new.get_mut(*i as usize),
                            self.decode(game, std::slice::from_ref(v)),
                        ) {
                            (Some(slot), Some(mut d)) => *slot = d.remove(0),
                            _ => ok = false,
                        }
                    }
                    if !ok {
                        continue;
                    }
                    let e = self.entries.get_mut(handle).expect("present above");
                    e.set_comp(ck, new.clone());
                    if let Some(c) = alive(client, e.client) {
                        let kinds = kinds_of(client, ck);
                        if write_changes(client, c, ck, &kinds, Some(&old), &new) {
                            mark(*handle);
                        }
                    }
                }
                Op::Drop { handle, comp } => {
                    let Some(ck) = self.key(client, *comp) else {
                        continue;
                    };
                    let Some(e) = self.entries.get_mut(handle) else {
                        continue;
                    };
                    e.comps.retain(|(k, _)| *k != ck);
                    if let Some(c) = alive(client, e.client)
                        && client.registry.has(&client.ecs, c, ck)
                    {
                        client.registry.remove(&mut client.ecs, c, ck);
                        mark(*handle);
                    }
                }
                Op::Own { handle, owned } => {
                    if *owned {
                        self.owned.insert(*handle);
                    } else {
                        self.owned.remove(handle);
                    }
                }
                Op::Remove { handles } => {
                    let mut gone: HashSet<Entity> = HashSet::new();
                    for h in handles {
                        self.owned.remove(h);
                        let Some(e) = self.entries.remove(h) else {
                            continue;
                        };
                        let Some(c) = e.client else {
                            continue;
                        };
                        self.by_client.remove(&c);
                        if tree::alive(client, c) && !tree::is_destroyed(client, c) {
                            gone.insert(c);
                        }
                    }
                    let mut roots: Vec<Entity> = gone
                        .iter()
                        .copied()
                        .filter(|c| !tree::ancestors(client, *c).iter().any(|a| gone.contains(a)))
                        .collect();
                    roots.sort();
                    // The client VM destroys them in its pump, after the
                    // player and character events that need them in place.
                    report.removed += roots.len();
                    self.pending.extend(roots);
                }
                Op::Up { .. } => {}
            }
        }
        report.updated = wrote
            .iter()
            .filter(|h| self.entries.get(h).is_some_and(|e| !e.comps.is_empty()))
            .count();
        self.last = Some(client.ecs.change_tick());
        client.ecs.increment_change_tick();
        report
    }
}

/// A frame through its bytes, as the in-process run sends it.
fn carry(frame: Frame) -> Frame {
    Frame::decode(&frame.encode()).expect("a frame decodes")
}

/// Puts both mirrors in place for a client world just forked from `server`:
/// every replicated server entity gets a handle and is bound to the client
/// entity of the same stable id, with the server's current values recorded
/// as sent.
pub(crate) fn init(server: &mut World, client: &mut World, set: &ReplicationSet) {
    let mut conn = Conn::default();
    let frame = carry(conn.bind(server, set));
    let mut rep = Replica::default();
    rep.apply(client, &frame);
    server.ecs.insert_resource(ServerReplica { local: conn });
    client.ecs.insert_resource(rep);
}

/// One replication pass between `server` and the in-process `client` (see
/// the module docs), with both halves' frames going through their bytes.
/// Without the mirrors [`CompatWorlds::new`](super::CompatWorlds::new) puts
/// in place, the pass starts new ones and replicates everything as new.
pub fn replicate(
    server: &mut World,
    client: &mut World,
    set: &ReplicationSet,
) -> ReplicationReport {
    if game_of(server).is_none() || game_of(client).is_none() {
        return ReplicationReport::default();
    }
    let mut rep = client.ecs.remove_resource::<Replica>().unwrap_or_default();
    let mut conns = server
        .ecs
        .remove_resource::<ServerReplica>()
        .unwrap_or_default();
    let player = client
        .ecs
        .get_resource::<NetLink>()
        .and_then(|l| l.local_player())
        .and_then(|id| server.entity(&id).ok());
    let up = carry(rep.diff_up(client, set));
    let upstream = conns.local.apply_up(server, set, player, &up, None).written;
    let down = carry(conns.local.diff(server, set, player));
    server.ecs.increment_change_tick();
    let mut report = rep.apply(client, &down);
    report.upstream = upstream;
    server.ecs.insert_resource(conns);
    client.ecs.insert_resource(rep);
    report
}

// ----------------------------------------------------------------- helpers

/// True if any component of `e` changed after `last`.
fn changed_since(w: &World, e: Entity, last: Tick, this: Tick) -> bool {
    let Ok(er) = w.ecs.get_entity(e) else {
        return false;
    };
    er.archetype().components().iter().any(|id| {
        er.get_change_ticks_by_id(*id)
            .is_some_and(|t| t.is_changed(last, this))
    })
}

/// The `game` root of a world.
fn game_of(w: &World) -> Option<Entity> {
    let g = w.entity(GAME_ID).ok()?;
    (tree::class(w, g) == Some(ROOT_CLASS)).then_some(g)
}

/// The service of class `class` under `game`.
fn service_in(w: &World, game: Entity, class: &str) -> Option<Entity> {
    crate::services::core::find_service(w, game, class)
}

/// The server entities to replicate, in tree order (parents first): the
/// replicated services and their descendants with a stable id.
fn replicated_order(server: &World, game: Entity, set: &ReplicationSet) -> Vec<Entity> {
    let mut out = Vec::new();
    for class in &set.containers {
        let Some(root) = service_in(server, game, class) else {
            continue;
        };
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            if server.ecs.get::<StableId>(e).is_none() || tree::is_destroyed(server, e) {
                continue;
            }
            out.push(e);
            stack.extend(hierarchy::children(&server.ecs, e).iter().rev().copied());
        }
    }
    out
}

/// How many instance references `v` holds.
fn refs_of(v: &Value) -> usize {
    match v {
        Value::Entity(Some(_)) => 1,
        Value::List(items) => items.iter().map(refs_of).sum(),
        Value::Map(m) => m.values().map(refs_of).sum(),
        _ => 0,
    }
}

/// How one field of a component is merged into the receiving side.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Merge {
    /// The whole value.
    Value,
    /// A string-keyed map: per key.
    Keys,
    /// A tag list: per tag.
    Set,
}

fn merge_kind(w: &World, k: ComponentKey, field: usize) -> Merge {
    let schema = w.registry.schema(k);
    match &schema.fields[field].ty {
        FieldType::Map => Merge::Keys,
        FieldType::List(_) if schema.name == "Tags" => Merge::Set,
        _ => Merge::Value,
    }
}

/// The value the receiver gets for a field whose sender value went from `old`
/// to `new`, given the receiver's `current` value.
fn merged(kind: Merge, old: &Value, new: &Value, current: Option<&Value>) -> Value {
    match (kind, old, new, current) {
        (Merge::Keys, Value::Map(old), Value::Map(new), Some(Value::Map(cur))) => {
            let mut out = cur.clone();
            for (k, v) in new {
                if old.get(k) != Some(v) {
                    out.insert(k.clone(), v.clone());
                }
            }
            for k in old.keys() {
                if !new.contains_key(k) {
                    out.remove(k);
                }
            }
            Value::Map(out)
        }
        (Merge::Set, Value::List(old), Value::List(new), Some(Value::List(cur))) => {
            let mut out = cur.clone();
            for t in new {
                if !old.contains(t) && !out.contains(t) {
                    out.push(t.clone());
                }
            }
            out.retain(|t| !(old.contains(t) && !new.contains(t)));
            Value::List(out)
        }
        _ => new.clone(),
    }
}

/// Writes the fields of component `k` that went from `old` to `new` on the
/// sending side into `to`'s entity `e` (key `tk`), merging maps per key and
/// tags per tag (with no `old`, from an empty map or list, so the receiver's
/// own keys and tags stay). Inserts the whole component when `to` lacks it.
/// Returns true when something was written; values that are not one per
/// field write nothing.
fn write_changes(
    to: &mut World,
    e: Entity,
    tk: ComponentKey,
    kinds: &[Merge],
    old: Option<&Vec<Value>>,
    new: &[Value],
) -> bool {
    // A peer's values that do not fit the component's fields.
    if new.len() != to.registry.schema(tk).fields.len() {
        return false;
    }
    let Some(current) = to.registry.get_all(&to.ecs, e, tk) else {
        return to.registry.insert(&mut to.ecs, e, tk, new.to_vec()).is_ok();
    };
    let mut wrote = false;
    for (i, nv) in new.iter().enumerate() {
        let kind = kinds.get(i).copied().unwrap_or(Merge::Value);
        // A component the sender did not have before (its first attribute
        // or tag) merges into what the receiver has on its own.
        let empty = match kind {
            Merge::Keys => Value::Map(Default::default()),
            Merge::Set => Value::List(Vec::new()),
            Merge::Value => Value::Bool(false),
        };
        let ov = match old {
            Some(o) => match o.get(i) {
                Some(ov) => ov,
                None => continue,
            },
            None => &empty,
        };
        if old.is_some() && ov == nv {
            continue;
        }
        let v = if old.is_some() || kind != Merge::Value {
            merged(kind, ov, nv, current.get(i))
        } else {
            nv.clone()
        };
        if current.get(i) != Some(&v) && to.registry.set_field(&mut to.ecs, e, tk, i, v).is_ok() {
            wrote = true;
        }
    }
    wrote
}

fn kinds_of(w: &World, k: ComponentKey) -> Vec<Merge> {
    (0..w.registry.schema(k).fields.len())
        .map(|i| merge_kind(w, k, i))
        .collect()
}

/// The client instances the server removed whose destruction is pending
/// (the roots and their descendants).
pub(crate) fn removing(client: &World) -> HashSet<Entity> {
    let mut out = HashSet::new();
    if let Some(r) = client.ecs.get_resource::<Replica>() {
        for root in &r.pending {
            if tree::alive(client, *root) {
                out.extend(hierarchy::subtree(&client.ecs, *root));
            }
        }
    }
    out
}

/// Takes the client subtree roots the server removed, for the client VM to
/// destroy (`Destroying`, then despawn when its cycle settles).
pub(crate) fn take_removed(client: &mut World) -> Vec<Entity> {
    client
        .ecs
        .get_resource_mut::<Replica>()
        .map(|mut r| std::mem::take(&mut r.pending))
        .unwrap_or_default()
}
