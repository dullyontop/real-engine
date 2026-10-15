//! From world changes to signals: the tree (`ChildAdded`, `ChildRemoved`,
//! `DescendantAdded`, `DescendantRemoving`, `AncestryChanged`), properties
//! (`Changed`, `GetPropertyChangedSignal`), attributes (`AttributeChanged`,
//! `GetAttributeChangedSignal`), tags (`CollectionService`'s instance added
//! and removed signals), and `WaitForChild`.
//!
//! * Tree changes are recorded by **observers** on `ChildOf`
//!   (`On<Insert<ChildOf>>` and `On<Discard<ChildOf>>`), never hooks:
//!   `ChildOf` is a bevy relationship that owns its hooks (ARCHITECTURE §4).
//!   They push to the world's [`HierarchyFeed`]. `Destroy` detaches the
//!   subtree like any other move; [`settle`] despawns it, with the observers
//!   silenced, once nothing is left to dispatch.
//! * Attribute and tag changes come from the world's change feeds, which the
//!   VM turns on and is the one consumer of. `CollectionService` counts only
//!   instances inside the place, so moves in and out of it count as tag
//!   changes too.
//! * Property changes come from a per-property diff of the instances someone
//!   watches (a `Changed` or property-changed connection), checked only when
//!   one of the instance's components changed since the last dispatch
//!   (bevy's change ticks). `Parent` changes come from the tree events.
//!
//! [`dispatch`] runs from [`settle`], the scheduler's settle hook, at the end of every
//! resumption cycle, and right after each write when signals are immediate.
//! It gathers the events with the world borrowed, then fires them with the
//! world released, so handlers can use the engine.

use std::collections::{BTreeMap, HashMap, HashSet};

use mlua::{Lua, MultiValue, Result, Value as LuaValue};
use re_reflect::Value;
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::change_detection::Tick;
use re_scene::ecs::hierarchy::ChildOf;
use re_scene::ecs::lifecycle::{Discard, Insert};
use re_scene::ecs::observer::On;
use re_scene::ecs::prelude::{Query, ResMut, Resource};
use re_scene::{Entity, Name, World};
use re_script::sched::{self, YieldToken};

use super::{Guard, SignalBehavior, SignalHub, SignalKey, fire_with};
use crate::class::{ClassMarker, Member};
use crate::data::Status;
use crate::instance::{self, Tombstone, attributes, tree};
use crate::runtime::{state, with_world};

/// One change of the tree.
#[derive(Clone, Debug)]
pub enum HierEvent {
    /// `child` was put under `parent`.
    Added {
        /// The instance that moved.
        child: Entity,
        /// Its new parent.
        parent: Entity,
    },
    /// `child` was taken from under `parent`.
    Removed {
        /// The instance that moved.
        child: Entity,
        /// Its old parent.
        parent: Entity,
        /// The old parent and its ancestors, nearest first, at that moment.
        chain: Vec<Entity>,
        /// The child's name and class at that moment.
        tomb: Tombstone,
    },
}

/// The tree changes since the last dispatch, in order.
#[derive(Resource, Default, Debug)]
pub struct HierarchyFeed {
    events: Vec<HierEvent>,
    suppress: bool,
}

#[derive(Resource)]
struct ObserversInstalled;

fn on_parented(
    ev: On<Insert<ChildOf>>,
    parents: Query<&ChildOf>,
    feed: Option<ResMut<HierarchyFeed>>,
) {
    let Some(mut feed) = feed else { return };
    if feed.suppress {
        return;
    }
    let child = ev.event().entity;
    if let Ok(c) = parents.get(child) {
        feed.events.push(HierEvent::Added {
            child,
            parent: c.parent(),
        });
    }
}

