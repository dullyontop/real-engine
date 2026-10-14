//! Debounced file watching for hot reload.
//!
//! [`AssetWatcher`] watches a project directory and reports changes with
//! project-relative paths. Editors (and the engine's own writers) save
//! atomically: they write a temporary file and rename it over the target, which
//! arrives as [`AssetChange::Created`] for the target, so a created path that is
//! already loaded must be treated as modified ([`AssetServer::apply_changes`]
//! does). Changes under `.real/` and `.git/`, and temporary and swap files, are
//! not reported.
//!
//! [`AssetServer::apply_changes`]: crate::AssetServer::apply_changes

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::RecursiveMode;
use notify::event::{EventKind, ModifyKind, RenameMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use re_core::{Diagnostic, Location};

use crate::{codes, fsutil};

/// One change, with paths relative to the watched root and `/` separators.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AssetChange {
    /// A file appeared (also an atomic save over an existing file).
    Created(String),
    /// A file's contents changed.
    Modified(String),
    /// A file was deleted or moved out of the project.
    Removed(String),
    /// A file was renamed or moved inside the project.
    Renamed {
        /// The old path.
        from: String,
        /// The new path.
        to: String,
    },
}

impl AssetChange {
    /// The path the change leaves behind (the new path of a rename).
    pub fn path(&self) -> &str {
        match self {
            AssetChange::Created(p) | AssetChange::Modified(p) | AssetChange::Removed(p) => p,
            AssetChange::Renamed { to, .. } => to,
        }
    }

    /// True for a Luau script under `scripts/` (script hot reload).
    pub fn is_script(&self) -> bool {
        let p = self.path();
        p.starts_with("scripts/") && p.ends_with(".luau")
    }
}

/// Watches a project directory.
pub struct AssetWatcher {
    // Dropping the debouncer stops its thread, so it lives as long as the watcher.
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    rx: mpsc::Receiver<DebounceEventResult>,
    root: PathBuf,
}

impl AssetWatcher {
    /// The debounce time used by the engine: bursts of writes within it are
    /// reported once.
    pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(150);

    /// Starts watching `root` recursively.
    ///
    /// # Errors
    ///
    /// [`codes::WATCH_FAILED`] when the directory does not exist or the
    /// operating system refuses to watch it (for example the inotify watch limit).
    pub fn new(root: &Path, debounce: Duration) -> Result<AssetWatcher, Diagnostic> {
        let failed = |e: &dyn std::fmt::Display| {
            Diagnostic::error(
                codes::WATCH_FAILED,
                format!("cannot watch `{}` for changes: {e}", fsutil::slash(root)),
            )
            .at(Location::file(fsutil::slash(root)))
        };
        // Event paths come back canonical; compare against the canonical root.
        let root = root.canonicalize().map_err(|e| failed(&e))?;
        let (tx, rx) = mpsc::channel();
        let mut debouncer = new_debouncer(debounce, None, tx).map_err(|e| failed(&e))?;
        debouncer
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| failed(&e))?;
        Ok(AssetWatcher {
            _debouncer: debouncer,
            rx,
            root,
        })
    }

    /// The watched directory (canonical).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every change debounced since the last call, without blocking. Call once
    /// per frame.
    pub fn poll(&self) -> Vec<AssetChange> {
        let mut out = Vec::new();
        for result in self.rx.try_iter() {
            match result {
                Ok(events) => {
                    for e in events {
                        if let Some(c) = self.classify(&e.event)
                            && !out.contains(&c)
                        {
                            out.push(c);
                        }
                    }
                }
                Err(errors) => errors
                    .iter()
                    .for_each(|e| tracing::warn!(error = %e, "file watcher error")),
            }
        }
        out
    }

    fn relative(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let text = fsutil::slash(rel);
        let ignored = text.is_empty()
            || text.starts_with(".real/")
            || text.starts_with(".git/")
            || text.ends_with(".tmp")
            || text.ends_with('~')
            || text.ends_with(".swp");
        (!ignored).then_some(text)
    }

    fn classify(&self, event: &notify::Event) -> Option<AssetChange> {
        let first = || self.relative(event.paths.first()?);
        Some(match event.kind {
            EventKind::Create(_) => AssetChange::Created(first()?),
            EventKind::Remove(_) => AssetChange::Removed(first()?),
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                let from = event.paths.first().and_then(|p| self.relative(p));
                let to = event.paths.get(1).and_then(|p| self.relative(p));
                match (from, to) {
                    (Some(from), Some(to)) => AssetChange::Renamed { from, to },
                    // A temporary file renamed over its target: an atomic save.
                    (None, Some(to)) => AssetChange::Created(to),
                    (Some(from), None) => AssetChange::Removed(from),
                    (None, None) => return None,
                }
            }
            // Unpaired halves of a rename (moved in or out of the watched tree).
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => AssetChange::Removed(first()?),
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => AssetChange::Created(first()?),
            EventKind::Modify(ModifyKind::Metadata(_)) => return None,
            EventKind::Modify(_) => AssetChange::Modified(first()?),
            _ => return None,
        })
    }
}
