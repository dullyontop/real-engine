//! The `generator` context: ProcGraph node kinds written in Luau (ADR-0043
//! §4).
//!
//! Each `.luau` file directly in `scripts/generators/` declares one kind:
//!
//! ```lua
//! return proc.node {
//!     kind = "project.rocks",          -- must start with `project.`
//!     version = 1, oldest = 1,         -- `oldest` defaults to 1
//!     doc = "Scatters rocks on a field.",
//!     inputs = { { name = "field", type = "field", required = true, doc = "The ground." } },
//!     output = "fragment",             -- field, points, mesh or fragment
//!     params = { count = proc.param.integer { default = 10, min = 0, max = 1000 } },
//!     uses_seed = true,
//!     migrate = function(from, params) return params end, -- optional
//!     eval = function(ctx, inputs, params) ... end,
//! }
//! ```
//!
//! Files in subfolders are modules the declarations `require`. [`declare`]
//! reads them all, compiles them once, and turns each declaration into a
//! [`NodeDef`] whose implementation hash is the hash of every generator
//! script, so editing any of them regenerates the sources that use a Luau
//! kind. A "Luau generator" is a Luau node whose output is a fragment, run
//! through a one-node graph.
//!
//! **Each evaluation gets a fresh VM** on the worker thread that runs the
//! node, made from the compiled bytecode: no state survives between
//! evaluations, so output cannot depend on their order or thread. The VM
//! has the safe standard libraries without `os` or `math.random`, the
//! `proc`, `quat` and `color` libraries, and `require` for modules under
//! `scripts/generators/`. It has no world, entity, command, task, time or
//! file access: output depends only on parameters, inputs, seed and scripts.
//!
//! **Budget.** The interrupt counts safepoints: past [`Limits::budget`]
//! (20,000,000 by default) the node fails with `RE4101`, at the same place on
//! every machine. The memory limit is [`Limits::memory_mb`]. A wall-clock
//! backstop of 60 s gives `RE4102` (machine-dependent). Neither can be
//! caught by a `pcall`.
//!
//! **`ctx`** has `node` (the node's id), `version` (the version the node was
//! written for), `seed` (an integer below 2^53), `rng` (a [`Pcg32`] on the
//! seed: `rng:next()` in [0, 1), `rng:int(lo, hi)` inclusive,
//! `rng:fork(n)`), and `ctx:log(text)`, which goes into the node's report
//! (`print` does too). Values cross as `marshal` describes.

mod marshal;
mod params;
mod vm;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use mlua::{Function, Lua, Table, UserData, UserDataMethods, Value};
use re_core::{Diagnostic, Location};
use re_noise::Pcg32;
use re_proc::{
    CustomKind, CustomPort, NodeCtx, NodeDef, NodeError, NodeRegistry, Output, PortType, Streaming,
    codes,
};
use serde_json::{Map, Value as Json};

use params::{ParamKind, ParamSpec, Params};
use vm::{Budget, Log, Scripts, Stop};

/// Where Luau node kinds are declared, relative to the project.
pub const GENERATORS_DIR: &str = "scripts/generators";

/// The default safepoint budget of one evaluation (about 3 s).
pub const DEFAULT_BUDGET: u64 = 20_000_000;

/// The default memory limit of one evaluation, in MiB.
pub const DEFAULT_MEMORY_MB: usize = 256;

/// The wall-clock backstop of one evaluation (`RE4102`).
pub const BACKSTOP: std::time::Duration = std::time::Duration::from_secs(60);

/// The version of the generator context, part of every Luau kind's
/// implementation hash: changing what a script can see or how it computes
/// (the portable `math` of ADR-0036, a library) bumps it, so cached outputs
/// are not reused across the change.
const CONTEXT_VERSION: &str = "re_script luau nodes v1";

/// The limits of every Luau node evaluation (`[proc] luau_budget` and
/// `luau_memory_mb` in `real.toml`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Safepoints per evaluation.
    pub budget: u64,
    /// MiB of memory per evaluation (0: no limit).
    pub memory_mb: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            budget: DEFAULT_BUDGET,
            memory_mb: DEFAULT_MEMORY_MB,
        }
    }
}

