//! Validation tier 2: the static API scan (ADR-0014).
//!
//! [`scan`] parses a file with full_moon (`LuaVersion::luau()`) and records what
//! it uses of the outside world, without running it:
//!
//! * global reads (`print`, `world`, `game`, …), by name;
//! * members read or called on a known receiver, as `receiver.member`
//!   (`world.query`, `game.GetService`, `Players.PlayerAdded`). A known receiver is
//!   a global, or a local holding `game:GetService("X")` (receiver `X`);
//! * `game:GetService("X")` lookups, `Instance.new("Class")` classes and
//!   `Enum.X.Y` items;
//! * datatype constructors: `X.new(...)` and `X.fromY(...)` on a capitalized
//!   global other than `Instance` (`Vector3.new`, `CFrame.fromEulerAnglesXYZ`);
//! * `require` targets: string literals by text, anything computed as dynamic.
//!
//! Members on receivers the scan cannot name (locals, call results) are counted
//! by member name in [`ApiUsage::unknown_receivers`]. Globals the file itself
//! defines (`helper = ...`, `function Util.f()`) are not counted. The scan is
//! approximate by design: scopes are tracked per block, and a local is visible
//! from its declaration to the end of its block. A file that does not compile
//! is not scanned (`RE3080`): Luau's compiler bounds the nesting the scan's
//! parser has to recurse through.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use full_moon::LuaVersion;
use full_moon::ast::{
    self, Call, Expression, FunctionArgs, FunctionCall, Index, Parameter, Prefix, Suffix, Var,
    VarExpression,
};
use full_moon::tokenizer::{TokenReference, TokenType};
use full_moon::visitors::Visitor;
use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};

use crate::codes;

/// What one file (or, merged, a whole corpus) uses.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiUsage {
    /// Global reads by name.
    pub globals: BTreeMap<String, usize>,
    /// Members read or called on known receivers, as `receiver.member`.
    pub by_member: BTreeMap<String, usize>,
    /// Members read or called on receivers the scan cannot name, as `.member`.
    pub unknown_receivers: BTreeMap<String, usize>,
    /// `game:GetService("X")` lookups by service name.
    pub services: BTreeMap<String, usize>,
    /// `Instance.new("Class")` by class name.
    pub instance_new: BTreeMap<String, usize>,
    /// `Enum.X.Y` items.
    pub enums: BTreeMap<String, usize>,
    /// Datatype constructors, as `Type.constructor`.
    pub constructors: BTreeMap<String, usize>,
    /// `require` targets given as string literals.
    pub requires: BTreeMap<String, usize>,
    /// `require` calls with a computed target.
    pub dynamic_requires: usize,
}

fn add(map: &mut BTreeMap<String, usize>, key: impl Into<String>, n: usize) {
    *map.entry(key.into()).or_default() += n;
}

impl ApiUsage {
    /// Adds `other`'s counts to this one.
    pub fn merge(&mut self, other: &ApiUsage) {
        for (mine, theirs) in [
            (&mut self.globals, &other.globals),
            (&mut self.by_member, &other.by_member),
            (&mut self.unknown_receivers, &other.unknown_receivers),
            (&mut self.services, &other.services),
            (&mut self.instance_new, &other.instance_new),
            (&mut self.enums, &other.enums),
            (&mut self.constructors, &other.constructors),
            (&mut self.requires, &other.requires),
        ] {
            for (k, n) in theirs {
                add(mine, k.clone(), *n);
            }
        }
        self.dynamic_requires += other.dynamic_requires;
    }
}

/// Scans the file at `path`. A file that does not parse gives `RE3080`; its
/// location is reported as `name`.
pub fn scan(path: &Path) -> Result<ApiUsage, Diagnostic> {
    let name = path.to_string_lossy().replace('\\', "/");
    let source = std::fs::read(path).map_err(|e| {
        Diagnostic::warning(codes::SCAN_PARSE, format!("cannot read {name}: {e}")).at(
            Location::File {
                file: name.clone(),
                line: None,
                column: None,
            },
        )
    })?;
    scan_source(&String::from_utf8_lossy(&source), &name)
}

