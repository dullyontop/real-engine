//! [`World`]: the engine-facing facade over bevy_ecs (ADR-0005).

use std::collections::HashMap;

use bevy_ecs::entity::Entity;
use bevy_ecs::schedule::{Schedule, SingleThreadedExecutor};
use bevy_ecs::world::World as EcsWorld;
use re_reflect::{Doc, Registry, Value};
use serde_json::{Value as Json, json};

use crate::attributes::{AttributeChanged, AttributeFeed};
use crate::components::{GlobalTransform, Name, register_builtins};
use crate::error::SceneError;
use crate::hierarchy::{self, children, propagate_all, propagate_changed, subtree};
use crate::ids::{IdGen, IdIndex, StableId};
use crate::procedural::{
    self, Generated, GeneratedFingerprint, Generators, Overrides, RegenReport, Suppressions,
};
use crate::tags::{TagChanged, TagFeed, TagIndex};

/// The world: bevy_ecs storage, the component registry and the generator registry.
///
/// Every engine-facing operation goes through stable ids, component and field
/// names and [`Value`]s; typed bevy queries reach [`World::ecs`] through the
/// [`re_scene::ecs`](crate::ecs) re-export. Schedules run on bevy's
/// `SingleThreadedExecutor`, so bevy's own compute task pool is never started
/// (ADR-0005, ADR-0007).
pub struct World {
    /// The bevy world.
    pub ecs: EcsWorld,
    /// The component registry of `ecs` (ADR-0032).
    pub registry: Registry,
    /// The procedural generators (ADR-0017).
    pub generators: Generators,
    propagate_changed: Schedule,
    propagate_all: Schedule,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

fn single_threaded_schedule() -> Schedule {
    let mut s = Schedule::default();
    s.set_executor(SingleThreadedExecutor::new());
    s
}

impl World {
    /// An empty world with the built-in components registered and no generators.
    pub fn new() -> Self {
        let mut ecs = EcsWorld::new();
        let mut registry = Registry::default();
        register_builtins(&mut registry, &mut ecs);
        Self::from_parts(ecs, registry, Generators::default())
    }

    fn from_parts(mut ecs: EcsWorld, registry: Registry, generators: Generators) -> Self {
        ecs.init_resource::<IdIndex>();
        ecs.init_resource::<TagIndex>();
        ecs.init_resource::<IdGen>();
        ecs.init_resource::<Suppressions>();
        let mut changed = single_threaded_schedule();
        changed.add_systems(propagate_changed);
        let mut all = single_threaded_schedule();
        all.add_systems(propagate_all);
        World {
            ecs,
            registry,
            generators,
            propagate_changed: changed,
            propagate_all: all,
        }
    }

    /// The id index.
    pub fn ids(&self) -> &IdIndex {
        self.ecs.resource::<IdIndex>()
    }

    /// The tag index (tag to entities), kept exact by `Tags`' hooks.
    pub fn tag_index(&self) -> &TagIndex {
        self.ecs.resource::<TagIndex>()
    }

    /// Starts recording [`AttributeChanged`] events (a no-op if already
    /// recording). Until then none are kept.
    pub fn record_attribute_changes(&mut self) {
        self.ecs.init_resource::<AttributeFeed>();
    }

    /// The attribute changes since the last call, in order (empty when not
    /// recording).
    pub fn drain_attribute_changes(&mut self) -> Vec<AttributeChanged> {
        self.ecs
            .get_resource_mut::<AttributeFeed>()
            .map(|mut f| f.drain())
            .unwrap_or_default()
    }

    /// Starts recording [`TagChanged`] events (a no-op if already
    /// recording). Until then none are kept.
    pub fn record_tag_changes(&mut self) {
        self.ecs.init_resource::<TagFeed>();
    }

    /// The tag changes since the last call, in order (empty when not recording).
    pub fn drain_tag_changes(&mut self) -> Vec<TagChanged> {
        self.ecs
            .get_resource_mut::<TagFeed>()
            .map(|mut f| f.drain())
            .unwrap_or_default()
    }

