//! Project files: every runtime read of a project file goes through here
//! (ADR-0037).
//!
//! A [`ProjectFiles`] is an ordered list of read-only [`FileLayer`]s plus the
//! game's [`WritableDir`]s. The first layer that has a path answers for it:
//!
//! * a development run (`real run`, `verify`, the editor) reads the project
//!   directory through a [`DirLayer`];
//! * a packaged game reads its archive (`re_app::package`), then the folder that
//!   holds the archive;
//! * in both, an engine [`MemLayer`] first serves files the engine itself
//!   provides, such as the compat default scripts under `.real/defaults/`.
//!
//! Paths are project-relative and `/`-separated ([`ProjectPath`]). A read returns
//! the bytes and their [`Form`]: loaders pick how to decode the bytes from the
//! form and never by sniffing them, so a `.luau` file holding bytecode is a
//! syntax error, and bytecode comes only from a packaged archive.
//!
//! Only a development run writes project files (sidecars, renames, the saved
//! scene); [`ProjectFiles::project_dir`] is the gate. Everything else a run
//! writes goes to a [`WritableDir`]: the caches, saved data, crash reports and
//! command outputs. The read side is a frozen contract from WP19 PR "files";
//! [`ProjectFiles::writable`] and [`WritableDir`] have the shape agreed with
//! WP29 (ADR-0049 §6).

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Why a string is not a project path.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathError {
    /// The path is empty.
    Empty,
    /// The path starts with `/` or a drive prefix.
    Absolute,
    /// A segment is `..`.
    ParentDir,
    /// A segment is `.`.
    CurrentDir,
    /// The path holds a `\`; project paths use `/`.
    Backslash,
    /// Two `/` in a row, or a trailing `/`.
    EmptySegment,
    /// The path holds a NUL byte.
    Nul,
    /// A relative reference resolves outside the project.
    LeavesProject,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PathError::Empty => "the path is empty",
            PathError::Absolute => "paths are relative to the project, not absolute",
            PathError::ParentDir => "paths may not contain `..`",
            PathError::CurrentDir => "paths may not contain `.` segments",
            PathError::Backslash => "paths use `/`, not `\\`",
            PathError::EmptySegment => "the path has an empty segment (`//` or a trailing `/`)",
            PathError::Nul => "the path holds a NUL byte",
            PathError::LeavesProject => "the path leaves the project",
        })
    }
}

impl std::error::Error for PathError {}

fn check_segments(path: &str) -> Result<(), PathError> {
    if path.contains('\0') {
        return Err(PathError::Nul);
    }
    if path.contains('\\') {
        return Err(PathError::Backslash);
    }
    if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        return Err(PathError::Absolute);
    }
    for segment in path.split('/') {
        match segment {
            "" => return Err(PathError::EmptySegment),
            "." => return Err(PathError::CurrentDir),
            ".." => return Err(PathError::ParentDir),
            _ => {}
        }
    }
    Ok(())
}

/// A validated project-relative file path: `/`-separated, with no leading `/`,
/// drive prefix, `.` or `..` segment, empty segment, backslash or NUL.
///
/// ```
/// use re_core::files::{PathError, ProjectPath};
/// assert_eq!(ProjectPath::new("scripts/game/main.luau").unwrap().as_str(), "scripts/game/main.luau");
/// assert_eq!(ProjectPath::new("../secret"), Err(PathError::ParentDir));
/// ```
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectPath(Box<str>);

impl ProjectPath {
    /// Validates `path`.
    ///
    /// # Errors
    ///
    /// The [`PathError`] for the first rule `path` breaks.
    pub fn new(path: &str) -> Result<ProjectPath, PathError> {
        if path.is_empty() {
            return Err(PathError::Empty);
        }
        check_segments(path)?;
        Ok(ProjectPath(path.into()))
    }

    /// Resolves `rel` (a relative reference such as a glTF buffer URI) against
    /// this path's directory, collapsing `.` and `..`.
    ///
    /// # Errors
    ///
    /// [`PathError::LeavesProject`] when the result would leave the project, and
    /// the usual errors for an absolute reference, a backslash or a NUL.
    ///
    /// ```
    /// use re_core::files::ProjectPath;
    /// let gltf = ProjectPath::new("assets/props/crate.gltf").unwrap();
    /// assert_eq!(gltf.join_relative("../tex/wood.png").unwrap().as_str(), "assets/tex/wood.png");
    /// assert!(gltf.join_relative("../../../x.bin").is_err());
    /// ```
    pub fn join_relative(&self, rel: &str) -> Result<ProjectPath, PathError> {
        if rel.is_empty() {
            return Err(PathError::Empty);
        }
        if rel.contains('\0') {
            return Err(PathError::Nul);
        }
        if rel.contains('\\') {
            return Err(PathError::Backslash);
        }
        if rel.starts_with('/') || rel.as_bytes().get(1) == Some(&b':') {
            return Err(PathError::Absolute);
        }
        let mut parts: Vec<&str> = match self.parent() {
            Some(dir) => dir.split('/').collect(),
            None => Vec::new(),
        };
        for segment in rel.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return Err(PathError::LeavesProject);
                    }
                }
                s => parts.push(s),
            }
        }
        ProjectPath::new(&parts.join("/"))
    }

    /// The path as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The directory part (`None` for a file at the project root).
    pub fn parent(&self) -> Option<&str> {
        self.0.rsplit_once('/').map(|(dir, _)| dir)
    }

    /// The last segment.
    pub fn file_name(&self) -> &str {
        self.0.rsplit_once('/').map_or(&self.0, |(_, name)| name)
    }

    /// The extension of the last segment, without the dot.
    pub fn extension(&self) -> Option<&str> {
        let name = self.file_name();
        name.rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map(|(_, ext)| ext)
    }
}

