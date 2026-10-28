//! Graph migration (ADR-0043): bringing every node of a graph document up to
//! its kind's current version, and graph files up to the current file format
//! (ADR-0061 §10), and the report of what that changes.
//!
//! Nodes always evaluate at the version they record, so a graph written for an
//! older engine keeps its output. Migrating is an explicit step: [`migrate_doc`]
//! runs each node's parameter migrations in memory, [`upgrade_format`] writes a
//! version 1 document as version 2 (only its `version` changes), and
//! [`migration_report`] says, per procedural source, which nodes move, whether
//! the output changes, which entity keys appear or go, and which overrides
//! stop applying, and per graph file, its format before and after.
//! `real migrate-graph` writes the result; `graph.migration_report` only reads.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use re_core::{Diagnostic, Location, Severity};
use re_scene::World;
use re_types::graph::GraphDoc;
use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::canon::doc_hash;
use crate::codes;
use crate::eval::{EvalOptions, fragment_hash_hex};
use crate::generator::{GENERATOR, apply_overrides, check_override, parse_overrides};
use crate::graph::ProcGraph;
use crate::nodes::NodeRegistry;
use crate::params;
use crate::store::GraphStore;

/// One node moved to its kind's current version.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeMigration {
    /// The node's id.
    pub id: String,
    /// Its kind.
    pub kind: String,
    /// The version the document recorded.
    pub from: u32,
    /// The kind's current version.
    pub to: u32,
    /// Each migration step's note, in order.
    pub notes: Vec<String>,
    /// True when a step says the node's output changes for the same inputs.
    pub changes_output: bool,
    /// The parameters before.
    pub params_before: Map<String, Json>,
    /// The parameters after.
    pub params_after: Map<String, Json>,
}

/// True when two param values mean the same to a node: equal, with numbers
/// compared by value (a step that writes `4` back as `4.0` keeps it).
fn same_value(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => x.as_f64() == y.as_f64(),
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_value(x, y))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| same_value(v, w)))
        }
        _ => a == b,
    }
}

