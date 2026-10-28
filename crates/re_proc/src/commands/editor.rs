//! `proc.convert_builder` (the ADR-0044 amendment, §7): the node canvas's
//! "Convert to a graph file", which turns a procedural source whose graph a
//! builder script makes into one that reads a graph file.
//!
//! The file is made through an absent document (ADR-0064): the command
//! opens it absent and, in one transaction, gives it the graph the builder
//! made (with the positions the canvas drew it at in `layout`), sets
//! `GraphSource.graph` to it and empties `GraphSource.builder`. Generation
//! reads the open document, so the source generates the same fragment
//! before the file is saved. Undo empties the document again, so the next
//! save removes the file, and gives the source its builder back. The script
//! stays where it is.

use std::collections::BTreeMap;
use std::path::Path;

use re_command::{Call, CommandRegistry, EditTarget, Op, Tier, codes as command_codes};
use re_core::{Diagnostic, Location};
use re_reflect::{Doc, Value};
use re_scene::World;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use super::{graph_source, store_of, valid_graph_path};
use crate::codes;
use crate::store::GraphStore;

/// Arguments of `proc.convert_builder`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConvertBuilderArgs {
    /// The procedural source: one whose graph its `GraphSource.builder`
    /// script makes.
    pub source: String,
    /// The graph file to make, relative to the project, ending in
    /// `.graph.json` (default `graphs/<source>.graph.json`). It must not
    /// exist, or be open with content.
    #[serde(default)]
    #[schemars(extend("x-real-path" = "write"))]
    pub graph: Option<String>,
    /// Where the node canvas draws nodes, `[x, y]` by node id, written into
    /// the file's `layout` (the editor sends the positions it laid the built
    /// graph out at). Nodes without one are laid out by the canvas.
    #[serde(default)]
    pub layout: BTreeMap<String, [f32; 2]>,
}

fn not_builder(id: &str, why: &str) -> Diagnostic {
    Diagnostic::error(
        codes::NOT_A_BUILDER_SOURCE,
        format!("`{id}` is not a builder source: {why}"),
    )
    .at(Location::entity(id.to_string()))
}

fn taken(path: &str, why: &str) -> Diagnostic {
    Diagnostic::error(
        codes::GRAPH_FILE_TAKEN,
        format!("cannot make {path}: {why}"),
    )
    .at(Location::file(path.to_string()))
    .with_hint("pass `graph` to make another file")
}

fn bad_argument(message: String) -> Diagnostic {
    Diagnostic::error(command_codes::INVALID_ARGUMENTS, message)
        .at(Location::command("proc.convert_builder"))
}

/// The graph file a source converts to by default:
/// `graphs/<source id>.graph.json`, lower case, with `_` for what a file
/// name does not hold.
fn default_path(id: &str) -> String {
    let stem: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("graphs/{stem}.graph.json")
}

/// True when the project has a file at `path`, as `store` reads files.
fn file_exists(store: &GraphStore, path: &str) -> bool {
    match (store.files(), store.root()) {
        (Some(files), _) => files.is_file(path),
        (None, Some(root)) => root.join(path).is_file(),
        (None, None) => Path::new(path).is_file(),
    }
}

/// A `GraphSource` field of `id`, or `""`.
fn source_field(world: &World, id: &str, field: &str) -> String {
    match world.get(id, "GraphSource", field) {
        Ok(Value::Str(s)) => s,
        _ => String::new(),
    }
}

fn convert_builder<C: EditTarget>(
    target: &mut C,
    a: ConvertBuilderArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let src = graph_source(world, &a.source)?;
    let graph = source_field(world, &a.source, "graph");
    if !graph.trim().is_empty() {
        return Err(not_builder(
            &a.source,
            &format!("it reads the graph file {graph}"),
        ));
    }
    let builder = source_field(world, &a.source, "builder");
    if builder.trim().is_empty() {
        return Err(not_builder(&a.source, "it has no builder script"));
    }
    let path = match &a.graph {
        Some(g) => re_scene::doc_path(g)
            .ok()
            .filter(|p| valid_graph_path(p))
            .ok_or_else(|| {
                Diagnostic::error(
                    command_codes::DOCUMENT_PATH,
                    format!(
                        "`{g}` is not a graph file path: a project path of plain names \
                         ending in `.graph.json`"
                    ),
                )
                .at(Location::command("proc.convert_builder"))
            })?,
        None => default_path(&a.source),
    };
    let mut doc = src.doc;
    for (node, p) in &a.layout {
        if doc.node(node).is_none() {
            return Err(bad_argument(format!(
                "`layout` places node `{node}`, which the built graph does not have"
            )));
        }
        if !p.iter().all(|x| x.is_finite()) {
            return Err(bad_argument(format!(
                "`layout` places node `{node}` at a position that is not a finite pair"
            )));
        }
    }
    doc.layout.extend(a.layout);
    let content = serde_json::to_value(&doc).expect("a GraphDoc always serializes");

    // The file is free: the World holds no content for it, and the project
    // has no such file (or it is open absent, so the next save removes it).
    let opened = match world.doc(&path) {
        Some(d) if d.is_absent() => false,
        Some(_) => return Err(taken(&path, "it is open with content")),
        None => {
            let store = store_of(world).expect("graph_source checked the store");
            if file_exists(&store, &path) {
                return Err(taken(&path, "the file exists"));
            }
            crate::register_doc_validators(world);
            world
                .open_doc(&path, Json::Null)
                .map_err(|e| bad_argument(e.to_string()))?;
            true
        }
    };
    let nodes = doc.nodes.len();
    let result = call.transact(
        target,
        format!("proc.convert_builder {} to {path}", a.source),
        vec![
            Op::SetDoc {
                doc: path.clone(),
                pointer: String::new(),
                value: Some(content),
            },
            Op::SetField {
                id: a.source.clone(),
                component: "GraphSource".into(),
                field: "graph".into(),
                value: Doc::Str(path.clone()),
            },
            Op::SetField {
                id: a.source.clone(),
                component: "GraphSource".into(),
                field: "builder".into(),
                value: Doc::Str(String::new()),
            },
        ],
    );
    if opened && (result.is_err() || call.dry_run()) {
        target.world().close_doc(&path);
    }
    result?;
    let mut data = json!({
        "source": a.source,
        "graph": path,
        "builder": builder,
        "nodes": nodes,
    });
    if call.dry_run() {
        data["dry_run"] = Json::Bool(true);
    }
    Ok(data)
}

/// Registers `proc.convert_builder`.
pub(super) fn register<C: EditTarget + 'static>(registry: &mut CommandRegistry<C>) {
    registry.register_typed::<ConvertBuilderArgs, _>(
        "proc.convert_builder",
        "Turn a procedural source whose graph a builder script makes into one that reads a graph file: makes the file (default graphs/<source>.graph.json) with the built graph and `layout`, and points GraphSource.graph at it. One undo step; the project save writes the file, and after an undo removes it. The script is kept.",
        Tier::Edit,
        false,
        convert_builder::<C>,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_file_is_named_after_the_source() {
        assert_eq!(default_path("island"), "graphs/island.graph.json");
        assert_eq!(default_path("Town/Hill 2"), "graphs/town_hill_2.graph.json");
        assert!(valid_graph_path(&default_path("Town/Hill 2")));
    }
}
