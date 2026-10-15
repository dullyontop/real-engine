//! `roblox.d.luau`: Luau type definitions of the Roblox-style API, generated
//! from the compat layer's class, enum and library tables, so `real check
//! --types` can check a compat project's scripts (docs/M2_PLAN.md, WP16).
//!
//! The file is written for luau-lsp's definitions loader, alongside
//! `engine.d.luau`:
//!
//! * every class becomes an extern type extending its superclass, with its
//!   properties (`read` when read-only), methods and events (as
//!   `RBXScriptSignal`); a member an ancestor already declares is left out;
//! * every enum becomes `Enum<Name>` (its items' type) and an
//!   `Enum<Name>_INTERNAL` table type, reached through `Enum.<Name>`;
//! * every datatype (a library with value members) becomes an extern type
//!   with its value members, plus its table (`Vector3.new`, `Vector3.zero`);
//!   the arithmetic operators of the geometry and UI datatypes are added here,
//!   since the tables do not describe operators;
//! * globals naming an instance (`game`, `workspace`) get their class.
//!
//! Members are declared whatever their implementation status: the
//! definitions describe the API's shape, and members the engine lacks are
//! reported by the scan and at run time (`RE9001`), not as type errors. A type
//! the tables name but the definitions cannot (a datatype not described yet)
//! is `any`.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use re_compat::ClassRegistry;
use re_compat::data::{LibraryDoc, LibraryMemberDoc, LibraryMemberKind, ParamDoc};

/// Where [`write_definitions`] puts `roblox.d.luau` in a project, next to
/// `engine.d.luau`.
pub const DEFINITIONS_PATH: &str = ".real/types/roblox.d.luau";

/// Names the definitions never declare: Luau's own globals.
const LUAU_GLOBALS: &[&str] = &["typeof", "type", "require", "print", "Enum"];

const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

fn is_name(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !KEYWORDS.contains(&s)
}

/// Arithmetic the datatype tables do not describe, as extern type members.
fn operators(datatype: &str) -> &'static [&'static str] {
    match datatype {
        "Vector3" => &[
            "__add: (Vector3, Vector3) -> Vector3",
            "__sub: (Vector3, Vector3) -> Vector3",
            "__mul: ((Vector3, Vector3) -> Vector3) & ((Vector3, number) -> Vector3)",
            "__div: ((Vector3, Vector3) -> Vector3) & ((Vector3, number) -> Vector3)",
            "__idiv: ((Vector3, Vector3) -> Vector3) & ((Vector3, number) -> Vector3)",
            "__unm: (Vector3) -> Vector3",
        ],
        "Vector2" => &[
            "__add: (Vector2, Vector2) -> Vector2",
            "__sub: (Vector2, Vector2) -> Vector2",
            "__mul: ((Vector2, Vector2) -> Vector2) & ((Vector2, number) -> Vector2)",
            "__div: ((Vector2, Vector2) -> Vector2) & ((Vector2, number) -> Vector2)",
            "__idiv: ((Vector2, Vector2) -> Vector2) & ((Vector2, number) -> Vector2)",
            "__unm: (Vector2) -> Vector2",
        ],
        "CFrame" => &[
            "__mul: ((CFrame, CFrame) -> CFrame) & ((CFrame, Vector3) -> Vector3)",
            "__add: (CFrame, Vector3) -> CFrame",
            "__sub: (CFrame, Vector3) -> CFrame",
        ],
        "UDim" => &[
            "__add: (UDim, UDim) -> UDim",
            "__sub: (UDim, UDim) -> UDim",
            "__unm: (UDim) -> UDim",
        ],
        "UDim2" => &[
            "__add: (UDim2, UDim2) -> UDim2",
            "__sub: (UDim2, UDim2) -> UDim2",
            "__unm: (UDim2) -> UDim2",
        ],
        _ => &[],
    }
}

/// The type names the definitions declare, for [`Types::luau`].
struct Types<'a> {
    reg: &'a ClassRegistry,
    datatypes: HashSet<&'a str>,
}

