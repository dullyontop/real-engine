//! The gap report: what scripts use that the engine does not provide (ADR-0012,
//! ADR-0014).
//!
//! [`Coverage::native`] is the M1 coverage table: the Luau builtins plus the
//! engine-native API, read from the definitions `re_script` generates from its
//! bindings, so the table cannot drift from what the VM has. M2 adds the
//! compatibility table.

use std::collections::{BTreeMap, BTreeSet};

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
}

impl GapReport {
    /// Compares `usage` with `coverage`.
    pub fn new(usage: &ApiUsage, coverage: &Coverage) -> GapReport {
        let mut r = GapReport::default();
        for (g, n) in &usage.globals {
            if coverage.globals.contains(g) {
                r.covered += 1;
            } else {
                r.missing.insert(g.clone(), *n);
            }
        }
        for (m, n) in &usage.by_member {
            if coverage.covers_member(m) {
                r.covered += 1;
            } else {
                r.missing.insert(m.clone(), *n);
            }
        }
        r
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
}
