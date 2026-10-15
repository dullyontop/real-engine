//! Agent access to the Roblox-style layer (docs/M2_PLAN.md, WP17):
//! `compat.coverage` queries the coverage table (`re_compat`'s
//! `coverage.json`), and `scene.dump_tree` returns the instance tree with
//! classes, attributes and tags.

use re_command::{CommandRegistry, EditTarget, Tier};
use re_compat::{COVERAGE_JSON, CoverageFile, Status};
use re_core::{Diagnostic, Location};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::codes;

/// The most rows `compat.coverage` returns in one call.
pub const MAX_ROWS: usize = 500;

pub use re_compat::dump::{DumpArgs, DumpOutput, MAX_NODES, TreeNode};

/// Arguments of `compat.coverage`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageArgs {
    /// One class, enum or library by exact name (`Part`, `Enum.KeyCode`,
    /// `Vector3`). A class comes back with every member it has, inherited
    /// ones included, and where each is declared.
    #[serde(default)]
    pub name: Option<String>,
    /// Rows whose key contains this text, ignoring case (`Workspace.`,
    /// `Tween`).
    #[serde(default)]
    pub search: Option<String>,
    /// Only rows with this status.
    #[serde(default)]
    pub status: Option<Status>,
    /// The most rows to return (default 50, at most 500).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One row of the coverage table.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct CoverageRow {
    /// `Class`, `Class.Member`, `Enum.Type`, `Library`, `Library.member`, or
    /// `Datatype.member` for a member of a datatype's values.
    pub key: String,
    /// `class`, `service`, `property`, `method`, `event`, `enum`, `library`,
    /// `function`, `constant`, or `value property` / `value method`.
    pub kind: String,
    /// The status.
    pub status: Status,
    /// The gap, for `partial`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// For a class's member: the class that declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// The result of `compat.coverage`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CoverageOutput {
    /// Counts by status over the whole table.
    pub totals: std::collections::BTreeMap<Status, usize>,
    /// The matching rows, in key order (for `name`, the item and its
    /// members).
    pub rows: Vec<CoverageRow>,
    /// How many rows matched before `limit`.
    pub matched: usize,
    /// True when rows were left out because of `limit`.
    pub truncated: bool,
}

fn coverage() -> Result<CoverageFile, Diagnostic> {
    CoverageFile::parse(COVERAGE_JSON).map_err(|e| e.to_diagnostic())
}

/// Every row of the table, in key order within each section.
fn all_rows(cov: &CoverageFile) -> Vec<CoverageRow> {
    let mut rows = Vec::new();
    for c in &cov.classes {
        rows.push(CoverageRow {
            key: c.name.clone(),
            kind: if c.service { "service" } else { "class" }.to_string(),
            status: c.status,
            note: String::new(),
            from: None,
        });
        for m in &c.members {
            rows.push(CoverageRow {
                key: format!("{}.{}", c.name, m.name),
                kind: m.kind.clone(),
                status: m.status,
                note: m.note.clone(),
                from: Some(c.name.clone()),
            });
        }
    }
    for e in &cov.enums {
        rows.push(CoverageRow {
            key: format!("Enum.{}", e.name),
            kind: "enum".to_string(),
            status: e.status,
            note: e.note.clone(),
            from: None,
        });
    }
    for l in &cov.libraries {
        rows.push(CoverageRow {
            key: l.name.clone(),
            kind: "library".to_string(),
            status: l.status,
            note: l.note.clone(),
            from: None,
        });
        for m in &l.members {
            rows.push(CoverageRow {
                key: format!("{}.{}", l.name, m.name),
                kind: m.kind.clone(),
                status: m.status,
                note: m.note.clone(),
                from: None,
            });
        }
        for m in &l.value_members {
            rows.push(CoverageRow {
                key: format!("{}.{}", l.name, m.name),
                kind: format!("value {}", m.kind),
                status: m.status,
                note: m.note.clone(),
                from: None,
            });
        }
    }
    rows
}