impl Types<'_> {
    /// The Luau type for a type as the tables write it.
    fn luau(&self, ty: &str) -> String {
        let ty = ty.trim();
        if let Some(inner) = ty.strip_suffix('?') {
            let t = self.luau(inner);
            return if t == "any" { t } else { format!("{t}?") };
        }
        if let Some(inner) = ty.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
            return format!("{{ {} }}", self.luau(inner));
        }
        if let Some(class) = ty
            .strip_prefix("Instance<")
            .and_then(|t| t.strip_suffix('>'))
        {
            return self.luau(class);
        }
        if let Some(e) = ty.strip_prefix("Enum.") {
            return if self.reg.enum_type(e).is_some() && is_name(e) {
                format!("Enum{e}")
            } else {
                "EnumItem".into()
            };
        }
        match ty {
            "bool" | "boolean" => "boolean".into(),
            "number" | "int" | "int64" | "float" | "double" => "number".into(),
            "string" | "any" | "nil" | "buffer" | "thread" | "vector" => ty.into(),
            "table" => "{ [any]: any }".into(),
            "function" => "(...any) -> ...any".into(),
            "RBXScriptSignal" | "RBXScriptConnection" | "EnumItem" => ty.into(),
            _ if self.reg.try_class(ty).is_some() && is_name(ty) => ty.into(),
            _ if self.datatypes.contains(ty) => ty.into(),
            _ => "any".into(),
        }
    }

    fn params(&self, params: &[ParamDoc], with_self: bool) -> String {
        let mut out: Vec<String> = Vec::new();
        if with_self {
            out.push("self".into());
        }
        for p in params {
            if p.name == "..." || p.ty == "..." {
                let t = if p.ty == "..." {
                    "any".to_string()
                } else {
                    self.luau(&p.ty)
                };
                // `...: T` in a declaration, `...T` in a function type.
                out.push(if with_self {
                    format!("...: {t}")
                } else {
                    format!("...{t}")
                });
                break;
            }
            let name = if is_name(&p.name) { &p.name } else { "arg" };
            let mut t = self.luau(&p.ty);
            if p.optional && !t.ends_with('?') && t != "any" {
                t = if t.contains(' ') {
                    format!("({t})?")
                } else {
                    format!("{t}?")
                };
            }
            out.push(format!("{name}: {t}"));
        }
        out.join(", ")
    }

    fn returns(&self, returns: &[String]) -> String {
        match returns {
            [] => "()".into(),
            [one] if one == "..." => "...any".into(),
            [one] => self.luau(one),
            many => format!(
                "({})",
                many.iter()
                    .map(|r| if r == "..." {
                        "...any".to_string()
                    } else {
                        self.luau(r)
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// `(a: T) -> R`, a function member's type in a table (`...T` for a
    /// variadic tail).
    fn function(&self, m: &LibraryMemberDoc) -> String {
        format!(
            "({}) -> {}",
            self.params(&m.params, false),
            self.returns(&m.returns)
        )
    }
}

fn comment(out: &mut String, indent: &str, doc: &str) {
    let line = doc.lines().next().unwrap_or("").trim();
    if !line.is_empty() {
        let _ = writeln!(out, "{indent}-- {line}");
    }
}

fn is_datatype(l: &LibraryDoc) -> bool {
    l.class.is_none() && (!l.value_members.is_empty() || !l.record.is_empty())
}

/// `roblox.d.luau` for the classes, enums and libraries of `reg`.
pub fn definitions(reg: &ClassRegistry) -> String {
    let datatypes: HashSet<&str> = reg
        .libraries()
        .filter(|l| is_datatype(l) && is_name(&l.name))
        .map(|l| l.name.as_str())
        .collect();
    let types = Types { reg, datatypes };
    let mut out = String::from(
        "--!strict\n-- roblox.d.luau: the Roblox-style API of Real Engine's compat layer.\n\
         -- Generated by re_script_check::roblox::definitions() from the class, enum\n\
         -- and library tables; do not edit.\n\
         -- Load it with: luau-lsp analyze --definitions=@roblox=roblox.d.luau --flag:LuauSolverV2=true\n\n",
    );

    out.push_str(
        "declare extern type RBXScriptConnection with\n\
         \tread Connected: boolean\n\
         \tfunction Disconnect(self): ()\n\
         end\n\n\
         declare extern type RBXScriptSignal with\n\
         \tfunction Connect(self, callback: (...any) -> ()): RBXScriptConnection\n\
         \tfunction Once(self, callback: (...any) -> ()): RBXScriptConnection\n\
         \tfunction ConnectParallel(self, callback: (...any) -> ()): RBXScriptConnection\n\
         \tfunction Wait(self): ...any\n\
         \tfunction connect(self, callback: (...any) -> ()): RBXScriptConnection\n\
         \tfunction wait(self): ...any\n\
         end\n\n\
         declare extern type EnumItem with\n\
         \tread Name: string\n\
         \tread Value: number\n\
         \tread EnumType: any\n\
         \tfunction IsA(self, enumName: string): boolean\n\
         end\n\n",
    );

    // Enums.
    let enums: Vec<_> = reg.enums().filter(|e| is_name(&e.name)).collect();
    for e in &enums {
        let _ = writeln!(
            out,
            "declare extern type Enum{} extends EnumItem with end",
            e.name
        );
    }
    out.push('\n');
    for e in &enums {
        let _ = writeln!(out, "declare extern type Enum{}_INTERNAL with", e.name);
        for item in e.items.iter().filter(|i| is_name(&i.name)) {
            let _ = writeln!(out, "\tread {}: Enum{}", item.name, e.name);
        }
        let _ = writeln!(out, "\tfunction GetEnumItems(self): {{ Enum{} }}", e.name);
        let _ = writeln!(
            out,
            "\tfunction FromName(self, name: string): Enum{}?",
            e.name
        );
        let _ = writeln!(
            out,
            "\tfunction FromValue(self, value: number): Enum{}?",
            e.name
        );
        out.push_str("end\n\n");
    }
    out.push_str("declare Enum: {\n");
    for e in &enums {
        let _ = writeln!(out, "\t{}: Enum{}_INTERNAL,", e.name, e.name);
    }
    out.push_str("}\n\n");

    // Datatypes: their values' members, then their tables.
    let libraries: Vec<&LibraryDoc> = reg.libraries().collect();
    for l in libraries
        .iter()
        .filter(|l| types.datatypes.contains(l.name.as_str()))
    {
        comment(&mut out, "", &l.doc);
        let _ = writeln!(out, "declare extern type {} with", l.name);
        let mut seen = HashSet::new();
        for m in &l.value_members {
            if !is_name(&m.name) || !seen.insert(m.name.as_str()) {
                continue;
            }
            match m.kind {
                LibraryMemberKind::Method | LibraryMemberKind::Function => {
                    let _ = writeln!(
                        out,
                        "\tfunction {}({}): {}",
                        m.name,
                        types.params(&m.params, true),
                        types.returns(&m.returns)
                    );
                }
                LibraryMemberKind::Property | LibraryMemberKind::Constant => {
                    let t = m.ty.as_deref().map_or("any".into(), |t| types.luau(t));
                    let _ = writeln!(out, "\tread {}: {t}", m.name);
                }
            }
        }
        for op in operators(&l.name) {
            let _ = writeln!(out, "\t{op}");
        }
        let _ = writeln!(out, "\tfunction __eq(self, other: {}): boolean", l.name);
        out.push_str("\tfunction __tostring(self): string\nend\n\n");
    }

    // Classes, ancestors first so each `extends` names a declared type.
    let mut order: Vec<&str> = Vec::new();
    let mut placed: HashSet<&str> = HashSet::new();
    let classes: BTreeMap<&str, _> = reg
        .classes()
        .filter(|c| is_name(&c.name))
        .map(|c| (c.name.as_str(), c))
        .collect();
    fn place<'a>(
        name: &'a str,
        classes: &BTreeMap<&'a str, &'a re_compat::ClassDef>,
        placed: &mut HashSet<&'a str>,
        order: &mut Vec<&'a str>,
    ) {
        if placed.contains(name) {
            return;
        }
        let Some(c) = classes.get(name) else {
            return;
        };
        placed.insert(name);
        if let Some(s) = c.superclass.as_deref() {
            place(s, classes, placed, order);
        }
        order.push(name);
    }
    for name in classes.keys() {
        place(name, &classes, &mut placed, &mut order);
    }
    for name in &order {
        let c = classes[name];
        comment(&mut out, "", &c.doc);
        match c.superclass.as_deref().filter(|s| classes.contains_key(s)) {
            Some(s) => {
                let _ = writeln!(out, "declare extern type {} extends {s} with", c.name);
            }
            None => {
                let _ = writeln!(out, "declare extern type {} with", c.name);
            }
        }
        // Members an ancestor declares are inherited.
        let inherited: HashSet<&str> = reg
            .ancestry(&c.name)
            .iter()
            .skip(1)
            .flat_map(|a| {
                a.properties
                    .iter()
                    .map(|p| p.name.as_str())
                    .chain(a.methods.iter().map(|m| m.name.as_str()))
                    .chain(a.events.iter().map(|e| e.name.as_str()))
            })
            .collect();
        let mut seen: HashSet<&str> = HashSet::new();
        let fresh = |name: &str| is_name(name) && !inherited.contains(name);
        for p in &c.properties {
            if fresh(&p.name) && seen.insert(&p.name) {
                let read = if p.read_only { "read " } else { "" };
                let _ = writeln!(out, "\t{read}{}: {}", p.name, types.luau(&p.ty));
            }
        }
        for e in &c.events {
            if fresh(&e.name) && seen.insert(&e.name) {
                let _ = writeln!(out, "\tread {}: RBXScriptSignal", e.name);
            }
        }
        for m in &c.methods {
            if fresh(&m.name) && seen.insert(&m.name) {
                let _ = writeln!(
                    out,
                    "\tfunction {}({}): {}",
                    m.name,
                    types.params(&m.params, true),
                    types.returns(&m.returns)
                );
            }
        }
        out.push_str("end\n\n");
    }

    // Globals: instances, then datatype and function tables.
    for l in &libraries {
        if !is_name(&l.name) || LUAU_GLOBALS.contains(&l.name.as_str()) {
            continue;
        }
        if let Some(class) = &l.class {
            let _ = writeln!(out, "declare {}: {}", l.name, types.luau(class));
            continue;
        }
        let members: Vec<&LibraryMemberDoc> =
            l.members.iter().filter(|m| is_name(&m.name)).collect();
        if members.is_empty() {
            continue;
        }
        let _ = writeln!(out, "declare {}: {{", l.name);
        let mut seen = HashSet::new();
        for m in members {
            if !seen.insert(m.name.as_str()) {
                continue;
            }
            let t = match m.kind {
                LibraryMemberKind::Function | LibraryMemberKind::Method => types.function(m),
                _ => m.ty.as_deref().map_or("any".into(), |t| types.luau(t)),
            };
            let _ = writeln!(out, "\t{}: {t},", m.name);
        }
        out.push_str("}\n");
    }
    out
}

/// Writes [`definitions`] to [`DEFINITIONS_PATH`] under `project` (only when
/// it changed) and returns the path.
pub fn write_definitions(
    project: &std::path::Path,
    reg: &ClassRegistry,
) -> std::io::Result<std::path::PathBuf> {
    let path = project.join(DEFINITIONS_PATH);
    let text = definitions(reg);
    if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(path);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> ClassRegistry {
        ClassRegistry::builtin().unwrap()
    }

    #[test]
    fn types_map_to_declared_names_or_any() {
        let reg = reg();
        let datatypes: HashSet<&str> = ["Vector3"].into_iter().collect();
        let t = Types {
            reg: &reg,
            datatypes,
        };
        assert_eq!(t.luau("bool"), "boolean");
        assert_eq!(t.luau("int"), "number");
        assert_eq!(t.luau("Instance<Part>"), t.luau("Part"));
        assert_eq!(t.luau("Instance?"), "Instance?");
        assert_eq!(t.luau("{Instance}"), "{ Instance }");
        assert_eq!(t.luau("Vector3"), "Vector3");
        assert_eq!(t.luau("NoSuchDatatype"), "any");
        assert_eq!(t.luau("Enum.NoSuchEnum"), "EnumItem");
        let p = |name: &str, ty: &str, optional| ParamDoc {
            name: name.into(),
            ty: ty.into(),
            optional,
        };
        assert_eq!(
            t.params(
                &[
                    p("name", "string", false),
                    p("recursive", "bool", true),
                    p("...", "any", true)
                ],
                true
            ),
            "self, name: string, recursive: boolean?, ...: any"
        );
        assert_eq!(t.returns(&[]), "()");
        assert_eq!(
            t.returns(&["number".into(), "bool".into()]),
            "(number, boolean)"
        );
    }

    #[test]
    fn every_class_enum_and_datatype_is_declared_once() {
        let reg = reg();
        let defs = definitions(&reg);
        for c in reg.classes() {
            let decl = format!("declare extern type {} ", c.name);
            assert_eq!(defs.matches(&decl).count(), 1, "{}", c.name);
        }
        assert!(defs.contains("declare game: DataModel"));
        assert!(defs.contains("declare extern type Vector3 with"));
        assert!(defs.contains("declare Vector3: {"));
        assert!(defs.contains("declare Enum: {"));
        // Declared before use as a superclass.
        let at = |s: &str| defs.find(&format!("declare extern type {s} ")).unwrap();
        assert!(at("Instance") < at("Folder"));
    }
}
