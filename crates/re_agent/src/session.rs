//! Agent sessions and checkpoints (ADR-0020).
//!
//! A [`Session`] starts with a checkpoint of the world. Before an agent
//! command first changes a project file, the session records the file's
//! original contents (or that it did not exist), so the start checkpoint
//! covers every file the session touches. `checkpoint.restore` saves the
//! current state as a new checkpoint and then returns to the start (or to a
//! named checkpoint), so a restore can itself be restored.
//!
//! On disk, under `.real/agent/sessions/<session>/`:
//!
//! ```text
//! session.json              {"id", "start", "originals": {path: file state}}
//! checkpoints/<id>.json     a Checkpoint: the world's root subtrees and file states
//! files/<blake3>            file contents, by hash
//! ```
//!
//! The world part of a checkpoint is a list of `restore` operations, one per
//! root subtree, so returning to it is an ordinary transaction (despawn
//! every root, restore the checkpoint's) that `undo` can revert.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use re_command::Op;
use re_core::time::{SystemTime, UNIX_EPOCH};
use re_core::{Diagnostic, Location};
use re_reflect::{Doc, FieldType};
use re_scene::World;
use re_scene::hierarchy::parent;
use serde::{Deserialize, Serialize};

use crate::{codes, lock};

/// The directory of every agent session (`.real/agent/sessions`).
pub fn sessions_dir(project: &Path) -> PathBuf {
    lock::state_dir(project).join("agent").join("sessions")
}

/// The state of one project file in a checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FileState {
    /// The file did not exist.
    Absent,
    /// The file existed with these contents (stored under `files/<hash>`).
    Present {
        /// The blake3 hash of the contents, in hex.
        hash: String,
    },
}

/// A saved state of the world and of the files the session touched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The checkpoint's id (`start`, or `cp<N>`).
    pub id: String,
    /// The session it belongs to.
    pub session: String,
    /// Why it was taken.
    pub label: String,
    /// When, in milliseconds since the Unix epoch.
    pub created_ms: u64,
    /// The world's `state_hash` at that moment, in hex.
    pub state_hash: String,
    /// One `restore` operation per root subtree, in id order.
    pub roots: Vec<Op>,
    /// The touched files, by project-relative path. A touched file missing
    /// here was untouched when the checkpoint was taken, so its state then
    /// was the session's original.
    pub files: BTreeMap<String, FileState>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct SessionFile {
    id: String,
    start: String,
    #[serde(default)]
    originals: BTreeMap<String, FileState>,
    #[serde(default)]
    next: u32,
}

/// An agent session.
#[derive(Debug)]
pub struct Session {
    project: PathBuf,
    dir: PathBuf,
    file: SessionFile,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn io_error(path: &Path, action: &str, e: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::error(
        codes::CHECKPOINT_IO,
        format!("could not {action} {}: {e}", path.display()),
    )
    .at(Location::file(path.display().to_string()))
    .with_hint("check that `.real/agent/` in the project is writable")
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Diagnostic> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_error(dir, "create", e))?;
    }
    let text = serde_json::to_string_pretty(value).expect("session data serializes");
    // Write then rename, so a crash never leaves half a checkpoint.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| io_error(&tmp, "write", e))?;
    std::fs::rename(&tmp, path).map_err(|e| io_error(path, "write", e))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, Diagnostic> {
    let text = std::fs::read_to_string(path).map_err(|e| io_error(path, "read", e))?;
    serde_json::from_str(&text).map_err(|e| io_error(path, "parse", e))
}

/// The world's root subtrees as `restore` operations, in id order.
pub fn snapshot_roots(world: &World) -> Vec<Op> {
    let mut roots: Vec<(&str, re_scene::Entity)> = world
        .ids()
        .iter()
        .filter(|(_, e)| parent(&world.ecs, *e).is_none())
        .collect();
    roots.sort_by(|a, b| a.0.cmp(b.0));
    roots
        .into_iter()
        .map(|(_, e)| {
            let snapshot = world.snapshot_subtree(e);
            Op::Restore {
                entities: snapshot.entities,
                index: None,
                generated: snapshot.generated,
                fresh: snapshot.fresh,
            }
        })
        .collect()
}

/// The ids of the world's roots, in id order.
fn root_ids(world: &World) -> Vec<String> {
    let mut ids: Vec<String> = world
        .ids()
        .iter()
        .filter(|(_, e)| parent(&world.ecs, *e).is_none())
        .map(|(id, _)| id.to_string())
        .collect();
    ids.sort();
    ids
}

