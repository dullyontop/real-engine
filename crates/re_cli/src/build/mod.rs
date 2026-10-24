//! `real build` and `real verify --package` (ADR-0037): turn a project into a
//! standalone game folder, and check that the packaged game plays as the
//! project does.

pub mod pack;
pub mod verify;
pub mod web;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use re_app::AppConfig;
use re_app::package::{ARCHIVE_NAME, RuntimeVersions};
use re_core::Diagnostic;
use serde::Serialize;

use crate::codes;

/// The file that marks a folder `real build` wrote, listing what it wrote.
pub const MARKER: &str = ".real-build";

/// `real build`.
#[derive(clap::Args, Debug)]
pub struct BuildArgs {
    /// The folder to write the game into: missing, empty, or one an earlier
    /// `real build` wrote.
    #[arg(long)]
    out: PathBuf,
    /// The target to build for, such as `x86_64-unknown-linux-gnu` (default:
    /// this machine's), or `web` for a folder a static web server hosts. A
    /// runtime for another OS needs `--runtime`.
    #[arg(long)]
    target: Option<String>,
    /// Use this prebuilt game runtime instead of building one with cargo (for
    /// `--target web`, a folder holding a web runtime).
    #[arg(long, value_name = "PATH")]
    runtime: Option<PathBuf>,
    /// Write only `game.rpak`.
    #[arg(long)]
    archive_only: bool,
    /// Print the build report as JSON.
    #[arg(long)]
    json: bool,
}

/// The `real build --json` report.
#[derive(Debug, Default, Serialize)]
pub struct BuildReport {
    /// True when the build finished without errors.
    pub ok: bool,
    /// The game folder's files, relative to `--out`.
    pub files: Vec<String>,
    /// Every archive entry with its form, sizes and compression.
    pub entries: Vec<re_app::package::archive::WrittenEntry>,
    /// The packing decisions.
    #[serde(flatten)]
    pub pack: pack::PackReport,
    /// The target the runtime is for.
    pub target: Option<String>,
    /// Steps not done for this target, and why.
    pub notes: Vec<String>,
}

/// The executable's name: `[project] name` limited to `A–Z a–z 0–9 _ -`.
pub fn executable_name(name: &str) -> String {
    let n: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .collect();
    if n.is_empty() { "game".to_owned() } else { n }
}

/// The OS part of a target triple: `linux`, `macos` or `windows`.
fn target_os(triple: &str) -> &'static str {
    if triple.contains("windows") {
        "windows"
    } else if triple.contains("apple") {
        "macos"
    } else {
        "linux"
    }
}

