//! `project.import` for agents (docs/M2_PLAN.md, WP17): `real import` as an
//! MCP tool, a dry run unless `write` is set.
//!
//! Everything the import reads must be inside the project: the input, and
//! for a Rojo project every `$path` it maps, with no symbolic links on the
//! way or inside mapped directories, and nothing under `.real/` or `.git/`.
//! The import is planned once. A write goes through the server that holds
//! the project lock:
//!
//! 1. for an agent, every file it writes or deletes must be in the policy's
//!    scopes, and deleting needs `destructive`;
//! 2. the files are recorded in the agent session and a checkpoint is taken,
//!    so `checkpoint.restore` returns the World and the files to before;
//! 3. the files are written, and the World is brought to the imported scene
//!    as one transaction (one `undo` reverts the World; the files stay);
//! 4. if writing or loading fails, the files are put back and the World is
//!    untouched.

use std::path::Path;

use re_agent::session::{Session, snapshot_roots};
use re_agent::{Agent, Policy, codes as agent_codes};
use re_app::App;
use re_command::{Call, CommandRegistry, Origin, Tier};
use re_core::{Diagnostic, Location};
use re_import::{ImportReport, InputFormat};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

const COMMAND: &str = "project.import";

/// Arguments of `project.import`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportArgs {
    /// The input, relative to the project: a `*.project.json` file, a
    /// directory holding `default.project.json`, or a Luau loader script.
    pub input: String,
    /// Write the import into the project (default false: only report what
    /// would change; an agent whose policy is read-only previews with
    /// `dry_run: true`).
    #[serde(default)]
    pub write: bool,
    /// Keep instances an earlier import created that the input no longer
    /// declares (default false: they are removed, which needs the
    /// `destructive` tier from an agent).
    #[serde(default)]
    pub keep_stale: bool,
}

/// The result of `project.import`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ImportOutput {
    /// True when the import was written into the project.
    pub written: bool,
    /// True when every instance was imported and nothing is an error.
    pub ok: bool,
    /// After a write: the agent checkpoint taken just before it.
    /// `checkpoint.restore` with it reverts the World and the files.
    pub checkpoint: Option<String>,
    /// What the import does (or did).
    pub report: ImportReport,
}

fn refuse(code: re_core::Code, message: String, hint: &str) -> Diagnostic {
    Diagnostic::error(code, message)
        .at(Location::command(COMMAND))
        .with_hint(hint.to_string())
}

fn outside(what: &str, why: impl std::fmt::Display) -> Diagnostic {
    refuse(
        agent_codes::OUTSIDE_SCOPE,
        format!("{COMMAND} may not read {what}: {why}"),
        "everything an import reads must be inside the project; copy the source in, or have a person run `real import <INPUT>`",
    )
}

/// `rel` (relative to the project, `/`-separated, `..` allowed while it
/// stays inside) as a normalized path, refused when it leaves the project,
/// enters `.real/` or `.git/`, or passes through a link.
fn confined(project: &Path, rel: &str, what: &str) -> Result<String, Diagnostic> {
    let unified = rel.replace('\\', "/");
    if unified.starts_with('/') || (unified.len() > 1 && unified.as_bytes()[1] == b':') {
        return Err(outside(what, "it is not relative to the project"));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(outside(what, "it leaves the project"));
                }
            }
            p => parts.push(p),
        }
    }
    let normal = parts.join("/");
    if normal.is_empty() {
        return Err(outside(what, "it is the project itself"));
    }
    if re_agent::policy::PROTECTED_DIRS.contains(&parts[0]) {
        return Err(outside(
            what,
            format!("`{}/` is never readable here", parts[0]),
        ));
    }
    re_agent::policy::no_links(project, &normal).map_err(|why| outside(what, why))?;
    Ok(normal)
}

/// Refuses a symbolic link anywhere under `dir`.
fn no_links_below(dir: &Path, what: &str) -> Result<(), Diagnostic> {
    let entries = std::fs::read_dir(dir).map_err(|e| outside(what, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| outside(what, e))?;
        let kind = entry.file_type().map_err(|e| outside(what, e))?;
        if kind.is_symlink() {
            return Err(outside(
                what,
                format!("{} is a symbolic link", entry.path().display()),
            ));
        }
        if kind.is_dir() {
            no_links_below(&entry.path(), what)?;
        }
    }
    Ok(())
}

