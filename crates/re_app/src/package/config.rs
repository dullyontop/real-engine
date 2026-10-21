//! `[package]` in `real.toml`: how `real build` packs the game (ADR-0037).

use serde::{Deserialize, Serialize};

/// How scripts are stored in the archive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptStorage {
    /// Luau bytecode, compiled by `real build` (the default).
    #[default]
    Bytecode,
    /// The source, compiled at load as in development.
    Source,
}

/// How assets are stored in the archive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetStorage {
    /// Processed when that is at most twice the compressed source, else the
    /// source (the default).
    #[default]
    Auto,
    /// Always the importer's processed output.
    Processed,
    /// Always the source, imported at first load into the per-user cache.
    Source,
}

/// `[package]`. Every key is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PackageConfig {
    /// Reverse-DNS id, such as `com.example.island`: the macOS bundle id and
    /// the name of the per-user folders. Default: `local.<name>`, with
    /// warning `RE0950`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// The name people see. Default: `[project] name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The game's version. Default: `"0.1.0"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A square PNG of at least 512 px, as a project path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// How scripts are stored.
    pub scripts: ScriptStorage,
    /// How assets are stored.
    pub assets: AssetStorage,
    /// Patterns (`*`, `**`) of project paths not to pack.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Patterns of project paths to pack beyond the default folders.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Patterns of the game's own notice files, appended to
    /// `THIRD_PARTY_NOTICES.txt`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

/// The default version of a package.
pub const DEFAULT_VERSION: &str = "0.1.0";

impl PackageConfig {
    /// The identifier, or `local.<name>` (with `name` reduced to the
    /// characters an identifier allows) when there is none.
    pub fn identifier_or_default(&self, name: &str) -> String {
        match &self.identifier {
            Some(id) => id.clone(),
            None => {
                let name: String = name
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect();
                format!("local.{}", name.trim_matches('-').to_ascii_lowercase())
            }
        }
    }

    /// The version, or [`DEFAULT_VERSION`].
    pub fn version_or_default(&self) -> &str {
        self.version.as_deref().unwrap_or(DEFAULT_VERSION)
    }
}

/// Why `identifier` is not a valid package identifier, if it is not: one or
/// more dot-separated parts of ASCII letters, digits and `-`, none empty.
pub fn identifier_problem(identifier: &str) -> Option<&'static str> {
    if identifier.is_empty() {
        return Some("it is empty");
    }
    if identifier.len() > 155 {
        return Some("it is longer than 155 characters");
    }
    if identifier.split('.').any(str::is_empty) {
        return Some("it has an empty part between dots");
    }
    if !identifier
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Some("only ASCII letters, digits, `.` and `-` are allowed");
    }
    None
}

/// True when the project path `path` matches the pattern: `*` matches within
/// one path segment and `**` matches any number of segments.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn segments(p: &[&str], s: &[&str]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(&"**"), _) => segments(&p[1..], s) || (!s.is_empty() && segments(p, &s[1..])),
            (Some(pat), Some(seg)) => {
                segment(pat.as_bytes(), seg.as_bytes()) && segments(&p[1..], &s[1..])
            }
            _ => false,
        }
    }
    fn segment(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => segment(&p[1..], s) || (!s.is_empty() && segment(p, &s[1..])),
            (Some(a), Some(b)) if a == b => segment(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let p: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    segments(&p, &s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_within_and_across_segments() {
        assert!(glob_match("assets/raw/**", "assets/raw/a/b.png"));
        assert!(glob_match("assets/raw/**", "assets/raw/b.png"));
        assert!(!glob_match("assets/raw/**", "assets/rawish/b.png"));
        assert!(glob_match("LICENSES/*.txt", "LICENSES/MIT.txt"));
        assert!(!glob_match("LICENSES/*.txt", "LICENSES/x/MIT.txt"));
        assert!(glob_match("**/*.psd", "assets/a/b.psd"));
        assert!(glob_match("**/*.psd", "b.psd"));
        assert!(glob_match("data/*", "data/x"));
    }

    #[test]
    fn identifiers_are_checked_and_defaulted() {
        assert_eq!(identifier_problem("com.example.island"), None);
        assert!(identifier_problem("com..x").is_some());
        assert!(identifier_problem("com.ex ample").is_some());
        let c = PackageConfig::default();
        assert_eq!(c.identifier_or_default("My Island"), "local.my-island");
        assert_eq!(c.version_or_default(), "0.1.0");
    }

    #[test]
    fn the_section_parses_with_its_defaults() {
        let c: PackageConfig = toml::from_str(
            "identifier = \"com.example.island\"\nassets = \"processed\"\nexclude = [\"assets/raw/**\"]",
        )
        .unwrap();
        assert_eq!(c.assets, AssetStorage::Processed);
        assert_eq!(c.scripts, ScriptStorage::Bytecode);
        assert!(toml::from_str::<PackageConfig>("bogus = 1").is_err());
    }
}
