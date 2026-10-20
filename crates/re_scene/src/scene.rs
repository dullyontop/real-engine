//! Scene files (ADR-0016, ARCHITECTURE §8): a flat, ordered list of entities,
//! each with a stable id, an optional name and parent id, its components by
//! name and, on procedural sources, its overrides.
//!
//! Ordering is deterministic so diffs stay small: roots sorted by id, then
//! depth-first with siblings in `Children` order; components sorted by name;
//! fields in declaration order. Lists of scalars are written on one line.
//! Generated entities are not written: their source's generator settings and
//! overrides are. The schemas of every script-defined component the world has
//! registered (including those only generated entities use) are declared in a
//! top-level `script_components` list, so the file loads before any script
//! runs. A declaration is kept until it is removed from the file while no
//! world that loaded it saves again. Save → load → save is byte-identical.

use std::collections::{BTreeMap, HashSet};

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::query::Without;
use bevy_ecs::resource::Resource;
use indexmap::IndexMap;
use re_reflect::{ComponentKey, ComponentOrigin, ComponentSchemaDoc, Doc, Value};
use serde::{Deserialize, Serialize};

use crate::components::Name;
use crate::error::SceneError;
use crate::hierarchy::{self, children};
use crate::ids::StableId;
use crate::ids::validate_id;
use crate::procedural::{
    self, Generated, GeneratedFingerprint, OverrideDoc, Overrides, Procedural,
};
use crate::world::World;

/// The `format` value of a scene file.
pub const FORMAT: &str = re_reflect::schema::SCENE_FORMAT;
/// The scene file version this build reads and writes.
pub const VERSION: u32 = re_reflect::schema::SCENE_VERSION;

/// A scene file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SceneDoc {
    /// The optional `$schema` key (a path to `scene.schema.json`), kept as written.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Always [`FORMAT`] (`"real-engine/scene"`).
    pub format: String,
    /// The file version ([`VERSION`]).
    pub version: u32,
    /// Schemas of the script-defined components the saving world had
    /// registered, sorted by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_components: Vec<ComponentSchemaDoc>,
    /// The entities, parents before children.
    pub entities: Vec<EntityDoc>,
}

/// The components of an entity in file form: component name → field name → value.
pub type ComponentsDoc = IndexMap<String, IndexMap<String, Doc>>;

/// One entity of a scene or prefab file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EntityDoc {
    /// The stable id.
    pub id: String,
    /// The display name (omitted when empty).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The parent's id; it must appear earlier in the file or exist in the world.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Components and their fields; missing fields take their defaults.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub components: ComponentsDoc,
    /// Overrides of generated entities (procedural sources only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<OverrideDoc>,
}

/// A generated entity of a [`Snapshot`]: its source's id and its key. The
/// entity's id is `<source>/<key>`; the source is named explicitly because
/// both source ids and keys may contain `/`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GeneratedDoc {
    /// The procedural source's id.
    pub source: String,
    /// The key within the source's output.
    pub key: String,
}

impl GeneratedDoc {
    /// The generated entity's id, `<source>/<key>`.
    pub fn id(&self) -> String {
        format!("{}/{}", self.source, self.key)
    }
}

/// An exact copy of a subtree, generated entities included
/// ([`World::snapshot_subtree`], [`World::restore`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Every entity of the subtree, root first, pre-order, with every field.
    pub entities: Vec<EntityDoc>,
    /// The generated entities among them.
    pub generated: Vec<GeneratedDoc>,
    /// Ids of the procedural sources among them whose generated output was
    /// up to date, so a restore does not force a regeneration.
    pub fresh: Vec<String>,
}

/// Options for [`World::save`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SaveOptions {
    /// Leave out fields equal to their default (smaller files; but a later
    /// change of default silently changes the scene).
    pub omit_defaults: bool,
}

/// The `$schema` of the last loaded scene, written back by [`World::save`].
#[derive(Resource, Clone, Debug, Default)]
pub(crate) struct SceneMeta {
    pub schema: Option<String>,
}

