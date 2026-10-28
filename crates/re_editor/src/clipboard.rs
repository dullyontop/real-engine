//! Copy, cut, paste and duplicate (ADR-0044's amendment for WP42, §6).
//!
//! Copy writes JSON text to the system clipboard, so an agent can read what
//! was copied and paste text of its own:
//!
//! ```json
//! { "format": "real-engine/entities", "version": 1,
//!   "subtrees": [ { "world": [16 numbers, column-major], "entities": [...] } ] }
//! ```
//!
//! Each subtree is a copied root, then its descendants, each after its
//! parent, as the scene file writes them (`EntityDoc`), with the root's world
//! transform when it was copied. Paste also takes a scene file's text, its
//! root entities as the subtrees.
//!
//! Paste is one `scene.apply` with one `restore` op per subtree, so one undo
//! step. Every entity gets a fresh id; references between the pasted
//! entities follow them; each root keeps its world transform under its new
//! parent. Duplicate pastes copies beside their originals without touching
//! the clipboard. The clipboard is editor state: copying is never a command.

use std::collections::{HashMap, HashSet};

use glam::{Mat4, Quat, Vec3};
use re_core::Diagnostic;
use re_reflect::{Doc, FieldType};
use re_scene::hierarchy::{children, parent, sibling_index};
use re_scene::{Entity, EntityDoc, Generated, GlobalTransform, SaveOptions, SceneDoc, World};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::codes;
use crate::selection::Selection;

/// The clipboard format's name.
pub const FORMAT: &str = "real-engine/entities";

/// The clipboard format's version.
pub const VERSION: u32 = 1;

/// Entities on the clipboard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Clip {
    /// Always [`FORMAT`].
    pub format: String,
    /// Always [`VERSION`].
    pub version: u32,
    /// The copied subtrees, in the order they were selected.
    pub subtrees: Vec<Subtree>,
}

/// One copied root and its descendants.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Subtree {
    /// The root's world transform when it was copied, column-major.
    pub world: [f32; 16],
    /// The root, then its descendants, each after its parent.
    pub entities: Vec<EntityDoc>,
}

impl Clip {
    fn new(subtrees: Vec<Subtree>) -> Clip {
        Clip {
            format: FORMAT.to_string(),
            version: VERSION,
            subtrees,
        }
    }

