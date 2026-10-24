//! `real check` and verify's `scan` stage: the script validation tiers of
//! ADR-0024 through `re_script_check` (compile, the static API scan and, with
//! `--types`, the external type checker).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use re_app::AppConfig;
use re_app::verify::{StageReport, VerifyContext, VerifyStage};
use re_core::{Diagnostic, Location};
use re_script_check::corpus::{self, CorpusOptions, CorpusReport};
use re_script_check::types::TypesOptions;
use re_script_check::{Coverage, GapReport};
use serde_json::json;

use crate::codes;

/// Arguments of `real check`.
#[derive(clap::Args, Debug, Default)]
pub struct CheckArgs {
    /// Also run the Luau type checker (luau-lsp) against `engine.d.luau`.
    #[arg(long)]
    pub types: bool,
    /// The type checker to run (default: `real.toml [scripts] checker`).
    #[arg(long)]
    pub checker: Option<PathBuf>,
    /// Check directories of scripts outside a project (the corpus suite) and
    /// print its full report; with no `--scripts-dir`, the directories in
    /// `REAL_SCENE_SCRIPTS`.
    #[arg(long)]
    pub corpus: bool,
    /// A directory of scripts to check (repeatable; default: the project's
    /// `scripts/`).
    #[arg(long = "scripts-dir")]
    pub scripts_dirs: Vec<PathBuf>,
    /// Print the JSON report.
    #[arg(long)]
    pub json: bool,
}

/// The checker to use: `--checker`, else the project's `[scripts] checker`
/// (a path in it is relative to the project).
fn checker(project: &Path, args: &CheckArgs, config: Option<&AppConfig>) -> Option<PathBuf> {
    if let Some(c) = &args.checker {
        return Some(c.clone());
    }
    let c = PathBuf::from(&config?.scripts.checker);
    Some(if c.components().count() > 1 && c.is_relative() {
        project.join(c)
    } else {
        c
    })
}

/// One warning per name the scripts use that the engine does not provide.
fn gap_warnings(gap: &GapReport) -> Vec<Diagnostic> {
    gap.ranked()
        .into_iter()
        .map(|(name, uses)| {
            Diagnostic::warning(
                codes::SCRIPT_API_GAP,
                format!(
                    "scripts use `{name}` ({uses} time{}), which the engine does not provide",
                    if uses == 1 { "" } else { "s" }
                ),
            )
            .with_hint("`real describe --luau-defs` lists the engine API")
        })
        .collect()
}

/// The project's own children of services, as `Service.Child`: instances in
/// its scenes whose parent is a service, and the first folder or script under
/// each `scripts/<Service>/`. The gap report drops these names, because
/// `ReplicatedStorage.Shared` reads an instance, not an API member.
fn service_children(project: &Path) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let scenes = re_app::verify::content_files(project, "scenes", ".scene.json");
    for file in &scenes {
        let Ok(text) = std::fs::read_to_string(project.join(file)) else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(entities) = doc["entities"].as_array() else {
            continue;
        };
        let services: std::collections::HashMap<&str, &str> = entities
            .iter()
            .filter(|e| e["components"].get("ServiceTag").is_some())
            .filter_map(|e| {
                Some((
                    e["id"].as_str()?,
                    e["components"]["ClassMarker"]["class"].as_str()?,
                ))
            })
            .collect();
        for e in entities {
            if let (Some(parent), Some(name)) = (e["parent"].as_str(), e["name"].as_str())
                && let Some(class) = services.get(parent)
            {
                out.insert(format!("{class}.{name}"));
            }
        }
    }
    if let Ok(services) = std::fs::read_dir(project.join("scripts")) {
        for service in services.flatten() {
            let service_name = service.file_name().to_string_lossy().into_owned();
            let Ok(children) = std::fs::read_dir(service.path()) else {
                continue;
            };
            for child in children.flatten() {
                let name = child.file_name().to_string_lossy().into_owned();
                let name = name.split('.').next().unwrap_or_default();
                if !name.is_empty() {
                    out.insert(format!("{service_name}.{name}"));
                }
            }
        }
    }
    out
}

/// Every diagnostic of a corpus report, errors first.
fn diagnostics(report: &CorpusReport) -> Vec<Diagnostic> {
    let mut all = report.compile.failed.clone();
    all.extend(report.scan.parse_failed.iter().cloned());
    if let Some(t) = &report.types {
        all.extend(t.diagnostics.iter().cloned());
    }
    all.extend(gap_warnings(&report.scan.gap));
    all
}

/// Writes `.real/types/roblox.d.luau` for a compat project and returns its
/// path; on failure, warns and returns `None` (the scripts are then checked
/// against the engine API alone).
fn compat_definitions(project: &Path) -> Option<PathBuf> {
    let written = re_compat::ClassRegistry::builtin()
        .map_err(|e| e.to_string())
        .and_then(|reg| {
            re_script_check::roblox::write_definitions(project, &reg).map_err(|e| e.to_string())
        });
    match written {
        Ok(path) => Some(path),
        Err(e) => {
            eprintln!(
                "warning: cannot write {}: {e}; the scripts are checked without the compat API",
                re_script_check::roblox::DEFINITIONS_PATH
            );
            None
        }
    }
}