    /// The entity with stable id `id`.
    pub fn entity(&self, id: &str) -> Result<Entity, SceneError> {
        self.ids()
            .get(id)
            .ok_or_else(|| SceneError::UnknownEntity(id.to_string()))
    }

    /// The stable id of `entity`, if it has one.
    pub fn id_of(&self, entity: Entity) -> Option<&str> {
        self.ecs.get::<StableId>(entity).map(|s| s.0.as_str())
    }

    /// The display name of `entity` (empty if none).
    pub fn name_of(&self, entity: Entity) -> &str {
        self.ecs
            .get::<Name>(entity)
            .map(|n| n.0.as_str())
            .unwrap_or("")
    }

    /// A fresh engine-made id (`e7kq2m9x4tb`) not used by any entity.
    pub fn new_id(&mut self) -> String {
        loop {
            let id = self.ecs.resource_mut::<IdGen>().next_id();
            if self.ids().get(&id).is_none() {
                return id;
            }
        }
    }

    /// Resolves a path of names such as `"Island/Crates/Crate_3"`: the first
    /// segment names a root (roots in id order), each further segment a child
    /// (in child order); the first match wins. A convenience, not an identity.
    pub fn find(&self, path: &str) -> Option<Entity> {
        let mut segments = path.split('/');
        let first = segments.next()?;
        let mut roots: Vec<(&str, Entity)> = self
            .ids()
            .iter()
            .filter(|(_, e)| hierarchy::parent(&self.ecs, *e).is_none())
            .collect();
        roots.sort();
        let mut cur = roots
            .into_iter()
            .map(|(_, e)| e)
            .find(|e| self.name_of(*e) == first)?;
        for seg in segments {
            cur = *children(&self.ecs, cur)
                .iter()
                .find(|c| self.name_of(**c) == seg)?;
        }
        Some(cur)
    }

    /// Reads one field by names, e.g. `get("e1", "Transform", "translation")`.
    pub fn get(&self, id: &str, component: &str, field: &str) -> Result<Value, SceneError> {
        let entity = self.entity(id)?;
        let key = self.registry.key(component)?;
        let index = self.registry.schema(key).field_index(field)?;
        self.registry
            .get_field(&self.ecs, entity, key, index)
            .ok_or_else(|| {
                re_reflect::ReflectError::MissingComponent {
                    component: component.to_string(),
                }
                .into()
            })
    }

    /// Reads one field as a text-boundary [`Doc`] (what an agent sees: entity
    /// references as ids).
    pub fn get_doc(&self, id: &str, component: &str, field: &str) -> Result<Doc, SceneError> {
        let value = self.get(id, component, field)?;
        let key = self.registry.key(component)?;
        let schema = self.registry.schema(key);
        let ty = &schema.fields[schema.field_index(field)?].ty;
        Ok(Doc::from_value(&value, ty, &|e| {
            self.id_of(e).map(str::to_string)
        }))
    }

    /// Writes one field by names, coercing the value (an equal value does not
    /// mark the component changed). A direct write: no undo, no override
    /// bookkeeping. Edits go through `re_command` instead.
    pub fn set(
        &mut self,
        id: &str,
        component: &str,
        field: &str,
        value: Value,
    ) -> Result<(), SceneError> {
        let entity = self.entity(id)?;
        let key = self.registry.key(component)?;
        let index = self.registry.schema(key).field_index(field)?;
        self.registry
            .set_field(&mut self.ecs, entity, key, index, value)?;
        Ok(())
    }