impl World {
    /// Serialises `entity` alone (not its children). `None` if it has no stable id.
    pub fn entity_doc(&self, entity: Entity, opts: SaveOptions) -> Option<EntityDoc> {
        let world = &self.ecs;
        let id = world.get::<StableId>(entity)?.0.clone();
        let name = world
            .get::<Name>(entity)
            .map(|n| n.0.clone())
            .unwrap_or_default();
        let parent = world
            .get::<ChildOf>(entity)
            .and_then(|c| self.id_of(c.parent()).map(str::to_string));
        let name_of = |e: Entity| self.id_of(e).map(str::to_string);
        let mut components = IndexMap::new();
        for key in self.registry.components_of(world, entity) {
            let schema = self.registry.schema(key);
            let values = self.registry.get_all(world, entity, key)?;
            let mut fields = IndexMap::new();
            for (f, v) in schema.fields.iter().zip(&values) {
                if opts.omit_defaults && *v == f.default {
                    continue;
                }
                fields.insert(f.name.clone(), Doc::from_value(v, &f.ty, &name_of));
            }
            components.insert(schema.name.clone(), fields);
        }
        let overrides = world
            .get::<Overrides>(entity)
            .map(|o| o.0.clone())
            .unwrap_or_default();
        Some(EntityDoc {
            id,
            name,
            parent,
            components,
            overrides,
        })
    }

