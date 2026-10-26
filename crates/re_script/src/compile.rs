//! Tier-1 validation: compile a script without running it.

use std::cell::RefCell;

use mlua::Lua;
use mlua::chunk::{ChunkMode, Compiler};
use re_core::files::{Form, LayerKind, ProjectFiles};
use re_core::{Diagnostic, Location};

use crate::codes;

use crate::diag::ScriptError;

/// The engine's compiler options, native and compat alike: optimisation level 1
/// and debug level 1 (line info and function names, enough for tracebacks),
/// which are Luau's defaults, with the builtins that [`crate::portable_math`]
/// replaces disabled (ADR-0036). [`COMPILER_ID`] names these options, which
/// `real build` compiles packaged bytecode with; a development run compiles at
/// debug level 2 ([`compiler_for`]).
pub fn compiler() -> Compiler {
    Compiler::new()
        .set_optimization_level(1)
        .set_debug_level(1)
        .set_disabled_builtins(crate::portable_math::DISABLED_BUILTINS)
}

/// The id of [`compiler`]'s options, stored with packaged bytecode (ADR-0037).
/// A packaged game refuses bytecode compiled with another id, so this changes
/// whenever the options do.
pub const COMPILER_ID: &str = "o1d1-m";

/// The compiler for the scripts of a run that reads its files through `files`
/// (ADR-0045): in a development run, [`compiler`] at debug level 2, which also
/// keeps the names of locals and upvalues, so the debugger can show them; in a
/// packaged game (its files come from an archive), [`compiler`] itself, the
/// options its bytecode was built with. Debug level 2 changes nothing a script
/// can observe and costs no measurable time; the bytecode is 5% to 11% larger.
pub fn compiler_for(files: &ProjectFiles) -> Compiler {
    let packaged = files
        .layers()
        .iter()
        .any(|l| l.kind() == LayerKind::Archive);
    if packaged {
        compiler()
    } else {
        compiler().set_debug_level(2)
    }
}

/// The version of the Luau the engine links, such as `"0.740"`, stored with
/// packaged bytecode (ADR-0037). `"unknown"` when mlua was built without it,
/// so two such builds compare equal.
pub fn luau_version() -> &'static str {
    mlua::ffi::luau_version().unwrap_or("unknown")
}

/// The bytecode format version [`compiler`] writes (its output's first byte),
/// stored with packaged bytecode (ADR-0037). Computed once.
pub fn bytecode_version() -> u8 {
    static VERSION: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *VERSION.get_or_init(|| {
        let code = compiler().compile("").expect("the empty chunk compiles");
        code[0]
    })
}

/// Compiles `source` to Luau bytecode with [`compiler`], for `real build`.
/// The output is deterministic.
///
/// # Errors
///
/// The script does not compile: the diagnostics of [`compile_check`].
///
/// ```
/// let bytecode = re_script::compile_bytecode("return 1 + 2", "scripts/a.luau").unwrap();
/// assert_eq!(bytecode, re_script::compile_bytecode("return 1 + 2", "scripts/a.luau").unwrap());
/// assert!(re_script::compile_bytecode("return = 1", "scripts/b.luau").is_err());
/// ```
pub fn compile_bytecode(source: &str, chunk_name: &str) -> Result<Vec<u8>, Vec<Diagnostic>> {
    match compiler().compile(source) {
        Ok(bytecode) => Ok(bytecode),
        Err(_) => Err(compile_check(source, chunk_name)),
    }
}

/// How to load a project file of this [`Form`]: `Text` for source and `Binary`
/// for bytecode from this engine's compiler. mlua never sniffs the bytes, so a
/// `.luau` file that holds bytecode is a syntax error (ADR-0037).
///
/// # Errors
///
/// Bytecode from another compiler, or a form that is not a script.
pub fn chunk_mode(form: &Form) -> Result<ChunkMode, String> {
    match form {
        Form::Source => Ok(ChunkMode::Text),
        Form::Bytecode { compiler } if &**compiler == COMPILER_ID => Ok(ChunkMode::Binary),
        Form::Bytecode { compiler } => Err(format!(
            "the script was compiled with options `{compiler}`, but this engine runs `{COMPILER_ID}`; rebuild the package"
        )),
        other => Err(format!("a script cannot be loaded from {other:?}")),
    }
}

thread_local! {
    /// A bare VM used only to load (not run) bytecode, which validates it.
    static LOADER: RefCell<Option<Lua>> = const { RefCell::new(None) };
}