impl fmt::Display for ProjectPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ProjectPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl AsRef<str> for ProjectPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A validated project-relative directory: the rules of [`ProjectPath`], except
/// that `""` is the project root.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ProjectDir(Box<str>);

impl ProjectDir {
    /// The project root.
    pub fn root() -> ProjectDir {
        ProjectDir("".into())
    }

    /// Validates `dir` (`""` for the root; a trailing `/` is allowed).
    ///
    /// # Errors
    ///
    /// The [`PathError`] for the first rule `dir` breaks.
    pub fn new(dir: &str) -> Result<ProjectDir, PathError> {
        let dir = dir.strip_suffix('/').unwrap_or(dir);
        if dir.is_empty() {
            return Ok(ProjectDir::root());
        }
        check_segments(dir)?;
        Ok(ProjectDir(dir.into()))
    }

    /// The directory as a string (`""` for the root).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True for the project root.
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// True when `path` lies in this directory or below it.
    pub fn contains(&self, path: &ProjectPath) -> bool {
        self.is_root()
            || path
                .as_str()
                .strip_prefix(&*self.0)
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// Turns an OS path below `root` into a project path, or `None` when a segment
/// is not valid UTF-8 or not a valid project path segment.
fn relative_project_path(root: &Path, path: &Path) -> Option<ProjectPath> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            _ => return None,
        }
    }
    ProjectPath::new(&parts.join("/")).ok()
}

fn os_join(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for segment in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(segment);
    }
    p
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// How the bytes of a file were stored.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Form {
    /// The file as the developer wrote it.
    Source,
    /// Luau bytecode compiled at build time with the compiler named here
    /// (`re_script::COMPILER_ID`). Only packaged archives hold bytecode.
    Bytecode {
        /// The compiler options id.
        compiler: Box<str>,
    },
    /// An importer's processed output (`re_assets`), stored at build time.
    Processed {
        /// The importer's name.
        importer: Box<str>,
        /// The importer's version.
        version: u32,
    },
}

/// Which kind of layer answered a read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum LayerKind {
    /// Files the engine provides itself.
    Engine,
    /// The project directory of a development run.
    Project,
    /// A packaged game's archive.
    Archive,
    /// The folder beside a packaged game's archive.
    Folder,
    /// An in-memory layer built by a test.
    Memory,
}

/// A file's bytes and how they were stored.
#[derive(Clone, Debug)]
pub struct FileData {
    /// The bytes.
    pub bytes: Arc<[u8]>,
    /// How they were stored.
    pub form: Form,
    /// The kind of layer they came from.
    pub layer: LayerKind,
}

/// What a path names in a layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    /// A file.
    File,
    /// A directory (a path prefix in an archive).
    Dir,
}

/// Why a read failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum FileError {
    /// No layer has the path.
    NotFound {
        /// The path asked for.
        path: ProjectPath,
        /// The layers tried, as [`FileLayer::describe`] gives them.
        tried: Vec<String>,
    },
    /// A layer has the path but could not read it.
    Io {
        /// The path asked for.
        path: ProjectPath,
        /// The layer, as [`FileLayer::describe`] gives it.
        layer: String,
        /// The error.
        error: io::Error,
    },
    /// A layer's stored copy is damaged (a packaged archive's hash check).
    Corrupt {
        /// The path asked for.
        path: ProjectPath,
        /// The layer.
        layer: String,
        /// What was wrong.
        detail: String,
    },
    /// The file is stored in another form than the caller accepts.
    WrongForm {
        /// The path asked for.
        path: ProjectPath,
        /// The form found.
        found: Form,
        /// What the caller wanted.
        wanted: &'static str,
    },
    /// A text file is not UTF-8.
    NotUtf8 {
        /// The path asked for.
        path: ProjectPath,
    },
    /// The path resolves (through a symbolic link) outside the layer's root.
    Outside {
        /// The path asked for.
        path: ProjectPath,
    },
    /// The string is not a project path.
    InvalidPath {
        /// The string given.
        path: String,
        /// Why.
        reason: PathError,
    },
}

impl FileError {
    /// The path the error is about.
    pub fn path(&self) -> &str {
        match self {
            FileError::NotFound { path, .. }
            | FileError::Io { path, .. }
            | FileError::Corrupt { path, .. }
            | FileError::WrongForm { path, .. }
            | FileError::NotUtf8 { path }
            | FileError::Outside { path } => path.as_str(),
            FileError::InvalidPath { path, .. } => path,
        }
    }

