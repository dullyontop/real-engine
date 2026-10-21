//! The `graph.*` commands (ARCHITECTURE §6): `proc.create_source`,
//! `graph.set_param` and `graph.regenerate` (edit), `graph.generate`, `graph.report` and
//! `graph.migration_report` (read).

use std::path::Path;

use re_command::{Call, CommandRegistry, EditTarget, Op, Tier};
use re_core::{Diagnostic, Location};
use re_reflect::{Doc, Value};
use re_scene::{Entity, Procedural, RegenReport, World};
use re_types::graph::GraphDoc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

use crate::codes;
use crate::generator::{
    GENERATOR, ParamOverrides, apply_overrides, overrides_text, parse_overrides, validate_override,
};
use crate::store::{Budget, GraphStore};

/// Arguments of `graph.set_param`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetParamArgs {
    /// The procedural source's id.
    pub source: String,
    /// The node's id in the source's graph.
    pub node: String,
    /// The parameter's name.
    pub param: String,
    /// The new value, checked against the node kind's parameter schema;
    /// `null` removes the override (the graph's own value applies again).
    pub value: Json,
}

/// Arguments of `graph.regenerate`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegenerateArgs {
    /// The procedural source's id.
    pub source: String,
    /// A new seed (`Procedural.seed`); without it the source is regenerated
    /// with its current seed and parameters.
    #[serde(default)]
    pub seed: Option<i64>,
}

/// Arguments of `graph.generate`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerateArgs {
    /// The procedural source's id.
    pub source: String,
    /// A seed to try instead of the source's.
    #[serde(default)]
    pub seed: Option<i64>,
    /// Parameter overrides to try on top of the source's:
    /// `{"<node>": {"<param>": value}}`.
    #[serde(default)]
    pub params: Option<Map<String, Json>>,
    /// Time budget in milliseconds (default 10000).
    #[serde(default)]
    pub max_ms: Option<u64>,
    /// Entity budget (default 100000).
    #[serde(default)]
    pub max_entities: Option<usize>,
}

/// Arguments of `graph.report`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportArgs {
    /// The procedural source's id.
    pub source: String,
}

/// Arguments of `graph.migration_report`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationReportArgs {
    /// Only this procedural source (default: every graph source).
    #[serde(default)]
    pub source: Option<String>,
}

/// Arguments of `proc.create_source`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSourceArgs {
    /// The node kind: one whose output is a fragment and whose inputs are
    /// all optional (a Luau generator, `project.*`).
    pub kind: String,
    /// The node's parameters, checked against the kind's schema.
    #[serde(default)]
    pub params: Map<String, Json>,
    /// The new source's id (generated when omitted).
    #[serde(default)]
    pub id: Option<String>,
    /// The display name (default: the kind without `project.`).
    #[serde(default)]
    pub name: Option<String>,
    /// The parent's id (a root when omitted).
    #[serde(default)]
    pub parent: Option<String>,
    /// `Procedural.seed` (default 0).
    #[serde(default)]
    pub seed: Option<i64>,
    /// The graph file to write (default `graphs/<name>.graph.json`, with
    /// `_2`, `_3`... when taken). It must not exist.
    #[serde(default)]
    pub graph: Option<String>,
}

/// The id of the one node of a graph `proc.create_source` writes.
pub const CREATED_NODE: &str = "gen";

/// What a command needs about its graph source.
struct Source {
    entity: Entity,
    doc: GraphDoc,
    /// `GraphSource.params`, or why it does not parse.
    overrides: Result<ParamOverrides, String>,
    seed: i64,
}

impl Source {
    /// The source's overrides; text that does not parse is an error rather
    /// than "no overrides", so a new override never silently replaces it.
    fn overrides(&self, id: &str) -> Result<ParamOverrides, Diagnostic> {
        self.overrides.clone().map_err(|e| {
            Diagnostic::error(codes::UNKNOWN_OVERRIDE, format!("{id}: {e}"))
                .at(Location::entity(id.to_string()))
                .with_hint("fix or clear `GraphSource.params` with `set_field` first")
        })
    }
}

