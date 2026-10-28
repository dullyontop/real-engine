//! Validation tier 3: the external Luau type checker (ADR-0013).
//!
//! The engine cannot type-check in process (mlua vendors no Analysis module), so
//! [`run`] starts a checker as a child process with the generated
//! `engine.d.luau`:
//!
//! ```text
//! luau-lsp analyze --platform=standard --flag:LuauSolverV2=true \
//!     --definitions=@engine=<engine.d.luau> --formatter=gnu <files>
//! ```
//!
//! or, with [`TypesOptions::checker`] naming a `luau-analyze` patched to load
//! definitions, `luau-analyze --formatter=gnu --definitions=<file> <files>`. It
//! runs in the project directory, so the project's `.luaurc` (shared with the
//! runtime's `require`) applies. The gnu output (`file:l.c-l.c: Kind: message`)
//! becomes `RE3081` errors and `RE3082` warnings. No checker gives `RE3090`;
//! a crash or an internal error (exit code 2) gives `RE3091`. Generator
//! scripts (`scripts/generators/`) are checked in a run of their own against
//! `generator.d.luau`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use re_core::{Diagnostic, Location};

use crate::codes;

/// The checker version CI pins and the definitions are tested against.
pub const LUAU_LSP_VERSION: &str = "1.70.1";

/// How [`run`] finds and calls the checker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypesOptions {
    /// The checker program (`--checker`, `real.toml [scripts] checker`). `None`
    /// looks for `luau-lsp` on `PATH`. A file name containing `luau-analyze` is
    /// called with luau-analyze's arguments.
    pub checker: Option<PathBuf>,
    /// An `engine.d.luau` to use instead of the one generated from `re_script`.
    pub definitions: Option<PathBuf>,
    /// A `roblox.d.luau` ([`crate::roblox`]) to load as well, for a compat
    /// project.
    pub compat_definitions: Option<PathBuf>,
    /// A `generator.d.luau` to check generator scripts against instead of the
    /// one generated from `re_script`.
    pub generator_definitions: Option<PathBuf>,
}

/// The checker [`run`] would use, as an absolute path: a bare program name is
/// looked up on `PATH`, any other path is taken relative to the current
/// directory. With no checker given, `luau-lsp` on `PATH`.
pub fn find_checker(options: &TypesOptions) -> Option<PathBuf> {
    match &options.checker {
        Some(c) if c.components().count() == 1 => on_path(&c.to_string_lossy()),
        Some(c) => {
            let c = std::path::absolute(c).ok()?;
            c.is_file().then_some(c)
        }
        None => on_path("luau-lsp"),
    }
}

fn on_path(program: &str) -> Option<PathBuf> {
    let suffix = std::env::consts::EXE_SUFFIX;
    let exe = if suffix.is_empty() || program.ends_with(suffix) {
        program.to_string()
    } else {
        format!("{program}{suffix}")
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&exe))
        .find(|p| p.is_file())
}

fn not_found(options: &TypesOptions) -> Diagnostic {
    let what = match &options.checker {
        Some(c) => format!("the type checker {} was not found", c.display()),
        None => "type checker not found: luau-lsp is not on PATH".to_string(),
    };
    Diagnostic::error(codes::CHECKER_NOT_FOUND, what).with_hint(format!(
        "install luau-lsp {LUAU_LSP_VERSION} or pass --checker <PATH>"
    ))
}

/// Type-checks `files` (absolute, relative to `project`, or relative to the
/// current directory) against the
/// engine definitions and returns the checker's findings. Locations are relative
/// to `project` where possible. Never panics on checker output.
///
/// Generator scripts ([`is_generator_script`]) run in their own VM with their
/// own API, so they are checked apart, against `generator.d.luau`
/// ([`TypesOptions::generator_definitions`]).
pub fn run(project: &Path, files: &[PathBuf], options: &TypesOptions) -> Vec<Diagnostic> {
    if files.is_empty() {
        return Vec::new();
    }
    let Some(checker) = find_checker(options) else {
        return vec![not_found(options)];
    };
    let project_abs = std::path::absolute(project).unwrap_or_else(|_| project.to_path_buf());
    let rel: Vec<PathBuf> = files
        .iter()
        .map(|f| {
            // A relative path is taken relative to the project, or failing that
            // to the current directory.
            let abs = if f.is_absolute() {
                f.clone()
            } else if project_abs.join(f).is_file() {
                project_abs.join(f)
            } else {
                std::path::absolute(f).unwrap_or_else(|_| f.clone())
            };
            abs.strip_prefix(&project_abs)
                .map(Path::to_path_buf)
                .unwrap_or(abs)
        })
        .collect();
    let (generators, scripts): (Vec<PathBuf>, Vec<PathBuf>) = rel
        .into_iter()
        .partition(|f| is_generator_script(&project_abs.join(f)));

    let mut out = Vec::new();
    let groups = [
        (
            scripts,
            "engine",
            &options.definitions,
            re_script::definitions as fn() -> String,
        ),
        (
            generators,
            "generator",
            &options.generator_definitions,
            re_script::generator::definitions,
        ),
    ];
    for (files, name, given, generate) in groups {
        if files.is_empty() {
            continue;
        }
        let generated;
        let defs = match given {
            Some(d) => d.clone(),
            None => match TempDefs::write(name, &generate()) {
                Ok(t) => {
                    generated = t;
                    generated.0.clone()
                }
                Err(e) => {
                    return vec![Diagnostic::error(
                        codes::CHECKER_FAILED,
                        format!("cannot write {name}.d.luau for the type checker: {e}"),
                    )];
                }
            },
        };
        let mut defs = vec![(name, std::path::absolute(&defs).unwrap_or(defs))];
        // A compat project's scripts also see the Roblox-style API; its
        // generators do not.
        if name == "engine"
            && let Some(c) = &options.compat_definitions
        {
            defs.push((
                "roblox",
                std::path::absolute(c).unwrap_or_else(|_| c.clone()),
            ));
        }
        match check(&checker, &project_abs, &files, &defs, options) {
            Ok(found) => out.extend(found),
            Err(fatal) => return vec![fatal],
        }
    }
    // The checker does not promise an order; sort by place so reports are stable.
    out.sort_by_key(sort_key);
    out
}

