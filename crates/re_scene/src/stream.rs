//! World streaming hooks (ADR-0042): cells, their keys, and applying or
//! unloading one cell's content without touching the rest of the world.
//!
//! The world has one square grid of **cells**, aligned with world X and Z. A
//! cell is named by the StableKey `c/<x>_<z>` ([`cell_key`]). The streaming
//! manager (`re_proc::stream`) decides which cells are loaded; this module
//! gives it the scene operations it needs:
//!
//! * [`World::apply_cell`]: the ADR-0017 diff for the content of one cell of a
//!   [`Streamed`] procedural source. The content lives under a generated
//!   **cell root** entity (key `c/<x>_<z>`, marked [`LoadedCell`]), and every
//!   key in the fragment starts with `c/<x>_<z>/`, so overrides, deletions,
//!   suppressions and commands address it like any generated entity. The
//!   cost is proportional to the cell, not to the source.
//! * [`World::unload_cell`]: despawns a cell's content without recording
//!   deletion overrides or suppressions. Overrides stay on the source and are
//!   re-applied when the cell loads again; an override of an unloaded cell is
//!   not an orphan.
//! * [`World::stash`] / [`World::unstash_cell`]: authored [`Streamed`] roots
//!   of an unloaded cell are kept as an exact [`Snapshot`] (play state
//!   included) and still written by [`World::save`].
//!
//! * [`World::set_terrain_chunk`] / [`World::remove_terrain_chunk`]: a
//!   source's terrain collision chunks (key `t/<x>_<z>`, marked
//!   [`TerrainChunk`]), generated entities with a static heightfield
//!   [`Collider`] near the focuses.
//!
//! [`StreamCell`] is the description of a cell that scripts and events see.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use glam::Vec3;
use re_core::{Diagnostic, Location};
use re_reflect::{Doc, FieldType, Value, component};
use serde::{Deserialize, Serialize};

use crate::codes;
use crate::components::{Collider, ColliderShape, MaterialRef, MeshRef, Name};
use crate::error::SceneError;
use crate::hierarchy::{self, children};
use crate::ids::StableId;
use crate::procedural::{
    self, GenNode, Generated, OverrideDoc, Overrides, Procedural, RegenReport, Scope,
    generated_children,
};
use crate::scene::{EntityDoc, GeneratedDoc, SaveOptions, Snapshot};
use crate::world::World;

component! {
    /// Streams this entity by cell (ADR-0042). On a procedural source (which
    /// must have an identity transform), its generator's output is applied
    /// per cell; on an authored root entity, the entity and its subtree are
    /// unloaded with the cell holding its translation.
    pub struct Streamed {
        /// For a procedural source: the half side, in metres, of the square
        /// around the world origin it generates (cells touching it are its
        /// cells); 0 is unbounded. Unused on authored entities.
        pub bounds: f32 = 0.0,
    }
}

component! {
    /// A point the streamed world loads around: the player, a vehicle, the
    /// editor camera. With none, the active camera is the focus.
    pub struct StreamFocus {}
}

/// Marks the root entity of one loaded cell of a streamed source. Runtime
/// only: never saved.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct LoadedCell {
    /// The cell's key (`c/<x>_<z>`).
    pub key: String,
}

/// Marks a terrain collision chunk of a streamed source (ADR-0042).
/// Runtime only: never saved.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct TerrainChunk {
    /// The chunk's key (`t/<x>_<z>`).
    pub key: String,
}

/// The key of terrain collision chunk `(x, z)`: `t/<x>_<z>`.
pub fn chunk_key(x: i32, z: i32) -> String {
    format!("t/{x}_{z}")
}

/// The coordinates of a terrain chunk key (`t/<x>_<z>`, canonical decimal
/// integers), or `None` if `key` is not one.
pub fn parse_chunk_key(key: &str) -> Option<[i32; 2]> {
    parse_cell_key(&format!("c/{}", key.strip_prefix("t/")?))
}