/// Runs `real check`.
///
/// # Errors
///
/// Never; problems are diagnostics and the exit code.
pub fn run(args: CheckArgs, project: &Path) -> anyhow::Result<ExitCode> {
    // A directory given on the command line needs no project.
    let config = if args.corpus || !args.scripts_dirs.is_empty() {
        None
    } else {
        match AppConfig::load(project) {
            Ok(c) => Some(c),
            Err(d) => {
                eprint!("{}", d.render_human());
                return Ok(ExitCode::from(1));
            }
        }
    };
    let dirs = if !args.scripts_dirs.is_empty() {
        args.scripts_dirs.clone()
    } else if args.corpus {
        corpus::dirs_from_env()
    } else {
        vec![project.join("scripts")]
    };
    if dirs.is_empty() {
        eprintln!(
            "error: `real check --corpus` needs --scripts-dir DIR or {}",
            corpus::ENV_VAR
        );
        return Ok(ExitCode::from(2));
    }
    // A project without scripts has nothing to check.
    let dirs: Vec<PathBuf> = if args.corpus {
        dirs
    } else {
        dirs.into_iter()
            .filter(|d| d.exists() || !args.scripts_dirs.is_empty())
            .collect()
    };
    // In a project the checker reads `.real/types/engine.d.luau`, the same file
    // editors and agents use; outside one it gets a temporary copy.
    let definitions = match (&config, args.types) {
        (Some(_), true) => match re_script::write_definitions(project) {
            Ok(path) => Some(path),
            Err(e) => {
                eprintln!(
                    "warning: cannot write {}: {e}; the type checker uses a temporary copy",
                    re_script::DEFINITIONS_PATH
                );
                None
            }
        },
        _ => None,
    };
    // A compat project's scripts also see the Roblox-style API.
    let compat_definitions = match &config {
        Some(c) if args.types && c.compat.is_some() => compat_definitions(project),
        _ => None,
    };
    let options = CorpusOptions {
        types: args.types,
        types_options: TypesOptions {
            checker: checker(project, &args, config.as_ref()),
            definitions,
            compat_definitions,
        },
    };
    let mut report = corpus::run(&dirs, &options);
    if config.as_ref().is_some_and(|c| c.compat.is_some()) {
        report.scan.gap.drop_children(&service_children(project));
    }
    let diags = diagnostics(&report);
    let errors = diags.iter().filter(|d| d.is_error()).count();
    if args.json {
        let mut value = serde_json::to_value(&report)?;
        value["ok"] = json!(errors == 0);
        if !args.corpus {
            value["diagnostics"] = serde_json::to_value(&diags)?;
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        if !diags.is_empty() {
            eprint!("{}", re_core::render_human_all(&diags));
        }
        let types = match &report.types {
            Some(t) => format!(
                "; types: {} error{}, {} warning{}",
                t.errors,
                if t.errors == 1 { "" } else { "s" },
                t.warnings,
                if t.warnings == 1 { "" } else { "s" }
            ),
            None => String::new(),
        };
        println!(
            "check: {} file{}, {} compiled, {} API name{} missing{types}: {}",
            report.files,
            if report.files == 1 { "" } else { "s" },
            report.compile.ok,
            report.scan.gap.missing.len(),
            if report.scan.gap.missing.len() == 1 {
                ""
            } else {
                "s"
            },
            if errors == 0 { "ok" } else { "FAILED" }
        );
    }
    Ok(if errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Verify's stage 3: the static API scan over the project's scripts. A file the
/// scanner cannot parse is an error; a name the engine does not provide is a
/// warning, and so is a `^` that calls the platform's `pow` (`RE3083`).
pub struct ScanStage;

impl VerifyStage for ScanStage {
    fn name(&self) -> &str {
        "scan"
    }

    fn run(&self, ctx: &VerifyContext<'_>) -> StageReport {
        let files = re_app::verify::content_files(ctx.project, "scripts", ".luau");
        let mut usage = re_script_check::ApiUsage::default();
        let mut diags = Vec::new();
        for file in &files {
            match std::fs::read_to_string(ctx.project.join(file)) {
                Ok(source) => match re_script_check::scan_source(&source, file) {
                    Ok(u) => {
                        usage.merge(&u);
                        diags.extend(re_script_check::pow_warnings(&source, file));
                    }
                    Err(d) => diags.push(d),
                },
                Err(e) => diags.push(
                    Diagnostic::error(
                        re_script::codes::SCRIPT_READ,
                        format!("cannot read {file}: {e}"),
                    )
                    .at(Location::file(file)),
                ),
            }
        }
        // A compat project's scripts are checked against the compatibility
        // table as well as the native API.
        let compat = std::fs::read_to_string(ctx.project.join("real.toml"))
            .ok()
            .and_then(|t| {
                re_compat::CompatConfig::from_project_toml(&t)
                    .ok()
                    .flatten()
            })
            .is_some();
        let coverage = if compat {
            Coverage::compat()
        } else {
            Coverage::native()
        };
        let mut gap = GapReport::new(&usage, &coverage);
        if compat {
            gap.drop_children(&service_children(ctx.project));
        }
        diags.extend(gap_warnings(&gap));
        StageReport::new(
            "scan",
            diags,
            json!({ "scripts": files.len(), "covered": gap.covered, "missing": gap.missing }),
        )
    }
}