impl Session {
    /// Starts a new session for `project` with a start checkpoint of
    /// `world`.
    pub fn start(project: &Path, world: &World) -> Result<Session, Diagnostic> {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("s{}-{}-{n}", now_ms(), std::process::id());
        let dir = sessions_dir(project).join(&id);
        let session = Session {
            project: project.to_path_buf(),
            dir,
            file: SessionFile {
                id,
                start: "start".to_string(),
                originals: BTreeMap::new(),
                next: 1,
            },
        };
        let start = session.make_checkpoint("start".to_string(), world, "session start")?;
        session.save_checkpoint(&start)?;
        session.save()?;
        Ok(session)
    }

    /// Opens an existing session (for `real agent rollback`). An unknown id
    /// is `RE8011`.
    pub fn open(project: &Path, id: &str) -> Result<Session, Diagnostic> {
        let dir = sessions_dir(project).join(id);
        let path = dir.join("session.json");
        if id.is_empty() || id.contains(['/', '\\']) || id.starts_with('.') || !path.exists() {
            let known = list_sessions(project);
            return Err(Diagnostic::error(
                codes::UNKNOWN_CHECKPOINT,
                format!("no agent session {id:?}"),
            )
            .with_hint(if known.is_empty() {
                "this project has no agent sessions yet".to_string()
            } else {
                format!("known sessions: {}", known.join(", "))
            }));
        }
        let file: SessionFile = read_json(&path)?;
        Ok(Session {
            project: project.to_path_buf(),
            dir,
            file,
        })
    }

    /// The session's id.
    pub fn id(&self) -> &str {
        &self.file.id
    }

    /// The id of the start checkpoint.
    pub fn start_checkpoint(&self) -> &str {
        &self.file.start
    }

    /// The project-relative paths the session has touched.
    pub fn touched(&self) -> impl Iterator<Item = &str> {
        self.file.originals.keys().map(String::as_str)
    }

    fn save(&self) -> Result<(), Diagnostic> {
        write_json(&self.dir.join("session.json"), &self.file)
    }

    fn checkpoint_path(&self, id: &str) -> PathBuf {
        self.dir.join("checkpoints").join(format!("{id}.json"))
    }

    /// A project file, refused if its path goes through a symbolic link.
    fn file(&self, rel: &str) -> Result<PathBuf, Diagnostic> {
        crate::policy::no_links(&self.project, rel).map_err(|why| {
            Diagnostic::error(
                codes::OUTSIDE_SCOPE,
                format!("cannot restore {rel:?}: {why}"),
            )
            .at(Location::file(rel.to_string()))
            .with_hint("replace the link with a file or directory")
        })
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.dir.join("files").join(hash)
    }

    fn save_checkpoint(&self, cp: &Checkpoint) -> Result<(), Diagnostic> {
        write_json(&self.checkpoint_path(&cp.id), cp)
    }

    /// The current state of a project file, storing its contents.
    fn capture(&self, rel: &str) -> Result<FileState, Diagnostic> {
        let path = self.file(rel)?;
        match std::fs::read(&path) {
            Ok(bytes) => {
                let hash = blake3::hash(&bytes).to_hex().to_string();
                let blob = self.blob_path(&hash);
                if !blob.exists() {
                    let dir = blob.parent().expect("blob has a parent");
                    std::fs::create_dir_all(dir).map_err(|e| io_error(dir, "create", e))?;
                    // Write then rename: a blob that exists is always whole.
                    let tmp = dir.join(format!("{hash}.{}.tmp", std::process::id()));
                    std::fs::write(&tmp, &bytes).map_err(|e| io_error(&tmp, "write", e))?;
                    std::fs::rename(&tmp, &blob).map_err(|e| io_error(&blob, "write", e))?;
                }
                Ok(FileState::Present { hash })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileState::Absent),
            Err(e) => Err(io_error(&path, "read", e)),
        }
    }

    /// Records the original state of `rel` (a normalised project-relative
    /// path) before the session first changes it. Later calls for the same
    /// path do nothing.
    pub fn touch(&mut self, rel: &str) -> Result<(), Diagnostic> {
        if self.file.originals.contains_key(rel) {
            return Ok(());
        }
        let state = self.capture(rel)?;
        self.file.originals.insert(rel.to_string(), state);
        self.save()
    }