    /// The clipboard text.
    pub fn to_text(&self) -> String {
        // A clip of documents always serialises.
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Reads clipboard text: entities in this format, or a scene file.
    /// Anything else is a note ([`codes::NOT_ENTITIES`]).
    pub fn parse(text: &str) -> Result<Clip, Diagnostic> {
        let refuse = |why: String| {
            Diagnostic::note(codes::NOT_ENTITIES, format!("nothing was pasted: {why}")).with_hint(
                "copy entities in the editor first; the clipboard takes the editor's own \
                 format or a scene file's text",
            )
        };
        let json: Json = serde_json::from_str(text)
            .map_err(|_| refuse("the clipboard does not hold entities".into()))?;
        let clip = match json.get("format").and_then(Json::as_str) {
            Some(FORMAT) => {
                let clip: Clip = serde_json::from_value(json)
                    .map_err(|e| refuse(format!("the copied entities could not be read: {e}")))?;
                if clip.version != VERSION {
                    return Err(refuse(format!(
                        "the copied entities are version {}, and this editor reads version \
                         {VERSION}",
                        clip.version
                    )));
                }
                clip
            }
            Some(re_scene::scene::FORMAT) => {
                let scene: SceneDoc = serde_json::from_value(json)
                    .map_err(|e| refuse(format!("the scene text could not be read: {e}")))?;
                from_scene(scene.entities)
            }
            _ => return Err(refuse("the clipboard does not hold entities".into())),
        };
        for (i, subtree) in clip.subtrees.iter().enumerate() {
            check_subtree(subtree)
                .map_err(|e| refuse(format!("the copied subtree {i} is not whole: {e}")))?;
        }
        if clip.subtrees.is_empty() {
            return Err(refuse("the clipboard holds no entities".into()));
        }
        Ok(clip)
    }
}

/// Each entity after the root has an earlier one as its parent, and no id
/// repeats.
fn check_subtree(subtree: &Subtree) -> Result<(), String> {
    let mut earlier: HashSet<&str> = HashSet::new();
    if subtree.entities.is_empty() {
        return Err("it has no entities".into());
    }
    for (i, doc) in subtree.entities.iter().enumerate() {
        if i > 0 && !doc.parent.as_deref().is_some_and(|p| earlier.contains(p)) {
            return Err(format!("{:?} does not come after its parent", doc.id));
        }
        if !earlier.insert(&doc.id) {
            return Err(format!("{:?} appears twice", doc.id));
        }
    }
    Ok(())
}

/// A scene file's root entities as subtrees, each root's world transform
/// read from its own `Transform`.
fn from_scene(entities: Vec<EntityDoc>) -> Clip {
    let ids: HashSet<&str> = entities.iter().map(|d| d.id.as_str()).collect();
    let roots: Vec<usize> = (0..entities.len())
        .filter(|&i| {
            !entities[i]
                .parent
                .as_deref()
                .is_some_and(|p| ids.contains(p))
        })
        .collect();
    let subtrees = roots
        .into_iter()
        .map(|r| {
            let mut members: HashSet<&str> = HashSet::from([entities[r].id.as_str()]);
            let mut docs = vec![entities[r].clone()];
            for doc in &entities[r + 1..] {
                if doc.parent.as_deref().is_some_and(|p| members.contains(p)) {
                    members.insert(&doc.id);
                    docs.push(doc.clone());
                }
            }
            let (s, q, t) =
                local_transform(&docs[0]).unwrap_or((Vec3::ONE, Quat::IDENTITY, Vec3::ZERO));
            Subtree {
                world: Mat4::from_scale_rotation_translation(s, q, t).to_cols_array(),
                entities: docs,
            }
        })
        .collect();
    Clip::new(subtrees)
}

/// What a copy took, and notes on what it left out.
pub(crate) struct Copied {
    /// The copied subtrees; `None` when nothing could be copied.
    pub clip: Option<Clip>,
    /// Generated entities left out ([`codes::COPY_LEFT_OUT`]).
    pub notes: Vec<Diagnostic>,
}

/// Copies the selection's topmost entities with their descendants.
///
/// A procedural source's generated output is left out: the copy generates
/// its own. A generated entity copied for itself becomes an authored one,
/// unless it shows a derived asset (`derived:…`, a mesh or heightfield that
/// exists only while its source makes it); then it is left out, with a note.
pub(crate) fn copy(world: &World, selection: &Selection) -> Copied {
    let mut notes = Vec::new();
    let mut subtrees = Vec::new();
    for id in selection.topmost(world) {
        let Ok(root) = world.entity(&id) else {
            continue;
        };
        let entities = subtree_docs(world, root, &mut notes);
        if entities.is_empty() {
            continue;
        }
        let matrix = world
            .ecs
            .get::<GlobalTransform>(root)
            .map_or(Mat4::IDENTITY, |g| Mat4::from(g.0));
        subtrees.push(Subtree {
            world: matrix.to_cols_array(),
            entities,
        });
    }
    Copied {
        clip: (!subtrees.is_empty()).then(|| Clip::new(subtrees)),
        notes,
    }
}

fn subtree_docs(world: &World, root: Entity, notes: &mut Vec<Diagnostic>) -> Vec<EntityDoc> {
    let generated = |e: Entity| world.ecs.get::<Generated>(e).is_some();
    let for_itself = generated(root);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if generated(e) && !for_itself {
            continue;
        }
        let Some(mut doc) = world.entity_doc(e, SaveOptions::default()) else {
            continue;
        };
        if generated(e) {
            if let Some(asset) = derived_asset(&doc) {
                notes.push(
                    Diagnostic::note(
                        codes::COPY_LEFT_OUT,
                        format!(
                            "{} was not copied: it shows `{asset}`, which exists only while its \
                             procedural source makes it",
                            doc.id
                        ),
                    )
                    .with_hint("bake the source (`proc.bake`) to copy what it made"),
                );
                continue;
            }
            // An authored copy: only a source keeps overrides.
            doc.overrides.clear();
        }
        out.push(doc);
        stack.extend(children(&world.ecs, e).iter().rev());
    }
    out
}

