//! The replication pass: server world → client world, plus the components a
//! client owns, client → server.
//!
//! The pass keeps a mirror (the [`Replica`] resource of the client world): for
//! each replicated server entity, its client entity and the values the server
//! last sent (its name, parent and every registry component). Each pass:
//! 1. **Upstream.** For each entity the local player owns ([`NetworkOwner`]),
//!    the [`ReplicationSet::upstream`] components the client changed are
//!    copied to the server.
//! 2. **Walk.** The replicated containers' subtrees on the server are walked
//!    in tree order. An entity the mirror lacks is new; one whose components
//!    changed since the last pass (bevy change ticks; a move or a rename
//!    changes `ChildOf` or `Name`) is diffed against the mirror.
//! 3. **Create**, parents first: same stable id, name and parent, then every
//!    registry component, with entity references remapped by stable id (a
//!    reference to an instance the client does not have reads as `nil`).
//! 4. **Diff.** Only what the server changed since the last pass is written:
//!    per field, per attribute key and per tag. This is the client-local
//!    write rule: a client's own write to a replicated instance stays until
//!    the server next changes that same field.
//! 5. **Remove.** A mirrored entity the walk did not reach (destroyed, or
//!    moved under a container that does not replicate) is taken out of the
//!    client tree, locked as destroyed and queued; the client VM's pump fires
//!    `Destroying` and despawns it (`net::pump`), or the next pass despawns it
//!    when no VM runs.
//!
//! Entities without a stable id are runtime-only and are not replicated.

use std::collections::{HashMap, HashSet};

use re_reflect::{ComponentKey, FieldType, Value};
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::change_detection::Tick;
use re_scene::ecs::component::Component;
use re_scene::ecs::prelude::Resource;
use re_scene::{Entity, Name, StableId, World, hierarchy};

use super::{NetLink, ReplicationSet};
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

/// One replicated entity.
struct Entry {
    client: Entity,
    name: String,
    /// The server parent last sent.
    parent: Option<Entity>,
    /// The server values last sent, by server component key.
    comps: Vec<(ComponentKey, Vec<Value>)>,
    /// The components last sent with a reference to an instance the client
    /// did not have yet (sent as nil), with the client values written; they
    /// are sent again once the reference resolves.
    unresolved: Vec<(ComponentKey, Vec<Value>)>,
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

/// The mirror of the replicated entities, kept in the client world.
#[derive(Resource, Default)]
pub struct Replica {
    mirror: HashMap<Entity, Entry>,
    by_client: HashMap<Entity, Entity>,
    server_last: Option<Tick>,
    client_last: Option<Tick>,
    /// Client subtree roots the server removed, waiting for the client VM.
    pub(crate) pending: Vec<Entity>,
}

impl Replica {
    /// The server entity a client entity replicates, if it does.
    pub fn server_of(&self, client: Entity) -> Option<Entity> {
        self.by_client.get(&client).copied()
    }

    /// The client entity of a replicated server entity.
    pub fn client_of(&self, server: Entity) -> Option<Entity> {
        self.mirror.get(&server).map(|e| e.client)
    }

    /// How many entities are replicated.
    pub fn len(&self) -> usize {
        self.mirror.len()
    }