/// `doc` with the overrides that apply, so a new value is checked against
/// the parameters the node really has (`scale_max` against an overridden
/// `scale_min`).
fn current_doc(
    doc: &GraphDoc,
    overrides: &ParamOverrides,
    registry: &crate::NodeRegistry,
    id: &str,
) -> GraphDoc {
    apply_overrides(doc, overrides, registry, id).0
}

fn at_source(d: Diagnostic, source: &str) -> Diagnostic {
    if d.location.is_some() {
        d
    } else {
        d.at(Location::entity(source.to_string()))
    }
}

fn graph_source(world: &World, id: &str) -> Result<Source, Diagnostic> {
    let entity = world.entity(id).map_err(|e| e.to_diagnostic())?;
    let not_graph = |why: String| {
        Diagnostic::error(
            codes::NOT_A_GRAPH_SOURCE,
            format!("`{id}` is not a graph source ({why})"),
        )
        .at(Location::entity(id.to_string()))
        .with_hint("pass the entity that carries the GraphSource; `query {\"with\": [\"GraphSource\"]}` lists them")
    };
    let p = world
        .ecs
        .get::<Procedural>(entity)
        .ok_or_else(|| not_graph("it has no Procedural component".into()))?;
    if p.generator != GENERATOR {
        return Err(not_graph(format!("its generator is `{}`", p.generator)));
    }
    let seed = p.seed;
    let text = match world.get(id, "GraphSource", "params") {
        Ok(Value::Str(s)) => s,
        _ => return Err(not_graph("it has no GraphSource component".into())),
    };
    let store = world.ecs.get_resource::<GraphStore>().ok_or_else(|| {
        Diagnostic::error(codes::NO_GRAPH, "the world has no GraphStore resource")
            .at(Location::entity(id.to_string()))
    })?;
    let doc = store.graph(id).cloned().ok_or_else(|| {
        Diagnostic::error(
            codes::NO_GRAPH,
            format!("no graph is stored for procedural source `{id}`"),
        )
        .at(Location::entity(id.to_string()))
    })?;
    let overrides = parse_overrides(&text);
    Ok(Source {
        entity,
        doc,
        overrides,
        seed,
    })
}

fn store_of(world: &World) -> Option<GraphStore> {
    world.ecs.get_resource::<GraphStore>().cloned()
}

/// `overrides` with `node.param` set to `value` (or removed for `null`).
fn with_override(
    mut overrides: ParamOverrides,
    node: &str,
    param: &str,
    value: &Json,
) -> ParamOverrides {
    let entry = overrides
        .entry(node.to_string())
        .or_insert_with(|| Json::Object(Map::new()));
    if let Some(m) = entry.as_object_mut() {
        if value.is_null() {
            m.remove(param);
        } else {
            m.insert(param.to_string(), value.clone());
        }
    }
    overrides
}

fn regen_json(report: &RegenReport) -> Json {
    json!({
        "sources": report.sources,
        "spawned": report.spawned,
        "updated": report.updated,
        "despawned": report.despawned,
        "released_assets": report.released_assets,
        "orphans": report.orphans.iter().map(|o| json!({
            "source": o.source,
            "target": o.target,
            "component": o.component,
            "field": o.field,
        })).collect::<Vec<_>>(),
    })
}

/// The warnings of a regeneration for `source`: orphaned overrides
/// (`RE4010`), overrides that could not be applied, and the graph's own
/// warnings. The first error, if the source's generator failed.
fn regen_diagnostics(
    world: &World,
    source: &str,
    report: &RegenReport,
) -> (Vec<Diagnostic>, Option<Diagnostic>) {
    let mut warnings: Vec<Diagnostic> = report
        .orphans
        .iter()
        .filter(|o| o.source == source)
        .map(|o| {
            Diagnostic::warning(codes::ORPHANED_OVERRIDE, o.to_string())
                .at(Location::entity(source.to_string()))
                .with_hint("the override stays in the scene file; remove it there if it is no longer wanted")
        })
        .collect();
    let mut error = None;
    for d in &report.diagnostics {
        let ours = matches!(&d.location, Some(Location::Entity { entity }) if entity == source)
            || d.message.starts_with(&format!("{source}:"));
        if d.is_error() {
            if error.is_none() && ours {
                error = Some(d.clone());
            }
        } else {
            warnings.push(d.clone());
        }
    }
    if let Some(r) = world
        .ecs
        .get_resource::<GraphStore>()
        .and_then(|s| s.report(source))
    {
        warnings.extend(r.all_diagnostics().filter(|d| !d.is_error()).cloned());
    }
    (warnings, error)
}

