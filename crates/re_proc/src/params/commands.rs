//! The graph param commands (ADR-0061 §3): `graph.params` reads a graph's
//! params with the values a source gives them, and `graph.declare_param` and
//! `graph.remove_param` change the declarations in the graph file, through
//! the graph-editing machinery of [`crate::edit`]. [`check_graph_value`]
//! checks the values `graph.set_param` and `graph.generate` give a source,
//! and [`check_edit`] refuses a graph edit that adds a problem.

use re_command::{Call, EditTarget, Op};
use re_core::{Diagnostic, Location};
use re_reflect::{Doc, Value};
use re_types::graph::{GraphDoc, ParamDoc, ParamType};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

use crate::codes;
use crate::edit::{Edit, edit, read_graph, registry, sources_of};
use crate::graph::ProcGraph;
use crate::nodes::NodeRegistry;

use super::{GRAPH_KEY, apply_values, bindings, bound_value, check_value, type_name};

/// Arguments of `graph.params`: one of `source` and `graph`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParamsArgs {
    /// A procedural source: its graph's params, with the values the source
    /// gives them.
    #[serde(default)]
    pub source: Option<String>,
    /// A graph file, relative to the project: its params and their defaults.
    #[serde(default)]
    pub graph: Option<String>,
}

/// Arguments of `graph.declare_param`: a graph param's declaration.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeclareParamArgs {
    /// The graph file, relative to the project.
    pub graph: String,
    /// The param's name: letters, digits and `_`, starting with a letter, at
    /// most 64 bytes. A declaration of that name is replaced where it is;
    /// a new one goes last, which is where the inspector shows it.
    pub name: String,
    /// Its type: `number`, `integer`, `bool`, `string`, `enum`, `vec2`,
    /// `vec3`, `color`, `file` or `curve`.
    #[serde(rename = "type")]
    pub ty: ParamType,
    /// Its default value; required unless the graph is a node kind (has a
    /// `kind` block).
    #[serde(default)]
    pub default: Option<Json>,
    /// The smallest value of a `number` or `integer` param.
    #[serde(default)]
    pub min: Option<f64>,
    /// The largest value of a `number` or `integer` param.
    #[serde(default)]
    pub max: Option<f64>,
    /// The values of an `enum` param.
    #[serde(default)]
    pub values: Vec<String>,
    /// The endings a `file` param accepts, as `[".prefab.json"]`.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// What the param does (the inspector's tooltip).
    #[serde(default)]
    pub doc: String,
}

/// Arguments of `graph.remove_param`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveParamArgs {
    /// The graph file, relative to the project.
    pub graph: String,
    /// The param's name.
    pub name: String,
    /// Write the param's default in place of each node param bound to it.
    /// Without it, a param that is bound is not removed.
    #[serde(default)]
    pub unbind: bool,
}

fn suggestion(name: &str, doc: &GraphDoc) -> String {
    re_reflect::suggest(name, doc.params.iter().map(|d| d.name.as_str()))
        .map(|s| format!(" (did you mean `{s}`?)"))
        .unwrap_or_default()
}

fn unknown_param(doc: &GraphDoc, name: &str) -> Diagnostic {
    let message = if doc.params.is_empty() {
        format!(
            "graph `{}` declares no params, so it has no param `{name}`",
            doc.name
        )
    } else {
        format!(
            "graph `{}` has no param `{name}`{}",
            doc.name,
            suggestion(name, doc)
        )
    };
    Diagnostic::error(codes::UNKNOWN_GRAPH_PARAM, message)
        .with_hint("`graph.params` lists a graph's params; `graph.declare_param` adds one")
}

/// `value` for the graph param `name` of `doc`, in the form a source stores
/// it ([`check_value`]'s), for `graph.set_param` and `graph.generate`.
/// `null` (no value: the default applies) is taken for any name, so a value
/// the graph no longer declares can be cleared. `RE4201` for a param the
/// graph does not declare, `RE4202` for a value its declaration refuses.
pub(crate) fn check_graph_value(
    doc: &GraphDoc,
    name: &str,
    value: &Json,
) -> Result<Json, Diagnostic> {
    if value.is_null() {
        return Ok(Json::Null);
    }
    let decl = doc
        .params
        .iter()
        .find(|d| d.name == name)
        .ok_or_else(|| unknown_param(doc, name))?;
    check_value(decl, value).map_err(|e| {
        Diagnostic::error(
            codes::GRAPH_PARAM_VALUE,
            format!("graph param `{name}` ({}): {e}", type_name(decl.ty)),
        )
        .with_hint("`graph.params` shows each param's type and range")
    })
}