/// `doc` with every node at its kind's current version, and what moved. A node
/// of an unknown kind is left as it is (validation reports it). A node param
/// bound to a graph param migrates with the param's default in place, and
/// keeps its reference when the migration leaves that value where it was
/// (ADR-0061 §2). `Err` is `RE4018` for a node older than its kind's oldest
/// supported version, or `RE4020` for a migration step that fails, a bound
/// param without a default, or a step that drops or changes a bound value
/// (the binding is named, to be unbound or bound again by hand); nothing is
/// migrated then.
pub fn migrate_doc(
    doc: &GraphDoc,
    registry: &NodeRegistry,
) -> Result<(GraphDoc, Vec<NodeMigration>), Diagnostic> {
    let mut out = doc.clone();
    let mut moved = Vec::new();
    for node in &mut out.nodes {
        let Some(def) = registry.get(&node.kind) else {
            continue;
        };
        if node.version >= def.version || node.version == 0 {
            continue;
        }
        if !def.supports(node.version) {
            return Err(Diagnostic::error(
                codes::RETIRED_NODE_VERSION,
                format!(
                    "graph `{}`: node `{}` ({}) is written for version {}, older than the oldest version this engine can migrate ({})",
                    doc.name, node.id, node.kind, node.version, def.oldest
                ),
            )
            .with_hint("migrate it with an older engine first, or replace the node"));
        }
        let failed = |message: String| {
            Diagnostic::error(
                codes::MIGRATION_FAILED,
                format!(
                    "graph `{}`: node `{}` ({}): {message}",
                    doc.name, node.id, node.kind
                ),
            )
        };
        // Each bound param, its graph param and the default written in its place.
        let mut bound = Vec::new();
        let mut before = node.params.clone();
        for (param, value) in &mut before {
            let Some(name) = GraphDoc::param_ref(value) else {
                continue;
            };
            let Some(default) = params::bound_value(doc, name) else {
                return Err(failed(format!(
                    "`{}.{param}` is bound to graph param `{name}`, which has no default to migrate it with",
                    node.id
                ))
                .with_hint("give the graph param a default, or write a value in place of the reference, then migrate"));
            };
            bound.push((param.clone(), name.to_string(), default.clone()));
            *value = default;
        }
        let before = Json::Object(before);
        let after = def.migrate(node.version, &before).map_err(|(from, e)| {
            failed(format!("the migration from version {from} failed: {e}"))
                .with_hint("fix the node's parameters so the migration accepts them")
        })?;
        let steps = def.steps_from(node.version);
        let mut params_after = match after.into_owned() {
            Json::Object(m) => m,
            _ => Map::new(),
        };
        for (param, name, default) in bound {
            match params_after.get_mut(&param) {
                Some(value) if same_value(value, &default) => *value = params::reference(&name),
                _ => {
                    return Err(failed(format!(
                        "the migration from version {} drops or changes `{}.{param}`, which is bound to graph param `{name}`",
                        node.version, node.id
                    ))
                    .with_hint("write a value in place of the reference (`graph.set_default`), migrate, then bind the param again"));
                }
            }
        }
        moved.push(NodeMigration {
            id: node.id.clone(),
            kind: node.kind.clone(),
            from: node.version,
            to: def.version,
            notes: if def.migrates_custom() {
                vec![format!(
                    "migrated by the kind's script from version {}",
                    node.version
                )]
            } else {
                steps.iter().map(|s| s.note.to_string()).collect()
            },
            // A script's steps say nothing about their output: assume they change it.
            changes_output: def.migrates_custom() || steps.iter().any(|s| s.changes_output),
            params_before: node.params.clone(),
            params_after: params_after.clone(),
        });
        node.params = params_after;
        node.version = def.version;
    }
    Ok((out, moved))
}

/// What migrating one procedural source changes.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SourceMigration {
    /// The source's id.
    pub source: String,
    /// Its `GraphSource.graph` file, when the graph comes from one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Its `GraphSource.builder` script, when the graph comes from one. A
    /// builder is not rewritten: its nodes move when the script asks for
    /// `kind@<version>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builder: Option<String>,
    /// The nodes that move.
    pub nodes: Vec<NodeMigration>,
    /// The output's fragment hash now, when it evaluates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fragment_hash_before: Option<String>,
    /// The output's fragment hash after migrating, when it evaluates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fragment_hash_after: Option<String>,
    /// True when the output differs (or could not be compared).
    pub output_changed: bool,
    /// Entity keys the migrated graph makes that the current one does not.
    pub keys_added: Vec<String>,
    /// Entity keys the current graph makes that the migrated one does not;
    /// overrides on those entities are orphaned when it regenerates.
    pub keys_removed: Vec<String>,
    /// `GraphSource.params` overrides that apply now but not after migrating
    /// (`<node>.<param>`).
    pub overrides_orphaned: Vec<String>,
    /// Problems with this source.
    pub diagnostics: Vec<Diagnostic>,
}

/// A graph file's format version before and after its rewrite.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FormatChange {
    /// The version the file has.
    pub from: u32,
    /// The version it is written at.
    pub to: u32,
}

/// One graph file `real migrate-graph` rewrites.
#[derive(Clone, Debug, Serialize)]
pub struct FileRewrite {
    /// The project-relative path.
    pub path: String,
    /// The sources that read it.
    pub sources: Vec<String>,
    /// Its format version before and after.
    pub format: FormatChange,
    /// The migrated document (not serialized; `real migrate-graph` writes it).
    #[serde(skip)]
    pub doc: GraphDoc,
}

/// What `real migrate-graph` upgrades.
#[derive(Clone, Copy, Debug, Default)]
pub struct MigrateOptions {
    /// Upgrade graph files' format alone, from version 1 to version 2, and no
    /// node (`--format-only`).
    pub format_only: bool,
}