/// What [`declare`] found.
#[derive(Debug, Default)]
pub struct Declared {
    /// The kinds, in path order.
    pub kinds: Vec<NodeDef>,
    /// `RE4100` for each script that does not declare a usable kind.
    pub diagnostics: Vec<Diagnostic>,
    /// The hash of every script under `scripts/generators/` (all zeros when
    /// there are none): every Luau kind's implementation hash.
    pub implementation: [u8; 32],
}

impl Declared {
    /// `base` with these kinds added: the registry a project's graphs use.
    pub fn registry(&self, base: &NodeRegistry) -> NodeRegistry {
        let mut r = base.clone();
        for k in &self.kinds {
            r.register(k.clone());
        }
        r
    }

    /// Like [`Declared::registry`], for a re-declaration (a hot reload):
    /// while some script has a declaration error, the `project.` kinds of
    /// `previous` that are no longer declared stay, so a half-typed edit
    /// does not take the sources using its kind down with it.
    pub fn registry_keeping(&self, base: &NodeRegistry, previous: &NodeRegistry) -> NodeRegistry {
        let mut r = self.registry(base);
        if self.diagnostics.is_empty() {
            return r;
        }
        for kind in previous.kinds() {
            if kind.starts_with("project.")
                && r.get(kind).is_none()
                && let Some(def) = previous.get(kind)
            {
                r.register(def.clone());
            }
        }
        r
    }
}

fn declaration_error(path: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::LUAU_DECLARATION, message).at(Location::file(path.to_string()))
}

/// Every project path under `scripts/generators/` ending in `.luau`, sorted.
fn generator_files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = format!("{rel}/{name}");
            let Ok(ty) = entry.file_type() else { continue };
            if ty.is_dir() {
                walk(&entry.path(), &path, out);
            } else if ty.is_file() && name.ends_with(".luau") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join(GENERATORS_DIR), GENERATORS_DIR, &mut out);
    out.sort();
    out
}

/// Declares the Luau node kinds of the project at `root`: compiles every
/// script under `scripts/generators/` and runs each file directly in it to
/// read its declaration. Kinds named in `base` are refused (`RE4100`), as
/// are duplicates; the other kinds are returned even when one script fails.
pub fn declare(root: &Path, base: &NodeRegistry, limits: Limits) -> Declared {
    let mut out = Declared::default();
    let files = generator_files(root);
    if files.is_empty() {
        return out;
    }
    let mut scripts = Scripts::new();
    let mut hasher = blake3::Hasher::new();
    hasher.update(CONTEXT_VERSION.as_bytes());
    for path in &files {
        let source = match std::fs::read(root.join(path)) {
            Ok(s) => s,
            Err(e) => {
                out.diagnostics.push(declaration_error(
                    path,
                    format!("{path}: cannot read it: {e}"),
                ));
                continue;
            }
        };
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(source.len() as u64).to_le_bytes());
        hasher.update(&source);
        match crate::compile::compiler().compile(&source) {
            Ok(bytecode) => {
                scripts.insert(path.clone(), Arc::from(bytecode));
            }
            Err(e) => out
                .diagnostics
                .push(declaration_error(path, format!("{path}: {}", short(&e)))),
        }
    }
    out.implementation = *hasher.finalize().as_bytes();
    let scripts = Arc::new(scripts);
    let mut kinds: BTreeMap<String, String> = BTreeMap::new();
    for path in files
        .iter()
        .filter(|p| !p[GENERATORS_DIR.len() + 1..].contains('/'))
    {
        if !scripts.contains_key(path) {
            continue; // did not compile: reported above
        }
        match read_declaration(&scripts, path, limits) {
            Ok(decl) => {
                if base.get(&decl.kind).is_some() {
                    out.diagnostics.push(declaration_error(
                        path,
                        format!("{path}: the kind `{}` is an engine kind", decl.kind),
                    ));
                    continue;
                }
                if let Some(other) = kinds.get(&decl.kind) {
                    out.diagnostics.push(declaration_error(
                        path,
                        format!("{path}: {other} already declares `{}`", decl.kind),
                    ));
                    continue;
                }
                kinds.insert(decl.kind.clone(), path.clone());
                out.kinds.push(node_def(
                    decl,
                    path.clone(),
                    scripts.clone(),
                    limits,
                    out.implementation,
                ));
            }
            Err(m) => out
                .diagnostics
                .push(declaration_error(path, format!("{path}: {m}"))),
        }
    }
    out
}

