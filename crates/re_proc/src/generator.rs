//! The `graph` generator: plugs ProcGraphs into `re_scene`'s procedural
//! sources (ADR-0015, ADR-0017).

use std::sync::Arc;
use std::time::{Duration, Instant};

use re_core::{Diagnostic, Location};
use re_scene::{GenContext, GeneratorInfo, Generators, SceneFragment};
use re_types::graph::GraphDoc;
use serde_json::{Map, Value as Json};

use crate::canon::{canonical_json, doc_hash, sorted};
use crate::codes;
use crate::eval::{EvalOptions, fragment_hash_hex};
use crate::graph::ProcGraph;
use crate::nodes::NodeRegistry;
use crate::store::GraphStore;

/// The generator's name (`Procedural.generator`).
pub const GENERATOR: &str = "graph";

/// Per-node parameter overrides, as stored in `GraphSource.params`:
/// `{"<node id>": {"<param>": <value>}}`.
pub type ParamOverrides = Map<String, Json>;

/// Parses `GraphSource.params` (empty text is no overrides).
pub fn parse_overrides(text: &str) -> Result<ParamOverrides, String> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Json>(text) {
        Ok(Json::Object(m)) if m.values().all(Json::is_object) => Ok(m),
        Ok(_) => Err(
            "`GraphSource.params` must be an object of objects: {\"<node>\": {\"<param>\": value}}"
                .into(),
        ),
        Err(e) => Err(format!("`GraphSource.params` is not valid JSON: {e}")),
    }
}