fn set_param<C: EditTarget>(
    target: &mut C,
    a: SetParamArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let src = graph_source(world, &a.source)?;
    let store = store_of(world).expect("graph_source checked the store");
    let registry = store.evaluator().registry();
    let overrides = src.overrides(&a.source)?;
    let current = current_doc(&src.doc, &overrides, registry, &a.source);
    validate_override(&current, registry, &a.node, &a.param, &a.value)
        .map_err(|d| at_source(d, &a.source))?;
    let overrides = with_override(overrides, &a.node, &a.param, &a.value);
    let text = overrides_text(&overrides);
    call.transact(
        target,
        format!("graph.set_param {}.{}", a.node, a.param),
        vec![Op::SetField {
            id: a.source.clone(),
            component: "GraphSource".into(),
            field: "params".into(),
            value: Doc::Str(text.clone()),
        }],
    )?;
    Ok(json!({
        "source": a.source,
        "node": a.node,
        "param": a.param,
        "value": a.value,
        "params": parse_overrides(&text).unwrap_or_default(),
    }))
}

fn regenerate<C: EditTarget>(
    target: &mut C,
    a: RegenerateArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let src = graph_source(target.world(), &a.source)?;
    let seed = a.seed.unwrap_or(src.seed);
    if seed != src.seed {
        call.transact(
            target,
            format!("graph.regenerate {} seed {seed}", a.source),
            vec![Op::SetField {
                id: a.source.clone(),
                component: "Procedural".into(),
                field: "seed".into(),
                value: Doc::Int(seed),
            }],
        )?;
    }
    if call.dry_run() {
        // Regenerate a copy of the world to report what would change.
        let world = target.world();
        let mut copy = world.fork();
        if let Some(store) = store_of(world) {
            copy.ecs.insert_resource(store);
        }
        let _ = copy.set(&a.source, "Procedural", "seed", Value::Int(seed));
        let entity = copy.entity(&a.source).map_err(|e| e.to_diagnostic())?;
        let report = copy.regenerate(entity);
        let (warnings, error) = regen_diagnostics(&copy, &a.source, &report);
        for w in warnings {
            call.push_diagnostic(w);
        }
        if let Some(e) = error {
            return Err(e);
        }
        return Ok(
            json!({ "source": a.source, "seed": seed, "dry_run": true, "regen": regen_json(&report) }),
        );
    }
    let world = target.world();
    let mut report = world.regenerate_changed();
    if !report.sources.contains(&a.source) {
        report.merge(world.regenerate(src.entity));
    }
    if let Some(store) = world.ecs.get_resource::<GraphStore>() {
        for name in &report.released_assets {
            store.release(name);
        }
    }
    let (warnings, error) = regen_diagnostics(world, &a.source, &report);
    for w in warnings {
        call.push_diagnostic(w);
    }
    if let Some(e) = error {
        return Err(e);
    }
    Ok(json!({ "source": a.source, "seed": seed, "regen": regen_json(&report) }))
}

/// The default time budget of `graph.generate`, in milliseconds.
pub const DEFAULT_GENERATE_MS: u64 = 10_000;
/// The default entity budget of `graph.generate`.
pub const DEFAULT_GENERATE_ENTITIES: usize = 100_000;