/// Compiles `source` with the engine's Luau compiler and loads the bytecode
/// (without running it). Returns no diagnostics when the script is valid. A syntax
/// error gives one `RE3001` diagnostic pointing at `chunk_name` and the line of
/// the error; callers should treat any returned error as a failure, because a
/// bytecode load failure (never seen in practice) is mapped like a runtime error.
///
/// `chunk_name` is the path shown in diagnostics, normally the script's path
/// relative to the project (`scripts/game/player.luau`). This is validation
/// tier 1 (ADR-0024): `real check`, the corpus runner and the agent's
/// `script.write` call it before a script is written or run.
///
/// ```
/// let ok = re_script::compile_check("local v = vector.create(1, 2, 3)", "scripts/a.luau");
/// assert!(ok.is_empty());
/// let bad = re_script::compile_check("local x = = 1", "scripts/b.luau");
/// assert_eq!(bad[0].code.to_string(), "RE3001");
/// ```
pub fn compile_check(source: &str, chunk_name: &str) -> Vec<Diagnostic> {
    let bytecode = match compiler().compile(source) {
        Ok(b) => b,
        Err(mlua::Error::SyntaxError { message, .. }) => {
            // The compiler reports `12: message`, without a chunk name.
            let (line, message) = match message.split_once(':') {
                Some((l, m)) if l.trim().parse::<u32>().is_ok() => {
                    (l.trim().parse::<u32>().ok(), m.trim().to_string())
                }
                _ => (None, message),
            };
            return vec![
                Diagnostic::error(codes::SYNTAX, message).at(Location::File {
                    file: chunk_name.to_string(),
                    line,
                    column: None,
                }),
            ];
        }
        Err(e) => return vec![ScriptError::from_mlua(chunk_name, &e).to_diagnostic()],
    };
    LOADER.with_borrow_mut(|lua| {
        let lua = lua.get_or_insert_with(Lua::new);
        match lua
            .load(&bytecode[..])
            .set_name(format!("@{chunk_name}"))
            .set_mode(ChunkMode::Binary)
            .into_function()
        {
            Ok(_) => Vec::new(),
            Err(e) => {
                let mut s = ScriptError::from_mlua(chunk_name, &e);
                s.chunk.get_or_insert_with(|| chunk_name.to_string());
                vec![s.to_diagnostic()]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use re_core::Location;

    #[test]
    fn compiler_id_names_the_compiler_options() {
        // The id must change with the options: compile a probe both ways.
        let probe = "local function f(a) return a * 2 end\nreturn f(21)";
        let expected = Compiler::new()
            .set_optimization_level(1)
            .set_debug_level(1)
            .set_disabled_builtins(crate::portable_math::DISABLED_BUILTINS)
            .compile(probe)
            .unwrap();
        assert_eq!(COMPILER_ID, "o1d1-m");
        assert_eq!(compile_bytecode(probe, "scripts/p.luau").unwrap(), expected);
    }

    #[test]
    fn a_development_run_keeps_local_names_and_a_packaged_game_the_package_options() {
        use re_core::files::MemLayer;
        // A constant local: at debug level 1 it is folded away, at level 2 it
        // keeps its register and its name.
        let probe = "local label = \"done\"\nprint(label)";
        let package = compiler().compile(probe).unwrap();
        let project = ProjectFiles::for_tests(MemLayer::new(LayerKind::Memory));
        let dev = compiler_for(&project).compile(probe).unwrap();
        let named = |code: &[u8]| code.windows(5).any(|w| w == b"label");
        assert!(named(&dev), "level 2 keeps the local's name");
        assert!(!named(&package), "level 1 drops it");
        let archive = ProjectFiles::for_tests(MemLayer::new(LayerKind::Archive));
        assert_eq!(compiler_for(&archive).compile(probe).unwrap(), package);
    }

    #[test]
    fn the_luau_and_bytecode_versions_are_known() {
        assert!(luau_version().starts_with("0."), "{}", luau_version());
        let v = bytecode_version();
        assert!((3..=11).contains(&v), "{v}");
        assert_eq!(compile_bytecode("return 1", "a").unwrap()[0], v);
    }

    #[test]
    fn valid_luau_compiles() {
        let src = "--!strict\nlocal n: number = 1_000_000\nlocal v = vector.create(1, 2, 3)\nreturn v * n";
        assert!(compile_check(src, "scripts/ok.luau").is_empty());
    }

    #[test]
    fn syntax_error_has_file_and_line() {
        let d = compile_check("local a = 1\nlocal x = = 1\n", "scripts/bad.luau");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, crate::codes::SYNTAX);
        assert_eq!(
            d[0].location,
            Some(Location::File {
                file: "scripts/bad.luau".into(),
                line: Some(2),
                column: None
            })
        );
        assert!(
            d[0].message.contains("Expected identifier"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn chunk_names_with_spaces_keep_the_line() {
        let d = compile_check("return +", "scripts/My Folder/a b.luau");
        assert_eq!(d.len(), 1);
        assert!(
            matches!(&d[0].location, Some(Location::File { file, line: Some(1), .. }) if file == "scripts/My Folder/a b.luau"),
            "{:?}",
            d[0].location
        );
    }
}
