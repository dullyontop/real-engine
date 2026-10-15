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
//! * `require` targets: string literals by text, anything computed as dynamic.
//!
//! Members on receivers the scan cannot name (locals, call results) are counted
//! by member name in [`ApiUsage::unknown_receivers`]. The scan is approximate by
//! design: scopes are tracked per block, and a local is visible from its
//! declaration to the end of its block.

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

/// The stack the parser and the walk get: full_moon recurses per nesting level.
const SCAN_STACK: usize = 64 * 1024 * 1024;

/// Scans Luau source; `name` labels diagnostics.
pub fn scan_source(source: &str, name: &str) -> Result<ApiUsage, Diagnostic> {
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
    Ok(walker.usage)
}

#[derive(Default)]
struct Scope {
    locals: HashSet<String>,
    /// Locals holding `game:GetService("X")`, by local name.
    services: HashMap<String, String>,
}

#[derive(Default)]
struct Walker {
    usage: ApiUsage,
    scopes: Vec<Scope>,
    /// Names declared by a function body or loop, added when its block opens.
    pending: Vec<String>,
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
        let receiver: Option<String> = match prefix {
            Prefix::Name(n) => {
                let Some(name) = identifier(n) else { return };
                if let Some(svc) = self.service_alias(name) {
                    Some(svc.to_string())
                } else if self.is_local(name) {
                    None
                } else {
                    add(&mut self.usage.globals, name, 1);
                    if name == "require" {
                        self.require(suffixes.first().copied());
                        return;
                    }
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
            _ => {}
        }
        add(&mut self.usage.by_member, format!("{receiver}.{m}"), 1);
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
    fn visit_block(&mut self, _: &ast::Block) {
        let mut scope = Scope::default();
        scope.locals.extend(self.pending.drain(..));
        self.scopes.push(scope);
    }

    fn visit_block_end(&mut self, _: &ast::Block) {
        self.scopes.pop();
    }

    fn visit_function_body(&mut self, body: &ast::FunctionBody) {
        for p in body.parameters() {
            if let Parameter::Name(n) = p
                && let Some(name) = identifier(n)
            {
                self.pending.push(name.to_string());
            }
        }
    }

    fn visit_numeric_for(&mut self, f: &ast::NumericFor) {
        if let Some(name) = identifier(f.index_variable()) {
            self.pending.push(name.to_string());
        }
    }

    fn visit_generic_for(&mut self, f: &ast::GenericFor) {
        for n in f.names() {
            if let Some(name) = identifier(n) {
                self.pending.push(name.to_string());
            }
        }
    }

    fn visit_local_function(&mut self, f: &ast::LocalFunction) {
        if let Some(name) = identifier(f.name()) {
            self.declare(name);
        }
    }

    fn visit_local_assignment(&mut self, a: &ast::LocalAssignment) {
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

    fn visit_function_call(&mut self, c: &FunctionCall) {
        self.chain(c.prefix(), c.suffixes());
    }

    fn visit_var_expression(&mut self, v: &VarExpression) {
        self.chain(v.prefix(), v.suffixes());
    }

    fn visit_var(&mut self, v: &Var) {
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
}
