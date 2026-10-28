//! The corpus validation suite (ADR-0024): every `.lua`/`.luau` file under some
//! directories, through the validation tiers, into one JSON report.
//!
//! 1. **Compile** with [`re_script::compile_check`]: the gate.
//! 2. **Static scan** ([`scan`](mod@crate::scan)) and the gap report against the native
//!    coverage table: a report, never a failure.
//! 3. **Types** ([`crate::types`]), when asked for: advisory here.
//!
//! The directories come from `real check --corpus --scripts-dir <DIR>` or from the
//! `REAL_SCENE_SCRIPTS` environment variable ([`dirs_from_env`]). Corpus content
//! is never committed to the repository; only the synthetic fixtures in
//! `tests/fixtures/` are.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};

use crate::gap::{Coverage, GapReport};
use crate::scan::{self, ApiUsage};
use crate::types::{self, TypesOptions};

/// The environment variable that lists corpus directories, separated like
/// `PATH` (`:` on Unix, `;` on Windows).
pub const ENV_VAR: &str = "REAL_SCENE_SCRIPTS";

/// What [`run`] does besides compiling and scanning.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CorpusOptions {
    /// Run the type checker (tier 3) over each directory.
    pub types: bool,
    /// How to find and call the checker.
    pub types_options: TypesOptions,
}

/// Tier 1 results.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompileReport {
    /// Files that compiled.
    pub ok: usize,
    /// The diagnostics of files that did not.
    pub failed: Vec<Diagnostic>,
}

/// Tier 2 results, summed over every file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanReport {
    /// Members read or called on known receivers, as `receiver.member`.
    pub by_member: BTreeMap<String, usize>,
    /// Members on receivers the scan cannot name, as `.member`.
    pub unknown_receivers: BTreeMap<String, usize>,
    /// `require` calls with a computed target.
    pub dynamic_requires: usize,
    /// Global reads.
    pub globals: BTreeMap<String, usize>,
    /// `game:GetService` lookups.
    pub services: BTreeMap<String, usize>,
    /// `Instance.new` classes.
    pub instance_new: BTreeMap<String, usize>,
    /// `Enum.X.Y` items.
    pub enums: BTreeMap<String, usize>,
    /// Datatype constructors, as `Type.constructor`.
    pub constructors: BTreeMap<String, usize>,
    /// The gap report against the native coverage table.
    pub gap: GapReport,
    /// Files the scan could not parse (`RE3080`).
    pub parse_failed: Vec<Diagnostic>,
}

/// Tier 3 results.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TypesReport {
    /// The checker that ran, or `None` when none was found.
    pub checker: Option<String>,
    /// Error diagnostics (`RE3081`, and `RE3090`/`RE3091` when the checker could
    /// not run).
    pub errors: usize,
    /// Warning diagnostics (`RE3082`).
    pub warnings: usize,
    /// Every diagnostic, in file order.
    pub diagnostics: Vec<Diagnostic>,
}

/// The suite's report, written as JSON.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CorpusReport {
    /// The directories, as given.
    pub dirs: Vec<String>,
    /// The Luau files found.
    pub files: usize,
    /// Tier 1.
    pub compile: CompileReport,
    /// Tier 2.
    pub scan: ScanReport,
    /// Tier 3, when asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<TypesReport>,
}

impl CorpusReport {
    /// True when every file compiled (the gate).
    pub fn compile_passed(&self) -> bool {
        self.compile.failed.is_empty()
    }

    /// The report as pretty-printed JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("the report serializes")
    }
}

/// The directories listed in [`ENV_VAR`], or none when it is unset or empty.
pub fn dirs_from_env() -> Vec<PathBuf> {
    match std::env::var_os(ENV_VAR) {
        Some(v) if !v.is_empty() => std::env::split_paths(&v)
            .filter(|p| !p.as_os_str().is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Every `.lua` and `.luau` file under `dir`, recursively, sorted. Symlinked
/// directories are not followed; unreadable directories are skipped.
pub fn luau_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "lua" || e == "luau") && path.is_file()
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn display(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Runs the suite over `dirs`.
pub fn run(dirs: &[PathBuf], options: &CorpusOptions) -> CorpusReport {
    let mut report = CorpusReport {
        dirs: dirs.iter().map(|d| display(d)).collect(),
        ..CorpusReport::default()
    };
    let mut usage = ApiUsage::default();
    // Generator scripts have an API of their own.
    let mut generator_usage = ApiUsage::default();
    let mut types = options.types.then(TypesReport::default);
    if let Some(t) = &mut types {
        t.checker = types::find_checker(&options.types_options).map(|p| display(&p));
    }
    // A file under two of the directories is checked once.
    let mut seen = std::collections::HashSet::new();
    for dir in dirs {
        if !dir.is_dir() {
            // A mistyped directory must not pass as an empty corpus.
            report.compile.failed.push(
                Diagnostic::error(
                    re_script::codes::SCRIPT_READ,
                    if dir.exists() {
                        format!("the corpus path {} is not a directory", display(dir))
                    } else {
                        format!("the corpus directory {} does not exist", display(dir))
                    },
                )
                .at(Location::File {
                    file: display(dir),
                    line: None,
                    column: None,
                }),
            );
            continue;
        }
        let files: Vec<PathBuf> = luau_files(dir)
            .into_iter()
            .filter(|f| seen.insert(std::fs::canonicalize(f).unwrap_or_else(|_| f.clone())))
            .collect();
        report.files += files.len();
        for file in &files {
            let name = display(file);
            let source = match std::fs::read(file) {
                Ok(s) => String::from_utf8_lossy(&s).into_owned(),
                Err(e) => {
                    report.compile.failed.push(
                        Diagnostic::error(
                            re_script::codes::SCRIPT_READ,
                            format!("cannot read {name}: {e}"),
                        )
                        .at(Location::File {
                            file: name,
                            line: None,
                            column: None,
                        }),
                    );
                    continue;
                }
            };
            let diags = re_script::compile_check(&source, &name);
            if diags.is_empty() {
                report.compile.ok += 1;
            } else {
                report.compile.failed.extend(diags);
            }
            match scan::scan_source(&source, &name) {
                Ok(u) if types::is_generator_script(file) => generator_usage.merge(&u),
                Ok(u) => usage.merge(&u),
                Err(d) => report.scan.parse_failed.push(d),
            }
        }
        if let Some(t) = &mut types {
            for mut d in types::run(dir, &files, &options.types_options) {
                // Locations are relative to the directory; make them match the
                // compile diagnostics.
                if let Some(Location::File { file, .. }) = &mut d.location
                    && !Path::new(file.as_str()).is_absolute()
                {
                    *file = display(&dir.join(file.as_str()));
                }
                t.diagnostics.push(d);
            }
        }
    }
    if let Some(t) = &mut types {
        t.errors = t
            .diagnostics
            .iter()
            .filter(|d| d.severity == re_core::Severity::Error)
            .count();
        t.warnings = t.diagnostics.len() - t.errors;
    }
    report.scan.gap = GapReport::new(&usage, &Coverage::compat());
    report
        .scan
        .gap
        .merge(GapReport::new(&generator_usage, &Coverage::generator()));
    usage.merge(&generator_usage);
    report.scan.by_member = usage.by_member;
    report.scan.unknown_receivers = usage.unknown_receivers;
    report.scan.dynamic_requires = usage.dynamic_requires;
    report.scan.globals = usage.globals;
    report.scan.services = usage.services;
    report.scan.instance_new = usage.instance_new;
    report.scan.enums = usage.enums;
    report.scan.constructors = usage.constructors;
    report.types = types;
    report
}