fn generate<C: EditTarget>(
    target: &mut C,
    a: GenerateArgs,
    _call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let src = graph_source(world, &a.source)?;
    let mut store = store_of(world).expect("graph_source checked the store");
    let registry = store.evaluator().registry().clone();
    let mut overrides = src.overrides(&a.source)?;
    if let Some(params) = &a.params {
        for (node, values) in params {
            let Some(values) = values.as_object() else {
                return Err(Diagnostic::error(
                    re_command::codes::INVALID_ARGUMENTS,
                    format!("`params.{node}` must be an object of parameter values"),
                )
                .at(Location::command("graph.generate")));
            };
            for (param, value) in values {
                let current = current_doc(&src.doc, &overrides, &registry, &a.source);
                validate_override(&current, &registry, node, param, value)
                    .map_err(|d| at_source(d, &a.source))?;
                overrides = with_override(overrides, node, param, value);
            }
        }
    }
    let seed = a.seed.unwrap_or(src.seed);
    store.set_budget(Budget {
        max_ms: Some(a.max_ms.unwrap_or(DEFAULT_GENERATE_MS)),
        max_entities: Some(a.max_entities.unwrap_or(DEFAULT_GENERATE_ENTITIES)),
    });
    let mut copy = world.fork();
    copy.ecs.insert_resource(store);
    let set = |copy: &mut World, c: &str, f: &str, v: Value| {
        copy.set(&a.source, c, f, v).map_err(|e| e.to_diagnostic())
    };
    set(&mut copy, "Procedural", "seed", Value::Int(seed))?;
    set(
        &mut copy,
        "GraphSource",
        "params",
        Value::Str(overrides_text(&overrides)),
    )?;
    let entity = copy.entity(&a.source).map_err(|e| e.to_diagnostic())?;
    let report = copy.regenerate(entity);
    let eval = copy
        .ecs
        .get_resource::<GraphStore>()
        .and_then(|s| s.report(&a.source));
    if let Some(d) = report.diagnostics.iter().find(|d| d.is_error()) {
        let first = eval
            .as_ref()
            .and_then(|r| r.first_error().cloned())
            .unwrap_or_else(|| d.clone());
        return Err(first);
    }
    Ok(json!({
        "source": a.source,
        "seed": seed,
        "params": overrides,
        "entities": eval.as_ref().and_then(|r| r.entities),
        "fragment_hash": eval.as_ref().and_then(|r| r.fragment_hash.clone()),
        "changes": {
            "spawned": report.spawned.len(),
            "updated": report.updated.len(),
            "despawned": report.despawned.len(),
            "orphans": report.orphans.len(),
        },
        "eval": eval,
    }))
}

fn create_error(message: String) -> Diagnostic {
    Diagnostic::error(codes::CREATE_SOURCE, message).at(Location::command("proc.create_source"))
}

/// A project path made of plain names (`[A-Za-z0-9_.-]`, no `..`) ending in
/// `.graph.json`.
fn valid_graph_path(path: &str) -> bool {
    path.ends_with(".graph.json")
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        })
}

