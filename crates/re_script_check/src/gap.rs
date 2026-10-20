//! The gap report: what scripts use that the engine does not provide (ADR-0012,
//! ADR-0014).
//!
//! [`Coverage::native`] is the M1 coverage table: the Luau builtins plus the
//! engine-native API, read from the definitions `re_script` generates from its
//! bindings, so the table cannot drift from what the VM has.
//! [`Coverage::compat`] adds the compatibility layer's table,
//! `crates/re_compat/coverage.json`: its `implemented` rows count as covered,
//! its `partial` and `stub-error` rows are reported apart, and its `missing`
//! rows stay gaps.

use std::collections::{BTreeMap, BTreeSet};

use re_compat::{CoverageFile, Status};
use serde::{Deserialize, Serialize};

use crate::scan::ApiUsage;

/// Luau's builtin globals (functions and values).
const BUILTIN_GLOBALS: &[&str] = &[
    "assert",
    "error",
    "gcinfo",
    "getmetatable",
    "ipairs",
    "newproxy",
    "next",
    "pairs",
    "pcall",
    "print",
    "rawequal",
    "rawget",
    "rawlen",
    "rawset",
    "require",
    "select",
    "setmetatable",
    "tonumber",
    "tostring",
    "type",
    "typeof",
    "unpack",
    "xpcall",
    "_G",
    "_VERSION",
];

/// Luau's builtin libraries; every member of one counts as covered.
const BUILTIN_LIBS: &[&str] = &[
    "bit32",
    "buffer",
    "coroutine",
    "debug",
    "math",
    "os",
    "string",
    "table",
    "utf8",
    "vector",
];

/// The names a project can use without a gap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Covered globals.
    pub globals: BTreeSet<String>,
    /// Covered members, as `receiver.member`.
    pub members: BTreeSet<String>,
    /// Receivers all of whose members are covered.
    pub whole: BTreeSet<String>,
    /// Names (globals, or `receiver.member`) that exist with a documented gap.
    pub partial: BTreeSet<String>,
    /// Names that exist and raise a clear error when used.
    pub stub_error: BTreeSet<String>,
    /// True once a compatibility table was added: the report then also checks
    /// enum items (`Enum.KeyCode.Space`), `Instance.new(<Class>)` and
    /// `GetService(<Service>)`.
    pub compat: bool,
}

impl Coverage {
    /// The M1 table: Luau builtins and the engine-native API.
    pub fn native() -> Coverage {
        let mut c = Coverage::from_definitions(&re_script::definitions());
        c.globals
            .extend(BUILTIN_GLOBALS.iter().map(|s| s.to_string()));
        c.globals.extend(BUILTIN_LIBS.iter().map(|s| s.to_string()));
        c.whole.extend(BUILTIN_LIBS.iter().map(|s| s.to_string()));
        c
    }

    /// The M2 table: [`Coverage::native`] plus the compatibility layer's
    /// `coverage.json`.
    pub fn compat() -> Coverage {
        let mut c = Coverage::native();
        let table = CoverageFile::parse(re_compat::COVERAGE_JSON)
            .expect("re_compat's committed coverage.json parses (checked by its tests)");
        c.add_compat(&table);
        c
    }

    /// Adds the rows of a compatibility coverage table, keyed as
    /// [`CoverageFile::keys`] keys them.
    pub fn add_compat(&mut self, table: &CoverageFile) {
        self.compat = true;
        for (key, status) in table.keys() {
            let global = !key.contains('.');
            match status {
                Status::Implemented if global => {
                    self.globals.insert(key);
                }
                Status::Implemented => {
                    self.members.insert(key);
                }
                Status::Partial => {
                    self.partial.insert(key);
                }
                Status::StubError => {
                    self.stub_error.insert(key);
                }
                Status::Missing => {}
            }
        }
    }

