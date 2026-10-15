//! The Studio command-bar loader reader: a Luau file that builds instances and
//! assigns each script's `Source`, read with full_moon and interpreted
//! statically. No Luau runs.
//!
//! The reader walks the chunk's statements and understands these shapes; any
//! other statement is reported in [`ImportReport::unrecognised`] and skipped:
//!
//! * `local x = <constant>`: strings (quoted or long brackets), numbers,
//!   booleans, `nil` and tables of constants.
//! * `local x = game:GetService("S")`, `game.S`, `workspace`: a service.
//! * `local x = Instance.new("C")`: a new instance named `C`.
//! * `x.Name = "n"`, `x.Parent = y`, `x.Source = "..."`, `x:AddTag("t")`,
//!   `CollectionService:AddTag(x, "t")`, `x:Destroy()`.
//! * `do ... end`, and `pcall(function() ... end)` (also as `local ok, err =`).
//! * Three helper functions, recognised by their body, not their name:
//!   * `make(class, name, parent)`: `Instance.new(class)`, set `Name`, set
//!     `Parent`, return it;
//!   * `resolve({"Service", "A", "B"})`: the service, then each child by
//!     name, created as a container when missing;
//!   * `clear(parent, name)`: destroy every child of `parent` called `name`.
//!     The import replaces those children: what an earlier import created
//!     there and the input no longer declares is removed.
//! * A script table: a top-level local holding a table whose every item is
//!   `{ {"Service", ..., "Name"}, "Script" | "LocalScript" | "ModuleScript",
//!   source }`. Each entry is applied the way such loaders do: the service,
//!   each container by name (created as a `Folder` when missing), then the
//!   script with that name and class (adopted when it exists, else created).
//!   Stale scripts an earlier import created are removed unless the chunk
//!   says `local REMOVE_OLD = false`; a `local TAG = "t"` names the tag put on
//!   what the table creates. The loop that applies the table in Studio is
//!   never run: the reader implements its effect.
//!
//! Inside a `do` block or a `pcall` body (where loaders build their trees) an
//! unrecognised statement stops the import with `RE9803`: skipping it could
//! leave a wrong tree. At the top level it is reported and skipped, so a
//! trailer that would fail if run (`error("boom")`) imports identically.

use std::collections::HashMap;
use std::rc::Rc;

use full_moon::LuaVersion;
use full_moon::ast::{
    self, Call, Expression, Field, FunctionArgs, FunctionCall, Index, Parameter, Prefix, Stmt,
    Suffix, Var,
};
use full_moon::node::Node as _;
use full_moon::tokenizer::{StringLiteralQuoteType, Symbol, TokenReference, TokenType};
use re_core::{Diagnostic, Location};

use crate::codes;
use crate::report::Unrecognised;
use crate::tree::{Kind, StaleScope, Tree, container_class};

/// The stack the parser and the walk get: full_moon recurses once per nesting
/// level. Reserved, not committed.
const STACK: usize = 512 * 1024 * 1024;

/// Reads a loader. `file` labels diagnostics.
pub fn read(text: &str, file: &str) -> Result<Tree, Diagnostic> {
    let text = text.to_string();
    let label = file.to_string();
    let worker = std::thread::Builder::new()
        .name("re_import loader".into())
        .stack_size(STACK)
        .spawn(move || read_on_this_thread(&text, &label));
    match worker {
        Ok(h) => h.join().unwrap_or_else(|_| {
            Err(Diagnostic::error(
                codes::INPUT_UNREADABLE,
                format!("reading {file} crashed"),
            ))
        }),
        Err(e) => Err(Diagnostic::error(
            codes::INPUT_UNREADABLE,
            format!("cannot start reading {file}: {e}"),
        )),
    }
}

