//! Stable ids: the identity of an entity in files, commands, logs, undo
//! history, agents and scripts (ADR-0005). Runtime `Entity` handles are never
//! persisted.
//!
//! [`StableId`] is an immutable component; its `on_insert` and `on_discard`
//! hooks keep the [`IdIndex`] resource exact on insert, replace and despawn
//! (including despawn of whole subtrees), so lookups by id are O(1) and cannot
//! drift from the world.
//!
//! Engine-made ids look like `e7kq2m9x4tb` ([`IdGen`]); humans and agents may
//! choose readable ids. `/` is reserved for generated ids
//! (`<source id>/<key>`, ADR-0017).

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::DeferredWorld;

use crate::error::SceneError;

/// The persistent identity of an entity. Immutable: to change an id, despawn
/// and respawn.
#[derive(Component, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[component(immutable, on_insert = index_id, on_discard = unindex_id)]
pub struct StableId(pub String);

fn index_id(mut world: DeferredWorld, ctx: HookContext) {
    let Some(id) = world.get::<StableId>(ctx.entity).map(|s| s.0.clone()) else {
        return;
    };
    if let Some(mut index) = world.get_resource_mut::<IdIndex>() {
        index.map.insert(id, ctx.entity);
    }
}

fn unindex_id(mut world: DeferredWorld, ctx: HookContext) {
    let Some(id) = world.get::<StableId>(ctx.entity).map(|s| s.0.clone()) else {
        return;
    };
    if let Some(mut index) = world.get_resource_mut::<IdIndex>()
        && index.map.get(&id) == Some(&ctx.entity)
    {
        index.map.remove(&id);
    }
}

/// The map from stable id to `Entity`, kept exact by [`StableId`]'s hooks.
#[derive(Resource, Default, Debug)]
pub struct IdIndex {
    map: HashMap<String, Entity>,
}

impl IdIndex {
    /// The entity with this id, if any.
    pub fn get(&self, id: &str) -> Option<Entity> {
        self.map.get(id).copied()
    }
    /// The number of indexed ids.
    pub fn len(&self) -> usize {
        self.map.len()
    }
    /// True if no id is indexed.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    /// Every (id, entity) pair, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Entity)> {
        self.map.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// Generates short random ids such as `e7kq2m9x4tb`: `e` plus ten Crockford
/// base32 characters (50 bits). Seeded from the clock and process id by
/// default; [`IdGen::seeded`] gives a reproducible sequence.
#[derive(Resource, Clone, Debug)]
pub struct IdGen {
    state: u64,
}

impl Default for IdGen {
    fn default() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self::seeded(nanos ^ ((std::process::id() as u64) << 32))
    }
}

impl IdGen {
    /// A generator with a fixed seed.
    pub fn seeded(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next id. Uniqueness against the world is checked by the caller
    /// (`re_scene::World::new_id`).
    pub fn next_id(&mut self) -> String {
        const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
        let mut x = splitmix64(&mut self.state);
        let mut s = String::with_capacity(11);
        s.push('e');
        for _ in 0..10 {
            s.push(ALPHABET[(x & 31) as usize] as char);
            x >>= 5;
        }
        s
    }
}

/// One step of SplitMix64: advances `state` and returns a well-mixed 64-bit value.
pub fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Checks an authored id: non-empty and without `/`.
pub fn validate_id(id: &str) -> Result<(), SceneError> {
    if id.is_empty() || id.contains('/') {
        return Err(SceneError::InvalidId(id.to_string()));
    }
    Ok(())
}