/// One cell of the streamed world, as scripts and events see it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamCell {
    /// The cell's StableKey, `c/<x>_<z>`.
    pub key: String,
    /// The cell's grid coordinates `[x, z]`.
    pub coord: [i32; 2],
    /// The smallest corner of its world bounds (Y from its content).
    pub min: Vec3,
    /// The largest corner of its world bounds.
    pub max: Vec3,
    /// Stable ids of the streamed sources with content in it, sorted.
    pub sources: Vec<String>,
    /// The generator-version stamp (hex) of the content (`re_proc::version_stamp`).
    pub version: String,
}

/// The key of cell `(x, z)`: `c/<x>_<z>`.
pub fn cell_key(x: i32, z: i32) -> String {
    format!("c/{x}_{z}")
}

/// The coordinates of a cell key (`c/<x>_<z>`, canonical decimal integers),
/// or `None` if `key` is not one.
pub fn parse_cell_key(key: &str) -> Option<[i32; 2]> {
    let (x, z) = key.strip_prefix("c/")?.split_once('_')?;
    let parse = |t: &str| -> Option<i32> {
        let v: i32 = t.parse().ok()?;
        // Only the canonical spelling, so one cell has one key.
        (v.to_string() == t).then_some(v)
    };
    Some([parse(x)?, parse(z)?])
}

/// The cell key a generated key lies under (`c/3_-2` for `c/3_-2/rocks/7`),
/// or `None`.
pub fn in_cell(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("c/")?;
    let end = 2 + rest.find('/')?;
    let cell = &key[..end];
    parse_cell_key(cell).map(|_| cell)
}

/// The cell a generated key belongs to: the cell itself for a cell key
/// (`c/3_-2`, its root entity), or the cell it lies under (`c/3_-2/rocks/7`).
pub fn owning_cell(key: &str) -> Option<&str> {
    in_cell(key).or_else(|| parse_cell_key(key).map(|_| key))
}

/// True if `key` lies under `cell` (`c/1_2/a` is under `c/1_2`).
pub fn under(key: &str, cell: &str) -> bool {
    key.strip_prefix(cell).is_some_and(|r| r.starts_with('/'))
}

/// One authored root kept while its cell is unloaded.
#[derive(Clone, Debug, PartialEq)]
pub struct StashedRoot {
    /// The root's stable id.
    pub id: String,
    /// The subtree, exactly (play state included).
    pub snapshot: Snapshot,
    /// Live entities outside the subtree whose entity fields pointed into it
    /// when it was stashed; re-pointed when it comes back.
    holders: Vec<Entity>,
}

/// Authored [`Streamed`] roots of unloaded cells, by cell key. Their ids
/// stay reserved ([`World::id_taken`]) while they are stashed.
#[derive(Resource, Clone, Debug, Default)]
pub struct Stash {
    cells: BTreeMap<String, Vec<StashedRoot>>,
    /// Every stashed entity id (roots and descendants), to its cell.
    ids: HashMap<String, String>,
    /// The handles the stashed entities had, to their ids: entity fields
    /// elsewhere still hold them, and saving writes them as these ids.
    pub(crate) gone: HashMap<Entity, String>,
}

impl Stash {
    /// Every stashed root, by cell key then id.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &StashedRoot)> {
        self.cells
            .iter()
            .flat_map(|(k, v)| v.iter().map(move |r| (k.as_str(), r)))
    }

    /// The stashed root with stable id `id`, if any.
    pub fn root(&self, id: &str) -> Option<&StashedRoot> {
        let cell = self.ids.get(id)?;
        self.cells[cell].iter().find(|r| r.id == id)
    }

    /// The cell holding the stashed entity `id` (a root or one of its
    /// descendants), if any.
    pub fn cell_of(&self, id: &str) -> Option<&str> {
        self.ids.get(id).map(String::as_str)
    }

    /// True if nothing is stashed.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    fn push(&mut self, cell: &str, root: StashedRoot) {
        for d in &root.snapshot.entities {
            self.ids.insert(d.id.clone(), cell.to_string());
        }
        let list = self.cells.entry(cell.to_string()).or_default();
        list.push(root);
        list.sort_by(|a, b| a.id.cmp(&b.id));
    }

    fn take(&mut self, cell: &str) -> Option<Vec<StashedRoot>> {
        let roots = self.cells.remove(cell)?;
        for r in &roots {
            for d in &r.snapshot.entities {
                self.ids.remove(&d.id);
            }
        }
        Some(roots)
    }

    fn put_back(&mut self, cell: &str, roots: Vec<StashedRoot>) {
        for r in roots {
            self.push(cell, r);
        }
    }

    /// The id a stashed entity's old handle stands for.
    pub(crate) fn gone_id(&self, e: Entity) -> Option<&str> {
        self.gone.get(&e).map(String::as_str)
    }

    /// The stash for a copy of the world (`World::fork`), its handles mapped
    /// through `map` (holders the copy does not have are dropped).
    pub(crate) fn mapped(&self, map: &HashMap<Entity, Entity>) -> Stash {
        let mut out = self.clone();
        out.gone = self
            .gone
            .iter()
            .filter_map(|(e, id)| Some((*map.get(e)?, id.clone())))
            .collect();
        for roots in out.cells.values_mut() {
            for r in roots {
                r.holders = r
                    .holders
                    .iter()
                    .filter_map(|e| map.get(e).copied())
                    .collect();
            }
        }
        out
    }
}

