//! `re_import`: brings Roblox-style projects into Real Engine (docs/M2_PLAN.md,
//! WP16), without running any of their Luau.
//!
//! Three input forms are read:
//! * **Rojo-style projects** ([`rojo`]): a `*.project.json` tree and the
//!   script, `*.model.json` and `*.meta.json` files it maps;
//! * **Studio command-bar loader scripts** ([`loader`]): a Luau file that
//!   creates instances and assigns each script's `Source`, parsed with
//!   full_moon and interpreted statically for the statement shapes it
//!   recognises. Anything else is reported, never run;
//! * **place and model files** ([`place`]): `.rbxl` and `.rbxm` (binary),
//!   `.rbxlx` and `.rbxmx` (XML), read by our own reader (ADR-0048 §4). A
//!   place's services and a model's instances are imported with the
//!   properties the class layer can set; the rest is reported (`RE9811`).
//!
//! [`import`] writes a compat project: `real.toml` with `[compat] api =
//! "roblox"`, `scenes/main.scene.json` in the compat scene conventions
//! ([`re_compat::scene`]) and the scripts under `scripts/`, one file per
//! script, named after the instance path with dots and odd characters kept.
//!
//! Importing again is idempotent. Instances are matched by path and class:
//! an existing one is adopted (its script updated in place), a missing one is
//! created, and an instance an earlier import created that the input no
//! longer declares is removed. Only instances an import created (their ids
//! start with [`IMPORT_ID_PREFIX`]) are ever removed. An import owns every
//! instance any import created in what its input covers (a loader's script
//! table: all imported scripts; a Rojo project: the whole place), so a project
//! takes one input; a model file owns only the instances named like its
//! roots under its parent. Every check runs before anything is written: an import
//! that fails to plan leaves the project as it was, and only a file system
//! error part-way through writing (`RE9808`) can leave it half-written.

pub mod apply;
pub mod codes;
pub mod loader;
pub mod names;
pub mod place;
pub mod report;
pub mod rojo;
pub mod tree;

use std::path::{Path, PathBuf};

use re_compat::ClassRegistry;
use re_core::{DiagInfo, Diagnostic, Location};

pub use apply::{
    ENTRY_SCENE, IMPORT_ID_PREFIX, PLACE_CHILDREN, PLACE_SERVICES, place_scene, project_toml,
};
pub use report::{
    Failure, FileChanges, ImportReport, InputFormat, Item, Unrecognised, display_path,
};

/// How to import.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// Plan and report, but write nothing.
    pub dry_run: bool,
    /// Keep instances an earlier import created even when the input no longer
    /// declares them.
    pub keep_stale: bool,
    /// The dotted path a model file's instances go under
    /// (`ReplicatedStorage.Maps`); `Workspace` when `None`. Refused for a
    /// place file.
    pub parent: Option<String>,
}

/// The input file and its form: a directory holding `default.project.json`
/// and any `*.project.json` file are Rojo projects; `.rbxl`, `.rbxlx`,
/// `.rbxm` and `.rbxmx` files are places and models; any other file is read
/// as a loader script.
pub fn detect(input: &Path) -> Result<(PathBuf, InputFormat), Diagnostic> {
    let unreadable = |msg: String| {
        Diagnostic::error(codes::INPUT_UNREADABLE, msg)
            .at(Location::file(input.display().to_string()))
    };
    if input.is_dir() {
        let p = input.join("default.project.json");
        if p.is_file() {
            return Ok((p, InputFormat::Rojo));
        }
        return Err(unreadable(format!(
            "{} is a directory without a default.project.json",
            input.display()
        ))
        .with_hint("pass the *.project.json file, or a loader script"));
    }
    if !input.is_file() {
        return Err(unreadable(format!("{} does not exist", input.display())));
    }
    let name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.ends_with(".project.json") {
        Ok((input.to_path_buf(), InputFormat::Rojo))
    } else if place::is_place_file(input) {
        Ok((input.to_path_buf(), InputFormat::Place))
    } else {
        Ok((input.to_path_buf(), InputFormat::Loader))
    }
}

/// Reads an input into its instance tree.
pub fn read(input: &Path) -> Result<(tree::Tree, InputFormat), Diagnostic> {
    read_with(input, &ImportOptions::default())
}

/// Reads an input into its instance tree, with the options that shape it
/// ([`ImportOptions::parent`]).
pub fn read_with(
    input: &Path,
    opts: &ImportOptions,
) -> Result<(tree::Tree, InputFormat), Diagnostic> {
    deep(
        || format!("reading {}", input.display()),
        || read_here(input, opts),
    )
}

/// The stack the import's tree walks get: they recurse once per level, and
/// a place file may nest [`place::xml::MAX_DEPTH`] levels deep. At that depth
/// a debug build needs between 4 and 8 MB (Windows gives its main thread 1 MB).
/// The stack is reserved, not committed, so the margin costs nothing.
const STACK: usize = 64 * 1024 * 1024;

/// Runs `f` on a thread with [`STACK`], so a deep tree cannot overflow the
/// caller's stack (an abort with no crash report). A panic in `f` carries on
/// in the caller.
pub(crate) fn deep<T: Send>(
    what: impl FnOnce() -> String,
    f: impl FnOnce() -> Result<T, Diagnostic> + Send,
) -> Result<T, Diagnostic> {
    std::thread::scope(|scope| {
        match std::thread::Builder::new()
            .name("re_import".into())
            .stack_size(STACK)
            .spawn_scoped(scope, f)
        {
            Ok(h) => h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)),
            Err(e) => Err(Diagnostic::error(
                codes::INPUT_UNREADABLE,
                format!("cannot start {}: {e}", what()),
            )),
        }
    })
}