    /// Regenerates every procedural source whose generator inputs changed
    /// since its last generation (sources in id order). Failures are reported
    /// in the result and leave the old output in place.
    pub fn regenerate_changed(&mut self) -> RegenReport {
        let mut sources: Vec<(String, Entity)> = self
            .ecs
            .query::<(Entity, &StableId, &procedural::Procedural)>()
            .iter(&self.ecs)
            .map(|(e, id, _)| (id.0.clone(), e))
            .collect();
        sources.sort();
        let mut report = RegenReport::default();
        // A source that lost `Procedural` (baked, or the component removed)
        // keeps no generated output.
        let mut orphaned: Vec<(String, Entity)> = self
            .ecs
            .query_filtered::<(Entity, &StableId), (
                bevy_ecs::query::With<GeneratedFingerprint>,
                bevy_ecs::query::Without<procedural::Procedural>,
            )>()
            .iter(&self.ecs)
            .map(|(e, id)| (id.0.clone(), e))
            .collect();
        orphaned.sort();
        for (_, source) in orphaned {
            report.merge(procedural::clear_output(self, source));
        }
        for (_, source) in sources {
            let Some(fp) = procedural::fingerprint(self, source) else {
                continue;
            };
            let last = self.ecs.get::<GeneratedFingerprint>(source).map(|f| f.0);
            if last != Some(fp) {
                report.merge(self.run_generator(source, fp));
            }
        }
        report
    }

    /// Runs `source`'s generator now, whether or not its inputs changed.
    pub fn regenerate(&mut self, source: Entity) -> RegenReport {
        let fp = procedural::fingerprint(self, source).unwrap_or_default();
        self.run_generator(source, fp)
    }

    fn run_generator(&mut self, source: Entity, fp: u64) -> RegenReport {
        let report = match procedural::regenerate(self, source) {
            Ok(r) => r,
            Err(e) => RegenReport {
                sources: vec![self.id_of(source).unwrap_or("?").to_string()],
                diagnostics: vec![e.to_diagnostic()],
                ..Default::default()
            },
        };
        if let Ok(mut e) = self.ecs.get_entity_mut(source) {
            e.insert(GeneratedFingerprint(fp));
        }
        report
    }

    /// Incremental transform propagation: recomputes `GlobalTransform` for
    /// subtrees whose `Transform` or parent changed.
    pub fn propagate(&mut self) {
        self.propagate_changed.run(&mut self.ecs);
    }

    /// Full transform propagation (every `GlobalTransform`).
    pub fn propagate_all(&mut self) {
        self.propagate_all.run(&mut self.ecs);
    }

    /// One frame of core work: [`World::regenerate_changed`], then [`World::propagate`].
    pub fn update(&mut self) -> RegenReport {
        let report = self.regenerate_changed();
        self.propagate();
        report
    }

    /// The registry description (components, fields, types, defaults and
    /// docs) plus the generators.
    pub fn describe(&self) -> Json {
        let mut d = self.registry.describe();
        let generators: Vec<Json> = self
            .generators
            .iter()
            .map(|g| json!({ "name": g.name, "doc": g.doc, "params": g.params_component }))
            .collect();
        if let Some(obj) = d.as_object_mut() {
            obj.insert("generators".into(), Json::Array(generators));
        }
        d
    }

    /// JSON Schema (2020-12) for scene files with the registered components.
    pub fn scene_json_schema(&self) -> Json {
        self.registry.scene_json_schema()
    }

    /// Suppresses a generated id (`<source id>/<key>`) for the rest of the
    /// session: regeneration will not spawn it again. Does not despawn
    /// anything; play state, never saved.
    pub fn suppress(&mut self, id: &str) -> Result<(), SceneError> {
        match id.split_once('/') {
            Some((source, key)) if !source.is_empty() && !key.is_empty() => {
                self.ecs.resource_mut::<Suppressions>().0.insert(id.into());
                Ok(())
            }
            _ => Err(SceneError::NotGenerated(id.to_string())),
        }
    }

    /// The session's suppressions.
    pub fn suppressions(&self) -> &Suppressions {
        self.ecs.resource::<Suppressions>()
    }

