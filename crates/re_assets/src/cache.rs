//! The processed-asset cache: engine-ready import results keyed by content.
//!
//! The key is a BLAKE3 hash over the importer's id and version, the import
//! settings, the source bytes and the bytes of every file the source pulls in.
//! Changing any of them gives a new key, so a stale entry is never served.
//!
//! An entry is a `(magic, format_version)` header followed by the postcard
//! encoding of the value, stored at `<dir>/<first 2 hex>/<hex>`. Entries are
//! written through a temporary file and a rename, so a crash never leaves a torn
//! entry. A corrupt or old-format entry reads as a miss. The cache is always
//! safe to delete (ARCHITECTURE §8).

use std::path::{Path, PathBuf};

use re_core::Diagnostic;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::fsutil;

const MAGIC: &[u8; 4] = b"REAC";

/// The entry format version. Bump it when the header or the encoding changes.
pub const CACHE_FORMAT_VERSION: u32 = 1;

/// A cache key: see the module documentation.
pub fn cache_key(importer: &str, version: u32, settings: &[u8], sources: &[&[u8]]) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    // Length-prefix every field so ("ab", "c") and ("a", "bc") hash differently.
    let mut field = |bytes: &[u8]| {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    field(importer.as_bytes());
    field(&version.to_le_bytes());
    field(settings);
    field(&(sources.len() as u64).to_le_bytes());
    for s in sources {
        field(s);
    }
    h.finalize()
}

/// Whether a lookup found an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheOutcome {
    /// The value came from the cache.
    Hit,
    /// The value was computed (and stored).
    Miss,
}

/// A directory of processed assets.
#[derive(Debug)]
pub struct ProcessedCache {
    dir: PathBuf,
    hits: u64,
    misses: u64,
}

impl ProcessedCache {
    /// A cache in `dir` (created on first write).
    pub fn new(dir: impl Into<PathBuf>) -> ProcessedCache {
        ProcessedCache {
            dir: dir.into(),
            hits: 0,
            misses: 0,
        }
    }

    /// The cache directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Lookups that found an entry.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Lookups that did not.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    fn entry_path(&self, key: &blake3::Hash) -> PathBuf {
        let hex = key.to_hex();
        self.dir.join(&hex[..2]).join(hex.as_str())
    }

    pub(crate) fn count(&mut self, hit: bool) {
        if hit {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
    }

    /// True when an entry exists for `key` (it may still fail to decode).
    pub fn contains(&self, key: &blake3::Hash) -> bool {
        self.entry_path(key).is_file()
    }

    /// The value stored under `key`, if there is a readable one. Does not count
    /// as a hit or miss.
    pub fn get<T: DeserializeOwned>(&self, key: &blake3::Hash) -> Option<T> {
        let bytes = std::fs::read(self.entry_path(key)).ok()?;
        let (header, body) = bytes.split_at_checked(8)?;
        if &header[..4] != MAGIC || header[4..] != CACHE_FORMAT_VERSION.to_le_bytes() {
            return None;
        }
        match postcard::from_bytes(body) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(key = %key.to_hex(), error = %e, "corrupt cache entry; it will be rebuilt");
                None
            }
        }
    }

    /// Stores `value` under `key`.
    ///
    /// # Errors
    ///
    /// [`codes::WRITE_FAILED`](crate::codes::WRITE_FAILED) when the entry cannot
    /// be written.
    pub fn put<T: Serialize>(&self, key: &blake3::Hash, value: &T) -> Result<(), Diagnostic> {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&CACHE_FORMAT_VERSION.to_le_bytes());
        let bytes = postcard::to_extend(value, bytes).expect("cache values always serialize");
        let path = self.entry_path(key);
        fsutil::write_atomic(&path, &bytes)
            .map_err(|e| fsutil::write_failed(&fsutil::slash(&path), &e))
    }

    /// The value under `key`, or `compute()`'s result, which is then stored. A
    /// failed store is logged and does not fail the call: the cache is an
    /// optimisation.
    pub fn get_or_insert_with<T, E>(
        &mut self,
        key: &blake3::Hash,
        compute: impl FnOnce() -> Result<T, E>,
    ) -> Result<(T, CacheOutcome), E>
    where
        T: Serialize + DeserializeOwned,
    {
        if let Some(v) = self.get(key) {
            self.hits += 1;
            return Ok((v, CacheOutcome::Hit));
        }
        self.misses += 1;
        let value = compute()?;
        if let Err(d) = self.put(key, &value) {
            tracing::warn!(code = %d.code, "{}", d.message);
        }
        Ok((value, CacheOutcome::Miss))
    }
}