/// What migrating every graph of a world changes.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MigrationReport {
    /// True when every source migrates (an all-or-nothing rewrite may go ahead).
    pub ok: bool,
    /// True when only the files' format is upgraded ([`MigrateOptions`]).
    pub format_only: bool,
    /// Per source with something to migrate, in id order.
    pub sources: Vec<SourceMigration>,
    /// The graph files that change.
    pub files: Vec<FileRewrite>,
    /// Graph files no open source reads whose nodes move
    /// ([`add_unopened_files`]); `files` lists them, and those whose format
    /// alone is upgraded.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unopened: Vec<UnopenedFile>,
    /// Problems not tied to one source.
    pub diagnostics: Vec<Diagnostic>,
}

impl MigrationReport {
    /// Every diagnostic, the report's own then each source's.
    pub fn all_diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .chain(self.sources.iter().flat_map(|s| s.diagnostics.iter()))
    }
}

/// The overrides that apply to `doc`, as `<node>.<param>`.
fn applying(
    doc: &GraphDoc,
    registry: &NodeRegistry,
    overrides: &Map<String, Json>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (node, params) in overrides {
        for (param, value) in params.as_object().into_iter().flatten() {
            if check_override(doc, registry, node, param, value).is_ok() {
                out.insert(format!("{node}.{param}"));
            }
        }
    }
    out
}

/// The source's output with `doc`: its fragment hash and entity keys, or the
/// first error.
fn output(
    world: &World,
    store: &GraphStore,
    source: &str,
    doc: &GraphDoc,
    overrides: &Map<String, Json>,
    seed: i64,
) -> Result<(String, BTreeSet<String>), Diagnostic> {
    let registry = store.evaluator().registry();
    let (doc, _) = apply_overrides(doc, overrides, registry, source);
    let graph = ProcGraph::from_doc(&doc, registry).map_err(|mut ds| ds.remove(0))?;
    let opts = EvalOptions {
        seed: Some(seed as u64),
        root: store.root().map(Into::into),
        files: store.files().cloned(),
        ..Default::default()
    };
    let (fragment, _) = store
        .evaluator()
        .evaluate(&graph, &opts)
        .scene_fragment(&world.registry)?;
    let keys = fragment.iter().map(|n| n.key.clone()).collect();
    Ok((fragment_hash_hex(&fragment), keys))
}

/// A project path as `re_app` reads it: `\\` as `/`, without `./` segments.
fn normalise(path: &str) -> String {
    let p = path.replace('\\', "/");
    let mut p = p.trim_start_matches("./").replace("/./", "/");
    while p.contains("//") {
        p = p.replace("//", "/");
    }
    p
}

/// Reads and parses the graph file `path` under `root`.
fn read_graph(root: &Path, path: &str) -> Result<GraphDoc, Diagnostic> {
    let text = std::fs::read_to_string(root.join(path)).map_err(|e| {
        Diagnostic::error(codes::INVALID_GRAPH, format!("cannot read {path}: {e}"))
            .at(Location::file(path.to_string()))
    })?;
    GraphDoc::from_json(&text).map_err(|e| {
        Diagnostic::error(
            codes::INVALID_GRAPH,
            format!("{path} is not a valid graph: {e}"),
        )
        .at(Location::file(path.to_string()))
    })
}

fn text_field(world: &World, id: &str, field: &str) -> String {
    match world.get(id, "GraphSource", field) {
        Ok(re_reflect::Value::Str(s)) => s,
        _ => String::new(),
    }
}