/// The first `derived:` asset a field of `doc` names.
fn derived_asset(doc: &EntityDoc) -> Option<String> {
    fn find(doc: &Doc) -> Option<&str> {
        match doc {
            Doc::Str(s) if s.starts_with("derived:") => Some(s),
            Doc::List(items) => items.iter().find_map(find),
            Doc::Map(map) => map.values().find_map(find),
            _ => None,
        }
    }
    doc.components
        .values()
        .flat_map(|fields| fields.values())
        .find_map(find)
        .map(str::to_string)
}

/// Where pasted roots go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Under the active entity's parent, after it; at the scene's root when
    /// nothing is selected (Ctrl+V).
    Beside,
    /// Under the active entity ("Paste as child").
    Under,
    /// Each copy after its original (Ctrl+D).
    AfterOriginals,
    /// At the scene's root (the Hierarchy's menu under its rows).
    Root,
}

/// A paste ready to send.
pub(crate) struct Pasted {
    /// `scene.apply`'s arguments: one `restore` op per subtree.
    pub args: Json,
    /// The new roots' ids, in order: the selection after the paste.
    pub roots: Vec<String>,
    /// References cleared because their entity does not exist
    /// ([`codes::REFERENCE_CLEARED`]).
    pub notes: Vec<Diagnostic>,
}

/// Builds the paste of `clip` into `world`, placed by `place` relative to
/// `selection`. Fresh engine ids come from [`World::new_id`], which is why
/// the world is borrowed mutably; nothing else in it changes.
///
/// # Errors
///
/// [`codes::PASTE_UNKNOWN_COMPONENT`] when an entity has a component the
/// world does not know: nothing is pasted.
pub(crate) fn paste(
    world: &mut World,
    clip: &Clip,
    selection: &Selection,
    place: Place,
) -> Result<Pasted, Diagnostic> {
    for doc in clip.subtrees.iter().flat_map(|s| &s.entities) {
        if let Some(name) = doc
            .components
            .keys()
            .find(|c| world.registry.try_key(c).is_none())
        {
            return Err(Diagnostic::error(
                codes::PASTE_UNKNOWN_COMPONENT,
                format!(
                    "nothing was pasted: {} has the component `{name}`, which this project \
                     does not define",
                    doc.id
                ),
            )
            .with_hint(
                "define the component in this project (a script component), or remove it \
                 from the copied text",
            ));
        }
    }
    // Fresh ids, in order, before anything is remapped.
    let mut chosen: HashSet<String> = HashSet::new();
    let mut fresh: Vec<Vec<String>> = Vec::new();
    let mut new_of: HashMap<String, String> = HashMap::new();
    for subtree in &clip.subtrees {
        let ids: Vec<String> = subtree
            .entities
            .iter()
            .map(|doc| {
                let id = fresh_id(world, &doc.id, &chosen);
                chosen.insert(id.clone());
                new_of.entry(doc.id.clone()).or_insert_with(|| id.clone());
                id
            })
            .collect();
        fresh.push(ids);
    }
    let targets = targets(world, clip, selection, place);
    // Which subtree's op makes each new entity: a reference to an entity
    // that a later op makes is set after every op has run.
    let made_by: HashMap<&str, usize> = fresh
        .iter()
        .enumerate()
        .flat_map(|(i, ids)| ids.iter().map(move |id| (id.as_str(), i)))
        .collect();
    let mut notes = Vec::new();
    let mut ops = Vec::new();
    let mut later = Vec::new();
    let mut roots = Vec::new();
    for (i, ((subtree, ids), (parent_id, index))) in
        clip.subtrees.iter().zip(&fresh).zip(targets).enumerate()
    {
        // Within a subtree its own copies come first (an id may repeat
        // across subtrees of hand-written text).
        let own: HashMap<&str, &str> = subtree
            .entities
            .iter()
            .map(|d| d.id.as_str())
            .zip(ids.iter().map(String::as_str))
            .collect();
        let new_id = |old: &str| {
            own.get(old)
                .map(|id| id.to_string())
                .or_else(|| new_of.get(old).cloned())
        };
        let elsewhere = |id: &str| made_by.get(id).is_some_and(|&j| j != i);
        let mut docs = subtree.entities.clone();
        let original_parent = docs[0].parent.clone();
        for (k, doc) in docs.iter_mut().enumerate() {
            let old = std::mem::replace(&mut doc.id, ids[k].clone());
            doc.parent = if k == 0 {
                parent_id.clone()
            } else {
                // `Clip::parse` checked that the parent came earlier.
                doc.parent
                    .as_deref()
                    .and_then(|p| own.get(p))
                    .map(|p| p.to_string())
            };
            let refs = Refs {
                new_id: &new_id,
                elsewhere: &elsewhere,
            };
            remap_references(world, doc, &old, &refs, &mut later, &mut notes);
        }
        if parent_id != original_parent {
            keep_world_transform(world, &mut docs[0], &subtree.world, parent_id.as_deref());
        }
        roots.push(docs[0].id.clone());
        let mut op = json!({ "op": "restore", "entities": docs });
        if let Some(index) = index {
            op["index"] = json!(index);
        }
        ops.push(op);
    }
    ops.extend(later);
    let verb = if place == Place::AfterOriginals {
        "duplicate"
    } else {
        "paste"
    };
    let label = match clip.subtrees.as_slice() {
        [one] => format!("{verb} {}", one.entities[0].id),
        many => format!("{verb} {} entities", many.len()),
    };
    Ok(Pasted {
        args: json!({ "label": label, "ops": ops }),
        roots,
        notes,
    })
}