/// Runs `real build`.
pub fn run(args: BuildArgs, project: &Path) -> ExitCode {
    let (report, ok) = build(&args, project);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        crate::print_diagnostics(&report.pack.diagnostics);
        for s in &report.pack.created_sidecars {
            println!("created {s} (commit it with the asset)");
        }
        for note in &report.notes {
            println!("note: {note}");
        }
        if ok {
            let stored: u64 = report.entries.iter().map(|e| e.stored_size).sum();
            println!(
                "built {} ({} entries, {:.1} MB) into {}",
                report.files.join(", "),
                report.entries.len(),
                stored as f64 / 1e6,
                args.out.display()
            );
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn build(args: &BuildArgs, project: &Path) -> (BuildReport, bool) {
    let mut report = BuildReport::default();
    let stop = |report: &mut BuildReport, d: Diagnostic| {
        report.pack.diagnostics.push(d);
        report.ok = false;
        false
    };
    let config = match AppConfig::load(project) {
        Ok(c) => c,
        Err(d) => {
            let ok = stop(&mut report, d);
            return (report, ok);
        }
    };
    let earlier = match check_out(&args.out) {
        Ok(listed) => listed,
        Err(d) => {
            let ok = stop(&mut report, d);
            return (report, ok);
        }
    };

    // 1. The checks `real verify` runs on the files.
    let scan = crate::tooling::check::ScanStage;
    let stages = re_app::verify::content_stages(project, &config, &[&scan]);
    for stage in stages {
        report.pack.diagnostics.extend(stage.diagnostics);
    }

    // 2. The archive.
    let mut web_runtime = None;
    let mut errors = report
        .pack
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .count();
    if errors == 0 {
        let (writer, pack_report) = pack::pack(project, &config);
        report.pack.package = pack_report.package.clone();
        report.pack.assets = pack_report.assets;
        report.pack.created_sidecars = pack_report.created_sidecars;
        report.pack.dropped = pack_report.dropped;
        report.pack.diagnostics.extend(pack_report.diagnostics);
        errors = report
            .pack
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .count();
        if errors == 0 {
            // The web runtime is ready before `out` is cleared, so a missing
            // toolchain leaves an earlier build in place.
            if args.target.as_deref().is_some_and(web::is_web) && !args.archive_only {
                let runtime = web::runtime(
                    &args.out,
                    args.runtime.as_deref(),
                    &mut report.pack.diagnostics,
                );
                match runtime {
                    Ok(dir) => web_runtime = Some(dir),
                    Err(d) => {
                        let ok = stop(&mut report, d);
                        return (report, ok);
                    }
                }
            }
            let written = match prepare_out(&args.out, &earlier) {
                Ok(()) => writer.write(&archive_path(&args.out)),
                Err(d) => {
                    let ok = stop(&mut report, d);
                    return (report, ok);
                }
            };
            match written {
                Ok(entries) => report.entries = entries,
                Err(e) => {
                    let ok = stop(
                        &mut report,
                        Diagnostic::error(
                            codes::BUNDLE_FAILED,
                            format!("cannot write {ARCHIVE_NAME}: {e}"),
                        ),
                    );
                    return (report, ok);
                }
            }
        }
    }
    if errors > 0 {
        let ok = stop(
            &mut report,
            Diagnostic::error(
                codes::BUILD_STOPPED,
                format!(
                    "real build stopped: the project has {errors} error{}",
                    if errors == 1 { "" } else { "s" }
                ),
            ),
        );
        return (report, ok);
    }
    report.files.push(ARCHIVE_NAME.to_owned());
    if args.archive_only {
        report.ok = finish(&args.out, &mut report);
        let ok = report.ok;
        return (report, ok);
    }

    // 3. The runtime.
    let triple = args
        .target
        .clone()
        .unwrap_or_else(crate::game::target_triple);
    report.target = Some(triple.clone());
    if let Some(runtime) = web_runtime {
        report.target = Some(web::TARGET.to_owned());
        report.ok = web::lay_out(&args.out, &runtime, &config, &mut report)
            && finish(&args.out, &mut report);
        let ok = report.ok;
        return (report, ok);
    }
    let runtime = match &args.runtime {
        Some(path) => check_runtime(path, &triple).map(|()| path.clone()),
        None => build_runtime(&triple),
    };
    let runtime = match runtime {
        Ok(p) => p,
        Err(d) => {
            let ok = stop(&mut report, d);
            return (report, ok);
        }
    };

    // 4. The game folder.
    match lay_out(&args.out, &runtime, &triple, &config, project, &mut report) {
        Ok(()) => {}
        Err(d) => {
            let ok = stop(&mut report, d);
            return (report, ok);
        }
    }
    report.ok = finish(&args.out, &mut report);
    let ok = report.ok;
    (report, ok)
}

/// Where the archive goes in the game folder: beside the executable in every
/// layout of this version (a macOS app keeps it in `Contents/Resources/` from
/// WP19 PR "bundles").
fn archive_path(out: &Path) -> PathBuf {
    out.join(ARCHIVE_NAME)
}

/// Checks that `out` is a folder `real build` may write: missing, empty, or one
/// an earlier build marked. Returns the files that build listed.
fn check_out(out: &Path) -> Result<Vec<String>, Diagnostic> {
    let refuse = |why: String| {
        Diagnostic::error(codes::BUNDLE_FAILED, why)
            .with_hint("choose a new or empty folder, or the folder of an earlier `real build`")
    };
    if !out.exists() {
        return Ok(Vec::new());
    }
    if !out.is_dir() {
        return Err(refuse(format!("--out {} is not a folder", out.display())));
    }
    let empty = std::fs::read_dir(out)
        .map_err(|e| refuse(format!("cannot read {}: {e}", out.display())))?
        .next()
        .is_none();
    if empty {
        return Ok(Vec::new());
    }
    let Ok(listed) = std::fs::read_to_string(out.join(MARKER)) else {
        return Err(refuse(format!(
            "--out {} holds files real build did not write",
            out.display()
        )));
    };
    // Only plain relative names inside `out`, as a build writes them.
    Ok(listed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('/') && !l.contains("..") && !l.contains(':'))
        .map(str::to_owned)
        .collect())
}

/// Creates `out`, or removes the files an earlier build listed in it.
fn prepare_out(out: &Path, earlier: &[String]) -> Result<(), Diagnostic> {
    std::fs::create_dir_all(out).map_err(|e| {
        Diagnostic::error(
            codes::BUNDLE_FAILED,
            format!("cannot create {}: {e}", out.display()),
        )
    })?;
    for name in earlier {
        let path = out.join(name);
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
    let _ = std::fs::remove_file(out.join(MARKER));
    Ok(())
}

/// Writes the marker listing the build's files.
fn finish(out: &Path, report: &mut BuildReport) -> bool {
    let mut listed = report.files.join("\n");
    listed.push('\n');
    if let Err(e) = std::fs::write(out.join(MARKER), listed) {
        report.pack.diagnostics.push(Diagnostic::error(
            codes::BUNDLE_FAILED,
            format!("cannot write {}: {e}", out.join(MARKER).display()),
        ));
        return false;
    }
    true
}

/// The engine workspace `real` was built from, when it is still there.
fn engine_workspace() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    (root.join("Cargo.toml").is_file() && root.join("crates/re_cli/src/game.rs").is_file())
        .then_some(root)
}

/// Builds the game runtime for `triple` with cargo in the engine workspace.
fn build_runtime(triple: &str) -> Result<PathBuf, Diagnostic> {
    let failed = |msg: String| Diagnostic::error(codes::RUNTIME_FAILED, msg);
    let Some(ws) = engine_workspace() else {
        return Err(failed("this `real` cannot build a game runtime: the engine's source is not where it was built".into())
            .with_hint("pass `--runtime PATH` with a runtime of this engine version, or `--archive-only`"));
    };
    let host = crate::game::target_triple();
    if target_os(triple) != target_os(&host) {
        return Err(
            failed(format!("a runtime for {triple} cannot be built on {host}")).with_hint(
                "build on that platform, or pass `--runtime PATH` with a runtime built there",
            ),
        );
    }
    let target_dir = ws.join("target").join("package");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    eprintln!("building the game runtime for {triple} (cargo --profile packaged)...");
    let mut cmd = Command::new(cargo);
    cmd.current_dir(&ws)
        .args(["build", "--profile", "packaged", "--locked", "-p", "re_cli"])
        .args(["--no-default-features", "--features", "game"])
        .args(["--target", triple])
        .arg("--target-dir")
        .arg(&target_dir);
    if target_os(triple) == "windows" {
        // Only the target gets the static C runtime, not build scripts.
        cmd.env(
            format!(
                "CARGO_TARGET_{}_RUSTFLAGS",
                triple.to_ascii_uppercase().replace('-', "_")
            ),
            "-C target-feature=+crt-static",
        );
    }
    // Progress goes to standard error, so `--json` output stays one document.
    let status = cmd
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|e| failed(format!("cannot run cargo: {e}")))?;
    if !status.success() {
        return Err(failed(format!(
            "cargo could not build the game runtime ({status})"
        )));
    }
    let exe = if target_os(triple) == "windows" {
        "real.exe"
    } else {
        "real"
    };
    let built = target_dir.join(triple).join("packaged").join(exe);
    if !built.is_file() {
        return Err(failed(format!(
            "cargo finished but {} is missing",
            built.display()
        )));
    }
    Ok(built)
}

