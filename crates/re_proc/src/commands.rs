//! The `graph.*` commands (ARCHITECTURE §6): `graph.set_param` and
//! `graph.regenerate` (edit), `graph.generate` and `graph.report` (read).

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
    GENERATOR, ParamOverrides, overrides_text, parse_overrides, validate_override,
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

/// What a command needs about its graph source.
struct Source {
    entity: Entity,
    doc: GraphDoc,
    overrides: ParamOverrides,
    seed: i64,
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
    let overrides = parse_overrides(&text).unwrap_or_default();
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
    validate_override(
        &src.doc,
        store.evaluator().registry(),
        &a.node,
        &a.param,
        &a.value,
    )
    .map_err(|d| at_source(d, &a.source))?;
    let overrides = with_override(src.overrides, &a.node, &a.param, &a.value);
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
    let mut overrides = src.overrides.clone();
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
                validate_override(&src.doc, &registry, node, param, value)
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
pub fn register_commands<C: EditTarget + 'static>(registry: &mut CommandRegistry<C>) {
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
}