/// Each subtree's new parent and position among the parent's children.
fn targets(
    world: &World,
    clip: &Clip,
    selection: &Selection,
    place: Place,
) -> Vec<(Option<String>, Option<usize>)> {
    let id_of = |e: Entity| world.id_of(e).map(str::to_string);
    let active = selection.active().and_then(|a| world.entity(a).ok());
    let n = clip.subtrees.len();
    match (place, active) {
        (Place::Beside, Some(a)) => {
            let parent = parent(&world.ecs, a).and_then(id_of);
            let at = sibling_index(&world.ecs, a);
            (0..n)
                .map(|i| (parent.clone(), at.map(|at| at + 1 + i)))
                .collect()
        }
        (Place::Under, Some(a)) => vec![(id_of(a), None); n],
        (Place::Beside | Place::Under, None) | (Place::Root, _) => vec![(None, None); n],
        (Place::AfterOriginals, _) => {
            // After each original. A copy put before a later original
            // pushes that original (and its copy) one further along.
            let originals: Vec<Option<(Option<Entity>, usize)>> = clip
                .subtrees
                .iter()
                .map(|s| {
                    let e = world.entity(&s.entities[0].id).ok()?;
                    Some((parent(&world.ecs, e), sibling_index(&world.ecs, e)?))
                })
                .collect();
            clip.subtrees
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let root = &s.entities[0];
                    let parent = root.parent.clone().filter(|p| world.entity(p).is_ok());
                    let index = originals[i].map(|(p, at)| {
                        let before = originals[..i]
                            .iter()
                            .flatten()
                            .filter(|(q, other)| *q == p && *other < at)
                            .count();
                        at + 1 + before
                    });
                    (parent, index)
                })
                .collect()
        }
    }
}

/// A fresh id for a copy of the entity `original`: its trailing number
/// moved to the next free one (`crate_1` → `crate_2`, `rock` → `rock_2`,
/// `tile_09` → `tile_10`), or a new engine id when `original` is an engine
/// id or has nothing but a number. A generated entity's id
/// (`<source>/<key>`) is numbered from its key's last part.
fn fresh_id(world: &mut World, original: &str, chosen: &HashSet<String>) -> String {
    let free = |world: &World, id: &str| {
        re_scene::validate_id(id).is_ok() && !world.id_taken(id) && !chosen.contains(id)
    };
    let base = original.rsplit('/').next().unwrap_or(original);
    let stem = base.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = &base[stem.len()..];
    let numbered = match digits.parse::<u64>() {
        _ if stem.is_empty() || is_engine_id(base) => None,
        Ok(n) => Some((stem.to_string(), n, digits.len())),
        Err(_) if digits.is_empty() => Some((format!("{base}_"), 1, 0)),
        Err(_) => None,
    };
    if let Some((stem, mut n, width)) = numbered {
        // Zero-padded numbers stay as wide (`tile_09` → `tile_10`).
        let width = if digits.starts_with('0') { width } else { 0 };
        for _ in 0..100_000 {
            let Some(next) = n.checked_add(1) else { break };
            n = next;
            let id = format!("{stem}{n:0width$}");
            if free(world, &id) {
                return id;
            }
        }
    }
    loop {
        let id = world.new_id();
        if !chosen.contains(&id) {
            return id;
        }
    }
}