/// A declaration, read into Rust.
struct Decl {
    kind: String,
    version: u32,
    oldest: u32,
    doc: String,
    inputs: Vec<CustomPort>,
    output: PortType,
    params: Params,
    uses_seed: bool,
    migrates: bool,
}

/// The first line of an mlua error, without its traceback.
fn short(e: &mlua::Error) -> String {
    let text = match e {
        mlua::Error::CallbackError { cause, .. } => return short(cause),
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::RuntimeError(m) => m.clone(),
        other => other.to_string(),
    };
    text.lines().next().unwrap_or_default().to_string()
}

/// Why a run of a script failed: a node error code and its message.
struct Failure {
    code: re_core::Code,
    message: String,
}

impl Failure {
    fn new(code: re_core::Code, message: impl Into<String>) -> Failure {
        Failure {
            code,
            message: message.into(),
        }
    }
}

/// Runs `f` under the budget: a stop is `RE4101` or `RE4102`, a memory
/// error `RE4101`, any other error `RE4008` (`what` prefixes its message).
fn budgeted<T>(
    limits: Limits,
    what: &str,
    f: impl FnOnce() -> mlua::Result<T>,
) -> Result<T, Failure> {
    let budget = Budget::arm(limits.budget, BACKSTOP);
    let result = f();
    match budget.finish() {
        Stop::None => {}
        Stop::Budget => {
            return Err(Failure::new(
                codes::LUAU_BUDGET,
                format!(
                    "{what}: the Luau node ran past its budget of {} safepoints",
                    limits.budget
                ),
            ));
        }
        Stop::Backstop => {
            return Err(Failure::new(
                codes::LUAU_BACKSTOP,
                format!(
                    "{what}: the Luau node ran past the {} s wall-clock backstop (this depends on the machine)",
                    BACKSTOP.as_secs()
                ),
            ));
        }
    }
    result.map_err(|e| {
        let mut cause = &e;
        while let mlua::Error::CallbackError { cause: c, .. } = cause {
            cause = c;
        }
        if matches!(cause, mlua::Error::MemoryError(_)) {
            Failure::new(
                codes::LUAU_BUDGET,
                format!(
                    "{what}: the Luau node ran past its memory limit of {} MiB",
                    limits.memory_mb
                ),
            )
        } else {
            Failure::new(codes::NODE_FAILED, format!("{what}: {}", short(&e)))
        }
    })
}

/// Runs the declaration script `path` in a fresh VM and returns the
/// declaration table with the VM.
fn run_declaration(
    scripts: &Arc<Scripts>,
    path: &str,
    limits: Limits,
    log: Log,
) -> Result<(Lua, Table), Failure> {
    let lua = vm::create(scripts.clone(), limits.memory_mb, log)
        .map_err(|e| Failure::new(codes::NODE_FAILED, short(&e)))?;
    let value = budgeted(limits, path, || {
        vm::load(&lua, scripts, path).and_then(|f| f.call::<Value>(()))
    })?;
    match value {
        Value::Table(t) if vm::marked(&lua, &t, vm::NODE_META) => Ok((lua, t)),
        other => Err(Failure::new(
            codes::NODE_FAILED,
            format!(
                "the script must return proc.node{{...}}, found {}",
                other.type_name()
            ),
        )),
    }
}