/// Checks a prebuilt runtime's `--runtime-info` against this engine.
fn check_runtime(path: &Path, triple: &str) -> Result<(), Diagnostic> {
    let failed = |msg: String| Diagnostic::error(codes::RUNTIME_FAILED, msg);
    let output = Command::new(path)
        .arg("--runtime-info")
        .output()
        .map_err(|e| failed(format!("cannot run {} --runtime-info: {e}", path.display())))?;
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        failed(format!(
            "{} is not a game runtime: its --runtime-info did not print the versions",
            path.display()
        ))
    })?;
    let ours = serde_json::to_value(RuntimeVersions::current()).unwrap_or_default();
    let mut differ = Vec::new();
    for key in ["engine", "luau", "bytecode_version", "compiler"] {
        if info.get(key) != ours.get(key) {
            differ.push(format!(
                "{key} {} (this build needs {})",
                info.get(key).unwrap_or(&serde_json::Value::Null),
                ours.get(key).unwrap_or(&serde_json::Value::Null)
            ));
        }
    }
    if info
        .get("target")
        .and_then(|t| t.as_str())
        .is_some_and(|t| t != triple)
    {
        differ.push(format!(
            "target {} (this build is for {triple})",
            info["target"]
        ));
    }
    if differ.is_empty() {
        Ok(())
    } else {
        Err(failed(format!(
            "the runtime at {} does not match: {}",
            path.display(),
            differ.join(", ")
        )))
    }
}