fn on_unparented(
    ev: On<Discard<ChildOf>>,
    parents: Query<&ChildOf>,
    info: Query<(Option<&Name>, Option<&ClassMarker>)>,
    feed: Option<ResMut<HierarchyFeed>>,
) {
    let Some(mut feed) = feed else { return };
    if feed.suppress {
        return;
    }
    let child = ev.event().entity;
    let Ok(c) = parents.get(child) else { return };
    let parent = c.parent();
    let mut chain = vec![parent];
    let mut cur = parent;
    while let Ok(p) = parents.get(cur) {
        cur = p.parent();
        chain.push(cur);
    }
    let tomb = match info.get(child) {
        Ok((name, class)) => Tombstone {
            name: name.map(|n| n.0.clone()).unwrap_or_default(),
            class: class
                .map(|c| c.class.clone())
                .unwrap_or_else(|| "Instance".into()),
        },
        Err(_) => Tombstone {
            name: String::new(),
            class: "Instance".into(),
        },
    };
    feed.events.push(HierEvent::Removed {
        child,
        parent,
        chain,
        tomb,
    });
}

/// Prepares a world for a VM: the tree observers (once per world), the tree
/// feed, and the attribute and tag change feeds.
pub fn prepare_world(w: &mut World) {
    if !w.ecs.contains_resource::<ObserversInstalled>() {
        w.ecs.insert_resource(ObserversInstalled);
        w.ecs.add_observer(on_parented);
        w.ecs.add_observer(on_unparented);
    }
    w.ecs.init_resource::<HierarchyFeed>();
    w.record_attribute_changes();
    w.record_tag_changes();
}

/// Runs `f` with the tree observers silenced.
pub fn suppressed<R>(w: &mut World, f: impl FnOnce(&mut World) -> R) -> R {
    let set = |w: &mut World, on: bool| {
        if let Some(mut feed) = w.ecs.get_resource_mut::<HierarchyFeed>() {
            feed.suppress = on;
        }
    };
    set(w, true);
    let r = f(w);
    set(w, false);
    r
}

// ---------------------------------------------------------------------------
// VM-side state: property watches and child waiters.
// ---------------------------------------------------------------------------

/// The last values of the watched properties of one instance.
struct Watch {
    values: BTreeMap<String, Value>,
}

#[derive(Default)]
struct Watches {
    by_entity: HashMap<Entity, Watch>,
    last_tick: Option<Tick>,
}

struct ChildWaiter {
    parent: Entity,
    name: String,
    token: YieldToken,
    since: f64,
    /// No timeout: warns once after five seconds.
    infinite: bool,
    warned: bool,
}

#[derive(Default)]
struct ChildWaiters(Vec<ChildWaiter>);

#[derive(Default)]
struct Dispatching(bool);

/// Destroyed subtrees (by root) waiting to be despawned when the cycle settles.
#[derive(Default)]
struct PendingDespawn(Vec<Entity>);

/// How long `WaitForChild` without a timeout waits before it warns.
pub const INFINITE_YIELD_WARNING_SECS: f64 = 5.0;

/// Installs the dispatch state into a new VM.
pub(crate) fn install(lua: &Lua) -> Result<()> {
    lua.set_app_data(Watches::default());
    lua.set_app_data(ChildWaiters::default());
    lua.set_app_data(Dispatching::default());
    lua.set_app_data(PendingDespawn::default());
    Ok(())
}

/// Registers a `WaitForChild` waiter (its thread is parked under `token`).
pub(crate) fn add_child_waiter(
    lua: &Lua,
    parent: Entity,
    name: String,
    token: YieldToken,
    infinite: bool,
) -> Result<()> {
    let since = sched::now(lua)?;
    if let Some(mut w) = lua.app_data_mut::<ChildWaiters>() {
        w.0.push(ChildWaiter {
            parent,
            name,
            token,
            since,
            infinite,
            warned: false,
        });
    }
    Ok(())
}

/// The properties an instance's watch covers, from the listeners on it.
fn watched_props(hub: &SignalHub, e: Entity, all: &dyn Fn() -> Vec<String>) -> Vec<String> {
    let names = hub.names_on(e);
    if names.iter().any(|n| &**n == "Changed") {
        return all();
    }
    names
        .iter()
        .filter_map(|n| n.strip_prefix("PropertyChanged:").map(str::to_string))
        .collect()
}