fn read_declaration(scripts: &Arc<Scripts>, path: &str, limits: Limits) -> Result<Decl, String> {
    let (lua, t) = run_declaration(scripts, path, limits, Log::default()).map_err(|f| f.message)?;
    let get = |key: &str| t.raw_get::<Value>(key).map_err(|e| short(&e));
    let string = |key: &str, required: bool| -> Result<Option<String>, String> {
        match get(key)? {
            Value::String(s) => Ok(Some(s.to_str().map_err(|e| e.to_string())?.to_string())),
            Value::Nil if !required => Ok(None),
            other => Err(format!(
                "`{key}` must be a string, found {}",
                other.type_name()
            )),
        }
    };
    let whole = |key: &str| -> Result<Option<u32>, String> {
        match get(key)? {
            Value::Nil => Ok(None),
            Value::Integer(i) => u32::try_from(crate::lua_int(i))
                .ok()
                .filter(|v| *v >= 1)
                .map(Some)
                .ok_or_else(|| format!("`{key}` must be a whole number from 1")),
            Value::Number(n) if n.fract() == 0.0 && (1.0..=f64::from(u32::MAX)).contains(&n) => {
                Ok(Some(n as u32))
            }
            other => Err(format!(
                "`{key}` must be a whole number from 1, found {other:?}"
            )),
        }
    };
    for pair in t.pairs::<Value, Value>() {
        let (k, _) = pair.map_err(|e| short(&e))?;
        let known = [
            "kind",
            "version",
            "oldest",
            "doc",
            "inputs",
            "output",
            "params",
            "uses_seed",
            "migrate",
            "eval",
        ];
        match k {
            Value::String(s) if known.contains(&&*s.to_str().map_err(|e| e.to_string())?) => {}
            other => {
                return Err(format!(
                    "unknown declaration field {} (fields: {})",
                    match &other {
                        // Never `tostring` a script value here: a
                        // `__tostring` would run outside the budget.
                        Value::String(s) => format!("`{}`", s.to_string_lossy()),
                        Value::Integer(i) => i.to_string(),
                        Value::Number(n) => n.to_string(),
                        v => format!("of type {}", v.type_name()),
                    },
                    known.join(", ")
                ));
            }
        }
    }
    let kind = string("kind", true)?.unwrap_or_default();
    let rest = kind.strip_prefix("project.").unwrap_or("");
    let valid = !rest.is_empty()
        && rest.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        });
    if !valid {
        return Err(format!(
            "`kind` must start with `project.` and use lowercase letters, digits, `_` and `.` (found `{kind}`)"
        ));
    }
    let version = whole("version")?.ok_or("`version` is required")?;
    let oldest = whole("oldest")?.unwrap_or(1);
    if oldest > version {
        return Err(format!(
            "`oldest` ({oldest}) is above `version` ({version})"
        ));
    }
    let doc = string("doc", true)?.unwrap_or_default();
    let output = match string("output", true)?.unwrap_or_default().as_str() {
        "field" => PortType::Field,
        "points" => PortType::Points,
        "mesh" => PortType::Mesh,
        "fragment" => PortType::Fragment,
        other => {
            return Err(format!(
                "`output` must be field, points, mesh or fragment (found `{other}`)"
            ));
        }
    };
    let inputs = match get("inputs")? {
        Value::Nil => Vec::new(),
        Value::Table(list) => read_inputs(&list)?,
        other => {
            return Err(format!(
                "`inputs` must be a list, found {}",
                other.type_name()
            ));
        }
    };
    let params = match get("params")? {
        Value::Nil => Params::default(),
        Value::Table(p) => read_params(&lua, &p)?,
        other => {
            return Err(format!(
                "`params` must be a table, found {}",
                other.type_name()
            ));
        }
    };
    let uses_seed = match get("uses_seed")? {
        Value::Nil => false,
        Value::Boolean(b) => b,
        other => {
            return Err(format!(
                "`uses_seed` must be true or false, found {}",
                other.type_name()
            ));
        }
    };
    if !matches!(get("eval")?, Value::Function(_)) {
        return Err("`eval` must be a function(ctx, inputs, params)".into());
    }
    let migrates = match get("migrate")? {
        Value::Nil => false,
        Value::Function(_) => true,
        other => {
            return Err(format!(
                "`migrate` must be a function(from, params), found {}",
                other.type_name()
            ));
        }
    };
    Ok(Decl {
        kind,
        version,
        oldest,
        doc,
        inputs,
        output,
        params,
        uses_seed,
        migrates,
    })
}

fn port_type(s: &str) -> Result<PortType, String> {
    match s {
        "field" => Ok(PortType::Field),
        "points" => Ok(PortType::Points),
        "mesh" => Ok(PortType::Mesh),
        "fragment" => Err("fragment inputs are not supported for Luau nodes".into()),
        other => Err(format!(
            "unknown port type `{other}` (field, points or mesh)"
        )),
    }
}