/// True if a field of type `ty` can hold an entity reference.
fn holds_entity(ty: &FieldType) -> bool {
    match ty {
        FieldType::Entity => true,
        FieldType::List(item) => holds_entity(item),
        FieldType::Record(id) => id.schema().fields.iter().any(|f| holds_entity(&f.ty)),
        _ => false,
    }
}

/// True if `v` references an entity of `set`.
fn refers(v: &Value, set: &HashSet<Entity>) -> bool {
    match v {
        Value::Entity(Some(e)) => set.contains(e),
        Value::List(items) | Value::Record(_, items) => items.iter().any(|i| refers(i, set)),
        _ => false,
    }
}

/// Re-points references in `v` through `map`; true if anything changed.
fn remap(v: &mut Value, map: &HashMap<Entity, Entity>) -> bool {
    match v {
        Value::Entity(Some(e)) => match map.get(e) {
            Some(n) => {
                *e = *n;
                true
            }
            None => false,
        },
        Value::List(items) | Value::Record(_, items) => {
            let mut changed = false;
            for i in items {
                changed |= remap(i, map);
            }
            changed
        }
        _ => false,
    }
}

fn invalid(message: String) -> SceneError {
    SceneError::Invalid(message)
}

impl World {
    fn streamed_source(&self, source: Entity) -> Result<String, SceneError> {
        let id = self
            .id_of(source)
            .ok_or_else(|| invalid("a streamed source needs a stable id".into()))?
            .to_string();
        if self.ecs.get::<Procedural>(source).is_none() {
            return Err(invalid(format!("{id:?} is not a procedural source")));
        }
        if self.ecs.get::<Streamed>(source).is_none() {
            return Err(invalid(format!(
                "{id:?} is not streamed (no Streamed component)"
            )));
        }
        Ok(id)
    }

    /// The root entity of `source`'s loaded cell `cell`, if it is loaded.
    pub fn cell_root(&self, source: Entity, cell: &str) -> Option<Entity> {
        let id = format!("{}/{cell}", self.id_of(source)?);
        let e = self.ids().get(&id)?;
        self.ecs.get::<LoadedCell>(e).is_some().then_some(e)
    }

