//! Structural overrides (ADR-0043 §3): moving a generated entity under
//! another entity of its source's output, replacing what a generated entity
//! is, and inserting authored entities into the output. They are kept in the
//! source's [`Overrides`](crate::Overrides) list with the field and deletion
//! overrides, and [`apply_structure`] applies them to each new fragment
//! before deletions and field overrides.

use std::collections::{HashMap, HashSet};
use std::fmt;

use re_core::files::ProjectFiles;
use re_reflect::{Registry, Value};
use serde::{Deserialize, Serialize};

use crate::error::SceneError;
use crate::prefab::PrefabFile;
use crate::procedural::{GenNode, OverrideDoc, SceneFragment};
use crate::scene::ComponentsDoc;

/// The first character of an inserted entity's key (`+lamp1`).
pub const INSERTED_PREFIX: char = '+';
/// The first character of a key segment that a replacement's prefab adds
/// (`trees/12/~trunk`).
pub const REPLACED_PREFIX: char = '~';

/// True if a generator may emit `key`: no segment starts with
/// [`INSERTED_PREFIX`] or [`REPLACED_PREFIX`], which structural overrides use.
pub fn is_generator_key(key: &str) -> bool {
    !key.split('/')
        .any(|s| s.starts_with(INSERTED_PREFIX) || s.starts_with(REPLACED_PREFIX))
}

/// The kind of a structural override. A target has at most one of each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StructuralKind {
    /// [`Move`].
    Moved,
    /// [`Replace`].
    Replaced,
    /// [`Insert`].
    Inserted,
}

impl fmt::Display for StructuralKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            StructuralKind::Moved => "move",
            StructuralKind::Replaced => "replacement",
            StructuralKind::Inserted => "insert",
        })
    }
}

/// What an entity becomes: a prefab's root (its other entities become its
/// descendants) or components written as in a scene file.
#[derive(Clone, Debug, PartialEq)]
pub enum Replacement {
    /// A prefab file with one root entity.
    Prefab(String),
    /// Components by name, each with the fields to set (the others take
    /// their defaults).
    Components(ComponentsDoc),
}

/// Re-parents a generated entity: `{"parent": <key or null>, "index"?: n}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Move {
    /// Key of the new parent in the same source's output; `None` (or
    /// missing) is the source itself.
    #[serde(default)]
    pub parent: Option<String>,
    /// The position among the new parent's generated children (default:
    /// last).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

/// Replaces what a generated entity is:
/// `{"prefab": path}` or `{"components": {...}}`, with `"keep_children"?`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawReplace", into = "RawReplace")]
pub struct Replace {
    /// What it becomes. Its generated `Transform` stays unless this sets one.
    pub with: Replacement,
    /// Keep the entity's generated descendants (by default they are not
    /// spawned).
    pub keep_children: bool,
}

/// Adds an authored entity to the output:
/// `{"parent"?, "index"?, "name"?, "prefab" | "components"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawInsert", into = "RawInsert")]
pub struct Insert {
    /// Key of the parent in the source's output; `None` is the source itself.
    pub parent: Option<String>,
    /// The position among the parent's generated children (default: last).
    pub index: Option<usize>,
    /// The display name (default: the prefab root's name, or the key).
    pub name: Option<String>,
    /// What it is.
    pub with: Replacement,
}

pub(crate) fn replacement(
    prefab: Option<String>,
    components: Option<ComponentsDoc>,
) -> Result<Replacement, String> {
    match (prefab, components) {
        (Some(p), None) if !p.is_empty() => Ok(Replacement::Prefab(p)),
        (None, Some(c)) => Ok(Replacement::Components(c)),
        _ => Err("expected either `prefab` or `components`".into()),
    }
}

