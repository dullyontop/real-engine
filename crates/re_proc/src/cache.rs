//! The disk cache of node outputs: `<dir>/<key hex>.bin` in a [`WritableDir`]
//! (the run's cache folder), postcard with a
//! `(magic, format_version)` header (ARCHITECTURE §8, "Caches"). Fields,
//! meshes, points, shapes and paths are stored; fragments are cheap to
//! rebuild from them and stay in memory only. Every entry is safe to delete.
//!
//! **Size.** The folder keeps to a limit (`[proc] disk_cache_mb`,
//! [`DEFAULT_DISK_CACHE_LIMIT`](crate::DEFAULT_DISK_CACHE_LIMIT) by default).
//! Its size is counted by listing it at the first write, then by adding up
//! what is written. A write that takes it past the limit trims it: the
//! entries used longest ago are removed until it holds at most nine tenths of
//! the limit, so the next trim is a tenth of the limit of writes away. A
//! writer that finds a trim running waits for it, so a write returns with the
//! folder back under the limit. Each trim lists the folder again, so caches
//! that share it with counts of their own (an editor and a `real verify`)
//! each bring it back under the limit. An entry's age is its file's
//! ([`WritableDir::list_by_age`]): writing an entry makes it young, and so
//! does a hit, once between trims ([`WritableDir::touch`]). Everything in the
//! folder counts, entries of an older format included. An entry larger than
//! the limit is not stored, and a trim that fails is tried again a tenth of
//! the limit of writes later.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

use re_core::files::WritableDir;
use serde::{Deserialize, Serialize};

use crate::value::{Field, MeshValue, Output, Points, RoadPath, Shapes};

const MAGIC: [u8; 8] = *b"REPROC\0\0";
/// Bump when the stored types change; older entries are then ignored.
const FORMAT_VERSION: u32 = 1;

/// The most entries remembered as used since the last trim; past it they
/// are forgotten, and a hit then marks its entry once more.
const MAX_FRESH: usize = 1 << 16;

#[derive(Serialize, Deserialize)]
enum Stored {
    Field(Field),
    Mesh(MeshValue),
    Points(Points),
    // Added after the M1 variants, so older entries still read the same.
    Shapes(Shapes),
    Path(RoadPath),
}

/// A directory of cached node outputs, kept to a size limit.
#[derive(Clone, Debug)]
pub(crate) struct DiskCache {
    dir: WritableDir,
    /// The most bytes the folder may hold.
    limit: u64,
    /// Shared by the clones (an evaluator's copy with another registry).
    usage: Arc<Usage>,
}

/// What a cache knows of its folder between trims.
#[derive(Debug, Default)]
struct Usage {
    tally: Mutex<Tally>,
    /// Held while a trim runs: a writer that finds the folder over the limit
    /// waits for the running trim, then looks again.
    trimming: Mutex<()>,
    /// The entries marked used since the last trim.
    fresh: Mutex<HashSet<[u8; 32]>>,
}

/// The folder's size, as last listed plus what was written since.
#[derive(Debug, Default)]
struct Tally {
    /// Bytes in the folder when it was last listed; `None` before then.
    listed: Option<u64>,
    /// Bytes written since. Rewriting an entry counts again, and so may a
    /// write the listing saw, so the sum counts high, never low (other
    /// processes' writes aside).
    written: u64,
}

impl Tally {
    /// True when the folder may hold more than `limit`, or has not been
    /// listed yet.
    fn over(&self, limit: u64) -> bool {
        self.listed
            .is_none_or(|listed| listed.saturating_add(self.written) > limit)
    }
}

impl Usage {
    fn tally(&self) -> MutexGuard<'_, Tally> {
        self.tally.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fresh(&self) -> MutexGuard<'_, HashSet<[u8; 32]>> {
        self.fresh.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn trimming(&self) -> MutexGuard<'_, ()> {
        self.trimming.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Remembers that `key`'s entry was marked used; true if it was not yet.
    fn mark_fresh(&self, key: &[u8; 32]) -> bool {
        let mut fresh = self.fresh();
        if fresh.len() >= MAX_FRESH {
            fresh.clear();
        }
        fresh.insert(*key)
    }

    /// Counts `bytes` written; true when the folder may now be over `limit`.
    fn add(&self, bytes: u64, limit: u64) -> bool {
        let mut t = self.tally();
        t.written = t.written.saturating_add(bytes);
        t.over(limit)
    }
}

