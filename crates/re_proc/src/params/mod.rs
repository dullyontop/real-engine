//! Graph params (ADR-0061 §2 and §3): a graph's typed parameters, the values
//! a procedural source gives them, and the node params bound to them with
//! `{"$param": "<name>"}`.
//!
//! Binding works on the document, before a node is migrated, checked or
//! keyed: [`bind`] writes each param's value where it is referenced, so a
//! bound node has exactly the key it would have with that value written in.
//! A source's values ([`apply_values`]) take the place of the defaults first.

use std::collections::BTreeSet;

use re_core::Diagnostic;
use re_core::files::ProjectPath;
use re_types::graph::{GraphDoc, NodeDoc, ParamDoc, ParamType};
use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::codes;
use crate::nodes::NodeRegistry;

pub mod commands;

/// The key of a source's graph param values in `GraphSource.params`:
/// `{"$graph": {"height": 6}}`. Node ids cannot start with `$`, so the key
/// never names a node.
pub const GRAPH_KEY: &str = "$graph";

/// The longest param name, in bytes.
pub const MAX_NAME_LEN: usize = 64;

/// The fewest and most points of a `curve` value.
pub const CURVE_POINTS: std::ops::RangeInclusive<usize> = 2..=256;

/// The largest size of an `integer` value: 2^53, up to which every whole
/// number is exact as an f64.
pub const MAX_INTEGER: i64 = 1 << 53;

/// The name of a param type, as written in a declaration's `type`.
pub fn type_name(ty: ParamType) -> &'static str {
    match ty {
        ParamType::Number => "number",
        ParamType::Integer => "integer",
        ParamType::Bool => "bool",
        ParamType::Str => "string",
        ParamType::Enum => "enum",
        ParamType::Vec2 => "vec2",
        ParamType::Vec3 => "vec3",
        ParamType::Color => "color",
        ParamType::File => "file",
        ParamType::Curve => "curve",
    }
}

/// True for a param name: letters, digits and `_`, starting with a letter,
/// at most [`MAX_NAME_LEN`] bytes.
pub fn valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME_LEN
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn numbers(value: &Json, len: impl Fn(usize) -> bool, what: &str) -> Result<Vec<f64>, String> {
    let items = value
        .as_array()
        .filter(|a| len(a.len()))
        .ok_or_else(|| format!("expected {what}, got {value}"))?;
    items
        .iter()
        .map(|x| {
            x.as_f64()
                .ok_or_else(|| format!("expected {what}, got {value}"))
        })
        .collect()
}

fn in_range(decl: &ParamDoc, x: f64) -> Result<(), String> {
    match (decl.min, decl.max) {
        (Some(a), Some(b)) if x < a || x > b => Err(format!("{x} is outside {a} to {b}")),
        (Some(a), _) if x < a => Err(format!("{x} is below the minimum {a}")),
        (_, Some(b)) if x > b => Err(format!("{x} is above the maximum {b}")),
        _ => Ok(()),
    }
}