/// The stack the parser and the walk get. full_moon recurses once per nesting
/// level, and only files that compile are parsed, so the depth is bounded by
/// Luau's own recursion limit (about 1000 levels). The deepest such file needs
/// well under this in a debug build; the memory is reserved, not committed.
const SCAN_STACK: usize = 512 * 1024 * 1024;

/// Scans Luau source; `name` labels diagnostics. Source that does not compile
/// gives `RE3080` with the compiler's first message.
pub fn scan_source(source: &str, name: &str) -> Result<ApiUsage, Diagnostic> {
    if let Some(d) = re_script::compile_check(source, name).into_iter().next() {
        let mut out = Diagnostic::warning(
            codes::SCAN_PARSE,
            format!(
                "the static scan skips this file: it does not compile: {}",
                d.message
            ),
        );
        out.location = d.location;
        return Err(out);
    }
    let source = source.to_string();
    let file = name.to_string();
    let worker = std::thread::Builder::new()
        .name("re_script_check scan".into())
        .stack_size(SCAN_STACK)
        .spawn(move || scan_on_this_thread(&source, &file));
    let name_err = |message: String| {
        Diagnostic::warning(codes::SCAN_PARSE, message).at(Location::File {
            file: name.to_string(),
            line: None,
            column: None,
        })
    };
    match worker {
        Ok(handle) => handle
            .join()
            .unwrap_or_else(|_| Err(name_err(format!("the scan of {name} crashed")))),
        Err(e) => Err(name_err(format!("cannot start the scan of {name}: {e}"))),
    }
}

fn scan_on_this_thread(source: &str, name: &str) -> Result<ApiUsage, Diagnostic> {
    let result = full_moon::parse_fallible(source, LuaVersion::luau());
    if let Some(err) = result.errors().first() {
        let (start, _) = err.range();
        return Err(Diagnostic::warning(
            codes::SCAN_PARSE,
            format!(
                "the static scan cannot parse this file: {}",
                err.error_message()
            ),
        )
        .at(Location::File {
            file: name.to_string(),
            line: Some(start.line() as u32),
            column: Some(start.character() as u32),
        }));
    }
    let mut walker = Walker::default();
    walker.scopes.push(Scope::default());
    walker.visit_ast(result.ast());
    Ok(walker.finish())
}

#[derive(Default)]
struct Scope {
    locals: HashSet<String>,
    /// Locals holding `game:GetService("X")`, by local name.
    services: HashMap<String, String>,
}

/// A block's address, to match a statement with the block it owns.
fn key(b: &ast::Block) -> usize {
    b as *const ast::Block as usize
}

#[derive(Default)]
struct Walker {
    usage: ApiUsage,
    scopes: Vec<Scope>,
    /// Names a function body or loop declares, by the block they belong to;
    /// added when that block opens.
    pending: HashMap<usize, Vec<String>>,
    /// `repeat` bodies: their scope stays open through the `until` condition.
    repeat_bodies: HashSet<usize>,
    /// Assignment targets, by address, so that `visit_var` skips them.
    targets: HashSet<usize>,
    /// Globals this file assigns or declares functions on.
    defined: HashSet<String>,
    /// Globals assigned `game:GetService("X")`, by name.
    global_services: HashMap<String, String>,
    /// Members and constructors used on global receivers, by receiver; kept
    /// apart until [`Walker::finish`] knows which globals the file defines.
    on_globals: BTreeMap<String, ApiUsage>,
}

fn identifier(t: &TokenReference) -> Option<&str> {
    match t.token().token_type() {
        TokenType::Identifier { identifier } => Some(identifier.as_str()),
        _ => None,
    }
}

fn string_literal(t: &TokenReference) -> Option<&str> {
    match t.token().token_type() {
        TokenType::StringLiteral { literal, .. } => Some(literal.as_str()),
        _ => None,
    }
}