impl DiskCache {
    /// A cache in `dir` holding at most `limit` bytes.
    pub(crate) fn new(dir: WritableDir, limit: u64) -> Self {
        DiskCache {
            dir,
            limit,
            usage: Arc::default(),
        }
    }

    pub(crate) fn dir(&self) -> &WritableDir {
        &self.dir
    }

    /// The most bytes the folder may hold.
    pub(crate) fn limit(&self) -> u64 {
        self.limit
    }

    fn path(key: &[u8; 32]) -> String {
        format!("{}.bin", blake3::Hash::from_bytes(*key).to_hex())
    }

    /// True for the output types that are stored.
    pub(crate) fn stores(output: &Output) -> bool {
        !matches!(output, Output::Fragment(_))
    }

    /// The cached output for `key`: `Ok(None)` when there is none, `Err` when
    /// the entry is unreadable or damaged.
    pub(crate) fn load(&self, key: &[u8; 32]) -> Result<Option<Output>, String> {
        let bytes = match self.dir.read(&Self::path(key)) {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(None),
            Err(e) => return Err(format!("cache entry could not be read: {e}")),
        };
        if bytes.len() < 12 || bytes[..8] != MAGIC {
            return Err("cache entry is damaged (bad header)".into());
        }
        let version = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        if version != FORMAT_VERSION {
            return Ok(None);
        }
        let stored: Stored = postcard::from_bytes(&bytes[12..])
            .map_err(|e| format!("cache entry is damaged: {e}"))?;
        // An entry that parses but breaks an invariant is damaged too.
        let output = match stored {
            Stored::Field(f) => {
                f.0.validate()
                    .map_err(|e| format!("cache entry is damaged: {e}"))?;
                Output::Field(Arc::new(f))
            }
            Stored::Mesh(m) => {
                if !m.mesh.is_well_formed() {
                    return Err("cache entry is damaged: malformed mesh".into());
                }
                Output::Mesh(Arc::new(m))
            }
            Stored::Points(p) => Output::Points(Arc::new(p)),
            Stored::Shapes(s) => {
                s.validate()
                    .map_err(|e| format!("cache entry is damaged: {e}"))?;
                Output::Shapes(Arc::new(s))
            }
            Stored::Path(p) => {
                p.validate()
                    .map_err(|e| format!("cache entry is damaged: {e}"))?;
                Output::Path(Arc::new(p))
            }
        };
        // A used entry is young again, so trims keep it. Marking it is
        // worth no warning: an entry that cannot be marked only goes sooner.
        if self.usage.mark_fresh(key) {
            let _ = self.dir.touch(&Self::path(key));
        }
        Ok(Some(output))
    }

    /// Writes `output` for `key` (through a temporary file, so a reader never
    /// sees half an entry), then trims the folder if it is over its limit.
    /// Fragments, and entries larger than the limit, are not stored.
    pub(crate) fn store(&self, key: &[u8; 32], output: &Output) -> Result<(), String> {
        let stored = match output {
            Output::Field(f) => Stored::Field((**f).clone()),
            Output::Mesh(m) => Stored::Mesh((**m).clone()),
            Output::Points(p) => Stored::Points((**p).clone()),
            Output::Shapes(s) => Stored::Shapes((**s).clone()),
            Output::Path(p) => Stored::Path((**p).clone()),
            Output::Fragment(_) => return Ok(()),
        };
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        let body = postcard::to_stdvec(&stored)
            .map_err(|e| format!("cache entry could not be encoded: {e}"))?;
        bytes.extend_from_slice(&body);
        let size = bytes.len() as u64;
        if size > self.limit {
            return Ok(());
        }
        // Written through a temporary file and a rename.
        self.dir
            .write(&Self::path(key), &bytes)
            .map_err(|e| format!("cache entry could not be written: {e}"))?;
        if self.usage.add(size, self.limit) {
            self.trim_if_over()?;
        }
        Ok(())
    }

    /// Trims the folder if, as counted, it may hold more than the limit.
    fn trim_if_over(&self) -> Result<(), String> {
        let _trimming = self.usage.trimming();
        // A trim that ran while this writer waited may have made room.
        if !self.usage.tally().over(self.limit) {
            return Ok(());
        }
        self.trim_locked()
    }

    /// [`trim_locked`](Self::trim_locked), taking the trim lock first.
    #[cfg(test)]
    fn trim(&self) -> Result<(), String> {
        let _trimming = self.usage.trimming();
        self.trim_locked()
    }