/// Every `$path` in a Rojo tree node and its children.
fn rojo_paths<'a>(node: &'a Json, out: &mut Vec<&'a str>) {
    if let Some(obj) = node.as_object() {
        for (key, value) in obj {
            if key == "$path" {
                if let Some(p) = value.as_str() {
                    out.push(p);
                }
            } else if !key.starts_with('$') {
                rojo_paths(value, out);
            }
        }
    }
}

/// The input as a path inside the project, after checking that everything
/// the import will read is inside it.
fn input_path(project: &Path, input: &str) -> Result<std::path::PathBuf, Diagnostic> {
    let rel = confined(project, input, &format!("{input:?}"))?;
    let path = project.join(&rel);
    let (file, format) = re_import::detect(&path)?;
    if format == InputFormat::Rojo {
        let file_rel = file
            .strip_prefix(project)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .map_err(|_| outside(&format!("{input:?}"), "it leaves the project"))?;
        confined(project, &file_rel, &format!("{file_rel:?}"))?;
        let text = std::fs::read_to_string(&file).map_err(|e| {
            Diagnostic::error(
                re_import::codes::INPUT_UNREADABLE,
                format!("cannot read {file_rel}: {e}"),
            )
        })?;
        // A project that does not parse is reported by the importer itself.
        if let Ok(json) = serde_json::from_str::<Json>(&text) {
            let base = file_rel.rsplit_once('/').map_or("", |(dir, _)| dir);
            let mut paths = Vec::new();
            rojo_paths(&json["tree"], &mut paths);
            for p in paths {
                let what = format!("the $path {p:?} of {file_rel}");
                let unified = p.replace('\\', "/");
                if unified.starts_with('/') || unified.get(1..2) == Some(":") {
                    return Err(outside(&what, "it is not relative to the project file"));
                }
                let joined = if base.is_empty() {
                    p.to_string()
                } else {
                    format!("{base}/{p}")
                };
                let mapped = confined(project, &joined, &what)?;
                let full = project.join(&mapped);
                if full.is_dir() {
                    no_links_below(&full, &what)?;
                }
            }
        }
    }
    Ok(path)
}

fn project_import(
    app: &mut App,
    agent: &Agent,
    args: ImportArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let project = app.project().to_path_buf();
    let input = input_path(&project, &args.input)?;
    let classes = super::import::classes()?;
    let dry = !args.write || call.dry_run();

    // Plan once: what is checked and recorded is what is written.
    let (tree, format) = re_import::read(&input)?;
    let target = re_import::apply::open_project(&project, &classes)?;
    let entry_scene = app.config().project.entry_scene.clone();
    if target.entry_scene != entry_scene {
        return Err(refuse(
            re_import::codes::PROJECT_UNSUITABLE,
            format!(
                "real.toml now names {} as the entry scene, but the open project uses {entry_scene}",
                target.entry_scene
            ),
            "restart `real mcp` (or the editor) so it opens the project as real.toml describes it",
        ));
    }
    let plan = re_import::apply::plan(
        &tree,
        format,
        &args.input,
        &target,
        &classes,
        !args.keep_stale,
        dry,
    )?;
    if dry {
        let ok = plan.report.ok();
        return Ok(json!(ImportOutput {
            written: false,
            ok,
            checkpoint: None,
            report: plan.report,
        }));
    }
    if plan.config.is_some() {
        return Err(refuse(
            re_import::codes::PROJECT_UNSUITABLE,
            "the import would rewrite real.toml".to_string(),
            "import into a compat project (`real new --template roblox`)",
        ));
    }
    let files = &plan.report.files;

    if matches!(call.origin(), Origin::Agent(_)) {
        // Read now, so a policy changed while the server runs applies.
        let policy = Policy::load(&project).unwrap_or_else(|_| Policy::read_only());
        for path in files.written.iter().chain(&files.deleted) {
            policy.check_path(COMMAND, path)?;
        }
        if !files.deleted.is_empty() && policy.max_tier < Tier::Destructive {
            return Err(refuse(
                agent_codes::TIER_REFUSED,
                format!(
                    "the import removes {} file(s) of instances the input no longer declares, which needs the destructive tier; the agent policy allows only {}",
                    files.deleted.len(),
                    policy.max_tier.as_str()
                ),
                "pass `keep_stale: true` to keep them; only a person can grant `destructive` in .real/agent-policy.toml",
            ));
        }
    }

    // Record every file before it changes, and checkpoint, so the files can
    // be put back.
    let world: &re_scene::World = app.world();
    let cp = agent.with_session(world, |s| {
        for path in files.written.iter().chain(&files.deleted) {
            s.touch(path)?;
        }
        s.checkpoint(world, &format!("before project.import {}", args.input))
    })?;
    let put_back = |app: &App, d: Diagnostic| -> Diagnostic {
        let restored = agent.with_session(app.world(), |s| {
            let changes = s.file_changes(&cp)?;
            s.apply_files(&changes)
        });
        match restored {
            Ok(()) => d,
            Err(e) => d.with_related(e).with_hint(format!(
                "the files could not all be put back; `checkpoint.restore {{\"checkpoint\": \"{}\"}}` returns the project to before the import",
                cp.id
            )),
        }
    };

    if let Err(d) = re_import::apply::write(&project, &plan) {
        return Err(put_back(app, d));
    }
    if plan.scene.is_some() {
        // Bring the World to the scene the import wrote, as one transaction.
        let ops = match App::open(&project, app.config().clone()) {
            Ok(imported) => {
                Session::restore_roots_ops(app.world(), &snapshot_roots(imported.world()))
            }
            Err(d) => return Err(put_back(app, d)),
        };
        if let Err(d) = call.transact(app, format!("import {}", args.input), ops) {
            return Err(put_back(app, d));
        }
        app.edit_frame();
    }
    let report = plan.report;
    let ok = report.ok();
    Ok(json!(ImportOutput {
        written: true,
        ok,
        checkpoint: Some(cp.id),
        report,
    }))
}