    /// Sets `source`'s terrain collision chunk `key` (`t/<x>_<z>`): a
    /// generated child entity `<source id>/t/<x>_<z>` at the source's origin
    /// with a static heightfield [`Collider`] naming `heightfield` (whose
    /// samples carry their own offset). An existing chunk gets the new
    /// heightfield; returns the one it named before. A chunk deleted by an
    /// override is not created.
    pub fn set_terrain_chunk(
        &mut self,
        source: Entity,
        key: &str,
        heightfield: &str,
    ) -> Result<Option<String>, SceneError> {
        let src_id = self.streamed_source(source)?;
        let [x, z] = parse_chunk_key(key)
            .ok_or_else(|| invalid(format!("{key:?} is not a terrain chunk key (t/<x>_<z>)")))?;
        let deleted = self
            .ecs
            .get::<Overrides>(source)
            .is_some_and(|o| o.is_deleted(key));
        if deleted {
            return Ok(None);
        }
        if let Some(e) = self.terrain_chunk(source, key) {
            let Some(mut c) = self.ecs.get_mut::<Collider>(e) else {
                return Ok(None);
            };
            if c.source == heightfield {
                return Ok(None);
            }
            return Ok(Some(std::mem::replace(
                &mut c.source,
                heightfield.to_string(),
            )));
        }
        let id = format!("{src_id}/{key}");
        if self.ids().get(&id).is_some() {
            return Err(invalid(format!(
                "id {id:?} is already taken by another entity"
            )));
        }
        let transform = self.registry.key("Transform")?;
        let defaults = self.registry.schema(transform).defaults();
        let e = self
            .ecs
            .spawn((
                StableId(id),
                Name(format!("Terrain chunk {x},{z}")),
                Generated {
                    source,
                    key: key.to_string(),
                },
                TerrainChunk {
                    key: key.to_string(),
                },
                Collider {
                    shape: ColliderShape::Heightfield,
                    source: heightfield.to_string(),
                    ..Collider::default()
                },
            ))
            .id();
        self.registry
            .insert(&mut self.ecs, e, transform, defaults)?;
        hierarchy::add_child(&mut self.ecs, source, e);
        Ok(None)
    }

    /// Despawns `source`'s terrain collision chunk `key`; returns the
    /// heightfield it named, or `None` if there was none.
    pub fn remove_terrain_chunk(&mut self, source: Entity, key: &str) -> Option<String> {
        let e = self.terrain_chunk(source, key)?;
        let heightfield = self.ecs.get::<Collider>(e).map(|c| c.source.clone());
        hierarchy::despawn_recursive(&mut self.ecs, e);
        heightfield
    }

    /// `source`'s terrain collision chunk `key`, if it exists.
    pub fn terrain_chunk(&self, source: Entity, key: &str) -> Option<Entity> {
        let id = format!("{}/{key}", self.id_of(source)?);
        let e = self.ids().get(&id)?;
        self.ecs.get::<TerrainChunk>(e).is_some().then_some(e)
    }

    /// The keys of `source`'s terrain collision chunks, sorted.
    pub fn terrain_chunks(&self, source: Entity) -> Vec<String> {
        let mut out: Vec<String> = children(&self.ecs, source)
            .iter()
            .filter_map(|e| self.ecs.get::<TerrainChunk>(*e).map(|c| c.key.clone()))
            .collect();
        out.sort();
        out
    }

    /// The keys of `source`'s loaded cells, sorted.
    pub fn loaded_cells(&self, source: Entity) -> Vec<String> {
        let mut out: Vec<String> = children(&self.ecs, source)
            .iter()
            .filter_map(|e| self.ecs.get::<LoadedCell>(*e).map(|c| c.key.clone()))
            .collect();
        out.sort();
        out
    }

