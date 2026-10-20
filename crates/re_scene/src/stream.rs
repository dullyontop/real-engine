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
//! [`StreamCell`] is the description of a cell that scripts and events see.

use std::collections::BTreeMap;

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use glam::Vec3;
use re_reflect::{Doc, component};
use serde::{Deserialize, Serialize};

use crate::components::{MaterialRef, MeshRef, Name};
use crate::error::SceneError;
use crate::hierarchy::{self, children};
use crate::ids::StableId;
use crate::procedural::{
    self, GenNode, Generated, Procedural, RegenReport, Scope, generated_children,
};
use crate::scene::{EntityDoc, GeneratedDoc, SaveOptions, Snapshot};
use crate::world::World;

component! {
    /// Streams this entity by cell (ADR-0042). On a procedural source (which
    /// must have an identity transform), its generator's output is applied
    /// per cell; on an authored root entity, the entity and its subtree are
    /// unloaded with the cell holding its translation.
    pub struct Streamed {}
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
}

/// Authored [`Streamed`] roots of unloaded cells, by cell key.
#[derive(Resource, Clone, Debug, Default)]
pub struct Stash {
    pub(crate) cells: BTreeMap<String, Vec<StashedRoot>>,
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
        self.iter().map(|(_, r)| r).find(|r| r.id == id)
    }

    /// The cell holding the stashed entity `id` (a root or one of its
    /// descendants), if any.
    pub fn cell_of(&self, id: &str) -> Option<&str> {
        self.iter()
            .find(|(_, r)| r.snapshot.entities.iter().any(|d| d.id == id))
            .map(|(k, _)| k)
    }

    /// True if nothing is stashed.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
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
        let mut old_assets = std::collections::BTreeSet::new();
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
    fn unreferenced(&mut self, candidates: std::collections::BTreeSet<String>) -> Vec<String> {
        if candidates.is_empty() {
            return Vec::new();
        }
        let mut live = std::collections::HashSet::new();
        let mut q = self.ecs.query::<(Option<&MeshRef>, Option<&MaterialRef>)>();
        for (m, mat) in q.iter(&self.ecs) {
            if let Some(m) = m {
                live.insert(m.asset.clone());
            }
            if let Some(mat) = mat {
                live.insert(mat.asset.clone());
            }
        }
        candidates
            .into_iter()
            .filter(|a| !live.contains(a))
            .collect()
    }

    /// Unloads the authored [`Streamed`] root `root` into cell `cell`'s stash:
    /// its subtree is snapshotted exactly (play state included) and despawned.
    /// Only root entities (no parent) can be stashed. [`World::save`] still
    /// writes the stashed authored entities.
    pub fn stash(&mut self, root: Entity, cell: &str) -> Result<(), SceneError> {
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
        hierarchy::despawn_recursive(&mut self.ecs, root);
        let mut stash = self.ecs.remove_resource::<Stash>().unwrap_or_default();
        let list = stash.cells.entry(cell.to_string()).or_default();
        list.push(StashedRoot { id, snapshot });
        list.sort_by(|a, b| a.id.cmp(&b.id));
        self.ecs.insert_resource(stash);
        Ok(())
    }

    /// Brings back every authored root stashed for cell `cell`, in id order,
    /// as new entities. On error the cell stays stashed and nothing is added.
    pub fn unstash_cell(&mut self, cell: &str) -> Result<Vec<Entity>, SceneError> {
        let Some(mut stash) = self.ecs.remove_resource::<Stash>() else {
            return Ok(Vec::new());
        };
        let Some(roots) = stash.cells.remove(cell) else {
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
                    stash.cells.insert(cell.to_string(), roots);
                    self.ecs.insert_resource(stash);
                    return Err(e);
                }
            }
        }
        self.ecs.insert_resource(stash);
        Ok(spawned)
    }

    /// The authored documents of a stashed root, as [`World::save`] writes
    /// them (generated entities left out; defaults dropped when `opts` says so).
    pub(crate) fn stashed_docs(&self, root: &StashedRoot, opts: SaveOptions) -> Vec<EntityDoc> {
        let generated: std::collections::HashSet<String> = root
            .snapshot
            .generated
            .iter()
            .map(GeneratedDoc::id)
            .collect();
        root.snapshot
            .entities
            .iter()
            .filter(|d| !generated.contains(&d.id))
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
