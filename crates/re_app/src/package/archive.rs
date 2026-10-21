//! `game.rpak`, the packaged game's archive (ADR-0037).
//!
//! Layout (every integer little-endian):
//!
//! 1. A [`HEADER_LEN`]-byte header: [`MAGIC`], `format_version: u32`
//!    ([`FORMAT_VERSION`]), `flags: u32` (0), then `index_offset`,
//!    `index_stored_size` and `index_size` as `u64`, and the blake3 hash of the
//!    stored index.
//! 2. The entries' bytes, back to back, in index order.
//! 3. The index: a [`PackageIndex`] in postcard, zstd-compressed.
//!
//! The archive is opened once and read with positional reads, so threads read
//! it concurrently and a truncated file is an error rather than a fault. Every
//! read checks the entry's hash.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use re_core::files::{
    EntryKind, FileData, FileError, FileLayer, Form, LayerKind, ProjectDir, ProjectPath,
};
use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};

use crate::codes;

/// The archive's file name, beside the game executable (in `../Resources/`
/// on macOS).
pub const ARCHIVE_NAME: &str = "game.rpak";

/// The first eight bytes. The `\r\n` and `\x1a` catch text-mode transfers.
pub const MAGIC: &[u8; 8] = b"RPAK\r\n\x1a\n";

/// The version of this layout.
pub const FORMAT_VERSION: u32 = 1;

/// The header's size in bytes.
pub const HEADER_LEN: u64 = 72;

/// The zstd level for entries and the index.
pub const ZSTD_LEVEL: i32 = 9;

/// The engine version stored in and required of archives.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The name, identifier and version of the packaged game.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInfo {
    /// `[project] name`.
    pub name: String,
    /// `[package] identifier`, also the name of the per-user folders.
    pub identifier: String,
    /// `[package] version`.
    pub version: String,
}

/// How an entry was stored, as [`Form`] is in memory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoredForm {
    /// [`Form::Source`].
    Source,
    /// [`Form::Bytecode`].
    Bytecode {
        /// The compiler options id.
        compiler: String,
    },
    /// [`Form::Processed`].
    Processed {
        /// The importer.
        importer: String,
        /// The importer's version.
        version: u32,
    },
}

impl StoredForm {
    /// The in-memory form.
    pub fn to_form(&self) -> Form {
        match self {
            StoredForm::Source => Form::Source,
            StoredForm::Bytecode { compiler } => Form::Bytecode {
                compiler: compiler.as_str().into(),
            },
            StoredForm::Processed { importer, version } => Form::Processed {
                importer: importer.as_str().into(),
                version: *version,
            },
        }
    }

    /// The stored form of `form`; `None` for a form this format does not know.
    pub fn from_form(form: &Form) -> Option<StoredForm> {
        Some(match form {
            Form::Source => StoredForm::Source,
            Form::Bytecode { compiler } => StoredForm::Bytecode {
                compiler: compiler.to_string(),
            },
            Form::Processed { importer, version } => StoredForm::Processed {
                importer: importer.to_string(),
                version: *version,
            },
            _ => return None,
        })
    }
}

/// How an entry's bytes are compressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compression {
    /// Stored as they are.
    None,
    /// One zstd frame.
    Zstd,
}

/// One file in the archive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// The project path.
    pub path: String,
    /// How it was stored.
    pub form: StoredForm,
    /// Where its bytes start, from the start of the file.
    pub offset: u64,
    /// Its size in the archive.
    pub stored_size: u64,
    /// Its size once decompressed.
    pub size: u64,
    /// How its bytes are compressed.
    pub compression: Compression,
    /// The blake3 hash of the decompressed bytes.
    pub hash: [u8; 32],
}

/// The archive's index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageIndex {
    /// The engine version that wrote it.
    pub engine: String,
    /// The Luau version of that engine.
    pub luau: String,
    /// The bytecode format version of that engine's compiler.
    pub bytecode_version: u32,
    /// The compiler options id (`re_script::COMPILER_ID`).
    pub compiler: String,
    /// The game.
    pub package: PackageInfo,
    /// The files, sorted by path.
    pub entries: Vec<IndexEntry>,
}

/// The versions an archive must match to be read by this engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeVersions {
    /// [`ENGINE_VERSION`].
    pub engine: String,
    /// `re_script::luau_version()`.
    pub luau: String,
    /// `re_script::bytecode_version()`.
    pub bytecode_version: u32,
    /// `re_script::COMPILER_ID`.
    pub compiler: String,
}