/// The rows for one named item: a class with its inherited members, an enum
/// with its items, or a library with its members.
fn named_rows(cov: &CoverageFile, name: &str) -> Result<Vec<CoverageRow>, Diagnostic> {
    if let Some(class) = cov.classes.iter().find(|c| c.name == name) {
        let mut rows = vec![CoverageRow {
            key: class.name.clone(),
            kind: if class.service { "service" } else { "class" }.to_string(),
            status: class.status,
            note: String::new(),
            from: None,
        }];
        let mut seen = std::collections::BTreeSet::new();
        let mut cur = Some(class);
        let mut guard = 0;
        while let Some(c) = cur {
            for m in &c.members {
                // A subclass's row wins over an inherited one of the same name.
                if seen.insert(m.name.clone()) {
                    rows.push(CoverageRow {
                        key: format!("{}.{}", class.name, m.name),
                        kind: m.kind.clone(),
                        status: m.status,
                        note: m.note.clone(),
                        from: Some(c.name.clone()),
                    });
                }
            }
            cur = c
                .superclass
                .as_deref()
                .and_then(|s| cov.classes.iter().find(|x| x.name == s));
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        return Ok(rows);
    }
    let enum_name = name.strip_prefix("Enum.").unwrap_or(name);
    if let Some(e) = cov.enums.iter().find(|e| e.name == enum_name) {
        let mut rows = vec![CoverageRow {
            key: format!("Enum.{}", e.name),
            kind: "enum".to_string(),
            status: e.status,
            note: e.note.clone(),
            from: None,
        }];
        rows.extend(e.items.iter().map(|i| CoverageRow {
            key: format!("Enum.{}.{i}", e.name),
            kind: "enum item".to_string(),
            status: e.status,
            note: String::new(),
            from: None,
        }));
        return Ok(rows);
    }
    if cov.libraries.iter().any(|l| l.name == name) {
        let prefix = format!("{name}.");
        return Ok(all_rows(cov)
            .into_iter()
            .filter(|r| r.key == name || r.key.starts_with(&prefix))
            .collect());
    }
    let mut names: Vec<String> = cov.classes.iter().map(|c| c.name.clone()).collect();
    names.extend(cov.enums.iter().map(|e| format!("Enum.{}", e.name)));
    names.extend(cov.libraries.iter().map(|l| l.name.clone()));
    let suggestion = re_reflect::suggest(name, names.iter().map(String::as_str));
    Err(Diagnostic::error(
        codes::UNKNOWN_COMPAT_NAME,
        match suggestion {
            Some(s) => format!(
                "the coverage table has no class, enum or library {name:?} (did you mean {s:?}?)"
            ),
            None => format!("the coverage table has no class, enum or library {name:?}"),
        },
    )
    .at(Location::command("compat.coverage"))
    .with_hint("search instead: `compat.coverage {\"search\": \"...\"}`"))
}

fn coverage_query(args: CoverageArgs) -> Result<Json, Diagnostic> {
    let cov = coverage()?;
    let mut rows = match &args.name {
        Some(name) => named_rows(&cov, name)?,
        None => all_rows(&cov),
    };
    if let Some(text) = &args.search {
        let text = text.to_lowercase();
        rows.retain(|r| r.key.to_lowercase().contains(&text));
    }
    if let Some(status) = args.status {
        rows.retain(|r| r.status == status);
    }
    let limit = args.limit.unwrap_or(50).clamp(1, MAX_ROWS);
    let matched = rows.len();
    rows.truncate(limit);
    Ok(json!({
        "totals": cov.totals,
        "rows": rows,
        "matched": matched,
        "truncated": matched > limit,
    }))
}

/// Registers `compat.coverage` and `scene.dump_tree` (both read tier).
pub fn register_commands<C: EditTarget + 'static>(registry: &mut CommandRegistry<C>) {
    registry.register_typed_output::<CoverageArgs, CoverageOutput, _>(
        "compat.coverage",
        "Query the Roblox-style API coverage table: which classes, members, enums and libraries are implemented, partial (with the gap), stub-error or missing. Give a `name` for one class (with inherited members), enum or library, or `search` and `status` to filter.",
        Tier::Read,
        true,
        |_target, args, _call| coverage_query(args),
    );
    registry.register_typed_output::<DumpArgs, DumpOutput, _>(
        "scene.dump_tree",
        "The entity tree as JSON: each entity's id, name, Roblox-style class, attributes and tags (and with `components: true`, every component), from a `root` or from every root.",
        Tier::Read,
        true,
        |target, args, _call| {
            let out = re_compat::dump::dump_tree(target.world(), &args)?;
            Ok(json!(out))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_lists_its_inherited_members() {
        let cov = coverage().unwrap();
        let class = cov
            .classes
            .iter()
            .find(|c| c.superclass.is_some())
            .expect("the table has a subclass");
        let rows = named_rows(&cov, &class.name).unwrap();
        assert_eq!(rows[0].key, class.name);
        let parent = class.superclass.as_deref().unwrap();
        let parent_members = &cov
            .classes
            .iter()
            .find(|c| c.name == parent)
            .unwrap()
            .members;
        for m in parent_members {
            if class.members.iter().all(|own| own.name != m.name) {
                assert!(
                    rows.iter()
                        .any(|r| r.key == format!("{}.{}", class.name, m.name)
                            && r.from.as_deref() == Some(parent)),
                    "{} lacks inherited {}",
                    class.name,
                    m.name
                );
            }
        }
    }

    #[test]
    fn unknown_names_suggest_the_closest() {
        let cov = coverage().unwrap();
        let first = cov.classes[0].name.clone();
        let typo = format!("{}x", first);
        let err = named_rows(&cov, &typo).unwrap_err();
        assert_eq!(err.code, codes::UNKNOWN_COMPAT_NAME);
        assert!(err.message.contains(&first), "{err:?}");
    }

    #[test]
    fn search_status_and_limit_filter() {
        let all = coverage_query(CoverageArgs {
            limit: Some(MAX_ROWS),
            ..Default::default()
        })
        .unwrap();
        let matched = all["matched"].as_u64().unwrap();
        assert!(matched > 0);
        let missing = coverage_query(CoverageArgs {
            status: Some(Status::Missing),
            limit: Some(MAX_ROWS),
            ..Default::default()
        })
        .unwrap();
        assert!(
            missing["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["status"] == "missing")
        );
        let one = coverage_query(CoverageArgs {
            limit: Some(1),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(one["rows"].as_array().unwrap().len(), 1);
        assert_eq!(one["truncated"], matched > 1);
    }
}