    /// Reads `declare name: { member: type, ... }` and `declare function name`
    /// from a definitions file.
    pub fn from_definitions(defs: &str) -> Coverage {
        let mut c = Coverage::default();
        let mut current: Option<String> = None;
        for line in defs.lines() {
            if let Some(rest) = line.strip_prefix("declare function ") {
                let name: String = rest
                    .chars()
                    .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                    .collect();
                c.globals.insert(name);
                continue;
            }
            if let Some(rest) = line.strip_prefix("declare ")
                && !rest.starts_with("extern ")
                && let Some((name, _)) = rest.split_once(':')
            {
                let name = name.trim().to_string();
                c.globals.insert(name.clone());
                current = rest.trim_end().ends_with('{').then_some(name);
                continue;
            }
            if line.starts_with('}') {
                current = None;
                continue;
            }
            if let Some(lib) = &current {
                let t = line.trim();
                if t.starts_with("--") {
                    continue;
                }
                if let Some((member, _)) = t.split_once(':') {
                    c.members.insert(format!("{lib}.{}", member.trim()));
                }
            }
        }
        c
    }

    fn covers_member(&self, key: &str) -> bool {
        self.members.contains(key)
            || key
                .split_once('.')
                .is_some_and(|(recv, _)| self.whole.contains(recv))
    }
}

/// What a body of scripts uses, split by whether the engine provides it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapReport {
    /// Distinct names used that are covered.
    pub covered: usize,
    /// Names used that are not covered, with how often (globals by name, members
    /// as `receiver.member`).
    pub missing: BTreeMap<String, usize>,
    /// Names used that exist with a documented gap, with how often.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub partial: BTreeMap<String, usize>,
    /// Names used that exist and raise a clear error, with how often.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stub_error: BTreeMap<String, usize>,
}

impl GapReport {
    /// Compares `usage` with `coverage`.
    pub fn new(usage: &ApiUsage, coverage: &Coverage) -> GapReport {
        let mut r = GapReport::default();
        let globals = usage
            .globals
            .iter()
            .map(|(g, n)| (g, *n, coverage.globals.contains(g)));
        let members = usage
            .by_member
            .iter()
            .map(|(m, n)| (m, *n, coverage.covers_member(m)));
        // Calls the compatibility table keys by argument.
        let mut extra: Vec<(String, usize)> = Vec::new();
        if coverage.compat {
            extra.extend(usage.enums.iter().map(|(k, n)| (format!("Enum.{k}"), *n)));
            extra.extend(
                usage
                    .instance_new
                    .iter()
                    .map(|(k, n)| (format!("Instance.new({k})"), *n)),
            );
            extra.extend(
                usage
                    .services
                    .iter()
                    .map(|(k, n)| (format!("GetService({k})"), *n)),
            );
        }
        let extra = extra.iter().map(|(k, n)| {
            (
                k,
                *n,
                coverage.members.contains(k) || coverage.globals.contains(k),
            )
        });
        for (name, n, covered) in globals.chain(members).chain(extra) {
            if covered {
                r.covered += 1;
            } else if coverage.partial.contains(name) {
                r.partial.insert(name.clone(), n);
            } else if coverage.stub_error.contains(name) {
                r.stub_error.insert(name.clone(), n);
            } else {
                r.missing.insert(name.clone(), n);
            }
        }
        r
    }

    /// Drops the `Service.Child` names that are the project's own instances
    /// (`ReplicatedStorage.Shared` reads a child, not an API member), so the
    /// report lists only API the engine lacks. Returns how many were dropped.
    pub fn drop_children(&mut self, children: &BTreeSet<String>) -> usize {
        let before = self.missing.len();
        self.missing.retain(|name, _| !children.contains(name));
        before - self.missing.len()
    }

