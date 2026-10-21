//! The browser runtime's side of `re_app` (ADR-0049, WP29).
//!
//! A browser has no folders to write, so the [`WritableDirs`] of a web run keep
//! their files in memory and hand every change of saved data and crash reports to
//! a [`Sink`], which the boot script stores in IndexedDB. At the next start the
//! boot script reads them back and passes them to [`browser_dirs`]. Caches and
//! command outputs stay in memory only.
//!
//! Nothing writes to them on the web yet: the data service persists only to a
//! folder on disk ([`WritableStore::path`] is `None` here), and the crash hook is
//! re_cli's. Until a data backend writes through a [`WritableDir`], a game's saves
//! last only as long as its page.
//!
//! [`play`] (wasm32 only) runs an [`App`](crate::App) in the page. The rest does
//! not depend on the browser, so it is tested natively.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use re_core::files::{WritableDir, WritableDirs, WritableStore};

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod play;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use play::{Report, play};

/// Receives each change to a persisted file: its key (`data/<path>` or
/// `crash/<path>`) and its new bytes, or `None` when it was removed. It must not
/// block, and it must not panic, because a crash report may be written through it.
pub type Sink = fn(key: &str, bytes: Option<&[u8]>);

/// The writable folders of a web run. `saved` holds the files persisted by
/// earlier runs, keyed as the [`Sink`] receives them; keys of other folders are
/// ignored.
pub fn browser_dirs(
    saved: impl IntoIterator<Item = (String, Vec<u8>)>,
    sink: Sink,
) -> WritableDirs {
    let mut data = BTreeMap::new();
    let mut crash = BTreeMap::new();
    for (key, bytes) in saved {
        if let Some(rel) = key.strip_prefix("data/") {
            data.insert(rel.to_owned(), bytes);
        } else if let Some(rel) = key.strip_prefix("crash/") {
            crash.insert(rel.to_owned(), bytes);
        }
    }
    let mirrored = |prefix: &'static str, files| {
        WritableDir::from_store(Arc::new(MirroredStore {
            files: Mutex::new(files),
            prefix,
            sink,
        }))
    };
    WritableDirs {
        cache: WritableDir::memory(),
        data: mirrored("data/", data),
        crash: mirrored("crash/", crash),
        out: WritableDir::memory(),
    }
}

/// Files in memory whose every change is also handed to a [`Sink`].
struct MirroredStore {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    prefix: &'static str,
    sink: Sink,
}

impl fmt::Debug for MirroredStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MirroredStore")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl MirroredStore {
    fn files(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl WritableStore for MirroredStore {
    fn read(&self, rel: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.files().get(rel).cloned())
    }

    fn write(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        self.files().insert(rel.to_owned(), bytes.to_vec());
        (self.sink)(&format!("{}{rel}", self.prefix), Some(bytes));
        Ok(())
    }

    fn remove(&self, rel: &str) -> io::Result<()> {
        if self.files().remove(rel).is_some() {
            (self.sink)(&format!("{}{rel}", self.prefix), None);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    static SEEN: Mutex<Vec<(String, Option<Vec<u8>>)>> = Mutex::new(Vec::new());

    fn record(key: &str, bytes: Option<&[u8]>) {
        SEEN.lock()
            .unwrap()
            .push((key.to_owned(), bytes.map(<[u8]>::to_vec)));
    }

    #[test]
    fn saved_files_come_back_and_changes_reach_the_sink() {
        let saved = vec![
            ("data/slots/1.json".to_owned(), b"{}".to_vec()),
            ("crash/last.txt".to_owned(), b"boom".to_vec()),
            ("cache/ignored".to_owned(), b"x".to_vec()),
        ];
        let dirs = browser_dirs(saved, record);
        assert_eq!(
            dirs.data.read("slots/1.json").unwrap().as_deref(),
            Some(&b"{}"[..])
        );
        assert_eq!(dirs.crash.list("").unwrap(), ["last.txt"]);
        assert_eq!(dirs.cache.read("ignored").unwrap(), None);
        assert_eq!(dirs.data.path(), None);

        dirs.data.write("slots/2.json", b"[1]").unwrap();
        dirs.data.remove("slots/1.json").unwrap();
        dirs.data.remove("never-written").unwrap();
        dirs.out.write("shot.png", b"png").unwrap();
        assert_eq!(
            *SEEN.lock().unwrap(),
            [
                ("data/slots/2.json".to_owned(), Some(b"[1]".to_vec())),
                ("data/slots/1.json".to_owned(), None),
            ]
        );
        assert_eq!(dirs.data.list("slots").unwrap(), ["slots/2.json"]);
    }
}