    /// True when nothing is replicated.
    pub fn is_empty(&self) -> bool {
        self.mirror.is_empty()
    }
}

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
fn service(w: &World, game: Entity, class: &str) -> Option<Entity> {
    crate::services::core::find_service(w, game, class)
}

/// The server entities to replicate, in tree order (parents first): the
/// replicated services and their descendants with a stable id.
fn replicated_order(server: &World, game: Entity, set: &ReplicationSet) -> Vec<Entity> {
    let mut out = Vec::new();
    for class in &set.containers {
        let Some(root) = service(server, game, class) else {
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

/// The client component key of a server component key (by name).
struct KeyMap(HashMap<ComponentKey, Option<ComponentKey>>);

impl KeyMap {
    fn get(&mut self, server: &World, client: &World, k: ComponentKey) -> Option<ComponentKey> {
        *self
            .0
            .entry(k)
            .or_insert_with(|| client.registry.try_key(&server.registry.schema(k).name))
    }
}

/// Maps entity references inside a value through `f` (`None`: no such
/// instance on the other side).
/// How many instance references `v` holds.
fn refs(v: &Value) -> usize {
    match v {
        Value::Entity(Some(_)) => 1,
        Value::List(items) => items.iter().map(refs).sum(),
        Value::Map(m) => m.values().map(refs).sum(),
        _ => 0,
    }
}

fn remap(v: &Value, f: &dyn Fn(Entity) -> Option<Entity>) -> Value {
    match v {
        Value::Entity(Some(e)) => Value::Entity(f(*e)),
        Value::List(items) => Value::List(items.iter().map(|i| remap(i, f)).collect()),
        Value::Map(m) => Value::Map(m.iter().map(|(k, x)| (k.clone(), remap(x, f))).collect()),
        other => other.clone(),
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
/// Returns true
/// when something was written.
fn write_changes(
    to: &mut World,
    e: Entity,
    tk: ComponentKey,
    kinds: &[Merge],
    old: Option<&Vec<Value>>,
    new: &[Value],
) -> bool {
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

/// Puts the client world's mirror of `server` in place: every replicated
/// server entity is matched with the client entity of the same stable id
/// (the client was just forked from the server), and the server's current
/// values are recorded as sent.
pub(crate) fn init(server: &mut World, client: &mut World, set: &ReplicationSet) {
    let mut rep = Replica::default();
    if let Some(game) = game_of(server) {
        for s in replicated_order(server, game, set) {
            let Some(c) = server.id_of(s).and_then(|id| client.entity(id).ok()) else {
                continue;
            };
            let comps = server
                .registry
                .components_of(&server.ecs, s)
                .into_iter()
                .filter_map(|k| server.registry.get_all(&server.ecs, s, k).map(|v| (k, v)))
                .collect();
            rep.mirror.insert(
                s,
                Entry {
                    client: c,
                    name: server.name_of(s).to_string(),
                    parent: hierarchy::parent(&server.ecs, s),
                    comps,
                    unresolved: Vec::new(),
                },
            );
            rep.by_client.insert(c, s);
        }
    }
    rep.server_last = Some(server.ecs.change_tick());
    server.ecs.increment_change_tick();
    rep.client_last = Some(client.ecs.change_tick());
    client.ecs.increment_change_tick();
    client.ecs.insert_resource(rep);
}

/// One replication pass from `server` to `client` (see the module docs).
/// The client world's mirror must have been made by
/// [`CompatWorlds::new`](super::CompatWorlds::new); without one, the pass
/// starts a new mirror and replicates everything as new.
pub fn replicate(
    server: &mut World,
    client: &mut World,
    set: &ReplicationSet,
) -> ReplicationReport {
    let mut report = ReplicationReport::default();
    let mut rep = client.ecs.remove_resource::<Replica>().unwrap_or_default();
    let (Some(game_s), Some(game_c)) = (game_of(server), game_of(client)) else {
        client.ecs.insert_resource(rep);
        return report;
    };
    let server_this = server.ecs.change_tick();
    let client_this = client.ecs.change_tick();
    let mut keys = KeyMap(HashMap::new());

    // Subtrees removed in an earlier pass that no client VM took: despawn.
    for root in std::mem::take(&mut rep.pending) {
        if tree::alive(client, root) {
            let _ = client.despawn_runtime(root);
        }
    }

    // 1. Upstream: the components the local player's client owns.
    let local = client
        .ecs
        .get_resource::<NetLink>()
        .and_then(|l| l.local_player())
        .and_then(|id| server.entity(&id).ok());
    if let Some(lp) = local {
        let mut q = server.ecs.query::<(Entity, &NetworkOwner)>();
        let roots: Vec<Entity> = q
            .iter(&server.ecs)
            .filter(|(_, o)| o.0 == lp)
            .map(|(e, _)| e)
            .collect();
        // Ownership covers the owned instances' descendants too, including
        // ones added later (a tool, an Animator), unless another player owns
        // them.
        let mut sorted: Vec<Entity> = Vec::new();
        for r in roots {
            for x in hierarchy::subtree(&server.ecs, r) {
                let other = server.ecs.get::<NetworkOwner>(x).is_some_and(|o| o.0 != lp);
                if !other {
                    sorted.push(x);
                }
            }
        }
        sorted.sort();
        sorted.dedup();
        for s in sorted {
            let Some(entry) = rep.mirror.get(&s) else {
                continue;
            };
            let c = entry.client;
            if !tree::alive(client, c)
                || rep
                    .client_last
                    .is_some_and(|last| !changed_since(client, c, last, client_this))
            {
                continue;
            }
            let by_client = &rep.by_client;
            let to_server = |x: Entity| {
                if x == game_c {
                    Some(game_s)
                } else {
                    by_client.get(&x).copied()
                }
            };
            let mut wrote = false;
            let mut updates = Vec::new();
            let names: Vec<String> = if set.upstream.iter().any(|n| n == UPSTREAM_ALL) {
                client
                    .registry
                    .components_of(&client.ecs, c)
                    .into_iter()
                    .map(|k| client.registry.schema(k).name.clone())
                    .collect()
            } else {
                set.upstream.clone()
            };
            for name in &names {
                let (Some(sk), Some(ck)) =
                    (server.registry.try_key(name), client.registry.try_key(name))
                else {
                    continue;
                };
                let Some(cv) = client.registry.get_all(&client.ecs, c, ck) else {
                    continue;
                };
                let cv: Vec<Value> = cv.iter().map(|v| remap(v, &to_server)).collect();
                let old = entry.comp(sk);
                if old == Some(&cv) {
                    continue;
                }
                let kinds = kinds_of(server, sk);
                if write_changes(server, s, sk, &kinds, old, &cv) || old.is_none() {
                    wrote = true;
                }
                updates.push((sk, cv));
            }
            let entry = rep.mirror.get_mut(&s).expect("checked above");
            for (sk, cv) in updates {
                entry.set_comp(sk, cv);
            }
            if wrote {
                report.upstream += 1;
            }
        }
    }

    // 2. Walk the replicated subtrees.
    let order = replicated_order(server, game_s, set);
    let mut seen: HashSet<Entity> = HashSet::with_capacity(order.len());
    let mut new = Vec::new();
    let mut changed = Vec::new();
    for &s in &order {
        seen.insert(s);
        if !rep.mirror.contains_key(&s) {
            new.push(s);
        } else if rep
            .server_last
            .is_none_or(|last| changed_since(server, s, last, server_this))
        {
            changed.push(s);
        } else if !rep.mirror[&s].unresolved.is_empty() {
            // A reference it holds may resolve to an instance created now.
            changed.push(s);
        }
    }

    // 3. Create, parents first.
    let mut fill = Vec::new();
    for s in new {
        let parent_s = hierarchy::parent(&server.ecs, s);
        let parent_c = match parent_s {
            Some(p) if p == game_s => Some(game_c),
            Some(p) => rep.mirror.get(&p).map(|e| e.client),
            None => None,
        };
        let Some(parent_c) = parent_c.filter(|p| tree::alive(client, *p)) else {
            continue; // its client parent is gone (destroyed by a client script)
        };
        let id = server.id_of(s).unwrap_or_default().to_string();
        let name = server.name_of(s).to_string();
        // A service the client made itself before the server had it: adopt it.
        let adopted = (parent_s == Some(game_s) && server.ecs.get::<ServiceTag>(s).is_some())
            .then(|| tree::class(server, s).map(str::to_string))
            .flatten()
            .and_then(|class| service(client, game_c, &class));
        let c = match adopted {
            Some(c) => {
                if let Some(old) = rep.by_client.remove(&c) {
                    rep.mirror.remove(&old);
                }
                if client.id_of(c) != Some(id.as_str()) {
                    client.ecs.entity_mut(c).insert(StableId(id.clone()));
                }
                c
            }
            None => {
                // A client-made instance holding this id gives it up.
                if let Ok(other) = client.entity(&id) {
                    let fresh = client.new_id();
                    client.ecs.entity_mut(other).insert(StableId(fresh));
                }
                let c = client.ecs.spawn((Name(name.clone()), StableId(id))).id();
                hierarchy::add_child(&mut client.ecs, parent_c, c);
                report.created += 1;
                c
            }
        };
        rep.mirror.insert(
            s,
            Entry {
                client: c,
                name,
                parent: parent_s,
                comps: Vec::new(),
                unresolved: Vec::new(),
            },
        );
        rep.by_client.insert(c, s);
        fill.push(s);
    }

    // 3b and 4. Components of the new entities, then the changes of the
    // others. References are remapped once every new entity exists.
    let to_client = |rep: &Replica, x: Entity| {
        if x == game_s {
            Some(game_c)
        } else {
            rep.mirror.get(&x).map(|e| e.client)
        }
    };
    for s in fill.into_iter().chain(changed) {
        let Some(entry) = rep.mirror.get(&s) else {
            continue;
        };
        let c = entry.client;
        if !tree::alive(client, c) {
            continue; // destroyed by a client script: stays gone
        }
        let mut wrote = false;
        // Name.
        let name = server.name_of(s).to_string();
        let rename = name != entry.name;
        if rename {
            tree::set_name(client, c, &name);
            wrote = true;
        }
        // Parent.
        let parent_s = hierarchy::parent(&server.ecs, s);
        let reparent = parent_s != entry.parent;
        if reparent
            && let Some(pc) = parent_s.and_then(|p| to_client(&rep, p))
            && tree::alive(client, pc)
            && !hierarchy::is_self_or_ancestor(&client.ecs, c, pc)
            && hierarchy::parent(&client.ecs, c) != Some(pc)
        {
            hierarchy::add_child(&mut client.ecs, pc, c);
            wrote = true;
        }
        // Components. An owned upstream component the client just sent is
        // equal to the mirror here, so only the server's own changes to it
        // go back down.
        let current = server.registry.components_of(&server.ecs, s);
        let mut updates: Vec<(ComponentKey, Vec<Value>)> = Vec::new();
        let mut unresolved: Vec<(ComponentKey, Vec<Value>)> = Vec::new();
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
            if let Some(ck) = keys.get(server, client, sk) {
                let mapped: Vec<Value> = sv
                    .iter()
                    .map(|v| remap(v, &|x| to_client(&rep, x)))
                    .collect();
                let old_mapped: Option<Vec<Value>> = match pending {
                    Some(sent) if old == Some(&sv) => Some(sent.clone()),
                    _ => old.map(|o| {
                        o.iter()
                            .map(|v| remap(v, &|x| to_client(&rep, x)))
                            .collect()
                    }),
                };
                if old_mapped.as_ref() != Some(&mapped) {
                    let kinds = kinds_of(client, ck);
                    if write_changes(client, c, ck, &kinds, old_mapped.as_ref(), &mapped) {
                        wrote = true;
                    }
                }
                if mapped.iter().map(refs).sum::<usize>() < sv.iter().map(refs).sum::<usize>() {
                    unresolved.push((sk, mapped));
                }
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
            if let Some(ck) = keys.get(server, client, *sk)
                && client.registry.has(&client.ecs, c, ck)
            {
                client.registry.remove(&mut client.ecs, c, ck);
                wrote = true;
            }
        }
        let entry = rep.mirror.get_mut(&s).expect("present above");
        entry.name = name;
        entry.parent = parent_s;
        for (k, v) in updates {
            entry.set_comp(k, v);
        }
        entry.comps.retain(|(k, _)| !dropped.contains(k));
        // What was re-checked this pass replaces what was pending.
        let checked: Vec<ComponentKey> = current.clone();
        entry.unresolved.retain(|(k, _)| !checked.contains(k));
        entry.unresolved.extend(unresolved);
        if wrote && !entry.comps.is_empty() {
            report.updated += 1;
        }
    }

    // 5. Remove what the walk did not reach.
    let gone: Vec<Entity> = rep
        .mirror
        .keys()
        .copied()
        .filter(|s| !seen.contains(s))
        .collect();
    let mut roots: Vec<Entity> = Vec::new();
    let mut gone_clients: HashSet<Entity> = HashSet::new();
    for s in gone {
        let entry = rep.mirror.remove(&s).expect("listed above");
        rep.by_client.remove(&entry.client);
        if tree::alive(client, entry.client) && !tree::is_destroyed(client, entry.client) {
            gone_clients.insert(entry.client);
        }
    }
    for &c in &gone_clients {
        let covered = tree::ancestors(client, c)
            .iter()
            .any(|a| gone_clients.contains(a));
        if !covered {
            roots.push(c);
        }
    }
    roots.sort();
    // The client VM destroys them in its pump, after the player and
    // character events that need them in place.
    for root in roots {
        rep.pending.push(root);
        report.removed += 1;
    }

    rep.server_last = Some(server_this);
    server.ecs.increment_change_tick();
    rep.client_last = Some(client.ecs.change_tick());
    client.ecs.increment_change_tick();
    client.ecs.insert_resource(rep);
    report
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
