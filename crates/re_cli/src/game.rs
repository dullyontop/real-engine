//! The game binary (`--features game`, ADR-0037): runs the game in the
//! `game.rpak` beside the executable and nothing else.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use re_app::package::{self, Package, RuntimeVersions};
use re_app::{App, AppConfig};
use re_core::Diagnostic;

use crate::{Live, PlayOptions, fail, play};

/// The notices `real build` writes beside the game.
pub const NOTICES_FILE: &str = "THIRD_PARTY_NOTICES.txt";

#[derive(Parser, Debug)]
#[command(about = "Run the game", disable_help_subcommand = true)]
struct GameArgs {
    /// Run without a window.
    #[arg(long)]
    headless: bool,
    /// Frames to run (default: the replay's length; 60 headless, or until the
    /// window closes).
    #[arg(long)]
    frames: Option<u64>,
    /// Drive the run with a replay file.
    #[arg(long)]
    replay: Option<PathBuf>,
    /// Write the last frame to this PNG.
    #[arg(long)]
    screenshot: Option<PathBuf>,
    /// Print the run report as JSON.
    #[arg(long)]
    json: bool,
    /// Keep the cache, saved data, crash reports and captures in DIR instead of
    /// the per-user folders.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Print the game's name and version and the engine version.
    #[arg(long)]
    version: bool,
    /// Print the third-party notices shipped with the game.
    #[arg(long)]
    notices: bool,
    /// Print the engine, Luau and bytecode versions, the compiler options and
    /// the target, as JSON (`real build --runtime` checks a runtime with it).
    #[arg(long)]
    runtime_info: bool,
}

/// What `--runtime-info` prints.
pub fn runtime_info() -> serde_json::Value {
    let mut info = serde_json::to_value(RuntimeVersions::current()).unwrap_or_default();
    info["target"] = serde_json::json!(target_triple());
    info
}

/// The target this binary was built for, such as `x86_64-unknown-linux-gnu`.
pub fn target_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "linux" => format!("{arch}-unknown-linux-gnu"),
        "macos" => format!("{arch}-apple-darwin"),
        "windows" => format!("{arch}-pc-windows-msvc"),
        os => format!("{arch}-unknown-{os}"),
    }
}

/// `main` of the game binary.
pub fn main() -> ExitCode {
    attach_console();
    let args = GameArgs::parse();
    if args.runtime_info {
        println!(
            "{}",
            serde_json::to_string_pretty(&runtime_info()).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot find this executable: {e}");
            return ExitCode::from(1);
        }
    };
    let archive = package::archive_beside(&exe);
    if args.notices {
        let path = archive.with_file_name(NOTICES_FILE);
        return match std::fs::read_to_string(&path) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", path.display());
                ExitCode::from(1)
            }
        };
    }
    let opened = match package::open_package(&archive, args.data_dir.as_deref()) {
        Ok(p) => p,
        Err(d) => return fail(d),
    };
    if args.version {
        let info = &opened.archive.index().package;
        println!(
            "{} {} (Real Engine {})",
            info.name,
            info.version,
            package::archive::ENGINE_VERSION
        );
        return ExitCode::SUCCESS;
    }
    crate::install_crash_hook(
        opened
            .files
            .writable(re_core::files::Writable::Crash)
            .clone(),
    );
    run(opened, args)
}

fn run(opened: Package, args: GameArgs) -> ExitCode {
    let app = match open_app(&opened, args.headless) {
        Ok(app) => app,
        Err(d) => return fail(d),
    };
    let options = PlayOptions {
        headless: args.headless,
        frames: args.frames,
        replay: args.replay,
        screenshot: args.screenshot,
        exit_after: false,
        json: args.json,
        live: Live::Packaged,
    };
    let archive = opened.archive.clone();
    play(app, options, &mut || archive.take_case_matches())
}

/// Opens the packaged game as an app.
///
/// # Errors
///
/// `real.toml` is missing or invalid, or the game does not open.
pub fn open_app(opened: &Package, headless: bool) -> Result<App, Diagnostic> {
    let config = AppConfig {
        headless,
        ..AppConfig::load_from(&opened.files)?
    };
    App::open_files(opened.files.clone(), config)
}

/// Started from a terminal, a Windows GUI-subsystem program has no console:
/// attach to the parent's, so `--version` and the rest print there. A parent
/// that pipes the output (CI, `real verify --package`) gets it either way.
#[cfg(windows)]
fn attach_console() {
    // SAFETY: AttachConsole has no preconditions; failure (no parent console)
    // is harmless.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}

#[cfg(not(windows))]
fn attach_console() {}
