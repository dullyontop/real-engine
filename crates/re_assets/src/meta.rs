//! `.meta.json` sidecars: a source file's stable id and import settings.
//!
//! ```json
//! { "version": 1, "uuid": "0b6d3f9e-7f5c-4c55-9a43-2f4b6b0e3c11", "settings": {} }
//! ```
//!
//! The sidecar of `assets/models/tree.glb` is `assets/models/tree.glb.meta.json`.
//! It is created on first import and should be committed with the file.

use std::path::{Path, PathBuf};

use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::fsutil;
use crate::{AssetId, codes};

/// The contents of a `.meta.json` sidecar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetMeta {
    /// The file format version, [`AssetMeta::VERSION`].
    pub version: u32,
    /// The asset's id.
    pub uuid: Uuid,
    /// Import settings for the file's importer. Textures read `color_space`
    /// (`"srgb"`, the default, or `"linear"` for normal and data maps).
    #[serde(default = "empty_object")]
    pub settings: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl AssetMeta {
    /// The current format version.
    pub const VERSION: u32 = 1;

    /// A sidecar with a new random id and the given settings.
    pub fn new(settings: serde_json::Value) -> AssetMeta {
        AssetMeta {
            version: Self::VERSION,
            uuid: Uuid::new_v4(),
            settings,
        }
    }

    /// The asset id.
    pub fn id(&self) -> AssetId {
        AssetId::Source(self.uuid)
    }

    /// The settings as canonical bytes (part of the cache key).
    pub fn settings_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.settings).expect("JSON values always serialize")
    }

    /// The sidecar path of a file: `<file>.meta.json`.
    pub fn path_for(file: &Path) -> PathBuf {
        let mut p = file.as_os_str().to_owned();
        p.push(".meta.json");
        PathBuf::from(p)
    }

    /// Reads the sidecar of `file`, or writes a new one with `default_settings`
    /// when there is none. `shown` is the file's project-relative path, for
    /// diagnostics.
    ///
    /// # Errors
    ///
    /// [`codes::META_INVALID`] for a sidecar that is not valid (located at the
    /// line), [`codes::WRITE_FAILED`] when a new sidecar cannot be written.
    pub fn read_or_create(
        file: &Path,
        shown: &str,
        default_settings: serde_json::Value,
    ) -> Result<AssetMeta, Diagnostic> {
        let path = Self::path_for(file);
        let shown_meta = format!("{shown}.meta.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let meta: AssetMeta = serde_json::from_str(&text).map_err(|e| {
                    Diagnostic::error(codes::META_INVALID, format!("invalid asset sidecar: {e}"))
                        .at(Location::file_at(&shown_meta, e.line() as u32, e.column() as u32))
                        .with_hint("fix the JSON, or delete the sidecar to give the asset a new id (references to the old id break)")
                })?;
                if meta.version != Self::VERSION {
                    return Err(Diagnostic::error(
                        codes::META_INVALID,
                        format!(
                            "asset sidecar version {} is not supported (expected {})",
                            meta.version,
                            Self::VERSION
                        ),
                    )
                    .at(Location::file(&shown_meta)));
                }
                Ok(meta)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let meta = AssetMeta::new(default_settings);
                meta.write(file)
                    .map_err(|e| fsutil::write_failed(&shown_meta, &e))?;
                tracing::debug!(file = shown, id = %meta.id(), "created asset sidecar");
                Ok(meta)
            }
            Err(e) => Err(Diagnostic::error(
                codes::READ_FAILED,
                format!("cannot read `{shown_meta}`: {e}"),
            )
            .at(Location::file(&shown_meta))),
        }
    }

    /// Writes this sidecar next to `file`, atomically, as pretty JSON.
    pub fn write(&self, file: &Path) -> std::io::Result<()> {
        let mut text = serde_json::to_string_pretty(self).expect("a sidecar always serializes");
        text.push('\n');
        fsutil::write_atomic(&Self::path_for(file), text.as_bytes())
    }
}