    /// Lists the folder and, when it holds more than the limit, removes the
    /// entries used longest ago until it holds at most nine tenths of it.
    /// The caller holds [`Usage::trimming`].
    fn trim_locked(&self) -> Result<(), String> {
        let target = self.limit - self.limit / 10;
        let written = self.usage.tally().written;
        let (total, failed) = match self.dir.list_by_age("") {
            Ok(files) => self.remove_oldest(files, target),
            Err(e) => (
                target,
                Some(format!(
                    "the cache could not be listed to keep it under its size limit: {e}"
                )),
            ),
        };
        // From now on the next hit on an entry marks it again, so the next
        // trim sees what was used since this one.
        self.usage.fresh().clear();
        let mut t = self.usage.tally();
        // After a failure the next try is a tenth of the limit of writes
        // away, not at every write.
        t.listed = Some(if failed.is_some() {
            total.min(target)
        } else {
            total
        });
        // What was written while the folder was listed may be counted twice.
        t.written = t.written.saturating_sub(written);
        failed.map_or(Ok(()), Err)
    }

    /// If `files` (oldest first) hold more than the limit, removes them from
    /// the front until they hold at most `target` bytes. Returns the bytes
    /// left and the first removal that failed; an entry that cannot be
    /// removed is passed over.
    fn remove_oldest(&self, files: Vec<(String, u64)>, target: u64) -> (u64, Option<String>) {
        let mut total: u64 = files.iter().map(|(_, size)| size).sum();
        let mut failed = None;
        if total > self.limit {
            for (path, size) in files {
                if total <= target {
                    break;
                }
                match self.dir.remove(&path) {
                    Ok(()) => total -= size,
                    Err(e) => {
                        failed.get_or_insert_with(|| {
                            format!(
                                "cache entry {path} could not be removed to keep the cache under its size limit: {e}"
                            )
                        });
                    }
                }
            }
        }
        (total, failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io;
    use std::path::Path;

    use re_core::files::WritableStore;
    use re_geom::Heightfield;

    /// A field output of `samples` × `samples` heights, all `h`.
    fn field(samples: u32, h: f32) -> Output {
        let n = samples as usize;
        let hf = Heightfield::centered(samples, 1.0, vec![h; n * n]).unwrap();
        Output::Field(Arc::new(Field(hf)))
    }

    fn key(n: u8) -> [u8; 32] {
        [n; 32]
    }

    /// The bytes an entry of `field(samples, _)` takes.
    fn entry_size(samples: u32) -> u64 {
        let dir = WritableDir::memory();
        DiskCache::new(dir.clone(), u64::MAX)
            .store(&key(0), &field(samples, 0.0))
            .unwrap();
        dir.list_by_age("").unwrap()[0].1
    }

    fn total(dir: &WritableDir) -> u64 {
        dir.list_by_age("").unwrap().iter().map(|(_, n)| n).sum()
    }

    fn has(dir: &WritableDir, n: u8) -> bool {
        dir.read(&DiskCache::path(&key(n))).unwrap().is_some()
    }

    /// The entries, oldest first.
    fn ages(dir: &WritableDir) -> Vec<String> {
        dir.list_by_age("")
            .unwrap()
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    fn paths<const N: usize>(keys: [u8; N]) -> Vec<String> {
        keys.iter().map(|&n| DiskCache::path(&key(n))).collect()
    }

    #[test]
    fn a_full_cache_drops_the_entries_used_longest_ago() {
        let size = entry_size(16);
        let dir = WritableDir::memory();
        // Room for ten entries and a half.
        let cache = DiskCache::new(dir.clone(), size * 10 + size / 2);
        for n in 0..10 {
            cache.store(&key(n), &field(16, f32::from(n))).unwrap();
        }
        assert_eq!(total(&dir), size * 10);
        // Entry 0 is used, so 1 is now the oldest.
        assert!(cache.load(&key(0)).unwrap().is_some());
        cache.store(&key(10), &field(16, 10.0)).unwrap();
        // Past the limit: down to nine tenths of it, so two entries went.
        assert_eq!(total(&dir), size * 9);
        assert!(has(&dir, 0), "a used entry stays");
        assert!(!has(&dir, 1) && !has(&dir, 2), "the oldest go");
        assert!((3..=10).all(|n| has(&dir, n)));
        // A cache over the same folder finds what is left.
        let other = DiskCache::new(dir.clone(), size * 10 + size / 2);
        let Some(Output::Field(f)) = other.load(&key(3)).unwrap() else {
            panic!("entry 3 is a field");
        };
        assert_eq!(f.0.samples[0], 3.0);
        assert!(other.load(&key(1)).unwrap().is_none());
    }

    #[test]
    fn a_hit_marks_its_entry_once_between_trims() {
        let size = entry_size(16);
        let dir = WritableDir::memory();
        let cache = DiskCache::new(dir.clone(), size * 4 + size / 2);
        for n in 0..4 {
            cache.store(&key(n), &field(16, 0.0)).unwrap();
        }
        assert_eq!(ages(&dir), paths([0, 1, 2, 3]));
        cache.load(&key(0)).unwrap().unwrap();
        cache.load(&key(1)).unwrap().unwrap();
        assert_eq!(ages(&dir), paths([2, 3, 0, 1]));
        // Marked already since the last trim: the order stays.
        cache.load(&key(0)).unwrap().unwrap();
        assert_eq!(ages(&dir), paths([2, 3, 0, 1]));
        // A trim, which removes the oldest, 2; after it a hit marks again.
        cache.store(&key(4), &field(16, 0.0)).unwrap();
        assert_eq!(ages(&dir), paths([3, 0, 1, 4]));
        cache.load(&key(0)).unwrap().unwrap();
        assert_eq!(ages(&dir), paths([3, 1, 4, 0]));
    }

    #[test]
    fn a_folder_over_the_limit_is_trimmed_at_the_first_write() {
        let size = entry_size(16);
        let dir = WritableDir::memory();
        // Whatever else is in the folder counts too: here the oldest file.
        dir.write("stray.tmp", &[0; 100]).unwrap();
        let before = DiskCache::new(dir.clone(), u64::MAX);
        for n in 0..20 {
            before.store(&key(n), &field(16, 0.0)).unwrap();
        }
        let limit = size * 10;
        let cache = DiskCache::new(dir.clone(), limit);
        cache.store(&key(20), &field(16, 0.0)).unwrap();
        assert_eq!(total(&dir), size * 9);
        assert_eq!(ages(&dir), paths([12, 13, 14, 15, 16, 17, 18, 19, 20]));
        assert_eq!(dir.read("stray.tmp").unwrap(), None);
    }

    #[test]
    fn a_cache_under_its_limit_removes_nothing() {
        let size = entry_size(16);
        let dir = WritableDir::memory();
        let cache = DiskCache::new(dir.clone(), size * 100);
        for n in 0..50 {
            cache.store(&key(n), &field(16, 0.0)).unwrap();
        }
        cache.trim().unwrap();
        assert_eq!(dir.list("").unwrap().len(), 50);
    }

    #[test]
    fn an_entry_larger_than_the_limit_is_not_stored() {
        let dir = WritableDir::memory();
        let cache = DiskCache::new(dir.clone(), entry_size(16));
        cache.store(&key(1), &field(32, 0.0)).unwrap();
        assert_eq!(dir.list("").unwrap(), Vec::<String>::new());
        cache.store(&key(2), &field(16, 0.0)).unwrap();
        assert!(has(&dir, 2));
    }

    #[test]
    fn clones_share_the_count() {
        let size = entry_size(16);
        let dir = WritableDir::memory();
        let cache = DiskCache::new(dir.clone(), size * 8 + size / 2);
        let copy = cache.clone();
        for n in 0..8 {
            let c = if n % 2 == 0 { &cache } else { &copy };
            c.store(&key(n), &field(16, 0.0)).unwrap();
        }
        assert_eq!(total(&dir), size * 8);
        // The copy knows of the original's writes, so this one trims.
        copy.store(&key(8), &field(16, 0.0)).unwrap();
        assert_eq!(total(&dir), size * 7);
    }

    #[test]
    fn writers_on_many_threads_share_one_folder() {
        let size = entry_size(16);
        let dir = WritableDir::memory();
        let limit = size * 20;
        let cache = DiskCache::new(dir.clone(), limit);
        std::thread::scope(|s| {
            for t in 0..4u8 {
                let cache = &cache;
                s.spawn(move || {
                    for n in 0..50u8 {
                        let mut k = key(n);
                        k[0] = t;
                        cache.store(&k, &field(16, f32::from(n))).unwrap();
                        // Never half an entry, whatever the other writers do.
                        cache.load(&k).unwrap();
                    }
                });
            }
        });
        // Writers wait for a running trim, so no write ends over the limit.
        assert!(total(&dir) <= limit, "{}", total(&dir));
        for (path, _) in dir.list_by_age("").unwrap() {
            let bytes = dir.read(&path).unwrap().unwrap();
            assert_eq!(bytes.len() as u64, size, "{path} is whole");
        }
    }

    #[test]
    fn the_limit_holds_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = WritableDir::dir(tmp.path());
        let size = entry_size(16);
        let limit = size * 5 + size / 2;
        let cache = DiskCache::new(dir.clone(), limit);
        for n in 0..5 {
            cache.store(&key(n), &field(16, 0.0)).unwrap();
        }
        // File times are coarse: age the entries a second apart, long ago.
        for n in 0..5u8 {
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1 + u64::from(n));
            std::fs::File::options()
                .write(true)
                .open(tmp.path().join(DiskCache::path(&key(n))))
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        // A new cache, as in the next run: a hit makes entry 0 young.
        let cache = DiskCache::new(dir.clone(), limit);
        assert!(cache.load(&key(0)).unwrap().is_some());
        cache.store(&key(5), &field(16, 0.0)).unwrap();
        assert_eq!(total(&dir), size * 4);
        assert!(has(&dir, 0) && has(&dir, 5));
        assert!(!has(&dir, 1) && !has(&dir, 2));
    }

    /// A folder in memory that cannot remove the files whose names start
    /// with `stuck` (`Some("")`: any file), nor be listed when `unlisted`.
    #[derive(Debug)]
    struct Failing {
        dir: WritableDir,
        stuck: Option<&'static str>,
        unlisted: bool,
    }

    impl Failing {
        fn dir(stuck: Option<&'static str>, unlisted: bool) -> WritableDir {
            WritableDir::from_store(Arc::new(Failing {
                dir: WritableDir::memory(),
                stuck,
                unlisted,
            }))
        }
    }

    impl WritableStore for Failing {
        fn read(&self, rel: &str) -> io::Result<Option<Vec<u8>>> {
            self.dir.read(rel)
        }
        fn write(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
            self.dir.write(rel, bytes)
        }
        fn remove(&self, rel: &str) -> io::Result<()> {
            if self.stuck.is_some_and(|stuck| rel.starts_with(stuck)) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"));
            }
            self.dir.remove(rel)
        }
        fn list(&self, dir: &str) -> io::Result<Vec<String>> {
            self.dir.list(dir)
        }
        fn list_by_age(&self, dir: &str) -> io::Result<Vec<(String, u64)>> {
            if self.unlisted {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "no access"));
            }
            self.dir.list_by_age(dir)
        }
        fn touch(&self, rel: &str) -> io::Result<()> {
            self.dir.touch(rel)
        }
        fn path(&self) -> Option<&Path> {
            None
        }
    }

    #[test]
    fn an_entry_that_cannot_be_removed_is_passed_over() {
        let size = entry_size(16);
        // Entry 0's file name starts with "00".
        let dir = Failing::dir(Some("00"), false);
        let cache = DiskCache::new(dir.clone(), size * 10 + size / 2);
        for n in 0..10 {
            cache.store(&key(n), &field(16, 0.0)).unwrap();
        }
        // Past the limit: entry 0, the oldest, stays, so the next ones go.
        let e = cache.store(&key(10), &field(16, 0.0)).unwrap_err();
        assert!(
            e.contains(&DiskCache::path(&key(0))) && e.contains("in use"),
            "{e}"
        );
        assert_eq!(total(&dir), size * 9);
        assert!(has(&dir, 0) && !has(&dir, 1) && !has(&dir, 2));
        assert!((3..=10).all(|n| has(&dir, n)));
    }

    #[test]
    fn a_trim_that_fails_is_tried_again_a_tenth_of_the_limit_later() {
        let size = entry_size(16);
        let tries = |dir: &WritableDir| -> Vec<(u8, String)> {
            // A tenth of the limit is two entries.
            let cache = DiskCache::new(dir.clone(), size * 20);
            (0..30)
                .filter_map(|n| Some((n, cache.store(&key(n), &field(16, 0.0)).err()?)))
                .collect()
        };
        // Nothing can be removed: the trim past the limit fails, and so does
        // every third write after it, not every write.
        let failed = tries(&Failing::dir(Some(""), false));
        assert_eq!(
            failed.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            [20, 23, 26, 29]
        );
        assert!(
            failed[0].1.contains("could not be removed"),
            "{}",
            failed[0].1
        );
        // A folder that cannot be listed fails at the first write, which
        // measures it, then every third write.
        let failed = tries(&Failing::dir(None, true));
        let at: Vec<u8> = (0..30).step_by(3).collect();
        assert_eq!(failed.iter().map(|(n, _)| *n).collect::<Vec<_>>(), at);
        assert!(
            failed[0].1.contains("could not be listed"),
            "{}",
            failed[0].1
        );
    }
}
