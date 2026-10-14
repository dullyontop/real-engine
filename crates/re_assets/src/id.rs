//! Asset ids and project paths.

use std::fmt;
use std::str::FromStr;

use re_core::Diagnostic;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::codes;

/// A stable asset id.
///
/// * `Source` ids belong to files in the project. They are random UUIDs stored in
///   the file's `.meta.json` sidecar, so renaming or moving a file (with its
///   sidecar) keeps every reference to it.
/// * `Derived` ids belong to assets made from another asset, such as the textures
///   inside a glTF file. They are a BLAKE3 hash of the parent's id and a label
///   (`image/0`), so they are the same on every import.
///
/// The text form is the UUID (`0b6d…-…`) for a source id and `derived:<64 hex>`
/// for a derived one; it is what [`TextureRef::Asset`](re_types::TextureRef)
/// holds. Serialized as the text form.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetId {
    /// A project file.
    Source(Uuid),
    /// A part of another asset.
    Derived(blake3::Hash),
}

impl AssetId {
    /// A new random source id.
    pub fn new_source() -> AssetId {
        AssetId::Source(Uuid::new_v4())
    }

    /// The id of the part `label` of `parent`.
    pub fn derived(parent: AssetId, label: &str) -> AssetId {
        let mut h = blake3::Hasher::new();
        let parent = parent.to_string();
        for field in [
            b"re_assets derived id".as_slice(),
            parent.as_bytes(),
            label.as_bytes(),
        ] {
            h.update(&(field.len() as u64).to_le_bytes());
            h.update(field);
        }
        AssetId::Derived(h.finalize())
    }

    /// Parses the text form.
    ///
    /// # Errors
    ///
    /// [`codes::UNKNOWN_ASSET`] when `text` is neither a UUID nor `derived:<hex>`.
    pub fn parse(text: &str) -> Result<AssetId, Diagnostic> {
        let parsed = match text.strip_prefix("derived:") {
            Some(hex) => blake3::Hash::from_hex(hex).ok().map(AssetId::Derived),
            None => Uuid::parse_str(text).ok().map(AssetId::Source),
        };
        parsed.ok_or_else(|| {
            Diagnostic::error(codes::UNKNOWN_ASSET, format!("`{text}` is not an asset id"))
                .with_hint(
                    "asset ids are a UUID (from a .meta.json file) or `derived:<64 hex digits>`",
                )
        })
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssetId::Source(u) => write!(f, "{}", u.hyphenated()),
            AssetId::Derived(h) => write!(f, "derived:{}", h.to_hex()),
        }
    }
}

impl fmt::Debug for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AssetId({self})")
    }
}

impl FromStr for AssetId {
    type Err = Diagnostic;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AssetId::parse(s)
    }
}

impl Serialize for AssetId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        AssetId::parse(&text).map_err(|d| serde::de::Error::custom(d.message))
    }
}

/// Checks and normalises a project-relative path: `/` separators, no `.` parts,
/// no `..` that leaves the project, not absolute.
///
/// # Errors
///
/// [`codes::PATH_INVALID`].
pub fn normalize_path(path: &str) -> Result<String, Diagnostic> {
    let invalid = |why: &str| {
        Diagnostic::error(codes::PATH_INVALID, format!("`{path}` is not a project-relative path: {why}"))
            .with_hint("asset paths are relative to the project root and use `/`, for example `assets/models/tree.glb`")
    };
    let unified = path.replace('\\', "/");
    if unified.starts_with('/') || unified.contains(':') {
        return Err(invalid("it is absolute"));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(invalid("it leaves the project"));
                }
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Err(invalid("it is empty"));
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_form_round_trips() {
        let s = AssetId::new_source();
        assert_eq!(AssetId::parse(&s.to_string()).unwrap(), s);
        let d = AssetId::derived(s, "image/0");
        assert!(d.to_string().starts_with("derived:"));
        assert_eq!(AssetId::parse(&d.to_string()).unwrap(), d);
        assert_eq!(d, AssetId::derived(s, "image/0"), "derived ids are stable");
        assert_ne!(d, AssetId::derived(s, "image/1"));
        assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{s}\""));
        assert!(AssetId::parse("tree.png").is_err());
        assert!(AssetId::parse("derived:xyz").is_err());
    }

    #[test]
    fn paths_normalise() {
        assert_eq!(
            normalize_path("assets\\a/./b/../c.png").unwrap(),
            "assets/a/c.png"
        );
        assert!(normalize_path("/etc/passwd").is_err());
        assert!(normalize_path("C:/x").is_err());
        assert!(normalize_path("../outside.png").is_err());
        assert!(normalize_path("").is_err());
    }
}