/// The single string-literal argument of a call, if that is what it has.
fn string_arg(args: &FunctionArgs) -> Option<String> {
    match args {
        FunctionArgs::String(t) => string_literal(t).map(str::to_string),
        FunctionArgs::Parentheses { arguments, .. } if arguments.len() == 1 => {
            match arguments.iter().next() {
                Some(Expression::String(t)) => string_literal(t).map(str::to_string),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The member name a suffix reads: `.name`, `["name"]` or `:name(...)`.
fn member(s: &Suffix) -> Option<String> {
    match s {
        Suffix::Index(Index::Dot { name, .. }) => identifier(name).map(str::to_string),
        Suffix::Index(Index::Brackets { expression, .. }) => match &**expression {
            Expression::String(t) => string_literal(t).map(str::to_string),
            _ => None,
        },
        Suffix::Call(Call::MethodCall(m)) => identifier(m.name()).map(str::to_string),
        _ => None,
    }
}

fn call_args(s: &Suffix) -> Option<&FunctionArgs> {
    match s {
        Suffix::Call(Call::AnonymousCall(args)) => Some(args),
        Suffix::Call(Call::MethodCall(m)) => Some(m.args()),
        _ => None,
    }
}

/// `game:GetService("X")` as `X`.
fn get_service<'a>(
    prefix: &Prefix,
    mut suffixes: impl Iterator<Item = &'a Suffix>,
) -> Option<String> {
    let Prefix::Name(n) = prefix else { return None };
    if identifier(n) != Some("game") {
        return None;
    }
    match (suffixes.next(), suffixes.next()) {
        (Some(s @ Suffix::Call(Call::MethodCall(m))), None)
            if identifier(m.name()) == Some("GetService") =>
        {
            call_args(s).and_then(string_arg)
        }
        _ => None,
    }
}

impl Walker {
    fn is_local(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.locals.contains(name))
    }

    fn service_alias(&self, name: &str) -> Option<&str> {
        for s in self.scopes.iter().rev() {
            if let Some(svc) = s.services.get(name) {
                return Some(svc);
            }
            if s.locals.contains(name) {
                return None;
            }
        }
        None
    }

    fn declare(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(name.to_string());
            scope.services.remove(name);
        }
    }

    /// Records a chain `prefix suffix…` (a call or a variable expression).
    fn chain<'a>(&mut self, prefix: &Prefix, suffixes: impl Iterator<Item = &'a Suffix>) {
        let suffixes: Vec<&Suffix> = suffixes.collect();
        let mut global = false;
        let receiver: Option<String> = match prefix {
            Prefix::Name(n) => {
                let Some(name) = identifier(n) else { return };
                if let Some(svc) = self.service_alias(name) {
                    Some(svc.to_string())
                } else if self.is_local(name) {
                    None
                } else if let Some(svc) = self.global_services.get(name) {
                    Some(svc.clone())
                } else {
                    add(&mut self.usage.globals, name, 1);
                    if name == "require" {
                        self.require(suffixes.first().copied());
                        return;
                    }
                    global = true;
                    Some(name.to_string())
                }
            }
            Prefix::Expression(_) => None,
            _ => None,
        };
        let Some(first) = suffixes.first() else {
            return;
        };
        let Some(m) = member(first) else { return };
        let Some(receiver) = receiver else {
            add(&mut self.usage.unknown_receivers, format!(".{m}"), 1);
            return;
        };
        match (receiver.as_str(), m.as_str()) {
            ("game", "GetService") => {
                if let Some(svc) = call_args(first).and_then(string_arg) {
                    add(&mut self.usage.services, svc.clone(), 1);
                    // game:GetService("X").Member reads a member of X.
                    if let Some(next) = suffixes.get(1).and_then(|s| member(s)) {
                        add(&mut self.usage.by_member, format!("{svc}.{next}"), 1);
                    }
                }
            }
            ("Instance", "new") => {
                if let Some(class) = suffixes
                    .get(1)
                    .and_then(|s| call_args(s))
                    .and_then(string_arg)
                {
                    add(&mut self.usage.instance_new, class, 1);
                }
            }
            ("Enum", kind) => {
                if let Some(item) = suffixes.get(1).and_then(|s| member(s)) {
                    add(&mut self.usage.enums, format!("{kind}.{item}"), 1);
                }
            }
            (ty, ctor)
                if ty.starts_with(|c: char| c.is_ascii_uppercase())
                    && (ctor == "new" || ctor.starts_with("from"))
                    && matches!(suffixes.get(1), Some(Suffix::Call(Call::AnonymousCall(_)))) =>
            {
                let to = if global {
                    &mut self
                        .on_globals
                        .entry(receiver.clone())
                        .or_default()
                        .constructors
                } else {
                    &mut self.usage.constructors
                };
                add(to, format!("{ty}.{ctor}"), 1);
            }
            _ => {}
        }
        let to = if global {
            &mut self
                .on_globals
                .entry(receiver.clone())
                .or_default()
                .by_member
        } else {
            &mut self.usage.by_member
        };
        add(to, format!("{receiver}.{m}"), 1);
    }

    /// Drops what the file defined itself from the usage.
    fn finish(mut self) -> ApiUsage {
        let defined = &self.defined;
        self.usage.globals.retain(|g, _| !defined.contains(g));
        for (receiver, used) in &self.on_globals {
            if !defined.contains(receiver) {
                self.usage.merge(used);
            }
        }
        self.usage
    }

    /// An assignment to `name`: a global becomes defined, and a local stops
    /// being a service alias.
    fn assign(&mut self, name: &str) {
        for s in self.scopes.iter_mut().rev() {
            if s.locals.contains(name) {
                s.services.remove(name);
                return;
            }
        }
        self.defined.insert(name.to_string());
    }

    fn require(&mut self, call: Option<&Suffix>) {
        match call.and_then(call_args).map(string_arg) {
            Some(Some(target)) => add(&mut self.usage.requires, target, 1),
            Some(None) => self.usage.dynamic_requires += 1,
            None => {} // `require` read as a value
        }
    }
}