pub(crate) fn split(with: Replacement) -> (Option<String>, Option<ComponentsDoc>) {
    match with {
        Replacement::Prefab(p) => (Some(p), None),
        Replacement::Components(c) => (None, Some(c)),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReplace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefab: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    components: Option<ComponentsDoc>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    keep_children: bool,
}

impl TryFrom<RawReplace> for Replace {
    type Error = String;
    fn try_from(r: RawReplace) -> Result<Self, String> {
        Ok(Replace {
            with: replacement(r.prefab, r.components)?,
            keep_children: r.keep_children,
        })
    }
}

impl From<Replace> for RawReplace {
    fn from(r: Replace) -> Self {
        let (prefab, components) = split(r.with);
        RawReplace {
            prefab,
            components,
            keep_children: r.keep_children,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInsert {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefab: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    components: Option<ComponentsDoc>,
}

impl TryFrom<RawInsert> for Insert {
    type Error = String;
    fn try_from(r: RawInsert) -> Result<Self, String> {
        Ok(Insert {
            parent: r.parent,
            index: r.index,
            name: r.name,
            with: replacement(r.prefab, r.components)?,
        })
    }
}

impl From<Insert> for RawInsert {
    fn from(i: Insert) -> Self {
        let (prefab, components) = split(i.with);
        RawInsert {
            parent: i.parent,
            index: i.index,
            name: i.name,
            prefab,
            components,
        }
    }
}

/// Checks the key an insert is written with (`lamp1` for `+lamp1`).
pub fn check_insert_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.contains('/') || !is_generator_key(key) {
        return Err(format!(
            "insert key {key:?} must be non-empty, without `/`, and not start with `+` or `~`"
        ));
    }
    Ok(())
}

/// The project files structural overrides read prefabs through (set by the
/// host; without it, a prefab replacement or insert is not applied).
#[derive(bevy_ecs::resource::Resource, Clone, Debug)]
pub struct SceneFiles(pub ProjectFiles);

/// Turns a [`Replacement`] into nodes: the first is the entity itself (with
/// `key` and no parent key), the rest its descendants keyed
/// `<key>/~<prefab entity id>`.
pub fn resolve(
    with: &Replacement,
    key: &str,
    registry: &Registry,
    files: Option<&ProjectFiles>,
    ids: &dyn Fn(&str) -> Option<bevy_ecs::entity::Entity>,
) -> Result<SceneFragment, SceneError> {
    match with {
        Replacement::Components(components) => {
            let mut out = Vec::with_capacity(components.len());
            for (cname, fields) in components {
                let key = registry.key(cname)?;
                let schema = registry.schema(key);
                let mut values = Vec::with_capacity(fields.len());
                for (fname, doc) in fields {
                    let index = schema.field_index(fname)?;
                    let v: Value =
                        doc.to_value(&schema.fields[index].ty, ids)
                            .map_err(|error| re_reflect::ReflectError::Value {
                                path: format!("{cname}.{fname}"),
                                error,
                            })?;
                    values.push((fname.clone(), v));
                }
                out.push((cname.clone(), values));
            }
            Ok(vec![GenNode {
                key: key.to_string(),
                name: String::new(),
                parent_key: None,
                components: out,
            }])
        }
        Replacement::Prefab(path) => {
            let files = files.ok_or_else(|| {
                SceneError::Invalid(format!("prefab {path:?}: no project files to read it from"))
            })?;
            let data = files
                .read(path)
                .map_err(|e| SceneError::Invalid(format!("prefab {path:?}: {e}")))?;
            let text = std::str::from_utf8(&data.bytes)
                .map_err(|_| SceneError::Invalid(format!("prefab {path:?} is not UTF-8")))?;
            let prefab = PrefabFile::from_json(text)?;
            let roots = prefab
                .entities
                .iter()
                .filter(|e| e.parent.is_none())
                .count();
            if roots != 1 {
                return Err(SceneError::Invalid(format!(
                    "prefab {path:?} has {roots} root entities; a replacement or insert needs one"
                )));
            }
            let mut nodes = prefab.instantiate(registry, key)?;
            let root = std::mem::take(&mut nodes[0].key);
            let rekey = |k: &str| -> String {
                if k == root {
                    key.to_string()
                } else {
                    format!("{key}/{REPLACED_PREFIX}{}", &k[key.len() + 1..])
                }
            };
            for (i, n) in nodes.iter_mut().enumerate() {
                n.key = if i == 0 {
                    key.to_string()
                } else {
                    rekey(&n.key)
                };
                n.parent_key = n.parent_key.as_deref().map(rekey);
            }
            Ok(nodes)
        }
    }
}

/// A structural override that could not be applied because its target or
/// its parent is not in the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Missing {
    pub target: String,
    pub kind: StructuralKind,
}

/// What [`apply_structure`] did besides returning the fragment.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    /// Overrides whose target or parent is not in the output (kept).
    pub missing: Vec<Missing>,
    /// Overrides that could not be applied, with why (`RE1209`).
    pub failed: Vec<(String, String)>,
}

/// One fragment as a tree: nodes by key, and each parent's children (by
/// key, in order; `None` is the source).
struct Tree {
    nodes: HashMap<String, GenNode>,
    children: HashMap<Option<String>, Vec<String>>,
}

impl Tree {
    fn new(fragment: SceneFragment) -> Tree {
        let mut children: HashMap<Option<String>, Vec<String>> = HashMap::new();
        let mut nodes = HashMap::with_capacity(fragment.len());
        for n in fragment {
            children
                .entry(n.parent_key.clone())
                .or_default()
                .push(n.key.clone());
            nodes.insert(n.key.clone(), n);
        }
        Tree { nodes, children }
    }

    fn has(&self, key: &str) -> bool {
        self.nodes.contains_key(key)
    }

    /// Puts `key` under `parent` at `index` (clamped; last when `None`).
    fn place(&mut self, key: &str, parent: Option<String>, index: Option<usize>) {
        if let Some(n) = self.nodes.get_mut(key) {
            let old = std::mem::replace(&mut n.parent_key, parent.clone());
            if let Some(list) = self.children.get_mut(&old) {
                list.retain(|k| k != key);
            }
        }
        let list = self.children.entry(parent).or_default();
        let at = index.map_or(list.len(), |i| i.min(list.len()));
        list.insert(at, key.to_string());
    }

    /// Adds `nodes` (the first is the root, placed under `parent` at
    /// `index`; the rest name their parents).
    fn add(&mut self, nodes: SceneFragment, parent: Option<String>, index: Option<usize>) {
        let mut nodes = nodes.into_iter();
        let Some(mut root) = nodes.next() else {
            return;
        };
        let key = root.key.clone();
        root.parent_key = None;
        self.nodes.insert(key.clone(), root);
        self.children.entry(None).or_default().push(key.clone());
        self.place(&key, parent, index);
        for n in nodes {
            self.children
                .entry(n.parent_key.clone())
                .or_default()
                .push(n.key.clone());
            self.nodes.insert(n.key.clone(), n);
        }
    }

    /// Removes the descendants of `key`.
    fn remove_descendants(&mut self, key: &str) {
        let mut stack = self
            .children
            .remove(&Some(key.to_string()))
            .unwrap_or_default();
        while let Some(k) = stack.pop() {
            self.nodes.remove(&k);
            stack.extend(self.children.remove(&Some(k)).unwrap_or_default());
        }
    }

    /// True if `key` is `of` or one of its descendants.
    fn is_within(&self, key: &str, of: &str) -> bool {
        let mut at = Some(key.to_string());
        while let Some(k) = at {
            if k == of {
                return true;
            }
            at = self.nodes.get(&k).and_then(|n| n.parent_key.clone());
        }
        false
    }

    /// Parents before children, siblings in order.
    fn into_fragment(mut self) -> SceneFragment {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<String> = self.children.remove(&None).unwrap_or_default();
        stack.reverse();
        while let Some(k) = stack.pop() {
            if let Some(n) = self.nodes.remove(&k) {
                out.push(n);
            }
            let mut kids = self.children.remove(&Some(k)).unwrap_or_default();
            kids.reverse();
            stack.extend(kids);
        }
        out
    }
}

/// Applies the structural overrides among `overrides` to `fragment`: inserts
/// are added, replacements applied, moves re-parented, and the result put
/// back into parents-before-children order. Without structural overrides the
/// fragment is returned as it is.
///
/// `resolve` turns a replacement into nodes (see [`resolve`]); an error
/// leaves that override unapplied and is reported in [`Applied::failed`].
pub(crate) fn apply_structure(
    fragment: SceneFragment,
    overrides: &[&OverrideDoc],
    resolve: &mut dyn FnMut(&Replacement, &str) -> Result<SceneFragment, SceneError>,
) -> (SceneFragment, Applied) {
    let mut applied = Applied::default();
    if !overrides.iter().any(|o| o.structural_kind().is_some()) {
        return (fragment, applied);
    }
    let mut tree = Tree::new(fragment);
    let missing = |target: &str, kind| Missing {
        target: target.to_string(),
        kind,
    };

    for o in overrides {
        let OverrideDoc::Inserted { target, insert } = o else {
            continue;
        };
        if tree.has(target) {
            continue;
        }
        if insert.parent.as_ref().is_some_and(|p| !tree.has(p)) {
            applied
                .missing
                .push(missing(target, StructuralKind::Inserted));
            continue;
        }
        match resolve(&insert.with, target) {
            Ok(mut nodes) => {
                let name = insert.name.clone().unwrap_or_else(|| {
                    if nodes[0].name.is_empty() {
                        target[1..].to_string()
                    } else {
                        nodes[0].name.clone()
                    }
                });
                nodes[0].name = name;
                tree.add(nodes, insert.parent.clone(), insert.index);
            }
            Err(e) => applied.failed.push((
                target.clone(),
                format!("insert {target:?} not applied: {e}"),
            )),
        }
    }

    for o in overrides {
        let OverrideDoc::Replaced { target, replace } = o else {
            continue;
        };
        if !tree.has(target) {
            applied
                .missing
                .push(missing(target, StructuralKind::Replaced));
            continue;
        }
        let nodes = match resolve(&replace.with, target) {
            Ok(nodes) => nodes,
            Err(e) => {
                applied.failed.push((
                    target.clone(),
                    format!("replacement of {target:?} not applied: {e}"),
                ));
                continue;
            }
        };
        if !replace.keep_children {
            tree.remove_descendants(target);
        }
        let mut nodes = nodes.into_iter();
        let root = nodes.next().expect("resolve returns the entity first");
        let node = tree.nodes.get_mut(target).expect("checked above");
        let transform = node
            .components
            .iter()
            .find(|(c, _)| c == "Transform")
            .cloned();
        node.components = root.components;
        if let Some(t) = transform
            && !node.components.iter().any(|(c, _)| c == "Transform")
        {
            node.components.insert(0, t);
        }
        for n in nodes {
            tree.children
                .entry(n.parent_key.clone())
                .or_default()
                .push(n.key.clone());
            tree.nodes.insert(n.key.clone(), n);
        }
    }

    let mut moved: HashSet<&str> = HashSet::new();
    for o in overrides {
        let OverrideDoc::Moved { target, to } = o else {
            continue;
        };
        if !tree.has(target) || to.parent.as_ref().is_some_and(|p| !tree.has(p)) {
            applied.missing.push(missing(target, StructuralKind::Moved));
            continue;
        }
        if let Some(p) = &to.parent
            && tree.is_within(p, target)
        {
            applied.failed.push((
                target.clone(),
                format!("move of {target:?} under {p:?} would make a cycle; left in place"),
            ));
            continue;
        }
        if moved.insert(target) {
            tree.place(target, to.parent.clone(), to.index);
        }
    }

    (tree.into_fragment(), applied)
}