    /// Applies `fragment` as the content of cell `cell` of the streamed
    /// procedural source `source` (ADR-0042), creating the cell's root entity
    /// if the cell was not loaded. Every key in `fragment` must lie under
    /// `cell`; a node without a parent key becomes a child of the cell root.
    ///
    /// The diff is ADR-0017's, restricted to the cell: surviving keys keep
    /// their `Entity`, overrides and suppressions under the cell are applied,
    /// and only overrides under the cell can be reported as orphans. On error
    /// nothing changes.
    pub fn apply_cell(
        &mut self,
        source: Entity,
        cell: &str,
        fragment: &[GenNode],
    ) -> Result<RegenReport, SceneError> {
        let src_id = self.streamed_source(source)?;
        let [x, z] = parse_cell_key(cell)
            .ok_or_else(|| invalid(format!("{cell:?} is not a cell key (c/<x>_<z>)")))?;
        let generator = self
            .ecs
            .get::<Procedural>(source)
            .map(|p| p.generator.clone());
        let generator = generator.unwrap_or_default();
        let root_id = format!("{src_id}/{cell}");
        let overrides = self
            .ecs
            .get::<Overrides>(source)
            .cloned()
            .unwrap_or_default();
        // A deleted or suppressed cell root takes the cell's content with it.
        if overrides.is_deleted(cell) || self.suppressions().contains(&root_id) {
            let mut report = self.unload_cell(source, cell)?;
            report.despawned.clear();
            report.released_assets.clear();
            return Ok(report);
        }
        let (root, created) = match self.cell_root(source, cell) {
            Some(r) => (r, false),
            None => {
                if self.ids().get(&root_id).is_some() {
                    return Err(SceneError::GeneratorFailed {
                        source_id: src_id,
                        message: format!("id {root_id:?} is already taken by another entity"),
                        cause: None,
                    });
                }
                let transform = self.registry.key("Transform")?;
                let defaults = self.registry.schema(transform).defaults();
                let e = self
                    .ecs
                    .spawn((
                        StableId(root_id.clone()),
                        Name(format!("Cell {x},{z}")),
                        Generated {
                            source,
                            key: cell.to_string(),
                        },
                        LoadedCell {
                            key: cell.to_string(),
                        },
                    ))
                    .id();
                self.registry
                    .insert(&mut self.ecs, e, transform, defaults)?;
                hierarchy::add_child(&mut self.ecs, source, e);
                (e, true)
            }
        };
        let result = procedural::apply_output(
            self,
            source,
            &src_id,
            &generator,
            Scope::Cell { root, key: cell },
            fragment,
        );
        match result {
            Ok(mut report) => {
                self.apply_root_overrides(root, &src_id, cell, &overrides, &mut report);
                if created {
                    report.spawned.insert(0, root_id);
                    self.order_cells(source);
                }
                Ok(report)
            }
            Err(e) => {
                if created {
                    hierarchy::despawn_recursive(&mut self.ecs, root);
                }
                Err(e)
            }
        }
    }

    /// Sets the field overrides that target the cell root `cell` (an edited
    /// cell root keeps its edits across unloads, like any generated entity).
    fn apply_root_overrides(
        &mut self,
        root: Entity,
        src_id: &str,
        cell: &str,
        overrides: &Overrides,
        report: &mut RegenReport,
    ) {
        for o in &overrides.0 {
            let OverrideDoc::Field {
                target,
                component,
                field,
                value,
            } = o
            else {
                continue;
            };
            if target != cell {
                continue;
            }
            let applied = (|| -> Result<(), SceneError> {
                let key = self.registry.key(component)?;
                let schema = self.registry.schema(key);
                let index = schema.field_index(field)?;
                let ids = self.ids();
                let v = value
                    .to_value(&schema.fields[index].ty, &|id| ids.get(id))
                    .map_err(|error| re_reflect::ReflectError::Value {
                        path: format!("{component}.{field}"),
                        error,
                    })?;
                if !self.registry.has(&self.ecs, root, key) {
                    return Err(re_reflect::ReflectError::MissingComponent {
                        component: component.clone(),
                    }
                    .into());
                }
                self.registry
                    .set_field(&mut self.ecs, root, key, index, v)?;
                Ok(())
            })();
            if let Err(e) = applied {
                report.diagnostics.push(
                    Diagnostic::warning(
                        codes::OVERRIDE_NOT_APPLIED,
                        format!("{src_id}: override on {target:?} not applied: {e}"),
                    )
                    .at(Location::entity(src_id.to_string()))
                    .with_related(e.to_diagnostic()),
                );
            }
        }
    }

    /// Puts `source`'s cell roots after its other generated children, in key order.
    fn order_cells(&mut self, source: Entity) {
        let generated: Vec<Entity> = generated_children(self, source)
            .into_iter()
            .filter(|e| self.ecs.get::<LoadedCell>(*e).is_none())
            .collect();
        procedural::order_generated_children(self, source, source, &generated);
    }