/// `doc`, read from the graph file `path`, written at the current file format
/// version: only `version` changes, so it means what it meant (ADR-0061 §10).
/// A document at the current version comes back as it is. `Err` (`RE4000`)
/// when version 2 cannot hold the document: a node id it does not allow, or
/// `"$param"` held inside a node param's value (a literal in version 1,
/// `RE4204` in version 2).
pub fn upgrade_format(doc: &GraphDoc, path: &str) -> Result<GraphDoc, Diagnostic> {
    let mut out = doc.clone();
    if out.version >= GraphDoc::VERSION {
        return Ok(out);
    }
    out.version = GraphDoc::VERSION;
    let problem = match out.validate() {
        Err(e) => Some(e.to_string()),
        Ok(()) => params::check_doc(&out)
            .into_iter()
            .find(|d| d.severity == Severity::Error)
            .map(|d| d.message),
    };
    match problem {
        None => Ok(out),
        Some(problem) => Err(Diagnostic::error(
            codes::INVALID_GRAPH,
            format!(
                "{path} cannot be written as graph file version {}: {problem}",
                GraphDoc::VERSION
            ),
        )
        .at(Location::file(path.to_string()))
        .with_hint("change the file by hand so that version 2 holds it, then migrate again")),
    }
}

/// `doc` with its nodes migrated ([`migrate_doc`]), or as it is when only the
/// format is upgraded.
fn migrate_nodes(
    doc: &GraphDoc,
    registry: &NodeRegistry,
    format_only: bool,
) -> Result<(GraphDoc, Vec<NodeMigration>), Diagnostic> {
    if format_only {
        Ok((doc.clone(), Vec::new()))
    } else {
        migrate_doc(doc, registry)
    }
}

/// The migration report of every `graph` source in `world`
/// ([`migration_report_with`] with the default options: nodes and format).
pub fn migration_report(world: &World, root: Option<&Path>) -> MigrationReport {
    migration_report_with(world, root, MigrateOptions::default())
}