impl RuntimeVersions {
    /// This engine's versions.
    pub fn current() -> RuntimeVersions {
        RuntimeVersions {
            engine: ENGINE_VERSION.to_owned(),
            luau: re_script::luau_version().to_owned(),
            bytecode_version: u32::from(re_script::bytecode_version()),
            compiler: re_script::COMPILER_ID.to_owned(),
        }
    }

    fn of(index: &PackageIndex) -> RuntimeVersions {
        RuntimeVersions {
            engine: index.engine.clone(),
            luau: index.luau.clone(),
            bytecode_version: index.bytecode_version,
            compiler: index.compiler.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// What [`PackageWriter::write`] stored for one entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WrittenEntry {
    /// The project path.
    pub path: String,
    /// How it was stored.
    pub form: StoredForm,
    /// Its size.
    pub size: u64,
    /// Its size in the archive.
    pub stored_size: u64,
    /// How it was compressed.
    pub compression: Compression,
}

/// Builds a `game.rpak`. Entries are kept sorted by path and compressed
/// independently, so the same inputs give a byte-identical archive.
#[derive(Debug)]
pub struct PackageWriter {
    versions: RuntimeVersions,
    package: PackageInfo,
    entries: BTreeMap<ProjectPath, (Vec<u8>, StoredForm)>,
}

impl PackageWriter {
    /// An empty archive for `package`, stamped with this engine's versions.
    pub fn new(package: PackageInfo) -> PackageWriter {
        PackageWriter::with_versions(package, RuntimeVersions::current())
    }

    /// An empty archive stamped with `versions` (tests write mismatched ones).
    pub fn with_versions(package: PackageInfo, versions: RuntimeVersions) -> PackageWriter {
        PackageWriter {
            versions,
            package,
            entries: BTreeMap::new(),
        }
    }

    /// Adds a file.
    ///
    /// # Errors
    ///
    /// The path is already in the archive.
    pub fn add(
        &mut self,
        path: ProjectPath,
        bytes: Vec<u8>,
        form: StoredForm,
    ) -> Result<(), String> {
        if self.entries.contains_key(&path) {
            return Err(format!("`{path}` is added twice"));
        }
        self.entries.insert(path, (bytes, form));
        Ok(())
    }

    /// True when `path` was added.
    pub fn contains(&self, path: &ProjectPath) -> bool {
        self.entries.contains_key(path)
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing was added.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The archive's bytes.
    pub fn to_bytes(&self) -> (Vec<u8>, Vec<WrittenEntry>) {
        let mut out = vec![0u8; HEADER_LEN as usize];
        let mut index = PackageIndex {
            engine: self.versions.engine.clone(),
            luau: self.versions.luau.clone(),
            bytecode_version: self.versions.bytecode_version,
            compiler: self.versions.compiler.clone(),
            package: self.package.clone(),
            entries: Vec::with_capacity(self.entries.len()),
        };
        let mut written = Vec::with_capacity(self.entries.len());
        for (path, (bytes, form)) in &self.entries {
            let (stored, compression) = compress(bytes);
            let offset = out.len() as u64;
            out.extend_from_slice(stored.as_deref().unwrap_or(bytes));
            let stored_size = out.len() as u64 - offset;
            index.entries.push(IndexEntry {
                path: path.as_str().to_owned(),
                form: form.clone(),
                offset,
                stored_size,
                size: bytes.len() as u64,
                compression,
                hash: *blake3::hash(bytes).as_bytes(),
            });
            written.push(WrittenEntry {
                path: path.as_str().to_owned(),
                form: form.clone(),
                size: bytes.len() as u64,
                stored_size,
                compression,
            });
        }
        let raw = postcard::to_stdvec(&index).expect("the index always serializes");
        let stored = zstd::bulk::compress(&raw, ZSTD_LEVEL).expect("zstd compresses to memory");
        let index_offset = out.len() as u64;
        out.extend_from_slice(&stored);
        let mut header = Vec::with_capacity(HEADER_LEN as usize);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&index_offset.to_le_bytes());
        header.extend_from_slice(&(stored.len() as u64).to_le_bytes());
        header.extend_from_slice(&(raw.len() as u64).to_le_bytes());
        header.extend_from_slice(blake3::hash(&stored).as_bytes());
        debug_assert_eq!(header.len() as u64, HEADER_LEN);
        out[..HEADER_LEN as usize].copy_from_slice(&header);
        (out, written)
    }

    /// Writes the archive to `path` through a temporary file in the same
    /// folder and a rename, so a reader never sees half an archive.
    ///
    /// # Errors
    ///
    /// The file cannot be written.
    pub fn write(&self, path: &Path) -> io::Result<Vec<WrittenEntry>> {
        let (bytes, written) = self.to_bytes();
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(ARCHIVE_NAME);
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let result = (|| {
            let mut file = File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.map(|()| written)
    }
}

/// `bytes` compressed, when that saves at least 10%.
fn compress(bytes: &[u8]) -> (Option<Vec<u8>>, Compression) {
    if bytes.len() < 64 {
        return (None, Compression::None);
    }
    match zstd::bulk::compress(bytes, ZSTD_LEVEL) {
        Ok(c) if c.len() as u64 * 10 <= bytes.len() as u64 * 9 => (Some(c), Compression::Zstd),
        _ => (None, Compression::None),
    }
}

/// The zstd-compressed size of `bytes` at the archive's level (for `real
/// build`'s choice of stored form).
pub fn compressed_size(bytes: &[u8]) -> u64 {
    match compress(bytes) {
        (Some(c), _) => c.len() as u64,
        (None, _) => bytes.len() as u64,
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// An open `game.rpak`: a [`FileLayer`] over its entries.
#[derive(Debug)]
pub struct Archive {
    file: File,
    path: PathBuf,
    index: PackageIndex,
    /// Lower-cased paths that name exactly one entry.
    folded: HashMap<String, usize>,
    /// Case-only matches served so far: (asked, stored).
    case_matches: Mutex<Vec<(String, String)>>,
}

fn corrupt(path: &Path, detail: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::error(
        codes::PACKAGE_UNREADABLE,
        format!(
            "{} is not a readable game archive: {detail}",
            path.display()
        ),
    )
    .at(Location::file(path.display().to_string()))
    .with_hint(
        "the download or copy may be incomplete; reinstall the game, or run `real build` again",
    )
}

impl Archive {
    /// Opens the archive at `path` and checks that this engine can read it.
    ///
    /// # Errors
    ///
    /// `RE0600` for a file that is not a readable archive, `RE0602` for one
    /// written by another engine or compiler.
    pub fn open(path: &Path) -> Result<Archive, Diagnostic> {
        let archive = Archive::open_unchecked(path)?;
        archive.check_versions(&RuntimeVersions::current())?;
        Ok(archive)
    }

    /// Opens the archive without comparing its versions with this engine's
    /// (for tools that list an archive).
    ///
    /// # Errors
    ///
    /// `RE0600` for a file that is not a readable archive.
    pub fn open_unchecked(path: &Path) -> Result<Archive, Diagnostic> {
        let file = File::open(path).map_err(|e| corrupt(path, e))?;
        let len = file.metadata().map_err(|e| corrupt(path, e))?.len();
        let mut header = [0u8; HEADER_LEN as usize];
        read_at(&file, &mut header, 0)
            .map_err(|_| corrupt(path, "it is shorter than its header"))?;
        if &header[..8] != MAGIC {
            return Err(corrupt(path, "it does not start with the archive magic"));
        }
        let u32_at =
            |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("4 bytes"));
        let u64_at =
            |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().expect("8 bytes"));
        let version = u32_at(8);
        if version != FORMAT_VERSION {
            return Err(Diagnostic::error(
                codes::PACKAGE_VERSION_MISMATCH,
                format!(
                    "{} has archive format {version}; this engine reads format {FORMAT_VERSION}",
                    path.display()
                ),
            )
            .with_hint("rebuild the package with this engine's `real build`"));
        }
        let (index_offset, index_stored, index_size) = (u64_at(16), u64_at(24), u64_at(32));
        if index_offset.checked_add(index_stored) != Some(len) || index_offset < HEADER_LEN {
            return Err(corrupt(
                path,
                "its index is not where the header says (truncated?)",
            ));
        }
        let mut stored = vec![0u8; usize::try_from(index_stored).map_err(|e| corrupt(path, e))?];
        read_at(&file, &mut stored, index_offset).map_err(|e| corrupt(path, e))?;
        if blake3::hash(&stored).as_bytes()[..] != header[40..72] {
            return Err(corrupt(path, "its index does not match its hash"));
        }
        let raw = zstd::bulk::decompress(
            &stored,
            usize::try_from(index_size).map_err(|e| corrupt(path, e))?,
        )
        .map_err(|e| corrupt(path, format!("its index does not decompress: {e}")))?;
        let index: PackageIndex = postcard::from_bytes(&raw)
            .map_err(|e| corrupt(path, format!("its index does not decode: {e}")))?;
        for pair in index.entries.windows(2) {
            if pair[0].path >= pair[1].path {
                return Err(corrupt(path, "its index is not sorted"));
            }
        }
        for e in &index.entries {
            if e.offset < HEADER_LEN
                || e.offset
                    .checked_add(e.stored_size)
                    .is_none_or(|end| end > index_offset)
            {
                return Err(corrupt(
                    path,
                    format!("entry `{}` lies outside the archive", e.path),
                ));
            }
        }
        let mut folded: HashMap<String, Option<usize>> = HashMap::new();
        for (i, e) in index.entries.iter().enumerate() {
            folded
                .entry(e.path.to_ascii_lowercase())
                .and_modify(|v| *v = None)
                .or_insert(Some(i));
        }
        Ok(Archive {
            file,
            path: path.to_owned(),
            index,
            folded: folded
                .into_iter()
                .filter_map(|(k, v)| Some((k, v?)))
                .collect(),
            case_matches: Mutex::new(Vec::new()),
        })
    }

    /// Refuses an archive whose versions differ from `runtime`'s.
    ///
    /// # Errors
    ///
    /// `RE0602`, naming each difference.
    pub fn check_versions(&self, runtime: &RuntimeVersions) -> Result<(), Diagnostic> {
        let stored = RuntimeVersions::of(&self.index);
        let mut differ = Vec::new();
        if stored.engine != runtime.engine {
            differ.push(format!(
                "engine {} (this runtime {})",
                stored.engine, runtime.engine
            ));
        }
        if stored.luau != runtime.luau {
            differ.push(format!(
                "Luau {} (this runtime {})",
                stored.luau, runtime.luau
            ));
        }
        if stored.bytecode_version != runtime.bytecode_version {
            differ.push(format!(
                "bytecode version {} (this runtime {})",
                stored.bytecode_version, runtime.bytecode_version
            ));
        }
        if stored.compiler != runtime.compiler {
            differ.push(format!(
                "compiler options {} (this runtime {})",
                stored.compiler, runtime.compiler
            ));
        }
        if differ.is_empty() {
            return Ok(());
        }
        Err(Diagnostic::error(
            codes::PACKAGE_VERSION_MISMATCH,
            format!(
                "{} was built for another runtime: {}",
                self.path.display(),
                differ.join(", ")
            ),
        )
        .with_hint(
            "rebuild the package: the archive and the game executable come from one `real build`",
        ))
    }

    /// The archive's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The index.
    pub fn index(&self) -> &PackageIndex {
        &self.index
    }

    /// The bytes of entry `i`, checked against its hash.
    fn read_entry(&self, i: usize) -> Result<Vec<u8>, String> {
        let e = &self.index.entries[i];
        let mut stored = vec![0u8; usize::try_from(e.stored_size).map_err(|x| x.to_string())?];
        read_at(&self.file, &mut stored, e.offset).map_err(|x| format!("cannot read it: {x}"))?;
        let size = usize::try_from(e.size).map_err(|x| x.to_string())?;
        let bytes = match e.compression {
            Compression::None => stored,
            Compression::Zstd => zstd::bulk::decompress(&stored, size)
                .map_err(|x| format!("it does not decompress: {x}"))?,
        };
        if bytes.len() != size || blake3::hash(&bytes).as_bytes() != &e.hash {
            return Err("its contents do not match the archive's hash".to_owned());
        }
        Ok(bytes)
    }

    /// Reads every entry and checks its hash (`real verify --package`).
    ///
    /// # Errors
    ///
    /// One `RE0601` per damaged entry.
    pub fn check_all(&self) -> Result<(), Vec<Diagnostic>> {
        let problems: Vec<Diagnostic> = (0..self.index.entries.len())
            .filter_map(|i| {
                let detail = self.read_entry(i).err()?;
                let path = &self.index.entries[i].path;
                Some(
                    Diagnostic::error(
                        codes::PACKAGE_ENTRY_CORRUPT,
                        format!("`{path}` in {} is damaged: {detail}", self.describe_name()),
                    )
                    .at(Location::file(path.clone())),
                )
            })
            .collect();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    /// The entry for `path`, exactly or (recorded, for `RE0603`) by a match
    /// that ignores ASCII case.
    fn find(&self, path: &str) -> Option<usize> {
        if let Ok(i) = self
            .index
            .entries
            .binary_search_by(|e| e.path.as_str().cmp(path))
        {
            return Some(i);
        }
        let i = *self.folded.get(&path.to_ascii_lowercase())?;
        let stored = &self.index.entries[i].path;
        let mut seen = self.case_matches.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.iter().any(|(asked, _)| asked == path) {
            tracing::warn!(
                code = %codes::PACKAGE_CASE_MATCH,
                "`{path}` is stored in the package as `{stored}`; the spellings differ only by case"
            );
            seen.push((path.to_owned(), stored.clone()));
        }
        Some(i)
    }

    /// One `RE0603` warning per path that was read under a spelling that
    /// differs from the stored one only by case, since the last call.
    pub fn take_case_matches(&self) -> Vec<Diagnostic> {
        let seen =
            std::mem::take(&mut *self.case_matches.lock().unwrap_or_else(|e| e.into_inner()));
        seen.into_iter()
            .map(|(asked, stored)| {
                Diagnostic::warning(
                    codes::PACKAGE_CASE_MATCH,
                    format!("`{asked}` is stored in the package as `{stored}`; the spellings differ only by case"),
                )
                .at(Location::file(asked))
                .with_hint("use the file's exact spelling: case-sensitive file systems would not find it")
            })
            .collect()
    }

    fn describe_name(&self) -> String {
        self.path.file_name().map_or_else(
            || ARCHIVE_NAME.to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }
}

impl FileLayer for Archive {
    fn kind(&self) -> LayerKind {
        LayerKind::Archive
    }

    fn read(&self, path: &ProjectPath) -> Result<Option<FileData>, FileError> {
        let Some(i) = self.find(path.as_str()) else {
            return Ok(None);
        };
        let bytes = self.read_entry(i).map_err(|detail| FileError::Corrupt {
            path: path.clone(),
            layer: self.describe_name(),
            detail,
        })?;
        Ok(Some(FileData {
            bytes: bytes.into(),
            form: self.index.entries[i].form.to_form(),
            layer: LayerKind::Archive,
        }))
    }

    fn entry(&self, path: &ProjectPath) -> Option<EntryKind> {
        let p = path.as_str();
        if self
            .index
            .entries
            .binary_search_by(|e| e.path.as_str().cmp(p))
            .is_ok()
        {
            return Some(EntryKind::File);
        }
        let prefix = format!("{p}/");
        let at = self
            .index
            .entries
            .partition_point(|e| e.path.as_str() < prefix.as_str());
        if self
            .index
            .entries
            .get(at)
            .is_some_and(|e| e.path.starts_with(&prefix))
        {
            return Some(EntryKind::Dir);
        }
        if self.folded.contains_key(&p.to_ascii_lowercase()) {
            return self.find(p).map(|_| EntryKind::File);
        }
        None
    }

    fn list(&self, dir: &ProjectDir, suffix: &str) -> Vec<ProjectPath> {
        let prefix = if dir.is_root() {
            String::new()
        } else {
            format!("{}/", dir.as_str().trim_end_matches('/'))
        };
        let at = self
            .index
            .entries
            .partition_point(|e| e.path.as_str() < prefix.as_str());
        self.index.entries[at..]
            .iter()
            .take_while(|e| e.path.starts_with(&prefix))
            .filter(|e| e.path.ends_with(suffix))
            .filter_map(|e| ProjectPath::new(&e.path).ok())
            .collect()
    }

    fn describe(&self) -> String {
        self.describe_name()
    }
}

/// Reads exactly `buf.len()` bytes at `offset` without moving a shared cursor.
#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

/// Reads exactly `buf.len()` bytes at `offset` without moving a shared cursor.
#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Positional reads exist only on Unix and Windows.
#[cfg(not(any(unix, windows)))]
fn read_at(_file: &File, _buf: &mut [u8], _offset: u64) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "game archives are read on desktop platforms only",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ProjectPath {
        ProjectPath::new(s).unwrap()
    }

    fn info() -> PackageInfo {
        PackageInfo {
            name: "island".into(),
            identifier: "com.example.island".into(),
            version: "1.0.0".into(),
        }
    }

    fn sample() -> PackageWriter {
        let mut w = PackageWriter::new(info());
        w.add(
            p("real.toml"),
            b"[project]\nname = \"island\"\n".to_vec(),
            StoredForm::Source,
        )
        .unwrap();
        w.add(
            p("scripts/main.luau"),
            re_script::compile_bytecode("return 1", "scripts/main.luau").unwrap(),
            StoredForm::Bytecode {
                compiler: re_script::COMPILER_ID.into(),
            },
        )
        .unwrap();
        w.add(
            p("assets/Rock.glb"),
            vec![7u8; 4096],
            StoredForm::Processed {
                importer: "gltf".into(),
                version: 3,
            },
        )
        .unwrap();
        w
    }

    /// Writes `w` into a new temporary folder, which lives as long as the
    /// returned guard.
    fn written(w: &PackageWriter, name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        w.write(&path).unwrap();
        (dir, path)
    }

    #[test]
    fn entries_read_back_with_their_forms() {
        let (_dir, path) = written(&sample(), ARCHIVE_NAME);
        let a = Archive::open(&path).unwrap();
        let data = a.read(&p("scripts/main.luau")).unwrap().unwrap();
        assert!(matches!(data.form, Form::Bytecode { .. }));
        assert_eq!(data.layer, LayerKind::Archive);
        let rock = a.read(&p("assets/Rock.glb")).unwrap().unwrap();
        assert_eq!(&*rock.bytes, &[7u8; 4096][..]);
        let rock_entry = &a.index().entries[0];
        assert_eq!(rock_entry.path, "assets/Rock.glb");
        assert_eq!(rock_entry.compression, Compression::Zstd);
        assert!(a.read(&p("missing.txt")).unwrap().is_none());
        assert_eq!(a.entry(&p("assets")), Some(EntryKind::Dir));
        assert_eq!(a.entry(&p("real.toml")), Some(EntryKind::File));
        assert_eq!(a.entry(&p("asset")), None);
        assert_eq!(
            a.list(&ProjectDir::new("scripts").unwrap(), ".luau"),
            vec![p("scripts/main.luau")]
        );
        assert_eq!(a.list(&ProjectDir::root(), "").len(), 3);
    }

    #[test]
    fn the_same_inputs_give_the_same_bytes() {
        assert_eq!(sample().to_bytes().0, sample().to_bytes().0);
    }

    #[test]
    fn a_case_only_match_is_served_and_reported_once() {
        let (_dir, path) = written(&sample(), ARCHIVE_NAME);
        let a = Archive::open(&path).unwrap();
        assert!(a.read(&p("assets/rock.glb")).unwrap().is_some());
        assert!(a.read(&p("assets/rock.glb")).unwrap().is_some());
        let warnings = a.take_case_matches();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, codes::PACKAGE_CASE_MATCH);
        assert!(warnings[0].message.contains("assets/Rock.glb"));
        assert!(a.take_case_matches().is_empty());
    }

    #[test]
    fn another_runtime_is_refused() {
        let mut versions = RuntimeVersions::current();
        versions.compiler = "o2d2".into();
        let mut w = PackageWriter::with_versions(info(), versions);
        w.add(p("real.toml"), b"x".to_vec(), StoredForm::Source)
            .unwrap();
        let (_dir, path) = written(&w, ARCHIVE_NAME);
        let err = Archive::open(&path).unwrap_err();
        assert_eq!(err.code, codes::PACKAGE_VERSION_MISMATCH);
        assert!(err.message.contains("o2d2"), "{}", err.message);
    }

    #[test]
    fn truncation_and_damage_are_errors() {
        let (_dir, path) = written(&sample(), ARCHIVE_NAME);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 10]).unwrap();
        assert_eq!(
            Archive::open(&path).unwrap_err().code,
            codes::PACKAGE_UNREADABLE
        );
        std::fs::write(&path, b"not an archive at all").unwrap();
        assert_eq!(
            Archive::open(&path).unwrap_err().code,
            codes::PACKAGE_UNREADABLE
        );
        // Flip a byte inside the first entry's data.
        let mut damaged = bytes.clone();
        damaged[HEADER_LEN as usize + 3] ^= 0xff;
        std::fs::write(&path, &damaged).unwrap();
        let a = Archive::open(&path).unwrap();
        assert!(matches!(
            a.read(&p("assets/Rock.glb")),
            Err(FileError::Corrupt { .. })
        ));
        let problems = a.check_all().unwrap_err();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].code, codes::PACKAGE_ENTRY_CORRUPT);
    }

    #[test]
    fn a_path_is_added_once() {
        let mut w = sample();
        assert!(w.add(p("real.toml"), vec![], StoredForm::Source).is_err());
    }
}