    /// True when no layer has the file.
    pub fn is_not_found(&self) -> bool {
        matches!(self, FileError::NotFound { .. })
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileError::NotFound { path, tried } => {
                write!(f, "`{path}` was not found (looked in {})", tried.join(", "))
            }
            FileError::Io { path, layer, error } => {
                write!(f, "cannot read `{path}` from {layer}: {error}")
            }
            FileError::Corrupt {
                path,
                layer,
                detail,
            } => write!(f, "`{path}` in {layer} is damaged: {detail}"),
            FileError::WrongForm {
                path,
                found,
                wanted,
            } => write!(f, "`{path}` is stored as {found:?}, but {wanted} is needed"),
            FileError::NotUtf8 { path } => write!(f, "`{path}` is not UTF-8 text"),
            FileError::Outside { path } => {
                write!(f, "`{path}` resolves outside the project through a link")
            }
            FileError::InvalidPath { path, reason } => write!(f, "`{path}`: {reason}"),
        }
    }
}

impl std::error::Error for FileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FileError::Io { error, .. } => Some(error),
            FileError::InvalidPath { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

/// One read-only source of project files.
pub trait FileLayer: Send + Sync + fmt::Debug {
    /// The kind of layer.
    fn kind(&self) -> LayerKind;

    /// The file at `path`: `Ok(None)` when this layer does not have it (the next
    /// layer is asked), `Err` for a real failure, which stops the lookup.
    ///
    /// # Errors
    ///
    /// [`FileError::Io`], [`FileError::Corrupt`] or [`FileError::Outside`].
    fn read(&self, path: &ProjectPath) -> Result<Option<FileData>, FileError>;

    /// What `path` names in this layer, if anything.
    fn entry(&self, path: &ProjectPath) -> Option<EntryKind>;

    /// Every file under `dir` (recursively) whose name ends with `suffix`,
    /// sorted.
    fn list(&self, dir: &ProjectDir, suffix: &str) -> Vec<ProjectPath>;

    /// The layer in diagnostics: "the project", "game.rpak", "the game folder".
    fn describe(&self) -> String;
}

/// A directory of loose files: the project in a development run
/// ([`LayerKind::Project`]) or the folder beside a packaged game's archive
/// ([`LayerKind::Folder`]). It serves [`Form::Source`] only.
///
/// The root is canonicalised once (on first use, so a root created after the
/// layer works). Each read canonicalises the joined path, refuses one that
/// leaves the root, and opens the canonical path it checked, so a symbolic link
/// out of the project is [`FileError::Outside`]; a link that stays inside the
/// project is followed. [`list`](FileLayer::list) follows linked files but not
/// linked directories, so a link cannot make the listing loop or alias a tree.
#[derive(Debug)]
pub struct DirLayer {
    kind: LayerKind,
    root: PathBuf,
    canonical: std::sync::OnceLock<PathBuf>,
}

/// Where a project path is in a [`DirLayer`].
enum Resolved {
    /// Inside the root, at this canonical path.
    Inside(PathBuf),
    /// Nothing there.
    Missing,
    /// Through a link, outside the root.
    Outside,
}

impl DirLayer {
    /// The project directory of a development run.
    pub fn project(root: impl Into<PathBuf>) -> DirLayer {
        DirLayer::new(LayerKind::Project, root.into())
    }

    /// The folder beside a packaged game's archive.
    pub fn folder(root: impl Into<PathBuf>) -> DirLayer {
        DirLayer::new(LayerKind::Folder, root.into())
    }

    fn new(kind: LayerKind, root: PathBuf) -> DirLayer {
        let layer = DirLayer {
            kind,
            root,
            canonical: std::sync::OnceLock::new(),
        };
        let _ = layer.canonical_root();
        layer
    }

    /// The directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn canonical_root(&self) -> io::Result<&Path> {
        if let Some(c) = self.canonical.get() {
            return Ok(c);
        }
        let c = self.root.canonicalize()?;
        Ok(self.canonical.get_or_init(|| c))
    }

    fn resolve(&self, path: &str) -> io::Result<Resolved> {
        let root = match self.canonical_root() {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Resolved::Missing),
            Err(e) => return Err(e),
        };
        match os_join(&self.root, path).canonicalize() {
            Ok(c) if c.starts_with(root) => Ok(Resolved::Inside(c)),
            Ok(_) => Ok(Resolved::Outside),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(Resolved::Missing)
            }
            Err(e) => Err(e),
        }
    }

    fn walk(&self, dir: &Path, rel: &str, suffix: &str, out: &mut Vec<ProjectPath>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                self.walk(&entry.path(), &child, suffix, out);
                continue;
            }
            // A linked file counts when it stays inside; a linked directory
            // is not followed.
            if name.ends_with(suffix)
                && let Ok(Resolved::Inside(c)) = self.resolve(&child)
                && c.is_file()
                && let Ok(path) = ProjectPath::new(&child)
            {
                out.push(path);
            }
        }
    }
}

impl FileLayer for DirLayer {
    fn kind(&self) -> LayerKind {
        self.kind
    }