/// The readable properties of `class`, inherited ones included (`Parent`
/// comes from the tree events instead).
fn all_props(classes: &crate::ClassRegistry, class: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in classes.ancestry(class) {
        for p in &c.properties {
            if p.name != "Parent"
                && matches!(p.status, Status::Implemented | Status::Partial)
                && !out.contains(&p.name)
            {
                out.push(p.name.clone());
            }
        }
    }
    out
}

/// Called when a connection or a `Wait` is made: a `Changed` or
/// property-changed signal starts (or extends) the watch of its instance, from
/// the values it has now.
pub(crate) fn watch_connected(lua: &Lua, key: &SignalKey) -> Result<()> {
    if &*key.name != "Changed" && !key.name.starts_with("PropertyChanged:") {
        return Ok(());
    }
    let st = state(lua)?;
    let classes = st.setup.classes.clone();
    let e = key.entity;
    let props: Vec<String> = {
        let hub = lua.app_data_ref::<SignalHub>().expect("hub present");
        let Ok(class) = with_world(lua, |w| tree::class(w, e).map(str::to_string)) else {
            return Ok(()); // no world bound: the watch starts at the next dispatch
        };
        let Some(class) = class else { return Ok(()) };
        watched_props(&hub, e, &|| all_props(&classes, &class))
    };
    let values: Vec<(String, Value)> = with_world(lua, |w| {
        props
            .iter()
            .filter_map(|p| classes.get_property(w, e, p).ok().map(|v| (p.clone(), v)))
            .collect()
    })?;
    if let Some(mut ws) = lua.app_data_mut::<Watches>() {
        let watch = ws.by_entity.entry(e).or_insert_with(|| Watch {
            values: BTreeMap::new(),
        });
        for (p, v) in values {
            watch.values.entry(p).or_insert(v);
        }
    }
    Ok(())
}

/// A value to pass to a handler.
enum Arg {
    Inst(Entity, Option<Tombstone>),
    Str(String),
    Nil,
}

struct Firing {
    key: SignalKey,
    args: Vec<Arg>,
}

struct Batch {
    fire: Vec<Firing>,
    wake: Vec<(YieldToken, Entity)>,
    cancel: Vec<YieldToken>,
    warn: Vec<String>,
}

impl Batch {
    fn is_empty(&self) -> bool {
        self.fire.is_empty()
            && self.wake.is_empty()
            && self.cancel.is_empty()
            && self.warn.is_empty()
    }
}

