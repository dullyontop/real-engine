//! Luau-defined tools (ADR-0026 answer 7, docs/M2_PLAN.md WP17): commands
//! written as scripts in the `tools` context.
//!
//! Each file under `scripts/tools/commands/` (subfolders included) is a module
//! that returns one tool:
//!
//! ```luau
//! return {
//!     name = "level.ring",                  -- the command name
//!     summary = "Place N markers in a ring.",
//!     tier = "edit",                        -- "read" or "edit" (the default)
//!     idempotent = false,                   -- optional
//!     input = {                             -- JSON Schema of the arguments
//!         type = "object",
//!         properties = { count = { type = "integer", minimum = 1 } },
//!         required = { "count" },
//!     },
//!     output = { type = "object" },         -- optional: JSON Schema of the result
//!     run = function(args)
//!         for i = 1, args.count do
//!             command.run("entity.spawn", { id = `ring_{i}` })
//!         end
//!         return { placed = args.count }
//!     end,
//! }
//! ```
//!
//! [`register`] loads every file once and registers each tool on the
//! registry, after the engine's own commands, so tools appear in
//! `tools/list`, `describe` and `real do` like any other command, under the
//! same agent policy. A tool can never be `destructive`, and a name that is
//! already taken is refused.
//!
//! A call validates its arguments against `input` (`RE2005`), then runs `run`
//! in a fresh `tools` VM. Inside, `command.run` reaches the built-in scene
//! commands (`entity.*`, `component.*`, `scene.apply`, `query`, ...), capped at
//! the tool's tier and, for an agent's call, checked against the agent policy.
//! Everything those commands change becomes **one** transaction of the
//! outer call: one `undo` reverts the whole tool, a dry run changes nothing
//! and reports every change, and a tool that fails changes nothing. A tool
//! that writes the world directly (`world.spawn`, `entity:set`) instead of
//! through `command.run` is reverted and fails with `RE8016`, because such
//! writes would bypass undo and dry runs.
//!
//! What the tool prints is returned as `RE8017` notes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use re_command::{
    Call, CommandOutcome, CommandRegistry, CommandSpec, EditTarget, History, Op, Origin,
    PolicyCheck, Tier, Transaction,
};
use re_core::{Diagnostic, Location};
use re_scene::World;
use re_script::{ContextKind, NoServices, RuntimeConfig, ScriptHost, ScriptRuntime};
use serde_json::{Map, Value as Json, json};

use crate::codes;
use crate::policy::Policy;

/// Where tool scripts live, relative to the project root.
pub const TOOLS_DIR: &str = "scripts/tools/commands";

/// The CPU time one tool call may use when no agent policy sets one.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(10);

/// A loaded tool definition.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDef {
    /// The command name.
    pub name: String,
    /// The script, relative to the project root.
    pub path: String,
    /// One sentence saying what it does.
    pub summary: String,
    /// `read` or `edit`.
    pub tier: Tier,
    /// Repeating the call has no further effect.
    pub idempotent: bool,
    /// The JSON Schema of the arguments (an object schema).
    pub input: Json,
    /// The JSON Schema of the result, if the tool gives one.
    pub output: Option<Json>,
}

fn invalid(path: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::INVALID_TOOL, message)
        .at(Location::file(path.to_string()))
        .with_hint("see `real explain RE8015` for the shape a tool script returns")
}