    /// Despawns `entity` and its subtree for a game script: a generated
    /// entity is [suppressed](World::suppress) by key (instead of recording an
    /// override), so a regeneration does not bring it back this session.
    pub fn despawn_runtime(&mut self, entity: Entity) -> Result<(), SceneError> {
        if self.ecs.get_entity(entity).is_err() {
            return Err(SceneError::Invalid(format!(
                "entity {entity} does not exist"
            )));
        }
        if self.ecs.get::<Generated>(entity).is_some()
            && let Some(id) = self.id_of(entity).map(str::to_string)
        {
            self.suppress(&id)?;
        }
        hierarchy::despawn_recursive(&mut self.ecs, entity);
        Ok(())
    }

    /// A copy of this world for `play.run` and `graph.generate`: a fresh
    /// `World` with the same registry (same component keys; built-in, Rust and
    /// script components), the same generators, and every entity with a
    /// stable id copied by value through the registry, including generated
    /// entities (with their keys), overrides, `GlobalTransform` and the
    /// session's suppressions. Ids, child order and [`World::state_hash`] are
    /// identical. ECS resources are not copied, except `IdIndex` and
    /// `TagIndex` (rebuilt by hooks) and the suppressions; change feeds are
    /// not copied either; a caller that needs one of its own
    /// resources in the copy clones it in. Undo history is not part of the
    /// world and is not copied. Components that are not in the registry
    /// (other than the ones named here) are not copied.
    pub fn fork(&self) -> World {
        let mut ecs = EcsWorld::new();
        let registry = self.registry.rebuild_for(&mut ecs);
        let mut out = World::from_parts(ecs, registry, self.generators.clone());
        out.ecs.insert_resource(self.suppressions().clone());

        // Every entity with an id, roots in id order, then depth-first.
        let mut roots: Vec<(&str, Entity)> = self
            .ids()
            .iter()
            .filter(|(_, e)| hierarchy::parent(&self.ecs, *e).is_none())
            .collect();
        roots.sort();
        let order: Vec<Entity> = roots
            .iter()
            .flat_map(|(_, r)| subtree(&self.ecs, *r))
            .filter(|e| self.ecs.get::<StableId>(*e).is_some())
            .collect();

        let mut map: HashMap<Entity, Entity> = HashMap::with_capacity(order.len());
        for &old in &order {
            let id = self.ecs.get::<StableId>(old).cloned();
            let name = self.ecs.get::<Name>(old).cloned().unwrap_or_default();
            let new = out.ecs.spawn((name,)).id();
            if let Some(id) = id {
                out.ecs.entity_mut(new).insert(id);
            }
            if let Some(p) = hierarchy::parent(&self.ecs, old).and_then(|p| map.get(&p)) {
                hierarchy::add_child(&mut out.ecs, *p, new);
            }
            map.insert(old, new);
        }
        let remap = |v: Value| match v {
            Value::Entity(Some(e)) => Value::Entity(map.get(&e).copied()),
            other => other,
        };
        for &old in &order {
            let new = map[&old];
            for key in self.registry.components_of(&self.ecs, old) {
                if let Some(values) = self.registry.get_all(&self.ecs, old, key) {
                    let values = values.into_iter().map(remap).collect();
                    // Same registry layout and already-coerced values: cannot fail.
                    let _ = out.registry.insert(&mut out.ecs, new, key, values);
                }
            }
            let mut e = out.ecs.entity_mut(new);
            if let Some(g) = self.ecs.get::<GlobalTransform>(old) {
                e.insert(*g);
            }
            if let Some(o) = self.ecs.get::<Overrides>(old) {
                e.insert(o.clone());
            }
            if let Some(f) = self.ecs.get::<GeneratedFingerprint>(old) {
                e.insert(*f);
            }
            if let Some(g) = self.ecs.get::<Generated>(old)
                && let Some(source) = map.get(&g.source)
            {
                e.insert(Generated {
                    source: *source,
                    key: g.key.clone(),
                });
            }
        }
        out
    }
}