fn read_inputs(list: &Table) -> Result<Vec<CustomPort>, String> {
    let mut out: Vec<CustomPort> = Vec::new();
    for i in 1..=list.raw_len() {
        let at = format!("inputs[{i}]");
        let Value::Table(p) = list.raw_get::<Value>(i).map_err(|e| short(&e))? else {
            return Err(format!(
                "{at} must be a table {{ name, type, required?, doc? }}"
            ));
        };
        let name: String = p
            .raw_get::<Option<String>>("name")
            .map_err(|_| format!("{at}.name must be a string"))?
            .ok_or_else(|| format!("{at}.name is required"))?;
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!(
                "{at}.name `{name}` must be letters, digits and `_`"
            ));
        }
        if out.iter().any(|o| o.name == name) {
            return Err(format!("{at}: the input `{name}` is declared twice"));
        }
        let accepts = match p.raw_get::<Value>("type").map_err(|e| short(&e))? {
            Value::String(s) => vec![port_type(&s.to_str().map_err(|e| e.to_string())?)?],
            Value::Table(types) => {
                let mut v = Vec::new();
                for j in 1..=types.raw_len() {
                    let s: String = types
                        .raw_get(j)
                        .map_err(|_| format!("{at}.type[{j}] must be a string"))?;
                    v.push(port_type(&s)?);
                }
                if v.is_empty() {
                    return Err(format!("{at}.type is empty"));
                }
                v
            }
            _ => return Err(format!("{at}.type must be a port type or a list of them")),
        };
        let required = p
            .raw_get::<Option<bool>>("required")
            .map_err(|_| format!("{at}.required must be true or false"))?
            .unwrap_or(false);
        let doc = p
            .raw_get::<Option<String>>("doc")
            .map_err(|_| format!("{at}.doc must be a string"))?
            .unwrap_or_default();
        out.push(CustomPort {
            name,
            accepts,
            required,
            doc,
        });
    }
    Ok(out)
}

fn read_params(lua: &Lua, t: &Table) -> Result<Params, String> {
    let mut out = Vec::new();
    for pair in t.pairs::<Value, Value>() {
        let (k, v) = pair.map_err(|e| short(&e))?;
        let Value::String(name) = k else {
            return Err("`params` keys must be names".into());
        };
        let name = name.to_str().map_err(|e| e.to_string())?.to_string();
        let at = format!("params.{name}");
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("{at}: parameter names are letters, digits and `_`"));
        }
        let Value::Table(p) = v else {
            return Err(format!("{at} must be proc.param.<type>{{...}}"));
        };
        if !vm::marked(lua, &p, vm::PARAM_META) {
            return Err(format!("{at} must be proc.param.<type>{{...}}"));
        }
        out.push(read_param(lua, name, &p).map_err(|m| format!("{at}: {m}"))?);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Params(out))
}