/// Lays out the game folder for the target's OS around the archive.
fn lay_out(
    out: &Path,
    runtime: &Path,
    triple: &str,
    config: &AppConfig,
    project: &Path,
    report: &mut BuildReport,
) -> Result<(), Diagnostic> {
    let failed = |what: String, e: std::io::Error| {
        Diagnostic::error(codes::BUNDLE_FAILED, format!("cannot write {what}: {e}"))
    };
    let name = executable_name(&config.project.name);
    let os = target_os(triple);
    let exe = if os == "windows" {
        format!("{name}.exe")
    } else {
        name.clone()
    };
    std::fs::copy(runtime, out.join(&exe)).map_err(|e| failed(exe.clone(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(out.join(&exe), std::fs::Permissions::from_mode(0o755))
            .map_err(|e| failed(exe.clone(), e))?;
    }
    report.files.push(exe);
    let pc = config.package.clone().unwrap_or_default();
    match os {
        "linux" => {
            let display = pc.display_name.clone().unwrap_or_else(|| config.project.name.clone());
            let mut desktop = format!(
                "[Desktop Entry]\nType=Application\nName={display}\nExec={name}\nTerminal=false\nCategories=Game;\n"
            );
            if let Some(icon) = &pc.icon {
                let png = format!("{name}.png");
                std::fs::copy(project.join(icon), out.join(&png)).map_err(|e| failed(png.clone(), e))?;
                report.files.push(png);
                desktop.push_str(&format!("Icon={name}\n"));
            }
            let file = format!("{name}.desktop");
            std::fs::write(out.join(&file), desktop).map_err(|e| failed(file.clone(), e))?;
            report.files.push(file);
        }
        other => report.notes.push(format!(
            "the {other} layout is a plain folder in this version; the platform bundle and the third-party notices arrive with WP19 PR \"bundles\""
        )),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_names_keep_only_safe_characters() {
        assert_eq!(executable_name("My Island!"), "MyIsland");
        assert_eq!(executable_name("plaza_2-b"), "plaza_2-b");
        assert_eq!(executable_name("***"), "game");
    }

    #[test]
    fn out_must_be_empty_or_an_earlier_build() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dist");
        assert!(check_out(&out).unwrap().is_empty());
        prepare_out(&out, &[]).unwrap();
        std::fs::write(out.join("mine.txt"), "x").unwrap();
        assert_eq!(check_out(&out).unwrap_err().code, codes::BUNDLE_FAILED);
        std::fs::write(out.join(MARKER), "mine.txt\n../escape\n").unwrap();
        let earlier = check_out(&out).unwrap();
        assert_eq!(earlier, ["mine.txt"]);
        prepare_out(&out, &earlier).unwrap();
        assert!(!out.join("mine.txt").exists());
    }
}