fn gather(lua: &Lua, w: &mut World) -> Batch {
    let mut b = Batch {
        fire: Vec::new(),
        wake: Vec::new(),
        cancel: Vec::new(),
        warn: Vec::new(),
    };
    let st = match state(lua) {
        Ok(s) => s,
        Err(_) => return b,
    };
    let classes = st.setup.classes.clone();
    let game = st.game;
    let hub = lua.app_data_ref::<SignalHub>().expect("hub present");
    let push = |b: &mut Batch, e: Entity, name: &str, args: Vec<Arg>| {
        if hub.listens(e, name) {
            b.fire.push(Firing {
                key: SignalKey::new(e, name),
                args,
            });
        }
    };

    // The tree. `moved` holds, for each instance that moved, whether it was
    // in the place before its first move of this batch.
    let mut moved: HashMap<Entity, bool> = HashMap::new();
    let mut order: Vec<Entity> = Vec::new();
    let events = w
        .ecs
        .get_resource_mut::<HierarchyFeed>()
        .map(|mut f| std::mem::take(&mut f.events))
        .unwrap_or_default();
    for ev in events {
        match ev {
            HierEvent::Added { child, parent } => {
                if !tree::is_instance(w, child) {
                    continue;
                }
                moved.entry(child).or_insert(false);
                order.push(child);
                push(&mut b, parent, "ChildAdded", vec![Arg::Inst(child, None)]);
                let mut chain = vec![parent];
                chain.extend(tree::ancestors(w, parent));
                let listening: Vec<Entity> = chain
                    .into_iter()
                    .filter(|a| hub.listens(*a, "DescendantAdded"))
                    .collect();
                let mut sub = vec![child];
                if !listening.is_empty() || !hub.is_empty() {
                    sub.extend(tree::descendants(w, child));
                }
                for d in &sub {
                    for a in &listening {
                        push(&mut b, *a, "DescendantAdded", vec![Arg::Inst(*d, None)]);
                    }
                }
                for d in &sub {
                    push(
                        &mut b,
                        *d,
                        "AncestryChanged",
                        vec![Arg::Inst(child, None), Arg::Inst(parent, None)],
                    );
                }
                push(&mut b, child, "Changed", vec![Arg::Str("Parent".into())]);
                push(&mut b, child, "PropertyChanged:Parent", vec![]);
            }
            HierEvent::Removed {
                child,
                parent,
                chain,
                tomb,
            } => {
                moved.entry(child).or_insert_with(|| chain.contains(&game));
                order.push(child);
                if tree::alive(w, parent) {
                    push(
                        &mut b,
                        parent,
                        "ChildRemoved",
                        vec![Arg::Inst(child, Some(tomb.clone()))],
                    );
                }
                let listening: Vec<Entity> = chain
                    .into_iter()
                    .filter(|a| tree::alive(w, *a) && hub.listens(*a, "DescendantRemoving"))
                    .collect();
                let alive = tree::is_instance(w, child);
                if !listening.is_empty() {
                    let sub: Vec<(Entity, Tombstone)> = if alive {
                        let mut s = vec![(child, tomb.clone())];
                        s.extend(
                            tree::descendants(w, child)
                                .into_iter()
                                .map(|d| (d, Tombstone::of(w, d))),
                        );
                        s
                    } else {
                        vec![(child, tomb.clone())]
                    };
                    for (d, t) in &sub {
                        for a in &listening {
                            push(
                                &mut b,
                                *a,
                                "DescendantRemoving",
                                vec![Arg::Inst(*d, Some(t.clone()))],
                            );
                        }
                    }
                }
                if alive && tree::parent(w, child).is_none() {
                    let mut sub = vec![child];
                    sub.extend(tree::descendants(w, child));
                    for d in &sub {
                        push(
                            &mut b,
                            *d,
                            "AncestryChanged",
                            vec![Arg::Inst(child, None), Arg::Nil],
                        );
                    }
                    push(&mut b, child, "Changed", vec![Arg::Str("Parent".into())]);
                    push(&mut b, child, "PropertyChanged:Parent", vec![]);
                }
            }
        }
    }

    // Attributes.
    for ch in w.drain_attribute_changes() {
        push(
            &mut b,
            ch.entity,
            "AttributeChanged",
            vec![Arg::Str(ch.key.clone())],
        );
        push(
            &mut b,
            ch.entity,
            &format!("AttributeChanged:{}", ch.key),
            vec![],
        );
    }

    // CollectionService tracks the tagged instances inside the place: an
    // instance entering or leaving it counts as its tags being added or
    // removed, and a tag change outside it counts for nothing.
    let cs = crate::services::core::find_service(w, game, "CollectionService");
    let mut covered: HashSet<Entity> = HashSet::new();
    // Outermost first, so a moved subtree counts once, from its root.
    let mut seen: HashSet<Entity> = HashSet::new();
    order.retain(|e| seen.insert(*e));
    order.sort_by_key(|e| tree::ancestors(w, *e).len());
    for root in order {
        if covered.contains(&root) {
            continue;
        }
        let before = moved[&root];
        let now = tree::is_instance(w, root)
            && !tree::is_destroyed(w, root)
            && tree::in_game(w, game, root);
        if before == now || !tree::is_instance(w, root) {
            continue;
        }
        // The subtree, less the parts that moved on their own (they count
        // from their own first move).
        let mut sub = Vec::new();
        let mut stack = vec![root];
        while let Some(d) = stack.pop() {
            if d != root && moved.contains_key(&d) {
                continue;
            }
            sub.push(d);
            stack.extend(tree::children(w, d).into_iter().rev());
        }
        for d in sub {
            covered.insert(d);
            let Some(cs) = cs else { continue };
            for tag in attributes::tags(w, d) {
                let name = if now {
                    format!("InstanceAdded:{tag}")
                } else {
                    format!("InstanceRemoved:{tag}")
                };
                push(&mut b, cs, &name, vec![Arg::Inst(d, None)]);
            }
        }
    }
    let tag_changes = w.drain_tag_changes();
    if let Some(cs) = cs {
        for ch in tag_changes {
            let e = ch.entity;
            if covered.contains(&e)
                || !tree::is_instance(w, e)
                || tree::is_destroyed(w, e)
                || !tree::in_game(w, game, e)
            {
                continue;
            }
            let name = if ch.added {
                format!("InstanceAdded:{}", ch.tag)
            } else {
                format!("InstanceRemoved:{}", ch.tag)
            };
            push(&mut b, cs, &name, vec![Arg::Inst(e, None)]);
        }
    }

    // Properties of watched instances.
    let this_tick = w.ecs.change_tick();
    if let Some(mut ws) = lua.app_data_mut::<Watches>() {
        let last = ws.last_tick;
        let entities: Vec<Entity> = ws.by_entity.keys().copied().collect();
        for e in entities {
            let Some(class) = tree::class(w, e).map(str::to_string) else {
                ws.by_entity.remove(&e);
                continue;
            };
            let props = watched_props(&hub, e, &|| all_props(&classes, &class));
            if props.is_empty() {
                ws.by_entity.remove(&e);
                continue;
            }
            if let Some(last) = last
                && !changed_since(w, e, last, this_tick)
            {
                continue;
            }
            let watch = ws.by_entity.get_mut(&e).expect("listed above");
            for p in props {
                let Ok(v) = classes.get_property(w, e, &p) else {
                    continue;
                };
                match watch.values.get(&p) {
                    Some(old) if *old == v => {}
                    Some(_) => {
                        watch.values.insert(p.clone(), v);
                        push(&mut b, e, "Changed", vec![Arg::Str(p.clone())]);
                        push(&mut b, e, &format!("PropertyChanged:{p}"), vec![]);
                    }
                    None => {
                        watch.values.insert(p, v);
                    }
                }
            }
        }
        ws.last_tick = Some(this_tick);
    }
    w.ecs.increment_change_tick();

    // WaitForChild.
    let now = sched::now(lua).unwrap_or(0.0);
    if let Some(mut waiters) = lua.app_data_mut::<ChildWaiters>() {
        waiters.0.retain_mut(|wt| {
            if !sched::is_parked(lua, wt.token).unwrap_or(false) {
                return false; // timed out, or its thread was stopped
            }
            if !tree::alive(w, wt.parent) {
                b.cancel.push(wt.token);
                return false;
            }
            if let Some(c) = tree::find_child(w, wt.parent, &wt.name, false) {
                b.wake.push((wt.token, c));
                return false;
            }
            if wt.infinite && !wt.warned && now - wt.since >= INFINITE_YIELD_WARNING_SECS - 1e-9 {
                wt.warned = true;
                b.warn.push(format!(
                    "Infinite yield possible on '{}:WaitForChild(\"{}\")'",
                    tree::full_name(w, wt.parent),
                    wt.name
                ));
            }
            true
        });
    }
    let _ = Member::Event; // the event members are resolved by the class layer
    b
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

fn to_lua(lua: &Lua, a: Arg) -> Result<LuaValue> {
    Ok(match a {
        Arg::Inst(e, None) => instance::push(lua, e)?,
        Arg::Inst(e, Some(t)) => instance::push_with_tomb(lua, e, &t)?,
        Arg::Str(s) => LuaValue::String(lua.create_string(&s)?),
        Arg::Nil => LuaValue::Nil,
    })
}

/// Turns the world's pending changes into signal firings and wakes the
/// `WaitForChild` callers whose child arrived. Returns true when it did
/// anything (so the scheduler drains again). Re-entrant calls (from a handler
/// run by an immediate firing) return at once; the outer call picks up what
/// they would have done.
pub fn dispatch(lua: &Lua) -> Result<bool> {
    {
        let Some(mut d) = lua.app_data_mut::<Dispatching>() else {
            return Ok(false);
        };
        if d.0 {
            return Ok(false);
        }
        d.0 = true;
    }
    let r = dispatch_rounds(lua);
    if let Some(mut d) = lua.app_data_mut::<Dispatching>() {
        d.0 = false;
    }
    r
}

/// How many gather-and-fire rounds one dispatch runs at most.
const MAX_ROUNDS: usize = 100;

fn dispatch_rounds(lua: &Lua) -> Result<bool> {
    let behavior = state(lua)?.behavior;
    let mut any = false;
    for _ in 0..MAX_ROUNDS {
        let Ok(batch) = with_world(lua, |w| gather(lua, w)) else {
            return Ok(any); // no host bound
        };
        if batch.is_empty() {
            return Ok(any);
        }
        any = true;
        for t in batch.cancel {
            sched::cancel_park(lua, t)?;
        }
        for line in batch.warn {
            re_script::vm::output(lua, true, line);
        }
        for (token, child) in batch.wake {
            let v = instance::push(lua, child)?;
            sched::wake(lua, token, MultiValue::from_iter([v]), behavior.wake_mode())?;
        }
        for f in batch.fire {
            let args = f
                .args
                .into_iter()
                .map(|a| to_lua(lua, a))
                .collect::<Result<MultiValue>>()?;
            fire_with(lua, &f.key, args, Guard::Connected)?;
        }
        if behavior == SignalBehavior::Deferred {
            // Deferred handlers run when the scheduler drains; it calls
            // dispatch again afterwards.
            return Ok(true);
        }
    }
    Ok(any)
}

/// Despawns the subtree of a destroyed instance when the cycle settles.
pub(crate) fn despawn_later(lua: &Lua, root: Entity) {
    if let Some(mut p) = lua.app_data_mut::<PendingDespawn>() {
        p.0.push(root);
    }
}

/// The settle hook: dispatches what is pending, and once nothing is left,
/// despawns the destroyed subtrees (their handles keep their name and class).
pub fn settle(lua: &Lua) -> Result<bool> {
    if dispatch(lua)? {
        return Ok(true);
    }
    let roots = lua
        .app_data_mut::<PendingDespawn>()
        .map(|mut p| std::mem::take(&mut p.0))
        .unwrap_or_default();
    if roots.is_empty() {
        return Ok(false);
    }
    let Ok(tombs) = with_world(lua, |w| {
        let mut tombs = Vec::new();
        for root in roots {
            if !tree::alive(w, root) {
                continue;
            }
            tombs.push((root, Tombstone::of(w, root)));
            for d in tree::descendants(w, root) {
                tombs.push((d, Tombstone::of(w, d)));
            }
            suppressed(w, |w| {
                let _ = w.despawn_runtime(root);
            });
        }
        // The despawned tags and attributes are no news to anyone.
        w.drain_tag_changes();
        w.drain_attribute_changes();
        tombs
    }) else {
        return Ok(false);
    };
    for (e, t) in tombs {
        instance::bury(lua, e, t)?;
    }
    Ok(false)
}

/// Called after a script changed the world: with immediate signals, the
/// changes fire now.
pub(crate) fn after_write(lua: &Lua) -> Result<()> {
    if state(lua)?.behavior == SignalBehavior::Immediate {
        dispatch(lua)?;
    }
    Ok(())
}
