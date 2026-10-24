//! `real build` and `real verify --package` (ADR-0037): turn a project into a
//! standalone game folder, and check that the packaged game plays as the
//! project does.

pub mod bundle;
pub mod notices;
pub mod pack;
pub mod verify;
pub mod web;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use re_app::AppConfig;
use re_app::package::{ARCHIVE_NAME, PackageWriter, RuntimeVersions};
use re_core::Diagnostic;
use serde::Serialize;

use crate::codes;
use notices::{NOTICES_FILE, NoticesReport};

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
    /// this machine's, or the `--runtime`'s own), or `web` for a folder a
    /// static web server hosts. A runtime for another OS needs `--runtime`.
    #[arg(long)]
    target: Option<String>,
    /// Use this prebuilt game runtime instead of building one with cargo: the
    /// `real` of a runtime folder (from `--runtime-only` or a release), which
    /// holds its notices. For `--target web`, a folder holding a web runtime.
    #[arg(long, value_name = "PATH")]
    runtime: Option<PathBuf>,
    /// Write only `game.rpak`.
    #[arg(long)]
    archive_only: bool,
    /// Write only a runtime folder for the target: the game runtime, its
    /// `runtime.json` and its notices, for `--runtime` on another machine. No
    /// project is read.
    #[arg(long, conflicts_with_all = ["runtime", "archive_only"])]
    runtime_only: bool,
    /// Print the build report as JSON.
    #[arg(long)]
    json: bool,
}

