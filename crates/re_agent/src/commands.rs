//! The commands `re_agent` registers (ARCHITECTURE §6): `script.write`
//! (edit, compile-gated), `checkpoint.restore` (edit) and `file.delete`
//! (destructive).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use re_command::{Call, CommandRegistry, EditTarget, Tier};
use re_core::{Diagnostic, Location};
use re_scene::World;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::codes;
use crate::policy::normalize;
use crate::session::Session;

/// What the agent commands share: the project root and the current session.
/// Cheap to clone; every clone sees the same session.
#[derive(Clone, Debug)]
pub struct Agent {
    project: PathBuf,
    session: Arc<Mutex<Option<Session>>>,
}

impl Agent {
    /// The agent state of `project`, with no session yet.
    pub fn new(project: &Path) -> Agent {
        Agent {
            project: project.to_path_buf(),
            session: Arc::new(Mutex::new(None)),
        }
    }

    /// The project root.
    pub fn project(&self) -> &Path {
        &self.project
    }

    fn lock(&self) -> MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts a session with a checkpoint of `world` (call it once the
    /// project is open, before serving agents). Returns the session id.
    pub fn start_session(&self, world: &World) -> Result<String, Diagnostic> {
        let session = Session::start(&self.project, world)?;
        let id = session.id().to_string();
        *self.lock() = Some(session);
        Ok(id)
    }

    /// Continues an existing session (for `real agent rollback`).
    pub fn resume_session(&self, id: &str) -> Result<(), Diagnostic> {
        let session = Session::open(&self.project, id)?;
        *self.lock() = Some(session);
        Ok(())
    }

    /// The current session's id.
    pub fn session_id(&self) -> Option<String> {
        self.lock().as_ref().map(|s| s.id().to_string())
    }

    /// Runs `f` with the session, starting one from `world` if there is none.
    fn with_session<R>(
        &self,
        world: &World,
        f: impl FnOnce(&mut Session) -> Result<R, Diagnostic>,
    ) -> Result<R, Diagnostic> {
        let mut guard = self.lock();
        if guard.is_none() {
            *guard = Some(Session::start(&self.project, world)?);
        }
        f(guard.as_mut().expect("session just started"))
    }
}

/// Arguments of `checkpoint.restore`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreArgs {
    /// The checkpoint to return to (default: the session's start). Each
    /// restore first saves the current state as a new checkpoint and returns
    /// its id, so a restore can be restored.
    #[serde(default)]
    pub checkpoint: Option<String>,
}

/// Arguments of `script.write`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptWriteArgs {
    /// The script's path, relative to the project root, ending in `.luau`
    /// (for example `scripts/game/player.luau`).
    pub path: String,
    /// The whole Luau source. It must compile; otherwise nothing is written.
    pub source: String,
}

/// Arguments of `file.delete`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteArgs {
    /// The file, relative to the project root (for example `assets/old.png`).
    pub path: String,
}

/// A path argument, normalised and kept out of `.real/` and `.git/`
/// whatever the policy (the policy checks its scopes before the handler
/// runs).
pub fn project_path(command: &str, path: &str) -> Result<String, Diagnostic> {
    let refuse = |why: &str| {
        Diagnostic::error(
            codes::OUTSIDE_SCOPE,
            format!("{command} may not write {path:?}: {why}"),
        )
        .at(Location::command(command.to_string()))
    };
    let normal = normalize(path).map_err(refuse)?;
    let first = normal.split('/').next().unwrap_or("").to_ascii_lowercase();
    if crate::policy::PROTECTED_DIRS.contains(&first.as_str()) {
        return Err(refuse("`.real/` and `.git/` are not writable by commands"));
    }
    Ok(normal)
}

/// The file `rel` (from [`project_path`]) in `agent`'s project, refused
/// (`RE8002`) if it goes through a symbolic link.
fn file_path(agent: &Agent, command: &str, rel: &str) -> Result<PathBuf, Diagnostic> {
    crate::policy::no_links(&agent.project, rel).map_err(|why| {
        Diagnostic::error(
            codes::OUTSIDE_SCOPE,
            format!("{command} may not write {rel:?}: {why}"),
        )
        .at(Location::command(command.to_string()))
        .with_hint("commands only write real files inside the project; replace the link with a file or directory")
    })
}

fn file_error(command: &str, path: &str, message: String) -> Diagnostic {
    Diagnostic::error(codes::FILE_ERROR, message)
        .at(Location::file(path.to_string()))
        .with_hint(format!("{command} works on files inside the project"))
}