fn read_param(lua: &Lua, name: String, p: &Table) -> Result<ParamSpec, String> {
    let ty: String = p.raw_get("type").map_err(|e| short(&e))?;
    let num = |key: &str| -> Result<Option<f64>, String> {
        match p.raw_get::<Value>(key).map_err(|e| short(&e))? {
            Value::Nil => Ok(None),
            Value::Integer(i) => Ok(Some(crate::lua_int(i) as f64)),
            Value::Number(n) if n.is_finite() => Ok(Some(n)),
            other => Err(format!(
                "`{key}` must be a number, found {}",
                other.type_name()
            )),
        }
    };
    let int = |key: &str| -> Result<Option<i64>, String> {
        match num(key)? {
            None => Ok(None),
            Some(n) if n.fract() == 0.0 && n.abs() <= 9.007_199_254_740_992e15 => {
                Ok(Some(n as i64))
            }
            Some(n) => Err(format!("`{key}` must be a whole number, found {n}")),
        }
    };
    let kind = match ty.as_str() {
        "number" => ParamKind::Number {
            min: num("min")?,
            max: num("max")?,
        },
        "integer" => ParamKind::Integer {
            min: int("min")?,
            max: int("max")?,
        },
        "bool" => ParamKind::Bool,
        "string" => ParamKind::Str,
        "vec3" => ParamKind::Vec3,
        "enum" => {
            let Value::Table(values) = p.raw_get::<Value>("values").map_err(|e| short(&e))? else {
                return Err("`values` must be a list of strings".into());
            };
            let mut v = Vec::new();
            for i in 1..=values.raw_len() {
                let s: String = values
                    .raw_get(i)
                    .map_err(|_| "`values` must be a list of strings".to_string())?;
                v.push(s);
            }
            if v.is_empty() {
                return Err("`values` is empty".into());
            }
            ParamKind::Enum(v)
        }
        other => return Err(format!("unknown parameter type `{other}`")),
    };
    let default = match p.raw_get::<Value>("default").map_err(|e| short(&e))? {
        Value::Nil => None,
        v => Some(crate::api::json::to_json(lua, &v).map_err(|e| short(&e))?),
    };
    let doc = p
        .raw_get::<Option<String>>("doc")
        .map_err(|_| "`doc` must be a string".to_string())?
        .unwrap_or_default();
    let spec = ParamSpec {
        name,
        kind,
        default,
        doc,
    };
    if let Some(d) = &spec.default {
        spec.check(d).map_err(|m| format!("the default: {m}"))?;
    }
    if let ParamKind::Number {
        min: Some(a),
        max: Some(b),
    } = spec.kind
        && a > b
    {
        return Err(format!("`min` ({a}) is above `max` ({b})"));
    }
    if let ParamKind::Integer {
        min: Some(a),
        max: Some(b),
    } = spec.kind
        && a > b
    {
        return Err(format!("`min` ({a}) is above `max` ({b})"));
    }
    Ok(spec)
}

/// Everything an evaluation needs; shared by the kind's closures.
struct Program {
    scripts: Arc<Scripts>,
    path: String,
    limits: Limits,
    output: PortType,
    params: Params,
}

fn node_def(
    decl: Decl,
    path: String,
    scripts: Arc<Scripts>,
    limits: Limits,
    implementation: [u8; 32],
) -> NodeDef {
    let program = Arc::new(Program {
        scripts,
        path,
        limits,
        output: decl.output,
        params: decl.params.clone(),
    });
    let check = {
        let program = program.clone();
        Arc::new(move |params: &Json| program.params.check(params))
    };
    let eval = {
        let program = program.clone();
        Arc::new(move |ctx: &NodeCtx, params: &Json| evaluate(&program, ctx, params))
    };
    let migrate = decl.migrates.then(|| {
        let program = program.clone();
        let current = decl.version;
        Arc::new(move |from: u32, params: &mut Map<String, Json>| {
            migrate(&program, current, from, params)
        }) as re_proc::MigrateFn
    });
    NodeDef::custom(CustomKind {
        kind: decl.kind,
        version: decl.version,
        oldest: decl.oldest,
        streaming: Streaming::Whole,
        implementation,
        doc: decl.doc,
        inputs: decl.inputs,
        output: decl.output,
        uses_seed: decl.uses_seed,
        params_schema: decl.params.schema(),
        check,
        eval,
        migrate,
    })
}

/// Runs the kind's `migrate(from, params)` once per version step, from
/// `from` up to `current`.
fn migrate(
    program: &Program,
    current: u32,
    from: u32,
    params: &mut Map<String, Json>,
) -> Result<(), (u32, String)> {
    let (lua, decl) = run_declaration(
        &program.scripts,
        &program.path,
        program.limits,
        Log::default(),
    )
    .map_err(|f| (from, f.message))?;
    let f: Function = decl.raw_get("migrate").map_err(|e| (from, short(&e)))?;
    for step in from..current {
        let value = budgeted(program.limits, "migrate", || {
            crate::api::json::from_json(&lua, &Json::Object(params.clone()))
                .and_then(|t| f.call::<Value>((step, t)))
        })
        .map_err(|f| (step, f.message))?;
        match crate::api::json::to_json(&lua, &value).map_err(|e| (step, short(&e)))? {
            Json::Object(m) => *params = m,
            Json::Array(a) if a.is_empty() => params.clear(),
            other => {
                return Err((
                    step,
                    format!("migrate must return the parameters table, returned {other}"),
                ));
            }
        }
    }
    Ok(())
}