    fn read(&self, path: &ProjectPath) -> Result<Option<FileData>, FileError> {
        let io_error = |error| FileError::Io {
            path: path.clone(),
            layer: self.describe(),
            error,
        };
        let os = match self.resolve(path.as_str()).map_err(io_error)? {
            Resolved::Inside(p) => p,
            Resolved::Missing => return Ok(None),
            Resolved::Outside => return Err(FileError::Outside { path: path.clone() }),
        };
        if os.is_dir() {
            return Ok(None);
        }
        match std::fs::read(&os) {
            Ok(bytes) => Ok(Some(FileData {
                bytes: bytes.into(),
                form: Form::Source,
                layer: self.kind,
            })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(error)),
        }
    }

    fn entry(&self, path: &ProjectPath) -> Option<EntryKind> {
        let Ok(Resolved::Inside(os)) = self.resolve(path.as_str()) else {
            return None;
        };
        if os.is_dir() {
            Some(EntryKind::Dir)
        } else if os.is_file() {
            Some(EntryKind::File)
        } else {
            None
        }
    }

    fn list(&self, dir: &ProjectDir, suffix: &str) -> Vec<ProjectPath> {
        let start = os_join(&self.root, dir.as_str());
        // The listed folder itself must be a real folder inside the root.
        if !dir.is_root() && !matches!(self.resolve(dir.as_str()), Ok(Resolved::Inside(_))) {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.walk(&start, dir.as_str(), suffix, &mut out);
        out.sort();
        out
    }

    fn describe(&self) -> String {
        match self.kind {
            LayerKind::Folder => format!("the game folder ({})", self.root.display()),
            _ => format!("the project ({})", self.root.display()),
        }
    }
}

/// Files held in memory: the engine's own files ([`LayerKind::Engine`]) and
/// test projects ([`LayerKind::Memory`]).
#[derive(Debug)]
pub struct MemLayer {
    kind: LayerKind,
    files: BTreeMap<ProjectPath, (Arc<[u8]>, Form)>,
}

impl MemLayer {
    /// An empty layer of this kind.
    pub fn new(kind: LayerKind) -> MemLayer {
        MemLayer {
            kind,
            files: BTreeMap::new(),
        }
    }

    /// Adds (or replaces) a file.
    pub fn insert(&mut self, path: ProjectPath, bytes: impl Into<Arc<[u8]>>, form: Form) {
        self.files.insert(path, (bytes.into(), form));
    }

    /// The number of files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// True when the layer holds no file.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl FileLayer for MemLayer {
    fn kind(&self) -> LayerKind {
        self.kind
    }

    fn read(&self, path: &ProjectPath) -> Result<Option<FileData>, FileError> {
        Ok(self.files.get(path).map(|(bytes, form)| FileData {
            bytes: bytes.clone(),
            form: form.clone(),
            layer: self.kind,
        }))
    }

    fn entry(&self, path: &ProjectPath) -> Option<EntryKind> {
        if self.files.contains_key(path) {
            return Some(EntryKind::File);
        }
        let prefix = format!("{path}/");
        self.files
            .range(ProjectPath(prefix.clone().into())..)
            .next()
            .filter(|(p, _)| p.as_str().starts_with(&prefix))
            .map(|_| EntryKind::Dir)
    }

    fn list(&self, dir: &ProjectDir, suffix: &str) -> Vec<ProjectPath> {
        self.files
            .keys()
            .filter(|p| dir.contains(p) && p.file_name().ends_with(suffix))
            .cloned()
            .collect()
    }