/// The `.luau` files under [`TOOLS_DIR`], as project-relative paths in order.
pub fn tool_files(project: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                walk(root, &path, out);
            } else if kind.is_file()
                && path.extension().is_some_and(|e| e == "luau")
                && let Ok(rel) = path.strip_prefix(root)
            {
                let rel: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(rel.join("/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(project, &project.join(TOOLS_DIR), &mut out);
    out.sort();
    out
}

/// The `require` path of a tool script from an `exec` snippet (which resolves
/// as if it were a file directly in `scripts/`).
fn require_path(rel: &str) -> String {
    let inner = rel.strip_prefix("scripts/").unwrap_or(rel);
    format!("./{}", inner.strip_suffix(".luau").unwrap_or(inner))
}

/// `value` as a Luau expression (`null` is `nil`).
pub fn luau_literal(value: &Json) -> String {
    fn string(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 || c == '\u{7f}' => {
                    out.push_str(&format!("\\u{{{:x}}}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
    fn go(v: &Json, out: &mut String) {
        match v {
            Json::Null => out.push_str("nil"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Number(n) => {
                // Keep integers integral and floats exact.
                if let Some(f) = n.as_f64().filter(|_| n.is_f64()) {
                    out.push_str(&format!("{f:?}"));
                } else {
                    out.push_str(&n.to_string());
                }
            }
            Json::String(s) => string(s, out),
            Json::Array(items) => {
                out.push('{');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    go(item, out);
                }
                out.push('}');
            }
            Json::Object(map) => {
                out.push('{');
                for (i, (k, item)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push('[');
                    string(k, out);
                    out.push_str("] = ");
                    go(item, out);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    go(value, &mut out);
    out
}

/// A schema written as a Luau table, made into proper JSON Schema: an empty
/// `required` (which JSON gets as `{}`) becomes `[]`, and the root is an
/// object schema with `properties`.
fn normalize_schema(mut schema: Json, root: bool) -> Json {
    /// `v` is a schema; `properties` maps names to schemas, so a property
    /// called `required` is not a keyword.
    fn fix(v: &mut Json) {
        let Json::Object(map) = v else {
            return;
        };
        for key in ["required", "enum"] {
            if map
                .get(key)
                .is_some_and(|r| r.as_object().is_some_and(Map::is_empty))
            {
                map.insert(key.to_string(), Json::Array(Vec::new()));
            }
        }
        for (key, child) in map.iter_mut() {
            match (key.as_str(), child) {
                ("properties" | "$defs" | "definitions", Json::Object(subs)) => {
                    subs.values_mut().for_each(fix);
                }
                ("items" | "additionalProperties" | "not", sub @ Json::Object(_)) => fix(sub),
                ("anyOf" | "oneOf" | "allOf" | "prefixItems", Json::Array(subs)) => {
                    subs.iter_mut().for_each(fix);
                }
                _ => {}
            }
        }
    }
    fix(&mut schema);
    if root {
        if !schema.is_object() {
            schema = json!({});
        }
        let map = schema.as_object_mut().expect("made an object above");
        map.entry("type").or_insert_with(|| Json::from("object"));
        map.entry("properties")
            .or_insert_with(|| Json::Object(Map::new()));
    }
    schema
}

/// Checks `value` against the subset of JSON Schema that tool inputs use:
/// `type` (one or a list), `properties`, `required`,
/// `additionalProperties: false`, `items`, `enum`, `minimum`, `maximum`,
/// `minLength` and `maxLength`. Other keywords are not checked. Returns the
/// first problem, naming where it is (`count`, `points[2].x`).
pub fn check_value(schema: &Json, value: &Json) -> Result<(), String> {
    fn at(path: &str) -> String {
        if path.is_empty() {
            "the arguments".to_string()
        } else {
            format!("`{path}`")
        }
    }
    fn type_ok(ty: &str, v: &Json) -> bool {
        match ty {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            "number" => v.is_number(),
            "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
            _ => true,
        }
    }
    fn go(schema: &Json, v: &Json, path: &str) -> Result<(), String> {
        let Some(s) = schema.as_object() else {
            return Ok(());
        };
        match s.get("type") {
            Some(Json::String(t)) if !type_ok(t, v) => {
                return Err(format!("{} must be of type {t}, found {v}", at(path)));
            }
            Some(Json::Array(ts)) if !ts.iter().filter_map(Json::as_str).any(|t| type_ok(t, v)) => {
                return Err(format!(
                    "{} must be one of the types {}, found {v}",
                    at(path),
                    Json::Array(ts.clone())
                ));
            }
            _ => {}
        }
        if let Some(Json::Array(options)) = s.get("enum")
            && !options.contains(v)
        {
            return Err(format!(
                "{} must be one of {}, found {v}",
                at(path),
                Json::Array(options.clone())
            ));
        }
        if let Some(n) = v.as_f64() {
            if let Some(min) = s.get("minimum").and_then(Json::as_f64)
                && n < min
            {
                return Err(format!("{} must be at least {min}, found {n}", at(path)));
            }
            if let Some(max) = s.get("maximum").and_then(Json::as_f64)
                && n > max
            {
                return Err(format!("{} must be at most {max}, found {n}", at(path)));
            }
        }
        if let Some(text) = v.as_str() {
            let len = text.chars().count() as u64;
            if let Some(min) = s.get("minLength").and_then(Json::as_u64)
                && len < min
            {
                return Err(format!("{} must have at least {min} characters", at(path)));
            }
            if let Some(max) = s.get("maxLength").and_then(Json::as_u64)
                && len > max
            {
                return Err(format!("{} must have at most {max} characters", at(path)));
            }
        }
        if let Some(obj) = v.as_object() {
            let props = s.get("properties").and_then(Json::as_object);
            if let Some(Json::Array(required)) = s.get("required") {
                for key in required.iter().filter_map(Json::as_str) {
                    if !obj.contains_key(key) {
                        let field = if path.is_empty() {
                            key.to_string()
                        } else {
                            format!("{path}.{key}")
                        };
                        return Err(format!("`{field}` is required"));
                    }
                }
            }
            for (key, item) in obj {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match props.and_then(|p| p.get(key)) {
                    Some(sub) => go(sub, item, &child)?,
                    None if s.get("additionalProperties") == Some(&Json::Bool(false)) => {
                        let known: Vec<&str> = props
                            .map(|p| p.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        return Err(match re_reflect::suggest(key, known.iter().copied()) {
                            Some(sug) => {
                                format!("unknown field `{child}` (did you mean `{sug}`?)")
                            }
                            None => format!("unknown field `{child}`"),
                        });
                    }
                    None => {}
                }
            }
        }
        if let (Some(items), Some(arr)) = (s.get("items"), v.as_array()) {
            for (i, item) in arr.iter().enumerate() {
                go(items, item, &format!("{path}[{}]", i + 1))?;
            }
        }
        Ok(())
    }
    go(schema, value, "")
}

/// What one tool's call needs, shared by its handler.
struct ToolRuntime<C: EditTarget> {
    def: ToolDef,
    project: PathBuf,
    /// The built-in commands `command.run` reaches inside the tool.
    nested: CommandRegistry<C>,
    /// The agent policy, applied to nested calls when an agent called the
    /// tool.
    policy: Option<Policy>,
}

/// The policy for `command.run` inside a tool.
struct NestedPolicy<'a> {
    tool: &'a str,
    tier: Tier,
    agent: Option<&'a Policy>,
}

impl PolicyCheck for NestedPolicy<'_> {
    fn check(&self, spec: &CommandSpec, args: &Json, dry_run: bool) -> Result<(), Diagnostic> {
        if matches!(spec.name.as_str(), "undo" | "redo") {
            return Err(Diagnostic::error(
                codes::TIER_REFUSED,
                format!("the tool {} may not call `{}`", self.tool, spec.name),
            )
            .with_hint("a tool's changes are undone as one step after it returns; it cannot undo inside its own run"));
        }
        if !dry_run && spec.tier > self.tier {
            return Err(Diagnostic::error(
                codes::TIER_REFUSED,
                format!(
                    "the tool {} is `{}` tier and may not run `{}` ({} tier)",
                    self.tool,
                    self.tier.as_str(),
                    spec.name,
                    spec.tier.as_str()
                ),
            )
            .with_hint("declare `tier = \"edit\"` in the tool to change the project"));
        }
        match self.agent {
            Some(p) => p.check(spec, args, dry_run),
            None => Ok(()),
        }
    }
}

/// The script host of one tool call: the target's world, no game services,
/// and nested commands through the built-in registry. It records every
/// operation the nested commands apply.
struct ToolHost<'a, C: EditTarget> {
    target: &'a mut C,
    registry: &'a CommandRegistry<C>,
    policy: &'a NestedPolicy<'a>,
    services: NoServices,
    applied: Vec<Op>,
}

impl<C: EditTarget> ScriptHost for ToolHost<'_, C> {
    fn world(&mut self) -> &mut World {
        self.target.world()
    }
    fn services(&mut self) -> &mut dyn re_types::host::HostServices {
        &mut self.services
    }
    fn run_command(
        &mut self,
        name: &str,
        args: Json,
        dry_run: bool,
        origin: Origin,
    ) -> Option<CommandOutcome> {
        let outcome =
            self.registry
                .dispatch_as(self.target, name, args, dry_run, self.policy, origin);
        let dry = dry_run || outcome.changes.is_empty();
        if outcome.ok && !dry {
            self.applied.extend(outcome.changes.iter().cloned());
        }
        Some(outcome)
    }
}

/// Returns the world to the roots in `snapshot` (from
/// [`crate::session::snapshot_roots`]), outside any history.
fn restore_snapshot(world: &mut World, snapshot: Vec<Op>) {
    let ops = crate::session::Session::restore_roots_ops(world, &snapshot);
    let mut scratch = History::default();
    let tx = Transaction::new("restore after a tool", Origin::User, ops);
    if let Err(e) = scratch.execute(world, tx) {
        eprintln!("re_agent: could not restore the world after a tool: {e}");
    }
}

impl<C: EditTarget + 'static> ToolRuntime<C> {
    fn call(&self, target: &mut C, args: &Json, call: &mut Call) -> Result<Json, Diagnostic> {
        let def = &self.def;
        let args = if args.is_null() {
            Json::Object(Map::new())
        } else {
            args.clone()
        };
        check_value(&def.input, &args).map_err(|why| {
            Diagnostic::error(
                re_command::codes::INVALID_ARGUMENTS,
                format!("invalid arguments for {}: {why}", def.name),
            )
            .at(Location::command(def.name.clone()))
            .with_hint(format!(
                "see the input schema with `command.describe {{\"name\": \"{}\"}}`",
                def.name
            ))
        })?;
        let by_agent = matches!(call.origin(), Origin::Agent(_));
        let agent = if by_agent { self.policy.as_ref() } else { None };
        let budget = agent.map_or(DEFAULT_BUDGET, |p| {
            Duration::from_millis(p.budgets().milliseconds)
        });
        let nested_policy = NestedPolicy {
            tool: &def.name,
            tier: def.tier,
            agent,
        };

        let start_hash = target.world().state_hash();
        let snapshot = crate::session::snapshot_roots(target.world());
        let saved_history = std::mem::take(target.history());

        let config = RuntimeConfig {
            cpu_budget: budget,
            ..RuntimeConfig::new(&self.project)
        };
        let mut runtime = ScriptRuntime::new(config).map_err(|e| e.to_diagnostic())?;
        let source = format!(
            "local tool = require({})\nreturn tool.run({})\n",
            luau_literal(&Json::from(require_path(&def.path))),
            luau_literal(&args)
        );
        let mut host = ToolHost {
            target,
            registry: &self.nested,
            policy: &nested_policy,
            services: NoServices::default(),
            applied: Vec::new(),
        };
        let result = runtime.exec(&mut host, ContextKind::Tools, &def.path, &source);
        let applied = std::mem::take(&mut host.applied);
        let target = host.target;
        for line in runtime.take_output() {
            call.push_diagnostic(
                Diagnostic::note(codes::TOOL_OUTPUT, line.text)
                    .at(Location::file(def.path.clone())),
            );
        }

        // Undo what the nested commands did, so the outer call can apply it
        // again as one transaction (or, for a dry run, only report it).
        let mut undo_error = None;
        loop {
            let mut history = std::mem::take(target.history());
            let step = history.undo(target.world());
            *target.history() = history;
            match step {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    undo_error = Some(e.to_diagnostic());
                    break;
                }
            }
        }
        *target.history() = saved_history;

        if undo_error.is_some() || target.world().state_hash() != start_hash {
            restore_snapshot(target.world(), snapshot);
            if let Some(d) = undo_error {
                return Err(d);
            }
            return Err(Diagnostic::error(
                codes::TOOL_FAILED,
                format!(
                    "the tool {} changed the world directly; the change was reverted",
                    def.name
                ),
            )
            .at(Location::file(def.path.clone()))
            .with_hint("change the project through `command.run` (for example `command.run(\"entity.set\", {...})`), so the change is undoable and dry runs work"));
        }
        let value = match result {
            Ok(v) => v,
            Err(mut diags) => {
                let first = diags.remove(0);
                for d in diags {
                    call.push_diagnostic(d);
                }
                return Err(first);
            }
        };
        if let Some(p) = agent {
            let cap = p.budgets().entities_per_transaction;
            if applied.len() as u64 > cap {
                return Err(Diagnostic::error(
                    codes::OVER_BUDGET,
                    format!(
                        "the tool {} made {} operations; the agent policy allows {cap} per transaction",
                        def.name,
                        applied.len()
                    ),
                )
                .at(Location::command(def.name.clone()))
                .with_hint("call the tool on fewer things at once, or raise `entities_per_transaction` in .real/agent-policy.toml"));
            }
        }
        if !applied.is_empty() {
            call.transact(target, format!("tool {}", def.name), applied)?;
        }
        Ok(value)
    }
}

/// Reads one tool definition by running its module in a scratch `tools` VM.
fn load_def(project: &Path, rel: &str) -> Result<ToolDef, Diagnostic> {
    let mut runtime =
        ScriptRuntime::new(RuntimeConfig::new(project)).map_err(|e| e.to_diagnostic())?;
    let source = format!(
        "local t = require({})\n\
         if type(t) ~= \"table\" then error(\"the script must return a table\") end\n\
         return {{ name = t.name, summary = t.summary, tier = t.tier, idempotent = t.idempotent,\n\
                  input = t.input, output = t.output, run = type(t.run) }}\n",
        luau_literal(&Json::from(require_path(rel)))
    );
    let mut world = World::new();
    let mut services = NoServices::default();
    let mut host = re_script::WorldHost {
        world: &mut world,
        services: &mut services,
    };
    let v = runtime
        .exec(&mut host, ContextKind::Tools, rel, &source)
        .map_err(|mut diags| {
            let first = diags.remove(0);
            invalid(rel, format!("{rel} could not be loaded: {}", first.message))
        })?;
    let name = v["name"]
        .as_str()
        .ok_or_else(|| invalid(rel, format!("{rel} has no `name` string")))?
        .to_string();
    let valid_name = !name.is_empty()
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        });
    if !valid_name {
        return Err(invalid(
            rel,
            format!("{rel}: the name {name:?} must be dotted lowercase words (`level.ring`)"),
        ));
    }
    let summary = v["summary"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid(rel, format!("{rel} ({name}) has no `summary` string")))?
        .to_string();
    let tier = match v["tier"].as_str() {
        None | Some("edit") => Tier::Edit,
        Some("read") => Tier::Read,
        Some("destructive") => {
            return Err(invalid(
                rel,
                format!("{rel} ({name}) asks for the destructive tier; tools are capped at `edit`"),
            ));
        }
        Some(other) => {
            return Err(invalid(
                rel,
                format!("{rel} ({name}) has tier {other:?}; use \"read\" or \"edit\""),
            ));
        }
    };
    if v["run"] != "function" {
        return Err(invalid(
            rel,
            format!("{rel} ({name}) has no `run` function"),
        ));
    }
    let mut input = normalize_schema(v["input"].clone(), true);
    if tier != Tier::Read
        && let Some(props) = input["properties"].as_object_mut()
    {
        props.insert(
            "dry_run".into(),
            json!({
                "type": "boolean",
                "description": "Return the changes the command would make, without making them (needs only the read tier)."
            }),
        );
    }
    let output = match &v["output"] {
        Json::Null => None,
        o => Some(normalize_schema(o.clone(), false)),
    };
    Ok(ToolDef {
        name,
        path: rel.to_string(),
        summary,
        tier,
        idempotent: v["idempotent"].as_bool().unwrap_or(false),
        input,
        output,
    })
}

/// Loads every tool script of `project` and registers each tool on
/// `registry`. Call it after every other command is registered: a tool whose
/// name is taken is refused. Returns the problems (`RE8015`, one per script
/// that was skipped) and the tools registered.
pub fn register<C: EditTarget + 'static>(
    registry: &mut CommandRegistry<C>,
    project: &Path,
) -> (Vec<ToolDef>, Vec<Diagnostic>) {
    let mut problems = Vec::new();
    let mut defs: Vec<ToolDef> = Vec::new();
    for rel in tool_files(project) {
        match load_def(project, &rel) {
            Ok(def) => {
                if registry.spec(&def.name).is_some() {
                    problems.push(invalid(
                        &rel,
                        format!("{rel}: the command name {} is already taken", def.name),
                    ));
                } else if let Some(other) = defs.iter().find(|d| d.name == def.name) {
                    problems.push(invalid(
                        &rel,
                        format!(
                            "{rel}: the command name {} is already used by {}",
                            def.name, other.path
                        ),
                    ));
                } else {
                    defs.push(def);
                }
            }
            Err(d) => problems.push(d),
        }
    }
    let policy = Policy::load(project).ok();
    for def in &defs {
        let mut nested = CommandRegistry::new();
        re_command::register_builtin_commands(&mut nested);
        let tool = Arc::new(ToolRuntime {
            def: def.clone(),
            project: project.to_path_buf(),
            nested,
            policy: policy.clone(),
        });
        let spec = CommandSpec {
            name: def.name.clone(),
            summary: format!("{} (Luau tool {})", def.summary.trim(), def.path),
            tier: def.tier,
            input_schema: def.input.clone(),
            annotations: re_command::CommandAnnotations::for_tier(def.tier, def.idempotent),
            output_schema: def.output.clone(),
        };
        registry.register(
            spec,
            Arc::new(move |target, args, call| tool.call(target, args, call)),
        );
    }
    (defs, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_round_trip_the_json_shapes() {
        let v = json!({ "a": [1, 2.5, "x\"y\n"], "b": null, "c": { "d": true } });
        assert_eq!(
            luau_literal(&v),
            r#"{["a"] = {1, 2.5, "x\"y\n"}, ["b"] = nil, ["c"] = {["d"] = true}}"#
        );
    }

    #[test]
    fn require_paths_are_relative_to_scripts() {
        assert_eq!(
            require_path("scripts/tools/commands/level/ring.luau"),
            "./tools/commands/level/ring"
        );
    }

    #[test]
    fn values_are_checked_against_the_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "minimum": 1 },
                "mode": { "enum": ["a", "b"] },
                "points": { "type": "array", "items": { "type": "number" } }
            },
            "required": ["count"],
            "additionalProperties": false
        });
        assert!(check_value(&schema, &json!({ "count": 3 })).is_ok());
        assert_eq!(
            check_value(&schema, &json!({})).unwrap_err(),
            "`count` is required"
        );
        assert!(
            check_value(&schema, &json!({ "count": 0 }))
                .unwrap_err()
                .contains("at least 1")
        );
        assert!(
            check_value(&schema, &json!({ "count": 1, "mod": "a" }))
                .unwrap_err()
                .contains("did you mean `mode`")
        );
        assert!(
            check_value(&schema, &json!({ "count": 1, "points": [1, "x"] }))
                .unwrap_err()
                .contains("`points[2]`")
        );
        assert!(
            check_value(&schema, &json!({ "count": 1.5 }))
                .unwrap_err()
                .contains("integer")
        );
    }

    #[test]
    fn empty_lists_written_as_luau_tables_are_fixed() {
        let s = normalize_schema(json!({ "required": {} }), true);
        assert_eq!(
            s,
            json!({ "type": "object", "properties": {}, "required": [] })
        );
    }
}