fn checkpoint_restore<C: EditTarget>(
    agent: &Agent,
    target: &mut C,
    args: RestoreArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let mut guard = agent.lock();
    let Some(session) = guard.as_mut() else {
        return Err(Diagnostic::error(
            codes::NO_SESSION,
            "there is no agent session to restore",
        )
        .at(Location::command("checkpoint.restore"))
        .with_hint(
            "sessions start when an agent connects (`real mcp`); `real agent rollback <session>` \
             restores an earlier one",
        ));
    };
    let wanted = args
        .checkpoint
        .unwrap_or_else(|| session.start_checkpoint().to_string());
    let cp = session.load_checkpoint(&wanted)?;
    let files = session.file_changes(&cp)?;
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    let ops = Session::restore_ops(target.world(), &cp);
    if call.dry_run() {
        call.transact(target, format!("checkpoint.restore {wanted}"), ops)?;
        return Ok(json!({ "restores": cp.id, "files": paths }));
    }
    let saved = session.checkpoint(target.world(), &format!("before restoring {}", cp.id))?;
    call.transact(target, format!("checkpoint.restore {}", cp.id), ops)?;
    session.apply_files(&files)?;
    let hash = target.world().state_hash().to_hex().to_string();
    Ok(json!({
        "restored": cp.id,
        "saved": saved.id,
        "state_hash": hash,
        "files": paths,
    }))
}

fn script_write<C: EditTarget>(
    agent: &Agent,
    target: &mut C,
    args: ScriptWriteArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let rel = project_path("script.write", &args.path)?;
    if !rel.ends_with(".luau") {
        return Err(file_error(
            "script.write",
            &rel,
            format!("{rel} is not a Luau script (the path must end in `.luau`)"),
        ));
    }
    // Tier-1 check (ADR-0024): a script that doesn't compile is never written.
    let diagnostics = re_script::compile_check(&args.source, &rel);
    let mut errors = diagnostics.iter().filter(|d| d.is_error()).cloned();
    if let Some(mut first) = errors.next() {
        for d in errors {
            call.push_diagnostic(d);
        }
        if first.hint.is_none() {
            first =
                first.with_hint("nothing was written; fix the script and call script.write again");
        }
        return Err(first);
    }
    for d in diagnostics {
        call.push_diagnostic(d);
    }
    let existed = file_path(agent, "script.write", &rel)?.is_file();
    if call.dry_run() {
        return Ok(json!({ "would_write": rel, "bytes": args.source.len(), "replaces": existed }));
    }
    write_file(
        agent,
        target.world(),
        "script.write",
        &rel,
        args.source.as_bytes(),
    )?;
    Ok(json!({ "written": rel, "bytes": args.source.len(), "replaced": existed }))
}

fn file_delete<C: EditTarget>(
    agent: &Agent,
    target: &mut C,
    args: DeleteArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let rel = project_path("file.delete", &args.path)?;
    let path = file_path(agent, "file.delete", &rel)?;
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            return Err(file_error(
                "file.delete",
                &rel,
                format!("{rel} is not a file"),
            ));
        }
        Err(e) => {
            return Err(file_error(
                "file.delete",
                &rel,
                format!("cannot delete {rel}: {e}"),
            ));
        }
    }
    if call.dry_run() {
        return Ok(json!({ "would_delete": rel }));
    }
    agent.with_session(target.world(), |s| s.touch(&rel))?;
    std::fs::remove_file(&path)
        .map_err(|e| file_error("file.delete", &rel, format!("cannot delete {rel}: {e}")))?;
    Ok(json!({ "deleted": rel }))
}

/// Writes a project file through the session (recording the original first),
/// for file-writing agent commands.
pub fn write_file(
    agent: &Agent,
    world: &World,
    command: &str,
    rel: &str,
    bytes: &[u8],
) -> Result<(), Diagnostic> {
    let path = file_path(agent, command, rel)?;
    agent.with_session(world, |s| s.touch(rel))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            file_error(
                command,
                rel,
                format!("cannot create {}: {e}", dir.display()),
            )
        })?;
    }
    std::fs::write(&path, bytes)
        .map_err(|e| file_error(command, rel, format!("cannot write {rel}: {e}")))
}

/// Registers `script.write`, `checkpoint.restore` and `file.delete` for
/// `agent`.
pub fn register_commands<C: EditTarget + 'static>(
    registry: &mut CommandRegistry<C>,
    agent: &Agent,
) {
    let a = agent.clone();
    registry.register_typed::<ScriptWriteArgs, _>(
        "script.write",
        "Write a Luau script file (create or replace) after checking that it compiles; with compile errors nothing is written and the errors are returned. The agent session keeps the old contents.",
        Tier::Edit,
        true,
        move |target, args, call| script_write(&a, target, args, call),
    );
    let a = agent.clone();
    registry.register_typed::<RestoreArgs, _>(
        "checkpoint.restore",
        "Return the scene and every file the agent session touched to a checkpoint (default: the session start); the current state is saved first as a new checkpoint, so the restore can be undone.",
        Tier::Edit,
        false,
        move |target, args, call| checkpoint_restore(&a, target, args, call),
    );
    let a = agent.clone();
    registry.register_typed::<DeleteArgs, _>(
        "file.delete",
        "Delete a project file (destructive; the agent session's checkpoint keeps its contents).",
        Tier::Destructive,
        true,
        move |target, args, call| file_delete(&a, target, args, call),
    );
}