impl Visitor for Walker {
    fn visit_block(&mut self, b: &ast::Block) {
        let mut scope = Scope::default();
        scope
            .locals
            .extend(self.pending.remove(&key(b)).unwrap_or_default());
        self.scopes.push(scope);
    }

    fn visit_block_end(&mut self, b: &ast::Block) {
        if !self.repeat_bodies.contains(&key(b)) {
            self.scopes.pop();
        }
    }

    fn visit_repeat(&mut self, r: &ast::Repeat) {
        self.repeat_bodies.insert(key(r.block()));
    }

    fn visit_repeat_end(&mut self, r: &ast::Repeat) {
        // The body's scope, kept open for the `until` condition.
        self.repeat_bodies.remove(&key(r.block()));
        self.scopes.pop();
    }

    fn visit_function_body(&mut self, body: &ast::FunctionBody) {
        let names = self.pending.entry(key(body.block())).or_default();
        for p in body.parameters() {
            if let Parameter::Name(n) = p
                && let Some(name) = identifier(n)
            {
                names.push(name.to_string());
            }
        }
    }

    fn visit_function_declaration(&mut self, f: &ast::FunctionDeclaration) {
        let name = f.name();
        if name.method_name().is_some() {
            self.pending
                .entry(key(f.body().block()))
                .or_default()
                .push("self".to_string());
        }
        if let Some(first) = name.names().iter().next().and_then(identifier) {
            let first = first.to_string();
            if name.names().len() == 1 && name.method_name().is_none() {
                self.assign(&first);
            } else if !self.is_local(&first) && !self.defined.contains(&first) {
                // `function Util.f()` on a global table reads `Util`.
                add(&mut self.usage.globals, first, 1);
            }
        }
    }

    fn visit_numeric_for(&mut self, f: &ast::NumericFor) {
        if let Some(name) = identifier(f.index_variable()) {
            self.pending
                .entry(key(f.block()))
                .or_default()
                .push(name.to_string());
        }
    }

    fn visit_generic_for(&mut self, f: &ast::GenericFor) {
        let names: Vec<String> = f
            .names()
            .iter()
            .filter_map(|n| identifier(n).map(str::to_string))
            .collect();
        self.pending
            .entry(key(f.block()))
            .or_default()
            .extend(names);
    }

    fn visit_local_function(&mut self, f: &ast::LocalFunction) {
        if let Some(name) = identifier(f.name()) {
            self.declare(name);
        }
    }