/// Whether `id` looks like one [`World::new_id`] makes: `e` and ten
/// characters of its alphabet.
fn is_engine_id(id: &str) -> bool {
    const ALPHABET: &str = "0123456789abcdefghjkmnpqrstvwxyz";
    id.len() == 11 && id.starts_with('e') && id[1..].chars().all(|c| ALPHABET.contains(c))
}

/// How a paste's references are remapped.
struct Refs<'a> {
    /// The new id of a pasted entity, by its id in the clip.
    new_id: &'a dyn Fn(&str) -> Option<String>,
    /// Whether a new id is made by another subtree's op than the one being
    /// built.
    elsewhere: &'a dyn Fn(&str) -> bool,
}

/// Points `doc`'s entity references at the pasted copies; a reference to an
/// entity outside the paste is kept when that entity exists, and cleared
/// with a note otherwise. A field that refers to a copy another subtree's
/// `restore` makes is left empty in this one, and set by a `set_field` op
/// pushed to `later`, which runs after every `restore`.
fn remap_references(
    world: &World,
    doc: &mut EntityDoc,
    original: &str,
    refs: &Refs,
    later: &mut Vec<Json>,
    notes: &mut Vec<Diagnostic>,
) {
    let id = doc.id.clone();
    for (component, fields) in &mut doc.components {
        let Some(key) = world.registry.try_key(component) else {
            continue;
        };
        let schema = world.registry.schema(key);
        for (field, value) in fields.iter_mut() {
            let Some(f) = schema.fields.iter().find(|f| f.name == *field) else {
                continue;
            };
            let mut deferred = false;
            remap(value, &f.ty, &mut |target: &mut Doc| {
                let Doc::Str(target_id) = target else {
                    return;
                };
                if let Some(new) = (refs.new_id)(target_id) {
                    deferred |= (refs.elsewhere)(&new);
                    *target_id = new;
                } else if world.entity(target_id).is_err() {
                    notes.push(Diagnostic::note(
                        codes::REFERENCE_CLEARED,
                        format!(
                            "the copy of {original} refers to {target_id} in {component}.{field}, \
                             which does not exist here: the reference was cleared"
                        ),
                    ));
                    *target = Doc::Null;
                }
            });
            if deferred {
                later.push(json!({
                    "op": "set_field", "id": id, "component": component,
                    "field": field, "value": value.clone(),
                }));
                remap(value, &f.ty, &mut |target: &mut Doc| {
                    if matches!(target, Doc::Str(t) if (refs.elsewhere)(t)) {
                        *target = Doc::Null;
                    }
                });
            }
        }
    }
}

/// Calls `f` on each entity reference in `value`, a field of type `ty`.
fn remap(value: &mut Doc, ty: &FieldType, f: &mut dyn FnMut(&mut Doc)) {
    match (ty, value) {
        (FieldType::Entity, value) => f(value),
        (FieldType::List(item), Doc::List(items)) => {
            for v in items {
                remap(v, item, f);
            }
        }
        (FieldType::Record(record), Doc::Map(map)) => {
            for field in &record.schema().fields {
                if let Some(v) = map.get_mut(&field.name) {
                    remap(v, &field.ty, f);
                }
            }
        }
        _ => {}
    }
}