/// True for a script of the generator context: one in a `scripts/generators/`
/// folder (ADR-0043 §4), which runs in its own VM with its own API.
pub fn is_generator_script(path: &Path) -> bool {
    let parts: Vec<_> = path.components().map(|c| c.as_os_str()).collect();
    parts
        .windows(2)
        .any(|w| w[0] == "scripts" && w[1] == "generators")
}

/// Runs `checker` over `files` (relative to `project`) with the definitions
/// files `defs`, each named as luau-lsp's `@name`. `Err` when the checker
/// cannot run at all.
fn check(
    checker: &Path,
    project: &Path,
    files: &[PathBuf],
    defs: &[(&str, PathBuf)],
    options: &TypesOptions,
) -> Result<Vec<Diagnostic>, Diagnostic> {
    let analyze = checker
        .file_name()
        .is_some_and(|n| n.to_string_lossy().contains("luau-analyze"));
    let mut out = Vec::new();
    // Long argument lists are split so that no command line gets too long.
    for chunk in files.chunks(200) {
        let mut cmd = Command::new(checker);
        if analyze {
            cmd.arg("--formatter=gnu");
            for (_, path) in defs {
                cmd.arg(format!("--definitions={}", path.display()));
            }
        } else {
            cmd.arg("analyze")
                .arg("--platform=standard")
                .arg("--flag:LuauSolverV2=true");
            for (name, path) in defs {
                cmd.arg(format!("--definitions=@{name}={}", path.display()));
            }
            cmd.arg("--formatter=gnu");
        }
        cmd.args(chunk).current_dir(project);
        let output = match cmd.output() {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found(options)),
            Err(e) => {
                return Err(Diagnostic::error(
                    codes::CHECKER_FAILED,
                    format!("cannot run {}: {e}", checker.display()),
                ));
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut found = parse_gnu(&stdout);
        found.extend(parse_gnu(&stderr));
        let code = output.status.code();
        let failed = match code {
            Some(0) => false,
            Some(1) => found.iter().all(|d| d.code != codes::TYPE_ERROR),
            _ => true,
        };
        if failed {
            // Temporary definitions paths differ per run; name them stably.
            let mut text = format!("{stdout}{stderr}");
            for (name, path) in defs {
                text = text.replace(&path.display().to_string(), &format!("{name}.d.luau"));
            }
            let tail: String = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(20)
                .collect::<Vec<_>>()
                .join("\n");
            out.push(
                Diagnostic::error(
                    codes::CHECKER_FAILED,
                    format!(
                        "the type checker {} failed ({})",
                        checker.display(),
                        code.map_or("killed".to_string(), |c| format!("exit code {c}"))
                    ),
                )
                .with_hint(tail),
            );
        }
        out.extend(found);
    }
    Ok(out)
}

fn sort_key(d: &Diagnostic) -> (String, u32, u32, String) {
    match &d.location {
        Some(Location::File { file, line, column }) => (
            file.clone(),
            line.unwrap_or(0),
            column.unwrap_or(0),
            d.message.clone(),
        ),
        _ => (String::new(), 0, 0, d.message.clone()),
    }
}

/// Parses gnu-formatted checker output: `file:l.c-l.c: Kind: message`. A line
/// that follows a diagnostic and is not one continues its message (the new
/// solver writes multi-line messages); `[INFO] ...` lines and anything before
/// the first diagnostic are ignored.
pub fn parse_gnu(output: &str) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    for line in output.lines() {
        if let Some(d) = parse_line(line) {
            out.push(d);
        } else if let Some(last) = out.last_mut()
            && !line.trim().is_empty()
            && !line.starts_with('[')
        {
            last.message.push('\n');
            last.message.push_str(line.trim_end());
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Diagnostic> {
    // Find `:L.C-L.C: ` after the path (the path itself may contain ':').
    for (i, _) in line.match_indices(':') {
        let rest = &line[i + 1..];
        let Some((range, after)) = rest.split_once(": ") else {
            continue;
        };
        let Some((start, end)) = range.split_once('-') else {
            continue;
        };
        let pos = |s: &str| -> Option<(u32, u32)> {
            let (l, c) = s.split_once('.')?;
            Some((l.parse().ok()?, c.parse().ok()?))
        };
        let (Some((line_no, col)), Some(_)) = (pos(start), pos(end)) else {
            continue;
        };
        let file = line[..i].trim();
        if file.is_empty() {
            return None;
        }
        let (kind, message) = after.split_once(": ").unwrap_or(("Error", after));
        let file = file.replace('\\', "/");
        let file = file.strip_prefix("./").unwrap_or(&file).to_string();
        let is_error = kind.ends_with("Error");
        let text = format!("{kind}: {}", message.trim());
        let d = if is_error {
            Diagnostic::error(codes::TYPE_ERROR, text)
        } else {
            Diagnostic::warning(codes::TYPE_WARNING, text)
        };
        return Some(d.at(Location::File {
            file,
            line: Some(line_no),
            column: Some(col),
        }));
    }
    None
}

/// `engine.d.luau` written to a temporary file, removed on drop.
struct TempDefs(PathBuf);

impl TempDefs {
    fn write(name: &str, text: &str) -> std::io::Result<TempDefs> {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "re_script_check-{}-{}-{name}.d.luau",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, text)?;
        Ok(TempDefs(path))
    }
}

impl Drop for TempDefs {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_scripts_are_the_ones_under_scripts_generators() {
        for (path, generator) in [
            ("scripts/generators/rocks.luau", true),
            ("/home/me/town/scripts/generators/lib/plan.luau", true),
            ("scripts/game/generators.luau", false),
            ("scripts/game/generators/a.luau", false),
            ("generators/rocks.luau", false),
        ] {
            assert_eq!(is_generator_script(Path::new(path)), generator, "{path}");
        }
    }

    #[test]
    fn gnu_lines_become_diagnostics() {
        let out = "[INFO] Loading definitions file: @engine - x\n\
                   scripts/a.luau:3.1-3.11: TypeError: Key 'Spawn' not found\n\
                   C:/proj/b.luau:4.7-4.9: LocalUnused: Variable 'n' is never used\n\
                   noise without a location\n";
        let d = parse_gnu(out);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].code, codes::TYPE_ERROR);
        assert_eq!(d[0].message, "TypeError: Key 'Spawn' not found");
        assert_eq!(
            d[0].location,
            Some(Location::File {
                file: "scripts/a.luau".into(),
                line: Some(3),
                column: Some(1)
            })
        );
        assert_eq!(d[1].code, codes::TYPE_WARNING);
        assert!(
            matches!(&d[1].location, Some(Location::File { file, .. }) if file == "C:/proj/b.luau")
        );
    }

    #[test]
    fn multi_line_messages_are_kept_whole() {
        let out = "a.luau:3.21-3.22: TypeError: Expected this to be\n\
                   \t'string'\n\
                   but got\n\
                   \t'number'\n\
                   [INFO] done\n\
                   a.luau:5.1-5.2: TypeError: Unknown global 'x'\n";
        let d = parse_gnu(out);
        assert_eq!(d.len(), 2);
        assert_eq!(
            d[0].message,
            "TypeError: Expected this to be\n\t'string'\nbut got\n\t'number'"
        );
        assert_eq!(d[1].message, "TypeError: Unknown global 'x'");
    }

    #[test]
    fn a_relative_checker_path_is_made_absolute() {
        // A directory under the current one, named by a relative path.
        let cwd = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&cwd).unwrap();
        let prog = dir.path().join("luau-lsp");
        std::fs::write(&prog, "").unwrap();
        let rel = prog.strip_prefix(&cwd).unwrap().to_path_buf();
        let options = TypesOptions {
            checker: Some(rel),
            ..TypesOptions::default()
        };
        assert_eq!(find_checker(&options), Some(prog));
    }

    #[test]
    fn a_missing_checker_is_re3090() {
        let options = TypesOptions {
            checker: Some(PathBuf::from("/nonexistent/luau-lsp")),
            ..TypesOptions::default()
        };
        let d = run(Path::new("."), &[PathBuf::from("a.luau")], &options);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, codes::CHECKER_NOT_FOUND);
    }
}