/// The migration report of every `graph` source in `world`, from the
/// documents in its [`GraphStore`]. Sources with nothing to migrate (no node
/// that moves, and no graph file at an older format version) are left out.
/// `root` (the project) is where graph files are read, to flag a source
/// whose stored document differs from its file (an unsaved edit, which a
/// rewrite of the file would not include). Only a source's nodes that move
/// are evaluated, to compare its output: a format upgrade alone changes no
/// output.
pub fn migration_report_with(
    world: &World,
    root: Option<&Path>,
    opts: MigrateOptions,
) -> MigrationReport {
    let mut report = MigrationReport {
        ok: true,
        format_only: opts.format_only,
        ..Default::default()
    };
    let Some(store) = world.ecs.get_resource::<GraphStore>() else {
        return report;
    };
    let registry = store.evaluator().registry();
    let migrate = |doc: &GraphDoc| migrate_nodes(doc, registry, opts.format_only);
    let mut files: BTreeMap<String, (GraphDoc, FormatChange, Vec<String>)> = BTreeMap::new();
    let mut ids: Vec<&str> = store.graphs().map(|(id, _)| id).collect();
    ids.sort_unstable();
    for id in ids {
        let doc = store.graph(id).expect("listed above");
        let Ok(entity) = world.entity(id) else {
            continue;
        };
        let Some(p) = world.ecs.get::<re_scene::Procedural>(entity) else {
            continue;
        };
        if p.generator != GENERATOR {
            continue;
        }
        let seed = p.seed;
        let file = text_field(world, id, "graph");
        let builder = text_field(world, id, "builder");
        let mut entry = SourceMigration {
            source: id.to_string(),
            file: (!file.is_empty()).then(|| normalise(&file)),
            builder: (file.is_empty() && !builder.is_empty()).then(|| builder.clone()),
            ..Default::default()
        };
        // A file source migrates what is in its file: the store may hold an
        // open, unsaved edit, which the rewrite must not write out.
        let on_disk = match (root, &entry.file) {
            (Some(root), Some(path)) => match read_graph(root, path) {
                Ok(d) => Some(d),
                Err(d) => {
                    // Only a source that has something to migrate needs it.
                    if doc.version >= GraphDoc::VERSION
                        && migrate(doc).is_ok_and(|(_, n)| n.is_empty())
                    {
                        continue;
                    }
                    report.ok = false;
                    entry.diagnostics.push(d);
                    report.sources.push(entry);
                    continue;
                }
            },
            _ => None,
        };
        let doc = on_disk.as_ref().unwrap_or(doc);
        let (migrated, nodes) = match migrate(doc) {
            Ok(m) => m,
            Err(d) => {
                report.ok = false;
                entry.diagnostics.push(at_source(d, id));
                report.sources.push(entry);
                continue;
            }
        };
        // A builder's graph is no file: only a graph file has a format to upgrade.
        let upgrade = entry
            .file
            .as_ref()
            .filter(|_| doc.version < GraphDoc::VERSION);
        if nodes.is_empty() && upgrade.is_none() {
            continue;
        }
        entry.nodes = nodes;
        let migrated = match upgrade.map(|path| upgrade_format(&migrated, path)) {
            None => migrated,
            Some(Ok(d)) => d,
            Some(Err(d)) => {
                report.ok = false;
                entry.diagnostics.push(d);
                report.sources.push(entry);
                continue;
            }
        };
        if let (Some(file), Some(path)) = (&on_disk, &entry.file) {
            // The graph generation uses: the open document if there is one
            // (ADR-0044), else the stored graph.
            let current = match store.current(world, id) {
                Some(Ok(c)) => doc_hash(&c),
                _ => doc_hash(store.graph(id).expect("listed above")),
            };
            let unsaved = world.doc(path).is_some_and(|d| d.dirty);
            if unsaved || doc_hash(file) != current {
                let message = format!(
                    "{id}: the open graph differs from {path}; the migration reads and rewrites the file, not the open graph"
                );
                // Rewriting the file under unsaved edits would lose one or
                // the other on the next save: refused.
                let d = if unsaved {
                    report.ok = false;
                    Diagnostic::error(codes::OPEN_GRAPH_DIFFERS, message)
                } else {
                    Diagnostic::warning(codes::OPEN_GRAPH_DIFFERS, message)
                };
                entry.diagnostics.push(
                    d.at(Location::file(path.clone()))
                        .with_hint("save or discard the open edits, then migrate again"),
                );
            }
        }
        if !entry.nodes.is_empty() {
            let overrides = parse_overrides(&text_field(world, id, "params")).unwrap_or_default();
            entry.overrides_orphaned = applying(doc, registry, &overrides)
                .difference(&applying(&migrated, registry, &overrides))
                .cloned()
                .collect();
            let before = output(world, store, id, doc, &overrides, seed);
            let after = output(world, store, id, &migrated, &overrides, seed);
            match (before, after) {
                (Ok((hb, kb)), Ok((ha, ka))) => {
                    entry.output_changed = hb != ha;
                    entry.fragment_hash_before = Some(hb);
                    entry.fragment_hash_after = Some(ha);
                    entry.keys_added = ka.difference(&kb).cloned().collect();
                    entry.keys_removed = kb.difference(&ka).cloned().collect();
                }
                (_, Err(d)) => {
                    entry.output_changed = true;
                    report.ok = false;
                    entry.diagnostics.push(at_source(d, id));
                }
                (Err(_), Ok((ha, ..))) => {
                    entry.output_changed = true;
                    entry.fragment_hash_after = Some(ha);
                }
            }
        }
        if let Some(path) = &entry.file {
            let format = FormatChange {
                from: doc.version,
                to: migrated.version,
            };
            let slot = files
                .entry(path.clone())
                .or_insert_with(|| (migrated.clone(), format, Vec::new()));
            slot.2.push(id.to_string());
        }
        report.sources.push(entry);
    }
    report.files = files
        .into_iter()
        .map(|(path, (doc, format, sources))| FileRewrite {
            path,
            sources,
            format,
            doc,
        })
        .collect();
    report
}