/// The text stored in `GraphSource.params` for `overrides`: canonical JSON
/// (keys sorted), or empty when there are none.
pub fn overrides_text(overrides: &ParamOverrides) -> String {
    let cleaned: Map<String, Json> = overrides
        .iter()
        .filter(|(_, v)| v.as_object().is_some_and(|m| !m.is_empty()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if cleaned.is_empty() {
        String::new()
    } else {
        canonical_json(&Json::Object(cleaned))
    }
}

/// Why one override cannot apply to a document.
fn check_override(
    doc: &GraphDoc,
    registry: &NodeRegistry,
    node: &str,
    param: &str,
    value: &Json,
) -> Result<(), String> {
    let Some(n) = doc.node(node) else {
        let ids = doc.nodes.iter().map(|n| n.id.as_str());
        let hint = re_reflect::suggest(node, ids)
            .map(|s| format!(" (did you mean `{s}`?)"))
            .unwrap_or_default();
        return Err(format!("the graph has no node `{node}`{hint}"));
    };
    let Some(def) = registry.get(&n.kind) else {
        return Err(format!("node `{node}` has unknown kind `{}`", n.kind));
    };
    let names = def.param_names();
    if !names.iter().any(|p| p == param) {
        let hint = re_reflect::suggest(param, names.iter().map(String::as_str))
            .map(|s| format!(" (did you mean `{s}`?)"))
            .unwrap_or_default();
        return Err(format!(
            "node `{node}` ({}) has no parameter `{param}`{hint}",
            n.kind
        ));
    }
    let mut params = n.params.clone();
    if value.is_null() {
        params.remove(param);
    } else {
        params.insert(param.to_string(), value.clone());
    }
    def.check(&Json::Object(params))
        .map_err(|e| format!("node `{node}` ({}): {e}", n.kind))
}

/// `doc` with `overrides` applied on top of its node parameters. An override
/// naming an unknown node or parameter, or a value the node refuses, is
/// skipped with an `RE4011` warning (the node keeps its document value).
pub fn apply_overrides(
    doc: &GraphDoc,
    overrides: &ParamOverrides,
    registry: &NodeRegistry,
    source_id: &str,
) -> (GraphDoc, Vec<Diagnostic>) {
    let mut out = doc.clone();
    let mut warnings = Vec::new();
    let mut nodes: Vec<&String> = overrides.keys().collect();
    nodes.sort();
    for node in nodes {
        let Some(params) = overrides[node].as_object() else {
            continue;
        };
        let mut names: Vec<&String> = params.keys().collect();
        names.sort();
        for param in names {
            let value = &params[param];
            match check_override(&out, registry, node, param, value) {
                Ok(()) => {
                    if let Some(n) = out.nodes.iter_mut().find(|n| &n.id == node) {
                        if value.is_null() {
                            n.params.remove(param);
                        } else {
                            n.params.insert(param.clone(), sorted(value));
                        }
                    }
                }
                Err(e) => warnings.push(
                    Diagnostic::warning(
                        codes::UNKNOWN_OVERRIDE,
                        format!("{source_id}: override of `{node}.{param}` ignored: {e}"),
                    )
                    .at(Location::entity(source_id.to_string()))
                    .with_hint(
                        "set it again with `graph.set_param`, or clear it with a null value",
                    ),
                ),
            }
        }
    }
    (out, warnings)
}

/// Checks one override strictly (for `graph.set_param` and `graph.generate`):
/// `RE4011` for an unknown node or parameter, `RE4005` for a refused value.
pub(crate) fn validate_override(
    doc: &GraphDoc,
    registry: &NodeRegistry,
    node: &str,
    param: &str,
    value: &Json,
) -> Result<(), Diagnostic> {
    check_override(doc, registry, node, param, value).map_err(|e| {
        let known = doc
            .node(node)
            .and_then(|n| registry.get(&n.kind))
            .is_some_and(|d| d.param_names().iter().any(|p| p == param));
        let code = if known {
            codes::INVALID_PARAMS
        } else {
            codes::UNKNOWN_OVERRIDE
        };
        Diagnostic::error(code, e)
    })
}

fn no_graph(source_id: &str, why: &str) -> Diagnostic {
    Diagnostic::error(
        codes::NO_GRAPH,
        format!("no graph is stored for procedural source `{source_id}` ({why})"),
    )
    .at(Location::entity(source_id.to_string()))
    .with_hint("give its GraphSource a `graph` file or a `builder` script; in Rust, use GraphStore::insert")
}

fn run(ctx: &GenContext) -> Result<SceneFragment, Diagnostic> {
    let source = ctx.source_id();
    let store = ctx
        .resource::<GraphStore>()
        .ok_or_else(|| no_graph(source, "the world has no GraphStore resource"))?;
    let doc = store
        .graph(source)
        .ok_or_else(|| no_graph(source, "the GraphStore has no entry for it"))?;
    let registry = store.evaluator().registry();
    let text: String = ctx.param("params")?;
    let (doc, mut warnings) = match parse_overrides(&text) {
        Ok(o) => apply_overrides(doc, &o, registry, source),
        Err(e) => (
            doc.clone(),
            vec![
                Diagnostic::warning(
                    codes::UNKNOWN_OVERRIDE,
                    format!("{source}: {e}; overrides ignored"),
                )
                .at(Location::entity(source.to_string())),
            ],
        ),
    };
    let graph = match ProcGraph::from_doc(&doc, registry) {
        Ok(g) => g,
        Err(problems) => {
            let mut report = crate::eval::EvalReport {
                graph: doc.name.clone(),
                seed: ctx.seed() as u64,
                diagnostics: problems.clone(),
                ..Default::default()
            };
            report.diagnostics.append(&mut warnings);
            store.set_report(source, report);
            let mut first = problems[0].clone();
            for d in &problems[1..] {
                first = first.with_related(d.clone());
            }
            return Err(first);
        }
    };
    let budget = store.budget();
    let opts = EvalOptions {
        seed: Some(ctx.seed() as u64),
        root: store.root().map(Into::into),
        files: store.files().cloned(),
        deadline: budget
            .max_ms
            .map(|ms| Instant::now() + Duration::from_millis(ms)),
    };
    let evaluation = store.evaluator().evaluate(&graph, &opts);
    let mut report = evaluation.report.clone();
    report.diagnostics.append(&mut warnings);
    let result = evaluation
        .scene_fragment(&ctx.world().registry)
        .and_then(|(fragment, assets)| {
            if let Some(max) = budget.max_entities
                && fragment.len() > max
            {
                return Err(Diagnostic::error(
                    codes::BUDGET_EXCEEDED,
                    format!(
                        "graph `{}` produced {} entities, over its budget of {max}",
                        graph.id,
                        fragment.len()
                    ),
                )
                .with_hint("raise `max_entities`, or produce fewer entities"));
            }
            Ok((fragment, assets))
        });
    match result {
        Ok((fragment, assets)) => {
            report.fragment_hash = Some(fragment_hash_hex(&fragment));
            report.entities = Some(fragment.len());
            store.add_derived(assets);
            store.set_report(source, report);
            Ok(fragment)
        }
        Err(mut d) => {
            if !report.all_diagnostics().any(|x| x == &d) {
                report.diagnostics.push(d.clone());
            }
            store.set_report(source, report);
            if d.location.is_none() {
                d.location = Some(Location::entity(source.to_string()));
            }
            Err(d)
        }
    }
}

fn fingerprint(ctx: &GenContext) -> u64 {
    let Some(doc) = ctx
        .resource::<GraphStore>()
        .and_then(|s| s.graph(ctx.source_id()))
    else {
        return 0;
    };
    let h = doc_hash(doc);
    let mut b = [0u8; 8];
    b.copy_from_slice(&h.as_bytes()[..8]);
    u64::from_le_bytes(b)
}

/// Registers the `graph` generator: its parameter component is
/// `GraphSource`; it evaluates the source's graph from the world's
/// [`GraphStore`] with the source's `Procedural.seed` and its
/// `GraphSource.params` overrides, and returns the graph's output fragment
/// (StableKeys `<node id>/<local key>`). The graph document's hash is part of
/// the source's fingerprint, so storing a changed document regenerates it.
pub fn register_generator(generators: &mut Generators) {
    generators.register(GeneratorInfo {
        name: GENERATOR.to_string(),
        doc: "Evaluates a ProcGraph (a `graph` file or a `builder` script, stored in the GraphStore) with the source's seed and its per-node parameter overrides.".to_string(),
        params_component: "GraphSource".to_string(),
        run: Arc::new(run),
        fingerprint_extra: Some(fingerprint),
    });
}