/// `value` checked against `decl`, in the form a node receives: an
/// `integer` written with a zero fraction (`4.0`) becomes `4`, so the node
/// reads it as a whole number. `Err` says what is wrong, without the
/// param's name.
pub fn check_value(decl: &ParamDoc, value: &Json) -> Result<Json, String> {
    match decl.ty {
        ParamType::Number => {
            let x = value
                .as_f64()
                .ok_or_else(|| format!("expected a number, got {value}"))?;
            in_range(decl, x)?;
            Ok(value.clone())
        }
        ParamType::Integer => {
            let x = if let Some(i) = value.as_i64() {
                (i.unsigned_abs() <= MAX_INTEGER.unsigned_abs()).then_some(i as f64)
            } else if value.is_u64() {
                // Above `i64::MAX`.
                None
            } else {
                value
                    .as_f64()
                    .filter(|x| x.fract() == 0.0 && x.abs() <= MAX_INTEGER as f64)
            }
            .ok_or_else(|| format!("expected a whole number of at most 2^53, got {value}"))?;
            in_range(decl, x)?;
            Ok(Json::from(x as i64))
        }
        ParamType::Bool => match value {
            Json::Bool(_) => Ok(value.clone()),
            _ => Err(format!("expected true or false, got {value}")),
        },
        ParamType::Str => match value {
            Json::String(_) => Ok(value.clone()),
            _ => Err(format!("expected a string, got {value}")),
        },
        ParamType::Enum => match value.as_str() {
            Some(s) if decl.values.iter().any(|v| v == s) => Ok(value.clone()),
            _ => Err(format!(
                "expected one of {}, got {value}",
                decl.values
                    .iter()
                    .map(|v| format!("`{v}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
        ParamType::Vec2 => numbers(value, |n| n == 2, "[x, y]").map(|_| value.clone()),
        ParamType::Vec3 => numbers(value, |n| n == 3, "[x, y, z]").map(|_| value.clone()),
        ParamType::Color => {
            numbers(value, |n| n == 3 || n == 4, "[r, g, b] or [r, g, b, a]").map(|_| value.clone())
        }
        ParamType::File => {
            let path = value
                .as_str()
                .ok_or_else(|| format!("expected a project path or \"\", got {value}"))?;
            if path.is_empty() {
                return Ok(value.clone());
            }
            ProjectPath::new(path).map_err(|e| format!("`{path}` is not a project path: {e}"))?;
            if !decl.extensions.is_empty()
                && !decl.extensions.iter().any(|x| path.ends_with(x.as_str()))
            {
                return Err(format!(
                    "`{path}` does not end with {}",
                    decl.extensions
                        .iter()
                        .map(|x| format!("`{x}`"))
                        .collect::<Vec<_>>()
                        .join(" or ")
                ));
            }
            Ok(value.clone())
        }
        ParamType::Curve => {
            let what = "[[x, y], …] with 2 to 256 points";
            let points = value
                .as_array()
                .filter(|a| CURVE_POINTS.contains(&a.len()))
                .ok_or_else(|| format!("expected {what}, got {value}"))?;
            let mut last: Option<f64> = None;
            for p in points {
                let xy = numbers(p, |n| n == 2, what)?;
                if let Some(l) = last
                    && xy[0] <= l
                {
                    return Err(format!(
                        "a curve's x values must increase strictly, and {} follows {l}",
                        xy[0]
                    ));
                }
                last = Some(xy[0]);
            }
            Ok(value.clone())
        }
    }
}

fn diag(code: re_core::Code, graph: &str, message: String) -> Diagnostic {
    Diagnostic::error(code, format!("graph `{graph}`: {message}"))
}

/// The problems with one declaration, as messages (`RE4200`).
fn declaration_problems(decl: &ParamDoc, kind_graph: bool) -> Vec<String> {
    let name = &decl.name;
    let mut out = Vec::new();
    let ty = type_name(decl.ty);
    let ranged = matches!(decl.ty, ParamType::Number | ParamType::Integer);
    if (decl.min.is_some() || decl.max.is_some()) && !ranged {
        out.push(format!(
            "param `{name}` ({ty}) has `min` or `max`, which only `number` and `integer` params take"
        ));
    }
    if let (Some(a), Some(b)) = (decl.min, decl.max)
        && a > b
    {
        out.push(format!("param `{name}`: `min` {a} is above `max` {b}"));
    }
    if decl.ty == ParamType::Enum {
        if decl.values.is_empty() {
            out.push(format!("param `{name}` (enum) lists no `values`"));
        }
        let distinct: BTreeSet<&String> = decl.values.iter().collect();
        if distinct.len() != decl.values.len() {
            out.push(format!("param `{name}` (enum) lists a value twice"));
        }
    } else if !decl.values.is_empty() {
        out.push(format!(
            "param `{name}` ({ty}) has `values`, which only `enum` params take"
        ));
    }
    if decl.ty == ParamType::File {
        if let Some(bad) = decl
            .extensions
            .iter()
            .find(|x| x.len() < 2 || !x.starts_with('.'))
        {
            out.push(format!(
                "param `{name}`: extension `{bad}` must start with `.`, as `.prefab.json`"
            ));
        }
    } else if !decl.extensions.is_empty() {
        out.push(format!(
            "param `{name}` ({ty}) has `extensions`, which only `file` params take"
        ));
    }
    match &decl.default {
        Some(v) => {
            if let Err(e) = check_value(decl, v) {
                out.push(format!("param `{name}`: its default is refused: {e}"));
            }
        }
        None if !kind_graph => out.push(format!(
            "param `{name}` has no `default`; only a node kind's params may leave it out"
        )),
        None => {}
    }
    out
}

/// True when `value` holds `"$param"` as an object key anywhere, `value`
/// itself included.
fn holds_param_key(value: &Json) -> bool {
    match value {
        Json::Object(m) => m.contains_key(GraphDoc::PARAM_REF) || m.values().any(holds_param_key),
        Json::Array(a) => a.iter().any(holds_param_key),
        _ => false,
    }
}

/// Every problem with `doc`'s param declarations (`RE4200`) and with its
/// references to them: one to an undeclared param (`RE4201`), or a
/// `"$param"` anywhere but as the whole value of a node param (`RE4204`).
pub fn check_doc(doc: &GraphDoc) -> Vec<Diagnostic> {
    let g = doc.name.as_str();
    let mut out = Vec::new();
    // A version 1 document has no params (`GraphDoc::validate`), and its
    // node params are literals, whatever keys they hold.
    if doc.version < 2 {
        return out;
    }
    let kind_graph = doc.kind.is_some();
    for (i, decl) in doc.params.iter().enumerate() {
        if !valid_name(&decl.name) {
            out.push(diag(
                codes::INVALID_GRAPH_PARAM,
                g,
                format!(
                    "param name `{}` is not allowed: letters, digits and `_`, starting with a letter, at most {MAX_NAME_LEN} bytes",
                    decl.name
                ),
            ));
        }
        if doc.params[..i].iter().any(|d| d.name == decl.name) {
            out.push(diag(
                codes::INVALID_GRAPH_PARAM,
                g,
                format!("param `{}` is declared twice", decl.name),
            ));
        }
        out.extend(
            declaration_problems(decl, kind_graph)
                .into_iter()
                .map(|m| diag(codes::INVALID_GRAPH_PARAM, g, m)),
        );
    }
    for node in &doc.nodes {
        for (param, value) in &node.params {
            match GraphDoc::param_ref(value) {
                Some(name) => {
                    if !doc.params.iter().any(|d| d.name == name) {
                        let mut message = format!(
                            "node `{}`'s param `{param}` is bound to `{name}`, which the graph does not declare",
                            node.id
                        );
                        if let Some(s) =
                            re_reflect::suggest(name, doc.params.iter().map(|d| d.name.as_str()))
                        {
                            message.push_str(&format!(" (did you mean `{s}`?)"));
                        }
                        out.push(
                            diag(codes::UNKNOWN_GRAPH_PARAM, g, message)
                                .with_hint("declare it with `graph.declare_param`, or write a value instead"),
                        );
                    }
                }
                None if holds_param_key(value) => out.push(
                    diag(
                        codes::MISPLACED_PARAM_REF,
                        g,
                        format!(
                            "node `{}`'s param `{param}` holds `\"$param\"` inside its value; a reference is a param's whole value: {{\"$param\": \"<name>\"}}",
                            node.id
                        ),
                    )
                    .with_hint("bind the whole param, or declare a graph param of the param's type"),
                ),
                None => {}
            }
        }
    }
    out
}

/// A reference to the graph param `name`, as a node param's whole value:
/// `{"$param": "<name>"}`.
pub fn reference(name: &str) -> Json {
    let mut m = Map::new();
    m.insert(GraphDoc::PARAM_REF.to_string(), Json::from(name));
    Json::Object(m)
}

/// One node param bound to a graph param.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Binding {
    /// The node's id.
    pub node: String,
    /// The node's param.
    pub param: String,
    /// The graph param it is bound to.
    pub graph_param: String,
}

/// The node params of `doc` bound to graph params, in document order.
pub fn bindings(doc: &GraphDoc) -> Vec<Binding> {
    doc.nodes
        .iter()
        .flat_map(|node| {
            node.params.iter().filter_map(|(param, value)| {
                GraphDoc::param_ref(value).map(|name| Binding {
                    node: node.id.clone(),
                    param: param.clone(),
                    graph_param: name.to_string(),
                })
            })
        })
        .collect()
}

/// `doc` with the default of each declared param that `values` sets
/// replaced by that value ([`check_value`]'s form). A value for an
/// undeclared param (`RE4201`) or one its declaration refuses (`RE4202`) is
/// skipped with a warning naming `source`, and the default stays; `null`
/// sets nothing.
pub fn apply_values(
    doc: &GraphDoc,
    values: &Map<String, Json>,
    source: &str,
) -> (GraphDoc, Vec<Diagnostic>) {
    let mut out = doc.clone();
    let mut warnings = Vec::new();
    for (name, value) in values {
        if value.is_null() {
            continue;
        }
        let at = re_core::Location::entity(source.to_string());
        let Some(decl) = out.params.iter_mut().find(|d| &d.name == name) else {
            warnings.push(
                Diagnostic::warning(
                    codes::UNKNOWN_GRAPH_PARAM,
                    format!(
                        "{source}: graph `{}` has no param `{name}`; its value is ignored",
                        doc.name
                    ),
                )
                .at(at)
                .with_hint("clear it with `graph.set_param` and a null value"),
            );
            continue;
        };
        match check_value(decl, value) {
            Ok(v) => decl.default = Some(v),
            Err(e) => warnings.push(
                Diagnostic::warning(
                    codes::GRAPH_PARAM_VALUE,
                    format!(
                        "{source}: the value of graph param `{name}` no longer fits ({e}); the default is used"
                    ),
                )
                .at(at)
                .with_hint("set it again with `graph.set_param`"),
            ),
        }
    }
    (out, warnings)
}

/// The value a reference to `name` stands for: the param's default, in
/// [`check_value`]'s form. `None` for an undeclared param or one without a
/// default.
pub(crate) fn bound_value(doc: &GraphDoc, name: &str) -> Option<Json> {
    let decl = doc.params.iter().find(|d| d.name == name)?;
    let v = decl.default.as_ref()?;
    Some(check_value(decl, v).unwrap_or_else(|_| v.clone()))
}

/// `node`'s params with each reference replaced by its param's default, as
/// [`bind`] writes them; references without a value are left as they are.
/// For checking one node's params where the document is not bound yet.
pub fn bound_params(doc: &GraphDoc, node: &NodeDoc) -> Map<String, Json> {
    let mut params = node.params.clone();
    for value in params.values_mut() {
        if let Some(v) = GraphDoc::param_ref(value).and_then(|name| bound_value(doc, name)) {
            *value = v;
        }
    }
    params
}

/// `doc` ready to evaluate: its declarations and references checked
/// ([`check_doc`]), then every bound node checked against the values its
/// params offer ([`check_bound_nodes`]), then bound ([`bind`]). A document
/// with nothing to bind comes back unchanged, so resolving twice is
/// harmless.
pub fn resolve(doc: &GraphDoc, registry: &NodeRegistry) -> Result<GraphDoc, Vec<Diagnostic>> {
    let mut problems = check_doc(doc);
    if problems.is_empty() {
        problems = check_bound_nodes(doc, registry);
    }
    if problems.is_empty() {
        bind(doc)
    } else {
        Err(problems)
    }
}

/// `doc` with every reference to a declared param replaced by the param's
/// default (call [`apply_values`] first to use a source's values). `Err`
/// (`RE4202`) names a bound param that has no value: a node kind's param
/// without a default, where no source value is given. References to
/// undeclared params are left as they are; [`check_doc`] reports them.
pub fn bind(doc: &GraphDoc) -> Result<GraphDoc, Vec<Diagnostic>> {
    let mut out = doc.clone();
    let mut problems = Vec::new();
    for node in &mut out.nodes {
        for (param, value) in &mut node.params {
            let Some(name) = GraphDoc::param_ref(value) else {
                continue;
            };
            if !doc.params.iter().any(|d| d.name == name) {
                continue;
            }
            match bound_value(doc, name) {
                Some(v) => *value = v,
                None => problems.push(
                    diag(
                        codes::GRAPH_PARAM_VALUE,
                        &doc.name,
                        format!(
                            "graph param `{name}` (bound to `{}.{param}`) has no value: it has no default, and none is given",
                            node.id
                        ),
                    )
                    .with_hint("give the param a default, or a value for this source with `graph.set_param`"),
                ),
            }
        }
    }
    if problems.is_empty() {
        Ok(out)
    } else {
        Err(problems)
    }
}

/// The values a param offers that its bound nodes must all accept, besides
/// its default: `min` and `max`, every `enum` value, and both booleans.
fn offered(decl: &ParamDoc) -> Vec<Json> {
    let mut out: Vec<Json> = match decl.ty {
        ParamType::Number => [decl.min, decl.max]
            .into_iter()
            .flatten()
            .map(Json::from)
            .collect(),
        // The whole numbers nearest the bounds, inside them.
        ParamType::Integer => [decl.min.map(f64::ceil), decl.max.map(f64::floor)]
            .into_iter()
            .flatten()
            .map(|x| Json::from(x as i64))
            .collect(),
        ParamType::Enum => decl.values.iter().map(|v| Json::from(v.as_str())).collect(),
        ParamType::Bool => vec![Json::Bool(false), Json::Bool(true)],
        _ => Vec::new(),
    };
    let default = decl
        .default
        .as_ref()
        .and_then(|v| check_value(decl, v).ok());
    out.retain(|v| Some(v) != default.as_ref() && check_value(decl, v).is_ok());
    out.dedup();
    out
}

fn describe_range(decl: &ParamDoc) -> String {
    match decl.ty {
        ParamType::Enum => format!(
            "allows {}",
            decl.values
                .iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ParamType::Bool => "allows true and false".to_string(),
        _ => match (decl.min, decl.max) {
            (Some(a), Some(b)) => format!("allows {a} to {b}"),
            (Some(a), None) => format!("allows {a} and above"),
            (None, Some(b)) => format!("allows up to {b}"),
            (None, None) => "allows any value".to_string(),
        },
    }
}

/// `RE4203`: every node param bound to a graph param accepts every value
/// the param offers besides its default (`min`, `max`, each `enum` value
/// and both booleans), each written in turn with the node's other bound
/// params at their defaults, migrated to the kind's current version and
/// checked by the kind. So a value the inspector offers never fails at
/// regeneration. The default itself is checked when the bound graph is
/// validated (`RE4005`). Nodes of unknown kinds or versions are skipped;
/// validation reports them. So is a node with a bound param that has no
/// value (a node kind's param without a default): it is checked when an
/// instance gives the value.
pub fn check_bound_nodes(doc: &GraphDoc, registry: &NodeRegistry) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for node in &doc.nodes {
        let Some(def) = registry.get(&node.kind) else {
            continue;
        };
        if !def.supports(node.version) || node.version > def.version {
            continue;
        }
        let mut base = node.params.clone();
        let mut bound = Vec::new();
        for (param, value) in &mut base {
            if let Some(decl) = GraphDoc::param_ref(value)
                .and_then(|name| doc.params.iter().find(|d| d.name == name))
            {
                bound.push((param.clone(), decl));
                if let Some(v) = bound_value(doc, &decl.name) {
                    *value = v;
                }
            }
        }
        if base.values().any(|v| GraphDoc::param_ref(v).is_some()) {
            continue;
        }
        for (param, decl) in bound {
            for v in offered(decl) {
                let mut params = base.clone();
                params.insert(param.clone(), v.clone());
                let params = Json::Object(params);
                let result = def
                    .migrate(node.version, &params)
                    .map_err(|(_, e)| e)
                    .and_then(|p| def.check(&p));
                if let Err(e) = result {
                    out.push(
                        diag(
                            codes::BOUND_PARAM_REFUSED,
                            &doc.name,
                            format!(
                                "graph param `{}` {}, but `{}.{param}` ({}) refuses {v}: {e}",
                                decl.name,
                                describe_range(decl),
                                node.id,
                                node.kind
                            ),
                        )
                        .with_hint("narrow the param's range or values to what the node accepts"),
                    );
                }
            }
        }
    }
    out
}
