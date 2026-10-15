//! The disk cache of node outputs: `<dir>/<key hex>.bin`, postcard with a
//! `(magic, format_version)` header (ARCHITECTURE §8, "Caches"). Fields,
//! meshes and points are stored; fragments are cheap to rebuild from them
//! and stay in memory only. Every entry is safe to delete.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::value::{Field, MeshValue, Output, Points};

const MAGIC: [u8; 8] = *b"REPROC\0\0";
/// Bump when the stored types change; older entries are then ignored.
const FORMAT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
enum Stored {
    Field(Field),
    Mesh(MeshValue),
    Points(Points),
}

/// A directory of cached node outputs.
#[derive(Clone, Debug)]
pub(crate) struct DiskCache {
    dir: PathBuf,
}

impl DiskCache {
    pub(crate) fn new(dir: PathBuf) -> Self {
        DiskCache { dir }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: &[u8; 32]) -> PathBuf {
        self.dir
            .join(format!("{}.bin", blake3::Hash::from_bytes(*key).to_hex()))
    }

    /// True for the output types that are stored.
    pub(crate) fn stores(output: &Output) -> bool {
        !matches!(output, Output::Fragment(_))
    }

    /// The cached output for `key`: `Ok(None)` when there is none, `Err` when
    /// the entry is unreadable or damaged.
    pub(crate) fn load(&self, key: &[u8; 32]) -> Result<Option<Output>, String> {
        let bytes = match fs::read(self.path(key)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
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
        Ok(Some(match stored {
            Stored::Field(f) => Output::Field(Arc::new(f)),
            Stored::Mesh(m) => Output::Mesh(Arc::new(m)),
            Stored::Points(p) => Output::Points(Arc::new(p)),
        }))
    }

    /// Writes `output` for `key` (through a temporary file, so a reader never
    /// sees half an entry). Fragments are not stored.
    pub(crate) fn store(&self, key: &[u8; 32], output: &Output) -> Result<(), String> {
        let stored = match output {
            Output::Field(f) => Stored::Field((**f).clone()),
            Output::Mesh(m) => Stored::Mesh((**m).clone()),
            Output::Points(p) => Stored::Points((**p).clone()),
            Output::Fragment(_) => return Ok(()),
        };
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        let body = postcard::to_stdvec(&stored)
            .map_err(|e| format!("cache entry could not be encoded: {e}"))?;
        bytes.extend_from_slice(&body);
        fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cache directory could not be created: {e}"))?;
        let path = self.path(key);
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        fs::write(&tmp, &bytes).map_err(|e| format!("cache entry could not be written: {e}"))?;
        fs::rename(&tmp, &path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("cache entry could not be written: {e}")
        })
    }
}