/// Sets the root's local `Transform` so that, under `parent`, it stands
/// where `world` (column-major) says. A root without a `Transform` is left
/// as it is.
fn keep_world_transform(w: &World, root: &mut EntityDoc, world: &[f32; 16], parent: Option<&str>) {
    let Some(transform) = root.components.get_mut("Transform") else {
        return;
    };
    let parent_world = parent
        .and_then(|p| w.entity(p).ok())
        .and_then(|p| w.ecs.get::<GlobalTransform>(p))
        .map_or(Mat4::IDENTITY, |g| Mat4::from(g.0));
    let local = parent_world.inverse() * Mat4::from_cols_array(world);
    if !local.is_finite() {
        return;
    }
    let (s, q, t) = local.to_scale_rotation_translation();
    let list = |v: &[f32]| Doc::List(v.iter().map(|x| Doc::F32(*x)).collect());
    transform.insert("translation".into(), list(&t.to_array()));
    transform.insert("rotation".into(), list(&q.normalize().to_array()));
    transform.insert("scale".into(), list(&s.to_array()));
}

/// A root's local `Transform` as written in its document (defaults for
/// missing fields), or `None` when it has none or a field is not a list of
/// numbers.
fn local_transform(doc: &EntityDoc) -> Option<(Vec3, Quat, Vec3)> {
    let transform = doc.components.get("Transform")?;
    let numbers = |name: &str, default: &[f32]| -> Option<Vec<f32>> {
        match transform.get(name) {
            None => Some(default.to_vec()),
            Some(Doc::List(items)) => items
                .iter()
                .map(|d| match d {
                    Doc::F32(x) => Some(*x),
                    Doc::F64(x) => Some(*x as f32),
                    Doc::Int(i) => Some(*i as f32),
                    _ => None,
                })
                .collect(),
            Some(_) => None,
        }
    };
    let t = numbers("translation", &[0.0; 3])?;
    let q = numbers("rotation", &[0.0, 0.0, 0.0, 1.0])?;
    let s = numbers("scale", &[1.0; 3])?;
    match (t.as_slice(), q.as_slice(), s.as_slice()) {
        ([tx, ty, tz], [qx, qy, qz, qw], [sx, sy, sz]) => Some((
            Vec3::new(*sx, *sy, *sz),
            Quat::from_xyzw(*qx, *qy, *qz, *qw).normalize(),
            Vec3::new(*tx, *ty, *tz),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, parent: Option<&str>, components: Json) -> EntityDoc {
        serde_json::from_value(json!({ "id": id, "parent": parent, "components": components }))
            .unwrap()
    }

    fn world(docs: &[EntityDoc]) -> World {
        let mut w = World::new();
        w.spawn_docs(docs, None).unwrap();
        w.propagate();
        w
    }

    #[test]
    fn fresh_ids_move_the_trailing_number_on() {
        let mut w = world(&[
            doc("crate_1", None, json!({})),
            doc("crate_2", None, json!({})),
            doc("rock", None, json!({})),
            doc("tile_09", None, json!({})),
        ]);
        let none = HashSet::new();
        assert_eq!(fresh_id(&mut w, "crate_1", &none), "crate_3");
        let chosen = HashSet::from(["crate_3".to_string()]);
        assert_eq!(fresh_id(&mut w, "crate_2", &chosen), "crate_4");
        assert_eq!(fresh_id(&mut w, "rock", &none), "rock_2");
        assert_eq!(fresh_id(&mut w, "tile_09", &none), "tile_10");
        assert_eq!(fresh_id(&mut w, "lamp", &none), "lamp_2");
        // A generated entity's copy is numbered from its key's last part.
        assert_eq!(fresh_id(&mut w, "town/house_7", &none), "house_8");
        // An engine id, or nothing but a number, gets a new engine id.
        for original in ["e0123456789", "town/c/3_4/17"] {
            let id = fresh_id(&mut w, original, &none);
            assert!(is_engine_id(&id) && id != original, "{original} -> {id}");
        }
        assert!(!is_engine_id("eagle_nest1") && !is_engine_id("elephant"));
    }

    #[test]
    fn a_clip_reads_its_own_text_and_a_scene_file() {
        let clip = Clip::new(vec![Subtree {
            world: Mat4::from_translation(Vec3::X).to_cols_array(),
            entities: vec![doc("a", None, json!({})), doc("b", Some("a"), json!({}))],
        }]);
        assert_eq!(Clip::parse(&clip.to_text()).unwrap(), clip);

        // A scene file's roots are the subtrees; an entity whose parent is
        // not in the file is a root too.
        let scene = json!({
            "format": "real-engine/scene", "version": 1,
            "entities": [
                { "id": "a", "components": { "Transform": { "translation": [1.0, 2.0, 3.0] } } },
                { "id": "b", "parent": "a" },
                { "id": "c", "parent": "elsewhere" },
                { "id": "d", "parent": "b" }
            ]
        });
        let clip = Clip::parse(&scene.to_string()).unwrap();
        let ids: Vec<Vec<&str>> = clip
            .subtrees
            .iter()
            .map(|s| s.entities.iter().map(|d| d.id.as_str()).collect())
            .collect();
        assert_eq!(ids, [vec!["a", "b", "d"], vec!["c"]]);
        let at = Mat4::from_cols_array(&clip.subtrees[0].world).w_axis;
        assert_eq!(at.truncate(), Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn text_that_is_not_whole_entities_is_refused() {
        let subtrees = |entities: Json| {
            let world = Mat4::IDENTITY.to_cols_array();
            json!({ "format": FORMAT, "version": 1,
                    "subtrees": [{ "world": world, "entities": entities }] })
            .to_string()
        };
        let cases = [
            "a line of a script".to_string(),
            json!({ "format": FORMAT, "version": 2, "subtrees": [] }).to_string(),
            json!({ "format": FORMAT, "version": 1, "subtrees": [] }).to_string(),
            subtrees(json!([{ "id": "b", "parent": "a" }, { "id": "a" }])),
            subtrees(json!([{ "id": "a" }, { "id": "a", "parent": "a" }])),
            subtrees(json!([])),
        ];
        for text in cases {
            let d = Clip::parse(&text).unwrap_err();
            assert_eq!(d.code, codes::NOT_ENTITIES, "{text}: {d:?}");
        }
    }

    #[test]
    fn a_copy_leaves_out_generated_output_and_derived_assets() {
        let mut w = world(&[
            doc("source", None, json!({ "Transform": {} })),
            doc("made", Some("source"), json!({ "Transform": {} })),
            doc(
                "rock",
                Some("source"),
                json!({ "MeshRef": { "asset": "derived:00ff" } }),
            ),
        ]);
        let source = w.entity("source").unwrap();
        for (id, key) in [("made", "made"), ("rock", "rock")] {
            let e = w.entity(id).unwrap();
            w.ecs.entity_mut(e).insert(Generated {
                source,
                key: key.into(),
            });
        }
        let copied = |w: &World, ids: &[&str]| {
            let s = Selection::from_ids(ids.iter().map(|s| s.to_string()), None);
            copy(w, &s)
        };
        // The source alone: its output is not copied (the copy makes its own).
        let c = copied(&w, &["source"]);
        let clip = c.clip.unwrap();
        assert_eq!(clip.subtrees[0].entities.len(), 1);
        assert!(c.notes.is_empty());
        // A generated entity copied for itself becomes an authored one...
        let c = copied(&w, &["made"]);
        assert_eq!(c.clip.unwrap().subtrees[0].entities[0].id, "made");
        // ...unless it shows a derived asset.
        let c = copied(&w, &["rock"]);
        assert!(c.clip.is_none());
        assert_eq!(c.notes[0].code, codes::COPY_LEFT_OUT);
        assert!(c.notes[0].message.contains("derived:00ff"), "{:?}", c.notes);

        // Cut deletes what it copied, and only that.
        let mut ed = crate::Editor::new();
        ed.set_selection(Selection::from_ids(["made".into(), "rock".into()], None));
        let (_, delete) = ed.cut(&w).unwrap();
        let crate::editor::Action::Command { args, .. } = delete else {
            panic!("{delete:?}")
        };
        assert_eq!(args["ops"], json!([{ "op": "despawn", "id": "made" }]));
        ed.set_selection(Selection::one("rock"));
        assert!(ed.cut(&w).is_none(), "nothing copied, nothing deleted");
    }
}