    /// Serialises the authored part of the subtree rooted at `root`
    /// (generated entities and their descendants are skipped).
    pub fn subtree_docs(&self, root: Entity, opts: SaveOptions) -> Vec<EntityDoc> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            if self.ecs.get::<Generated>(e).is_some() {
                continue;
            }
            let Some(doc) = self.entity_doc(e, opts) else {
                continue;
            };
            out.push(doc);
            stack.extend(children(&self.ecs, e).iter().rev());
        }
        out
    }

    /// Snapshots the whole subtree rooted at `root`, generated entities
    /// included, for an exact restore ([`World::restore`]).
    pub fn snapshot_subtree(&self, root: Entity) -> Snapshot {
        let mut snap = Snapshot::default();
        for e in hierarchy::subtree(&self.ecs, root) {
            let Some(doc) = self.entity_doc(e, SaveOptions::default()) else {
                continue;
            };
            if let Some(g) = self.ecs.get::<Generated>(e)
                && let Some(source) = self.id_of(g.source)
            {
                snap.generated.push(GeneratedDoc {
                    source: source.to_string(),
                    key: g.key.clone(),
                });
            }
            if let Some(last) = self.ecs.get::<GeneratedFingerprint>(e)
                && procedural::fingerprint(self, e) == Some(last.0)
            {
                snap.fresh.push(doc.id.clone());
            }
            snap.entities.push(doc);
        }
        snap
    }

    /// Writes every authored entity into a [`SceneDoc`], in the deterministic
    /// order described in the module docs.
    pub fn save(&mut self, opts: SaveOptions) -> SceneDoc {
        let roots: Vec<(String, Entity)> = self
            .ecs
            .query_filtered::<(Entity, &StableId), Without<ChildOf>>()
            .iter(&self.ecs)
            .map(|(e, id)| (id.0.clone(), e))
            .collect();
        // Authored roots of unloaded stream cells are written where they
        // would be if their cell were loaded (ADR-0042).
        let mut docs: Vec<(String, Vec<EntityDoc>)> = roots
            .into_iter()
            .map(|(id, root)| (id, self.subtree_docs(root, opts)))
            .collect();
        if let Some(stash) = self.stashed() {
            docs.extend(
                stash
                    .iter()
                    .map(|(_, r)| (r.id.clone(), self.stashed_docs(r, opts))),
            );
        }
        docs.sort_by(|a, b| a.0.cmp(&b.0));
        let entities: Vec<EntityDoc> = docs.into_iter().flat_map(|(_, d)| d).collect();
        // Every script component the world knows (declared by the loaded scene
        // or registered since), not only those authored entities use: generated
        // entities (a prefab's) may use one, and the saved file must still load
        // before any script runs. A world that ran game scripts would also
        // write the components they defined; no caller saves such a world.
        let declared: BTreeMap<String, ComponentKey> = self
            .registry
            .keys()
            .filter(|k| self.registry.schema(*k).origin == ComponentOrigin::Script)
            .map(|k| (self.registry.schema(k).name.clone(), k))
            .collect();
        let script_components = declared
            .values()
            .map(|k| ComponentSchemaDoc::from(self.registry.schema(*k)))
            .collect();
        let schema = self
            .ecs
            .get_resource::<SceneMeta>()
            .and_then(|m| m.schema.clone());
        SceneDoc {
            schema,
            format: FORMAT.to_string(),
            version: VERSION,
            script_components,
            entities,
        }
    }

    /// Adds the entities of `scene` to the world, after declaring its
    /// `script_components`. On any error no entity is added (script component
    /// types declared by the file stay registered). Returns the spawned
    /// entities in file order.
    pub fn load(&mut self, scene: &SceneDoc) -> Result<Vec<Entity>, SceneError> {
        if scene.format != FORMAT {
            return Err(SceneError::UnsupportedFormat(format!(
                "expected format {FORMAT:?}, found {:?}",
                scene.format
            )));
        }
        if scene.version != VERSION {
            return Err(SceneError::UnsupportedFormat(format!(
                "unsupported scene version {} (this build reads {VERSION})",
                scene.version
            )));
        }
        self.declare_script_components(&scene.script_components)?;
        let spawned = self.spawn_docs(&scene.entities, None)?;
        if scene.schema.is_some() {
            self.ecs.insert_resource(SceneMeta {
                schema: scene.schema.clone(),
            });
        }
        Ok(spawned)
    }

    /// Registers the script components a file declares, or checks that the
    /// already-registered component of each name has the same shape. Every
    /// entry is checked before any is registered.
    pub fn declare_script_components(
        &mut self,
        docs: &[ComponentSchemaDoc],
    ) -> Result<(), SceneError> {
        let mut new = Vec::new();
        for (i, doc) in docs.iter().enumerate() {
            let at = format!("script_components[{i}] (name {:?})", doc.name);
            let schema = doc.to_schema().map_err(|e| SceneError::ParseAt {
                at: at.clone(),
                error: Box::new(e.into()),
            })?;
            match self.registry.try_key(&schema.name) {
                Some(k) => {
                    let existing = self.registry.schema(k);
                    if existing.origin != ComponentOrigin::Script || !existing.same_shape(&schema) {
                        return Err(SceneError::ScriptComponentMismatch { name: schema.name });
                    }
                }
                None => new.push(schema),
            }
        }
        for schema in new {
            self.registry.register_script(&mut self.ecs, schema)?;
        }
        Ok(())
    }

    /// Spawns entity docs atomically: on error every entity spawned so far is
    /// despawned again. Parents must appear before children (or exist in the
    /// world). `root_index` places the first entity at that index under its
    /// parent. Every id must be a valid authored id ([`validate_id`]).
    pub fn spawn_docs(
        &mut self,
        docs: &[EntityDoc],
        root_index: Option<usize>,
    ) -> Result<Vec<Entity>, SceneError> {
        self.restore_parts(docs, root_index, &[], &[])
    }

    /// Recreates a [`World::snapshot_subtree`] exactly and atomically (like
    /// [`World::spawn_docs`]): the generated entities are marked as generated
    /// by their source again, and the up-to-date sources keep their
    /// generation state. The restored entities are new `Entity` handles.
    ///
    /// Checked before anything is attached: authored ids are valid, each
    /// generated entry names an entity of the snapshot whose id is
    /// `<source>/<key>`, whose source carries `Procedural`, and whose parent is
    /// that source or another entity generated by it.
    pub fn restore(
        &mut self,
        snapshot: &Snapshot,
        root_index: Option<usize>,
    ) -> Result<Vec<Entity>, SceneError> {
        self.restore_parts(
            &snapshot.entities,
            root_index,
            &snapshot.generated,
            &snapshot.fresh,
        )
    }

    fn restore_parts(
        &mut self,
        docs: &[EntityDoc],
        root_index: Option<usize>,
        generated: &[GeneratedDoc],
        fresh: &[String],
    ) -> Result<Vec<Entity>, SceneError> {
        let mut spawned: Vec<Entity> = Vec::with_capacity(docs.len());
        let result = self.spawn_docs_inner(docs, root_index, generated, fresh, &mut spawned);
        if result.is_err() {
            for e in spawned.into_iter().rev() {
                hierarchy::despawn_recursive(&mut self.ecs, e);
            }
        }
        result
    }

    fn spawn_docs_inner(
        &mut self,
        docs: &[EntityDoc],
        root_index: Option<usize>,
        generated: &[GeneratedDoc],
        fresh: &[String],
        spawned: &mut Vec<Entity>,
    ) -> Result<Vec<Entity>, SceneError> {
        let at = |i: usize, d: &EntityDoc| format!("entities[{i}] (id {:?})", d.id);
        // Pass 0: ids. Authored ids are validated; `/` is reserved for the
        // generated entries, which name their source explicitly.
        let mut gen_by_id: BTreeMap<String, &GeneratedDoc> = BTreeMap::new();
        for g in generated {
            if g.key.is_empty() || g.source.is_empty() {
                return Err(SceneError::Invalid(format!(
                    "generated entry {:?}: source and key must not be empty",
                    g.id()
                )));
            }
            if gen_by_id.insert(g.id(), g).is_some() {
                return Err(SceneError::DuplicateId(g.id()));
            }
        }
        let mut seen = HashSet::new();
        for (i, d) in docs.iter().enumerate() {
            if d.id.is_empty() {
                return Err(SceneError::Parse(format!(
                    "{}: id must not be empty",
                    at(i, d)
                )));
            }
            if !gen_by_id.contains_key(&d.id) {
                validate_id(&d.id).map_err(|e| SceneError::ParseAt {
                    at: at(i, d),
                    error: Box::new(e),
                })?;
            }
            if !seen.insert(d.id.as_str()) || self.ids().get(&d.id).is_some() {
                return Err(SceneError::DuplicateId(d.id.clone()));
            }
        }
        if let Some(id) = gen_by_id.keys().find(|id| !seen.contains(id.as_str())) {
            return Err(SceneError::Invalid(format!(
                "generated entry {id:?} names no entity of the snapshot"
            )));
        }
        // Pass 1: ids, names, parents. Children attach in file order, which
        // reproduces sibling order.
        for (i, d) in docs.iter().enumerate() {
            let parent = match &d.parent {
                Some(p) => Some(self.ids().get(p).ok_or_else(|| SceneError::ForwardParent {
                    at: at(i, d),
                    parent: p.clone(),
                })?),
                None => None,
            };
            let mut e = self
                .ecs
                .spawn((StableId(d.id.clone()), Name(d.name.clone())));
            let entity = e.id();
            spawned.push(entity);
            if !d.overrides.is_empty() {
                e.insert(Overrides(d.overrides.clone()));
            }
            if let Some(p) = parent {
                match root_index {
                    Some(index) if i == 0 => {
                        hierarchy::insert_child(&mut self.ecs, p, index, entity)
                    }
                    _ => hierarchy::add_child(&mut self.ecs, p, entity),
                }
            }
        }
        // Pass 2: components (entity references can now resolve).
        for (i, d) in docs.iter().enumerate() {
            let entity = spawned[i];
            for (cname, fields) in &d.components {
                let wrap = |e: SceneError| SceneError::ParseAt {
                    at: at(i, d),
                    error: Box::new(e),
                };
                let key = self
                    .registry
                    .key(cname)
                    .map_err(|e| wrap(SceneError::from(e)))?;
                let values = self.values_from_doc(key, fields).map_err(wrap)?;
                self.registry.insert(&mut self.ecs, entity, key, values)?;
            }
        }
        // Pass 3: generated entities, in order (a generated parent is marked
        // before its children), now that sources carry their components.
        for (i, d) in docs.iter().enumerate() {
            let Some(g) = gen_by_id.get(&d.id) else {
                continue;
            };
            let entity = spawned[i];
            let invalid = |why: String| SceneError::Invalid(format!("{}: {why}", at(i, d)));
            let source = self.ids().get(&g.source).ok_or_else(|| {
                invalid(format!(
                    "source {:?} of a generated entity does not exist",
                    g.source
                ))
            })?;
            if self.ecs.get::<Procedural>(source).is_none() {
                return Err(invalid(format!(
                    "source {:?} of a generated entity is not a procedural source",
                    g.source
                )));
            }
            let parent = hierarchy::parent(&self.ecs, entity);
            let parent_ok = parent == Some(source)
                || parent.is_some_and(|p| {
                    self.ecs
                        .get::<Generated>(p)
                        .is_some_and(|pg| pg.source == source)
                });
            if !parent_ok {
                return Err(invalid(format!(
                    "a generated entity's parent must be its source {:?} or another entity it generated",
                    g.source
                )));
            }
            self.ecs.entity_mut(entity).insert(Generated {
                source,
                key: g.key.clone(),
            });
        }
        // Up-to-date sources keep their generation state.
        for (i, d) in docs.iter().enumerate() {
            if fresh.contains(&d.id)
                && let Some(fp) = procedural::fingerprint(self, spawned[i])
            {
                self.ecs
                    .entity_mut(spawned[i])
                    .insert(GeneratedFingerprint(fp));
            }
        }
        Ok(spawned.clone())
    }

    /// Builds a full field list for `key` from a partial doc map; missing
    /// fields take their defaults; unknown fields are errors. Entity
    /// references resolve through the id index.
    pub fn values_from_doc(
        &self,
        key: ComponentKey,
        fields: &IndexMap<String, Doc>,
    ) -> Result<Vec<Value>, SceneError> {
        let schema = self.registry.schema(key);
        let mut values = schema.defaults();
        let ids = self.ids();
        let resolve = |id: &str| ids.get(id);
        for (fname, doc) in fields {
            let index = schema.field_index(fname)?;
            let field = &schema.fields[index];
            values[index] = doc.to_value(&field.ty, &resolve).map_err(|error| {
                re_reflect::ReflectError::Value {
                    path: format!("{}.{}", schema.name, field.name),
                    error,
                }
            })?;
        }
        Ok(values)
    }
}

