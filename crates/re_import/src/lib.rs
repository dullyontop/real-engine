//! `re_import`: brings Roblox-style projects into Real Engine (docs/M2_PLAN.md,
//! WP16), without running any of their Luau.
//!
//! Two input forms are read:
//! * **Rojo-style projects** ([`rojo`]): a `*.project.json` tree and the
//!   script, `*.model.json` and `*.meta.json` files it maps;
//! * **Studio command-bar loader scripts** ([`loader`]): a Luau file that
//!   creates instances and assigns each script's `Source`, parsed with
//!   full_moon and interpreted statically for the statement shapes it
//!   recognises. Anything else is reported, never run.
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
//! takes one input. Every check runs before anything is written: an import
//! that fails to plan leaves the project as it was, and only a file system
//! error part-way through writing (`RE9808`) can leave it half-written.
//!
//! A general `.rbxl`/`.rbxlx` reader is M3.

pub mod apply;
pub mod codes;
pub mod loader;
pub mod names;
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
}

/// The input file and its form: a directory holding `default.project.json`
/// and any `*.project.json` file are Rojo projects; any other file is read as
/// a loader script.
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
    } else if name.ends_with(".rbxl")
        || name.ends_with(".rbxlx")
        || name.ends_with(".rbxm")
        || name.ends_with(".rbxmx")
    {
        Err(unreadable(format!(
            "{name} is a place or model file, which the importer does not read yet"
        ))
        .with_hint("export the scripts as a Rojo project, or use a loader script"))
    } else {
        Ok((input.to_path_buf(), InputFormat::Loader))
    }
}

/// Reads an input into its instance tree.
pub fn read(input: &Path) -> Result<(tree::Tree, InputFormat), Diagnostic> {
    let (file, format) = detect(input)?;
    let tree = match format {
        InputFormat::Rojo => rojo::read(&file)?,
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
    let (tree, format) = read(input)?;
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
