//! The disk cache of node outputs: `<dir>/<key hex>.bin` in a [`WritableDir`]
//! (the run's cache folder), postcard with a
//! `(magic, format_version)` header (ARCHITECTURE §8, "Caches"). Fields,
//! meshes, points, shapes, paths and polygon meshes are stored; fragments are
//! cheap to rebuild from them and stay in memory only. Every entry is safe to
//! delete.

use std::sync::Arc;

use re_core::files::WritableDir;
use serde::{Deserialize, Serialize};

use crate::value::{Field, MeshValue, Output, Points, PolyMesh, RoadPath, Shapes};

const MAGIC: [u8; 8] = *b"REPROC\0\0";
/// Bump when the stored types change; older entries are then ignored.
const FORMAT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
enum Stored {
    Field(Field),
    Mesh(MeshValue),
    Points(Points),
    // Added after the M1 variants, so older entries still read the same.
    Shapes(Shapes),
    Path(RoadPath),
    // ADR-0062 §6, appended in turn.
    PolyMesh(PolyMesh),
}

/// A directory of cached node outputs.
#[derive(Clone, Debug)]
pub(crate) struct DiskCache {
    dir: WritableDir,
}

impl DiskCache {
    pub(crate) fn new(dir: WritableDir) -> Self {
        DiskCache { dir }
    }

    pub(crate) fn dir(&self) -> &WritableDir {
        &self.dir
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
        Ok(Some(match stored {
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
            Stored::PolyMesh(m) => {
                m.validate()
                    .map_err(|e| format!("cache entry is damaged: {e}"))?;
                Output::PolyMesh(Arc::new(m))
            }
        }))
    }

    /// Writes `output` for `key` (through a temporary file, so a reader never
    /// sees half an entry). Fragments are not stored.
    pub(crate) fn store(&self, key: &[u8; 32], output: &Output) -> Result<(), String> {
        let stored = match output {
            Output::Field(f) => Stored::Field((**f).clone()),
            Output::Mesh(m) => Stored::Mesh((**m).clone()),
            Output::Points(p) => Stored::Points((**p).clone()),
            Output::Shapes(s) => Stored::Shapes((**s).clone()),
            Output::Path(p) => Stored::Path((**p).clone()),
            Output::PolyMesh(m) => Stored::PolyMesh((**m).clone()),
            Output::Fragment(_) => return Ok(()),
        };
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        let body = postcard::to_stdvec(&stored)
            .map_err(|e| format!("cache entry could not be encoded: {e}"))?;
        bytes.extend_from_slice(&body);
        // Written through a temporary file and a rename.
        self.dir
            .write(&Self::path(key), &bytes)
            .map_err(|e| format!("cache entry could not be written: {e}"))
    }
}