    fn describe(&self) -> String {
        match self.kind {
            LayerKind::Engine => "the engine's built-in files".into(),
            _ => "memory".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// What a [`WritableDir`] stores in (ADR-0049 §6).
///
/// Paths are relative, `/`-separated and already validated. `write` replaces a
/// file atomically (no reader ever sees a torn file) and creates the folders it
/// needs; it takes no lock a panicking thread could hold, because the crash hook
/// writes through it.
pub trait WritableStore: Send + Sync + fmt::Debug {
    /// The file's bytes, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Any other I/O error.
    fn read(&self, rel: &str) -> io::Result<Option<Vec<u8>>>;
    /// Writes the file, replacing it atomically.
    ///
    /// # Errors
    ///
    /// The file cannot be written.
    fn write(&self, rel: &str, bytes: &[u8]) -> io::Result<()>;
    /// Removes the file; a missing file is not an error.
    ///
    /// # Errors
    ///
    /// The file exists but cannot be removed.
    fn remove(&self, rel: &str) -> io::Result<()>;
    /// Every file under `dir` (`""` for all), recursively, relative to the
    /// store's root, sorted.
    ///
    /// # Errors
    ///
    /// The folder exists but cannot be listed.
    fn list(&self, dir: &str) -> io::Result<Vec<String>>;
    /// The store's folder on disk, when it has one.
    fn path(&self) -> Option<&Path>;
}

/// A folder on disk.
#[derive(Debug)]
struct DiskStore {
    root: PathBuf,
}

impl WritableStore for DiskStore {
    fn read(&self, rel: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(os_join(&self.root, rel)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        write_atomic(&os_join(&self.root, rel), bytes)
    }

    fn remove(&self, rel: &str) -> io::Result<()> {
        match std::fs::remove_file(os_join(&self.root, rel)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn list(&self, dir: &str) -> io::Result<Vec<String>> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> io::Result<()> {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e),
            };
            for entry in entries {
                let p = entry?.path();
                if p.is_dir() {
                    walk(root, &p, out)?;
                } else if let Some(rel) = relative_project_path(root, &p) {
                    out.push(rel.as_str().to_owned());
                }
            }
            Ok(())
        }
        let mut out = Vec::new();
        walk(&self.root, &os_join(&self.root, dir), &mut out)?;
        out.sort();
        Ok(out)
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.root)
    }
}

/// Files kept in memory (tests, and stores with nowhere to persist).
#[derive(Debug, Default)]
struct MemoryStore {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl MemoryStore {
    fn files(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl WritableStore for MemoryStore {
    fn read(&self, rel: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.files().get(rel).cloned())
    }

    fn write(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        self.files().insert(rel.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn remove(&self, rel: &str) -> io::Result<()> {
        self.files().remove(rel);
        Ok(())
    }

    fn list(&self, dir: &str) -> io::Result<Vec<String>> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        Ok(self
            .files()
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect())
    }

    fn path(&self) -> Option<&Path> {
        None
    }
}

/// Writes `bytes` to `path` through a temporary file in the same folder and a
/// rename, creating the folder first, so a reader never sees a torn file.
///
/// # Errors
///
/// The folder or the file cannot be written.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Unique per process and write, so threads writing the same file do not
    // share a temporary file.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}-{n}.tmp", crate::process_id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn invalid(rel: &str, reason: PathError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("`{rel}`: {reason}"))
}

/// A folder a run may write: the caches, saved data, crash reports and command
/// outputs ([`Writable`]). Cheap to clone. ADR-0049 §6 (WP29 PR
/// "portable").
#[derive(Clone, Debug)]
pub struct WritableDir {
    store: Arc<dyn WritableStore>,
    /// A sub-folder of the store, `""` or ending in `/`.
    prefix: String,
    /// The folder on disk, when the store has one.
    path: Option<PathBuf>,
}

impl WritableDir {
    /// A folder on disk (created on the first write).
    pub fn dir(path: impl Into<PathBuf>) -> WritableDir {
        WritableDir::from_store(Arc::new(DiskStore { root: path.into() }))
    }

    /// A folder held in memory.
    pub fn memory() -> WritableDir {
        WritableDir::from_store(Arc::new(MemoryStore::default()))
    }

    /// A folder over any store.
    pub fn from_store(store: Arc<dyn WritableStore>) -> WritableDir {
        WritableDir {
            path: store.path().map(Path::to_path_buf),
            store,
            prefix: String::new(),
        }
    }

    /// The sub-folder `name` (one or more `/`-separated segments).
    ///
    /// # Panics
    ///
    /// When `name` is not a valid relative path: sub-folder names are constants
    /// in the engine.
    pub fn sub(&self, name: &str) -> WritableDir {
        if let Err(e) = ProjectPath::new(name) {
            panic!("invalid writable sub-folder `{name}`: {e}");
        }
        WritableDir {
            store: self.store.clone(),
            prefix: format!("{}{name}/", self.prefix),
            path: self.path.as_ref().map(|p| os_join(p, name)),
        }
    }

    fn full(&self, rel: &str) -> io::Result<String> {
        ProjectPath::new(rel).map_err(|e| invalid(rel, e))?;
        Ok(format!("{}{rel}", self.prefix))
    }

    /// The file's bytes, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// `rel` is not a valid relative path, or the file cannot be read.
    pub fn read(&self, rel: &str) -> io::Result<Option<Vec<u8>>> {
        self.store.read(&self.full(rel)?)
    }

    /// Writes the file, replacing it atomically.
    ///
    /// # Errors
    ///
    /// `rel` is not a valid relative path, or the file cannot be written.
    pub fn write(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        self.store.write(&self.full(rel)?, bytes)
    }

    /// Removes the file; a missing file is not an error.
    ///
    /// # Errors
    ///
    /// `rel` is not a valid relative path, or the file cannot be removed.
    pub fn remove(&self, rel: &str) -> io::Result<()> {
        self.store.remove(&self.full(rel)?)
    }

    /// Every file under `dir` (`""` for the whole folder), relative to this
    /// folder, sorted.
    ///
    /// # Errors
    ///
    /// `dir` is not a valid relative directory, or it cannot be listed.
    pub fn list(&self, dir: &str) -> io::Result<Vec<String>> {
        let dir = ProjectDir::new(dir).map_err(|e| invalid(dir, e))?;
        let full = format!("{}{}", self.prefix, dir.as_str());
        let full = full.strip_suffix('/').unwrap_or(&full);
        Ok(self
            .store
            .list(full)?
            .into_iter()
            .filter_map(|p| p.strip_prefix(&self.prefix).map(str::to_owned))
            .collect())
    }

    /// The folder on disk, when it has one (`None` in memory and on the web).
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// The folders a run writes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Writable {
    /// Caches that are always safe to delete (`.real/cache`).
    Cache,
    /// Saved data (`.real/data`, ADR-0048).
    Data,
    /// Crash reports (`.real/crash`).
    Crash,
    /// Captures and other outputs of runtime commands (`.real/out`).
    Out,
}

/// The four [`Writable`] folders.
#[derive(Clone, Debug)]
pub struct WritableDirs {
    /// [`Writable::Cache`].
    pub cache: WritableDir,
    /// [`Writable::Data`].
    pub data: WritableDir,
    /// [`Writable::Crash`].
    pub crash: WritableDir,
    /// [`Writable::Out`].
    pub out: WritableDir,
}

impl WritableDirs {
    /// `<project>/.real/{cache,data,crash,out}`, as in a development run.
    pub fn in_project(project: &Path) -> WritableDirs {
        let real = project.join(".real");
        WritableDirs {
            cache: WritableDir::dir(real.join("cache")),
            data: WritableDir::dir(real.join("data")),
            crash: WritableDir::dir(real.join("crash")),
            out: WritableDir::dir(real.join("out")),
        }
    }

    /// Four folders in memory.
    pub fn memory() -> WritableDirs {
        WritableDirs {
            cache: WritableDir::memory(),
            data: WritableDir::memory(),
            crash: WritableDir::memory(),
            out: WritableDir::memory(),
        }
    }
}

// ---------------------------------------------------------------------------
// The project
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Inner {
    layers: Vec<Arc<dyn FileLayer>>,
    writable: WritableDirs,
    project_dir: Option<PathBuf>,
}

/// The files of the running project: layers to read, folders to write. Cheap to
/// clone (one `Arc`).
#[derive(Clone, Debug)]
pub struct ProjectFiles {
    inner: Arc<Inner>,
}

impl ProjectFiles {
    /// A development run of the project in `dir`: one [`DirLayer`], and the
    /// writable folders under `dir/.real/`.
    pub fn project(dir: impl Into<PathBuf>) -> ProjectFiles {
        let dir = dir.into();
        ProjectFiles::new(
            vec![Arc::new(DirLayer::project(dir.clone()))],
            WritableDirs::in_project(&dir),
            Some(dir),
        )
    }

    /// Any stack of layers. `project_dir` is `Some` only for a development run;
    /// it allows writing project files.
    pub fn new(
        layers: Vec<Arc<dyn FileLayer>>,
        writable: WritableDirs,
        project_dir: Option<PathBuf>,
    ) -> ProjectFiles {
        ProjectFiles {
            inner: Arc::new(Inner {
                layers,
                writable,
                project_dir,
            }),
        }
    }

    /// A project held in memory, with in-memory writable folders and no project
    /// directory: the test pattern for crates that read project files.
    pub fn for_tests(files: MemLayer) -> ProjectFiles {
        ProjectFiles::new(vec![Arc::new(files)], WritableDirs::memory(), None)
    }

    /// The same files with `layer` asked first (the engine layer).
    pub fn with_first_layer(&self, layer: Arc<dyn FileLayer>) -> ProjectFiles {
        let mut layers = vec![layer];
        layers.extend(self.inner.layers.iter().cloned());
        ProjectFiles::new(
            layers,
            self.inner.writable.clone(),
            self.inner.project_dir.clone(),
        )
    }

    /// The layers, first asked first.
    pub fn layers(&self) -> &[Arc<dyn FileLayer>] {
        &self.inner.layers
    }

    fn path(path: &str) -> Result<ProjectPath, FileError> {
        ProjectPath::new(path).map_err(|reason| FileError::InvalidPath {
            path: path.to_owned(),
            reason,
        })
    }

    /// The file at `path`, from the first layer that has it.
    ///
    /// # Errors
    ///
    /// [`FileError::InvalidPath`], [`FileError::NotFound`] (naming the layers
    /// tried), or the first layer's failure.
    pub fn read(&self, path: &str) -> Result<FileData, FileError> {
        let path = Self::path(path)?;
        for layer in &self.inner.layers {
            if let Some(data) = layer.read(&path)? {
                return Ok(data);
            }
        }
        Err(FileError::NotFound {
            path,
            tried: self.inner.layers.iter().map(|l| l.describe()).collect(),
        })
    }

    /// The file's bytes, refusing any form but [`Form::Source`].
    ///
    /// # Errors
    ///
    /// As [`read`](Self::read), plus [`FileError::WrongForm`].
    pub fn read_source(&self, path: &str) -> Result<Arc<[u8]>, FileError> {
        let data = self.read(path)?;
        match data.form {
            Form::Source => Ok(data.bytes),
            found => Err(FileError::WrongForm {
                path: Self::path(path)?,
                found,
                wanted: "the source file",
            }),
        }
    }

    /// The file as UTF-8 text ([`Form::Source`] only).
    ///
    /// # Errors
    ///
    /// As [`read_source`](Self::read_source), plus [`FileError::NotUtf8`].
    pub fn read_str(&self, path: &str) -> Result<String, FileError> {
        let bytes = self.read_source(path)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| FileError::NotUtf8 {
            path: ProjectPath(path.into()),
        })
    }

    /// What `path` names in the first layer that has it.
    pub fn entry(&self, path: &str) -> Option<EntryKind> {
        let path = ProjectPath::new(path).ok()?;
        self.inner.layers.iter().find_map(|l| l.entry(&path))
    }

    /// True when `path` is a file in some layer.
    pub fn is_file(&self, path: &str) -> bool {
        self.entry(path) == Some(EntryKind::File)
    }

    /// True when `path` is a directory in some layer.
    pub fn is_dir(&self, path: &str) -> bool {
        self.entry(path) == Some(EntryKind::Dir)
    }

    /// Every file under `dir` (`""` for the whole project, recursively) whose
    /// name ends with `suffix`, from all layers, sorted and without duplicates.
    /// An invalid `dir` lists nothing.
    pub fn list(&self, dir: &str, suffix: &str) -> Vec<ProjectPath> {
        let Ok(dir) = ProjectDir::new(dir) else {
            return Vec::new();
        };
        let mut all: Vec<ProjectPath> = self
            .inner
            .layers
            .iter()
            .flat_map(|l| l.list(&dir, suffix))
            .collect();
        all.sort();
        all.dedup();
        all
    }

    /// A folder the run may write (ADR-0049 §6).
    pub fn writable(&self, which: Writable) -> &WritableDir {
        let w = &self.inner.writable;
        match which {
            Writable::Cache => &w.cache,
            Writable::Data => &w.data,
            Writable::Crash => &w.crash,
            Writable::Out => &w.out,
        }
    }

    /// The project directory, only in a development run: the gate for writing
    /// project files (sidecars, renames, the saved scene) and for the hot-reload
    /// watcher.
    pub fn project_dir(&self) -> Option<&Path> {
        self.inner.project_dir.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ProjectPath {
        ProjectPath::new(s).unwrap()
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("re_core_files_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn project_paths_are_checked() {
        assert_eq!(ProjectPath::new(""), Err(PathError::Empty));
        assert_eq!(ProjectPath::new("/a"), Err(PathError::Absolute));
        assert_eq!(ProjectPath::new("C:/a"), Err(PathError::Absolute));
        assert_eq!(ProjectPath::new("a/../b"), Err(PathError::ParentDir));
        assert_eq!(ProjectPath::new("./a"), Err(PathError::CurrentDir));
        assert_eq!(ProjectPath::new("a\\b"), Err(PathError::Backslash));
        assert_eq!(ProjectPath::new("a//b"), Err(PathError::EmptySegment));
        assert_eq!(ProjectPath::new("a/"), Err(PathError::EmptySegment));
        assert_eq!(ProjectPath::new("a\0"), Err(PathError::Nul));
        let path = p("scenes/main.scene.ron");
        assert_eq!(path.parent(), Some("scenes"));
        assert_eq!(path.file_name(), "main.scene.ron");
        assert_eq!(path.extension(), Some("ron"));
        assert_eq!(p("real.toml").parent(), None);
        assert_eq!(p(".luaurc").extension(), None);
    }

    #[test]
    fn join_relative_collapses_and_stays_inside() {
        let base = p("assets/models/crate.gltf");
        assert_eq!(
            base.join_relative("crate.bin").unwrap(),
            p("assets/models/crate.bin")
        );
        assert_eq!(
            base.join_relative("./tex/../tex/a.png").unwrap(),
            p("assets/models/tex/a.png")
        );
        assert_eq!(base.join_relative("../../top.png").unwrap(), p("top.png"));
        assert_eq!(
            base.join_relative("../../../x"),
            Err(PathError::LeavesProject)
        );
        assert_eq!(base.join_relative("/etc/passwd"), Err(PathError::Absolute));
        assert_eq!(
            p("root.gltf").join_relative("../x"),
            Err(PathError::LeavesProject)
        );
    }

    #[test]
    fn project_dirs() {
        assert!(ProjectDir::new("").unwrap().is_root());
        assert_eq!(ProjectDir::new("scripts/").unwrap().as_str(), "scripts");
        assert_eq!(ProjectDir::new(".."), Err(PathError::ParentDir));
        let scripts = ProjectDir::new("scripts").unwrap();
        assert!(scripts.contains(&p("scripts/a.luau")));
        assert!(!scripts.contains(&p("scripts2/a.luau")));
        assert!(!scripts.contains(&p("scripts")));
    }

    #[test]
    fn layers_are_asked_in_order() {
        let mut engine = MemLayer::new(LayerKind::Engine);
        engine.insert(p(".real/defaults/a.luau"), &b"engine"[..], Form::Source);
        engine.insert(p("shared.txt"), &b"engine"[..], Form::Source);
        let mut project = MemLayer::new(LayerKind::Memory);
        project.insert(p("shared.txt"), &b"project"[..], Form::Source);
        project.insert(p("scripts/x/b.luau"), &b"b"[..], Form::Source);
        let files = ProjectFiles::for_tests(project).with_first_layer(Arc::new(engine));
        let shared = files.read("shared.txt").unwrap();
        assert_eq!(&*shared.bytes, b"engine");
        assert_eq!(shared.layer, LayerKind::Engine);
        assert_eq!(files.read_str("scripts/x/b.luau").unwrap(), "b");
        assert!(files.is_dir("scripts"));
        assert!(files.is_dir("scripts/x"));
        assert!(files.is_file("scripts/x/b.luau"));
        assert!(!files.is_file("scripts"));
        assert_eq!(files.entry("nope"), None);
        assert_eq!(
            files.list("", ".luau"),
            vec![p(".real/defaults/a.luau"), p("scripts/x/b.luau")]
        );
        assert_eq!(files.list("scripts", ""), vec![p("scripts/x/b.luau")]);
        let err = files.read("missing.txt").unwrap_err();
        assert!(err.is_not_found());
        assert!(
            err.to_string().contains("the engine's built-in files"),
            "{err}"
        );
        assert!(matches!(
            files.read("../x"),
            Err(FileError::InvalidPath { .. })
        ));
        assert!(files.project_dir().is_none());
    }

    #[test]
    fn forms_are_kept_and_checked() {
        let mut mem = MemLayer::new(LayerKind::Memory);
        mem.insert(
            p("main.luau"),
            &b"\x06bytecode"[..],
            Form::Bytecode {
                compiler: "o1d1".into(),
            },
        );
        mem.insert(p("bad.txt"), &b"\xff\xfe"[..], Form::Source);
        let files = ProjectFiles::for_tests(mem);
        assert!(matches!(
            files.read("main.luau").unwrap().form,
            Form::Bytecode { .. }
        ));
        assert!(matches!(
            files.read_source("main.luau"),
            Err(FileError::WrongForm { .. })
        ));
        assert!(matches!(
            files.read_str("bad.txt"),
            Err(FileError::NotUtf8 { .. })
        ));
    }

    #[test]
    fn dir_layer_reads_lists_and_refuses_links_out() {
        let root = tempdir("dir");
        std::fs::create_dir_all(root.join("scripts/sub")).unwrap();
        std::fs::write(root.join("real.toml"), "x").unwrap();
        std::fs::write(root.join("scripts/a.luau"), "a").unwrap();
        std::fs::write(root.join("scripts/sub/b.luau"), "b").unwrap();
        std::fs::write(root.join("scripts/notes.md"), "n").unwrap();
        let files = ProjectFiles::project(&root);
        assert_eq!(files.read_str("real.toml").unwrap(), "x");
        assert_eq!(files.read("real.toml").unwrap().layer, LayerKind::Project);
        assert!(files.is_dir("scripts/sub"));
        assert!(files.read("scripts").unwrap_err().is_not_found());
        assert_eq!(
            files.list("scripts", ".luau"),
            vec![p("scripts/a.luau"), p("scripts/sub/b.luau")]
        );
        assert_eq!(files.project_dir(), Some(root.as_path()));
        #[cfg(unix)]
        {
            let outside = tempdir("outside");
            std::fs::write(outside.join("secret"), "s").unwrap();
            std::os::unix::fs::symlink(outside.join("secret"), root.join("link")).unwrap();
            std::os::unix::fs::symlink(root.join("real.toml"), root.join("inside")).unwrap();
            assert!(matches!(files.read("link"), Err(FileError::Outside { .. })));
            assert_eq!(files.read_str("inside").unwrap(), "x");
            assert!(!files.list("", "").contains(&p("link")));
            // Linked directories are not followed, so links cannot loop.
            std::os::unix::fs::symlink(root.join("scripts"), root.join("scripts/sub/up")).unwrap();
            assert_eq!(
                files.list("", ".luau"),
                vec![p("scripts/a.luau"), p("scripts/sub/b.luau")]
            );
            let _ = std::fs::remove_dir_all(&outside);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn writable_dirs_on_disk_and_in_memory() {
        let root = tempdir("writable");
        for dir in [WritableDir::dir(&root), WritableDir::memory()] {
            let cache = dir.sub("cache").sub("assets");
            assert_eq!(cache.read("a/b.bin").unwrap(), None);
            cache.write("a/b.bin", b"one").unwrap();
            cache.write("a/b.bin", b"two").unwrap();
            cache.write("c.bin", b"c").unwrap();
            assert_eq!(cache.read("a/b.bin").unwrap().as_deref(), Some(&b"two"[..]));
            assert_eq!(cache.list("").unwrap(), vec!["a/b.bin", "c.bin"]);
            assert_eq!(cache.list("a").unwrap(), vec!["a/b.bin"]);
            assert_eq!(
                dir.list("").unwrap(),
                vec!["cache/assets/a/b.bin", "cache/assets/c.bin"]
            );
            cache.remove("a/b.bin").unwrap();
            cache.remove("a/b.bin").unwrap();
            assert_eq!(cache.list("").unwrap(), vec!["c.bin"]);
            assert!(cache.write("../x", b"").is_err());
        }
        assert_eq!(
            WritableDir::dir(&root).sub("cache").path(),
            Some(root.join("cache").as_path())
        );
        assert_eq!(WritableDir::memory().path(), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_writable_dirs() {
        let files = ProjectFiles::project("/some/game");
        assert_eq!(
            files.writable(Writable::Cache).path(),
            Some(Path::new("/some/game/.real/cache"))
        );
        assert_eq!(
            files.writable(Writable::Out).path(),
            Some(Path::new("/some/game/.real/out"))
        );
    }

    #[test]
    fn handles_are_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<ProjectFiles>();
        check::<WritableDir>();
        check::<FileError>();
    }
}