/// Refuses an edit of the graph file `path` that adds a problem: one that
/// building the edited graph ([`ProcGraph::from_doc`]) reports and building
/// it before did not, such as a node param bound to a graph param that does
/// not take one of the values the param offers (`RE4203`), or a default the
/// node refuses (`RE4005`). A node kind's param without a default gets its
/// value from each instance, so its missing value (`RE4202`) is not a
/// problem of the kind's file.
pub(crate) fn check_edit(
    path: &str,
    before: &GraphDoc,
    after: &GraphDoc,
    registry: &NodeRegistry,
) -> Result<(), Diagnostic> {
    let problems = |doc: &GraphDoc| {
        let mut out = ProcGraph::from_doc(doc, registry).err().unwrap_or_default();
        if doc.kind.is_some() {
            out.retain(|d| d.code != codes::GRAPH_PARAM_VALUE);
        }
        out
    };
    let old = problems(before);
    let mut added: Vec<Diagnostic> = problems(after)
        .into_iter()
        .filter(|d| {
            !old.iter()
                .any(|o| o.code == d.code && o.message == d.message)
        })
        .collect();
    if added.is_empty() {
        return Ok(());
    }
    let mut first = added.remove(0);
    for d in added {
        first = first.with_related(d);
    }
    if first.location.is_none() {
        first = first.at(Location::file(path.to_string()));
    }
    Err(first)
}

/// The `set_doc` operations that turn the open document `path`, which holds
/// `before`, into `after`: one per top-level field that changes, or one that
/// replaces the whole document when a field appears or goes, so the fields
/// keep the order the format writes them in.
pub(crate) fn doc_ops(path: &str, before: &GraphDoc, after: &GraphDoc) -> Vec<Op> {
    let as_map = |doc: &GraphDoc| match serde_json::to_value(doc) {
        Ok(Json::Object(m)) => m,
        _ => Map::new(),
    };
    let (old, new) = (as_map(before), as_map(after));
    if !old.keys().eq(new.keys()) {
        return vec![Op::SetDoc {
            doc: path.to_string(),
            pointer: String::new(),
            value: Some(Json::Object(new)),
        }];
    }
    new.into_iter()
        .filter(|(k, v)| old.get(k) != Some(v))
        .map(|(k, v)| Op::SetDoc {
            doc: path.to_string(),
            pointer: format!("/{}", crate::edit::token(&k)),
            value: Some(v),
        })
        .collect()
}

/// `graph.params { source }` or `graph.params { graph }`: each param in
/// declaration order, with its declaration, the source's value (`value`,
/// when it sets one), the value in use (`in_use`: that value when the
/// declaration still takes it, else the default) and the node params bound
/// to it (`bound`). A source's value the graph no longer takes is a warning
/// (`RE4201`, `RE4202`), as at regeneration.
pub(crate) fn params<C: EditTarget>(
    target: &mut C,
    a: ParamsArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let (doc, file, values) = match (&a.source, &a.graph) {
        (Some(id), None) => {
            let src = crate::commands::graph_source(world, id)?;
            let overrides = src.overrides(id)?;
            let values = overrides
                .get(GRAPH_KEY)
                .and_then(Json::as_object)
                .cloned()
                .unwrap_or_default();
            let file = match world.get(id, "GraphSource", "graph") {
                Ok(Value::Str(g)) if !g.is_empty() => re_scene::doc_path(&g).ok(),
                _ => None,
            };
            (src.doc, file, values)
        }
        (None, Some(graph)) => {
            let (path, doc) = read_graph(world, graph)?;
            (doc, Some(path), Map::new())
        }
        _ => {
            return Err(Diagnostic::error(
                re_command::codes::INVALID_ARGUMENTS,
                "pass one of `source` (a procedural source) and `graph` (a graph file)",
            )
            .at(Location::command("graph.params")));
        }
    };
    let (in_use, warnings) = apply_values(&doc, &values, a.source.as_deref().unwrap_or(""));
    for w in warnings {
        call.push_diagnostic(w);
    }
    let bound = bindings(&doc);
    let params: Vec<Json> = doc
        .params
        .iter()
        .zip(&in_use.params)
        .map(|(decl, used)| {
            let mut entry = match serde_json::to_value(decl) {
                Ok(Json::Object(m)) => m,
                _ => Map::new(),
            };
            if let Some(v) = values.get(&decl.name).filter(|v| !v.is_null()) {
                entry.insert("value".into(), v.clone());
            }
            let value = used
                .default
                .as_ref()
                .map(|v| check_value(used, v).unwrap_or_else(|_| v.clone()));
            entry.insert("in_use".into(), value.unwrap_or(Json::Null));
            entry.insert(
                "bound".into(),
                bound
                    .iter()
                    .filter(|b| b.graph_param == decl.name)
                    .map(|b| json!({ "node": b.node, "param": b.param }))
                    .collect(),
            );
            Json::Object(entry)
        })
        .collect();
    Ok(json!({
        "graph": doc.name,
        "file": file,
        "source": a.source,
        "version": doc.version,
        "params": params,
    }))
}