/// Writes a scene as pretty JSON with scalar lists on one line, ending with a newline.
pub fn to_json(scene: &SceneDoc) -> String {
    to_pretty_json(scene)
}

/// Pretty JSON of any serialisable value, with scalar lists collapsed onto
/// one line and a final newline (the scene file style).
pub fn to_pretty_json<T: Serialize + ?Sized>(value: &T) -> String {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    let mut s = collapse_scalar_lists(&text);
    s.push('\n');
    s
}

/// Reads a scene file. Older versions are migrated first ([`migrate`]);
/// unknown keys are errors.
pub fn from_json(text: &str) -> Result<SceneDoc, SceneError> {
    match serde_json::from_str::<SceneDoc>(text) {
        Ok(doc) if doc.version >= VERSION => Ok(doc),
        typed => {
            #[derive(Deserialize)]
            struct VersionOnly {
                version: u32,
            }
            match serde_json::from_str::<VersionOnly>(text) {
                Ok(v) if v.version < VERSION => {
                    let mut raw: serde_json::Value =
                        serde_json::from_str(text).map_err(|e| SceneError::Parse(e.to_string()))?;
                    migrate(&mut raw)?;
                    serde_json::from_value(raw).map_err(|e| SceneError::Parse(e.to_string()))
                }
                _ => typed.map_err(|e| SceneError::Parse(e.to_string())),
            }
        }
    }
}