/// The `real build --json` report.
#[derive(Debug, Default, Serialize)]
pub struct BuildReport {
    /// True when the build finished without errors.
    pub ok: bool,
    /// The game folder's files, relative to `--out` (a macOS app is one).
    pub files: Vec<String>,
    /// Every archive entry with its form, sizes and compression.
    pub entries: Vec<re_app::package::archive::WrittenEntry>,
    /// The packing decisions.
    #[serde(flatten)]
    pub pack: pack::PackReport,
    /// The target the runtime is for.
    pub target: Option<String>,
    /// The third-party notices.
    pub notices: Option<NoticesReport>,
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

/// The runtime binary's name in a runtime folder.
fn runtime_name(triple: &str) -> &'static str {
    if target_os(triple) == "windows" {
        "real.exe"
    } else {
        "real"
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
        if ok && args.runtime_only {
            println!(
                "built the runtime for {} ({}) into {}",
                report.target.as_deref().unwrap_or("?"),
                report.files.join(", "),
                args.out.display()
            );
        } else if ok {
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
    let built = if args.runtime_only {
        runtime_only(args, &mut report)
    } else {
        build_game(args, project, &mut report)
    };
    report.ok = match built {
        Ok(()) => finish(&args.out, &mut report),
        Err(d) => {
            report.pack.diagnostics.push(d);
            false
        }
    };
    let ok = report.ok;
    (report, ok)
}

/// What the game folder is built around.
enum Target {
    /// Only the archive.
    ArchiveOnly,
    /// A web runtime folder.
    Web(web::WebRuntime),
    /// A desktop game runtime.
    Desktop(ReadyRuntime),
}

/// A desktop game runtime ready to copy, with its target and the runtime's
/// part of the notices.
struct ReadyRuntime {
    binary: PathBuf,
    triple: String,
    notices: String,
    report: NoticesReport,
}

/// `RE0951` when the report holds errors.
fn stop_on_errors(report: &BuildReport) -> Result<(), Diagnostic> {
    let errors = report
        .pack
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .count();
    if errors == 0 {
        return Ok(());
    }
    Err(Diagnostic::error(
        codes::BUILD_STOPPED,
        format!(
            "real build stopped: the project has {errors} error{}",
            if errors == 1 { "" } else { "s" }
        ),
    ))
}

fn build_game(
    args: &BuildArgs,
    project: &Path,
    report: &mut BuildReport,
) -> Result<(), Diagnostic> {
    let config = AppConfig::load(project)?;
    let earlier = check_out(&args.out)?;

    // 1. The checks `real verify` runs on the files.
    let scan = crate::tooling::check::ScanStage;
    for stage in re_app::verify::content_stages(project, &config, &[&scan]) {
        report.pack.diagnostics.extend(stage.diagnostics);
    }
    stop_on_errors(report)?;

    // 2. The archive.
    let (writer, pack_report) = pack::pack(project, &config);
    report.pack.package = pack_report.package.clone();
    report.pack.assets = pack_report.assets;
    report.pack.created_sidecars = pack_report.created_sidecars;
    report.pack.dropped = pack_report.dropped;
    report.pack.diagnostics.extend(pack_report.diagnostics);
    stop_on_errors(report)?;

    // 3. The runtime and the notices, ready before `out` is cleared, so a
    // runtime that cannot be built leaves an earlier build in place.
    let target = if args.archive_only {
        Target::ArchiveOnly
    } else if args.target.as_deref().is_some_and(web::is_web) {
        Target::Web(web::runtime(
            &args.out,
            args.runtime.as_deref(),
            &mut report.pack.diagnostics,
        )?)
    } else {
        Target::Desktop(desktop_runtime(args)?)
    };
    let game_notices = match target {
        Target::ArchiveOnly => String::new(),
        _ => {
            let patterns = config
                .package
                .as_ref()
                .map(|p| p.notices.clone())
                .unwrap_or_default();
            let (text, listed) =
                notices::game_part(&re_app::package::dev_files(project), &patterns)?;
            report.notices = Some(NoticesReport {
                game_files: listed,
                ..NoticesReport::default()
            });
            text
        }
    };

    // 4. The game folder.
    prepare_out(&args.out, &earlier)?;
    match target {
        Target::ArchiveOnly => write_archive(&args.out, ARCHIVE_NAME, &writer, report),
        Target::Web(runtime) => {
            report.target = Some(web::TARGET.to_owned());
            write_archive(&args.out, ARCHIVE_NAME, &writer, report)?;
            if let Some(n) = &mut report.notices {
                n.crates = runtime.notices.as_ref().and_then(|r| r.crates);
                n.texts = runtime.notices.as_ref().and_then(|r| r.texts);
                n.standard_texts = runtime
                    .notices
                    .map(|r| r.standard_texts)
                    .unwrap_or_default();
            }
            web::lay_out(&args.out, &runtime.dir, &config, &game_notices, report)
        }
        Target::Desktop(runtime) => {
            report.target = Some(runtime.triple.clone());
            if let Some(n) = &mut report.notices {
                n.crates = runtime.report.crates;
                n.texts = runtime.report.texts;
                n.standard_texts = runtime.report.standard_texts.clone();
            }
            let layout = bundle::layout(&config, target_os(&runtime.triple));
            write_archive(&args.out, &layout.archive, &writer, report)?;
            lay_out(
                &args.out,
                &layout,
                &runtime,
                &game_notices,
                &config,
                project,
                report,
            )
        }
    }
}

/// Writes the archive at `rel` in `out`; lists it unless it is inside a
/// macOS app, which is listed whole.
fn write_archive(
    out: &Path,
    rel: &str,
    writer: &PackageWriter,
    report: &mut BuildReport,
) -> Result<(), Diagnostic> {
    report.entries = writer.write(&out.join(rel)).map_err(|e| {
        Diagnostic::error(
            codes::BUNDLE_FAILED,
            format!("cannot write {ARCHIVE_NAME}: {e}"),
        )
    })?;
    if !rel.contains('/') {
        report.files.push(rel.to_owned());
    }
    Ok(())
}

/// `real build --runtime-only`: a runtime folder, as `--runtime` takes it.
fn runtime_only(args: &BuildArgs, report: &mut BuildReport) -> Result<(), Diagnostic> {
    let earlier = check_out(&args.out)?;
    let failed = |what: &str, e: std::io::Error| {
        Diagnostic::error(codes::BUNDLE_FAILED, format!("cannot write {what}: {e}"))
    };
    if args.target.as_deref().is_some_and(web::is_web) {
        report.target = Some(web::TARGET.to_owned());
        let runtime = web::runtime(&args.out, None, &mut report.pack.diagnostics)?;
        prepare_out(&args.out, &earlier)?;
        for name in web::RUNTIME_FILES {
            std::fs::copy(runtime.dir.join(name), args.out.join(name))
                .map_err(|e| failed(name, e))?;
            report.files.push((*name).to_owned());
        }
        report.notices = runtime.notices;
        return Ok(());
    }
    let triple = args
        .target
        .clone()
        .unwrap_or_else(crate::game::target_triple);
    report.target = Some(triple.clone());
    let (binary, ws) = build_runtime(&triple)?;
    let (text, notices_report) = notices::runtime_notices(&ws, notices::Runtime::Desktop, &triple)?;
    prepare_out(&args.out, &earlier)?;
    let exe = runtime_name(&triple);
    copy_executable(&binary, &args.out.join(exe)).map_err(|e| failed(exe, e))?;
    let mut info = serde_json::to_value(RuntimeVersions::current()).unwrap_or_default();
    info["target"] = triple.clone().into();
    let info = serde_json::to_string_pretty(&info).unwrap_or_default() + "\n";
    std::fs::write(args.out.join(web::RUNTIME_INFO), info)
        .map_err(|e| failed(web::RUNTIME_INFO, e))?;
    std::fs::write(args.out.join(NOTICES_FILE), text).map_err(|e| failed(NOTICES_FILE, e))?;
    report.files = vec![
        exe.to_owned(),
        web::RUNTIME_INFO.to_owned(),
        NOTICES_FILE.to_owned(),
    ];
    report.notices = Some(notices_report);
    Ok(())
}

/// Copies an executable, keeping it executable on Unix.
fn copy_executable(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::copy(from, to)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// The desktop runtime for the build: the `--runtime` binary, checked and
/// with the notices of its folder, or one built with cargo, with notices
/// listed from the workspace.
fn desktop_runtime(args: &BuildArgs) -> Result<ReadyRuntime, Diagnostic> {
    let Some(path) = &args.runtime else {
        let triple = args
            .target
            .clone()
            .unwrap_or_else(crate::game::target_triple);
        let (binary, ws) = build_runtime(&triple)?;
        let (notices, report) = notices::runtime_notices(&ws, notices::Runtime::Desktop, &triple)?;
        return Ok(ReadyRuntime {
            binary,
            triple,
            notices,
            report,
        });
    };
    let info = runtime_info(path)?;
    let triple = args
        .target
        .clone()
        .or_else(|| info["target"].as_str().map(str::to_owned))
        .unwrap_or_else(crate::game::target_triple);
    check_runtime(&info, path, &triple)?;
    let beside = re_app::package::archive_beside(path).with_file_name(NOTICES_FILE);
    let text = std::fs::read_to_string(&beside).map_err(|e| {
        Diagnostic::error(
            codes::NOTICES_FAILED,
            format!(
                "the runtime at {} has no {NOTICES_FILE} beside it ({}: {e})",
                path.display(),
                beside.display()
            ),
        )
        .with_hint("use the runtime folder `real build --runtime-only` writes, or a release's: it holds the notices of the crates the runtime was built with")
    })?;
    Ok(ReadyRuntime {
        binary: path.clone(),
        triple,
        notices: notices::runtime_part(&text),
        report: NoticesReport::default(),
    })
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

/// Builds the game runtime for `triple` with cargo in the engine workspace;
/// returns the binary and the workspace.
fn build_runtime(triple: &str) -> Result<(PathBuf, PathBuf), Diagnostic> {
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
    let built = target_dir
        .join(triple)
        .join("packaged")
        .join(runtime_name(triple));
    if !built.is_file() {
        return Err(failed(format!(
            "cargo finished but {} is missing",
            built.display()
        )));
    }
    Ok((built, ws))
}

/// What a prebuilt runtime says of itself: its `--runtime-info`, or the
/// `runtime.json` beside it when it cannot run here (a runtime for another
/// platform).
fn runtime_info(path: &Path) -> Result<serde_json::Value, Diagnostic> {
    let failed = |msg: String| Diagnostic::error(codes::RUNTIME_FAILED, msg);
    match Command::new(path).arg("--runtime-info").output() {
        Ok(output) => serde_json::from_slice(&output.stdout).map_err(|_| {
            failed(format!(
                "{} is not a game runtime: its --runtime-info did not print the versions",
                path.display()
            ))
        }),
        Err(e) => {
            let beside = path.with_file_name(web::RUNTIME_INFO);
            let text = std::fs::read_to_string(&beside).map_err(|_| {
                failed(format!("cannot run {} --runtime-info: {e}", path.display())).with_hint(
                    "a runtime for another platform needs the `runtime.json` of its runtime folder beside it",
                )
            })?;
            serde_json::from_str(&text).map_err(|e| {
                failed(format!(
                    "{} is not a runtime's {}: {e}",
                    beside.display(),
                    web::RUNTIME_INFO
                ))
            })
        }
    }
}

/// Checks what a prebuilt runtime said of itself against this engine and
/// `triple`.
fn check_runtime(info: &serde_json::Value, path: &Path, triple: &str) -> Result<(), Diagnostic> {
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
        Err(Diagnostic::error(
            codes::RUNTIME_FAILED,
            format!(
                "the runtime at {} does not match: {}",
                path.display(),
                differ.join(", ")
            ),
        ))
    }
}

/// Lays out the game folder for the target's OS around the archive: the
/// executable, the notices, and the Linux desktop entry or the macOS app's
/// `Info.plist` and icon.
fn lay_out(
    out: &Path,
    layout: &bundle::Layout,
    runtime: &ReadyRuntime,
    game_notices: &str,
    config: &AppConfig,
    project: &Path,
    report: &mut BuildReport,
) -> Result<(), Diagnostic> {
    let failed = |what: &str, e: std::io::Error| {
        Diagnostic::error(codes::BUNDLE_FAILED, format!("cannot write {what}: {e}"))
    };
    let name = executable_name(&config.project.name);
    let pc = config.package.clone().unwrap_or_default();
    let display = bundle::display_name(config);
    copy_executable(&runtime.binary, &out.join(&layout.exe)).map_err(|e| failed(&layout.exe, e))?;
    std::fs::write(
        out.join(&layout.notices),
        format!("{}{game_notices}", runtime.notices),
    )
    .map_err(|e| failed(&layout.notices, e))?;

    if let Some(app) = &layout.app {
        let contents = out.join(app).join("Contents");
        let icon_file = format!("{name}.icns");
        let icon = match &pc.icon {
            Some(icon) => {
                let image = pack::icon_image(&re_app::package::dev_files(project), icon)?;
                let icns = bundle::icns(&image).map_err(|why| {
                    Diagnostic::error(
                        codes::BUNDLE_FAILED,
                        format!("cannot write {icon_file}: {why}"),
                    )
                })?;
                std::fs::write(contents.join("Resources").join(&icon_file), icns)
                    .map_err(|e| failed(&icon_file, e))?;
                Some(icon_file.as_str())
            }
            None => None,
        };
        let (package, _) = pack::package_info(config);
        let plist = bundle::info_plist(&bundle::AppInfo {
            executable: &name,
            identifier: &package.identifier,
            name: &display,
            version: &package.version,
            icon,
            minimum_system: bundle::minimum_macos(&runtime.triple),
        });
        std::fs::write(contents.join("Info.plist"), plist).map_err(|e| failed("Info.plist", e))?;
        report.files.push(app.clone());
        if cfg!(target_os = "macos") {
            bundle::seal(&out.join(app))?;
        } else {
            report.notes.push(format!(
                "{app} is not sealed: `real build` seals a macOS app (`codesign --force --sign -`) only when it runs on a Mac"
            ));
        }
        return Ok(());
    }

    report.files.push(layout.exe.clone());
    report.files.push(layout.notices.clone());
    if layout.os == "linux" {
        let mut desktop = format!(
            "[Desktop Entry]\nType=Application\nName={display}\nExec={name}\nTerminal=false\nCategories=Game;\n"
        );
        if let Some(icon) = &pc.icon {
            let png = format!("{name}.png");
            std::fs::copy(project.join(icon), out.join(&png)).map_err(|e| failed(&png, e))?;
            report.files.push(png);
            desktop.push_str(&format!("Icon={name}\n"));
        }
        let file = format!("{name}.desktop");
        std::fs::write(out.join(&file), desktop).map_err(|e| failed(&file, e))?;
        report.files.push(file);
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
