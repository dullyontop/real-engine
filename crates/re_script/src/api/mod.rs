//! The engine-native Luau API (ARCHITECTURE §4, "Luau view"; ADR-0012).
//!
//! Every library function is registered through `Lib::func` together with its
//! Luau type signature and a one-line doc, and every userdata type has its
//! declaration next to its `UserData` impl. [`definitions`] writes
//! `engine.d.luau` from exactly those registrations, so the type checker, editors
//! and agents see the API the VM really has.
//!
//! Library tables are strict: reading a member that does not exist raises
//! `RE9001 unsupported API member: <lib>.<member>` instead of returning `nil`.

use mlua::{
    AnyUserData, FromLuaMulti, IntoLua, IntoLuaMulti, Lua, MetaMethod, Result, Table,
    UserDataMethods, Value,
};
use re_core::Code;

use crate::codes;
use crate::vm::ContextKind;

pub(crate) mod anim;
pub mod entity;
pub mod game;
pub(crate) mod json;
pub(crate) mod libs;
pub mod math;
pub mod proc;
pub(crate) mod scene;
pub(crate) mod stream;
pub mod value;
pub(crate) mod world;

pub use entity::EntityRef;
pub use math::{LColor, LQuat, V3};

/// A Luau error carrying an `RE####` code (`[RE####] message`), which
/// [`ScriptError`](crate::ScriptError) turns back into the diagnostic's code.
pub(crate) fn error(code: Code, message: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(format!("[{code}] {message}"))
}

/// The `RE9001` error for a member that does not exist.
pub(crate) fn unsupported(owner: &str, member: &str) -> mlua::Error {
    error(
        codes::UNSUPPORTED_MEMBER,
        format!("unsupported API member: {owner}.{member}"),
    )
}

/// A conversion error with the Luau type that was found.
pub(crate) fn conversion_error(found: &Value, to: &'static str, hint: &str) -> mlua::Error {
    mlua::Error::FromLuaConversionError {
        from: found.type_name(),
        to: to.into(),
        message: Some(hint.into()),
    }
}

/// Makes reading an unknown member of a userdata raise `RE9001`.
pub(crate) fn strict_userdata<T, M: UserDataMethods<T>>(methods: &mut M, type_name: &'static str) {
    methods.add_meta_function(
        MetaMethod::Index,
        move |_, (_ud, key): (AnyUserData, Value)| -> Result<Value> {
            let key = key.to_string().unwrap_or_else(|_| "?".into());
            Err(unsupported(type_name, &key))
        },
    );
}

/// One library table being built, with the declarations of its members.
pub(crate) struct Lib {
    name: &'static str,
    doc: &'static str,
    table: Table,
    members: Vec<String>,
}

impl Lib {
    /// An empty library named `name` (the global it will be installed as).
    pub(crate) fn new(lua: &Lua, name: &'static str, doc: &'static str) -> Result<Lib> {
        Ok(Lib {
            name,
            doc,
            table: lua.create_table()?,
            members: Vec::new(),
        })
    }

    /// Adds a Rust function with its Luau signature (`(a: number) -> string`) and doc.
    pub(crate) fn func<A, R, F>(
        &mut self,
        lua: &Lua,
        name: &str,
        signature: &str,
        doc: &str,
        f: F,
    ) -> Result<()>
    where
        A: FromLuaMulti,
        R: IntoLuaMulti,
        F: Fn(&Lua, A) -> Result<R> + 'static,
    {
        let function = lua.create_function(f)?;
        self.value(name, signature, doc, function)
    }

    /// Adds any value (a constant, or a function written in Luau) with its type.
    pub(crate) fn value(
        &mut self,
        name: &str,
        ty: &str,
        doc: &str,
        value: impl IntoLua,
    ) -> Result<()> {
        self.table.raw_set(name, value)?;
        self.members
            .push(format!("    -- {doc}\n    {name}: {ty},"));
        Ok(())
    }

    /// The `declare <name>: { ... }` block for `engine.d.luau`.
    pub(crate) fn declaration(&self) -> String {
        format!(
            "-- {}\ndeclare {}: {{\n{}\n}}",
            self.doc,
            self.name,
            self.members.join("\n")
        )
    }

    /// Makes the table strict and returns it, ready to be installed as a global.
    pub(crate) fn finish(self, lua: &Lua) -> Result<(Table, String)> {
        let decl = self.declaration();
        let mt = lua.create_table()?;
        let owner = self.name;
        mt.raw_set(
            "__index",
            lua.create_function(move |_, (_t, key): (Table, Value)| -> Result<Value> {
                let key = key.to_string().unwrap_or_else(|_| "?".into());
                Err(unsupported(owner, &key))
            })?,
        )?;
        mt.raw_set("__metatable", false)?;
        self.table.set_metatable(Some(mt))?;
        Ok((self.table, decl))
    }
}

/// Installs every library of `kind`'s context into `globals`; returns the
/// declarations in a fixed order.
pub(crate) fn install(lua: &Lua, globals: &Table, kind: ContextKind) -> Result<Vec<String>> {
    let mut decls = vec![
        entity::DECL.to_string(),
        math::QUAT_DECL.to_string(),
        math::COLOR_DECL.to_string(),
        libs::CONNECTION_DECL.to_string(),
        proc::GRAPH_DECLS.to_string(),
        scene::SCENE_LOAD_DECL.to_string(),
    ];
    for (name, table, decl) in libs::all(lua, kind)? {
        globals.raw_set(name, table)?;
        decls.push(decl);
    }
    decls.push(libs::GLOBAL_DECLS.to_string());
    Ok(decls)
}

/// `engine.d.luau`: the Luau type definitions of the whole native API, generated
/// from the binding registrations (ADR-0013). The `command` library exists only
/// in the `tools` context; it is included and documented as such.
///
/// The file loads in luau-lsp 1.70.1 with
/// `--definitions=@engine=engine.d.luau --flag:LuauSolverV2=true`.
pub fn definitions() -> String {
    let lua = Lua::new();
    let decls =
        crate::vm::install_engine(&lua, ContextKind::Tools, std::time::Duration::from_secs(1))
            .expect("the API installs into a fresh VM");
    let mut out = String::from(
        "--!strict\n-- engine.d.luau: the Real Engine native Luau API (M1).\n\
         -- Generated by re_script::api::definitions(); do not edit.\n\
         -- Load it with: luau-lsp analyze --definitions=@engine=engine.d.luau --flag:LuauSolverV2=true\n\n",
    );
    out.push_str(&decls.join("\n\n"));
    out.push('\n');
    out
}

/// Where [`write_definitions`] puts `engine.d.luau` in a project, so editors
/// running luau-lsp and agents find it at a stable path.
pub const DEFINITIONS_PATH: &str = ".real/types/engine.d.luau";

/// Writes [`definitions`] to [`DEFINITIONS_PATH`] under `project` and returns
/// its full path. The contents depend only on the engine build; a file that
/// already holds them is left untouched, so its modification time only changes
/// with the engine.
///
/// # Errors
///
/// The directory cannot be created or the file cannot be written.
pub fn write_definitions(project: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let path = project.join(DEFINITIONS_PATH);
    let text = definitions();
    if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(path);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, text)?;
    Ok(path)
}