/// Migrates a scene document (as raw JSON) forwards to [`VERSION`], one
/// function per version step. Version 1 is the first version, so there is
/// nothing to migrate yet; a version newer than this build is an error.
pub fn migrate(doc: &mut serde_json::Value) -> Result<(), SceneError> {
    let version = doc
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| SceneError::Parse("missing field `version`".into()))?;
    if version > VERSION as u64 || version == 0 {
        return Err(SceneError::UnsupportedFormat(format!(
            "unsupported scene version {version} (this build reads {VERSION})"
        )));
    }
    // Steps go here: `if version < 2 { v1_to_v2(doc); }` and so on.
    Ok(())
}

/// Puts every list that holds only scalars (vectors, colours) on one line:
/// `[0.0, 1.0, 0.0]` instead of five lines. String-aware; works on pretty JSON.
pub fn collapse_scalar_lists(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut last = 0;
    let mut in_str = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            match b {
                b'\\' => i += 1,
                b'"' => in_str = false,
                _ => {}
            }
        } else if b == b'"' {
            in_str = true;
        } else if b == b'['
            && let Some(end) = scalar_list_end(bytes, i)
        {
            out.push_str(&text[last..i]);
            out.push('[');
            let items = split_items(&text[i + 1..end]);
            out.push_str(&items.join(", "));
            out.push(']');
            i = end + 1;
            last = i;
            continue;
        }
        i += 1;
    }
    out.push_str(&text[last..]);
    out
}

/// Index of the `]` closing the list at `start`, if the list contains no
/// nested list, map or tuple.
fn scalar_list_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut in_str = false;
    let mut i = start + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            match b {
                b'\\' => i += 1,
                b'"' => in_str = false,
                _ => {}
            }
        } else {
            match b {
                b'"' => in_str = true,
                b']' => return Some(i),
                b'[' | b'{' | b'(' => return None,
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn split_items(inner: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let bytes = inner.as_bytes();
    let (mut in_str, mut escaped, mut start) = (false, false, 0);
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
        } else if b == b'"' {
            in_str = true;
        } else if b == b',' {
            items.push(inner[start..i].trim());
            start = i + 1;
        }
    }
    items.push(inner[start..].trim());
    items.retain(|s| !s.is_empty());
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_lists_collapse_but_strings_and_nesting_do_not() {
        let text = "{\n  \"a\": [\n    1.0,\n    2.0\n  ],\n  \"b\": \"[x,\\\" y]\",\n  \"c\": [\n    [\n      1\n    ]\n  ]\n}";
        let out = collapse_scalar_lists(text);
        assert!(out.contains("\"a\": [1.0, 2.0]"), "{out}");
        assert!(out.contains("\"b\": \"[x,\\\" y]\""), "{out}");
        assert!(out.contains("\"c\": [\n    [1]\n  ]"), "{out}");
    }

    #[test]
    fn newer_versions_are_refused() {
        let mut v = serde_json::json!({ "version": 99 });
        assert!(matches!(
            migrate(&mut v),
            Err(SceneError::UnsupportedFormat(_))
        ));
    }
}
