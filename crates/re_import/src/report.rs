//! [`ImportReport`]: what an import did, path by path (frozen at WP16 PR
//! "importer"; WP17's `project.import` tool returns it).

use std::collections::BTreeMap;

use re_core::Diagnostic;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The input forms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum InputFormat {
    /// A Rojo-style project (`*.project.json` and the files it maps).
    Rojo,
    /// A Studio command-bar loader script, read without running it.
    Loader,
    /// A place or model file (`.rbxl`, `.rbxlx`, `.rbxm`, `.rbxmx`).
    Place,
}

/// One instance the import created, adopted or removed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    /// The names from the service down. Each element is one `Name`, dots and
    /// all: `["ServerStorage", "Old", "A.B"]` is three instances deep.
    pub path: Vec<String>,
    /// The class.
    pub class: String,
    /// The entity's stable id in the scene.
    pub id: String,
    /// The line of the input that declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// For an adopted script: its source changed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub updated: bool,
}

/// An instance of the input that was not imported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Failure {
    /// The names from the service down.
    pub path: Vec<String>,
    /// The class the input gives it.
    pub class: String,
    /// The line of the input that declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Why.
    pub message: String,
}

/// A loader statement the importer skipped without running it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Unrecognised {
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub column: u32,
    /// The statement's first line, shortened.
    pub statement: String,
}

/// Files written to and deleted from the project, relative to it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FileChanges {
    pub written: Vec<String>,
    pub deleted: Vec<String>,
}

/// What an import did. Paths are the authoritative key: a later import of
/// the same input matches instances by path (and class), so re-importing is
/// idempotent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImportReport {
    /// The input as given.
    pub input: String,
    pub format: InputFormat,
    /// The project directory as given.
    pub project: String,
    /// Nothing was written.
    pub dry_run: bool,
    /// The project did not exist and the import creates it.
    pub new_project: bool,
    /// Instances the import created, in the input's order.
    pub created: Vec<Item>,
    /// Instances that already existed and were updated in place.
    pub adopted: Vec<Item>,
    /// Instances an earlier import created that the input no longer declares.
    pub removed: Vec<Item>,
    /// Instances that could not be imported; the rest were.
    pub failed: Vec<Failure>,
    /// Loader statements that were skipped, not run.
    pub unrecognised: Vec<Unrecognised>,
    /// The instances the input declares, by class (services excluded).
    pub classes: BTreeMap<String, u32>,
    pub files: FileChanges,
    /// Warnings: scripts that do not compile, duplicate entries, skipped
    /// statements. Each is a diagnostic in `real`'s JSON form.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub diagnostics: Vec<Diagnostic>,
}

impl ImportReport {
    /// True when every instance was imported and nothing is an error.
    pub fn ok(&self) -> bool {
        self.failed.is_empty() && !self.diagnostics.iter().any(Diagnostic::is_error)
    }

    /// A short human summary, one line per category plus the failures.
    pub fn render_human(&self) -> String {
        let mut out = format!(
            "{} {} into {}{}\n  created {}, adopted {} ({} updated), removed {}, failed {}\n",
            if self.dry_run {
                "would import"
            } else {
                "imported"
            },
            self.input,
            self.project,
            if self.new_project {
                " (new project)"
            } else {
                ""
            },
            self.created.len(),
            self.adopted.len(),
            self.adopted.iter().filter(|i| i.updated).count(),
            self.removed.len(),
            self.failed.len(),
        );
        if !self.classes.is_empty() {
            let classes: Vec<String> = self
                .classes
                .iter()
                .map(|(c, n)| format!("{n} {c}"))
                .collect();
            out.push_str(&format!("  declares {}\n", classes.join(", ")));
        }
        for f in &self.failed {
            out.push_str(&format!(
                "  failed {} ({}): {}\n",
                display_path(&f.path),
                f.class,
                f.message
            ));
        }
        if !self.unrecognised.is_empty() {
            out.push_str(&format!(
                "  skipped {} statement(s) the importer does not run:\n",
                self.unrecognised.len()
            ));
            for u in &self.unrecognised {
                out.push_str(&format!("    line {}: {}\n", u.line, u.statement));
            }
        }
        for d in &self.diagnostics {
            out.push_str(&d.render_human());
            out.push('\n');
        }
        out
    }
}

/// A path for people: names joined by `.`, with any name that holds a `.`
/// (or is empty) quoted, so it is never read as two instances.
pub fn display_path(path: &[String]) -> String {
    path.iter()
        .map(|n| {
            if n.is_empty() || n.contains('.') || n.contains('"') {
                format!("{n:?}")
            } else {
                n.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}