    /// Despawns the content of `source`'s cell `cell` and its root, without
    /// recording deletion overrides or suppressions (ADR-0042). Returns the
    /// despawned ids (sorted) and the `MeshRef` / `MaterialRef` assets nothing
    /// references any more. Unloading a cell that is not loaded does nothing.
    pub fn unload_cell(&mut self, source: Entity, cell: &str) -> Result<RegenReport, SceneError> {
        let src_id = self.streamed_source(source)?;
        let mut report = RegenReport::default();
        let Some(root) = self.cell_root(source, cell) else {
            return Ok(report);
        };
        let mut old_assets = BTreeSet::new();
        for e in hierarchy::subtree(&self.ecs, root) {
            if let Some(id) = self.id_of(e) {
                report.despawned.push(id.to_string());
            }
            for a in [
                self.ecs.get::<MeshRef>(e).map(|m| m.asset.clone()),
                self.ecs.get::<MaterialRef>(e).map(|m| m.asset.clone()),
            ]
            .into_iter()
            .flatten()
            {
                if !a.is_empty() {
                    old_assets.insert(a);
                }
            }
        }
        report.despawned.sort();
        report.sources.push(src_id);
        hierarchy::despawn_recursive(&mut self.ecs, root);
        report.released_assets = self.unreferenced(old_assets);
        Ok(report)
    }

    /// The assets of `candidates` that no entity references any more, sorted.
    fn unreferenced(&mut self, mut candidates: BTreeSet<String>) -> Vec<String> {
        // Only entities with a reference are visited, and nothing is cloned:
        // this runs on every cell unload, over the whole world.
        let mut meshes = self.ecs.query::<&MeshRef>();
        for m in meshes.iter(&self.ecs) {
            if candidates.is_empty() {
                return Vec::new();
            }
            candidates.remove(&m.asset);
        }
        let mut materials = self.ecs.query::<&MaterialRef>();
        for m in materials.iter(&self.ecs) {
            if candidates.is_empty() {
                return Vec::new();
            }
            candidates.remove(&m.asset);
        }
        candidates.into_iter().collect()
    }

    /// Unloads the authored [`Streamed`] root `root` into cell `cell`'s stash:
    /// its subtree is snapshotted exactly (play state included) and despawned.
    /// Only root entities (no parent) can be stashed. [`World::save`] still
    /// writes the stashed authored entities. Returns how many entities
    /// outside the subtree reference into it: those references read as empty
    /// until it comes back (`RE4303` for the streaming manager), then point at
    /// it again.
    pub fn stash(&mut self, root: Entity, cell: &str) -> Result<usize, SceneError> {
        let id = self
            .id_of(root)
            .ok_or_else(|| invalid("a streamed root needs a stable id".into()))?
            .to_string();
        if parse_cell_key(cell).is_none() {
            return Err(invalid(format!("{cell:?} is not a cell key (c/<x>_<z>)")));
        }
        if self.ecs.get::<Streamed>(root).is_none() {
            return Err(invalid(format!(
                "{id:?} is not streamed (no Streamed component)"
            )));
        }
        if self.ecs.get::<Generated>(root).is_some() || hierarchy::parent(&self.ecs, root).is_some()
        {
            return Err(invalid(format!(
                "{id:?}: only authored root entities stream on their own; move the Streamed component to the subtree's root"
            )));
        }
        let snapshot = self.snapshot_subtree(root);
        let inside: HashSet<Entity> = hierarchy::subtree(&self.ecs, root).into_iter().collect();
        let holders = self.holders_of(&inside);
        let outside = holders.len();
        let gone: Vec<(Entity, String)> = inside
            .iter()
            .filter_map(|e| Some((*e, self.id_of(*e)?.to_string())))
            .collect();
        hierarchy::despawn_recursive(&mut self.ecs, root);
        let mut stash = self.ecs.remove_resource::<Stash>().unwrap_or_default();
        stash.gone.extend(gone);
        stash.push(
            cell,
            StashedRoot {
                id,
                snapshot,
                holders,
            },
        );
        self.ecs.insert_resource(stash);
        Ok(outside)
    }