fn read_here(input: &Path, opts: &ImportOptions) -> Result<(tree::Tree, InputFormat), Diagnostic> {
    let (file, format) = detect(input)?;
    if opts.parent.is_some() && format != InputFormat::Place {
        return Err(Diagnostic::error(
            codes::INPUT_UNREADABLE,
            format!(
                "{} is not a model file, so it takes no parent",
                input.display()
            ),
        )
        .with_hint("--parent is for model files (.rbxm, .rbxmx)"));
    }
    let tree = match format {
        InputFormat::Rojo => rojo::read(&file)?,
        InputFormat::Place => place::read(&file, opts.parent.as_deref())?,
        InputFormat::Loader => {
            let bytes = std::fs::read(&file).map_err(|e| {
                Diagnostic::error(
                    codes::INPUT_UNREADABLE,
                    format!("cannot read {}: {e}", file.display()),
                )
            })?;
            let text = String::from_utf8(bytes).map_err(|e| {
                Diagnostic::error(
                    codes::INPUT_UNREADABLE,
                    format!(
                        "{} is not UTF-8 text (byte {})",
                        file.display(),
                        e.utf8_error().valid_up_to()
                    ),
                )
            })?;
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            loader::read(text, &file.display().to_string())?
        }
    };
    Ok((tree, format))
}

/// Imports `input` into the project at `project` (created when it does not
/// exist or is empty). The class layer decides which classes and services
/// exist. On `Err` nothing was written.
pub fn import(
    input: &Path,
    project: &Path,
    classes: &ClassRegistry,
    opts: &ImportOptions,
) -> Result<ImportReport, Diagnostic> {
    let (tree, format) = read_with(input, opts)?;
    let proj = apply::open_project(project, classes)?;
    let plan = apply::plan(
        &tree,
        format,
        &input.display().to_string(),
        &proj,
        classes,
        !opts.keep_stale,
        opts.dry_run,
    )?;
    if !opts.dry_run {
        apply::write(project, &plan)?;
    }
    Ok(plan.report)
}

/// Creates an empty compat project at `dir` (which must not exist or be
/// empty): `real.toml` with `[compat]`, the place scene and `scripts/`.
/// Returns the files written, relative to `dir`.
pub fn new_project(
    dir: &Path,
    name: Option<&str>,
    classes: &ClassRegistry,
) -> Result<Vec<String>, Diagnostic> {
    let name = name.map(str::to_string).unwrap_or_else(|| {
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "game".into())
    });
    let scene = re_scene::scene::to_json(&place_scene(classes)?);
    let fail = |what: &str, e: std::io::Error| {
        Diagnostic::error(codes::WRITE_FAILED, format!("cannot write {what}: {e}"))
    };
    for d in ["scenes", "scripts"] {
        std::fs::create_dir_all(dir.join(d)).map_err(|e| fail(d, e))?;
    }
    let files = [
        ("real.toml", project_toml(&name)),
        (ENTRY_SCENE, scene),
        (
            ".gitignore",
            ".real/*\n!.real/agent-policy.toml\nout/\n".to_string(),
        ),
    ];
    let mut written = Vec::new();
    for (path, text) in files {
        std::fs::write(dir.join(path), text).map_err(|e| fail(path, e))?;
        written.push(path.to_string());
    }
    Ok(written)
}

/// Every diagnostic code `re_import` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::INPUT_UNREADABLE,
        title: "Import input unreadable",
        page: include_str!("../explain/RE9801.md"),
    },
    DiagInfo {
        code: codes::LOADER_SYNTAX,
        title: "Loader script does not parse",
        page: include_str!("../explain/RE9802.md"),
    },
    DiagInfo {
        code: codes::UNRECOGNISED_STATEMENT,
        title: "Loader statement not recognised",
        page: include_str!("../explain/RE9803.md"),
    },
    DiagInfo {
        code: codes::PROJECT_UNSUITABLE,
        title: "Project cannot take an import",
        page: include_str!("../explain/RE9804.md"),
    },
    DiagInfo {
        code: codes::INSTANCE_FAILED,
        title: "Instance not imported",
        page: include_str!("../explain/RE9805.md"),
    },
    DiagInfo {
        code: codes::ROJO_INVALID,
        title: "Rojo project file malformed",
        page: include_str!("../explain/RE9806.md"),
    },
    DiagInfo {
        code: codes::SCRIPT_SYNTAX,
        title: "Imported script does not compile",
        page: include_str!("../explain/RE9807.md"),
    },
    DiagInfo {
        code: codes::WRITE_FAILED,
        title: "Import write failed",
        page: include_str!("../explain/RE9808.md"),
    },
    DiagInfo {
        code: codes::PLACE_MALFORMED,
        title: "Place file malformed",
        page: include_str!("../explain/RE9809.md"),
    },
    DiagInfo {
        code: codes::PLACE_VALUE_TYPE,
        title: "Unknown value type in a place file",
        page: include_str!("../explain/RE9810.md"),
    },
    DiagInfo {
        code: codes::PLACE_UNMAPPED,
        title: "Place file property not imported",
        page: include_str!("../explain/RE9811.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 9800..=9899;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_valid() {
        let problems = re_core::validate_catalog(DIAGNOSTICS, CODE_RANGE);
        assert!(problems.is_empty(), "{problems:?}");
    }
}
