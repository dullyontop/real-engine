//! The instance tree as JSON (`scene.dump_tree`, and the `dump_tree` step
//! of `play.run`): each entity's id, name, class, attributes and tags, and
//! optionally every component.

use re_core::Diagnostic;
use re_scene::hierarchy::{children, parent};
use re_scene::{Entity, SaveOptions, World};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

/// The most nodes one dump returns.
pub const MAX_NODES: usize = 10_000;

/// Arguments of `scene.dump_tree`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DumpArgs {
    /// The entity to start from, by id (`game`, `game/Workspace`); default:
    /// every root.
    #[serde(default)]
    pub root: Option<String>,
    /// How many levels below the start to include (default: all).
    #[serde(default)]
    pub depth: Option<u32>,
    /// Include every component's fields, not just the class, attributes and
    /// tags (default false).
    #[serde(default)]
    pub components: bool,
    /// The most nodes to return (default 2000, at most 10000).
    #[serde(default)]
    pub max_nodes: Option<usize>,
}

/// One entity in the tree.
#[derive(Debug, Serialize, JsonSchema)]
pub struct TreeNode {
    /// The stable id.
    pub id: String,
    /// The display name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The Roblox-style class (`ClassMarker`), if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Its attributes, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Json>,
    /// Its tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Json>,
    /// Every component's fields (with `components: true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<Json>,
    /// The children, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
    /// Children left out by `depth` or `max_nodes`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub children_not_listed: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The result of `scene.dump_tree`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DumpOutput {
    /// The tree (one node per root when no `root` is given).
    pub roots: Vec<TreeNode>,
    /// How many nodes were returned.
    pub nodes: usize,
    /// True when `max_nodes` cut the tree short.
    pub truncated: bool,
}

struct Dump<'a> {
    world: &'a World,
    components: bool,
    max_nodes: usize,
    nodes: usize,
    truncated: bool,
}

impl Dump<'_> {
    fn node(&mut self, e: Entity, depth: Option<u32>) -> Option<TreeNode> {
        if self.nodes >= self.max_nodes {
            self.truncated = true;
            return None;
        }
        let doc = self.world.entity_doc(e, SaveOptions::default())?;
        self.nodes += 1;
        let field = |component: &str, field: &str| -> Option<Json> {
            doc.components
                .get(component)
                .and_then(|c| c.get(field))
                .and_then(|v| serde_json::to_value(v).ok())
        };
        let class = field("ClassMarker", "class").and_then(|v| v.as_str().map(str::to_string));
        let attributes =
            field("Attributes", "values").filter(|v| v.as_object().is_some_and(|m| !m.is_empty()));
        let tags = field("Tags", "tags").filter(|v| v.as_array().is_some_and(|a| !a.is_empty()));
        let components = self.components.then(|| {
            let mut map = Map::new();
            for (name, fields) in &doc.components {
                map.insert(
                    name.clone(),
                    serde_json::to_value(fields).unwrap_or(Json::Null),
                );
            }
            Json::Object(map)
        });
        let kids = children(&self.world.ecs, e).to_vec();
        let mut node = TreeNode {
            id: doc.id,
            name: doc.name,
            class,
            attributes,
            tags,
            components,
            children: Vec::new(),
            children_not_listed: 0,
        };
        match depth {
            Some(0) => node.children_not_listed = kids.len(),
            _ => {
                let next = depth.map(|d| d - 1);
                for (i, child) in kids.iter().enumerate() {
                    match self.node(*child, next) {
                        Some(n) => node.children.push(n),
                        None if self.truncated => {
                            node.children_not_listed = kids.len() - i;
                            break;
                        }
                        None => {}
                    }
                }
            }
        }
        Some(node)
    }
}

/// The tree `args` asks for.
///
/// # Errors
///
/// `args.root` names no entity.
pub fn dump_tree(world: &World, args: &DumpArgs) -> Result<DumpOutput, Diagnostic> {
    let starts: Vec<Entity> = match &args.root {
        Some(id) => vec![world.entity(id).map_err(|e| e.to_diagnostic())?],
        None => {
            let mut roots: Vec<(&str, Entity)> = world
                .ids()
                .iter()
                .filter(|(_, e)| parent(&world.ecs, *e).is_none())
                .collect();
            roots.sort_by(|a, b| a.0.cmp(b.0));
            roots.into_iter().map(|(_, e)| e).collect()
        }
    };
    let mut dump = Dump {
        world,
        components: args.components,
        max_nodes: args.max_nodes.unwrap_or(2000).clamp(1, MAX_NODES),
        nodes: 0,
        truncated: false,
    };
    let mut roots = Vec::new();
    for e in starts {
        if let Some(n) = dump.node(e, args.depth) {
            roots.push(n);
        }
    }
    Ok(DumpOutput {
        roots,
        nodes: dump.nodes,
        truncated: dump.truncated,
    })
}