fn create_source<C: EditTarget>(
    target: &mut C,
    a: CreateSourceArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let store = world.ecs.get_resource::<GraphStore>().ok_or_else(|| {
        Diagnostic::error(codes::NO_GRAPH, "the world has no GraphStore resource")
            .at(Location::command("proc.create_source"))
    })?;
    let root = store.root().map(Path::to_path_buf).ok_or_else(|| {
        create_error("the world's GraphStore has no project root to write the graph file in".into())
    })?;
    let registry = store.evaluator().registry();
    let def = registry.get(&a.kind).ok_or_else(|| {
        let mut project: Vec<&str> = registry
            .kinds()
            .filter(|k| k.starts_with("project."))
            .collect();
        project.sort_unstable();
        create_error(format!(
            "unknown node kind `{}` (project kinds: {})",
            a.kind,
            if project.is_empty() {
                "none".to_string()
            } else {
                project.join(", ")
            }
        ))
    })?;
    if def.output != crate::PortType::Fragment {
        return Err(create_error(format!(
            "`{}` outputs {}, not a fragment: only a kind that outputs a fragment can be a source",
            a.kind,
            def.output.port_name()
        )));
    }
    if let Some(port) = def.inputs.iter().find(|p| p.required) {
        return Err(create_error(format!(
            "`{}` needs the input `{}`: wire it into a graph file instead",
            a.kind, port.name
        )));
    }
    let params = Json::Object(a.params.clone());
    def.check(&params).map_err(|m| {
        Diagnostic::error(codes::INVALID_PARAMS, format!("`{}`: {m}", a.kind))
            .at(Location::command("proc.create_source"))
    })?;
    let name = a.name.clone().unwrap_or_else(|| {
        a.kind
            .strip_prefix("project.")
            .unwrap_or(&a.kind)
            .to_string()
    });
    let seed = a.seed.unwrap_or(0);
    let doc: GraphDoc = serde_json::from_value(json!({
        "format": "real-engine/graph",
        "version": 1,
        "name": name,
        "seed": seed,
        "nodes": [{ "id": CREATED_NODE, "kind": a.kind, "version": def.version, "params": a.params }],
        "edges": [],
    }))
    .map_err(|e| create_error(format!("cannot build the graph: {e}")))?;
    // A file that already holds exactly this graph (left by an undone
    // `proc.create_source`) is used again rather than written beside.
    let text = doc.to_json();
    let free = |p: &str| match std::fs::read(root.join(p)) {
        Ok(bytes) => bytes == text.as_bytes(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    };
    let graph = match &a.graph {
        Some(g) if !valid_graph_path(g) => {
            return Err(create_error(format!(
                "`{g}` is not a project path ending in `.graph.json` (plain names, no `..`)"
            )));
        }
        Some(g) if !free(g) => {
            return Err(create_error(format!(
                "the graph file `{g}` already exists (with other contents)"
            )));
        }
        Some(g) => g.clone(),
        None => {
            let stem: String =
                a.id.as_deref()
                    .unwrap_or(&name)
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' {
                            c.to_ascii_lowercase()
                        } else {
                            '_'
                        }
                    })
                    .collect();
            let stem = if stem.is_empty() {
                "source".to_string()
            } else {
                stem
            };
            (1..)
                .map(|n| match n {
                    1 => format!("graphs/{stem}.graph.json"),
                    n => format!("graphs/{stem}_{n}.graph.json"),
                })
                .find(|p| free(p))
                .expect("a free name")
        }
    };
    let mut components = re_scene::scene::ComponentsDoc::default();
    components.insert(
        "GraphSource".into(),
        [("graph".to_string(), Doc::Str(graph.clone()))]
            .into_iter()
            .collect(),
    );
    components.insert(
        "Procedural".into(),
        [
            ("generator".to_string(), Doc::Str(GENERATOR.to_string())),
            ("seed".to_string(), Doc::Int(seed)),
        ]
        .into_iter()
        .collect(),
    );
    components.insert("Transform".into(), Default::default());
    let file = root.join(&graph);
    let written = !call.dry_run() && !file.exists();
    if written {
        let write = || -> std::io::Result<()> {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // `create_new`: a file written meanwhile is never replaced.
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)?;
            f.write_all(text.as_bytes())
        };
        write().map_err(|e| create_error(format!("cannot write `{graph}`: {e}")))?;
    }
    let applied = call.transact(
        target,
        format!("proc.create_source {}", a.kind),
        vec![Op::Spawn {
            id: a.id.clone(),
            name: name.clone(),
            parent: a.parent.clone(),
            index: None,
            components,
        }],
    );
    let applied = match applied {
        Ok(ops) => ops,
        Err(d) => {
            if written {
                let _ = std::fs::remove_file(&file);
            }
            return Err(d);
        }
    };
    let id = applied
        .iter()
        .find_map(|op| match op {
            Op::Spawn { id, .. } => id.clone(),
            _ => None,
        })
        .or(a.id)
        .unwrap_or_default();
    if !call.dry_run()
        && let Some(mut store) = target.world().ecs.get_resource_mut::<GraphStore>()
    {
        store.insert(id.clone(), doc);
    }
    Ok(json!({
        "source": id,
        "graph": graph,
        "node": CREATED_NODE,
        "kind": a.kind,
        "seed": seed,
    }))
}

