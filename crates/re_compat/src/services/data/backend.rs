//! Where the data services keep their files (ADR-0048): a directory
//! (`.real/data/` in `real run`) or memory (a session seeded from
//! `data/seed/`). A browser build adds its own backend behind
//! [`DataBackend`] (ADR-0049).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// The files of the data services: paths are relative, `/`-separated, and
/// made of [`encode`]d components.
pub trait DataBackend {
    /// The file at `path`, or `None` when there is none.
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>>;
    /// Replaces the file at `path` as one step: a failed write leaves the
    /// previous version.
    fn write(&mut self, path: &str, bytes: &[u8]) -> io::Result<()>;
    /// Moves an unreadable file at `path` aside to `path.bad`, so it is kept
    /// and never overwritten.
    fn set_aside(&mut self, path: &str) -> io::Result<()>;
    /// The names of the files and directories directly under `dir`, sorted.
    fn list(&self, dir: &str) -> io::Result<Vec<String>>;
    /// True when writes reach disk (a later run sees them).
    fn persists(&self) -> bool;
}

/// The longest [`encode`]d name, in bytes, so a file name (with `.json`,
/// `.tmp` or `.bad.N` added) stays well under the usual 255-byte limit.
pub const MAX_ENCODED: usize = 200;

/// One path component for a store, scope or structure name. The result
/// starts with `_` (so no name becomes a reserved device name such as `con`
/// on Windows, or `.` and `..`), keeps `a`–`z`, `0`–`9`, `-` and `_`, writes
/// an uppercase letter as `^` and its lowercase (so names that differ only by
/// case stay apart on case-insensitive file systems) and every other byte as
/// `%XX`.
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 1);
    out.push('_');
    for b in name.bytes() {
        match b {
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => out.push(b as char),
            b'A'..=b'Z' => {
                out.push('^');
                out.push(b.to_ascii_lowercase() as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The name an [`encode`]d component stands for.
pub fn decode(component: &str) -> Option<String> {
    let s = component.strip_prefix('_')?;
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'^' => {
                let c = *b.get(i + 1)?;
                if !c.is_ascii_lowercase() {
                    return None;
                }
                out.push(c.to_ascii_uppercase());
                i += 2;
            }
            b'%' => {
                let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Files in a directory.
pub struct DirBackend {
    root: PathBuf,
}

impl DirBackend {
    /// The files under `root`, which is created on the first write.
    pub fn new(root: impl Into<PathBuf>) -> DirBackend {
        DirBackend { root: root.into() }
    }

    fn path(&self, rel: &str) -> PathBuf {
        let mut p = self.root.clone();
        for c in rel.split('/').filter(|c| !c.is_empty()) {
            p.push(c);
        }
        p
    }
}

impl DataBackend for DirBackend {
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(path)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write(&mut self, path: &str, bytes: &[u8]) -> io::Result<()> {
        let file = self.path(path);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)?;
        // Rename replaces the target in one step on all three platforms.
        std::fs::rename(&tmp, &file)
    }

    fn set_aside(&mut self, path: &str) -> io::Result<()> {
        // An earlier `.bad` is kept: the next free `.bad.N` takes this one.
        let file = self.path(path);
        let mut n = 0;
        loop {
            let mut bad = file.clone().into_os_string();
            bad.push(if n == 0 {
                ".bad".to_string()
            } else {
                format!(".bad.{n}")
            });
            let bad = PathBuf::from(bad);
            if !bad.exists() {
                return std::fs::rename(&file, bad);
            }
            n += 1;
        }
    }

    fn list(&self, dir: &str) -> io::Result<Vec<String>> {
        list_dir(&self.path(dir))
    }

    fn persists(&self) -> bool {
        true
    }
}

fn list_dir(dir: &Path) -> io::Result<Vec<String>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in rd {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".tmp") && !name.contains(".bad") {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

/// Files in memory, for a session that keeps nothing.
#[derive(Default)]
pub struct MemoryBackend {
    files: BTreeMap<String, Vec<u8>>,
}

impl MemoryBackend {
    /// An empty session.
    pub fn new() -> MemoryBackend {
        MemoryBackend::default()
    }

    /// A session that starts from a copy of the files under `seed` (the
    /// project's `data/seed/`); a missing directory gives an empty session.
    pub fn seeded(seed: &Path) -> io::Result<MemoryBackend> {
        let mut m = MemoryBackend::new();
        copy_tree(seed, seed, &mut m.files)?;
        Ok(m)
    }
}

fn copy_tree(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> io::Result<()> {
    for name in list_dir(dir)? {
        let p = dir.join(&name);
        if p.is_dir() {
            copy_tree(root, &p, out)?;
        } else {
            let rel = p
                .strip_prefix(root)
                .expect("under the root")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.insert(rel, std::fs::read(&p)?);
        }
    }
    Ok(())
}

impl DataBackend for MemoryBackend {
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.files.get(path).cloned())
    }

    fn write(&mut self, path: &str, bytes: &[u8]) -> io::Result<()> {
        self.files.insert(path.to_string(), bytes.to_vec());
        Ok(())
    }

    fn set_aside(&mut self, path: &str) -> io::Result<()> {
        self.files.remove(path);
        Ok(())
    }

    fn list(&self, dir: &str) -> io::Result<Vec<String>> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{}/", dir.trim_end_matches('/'))
        };
        let mut out: Vec<String> = self
            .files
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix))
            .map(|rest| rest.split('/').next().unwrap_or(rest).to_string())
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn persists(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_encode_safely_and_round_trip() {
        for name in [
            "",
            "Coins",
            "coins",
            "con",
            "..",
            "a/b\\c",
            "Ünïcode ✓",
            "x%y^z",
        ] {
            let e = encode(name);
            assert!(e.starts_with('_'), "{e}");
            assert!(
                e.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b"-_^%ABCDEF".contains(&b)),
                "{e}"
            );
            assert_eq!(decode(&e).as_deref(), Some(name), "{e}");
        }
        assert_ne!(encode("Coins"), encode("coins"));
        assert_eq!(decode("nope"), None);
        assert_eq!(decode("_%4"), None);
    }

    #[test]
    fn a_directory_writes_atomically_and_sets_bad_files_aside() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = DirBackend::new(dir.path().join("data"));
        assert_eq!(b.read("datastore/_a/_global.json").unwrap(), None);
        b.write("datastore/_a/_global.json", b"{}").unwrap();
        b.write("datastore/_a/_global.json", b"[1]").unwrap();
        assert_eq!(
            b.read("datastore/_a/_global.json").unwrap().as_deref(),
            Some(&b"[1]"[..])
        );
        assert_eq!(b.list("datastore").unwrap(), vec!["_a".to_string()]);
        b.set_aside("datastore/_a/_global.json").unwrap();
        assert_eq!(b.read("datastore/_a/_global.json").unwrap(), None);
        assert!(
            dir.path()
                .join("data/datastore/_a/_global.json.bad")
                .exists()
        );
        assert!(b.list("datastore/_a").unwrap().is_empty());
    }

    #[test]
    fn a_seeded_session_copies_the_seed_and_keeps_it_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = DirBackend::new(dir.path());
        d.write("datastore/_s/_global.json", b"seed").unwrap();
        let mut m = MemoryBackend::seeded(dir.path()).unwrap();
        assert_eq!(m.list("").unwrap(), vec!["datastore".to_string()]);
        assert_eq!(m.list("datastore").unwrap(), vec!["_s".to_string()]);
        m.write("datastore/_s/_global.json", b"changed").unwrap();
        assert_eq!(
            d.read("datastore/_s/_global.json").unwrap().as_deref(),
            Some(&b"seed"[..])
        );
        assert!(!m.persists());
        assert!(
            MemoryBackend::seeded(&dir.path().join("none"))
                .unwrap()
                .files
                .is_empty()
        );
    }
}