/// The `rng` of `ctx`: a [`Pcg32`].
struct Rng(Pcg32);

impl UserData for Rng {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("next", |_, this, ()| Ok(this.0.next_f64()));
        methods.add_method_mut("int", |_, this, (lo, hi): (i64, i64)| {
            let span = i128::from(hi) - i128::from(lo);
            if span < 0 || span >= i128::from(u32::MAX) {
                return Err(mlua::Error::runtime(format!(
                    "rng:int({lo}, {hi}): lo must not be above hi, and the range must fit 32 bits"
                )));
            }
            let span = span as u32;
            Ok(lo + i64::from(this.0.range_u32(0, span.wrapping_add(1).max(1)).min(span)))
        });
        methods.add_method("fork", |_, this, n: i64| Ok(Rng(this.0.fork(n as u64))));
    }
}

fn node_error(code: re_core::Code, message: String) -> NodeError {
    NodeError { code, message }
}

fn evaluate(program: &Program, ctx: &NodeCtx, params: &Json) -> Result<Output, NodeError> {
    let log = Log::default();
    let result = (|| -> Result<Output, NodeError> {
        let failed = |m: String| node_error(codes::NODE_FAILED, m);
        let (lua, decl) =
            run_declaration(&program.scripts, &program.path, program.limits, log.clone())
                .map_err(|f| node_error(f.code, f.message))?;
        let eval: Function = decl.raw_get("eval").map_err(|e| failed(short(&e)))?;
        let lua_ctx = context(&lua, ctx, log.clone()).map_err(|e| failed(short(&e)))?;
        let inputs = lua.create_table().map_err(|e| failed(short(&e)))?;
        for (port, value) in &ctx.inputs {
            let v = marshal::to_lua(&lua, value).map_err(|e| failed(short(&e)))?;
            inputs
                .raw_set(port.as_str(), v)
                .map_err(|e| failed(short(&e)))?;
        }
        let resolved = program.params.resolve(params);
        let lua_params =
            params_to_lua(&lua, &program.params, &resolved).map_err(|e| failed(short(&e)))?;
        let value = budgeted(program.limits, &program.path, || {
            eval.call::<Value>((lua_ctx, inputs, lua_params))
        })
        .map_err(|f| node_error(f.code, f.message))?;
        marshal::from_lua(value, program.output, ctx.node_id, &ctx.key)
            .map_err(|m| node_error(codes::LUAU_OUTPUT, format!("{}: {m}", program.path)))
    })();
    for line in log.borrow().iter() {
        ctx.log(line);
    }
    result
}

fn context(lua: &Lua, ctx: &NodeCtx, log: Log) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("node", ctx.node_id)?;
    t.raw_set("version", ctx.version)?;
    t.raw_set("seed", (ctx.seed >> 11) as f64)?;
    t.raw_set("rng", Rng(Pcg32::from_seed(ctx.seed)))?;
    t.raw_set(
        "log",
        lua.create_function(move |_, (_ctx, text): (Value, mlua::LuaString)| {
            vm::push_log(&log, &text.as_bytes());
            Ok(())
        })?,
    )?;
    t.set_readonly(true);
    Ok(t)
}

fn params_to_lua(lua: &Lua, params: &Params, resolved: &Map<String, Json>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for p in &params.0 {
        let Some(v) = resolved.get(&p.name) else {
            continue;
        };
        let value = match (&p.kind, v) {
            (ParamKind::Vec3, Json::Array(a)) => {
                let c = |i: usize| a.get(i).and_then(Json::as_f64).unwrap_or(0.0) as f32;
                Value::Vector(mlua::Vector::new(c(0), c(1), c(2)))
            }
            (ParamKind::Integer { .. }, v) => {
                match v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)) {
                    Some(i) => Value::Number(i as f64),
                    None => Value::Nil,
                }
            }
            _ => crate::api::json::from_json(lua, v)?,
        };
        t.raw_set(p.name.as_str(), value)?;
    }
    Ok(t)
}