fn report<C: EditTarget>(
    target: &mut C,
    a: ReportArgs,
    _call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    graph_source(world, &a.source)?;
    let report = world
        .ecs
        .get_resource::<GraphStore>()
        .and_then(|s| s.report(&a.source));
    Ok(json!({
        "source": a.source,
        "evaluated": report.is_some(),
        "report": report,
    }))
}

fn migration_report<C: EditTarget>(
    target: &mut C,
    a: MigrationReportArgs,
    _call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    if let Some(id) = &a.source {
        graph_source(world, id)?;
    }
    let root: Option<std::path::PathBuf> = world
        .ecs
        .get_resource::<GraphStore>()
        .and_then(|s| s.root().map(Into::into));
    let mut report = crate::migrate::migration_report(world, root.as_deref());
    if let Some(id) = &a.source {
        report.sources.retain(|s| &s.source == id);
        report.files.retain(|f| f.sources.contains(id));
        report.ok = report
            .sources
            .iter()
            .all(|s| !s.diagnostics.iter().any(Diagnostic::is_error));
    }
    serde_json::to_value(&report).map_err(|e| {
        Diagnostic::error(
            codes::NO_GRAPH,
            format!("cannot write the migration report: {e}"),
        )
    })
}

/// Registers `graph.set_param` and `graph.regenerate` (edit tier) and
/// `graph.generate` and `graph.report` (read tier).
///
/// * `graph.set_param {source, node, param, value}` checks the value against
///   the node kind's parameter schema and stores it in the source's
///   `GraphSource.params` with one ordinary `set_field`, so it is undoable
///   and saved with the scene; the graph file or builder script is never
///   rewritten. The source regenerates when the world next runs
///   `regenerate_changed` (end of frame).
/// * `graph.regenerate {source, seed?}` sets `Procedural.seed` (undoable)
///   and regenerates now, returning the `RegenReport` (the `derived:` assets
///   it released are also dropped from the `GraphStore`) and `RE4010` for
///   orphaned overrides. A dry run regenerates a copy of the world.
/// * `graph.generate {source, seed?, params?, max_ms?, max_entities?}`
///   regenerates a copy of the world (`World::fork`, with the `GraphStore`
///   cloned in) within a time and entity budget, and returns the evaluation
///   report; nothing in the project changes.
/// * `graph.report {source}` returns the source's last evaluation report.
/// * `graph.migration_report {source?}` returns
///   [`migration_report`](crate::migrate::migration_report): what upgrading
///   every node to its kind's current version would change. It changes
///   nothing; `real migrate-graph` writes the files.
pub fn register_commands<C: EditTarget + 'static>(registry: &mut CommandRegistry<C>) {
    registry.register_typed::<CreateSourceArgs, _>(
        "proc.create_source",
        "Create a procedural source from one node kind (a Luau generator): writes a one-node graph file and spawns the source; its parameters are then set with graph.set_param.",
        Tier::Edit,
        true,
        create_source::<C>,
    );
    registry.register_typed::<SetParamArgs, _>(
        "graph.set_param",
        "Set one parameter of one node of a procedural source's graph (stored on the source, undoable).",
        Tier::Edit,
        true,
        set_param::<C>,
    );
    registry.register_typed::<RegenerateArgs, _>(
        "graph.regenerate",
        "Regenerate a procedural source now, optionally with a new seed, and report what changed.",
        Tier::Edit,
        false,
        regenerate::<C>,
    );
    registry.register_typed::<GenerateArgs, _>(
        "graph.generate",
        "Try a seed or parameters on a copy of the world, within a budget, and report the result; changes nothing.",
        Tier::Read,
        true,
        generate::<C>,
    );
    registry.register_typed::<ReportArgs, _>(
        "graph.report",
        "The last evaluation report of a procedural source's graph: per node, cache hit or miss, time, output size and diagnostics.",
        Tier::Read,
        true,
        report::<C>,
    );
    registry.register_typed::<MigrationReportArgs, _>(
        "graph.migration_report",
        "What upgrading every node to its kind's current version would change, per procedural source: nodes, output, keys added and removed, orphaned overrides. Changes nothing; `real migrate-graph` writes it.",
        Tier::Read,
        true,
        migration_report::<C>,
    );
}