/// Adds the graph files in `paths` (project-relative, under `root`) that no
/// open source reads, such as graphs of scenes that are not loaded: their
/// nodes migrate too (unless the report upgrades the format alone), without
/// an output comparison, and their format is upgraded. A file that does not
/// parse or migrate makes the report not ok.
pub fn add_unopened_files(
    report: &mut MigrationReport,
    root: &Path,
    registry: &NodeRegistry,
    paths: &[String],
) {
    for path in paths {
        let path = &normalise(path);
        let known = report.files.iter().any(|f| &f.path == path)
            || report.sources.iter().any(|s| s.file.as_ref() == Some(path));
        if known {
            continue;
        }
        let doc = match read_graph(root, path) {
            Ok(d) => d,
            Err(d) => {
                report.ok = false;
                report.diagnostics.push(d);
                continue;
            }
        };
        let migrated = migrate_nodes(&doc, registry, report.format_only)
            .map_err(|d| d.at(Location::file(path.clone())))
            .and_then(|(migrated, nodes)| {
                if nodes.is_empty() && doc.version >= GraphDoc::VERSION {
                    return Ok(None);
                }
                Ok(Some((upgrade_format(&migrated, path)?, nodes)))
            });
        match migrated {
            Ok(None) => {}
            Ok(Some((migrated, nodes))) => {
                if !nodes.is_empty() {
                    report.unopened.push(UnopenedFile {
                        path: path.clone(),
                        nodes,
                    });
                }
                report.files.push(FileRewrite {
                    path: path.clone(),
                    sources: Vec::new(),
                    format: FormatChange {
                        from: doc.version,
                        to: migrated.version,
                    },
                    doc: migrated,
                });
            }
            Err(d) => {
                report.ok = false;
                report.diagnostics.push(d);
            }
        }
    }
    report.files.sort_by(|a, b| a.path.cmp(&b.path));
}

/// A graph file no open source reads, and the nodes in it that move.
#[derive(Clone, Debug, Serialize)]
pub struct UnopenedFile {
    /// The project-relative path.
    pub path: String,
    /// The nodes that move.
    pub nodes: Vec<NodeMigration>,
}

fn at_source(d: Diagnostic, source: &str) -> Diagnostic {
    if d.location.is_some() {
        d
    } else {
        d.at(Location::entity(source.to_string()))
    }
}

/// Writes each migrated file of `report` under `root`, in the stable
/// `GraphDoc` form, as close to all-or-nothing as a file system allows: every
/// file is first written beside its target, and only when all of them are
/// written are they renamed over the old ones. Writes nothing unless the
/// report is ok.
///
/// # Errors
///
/// The first file that cannot be written (then no file has changed and the
/// temporary files are removed), or renamed (then the files before it are
/// already replaced; they are named in the error).
pub fn write_files(report: &MigrationReport, root: &Path) -> std::io::Result<Vec<String>> {
    if !report.ok {
        return Ok(Vec::new());
    }
    let temp = |path: &str| root.join(format!("{path}.migrating"));
    let mut staged: Vec<&FileRewrite> = Vec::new();
    for f in &report.files {
        if let Err(e) = std::fs::write(temp(&f.path), f.doc.to_json()) {
            for s in staged.iter().copied().chain([f]) {
                let _ = std::fs::remove_file(temp(&s.path));
            }
            return Err(e);
        }
        staged.push(f);
    }
    let mut written = Vec::new();
    for f in staged {
        if let Err(e) = std::fs::rename(temp(&f.path), root.join(&f.path)) {
            for s in &report.files {
                let _ = std::fs::remove_file(temp(&s.path));
            }
            return Err(std::io::Error::new(
                e.kind(),
                format!(
                    "{e} (renaming {}; already rewritten: {})",
                    f.path,
                    if written.is_empty() {
                        "none".to_string()
                    } else {
                        written.join(", ")
                    }
                ),
            ));
        }
        written.push(f.path.clone());
    }
    Ok(written)
}