/// Registers `project.import` (edit tier) for `agent`.
pub(crate) fn register(registry: &mut CommandRegistry<App>, agent: &Agent) {
    let agent = agent.clone();
    registry.register_typed_output::<ImportArgs, ImportOutput, _>(
        COMMAND,
        "Import a Rojo-style project or a Studio loader script from inside the project into this compat project. Only reports what would change unless `write` is true. A write checkpoints first and returns the checkpoint: `undo` reverts the World, `checkpoint.restore` reverts the World and the files. Without `keep_stale` it deletes the scripts of instances the input no longer declares (destructive tier).",
        Tier::Edit,
        true,
        move |app, args, call| project_import(app, &agent, args, call),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rojo(project: &Path, tree: &str) -> Result<std::path::PathBuf, Diagnostic> {
        std::fs::create_dir_all(project.join("import/src")).unwrap();
        std::fs::write(project.join("import/src/a.luau"), "return 1\n").unwrap();
        std::fs::write(
            project.join("import/default.project.json"),
            format!(r#"{{ "name": "t", "tree": {{ "$className": "DataModel", "ServerStorage": {tree} }} }}"#),
        )
        .unwrap();
        input_path(project, "import")
    }

    fn scope() -> String {
        agent_codes::OUTSIDE_SCOPE.to_string()
    }

    fn code(r: Result<std::path::PathBuf, Diagnostic>) -> String {
        r.unwrap_err().code.to_string()
    }

    #[test]
    fn everything_an_import_reads_is_inside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p");
        std::fs::create_dir_all(&project).unwrap();
        // Inside, `..` included, is fine.
        assert!(rojo(&project, r#"{ "$path": "src" }"#).is_ok());
        assert!(rojo(&project, r#"{ "$path": "../import/src/a.luau" }"#).is_ok());
        // Out of the project, absolute, or into .git is not.
        assert_eq!(
            code(rojo(&project, r#"{ "$path": "../../../x" }"#)),
            scope()
        );
        assert_eq!(code(rojo(&project, r#"{ "$path": "/etc" }"#)), scope());
        assert_eq!(
            code(rojo(&project, r#"{ "X": { "$path": "../.git" } }"#)),
            scope()
        );
        assert_eq!(code(input_path(&project, ".git/config")), scope());
        assert_eq!(code(input_path(&project, "../x.luau")), scope());
    }

    #[cfg(unix)]
    #[test]
    fn links_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p");
        let secret = dir.path().join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        assert!(rojo(&project, r#"{ "$path": "src" }"#).is_ok());
        // A link inside a mapped directory.
        std::os::unix::fs::symlink(&secret, project.join("import/src/link")).unwrap();
        assert_eq!(code(rojo(&project, r#"{ "$path": "src" }"#)), scope());
        std::fs::remove_file(project.join("import/src/link")).unwrap();
        // A linked project file.
        let other = project.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(
            project.join("import/default.project.json"),
            other.join("default.project.json"),
        )
        .unwrap();
        assert_eq!(code(input_path(&project, "other")), scope());
    }
}