    /// The missing names, most used first (ties by name).
    pub fn ranked(&self) -> Vec<(&str, usize)> {
        let mut v: Vec<(&str, usize)> =
            self.missing.iter().map(|(k, n)| (k.as_str(), *n)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::scan_source;

    #[test]
    fn native_coverage_reads_the_generated_definitions() {
        let c = Coverage::native();
        for g in [
            "world", "task", "physics", "print", "math", "warn", "script", "command",
        ] {
            assert!(c.globals.contains(g), "{g}");
        }
        for m in [
            "world.query",
            "task.wait",
            "physics.raycast",
            "time.on_step",
            "proc.graph",
        ] {
            assert!(c.members.contains(m), "{m}");
        }
        assert!(!c.members.contains("world.GetChildren"));
    }

    #[test]
    fn roblox_style_names_are_gaps() {
        let u = scan_source(
            "local P = game:GetService('Players')\nP.PlayerAdded:Connect(print)\nworld.query('A')\nmath.floor(1)\nworld.Spawn()",
            "t.luau",
        )
        .unwrap();
        let g = GapReport::new(&u, &Coverage::native());
        assert!(g.missing.contains_key("game"));
        assert!(g.missing.contains_key("Players.PlayerAdded"));
        assert!(g.missing.contains_key("world.Spawn"));
        assert!(!g.missing.contains_key("world.query"));
        assert!(!g.missing.contains_key("math.floor"));
    }

    #[test]
    fn compat_table_rows_count_by_status() {
        let table = CoverageFile::parse(
            r#"{
              "format": "real-engine/compat-coverage", "version": 1, "totals": {},
              "classes": [
                {"name": "Instance", "status": "partial", "members": [
                  {"name": "Name", "kind": "property", "status": "implemented"},
                  {"name": "Destroy", "kind": "method", "status": "missing"}]},
                {"name": "Players", "superclass": "Instance", "service": true, "status": "partial",
                 "members": [{"name": "LocalPlayer", "kind": "property", "status": "partial", "note": "n"}]},
                {"name": "DataModel", "superclass": "Instance", "status": "partial", "members": [
                  {"name": "GetService", "kind": "method", "status": "implemented"}]}],
              "enums": [{"name": "KeyCode", "status": "implemented", "items": ["A"]}],
              "libraries": [
                {"name": "game", "class": "DataModel", "status": "implemented", "members": []},
                {"name": "DataStoreService", "status": "stub-error", "members": []}]
            }"#,
        )
        .unwrap();
        let mut c = Coverage::native();
        c.add_compat(&table);
        let u = scan_source(
            "local P = game:GetService('Players')\nprint(P.LocalPlayer, P.Name)\nP:Destroy()\nlocal _ = Enum.KeyCode.A\nlocal _ = DataStoreService",
            "t.luau",
        )
        .unwrap();
        let g = GapReport::new(&u, &c);
        assert!(!g.missing.contains_key("game"), "{g:?}");
        assert!(!g.missing.contains_key("game.GetService"), "{g:?}");
        assert!(!g.missing.contains_key("Players.Name"), "inherited: {g:?}");
        assert!(g.partial.contains_key("Players.LocalPlayer"), "{g:?}");
        assert!(g.missing.contains_key("Players.Destroy"), "{g:?}");
        assert!(g.stub_error.contains_key("DataStoreService"), "{g:?}");
        assert!(!g.missing.contains_key("Enum"), "{g:?}");
        assert!(!g.missing.contains_key("Enum.KeyCode"), "{g:?}");
        assert!(!g.missing.contains_key("Enum.KeyCode.A"), "{g:?}");
        assert!(g.partial.contains_key("GetService(Players)"), "{g:?}");
    }

    #[test]
    fn project_children_are_not_gaps() {
        let mut c = Coverage::native();
        c.add_compat(&CoverageFile::parse(re_compat::COVERAGE_JSON).unwrap());
        let u = scan_source(
            "local RS = game:GetService('ReplicatedStorage')\nlocal _ = RS.Shared\nlocal _ = RS.Nope",
            "t.luau",
        )
        .unwrap();
        let mut g = GapReport::new(&u, &c);
        assert!(g.missing.contains_key("ReplicatedStorage.Shared"), "{g:?}");
        let children = BTreeSet::from(["ReplicatedStorage.Shared".to_string()]);
        assert_eq!(g.drop_children(&children), 1);
        assert!(!g.missing.contains_key("ReplicatedStorage.Shared"), "{g:?}");
        assert!(g.missing.contains_key("ReplicatedStorage.Nope"), "{g:?}");
    }

    #[test]
    fn committed_compat_table_loads() {
        let c = Coverage::compat();
        assert!(c.globals.contains("print"));
    }
}