/// `graph.declare_param`: adds a declaration to the graph file, or replaces
/// the one of that name where it is. The file becomes version 2 in the same
/// edit when it was version 1. Refused when the declaration is invalid
/// (`RE4200`), or when a node param bound to it does not take its default
/// (`RE4005`) or a value it offers (`RE4203`).
pub(crate) fn declare_param<C: EditTarget>(
    target: &mut C,
    a: DeclareParamArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let graph = a.graph.clone();
    edit(target, call, &graph, |world, path, doc| {
        let evaluator = registry(world);
        let decl = ParamDoc {
            name: a.name.clone(),
            ty: a.ty,
            default: a.default.clone(),
            min: a.min,
            max: a.max,
            values: a.values.clone(),
            extensions: a.extensions.clone(),
            doc: a.doc.clone(),
        };
        let mut after = doc.clone();
        let replaced = match after.params.iter().position(|d| d.name == decl.name) {
            Some(i) => Some(std::mem::replace(&mut after.params[i], decl.clone())),
            None => {
                after.params.push(decl.clone());
                None
            }
        };
        after.raise_version();
        let data = json!({
            "graph": path,
            "param": decl,
            "replaced": replaced.is_some(),
            "version": after.version,
        });
        if replaced.as_ref() == Some(&decl) {
            return Ok(Edit {
                label: String::new(),
                ops: Vec::new(),
                data,
            });
        }
        check_edit(path, doc, &after, evaluator.registry())?;
        Ok(Edit {
            label: format!("graph.declare_param {}", a.name),
            ops: doc_ops(path, doc, &after),
            data,
        })
    })
}

/// `graph.remove_param`: removes a declaration from the graph file, and the
/// sources' values for it (so a param declared later with the name does not
/// inherit them). A param a node param is bound to is refused (`RE4201`:
/// the reference would name an undeclared param) unless `unbind`, which
/// writes the param's default in place of each reference in the same edit.
/// The file stays at its version.
pub(crate) fn remove_param<C: EditTarget>(
    target: &mut C,
    a: RemoveParamArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let graph = a.graph.clone();
    edit(target, call, &graph, |world, path, doc| {
        let at = || Location::file(path.to_string());
        let Some(index) = doc.params.iter().position(|d| d.name == a.name) else {
            return Err(unknown_param(doc, &a.name).at(at()));
        };
        let bound: Vec<_> = bindings(doc)
            .into_iter()
            .filter(|b| b.graph_param == a.name)
            .collect();
        let names = || {
            bound
                .iter()
                .map(|b| format!("`{}.{}`", b.node, b.param))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut after = doc.clone();
        after.params.remove(index);
        if !bound.is_empty() {
            if !a.unbind {
                return Err(Diagnostic::error(
                    codes::UNKNOWN_GRAPH_PARAM,
                    format!(
                        "graph param `{}` is bound to {}; removing it would leave them naming a param the graph does not declare",
                        a.name,
                        names()
                    ),
                )
                .at(at())
                .with_hint("pass `unbind: true` to write its default in their place"));
            }
            let Some(value) = bound_value(doc, &a.name) else {
                return Err(Diagnostic::error(
                    codes::GRAPH_PARAM_VALUE,
                    format!(
                        "graph param `{}` has no default to write in place of {}",
                        a.name,
                        names()
                    ),
                )
                .at(at())
                .with_hint("set those node params with `graph.set_default` first"));
            };
            for b in &bound {
                if let Some(node) = after.nodes.iter_mut().find(|n| n.id == b.node) {
                    node.params.insert(b.param.clone(), value.clone());
                }
            }
        }
        check_edit(path, doc, &after, registry(world).registry())?;
        let mut ops = doc_ops(path, doc, &after);
        let mut values_removed = Vec::new();
        for (source, _) in sources_of(world, path) {
            let text = match world.get(&source, "GraphSource", "params") {
                Ok(Value::Str(text)) => text,
                _ => String::new(),
            };
            // Text that does not parse is an error, not "no values": the
            // source would keep a stale value of the removed param.
            let mut overrides = crate::generator::parse_overrides(&text).map_err(|e| {
                Diagnostic::error(codes::UNKNOWN_OVERRIDE, format!("{source}: {e}"))
                    .at(Location::entity(source.clone()))
                    .with_hint("fix or clear `GraphSource.params` with `set_field` first")
            })?;
            let Some(value) = overrides
                .get_mut(GRAPH_KEY)
                .and_then(Json::as_object_mut)
                .and_then(|values| values.remove(&a.name))
            else {
                continue;
            };
            values_removed.push(json!({ "source": source, "value": value }));
            ops.push(Op::SetField {
                id: source.clone(),
                component: "GraphSource".into(),
                field: "params".into(),
                value: Doc::Str(crate::generator::overrides_text(&overrides)),
            });
        }
        Ok(Edit {
            label: format!("graph.remove_param {}", a.name),
            ops,
            data: json!({
                "graph": path,
                "param": a.name,
                "unbound": bound
                    .iter()
                    .map(|b| json!({ "node": b.node, "param": b.param }))
                    .collect::<Vec<_>>(),
                "values_removed": values_removed,
            }),
        })
    })
}
