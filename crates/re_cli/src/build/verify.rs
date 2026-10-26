//! `real verify --package DIR` (ADR-0037): runs the packaged game as a child
//! process and compares each run's state hash with a run of the project.

use std::path::Path;
use std::process::{Command, ExitCode};

use re_app::package::Archive;
use re_app::{App, AppConfig};
use re_core::{Diagnostic, Location};
use serde::Serialize;
use serde_json::{Value as Json, json};

use crate::codes;

/// Frames of the boot run.
pub const BOOT_FRAMES: u64 = 120;

/// One compared run.
#[derive(Debug, Serialize)]
pub struct PackagedRun {
    /// `boot`, or the replay file.
    pub run: String,
    /// The packaged game's state hash.
    pub packaged: Option<String>,
    /// The project's state hash.
    pub project: Option<String>,
    /// True when they are equal.
    pub same: bool,
}

/// The `real verify --package --json` report.
#[derive(Debug, Serialize)]
pub struct PackageReport {
    /// True when every run matched and nothing failed.
    pub ok: bool,
    /// The game folder.
    pub package: String,
    /// The runs.
    pub runs: Vec<PackagedRun>,
    /// What went wrong.
    pub diagnostics: Vec<Diagnostic>,
}

/// A run to compare: its name, the packaged game's arguments, and the project
/// run's frames and replay.
struct Planned {
    name: String,
    args: Vec<String>,
    frames: Option<u64>,
    replay: Option<re_types::ReplayDoc>,
}

/// Runs the packaged game with `args` (and a fresh `--data-dir`), returning
/// its JSON report.
fn run_packaged(exe: &Path, args: &[&str]) -> Result<Json, Diagnostic> {
    static RUNS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let data = std::env::temp_dir().join(format!("real-verify-package-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let output = Command::new(exe)
        .args(args)
        .args(["--headless", "--json", "--data-dir"])
        .arg(&data)
        .output();
    let _ = std::fs::remove_dir_all(&data);
    let output = output.map_err(|e| {
        Diagnostic::error(
            codes::PACKAGE_DIFFERS,
            format!("cannot run {}: {e}", exe.display()),
        )
    })?;
    serde_json::from_slice(&output.stdout).map_err(|_| {
        Diagnostic::error(
            codes::PACKAGE_DIFFERS,
            format!(
                "{} {} printed no report ({}): {}",
                exe.display(),
                args.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        )
    })
}

/// The packaged run's diagnostics: errors stay errors, and a case-only match
/// (`RE0603`) is an error here, so it is fixed before the game ships.
fn packaged_problems(report: &Json, run: &str) -> Vec<Diagnostic> {
    let diags: Vec<Diagnostic> = report
        .get("diagnostics")
        .cloned()
        .and_then(|d| serde_json::from_value(d).ok())
        .unwrap_or_default();
    diags
        .into_iter()
        .filter_map(|mut d| {
            if d.code == re_app::codes::PACKAGE_CASE_MATCH {
                d.severity = re_core::Severity::Error;
            }
            d.is_error().then(|| {
                d.with_related(Diagnostic::note(
                    codes::PACKAGE_DIFFERS,
                    format!("reported by the packaged game ({run})"),
                ))
            })
        })
        .collect()
}

/// Runs the project in process, headless and with no saved data.
fn run_project(
    project: &Path,
    config: &AppConfig,
    frames: Option<u64>,
    replay: Option<re_types::ReplayDoc>,
) -> Result<String, Diagnostic> {
    let config = AppConfig {
        headless: true,
        ..config.clone()
    };
    let mut app = App::open(project, config)?;
    Ok(app.run_headless(frames, replay)?.state_hash)
}

/// Compares the packaged game in `dir` with the project.
pub fn check(dir: &Path, project: &Path) -> PackageReport {
    let mut report = PackageReport {
        ok: false,
        package: dir.display().to_string(),
        runs: Vec::new(),
        diagnostics: Vec::new(),
    };
    let config = match AppConfig::load(project) {
        Ok(c) => c,
        Err(d) => {
            report.diagnostics.push(d);
            return report;
        }
    };
    // The game is laid out for this machine's OS (a macOS app keeps the
    // archive in its resources).
    let layout = super::bundle::layout(&config, super::target_os(&crate::game::target_triple()));
    match Archive::open(&dir.join(&layout.archive)) {
        Ok(archive) => {
            if let Err(ds) = archive.check_all() {
                report.diagnostics.extend(ds);
            }
        }
        Err(d) => {
            report.diagnostics.push(d);
            return report;
        }
    }
    let exe = dir.join(&layout.exe);
    let mut runs = vec![Planned {
        name: "boot".into(),
        args: vec!["--frames".into(), BOOT_FRAMES.to_string()],
        frames: Some(BOOT_FRAMES),
        replay: None,
    }];
    for file in re_app::verify::content_files(project, "tests/replays", ".replay.json") {
        let path = project.join(&file);
        match re_input::load_replay(&path) {
            Ok(doc) => {
                let abs = std::path::absolute(&path).unwrap_or(path);
                runs.push(Planned {
                    name: file,
                    args: vec!["--replay".into(), abs.display().to_string()],
                    frames: None,
                    replay: Some(doc),
                });
            }
            Err(d) => report.diagnostics.push(d),
        }
    }
    for Planned {
        name,
        args,
        frames,
        replay,
    } in runs
    {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let packaged = match run_packaged(&exe, &args) {
            Ok(json) => {
                report.diagnostics.extend(packaged_problems(&json, &name));
                json.get("state_hash")
                    .and_then(Json::as_str)
                    .map(str::to_owned)
            }
            Err(d) => {
                report.diagnostics.push(d);
                None
            }
        };
        let ours = match run_project(project, &config, frames, replay) {
            Ok(h) => Some(h),
            Err(d) => {
                report.diagnostics.push(d);
                None
            }
        };
        let same = packaged.is_some() && packaged == ours;
        if !same && let (Some(p), Some(o)) = (&packaged, &ours) {
            report.diagnostics.push(
                Diagnostic::error(
                    codes::PACKAGE_DIFFERS,
                    format!(
                        "{name}: the packaged game ended with state hash {p}, the project with {o}"
                    ),
                )
                .at(Location::file(if name == "boot" {
                    "real.toml".to_owned()
                } else {
                    name.clone()
                })),
            );
        }
        report.runs.push(PackagedRun {
            run: name,
            packaged,
            project: ours,
            same,
        });
    }
    report.ok =
        !report.diagnostics.iter().any(Diagnostic::is_error) && report.runs.iter().all(|r| r.same);
    report
}

/// Runs `real verify --package DIR` and prints the report.
pub fn run(dir: &Path, project: &Path, as_json: bool) -> ExitCode {
    let report = check(dir, project);
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!(report)).unwrap_or_default()
        );
    } else {
        for r in &report.runs {
            println!("{:<40} {}", r.run, if r.same { "same" } else { "DIFFERS" });
        }
        crate::print_diagnostics(&report.diagnostics);
        println!(
            "{}",
            if report.ok {
                "verify --package: ok"
            } else {
                "verify --package: FAILED"
            }
        );
    }
    if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