    /// Brings back every authored root stashed for cell `cell`, in id order,
    /// as new entities. On error the cell stays stashed and nothing is added.
    pub fn unstash_cell(&mut self, cell: &str) -> Result<Vec<Entity>, SceneError> {
        let Some(mut stash) = self.ecs.remove_resource::<Stash>() else {
            return Ok(Vec::new());
        };
        let Some(roots) = stash.take(cell) else {
            self.ecs.insert_resource(stash);
            return Ok(Vec::new());
        };
        let mut spawned = Vec::new();
        for r in &roots {
            match self.restore(&r.snapshot, None) {
                Ok(es) => spawned.push(es[0]),
                Err(e) => {
                    for &s in &spawned {
                        hierarchy::despawn_recursive(&mut self.ecs, s);
                    }
                    stash.put_back(cell, roots);
                    self.ecs.insert_resource(stash);
                    return Err(e);
                }
            }
        }
        // Entity fields that pointed into the stashed subtrees follow them.
        let back: HashSet<&str> = roots
            .iter()
            .flat_map(|r| r.snapshot.entities.iter().map(|d| d.id.as_str()))
            .collect();
        let mut map = HashMap::new();
        stash.gone.retain(|old, id| {
            if !back.contains(id.as_str()) {
                return true;
            }
            if let Some(new) = self.ids().get(id) {
                map.insert(*old, new);
            }
            false
        });
        for r in &roots {
            for &holder in &r.holders {
                self.remap_refs(holder, &map);
            }
        }
        self.ecs.insert_resource(stash);
        Ok(spawned)
    }

    /// The live entities outside `inside` with an entity field pointing into it.
    fn holders_of(&self, inside: &HashSet<Entity>) -> Vec<Entity> {
        let keys: Vec<_> = self
            .registry
            .keys()
            .filter(|k| {
                self.registry
                    .schema(*k)
                    .fields
                    .iter()
                    .any(|f| holds_entity(&f.ty))
            })
            .collect();
        if keys.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<Entity> = self
            .ids()
            .iter()
            .map(|(_, e)| e)
            .filter(|e| !inside.contains(e))
            .filter(|&e| {
                keys.iter().any(|&k| {
                    self.registry
                        .get_all(&self.ecs, e, k)
                        .is_some_and(|vs| vs.iter().any(|v| refers(v, inside)))
                })
            })
            .collect();
        out.sort();
        out
    }

    /// Re-points `holder`'s entity fields through `map`.
    fn remap_refs(&mut self, holder: Entity, map: &HashMap<Entity, Entity>) {
        if self.ecs.get_entity(holder).is_err() {
            return;
        }
        for k in self.registry.components_of(&self.ecs, holder) {
            let Some(values) = self.registry.get_all(&self.ecs, holder, k) else {
                continue;
            };
            for (i, mut v) in values.into_iter().enumerate() {
                if remap(&mut v, map) {
                    // Same field type, already coerced: cannot fail.
                    let _ = self.registry.set_field(&mut self.ecs, holder, k, i, v);
                }
            }
        }
    }

    /// The authored documents of a stashed root, as [`World::save`] writes
    /// them (generated entities left out; defaults dropped when `opts` says so).
    pub(crate) fn stashed_docs(&self, root: &StashedRoot, opts: SaveOptions) -> Vec<EntityDoc> {
        // Like `subtree_docs`: a generated entity and everything under it
        // (authored children included) are left out. Parents come first.
        let mut skipped: HashSet<String> = root
            .snapshot
            .generated
            .iter()
            .map(GeneratedDoc::id)
            .collect();
        for d in &root.snapshot.entities {
            if d.parent.as_ref().is_some_and(|p| skipped.contains(p)) {
                skipped.insert(d.id.clone());
            }
        }
        root.snapshot
            .entities
            .iter()
            .filter(|d| !skipped.contains(&d.id))
            .map(|d| {
                let mut d = d.clone();
                if opts.omit_defaults {
                    for (name, fields) in d.components.iter_mut() {
                        let Some(key) = self.registry.try_key(name) else {
                            continue;
                        };
                        let schema = self.registry.schema(key);
                        fields.retain(|fname, value| {
                            schema
                                .fields
                                .iter()
                                .find(|f| &f.name == fname)
                                .is_none_or(|f| {
                                    *value != Doc::from_value(&f.default, &f.ty, &|_| None)
                                })
                        });
                    }
                }
                d
            })
            .collect()
    }

    /// The stash of unloaded authored roots.
    pub fn stashed(&self) -> Option<&Stash> {
        self.ecs.get_resource::<Stash>()
    }
}