    fn visit_local_assignment_end(&mut self, a: &ast::LocalAssignment) {
        // Declared after the right-hand side, so `local x = x` reads the outer x.
        let services: Vec<Option<String>> = a
            .expressions()
            .iter()
            .map(|e| match e {
                Expression::FunctionCall(c) => get_service(c.prefix(), c.suffixes()),
                _ => None,
            })
            .collect();
        for (i, n) in a.names().iter().enumerate() {
            let Some(name) = identifier(n) else { continue };
            self.declare(name);
            if let Some(Some(svc)) = services.get(i)
                && let Some(scope) = self.scopes.last_mut()
            {
                scope.services.insert(name.to_string(), svc.clone());
            }
        }
    }

    fn visit_assignment(&mut self, a: &ast::Assignment) {
        let exprs: Vec<&Expression> = a.expressions().iter().collect();
        for (i, v) in a.variables().iter().enumerate() {
            if let Var::Name(n) = v
                && let Some(name) = identifier(n)
            {
                self.targets.insert(v as *const Var as usize);
                self.assign(name);
                if self.is_local(name) {
                    continue;
                }
                // `Players = game:GetService("Players")` makes a global alias.
                match exprs.get(i) {
                    Some(Expression::FunctionCall(c)) => {
                        match get_service(c.prefix(), c.suffixes()) {
                            Some(svc) => self.global_services.insert(name.to_string(), svc),
                            None => self.global_services.remove(name),
                        };
                    }
                    _ => {
                        self.global_services.remove(name);
                    }
                }
            }
        }
    }

    fn visit_compound_assignment(&mut self, a: &ast::CompoundAssignment) {
        if let Var::Name(n) = a.lhs()
            && let Some(name) = identifier(n)
        {
            self.assign(name);
        }
    }

    fn visit_function_call(&mut self, c: &FunctionCall) {
        self.chain(c.prefix(), c.suffixes());
    }

    fn visit_var_expression(&mut self, v: &VarExpression) {
        self.chain(v.prefix(), v.suffixes());
    }