    fn make_checkpoint(
        &self,
        id: String,
        world: &World,
        label: &str,
    ) -> Result<Checkpoint, Diagnostic> {
        let mut files = BTreeMap::new();
        for rel in self.file.originals.keys() {
            files.insert(rel.clone(), self.capture(rel)?);
        }
        Ok(Checkpoint {
            id,
            session: self.file.id.clone(),
            label: label.to_string(),
            created_ms: now_ms(),
            state_hash: world.state_hash().to_hex().to_string(),
            roots: snapshot_roots(world),
            files,
        })
    }

    /// Saves the current state of `world` and of every touched file as a new
    /// checkpoint.
    pub fn checkpoint(&mut self, world: &World, label: &str) -> Result<Checkpoint, Diagnostic> {
        let id = format!("cp{}", self.file.next);
        let cp = self.make_checkpoint(id, world, label)?;
        self.save_checkpoint(&cp)?;
        self.file.next += 1;
        self.save()?;
        Ok(cp)
    }

    /// Loads a checkpoint of this session. An unknown id is `RE8011`.
    pub fn load_checkpoint(&self, id: &str) -> Result<Checkpoint, Diagnostic> {
        let path = self.checkpoint_path(id);
        if id.is_empty() || id.contains(['/', '\\', '.']) || !path.exists() {
            return Err(Diagnostic::error(
                codes::UNKNOWN_CHECKPOINT,
                format!("session {} has no checkpoint {id:?}", self.file.id),
            )
            .with_hint(format!(
                "the start checkpoint is {:?}; each restore saves a new one (cp1, cp2, …)",
                self.file.start
            )));
        }
        read_json(&path)
    }

    /// The transaction that returns `world` to `cp`: despawn every current
    /// root, restore the checkpoint's roots, then set the entity references
    /// that point from one root's subtree into another's (a subtree can only
    /// be restored once the entities it references exist, so those fields
    /// are restored empty and set at the end).
    pub fn restore_ops(world: &World, cp: &Checkpoint) -> Vec<Op> {
        Session::restore_roots_ops(world, &cp.roots)
    }

    /// The transaction that returns `world` to `roots` (from
    /// [`snapshot_roots`]), as [`Session::restore_ops`] does for a
    /// checkpoint.
    pub fn restore_roots_ops(world: &World, roots: &[Op]) -> Vec<Op> {
        let mut ops: Vec<Op> = root_ids(world)
            .into_iter()
            .map(|id| Op::Despawn { id })
            .collect();
        let mut fixups = Vec::new();
        for op in roots {
            let Op::Restore {
                entities,
                index,
                generated,
                fresh,
            } = op
            else {
                ops.push(op.clone());
                continue;
            };
            let inside: HashSet<&str> = entities.iter().map(|e| e.id.as_str()).collect();
            let mut entities = entities.clone();
            for doc in &mut entities {
                // Generated entities (`<source>/<key>`) keep their fields:
                // setting one afterwards would record an override.
                if doc.id.contains('/') {
                    continue;
                }
                for (component, fields) in doc.components.iter_mut() {
                    let Some(key) = world.registry.try_key(component) else {
                        continue;
                    };
                    let schema = world.registry.schema(key);
                    for (field, value) in fields.iter_mut() {
                        let is_ref = schema
                            .fields
                            .iter()
                            .any(|f| &f.name == field && f.ty == FieldType::Entity);
                        if let Doc::Str(target) = value
                            && is_ref
                            && !inside.contains(target.as_str())
                        {
                            fixups.push(Op::SetField {
                                id: doc.id.clone(),
                                component: component.clone(),
                                field: field.clone(),
                                value: std::mem::replace(value, Doc::Null),
                            });
                        }
                    }
                }
            }
            ops.push(Op::Restore {
                entities,
                index: *index,
                generated: generated.clone(),
                fresh: fresh.clone(),
            });
        }
        ops.extend(fixups);
        ops
    }

    /// The id of the session's latest checkpoint (`start` until another is
    /// taken).
    pub fn latest_checkpoint(&self) -> String {
        if self.file.next > 1 {
            format!("cp{}", self.file.next - 1)
        } else {
            self.file.start.clone()
        }
    }

    fn baseline_path(&self, checkpoint: &str, key: &str) -> PathBuf {
        self.dir
            .join("checkpoints")
            .join(format!("{checkpoint}.verify-{key}.json"))
    }