fn read_on_this_thread(text: &str, file: &str) -> Result<Tree, Diagnostic> {
    let parsed = full_moon::parse_fallible(text, LuaVersion::luau());
    if let Some(err) = parsed.errors().first() {
        let (start, _) = err.range();
        return Err(Diagnostic::error(
            codes::LOADER_SYNTAX,
            format!("the loader does not parse: {}", err.error_message()),
        )
        .at(Location::file_at(
            file,
            start.line() as u32,
            start.character() as u32,
        ))
        .with_hint("nothing was imported; fix the file or regenerate it"));
    }
    let mut r = Reader {
        tree: Tree::new(),
        scopes: vec![Vec::new()],
        file: file.to_string(),
    };
    r.block(parsed.ast().nodes(), false)?;
    r.apply_script_tables();
    Ok(r.tree)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Helper {
    Make,
    Resolve,
    Clear,
}

#[derive(Clone, Debug)]
enum Val {
    Nil,
    Bool(bool),
    Num,
    Str(String),
    Table {
        items: Rc<Vec<Val>>,
        keyed: bool,
        line: u32,
    },
    Game,
    Inst(usize),
    Helper(Helper),
    /// A value the reader does not track.
    Opaque,
}

/// Why an expression or statement was not understood (always the same answer:
/// it is not one of the shapes above).
struct Unknown;

struct Reader {
    tree: Tree,
    /// Locals by scope, in declaration order.
    scopes: Vec<Vec<(String, Val)>>,
    file: String,
}

fn line_of(node: &impl full_moon::node::Node) -> (u32, u32) {
    node.start_position()
        .map(|p| (p.line() as u32, p.character() as u32))
        .unwrap_or((0, 0))
}

fn short(text: &str) -> String {
    let first = text.trim().lines().next().unwrap_or("").trim();
    if first.chars().count() > 100 {
        let cut: String = first.chars().take(97).collect();
        format!("{cut}...")
    } else {
        first.to_string()
    }
}

fn ident(t: &TokenReference) -> Option<&str> {
    match t.token().token_type() {
        TokenType::Identifier { identifier } => Some(identifier.as_str()),
        _ => None,
    }
}

impl Reader {
    fn lookup(&self, name: &str) -> Option<&Val> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v))
    }

    fn bind(&mut self, name: &str, v: Val) {
        self.scopes
            .last_mut()
            .expect("a scope")
            .push((name.to_string(), v));
    }

    fn skipped(
        &mut self,
        node: &impl full_moon::node::Node,
        text: String,
        strict: bool,
    ) -> Result<(), Diagnostic> {
        let (line, column) = line_of(node);
        let statement = short(&text);
        if strict {
            return Err(Diagnostic::error(
                codes::UNRECOGNISED_STATEMENT,
                format!(
                    "the loader builds instances with a statement the importer does not recognise: {statement}"
                ),
            )
            .at(Location::file_at(&self.file, line, column))
            .with_hint(
                "nothing was imported: inside a building block, skipping a statement could leave a wrong tree; \
                 `real explain RE9803` lists the shapes the importer reads",
            ));
        }
        self.tree.unrecognised.push(Unrecognised {
            line,
            column,
            statement,
        });
        Ok(())
    }

    fn block(&mut self, block: &ast::Block, strict: bool) -> Result<(), Diagnostic> {
        for stmt in block.stmts() {
            self.stmt(stmt, strict)?;
        }
        if let Some(last) = block.last_stmt() {
            self.skipped(last, last.to_string(), strict)?;
        }
        Ok(())
    }

    fn scoped(&mut self, block: &ast::Block) -> Result<(), Diagnostic> {
        self.scopes.push(Vec::new());
        let r = self.block(block, true);
        self.scopes.pop();
        r
    }

    fn stmt(&mut self, stmt: &Stmt, strict: bool) -> Result<(), Diagnostic> {
        let ok = match stmt {
            Stmt::LocalAssignment(la) => {
                let names: Vec<String> = la
                    .names()
                    .iter()
                    .filter_map(|t| ident(t).map(str::to_string))
                    .collect();
                let exprs: Vec<&Expression> = la.expressions().iter().collect();
                if exprs.len() == 1
                    && let Some(body) = pcall_body(exprs[0])
                {
                    self.scoped(body)?;
                    for n in &names {
                        self.bind(n, Val::Opaque);
                    }
                    true
                } else if names.len() == exprs.len() || exprs.is_empty() {
                    let mut vals = Vec::new();
                    let mut understood = true;
                    for e in &exprs {
                        match self.eval(e, strict)? {
                            Ok(v) => vals.push(v),
                            Err(Unknown) => {
                                understood = false;
                                vals.push(Val::Opaque);
                            }
                        }
                    }
                    for (i, n) in names.iter().enumerate() {
                        self.bind(n, vals.get(i).cloned().unwrap_or(Val::Nil));
                    }
                    understood
                } else {
                    for n in &names {
                        self.bind(n, Val::Opaque);
                    }
                    false
                }
            }
            Stmt::Assignment(a) => {
                let vars: Vec<&Var> = a.variables().iter().collect();
                let exprs: Vec<&Expression> = a.expressions().iter().collect();
                vars.len() == 1 && exprs.len() == 1 && self.assign(vars[0], exprs[0], strict)?
            }
            Stmt::FunctionCall(call) => {
                if let Some(body) = pcall_call_body(call) {
                    self.scoped(body)?;
                    true
                } else {
                    self.call(call, strict)?.is_ok()
                }
            }
            Stmt::Do(d) => {
                self.scoped(d.block())?;
                true
            }
            Stmt::LocalFunction(f) => {
                let name = ident(f.name()).unwrap_or_default().to_string();
                let helper = classify(f.body());
                // A function nobody calls does nothing; a call to one the
                // reader does not know is reported where it is made.
                self.bind(&name, helper.map(Val::Helper).unwrap_or(Val::Opaque));
                true
            }
            Stmt::TypeDeclaration(_) | Stmt::ExportedTypeDeclaration(_) => true,
            _ => false,
        };
        if !ok {
            self.skipped(stmt, stmt.to_string(), strict)?;
        }
        Ok(())
    }

    /// `x.Name = ...`, `x.Parent = ...`, `x.Source = ...`.
    fn assign(&mut self, var: &Var, expr: &Expression, strict: bool) -> Result<bool, Diagnostic> {
        let Var::Expression(ve) = var else {
            return Ok(false);
        };
        let suffixes: Vec<&Suffix> = ve.suffixes().collect();
        let [Suffix::Index(Index::Dot { name, .. })] = suffixes.as_slice() else {
            return Ok(false);
        };
        let Prefix::Name(target) = ve.prefix() else {
            return Ok(false);
        };
        let Some(Val::Inst(node)) = ident(target).and_then(|n| self.lookup(n)).cloned() else {
            return Ok(false);
        };
        let Ok(value) = self.eval(expr, strict)? else {
            return Ok(false);
        };
        match (ident(name), value) {
            (Some("Name"), Val::Str(s)) => self.tree.nodes[node].name = s,
            (Some("Source"), Val::Str(s)) => self.tree.nodes[node].source = Some(s),
            (Some("Parent"), Val::Inst(p)) => self.tree.set_parent(node, Some(p)),
            (Some("Parent"), Val::Nil) => self.tree.set_parent(node, None),
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn eval(
        &mut self,
        expr: &Expression,
        strict: bool,
    ) -> Result<Result<Val, Unknown>, Diagnostic> {
        Ok(match expr {
            Expression::String(t) => string_value(t).map(Val::Str).ok_or(Unknown),
            Expression::Number(t) => number_value(t).map(|_| Val::Num).ok_or(Unknown),
            Expression::Symbol(t) => match t.token().token_type() {
                TokenType::Symbol {
                    symbol: Symbol::True,
                } => Ok(Val::Bool(true)),
                TokenType::Symbol {
                    symbol: Symbol::False,
                } => Ok(Val::Bool(false)),
                TokenType::Symbol {
                    symbol: Symbol::Nil,
                } => Ok(Val::Nil),
                _ => Err(Unknown),
            },
            Expression::Parentheses { expression, .. } => return self.eval(expression, strict),
            Expression::TypeAssertion { expression, .. } => return self.eval(expression, strict),
            Expression::TableConstructor(t) => {
                let mut items = Vec::new();
                let mut keyed = false;
                for f in t.fields() {
                    let v = match f {
                        Field::NoKey(e) => self.eval(e, strict)?,
                        Field::NameKey { value, .. } => {
                            keyed = true;
                            self.eval(value, strict)?
                        }
                        Field::ExpressionKey { key, value, .. } => {
                            keyed = true;
                            if self.eval(key, strict)?.is_err() {
                                return Ok(Err(Unknown));
                            }
                            self.eval(value, strict)?
                        }
                        _ => Err(Unknown),
                    };
                    match v {
                        Ok(v) => items.push(v),
                        Err(u) => return Ok(Err(u)),
                    }
                }
                Ok(Val::Table {
                    items: Rc::new(items),
                    keyed,
                    line: line_of(&**t).0,
                })
            }
            Expression::Var(Var::Name(t)) => match ident(t) {
                Some(n) => match self.lookup(n) {
                    Some(v) => Ok(v.clone()),
                    None if n == "game" => Ok(Val::Game),
                    None if n == "workspace" => Ok(Val::Inst(
                        self.tree.service("Workspace", Some(line_of(t).0)),
                    )),
                    None => Err(Unknown),
                },
                None => Err(Unknown),
            },
            Expression::Var(Var::Expression(ve)) => {
                // `game.Service`.
                let suffixes: Vec<&Suffix> = ve.suffixes().collect();
                match (ve.prefix(), suffixes.as_slice()) {
                    (Prefix::Name(g), [Suffix::Index(Index::Dot { name, .. })])
                        if self.is_game(g) =>
                    {
                        match ident(name) {
                            Some(s) => Ok(Val::Inst(self.tree.service(s, Some(line_of(g).0)))),
                            None => Err(Unknown),
                        }
                    }
                    _ => Err(Unknown),
                }
            }
            Expression::FunctionCall(call) => return self.call(call, strict),
            _ => Err(Unknown),
        })
    }

    fn is_game(&self, t: &TokenReference) -> bool {
        match ident(t) {
            Some(n) => match self.lookup(n) {
                Some(Val::Game) => true,
                Some(_) => false,
                None => n == "game",
            },
            None => false,
        }
    }

    fn args(
        &mut self,
        args: &FunctionArgs,
        strict: bool,
    ) -> Result<Result<Vec<Val>, Unknown>, Diagnostic> {
        Ok(match args {
            FunctionArgs::Parentheses { arguments, .. } => {
                let mut out = Vec::new();
                for e in arguments.iter() {
                    match self.eval(e, strict)? {
                        Ok(v) => out.push(v),
                        Err(u) => return Ok(Err(u)),
                    }
                }
                Ok(out)
            }
            FunctionArgs::String(t) => string_value(t).map(|s| vec![Val::Str(s)]).ok_or(Unknown),
            _ => Err(Unknown),
        })
    }

    fn call(
        &mut self,
        call: &FunctionCall,
        strict: bool,
    ) -> Result<Result<Val, Unknown>, Diagnostic> {
        let Prefix::Name(head) = call.prefix() else {
            return Ok(Err(Unknown));
        };
        let line = line_of(head).0;
        let suffixes: Vec<&Suffix> = call.suffixes().collect();
        let head_val = ident(head).and_then(|n| self.lookup(n)).cloned();
        match suffixes.as_slice() {
            // game:GetService("S")
            [Suffix::Call(Call::MethodCall(m))] if self.is_game(head) => {
                let Ok(args) = self.args(m.args(), strict)? else {
                    return Ok(Err(Unknown));
                };
                match (ident(m.name()), args.as_slice()) {
                    (Some("GetService"), [Val::Str(s)]) => {
                        Ok(Ok(Val::Inst(self.tree.service(s, Some(line)))))
                    }
                    _ => Ok(Err(Unknown)),
                }
            }
            // x:AddTag("t"), x:Destroy(), CollectionService:AddTag(x, "t")
            [Suffix::Call(Call::MethodCall(m))] => {
                let Some(Val::Inst(node)) = head_val else {
                    return Ok(Err(Unknown));
                };
                let Ok(args) = self.args(m.args(), strict)? else {
                    return Ok(Err(Unknown));
                };
                let is_cs = self.tree.nodes[node].kind == Kind::Service
                    && self.tree.nodes[node].class == "CollectionService";
                match (ident(m.name()), args.as_slice()) {
                    (Some("AddTag"), [Val::Str(t)]) if !is_cs => {
                        add_tag(&mut self.tree.nodes[node].tags, t);
                        Ok(Ok(Val::Nil))
                    }
                    (Some("AddTag"), [Val::Inst(target), Val::Str(t)]) if is_cs => {
                        add_tag(&mut self.tree.nodes[*target].tags, t);
                        Ok(Ok(Val::Nil))
                    }
                    (Some("Destroy"), []) if self.tree.nodes[node].kind != Kind::Service => {
                        self.tree.destroy(node);
                        self.tree.set_parent(node, None);
                        Ok(Ok(Val::Nil))
                    }
                    _ => Ok(Err(Unknown)),
                }
            }
            // Instance.new("C"[, parent])
            [
                Suffix::Index(Index::Dot { name, .. }),
                Suffix::Call(Call::AnonymousCall(a)),
            ] if ident(head) == Some("Instance")
                && head_val.is_none()
                && ident(name) == Some("new") =>
            {
                let Ok(args) = self.args(a, strict)? else {
                    return Ok(Err(Unknown));
                };
                match args.as_slice() {
                    [Val::Str(c)] => Ok(Ok(Val::Inst(self.tree.instance(
                        c,
                        c,
                        Kind::Declared,
                        Some(line),
                    )))),
                    [Val::Str(c), Val::Inst(p)] => {
                        let n = self.tree.instance(c, c, Kind::Declared, Some(line));
                        self.tree.set_parent(n, Some(*p));
                        Ok(Ok(Val::Inst(n)))
                    }
                    _ => Ok(Err(Unknown)),
                }
            }
            // A helper.
            [Suffix::Call(Call::AnonymousCall(a))] => {
                let Some(Val::Helper(h)) = head_val else {
                    return Ok(Err(Unknown));
                };
                let Ok(args) = self.args(a, strict)? else {
                    return Ok(Err(Unknown));
                };
                Ok(self.helper(h, &args, line))
            }
            _ => Ok(Err(Unknown)),
        }
    }

    fn helper(&mut self, h: Helper, args: &[Val], line: u32) -> Result<Val, Unknown> {
        match (h, args) {
            (Helper::Make, [Val::Str(class), Val::Str(name), Val::Inst(parent)]) => {
                let n = self.tree.instance(class, name, Kind::Declared, Some(line));
                self.tree.set_parent(n, Some(*parent));
                Ok(Val::Inst(n))
            }
            (
                Helper::Resolve,
                [
                    Val::Table {
                        items,
                        keyed: false,
                        ..
                    },
                ],
            ) => {
                let names: Option<Vec<&str>> = items
                    .iter()
                    .map(|v| match v {
                        Val::Str(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .collect();
                let names = names.ok_or(Unknown)?;
                let (first, rest) = names.split_first().ok_or(Unknown)?;
                let mut at = self.tree.service(first, Some(line));
                for name in rest {
                    let class = container_class(&self.tree.nodes[at].class, name);
                    at = self.tree.container(at, name, class, Some(line));
                }
                Ok(Val::Inst(at))
            }
            (Helper::Clear, [Val::Inst(parent), Val::Str(name)]) => {
                let doomed: Vec<usize> = self
                    .tree
                    .live_children(*parent)
                    .filter(|&c| self.tree.nodes[c].name == *name)
                    .collect();
                for c in doomed {
                    self.tree.destroy(c);
                }
                self.tree.scopes.push(StaleScope::Named {
                    parent: *parent,
                    name: name.clone(),
                });
                Ok(Val::Nil)
            }
            _ => Err(Unknown),
        }
    }

    /// Applies the top-level script tables.
    fn apply_script_tables(&mut self) {
        let top = self.scopes[0].clone();
        let tag = top.iter().rev().find_map(|(n, v)| match (n.as_str(), v) {
            ("TAG", Val::Str(s)) => Some(s.clone()),
            _ => None,
        });
        if let Some(Val::Bool(false)) = top
            .iter()
            .rev()
            .find(|(n, _)| n == "REMOVE_OLD")
            .map(|(_, v)| v)
        {
            self.tree.remove_stale = false;
        }
        let mut any = false;
        let mut partial = false;
        for (name, v) in &top {
            let Val::Table {
                items,
                keyed: false,
                line: table_line,
            } = v
            else {
                continue;
            };
            let read: Vec<Option<Entry>> = items.iter().map(script_entry).collect();
            let good = read.iter().filter(|e| e.is_some()).count();
            if good == 0 {
                continue;
            }
            if good < read.len() {
                // Most of it is a script table; the rest is not, and skipping
                // it quietly would lose scripts.
                partial = true;
                for (i, e) in read.iter().enumerate() {
                    if e.is_none() {
                        let line = match &items[i] {
                            Val::Table { line, .. } => *line,
                            _ => *table_line,
                        };
                        self.tree.unrecognised.push(Unrecognised {
                            line,
                            column: 1,
                            statement: format!(
                                "entry {} of the script table {name} is not {{ {{path...}}, \"Script\" | \"LocalScript\" | \"ModuleScript\", source }}",
                                i + 1
                            ),
                        });
                    }
                }
            }
            any = true;
            for (path, class, source, line) in read.into_iter().flatten() {
                self.apply_entry(&path, &class, source, line, tag.as_deref());
            }
        }
        if any && partial {
            self.tree.warnings.push(Diagnostic::warning(
                codes::UNRECOGNISED_STATEMENT,
                "a script table has entries the importer does not read, so no stale scripts were removed",
            ));
        } else if any {
            self.tree.scopes.push(StaleScope::Scripts);
        }
    }

    fn apply_entry(
        &mut self,
        path: &[String],
        class: &str,
        source: String,
        line: u32,
        tag: Option<&str>,
    ) {
        let at_line = Some(line);
        let mut at = self.tree.service(&path[0], at_line);
        for name in &path[1..path.len() - 1] {
            let class = container_class(&self.tree.nodes[at].class, name);
            at = self.tree.container(at, name, class, at_line);
            // The loader tags the folders it makes, not ones that exist.
            if let Some(t) = tag {
                add_tag(&mut self.tree.nodes[at].tags_if_created, t);
            }
        }
        let name = &path[path.len() - 1];
        let existing = self
            .tree
            .live_children(at)
            .find(|&c| self.tree.nodes[c].name == *name && self.tree.nodes[c].class == class);
        let node = match existing {
            Some(n) => {
                self.tree.warnings.push(
                    Diagnostic::warning(
                        codes::INSTANCE_FAILED,
                        format!(
                            "{} is listed twice; the later entry's source wins, as when the loader runs",
                            crate::report::display_path(path)
                        ),
                    )
                    .at(Location::file_at(&self.file, line, 1)),
                );
                n
            }
            None => {
                let n = self.tree.instance(class, name, Kind::Declared, at_line);
                self.tree.set_parent(n, Some(at));
                n
            }
        };
        self.tree.nodes[node].source = Some(source);
        if let Some(t) = tag {
            add_tag(&mut self.tree.nodes[node].tags, t);
        }
    }
}

fn add_tag(tags: &mut Vec<String>, t: &str) {
    if !tags.iter().any(|x| x == t) {
        tags.push(t.to_string());
    }
}

/// A script table entry: path, class, source and line.
type Entry = (Vec<String>, String, String, u32);

/// `{ {"Service", ..., "Name"}, "Class", source }`, for a script class.
fn script_entry(v: &Val) -> Option<Entry> {
    let Val::Table {
        items,
        keyed: false,
        line,
    } = v
    else {
        return None;
    };
    let [
        Val::Table {
            items: path,
            keyed: false,
            ..
        },
        Val::Str(class),
        Val::Str(source),
    ] = items.as_slice()
    else {
        return None;
    };
    if !matches!(class.as_str(), "Script" | "LocalScript" | "ModuleScript") || path.len() < 2 {
        return None;
    }
    let path: Option<Vec<String>> = path
        .iter()
        .map(|p| match p {
            Val::Str(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    Some((path?, class.clone(), source.clone(), *line))
}

/// The body of `pcall(function() ... end)`.
fn pcall_body(e: &Expression) -> Option<&ast::Block> {
    match e {
        Expression::FunctionCall(c) => pcall_call_body(c),
        _ => None,
    }
}

fn pcall_call_body(c: &FunctionCall) -> Option<&ast::Block> {
    let Prefix::Name(n) = c.prefix() else {
        return None;
    };
    if ident(n) != Some("pcall") {
        return None;
    }
    let suffixes: Vec<&Suffix> = c.suffixes().collect();
    let [Suffix::Call(Call::AnonymousCall(a))] = suffixes.as_slice() else {
        return None;
    };
    let FunctionArgs::Parentheses { arguments, .. } = &**a else {
        return None;
    };
    let args: Vec<&Expression> = arguments.iter().collect();
    match args.as_slice() {
        [Expression::Function(f)] if f.body().parameters().is_empty() => Some(f.body().block()),
        _ => None,
    }
}

/// Recognises a helper by its parameters and body.
fn classify(body: &ast::FunctionBody) -> Option<Helper> {
    let params: Option<Vec<String>> = body
        .parameters()
        .iter()
        .map(|p| match p {
            Parameter::Name(t) => ident(t).map(str::to_string),
            _ => None,
        })
        .collect();
    let params = params?;
    // The body's code without whitespace or comments.
    let mut code = String::new();
    let mut words: Vec<String> = Vec::new();
    let mut loops = false;
    let mut returns = false;
    // `tokens()` yields a span's brackets before its contents: sort by place.
    let mut tokens: Vec<&TokenReference> = body.block().tokens().collect();
    tokens.sort_by_key(|t| t.token().start_position().bytes());
    for t in tokens {
        if let TokenType::Symbol { symbol } = t.token().token_type() {
            loops |= matches!(symbol, Symbol::For | Symbol::While | Symbol::Repeat);
            returns |= *symbol == Symbol::Return;
        }
        let text = t.token().to_string();
        code.push_str(&text);
        if text != ";" {
            words.push(text);
        }
    }
    let has = |s: &str| code.contains(s);
    let shape = |pattern: &[&str], bound: &[(&str, &String)]| shaped(&words, pattern, bound);
    match params.as_slice() {
        [c, n, p] => {
            let bound = [("$c", c), ("$n", n), ("$p", p)];
            let name_first = [
                "local", "$v", "=", "Instance", ".", "new", "(", "$c", ")", "$v", ".", "Name", "=",
                "$n", "$v", ".", "Parent", "=", "$p", "return", "$v",
            ];
            let parent_first = [
                "local", "$v", "=", "Instance", ".", "new", "(", "$c", ")", "$v", ".", "Parent",
                "=", "$p", "$v", ".", "Name", "=", "$n", "return", "$v",
            ];
            (shape(&name_first, &bound) || shape(&parent_first, &bound)).then_some(Helper::Make)
        }
        [path]
            if has("GetService")
                && has(":FindFirstChild(")
                && (has("Instance.new(\"Folder\")") || has("Instance.new('Folder')"))
                && (has(&format!("ipairs({path})")) || has(&format!("in{path}do")))
                && loops
                && returns =>
        {
            Some(Helper::Resolve)
        }
        [parent, name] => {
            let bound = [("$parent", parent), ("$name", name)];
            let children = ["$parent", ":", "GetChildren", "(", ")"];
            let body = [
                "do", "if", "$v", ".", "Name", "==", "$name", "then", "$v", ":", "Destroy", "(",
                ")", "end", "end",
            ];
            // `in ipairs(p:GetChildren())`, `in pairs(...)` or `in p:GetChildren()`.
            let clear = |iter: Option<&str>| {
                let mut pattern = vec!["for", "$k", ",", "$v", "in"];
                if let Some(f) = iter {
                    pattern.extend([f, "("]);
                }
                pattern.extend(children);
                if iter.is_some() {
                    pattern.push(")");
                }
                pattern.extend(body);
                shape(&pattern, &bound)
            };
            (clear(Some("ipairs")) || clear(Some("pairs")) || clear(None)).then_some(Helper::Clear)
        }
        _ => None,
    }
}

/// Whether a body's tokens are exactly `pattern`. `$x` in the pattern is a
/// name: the parameter `bound` gives it, or else any local name, the same at
/// every `$x` and different from the parameters and other `$` names.
fn shaped(words: &[String], pattern: &[&str], bound: &[(&str, &String)]) -> bool {
    if words.len() != pattern.len() {
        return false;
    }
    let mut names: HashMap<&str, &str> = bound.iter().map(|(k, v)| (*k, v.as_str())).collect();
    for (w, p) in words.iter().zip(pattern) {
        if !p.starts_with('$') {
            if w != p {
                return false;
            }
            continue;
        }
        match names.get(p) {
            Some(&v) if v != w => return false,
            Some(_) => {}
            None => {
                let is_name = w
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
                    && w.chars().all(|c| c.is_alphanumeric() || c == '_');
                if !is_name || names.values().any(|v| v == w) {
                    return false;
                }
                names.insert(p, w);
            }
        }
    }
    true
}

fn number_value(t: &TokenReference) -> Option<f64> {
    let TokenType::Number { text } = t.token().token_type() else {
        return None;
    };
    let s = text.replace('_', "");
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok().map(|v| v as f64);
    }
    if let Some(bin) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        return u64::from_str_radix(bin, 2).ok().map(|v| v as f64);
    }
    s.parse().ok()
}

/// The value of a string literal: escapes decoded, or a long string's text
/// without the newline that follows its opening bracket.
pub(crate) fn string_value(t: &TokenReference) -> Option<String> {
    let TokenType::StringLiteral {
        literal,
        quote_type,
        ..
    } = t.token().token_type()
    else {
        return None;
    };
    match quote_type {
        StringLiteralQuoteType::Brackets => {
            let s = literal.as_str();
            let s = s
                .strip_prefix("\r\n")
                .or_else(|| s.strip_prefix('\n'))
                .or_else(|| s.strip_prefix('\r'))
                .unwrap_or(s);
            Some(s.to_string())
        }
        _ => unescape(literal.as_str()),
    }
}

/// Decodes Luau's escapes in a quoted string's text.
fn unescape(s: &str) -> Option<String> {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'\\' {
            out.push(b);
            i += 1;
            continue;
        }
        i += 1;
        let e = *bytes.get(i)?;
        i += 1;
        match e {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'\\' => out.push(b'\\'),
            b'"' => out.push(b'"'),
            b'\'' => out.push(b'\''),
            b'\n' => out.push(b'\n'),
            b'\r' => {
                out.push(b'\n');
                if bytes.get(i) == Some(&b'\n') {
                    i += 1;
                }
            }
            b'z' => {
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
            }
            b'x' => {
                let h = std::str::from_utf8(bytes.get(i..i + 2)?).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 2;
            }
            b'u' => {
                if bytes.get(i) != Some(&b'{') {
                    return None;
                }
                let end = i + bytes[i..].iter().position(|&c| c == b'}')?;
                let cp =
                    u32::from_str_radix(std::str::from_utf8(&bytes[i + 1..end]).ok()?, 16).ok()?;
                let ch = char::from_u32(cp)?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                i = end + 1;
            }
            d if d.is_ascii_digit() => {
                let mut v = (d - b'0') as u32;
                for _ in 0..2 {
                    match bytes.get(i) {
                        Some(c) if c.is_ascii_digit() => {
                            v = v * 10 + (c - b'0') as u32;
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push(u8::try_from(v).ok()?);
            }
            _ => return None,
        }
    }
    String::from_utf8(out).ok()
}