    fn visit_var(&mut self, v: &Var) {
        if self.targets.remove(&(v as *const Var as usize)) {
            return;
        }
        if let Var::Name(n) = v
            && let Some(name) = identifier(n)
            && !self.is_local(name)
        {
            add(&mut self.usage.globals, name, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(src: &str) -> ApiUsage {
        scan_source(src, "t.luau").unwrap()
    }

    #[test]
    fn records_globals_members_services_and_requires() {
        let u = usage(
            r#"
            local Players = game:GetService("Players")
            local util = require("./util")
            local m = require(script.Parent.Module)
            Players.PlayerAdded:Connect(function(player)
                local part = Instance.new("Part")
                part.Anchored = true
                print(player.Name, Enum.KeyCode.E, workspace.Gravity)
            end)
            local x = world.query("Transform")
            for _, e in x do e:destroy() end
            "#,
        );
        assert_eq!(u.services["Players"], 1);
        assert_eq!(u.by_member["Players.PlayerAdded"], 1);
        assert_eq!(u.by_member["game.GetService"], 1);
        assert_eq!(u.by_member["workspace.Gravity"], 1);
        assert_eq!(u.by_member["world.query"], 1);
        assert_eq!(u.instance_new["Part"], 1);
        assert_eq!(u.enums["KeyCode.E"], 1);
        assert_eq!(u.requires["./util"], 1);
        assert_eq!(u.dynamic_requires, 1);
        assert_eq!(u.unknown_receivers[".Anchored"], 1);
        assert_eq!(u.unknown_receivers[".destroy"], 1);
        assert!(!u.globals.contains_key("player") && !u.globals.contains_key("e"));
        assert!(!u.globals.contains_key("Players"));
        assert_eq!(u.globals["print"], 1);
    }

    #[test]
    fn locals_shadow_globals_only_inside_their_block() {
        let u = usage("do local world = {} world.x() end world.get('a')");
        assert_eq!(u.by_member.get("world.x"), None);
        assert_eq!(u.by_member["world.get"], 1);
    }

    #[test]
    fn parse_errors_are_reported_with_a_line() {
        let d = scan_source("local x = = 1", "bad.luau").unwrap_err();
        assert_eq!(d.code, codes::SCAN_PARSE);
        assert!(matches!(
            d.location,
            Some(Location::File { line: Some(1), .. })
        ));
    }

    #[test]
    fn local_x_equals_x_reads_the_global() {
        let u = usage(
            "local Vector3 = Vector3\nlocal game = game\nlocal v = Vector3.new(1, 2, 3)\ngame:GetService('Players')",
        );
        assert_eq!(u.globals["Vector3"], 1);
        assert_eq!(u.globals["game"], 1);
        // Through the local copies the receivers are unknown.
        assert_eq!(u.unknown_receivers[".new"], 1);
    }

    #[test]
    fn methods_declare_self_and_loops_scope_their_names() {
        let u = usage(
            "local Obj = {}\nfunction Obj:m() return self.x end\n\
             for i = 1, (function() return 2 end)() do print(i) end\n\
             repeat local done = true until done",
        );
        assert!(!u.globals.contains_key("self"));
        assert!(!u.globals.contains_key("i"));
        assert!(!u.globals.contains_key("done"));
        assert!(!u.by_member.contains_key("self.x"));
    }

    #[test]
    fn globals_the_file_defines_are_not_gaps() {
        let u = usage(
            "helper = 1\nfunction util() end\nUtils = {}\nfunction Utils.f() end\n\
             print(helper, util(), Utils.f())",
        );
        for g in ["helper", "util", "Utils"] {
            assert!(!u.globals.contains_key(g), "{g}");
        }
        assert!(!u.by_member.contains_key("Utils.f"));
        assert_eq!(u.globals["print"], 1);
    }

    #[test]
    fn reassigned_service_locals_lose_the_service() {
        let u = usage("local s = game:GetService('A')\ns.P()\ns = {}\ns.Q()");
        assert_eq!(u.by_member["A.P"], 1);
        assert!(!u.by_member.contains_key("A.Q"));
        assert_eq!(u.unknown_receivers[".Q"], 1);
    }

    #[test]
    fn datatype_constructors_are_recorded() {
        let u = usage(
            "local a = Vector3.new(1, 2, 3)\nlocal b = CFrame.fromEulerAnglesXYZ(0, 0, 0)\nlocal p = Instance.new('Part')\nlocal f = Vector3.new",
        );
        assert_eq!(u.constructors["Vector3.new"], 1);
        assert_eq!(u.constructors["CFrame.fromEulerAnglesXYZ"], 1);
        assert!(!u.constructors.contains_key("Instance.new"));
    }

    #[test]
    fn deep_nesting_is_skipped_not_crashed() {
        let deep = format!("local x = {}1{}", "(".repeat(5000), ")".repeat(5000));
        let d = scan_source(&deep, "deep.luau").unwrap_err();
        assert_eq!(d.code, codes::SCAN_PARSE);
        assert!(d.message.contains("does not compile"), "{}", d.message);
        // The deepest nesting Luau compiles fits the scan's stack.
        for src in [
            format!("{}{}", "do ".repeat(1000), "end ".repeat(1000)),
            format!("local x = {}true", "not ".repeat(990)),
            format!("local x = 'a'{}", " .. 'a'".repeat(990)),
            format!("local x = {}1{}", "(".repeat(990), ")".repeat(990)),
        ] {
            assert!(re_script::compile_check(&src, "t").is_empty());
            assert!(scan_source(&src, "t.luau").is_ok());
        }
    }

    #[test]
    fn many_flat_if_expressions_scan() {
        let src = format!(
            "local a = true\n{}",
            "local x = if a then 1 else 2\n".repeat(400)
        );
        assert!(scan_source(&src, "ifs.luau").is_ok());
    }

    #[test]
    fn global_service_aliases_and_defined_names() {
        let u = usage(
            "Players = game:GetService('Players')\nprint(Players.LocalPlayer)\n\
             Signal = {}\nfunction Signal.new() end\nSignal.new()",
        );
        assert_eq!(u.by_member["Players.LocalPlayer"], 1);
        assert!(!u.constructors.contains_key("Signal.new"));
        assert!(!u.by_member.contains_key("Signal.new"));
        let u = usage(
            "local P = game:GetService('Players')\nprint(P.LocalPlayer)\nfunction Players() end",
        );
        assert_eq!(u.by_member["Players.LocalPlayer"], 1);
    }
}