    /// The verify report stored for `checkpoint` under `key` (the verify
    /// options it ran with), if there is one.
    pub fn baseline(&self, checkpoint: &str, key: &str) -> Option<serde_json::Value> {
        let path = self.baseline_path(checkpoint, key);
        path.exists().then(|| read_json(&path).ok()).flatten()
    }

    /// Stores the verify report of `checkpoint` under `key`.
    pub fn save_baseline(
        &self,
        checkpoint: &str,
        key: &str,
        report: &serde_json::Value,
    ) -> Result<(), Diagnostic> {
        write_json(&self.baseline_path(checkpoint, key), report)
    }

    /// True when `world` and every touched file are as they were at `cp`.
    pub fn is_current(&self, cp: &Checkpoint, world: &World) -> Result<bool, Diagnostic> {
        Ok(world.state_hash().to_hex().as_str() == cp.state_hash
            && self.file_changes(cp)?.is_empty())
    }

    /// Copies the project to `dest` (an empty or missing directory; `.real/`,
    /// `.git/` and `target/` are left out) with every touched file as it was
    /// at `cp`. The scene file is copied as it is now: the caller restores the
    /// world part of `cp` into it.
    pub fn materialize(&self, cp: &Checkpoint, dest: &Path) -> Result<(), Diagnostic> {
        fn copy(from: &Path, to: &Path, top: bool) -> std::io::Result<()> {
            std::fs::create_dir_all(to)?;
            for entry in std::fs::read_dir(from)? {
                let entry = entry?;
                let name = entry.file_name();
                if top && [".real", ".git", "target"].iter().any(|n| name == *n) {
                    continue;
                }
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    copy(&entry.path(), &to.join(&name), false)?;
                } else if kind.is_file() {
                    std::fs::copy(entry.path(), to.join(&name))?;
                }
            }
            Ok(())
        }
        copy(&self.project, dest, true).map_err(|e| io_error(dest, "copy the project to", e))?;
        for (rel, original) in &self.file.originals {
            let state = cp.files.get(rel).unwrap_or(original);
            let path = dest.join(rel);
            match state {
                FileState::Absent => match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_error(&path, "remove", e)),
                },
                FileState::Present { hash } => {
                    let blob = self.blob_path(hash);
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir).map_err(|e| io_error(dir, "create", e))?;
                    }
                    std::fs::copy(&blob, &path).map_err(|e| io_error(&blob, "copy", e))?;
                }
            }
        }
        Ok(())
    }

    /// The session's directory (`.real/agent/sessions/<id>`).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file changes that return the touched files to `cp`, as
    /// `(path, state)` pairs (only those that differ from now).
    pub fn file_changes(&self, cp: &Checkpoint) -> Result<Vec<(String, FileState)>, Diagnostic> {
        let mut out = Vec::new();
        for (rel, original) in &self.file.originals {
            let target = cp.files.get(rel).unwrap_or(original);
            if &self.capture(rel)? != target {
                out.push((rel.clone(), target.clone()));
            }
        }
        Ok(out)
    }

    /// Writes (or deletes) each file to its state.
    pub fn apply_files(&self, changes: &[(String, FileState)]) -> Result<(), Diagnostic> {
        // Read and check every stored file first, so a missing or damaged
        // blob changes nothing.
        let mut staged = Vec::new();
        for (rel, state) in changes {
            let path = self.file(rel)?;
            let bytes = match state {
                FileState::Absent => None,
                FileState::Present { hash } => {
                    let blob = self.blob_path(hash);
                    let bytes = std::fs::read(&blob).map_err(|e| io_error(&blob, "read", e))?;
                    if blake3::hash(&bytes).to_hex().as_str() != hash {
                        return Err(io_error(
                            &blob,
                            "use",
                            std::io::Error::other("its contents do not match its hash"),
                        ));
                    }
                    Some(bytes)
                }
            };
            staged.push((path, bytes));
        }
        for (path, bytes) in staged {
            match bytes {
                None => match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_error(&path, "remove", e)),
                },
                Some(bytes) => {
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir).map_err(|e| io_error(dir, "create", e))?;
                    }
                    std::fs::write(&path, bytes).map_err(|e| io_error(&path, "write", e))?;
                }
            }
        }
        Ok(())
    }
}

/// The ids of the project's agent sessions, oldest first.
pub fn list_sessions(project: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(sessions_dir(project))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("session.json").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort_by_key(|id| {
        id.trim_start_matches('s')
            .split('-')
            .next()
            .and_then(|ms| ms.parse::<u64>().ok())
            .unwrap_or(0)
    });
    ids
}
