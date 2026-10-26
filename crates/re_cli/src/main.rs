//! `real`: the Real Engine command-line tool (ADR-0001).
//!
//! A runtime-only build (`--no-default-features`) runs, verifies, describes and
//! explains a project; the default `tooling` build adds the subcommands that
//! edit it or connect tools to it (ADR-0027).
//!
//! With the `game` feature (ADR-0037) the binary is a packaged game's executable
//! instead: it runs the game in the `game.rpak` beside it ([`game`]).
//!
//! Exit codes (ADR-0022): 0 success, 1 errors were reported, 2 usage errors,
//! 101 a crash (a report is written to `.real/crash/`).

// A packaged game opens no console window when double-clicked (ADR-0037).
#![cfg_attr(all(windows, feature = "game"), windows_subsystem = "windows")]

#[cfg(feature = "tooling")]
mod build;
mod codes;
mod explain;
mod game;
mod graph;
mod net_stage;
mod new;
mod schema;
#[cfg(feature = "tooling")]
mod tooling;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use re_app::{App, AppConfig, VerifyOptions};
use re_core::{DiagInfo, Diagnostic};

/// Every diagnostic code `re_cli` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::UNKNOWN_CODE,
        title: "Unknown diagnostic code",
        page: include_str!("../explain/RE0900.md"),
    },
    DiagInfo {
        code: codes::PROJECT_EXISTS,
        title: "Directory is not empty",
        page: include_str!("../explain/RE0901.md"),
    },
    DiagInfo {
        code: codes::WRITE_FAILED,
        title: "Output file could not be written",
        page: include_str!("../explain/RE0902.md"),
    },
    DiagInfo {
        code: codes::SCRIPT_API_GAP,
        title: "Script uses an API the engine does not provide",
        page: include_str!("../explain/RE0903.md"),
    },
    DiagInfo {
        code: codes::PACKAGE_CONFIG,
        title: "Invalid [package] section",
        page: include_str!("../explain/RE0950.md"),
    },
    DiagInfo {
        code: codes::BUILD_STOPPED,
        title: "real build stopped on errors",
        page: include_str!("../explain/RE0951.md"),
    },
    DiagInfo {
        code: codes::CASE_COLLISION,
        title: "Two project paths differ only by case",
        page: include_str!("../explain/RE0952.md"),
    },
    DiagInfo {
        code: codes::RUNTIME_FAILED,
        title: "Game runtime build or check failed",
        page: include_str!("../explain/RE0953.md"),
    },
    DiagInfo {
        code: codes::NOTICES_FAILED,
        title: "Third-party notices could not be written",
        page: include_str!("../explain/RE0954.md"),
    },
    DiagInfo {
        code: codes::BUNDLE_FAILED,
        title: "A game folder step failed",
        page: include_str!("../explain/RE0955.md"),
    },
    DiagInfo {
        code: codes::PACKAGE_DIFFERS,
        title: "Packaged run differs from the project",
        page: include_str!("../explain/RE0956.md"),
    },
    DiagInfo {
        code: codes::NOT_PACKED,
        title: "A packed file names a file that is not packed",
        page: include_str!("../explain/RE0957.md"),
    },
    DiagInfo {
        code: codes::WEB_TOOLCHAIN_MISSING,
        title: "The web build's toolchain is missing",
        page: include_str!("../explain/RE0970.md"),
    },
    DiagInfo {
        code: codes::WEB_RUNTIME_FAILED,
        title: "Web runtime build or check failed",
        page: include_str!("../explain/RE0971.md"),
    },
    DiagInfo {
        code: codes::WASM_BINDGEN_MISMATCH,
        title: "wasm-bindgen CLI version differs",
        page: include_str!("../explain/RE0972.md"),
    },
    DiagInfo {
        code: codes::BINARYEN_MISSING,
        title: "wasm-opt is not installed",
        page: include_str!("../explain/RE0973.md"),
    },
    DiagInfo {
        code: codes::WEB_ARCHIVE_LARGE,
        title: "The web build's archive is large",
        page: include_str!("../explain/RE0974.md"),
    },
];

#[derive(Parser, Debug)]
#[command(
    name = "real",
    version,
    about = "Real Engine: run, verify, describe and edit a game project"
)]
struct Cli {
    /// The project directory.
    #[arg(long, global = true, default_value = ".")]
    project: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Create a new project in DIR.
    New {
        /// The directory to create.
        #[arg(required_unless_present = "list_templates")]
        dir: Option<PathBuf>,
        /// The project name (default: the directory name).
        #[arg(long)]
        name: Option<String>,
        /// The template: `native` (the engine's own API), `roblox` (an empty
        /// Roblox-style place), `terrain`, or a template directory.
        /// `--list-templates` lists them; all but `native` need the tooling
        /// build.
        #[arg(long, default_value = "native")]
        template: String,
        /// A template parameter, `KEY=VALUE` (repeatable).
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Run the template's checks (`real verify` and the rest) on the new
        /// project.
        #[arg(long)]
        check: bool,
        /// List the templates and their parameters instead.
        #[arg(long, conflicts_with_all = ["dir", "set", "check"])]
        list_templates: bool,
        /// With `--list-templates`: print JSON.
        #[arg(long, requires = "list_templates")]
        json: bool,
    },
    /// Run the project.
    Run(RunArgs),
    /// Describe what the project can use: components, generators, procedural
    /// node kinds, scripts and commands.
    Describe {
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// Print `engine.d.luau`, the Luau definitions of the engine API (for
        /// luau-lsp and editors).
        #[arg(long, conflicts_with = "json")]
        luau_defs: bool,
    },
    /// Check the project: content, scripts, boot, replays, goldens and frame times.
    Verify {
        /// Print the JSON report.
        #[arg(long)]
        json: bool,
        /// Write missing or differing golden images from this run instead of failing.
        #[arg(long)]
        update_goldens: bool,
        /// Check the game `real build` wrote in DIR instead: run it as a
        /// child process (a boot and each replay) and compare its state hashes
        /// with the project's.
        #[cfg(feature = "tooling")]
        #[arg(long, value_name = "DIR", conflicts_with = "update_goldens")]
        package: Option<PathBuf>,
    },
    /// Explain a diagnostic code (or list every code).
    Explain {
        /// The code, such as RE1001.
        code: Option<String>,
    },
    /// Work with procedural graphs.
    #[command(subcommand)]
    Graph(graph::GraphCommand),
    /// Upgrade every procedural graph's nodes to their kinds' current
    /// versions, after reporting what changes (all or nothing).
    MigrateGraph(graph::MigrateArgs),
    /// Write every JSON Schema the engine defines.
    Schema {
        /// The output directory.
        #[arg(long, default_value = "schemas")]
        out: PathBuf,
    },
    /// Check the project's scripts: compile, the static API scan and (with
    /// --types) the Luau type checker.
    #[cfg(feature = "tooling")]
    Check(tooling::check::CheckArgs),
    /// Run one engine command against the project (through the editor when it
    /// has the project open).
    #[cfg(feature = "tooling")]
    Do(tooling::do_cmd::DoArgs),
    /// Serve the project's commands to MCP clients over stdio.
    #[cfg(feature = "tooling")]
    Mcp(tooling::mcp::McpArgs),
    /// Set up and manage agent access.
    #[cfg(feature = "tooling")]
    Agent(tooling::agent::AgentArgs),
    /// Open the editor.
    #[cfg(feature = "tooling")]
    Editor(tooling::editor::EditorArgs),
    /// Build a standalone game: `game.rpak`, the game runtime and the
    /// platform's layout.
    #[cfg(feature = "tooling")]
    Build(build::BuildArgs),
    /// Import a Rojo-style project or a Studio loader script into a
    /// Roblox-style project (created when --project is new or empty).
    #[cfg(feature = "tooling")]
    Import(tooling::import::ImportArgs),
}

#[derive(clap::Args, Debug)]
struct RunArgs {
    /// Run the game `real build` wrote in DIR, or its `game.rpak`, instead
    /// of the project, as the game's own executable does. DIR may also be a
    /// macOS build's app, or hold one.
    #[arg(long, value_name = "DIR|FILE", conflicts_with = "debug")]
    package: Option<PathBuf>,
    /// With --package: keep the game's cache, saved data and crash reports in
    /// DIR instead of the per-user folders.
    #[arg(long, value_name = "DIR", requires = "package")]
    data_dir: Option<PathBuf>,
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
    /// Exit after the frames and print `RUN OK` when no error was reported.
    #[arg(long)]
    exit_after: bool,
    /// Print the run report as JSON.
    #[arg(long)]
    json: bool,
    /// With --headless: record the state hash every K frames and after the
    /// last one, as `checkpoints` in the JSON report (`cargo xtask d2`, ADR-0036).
    #[arg(long, value_name = "K", requires = "headless")]
    checkpoints: Option<std::num::NonZeroU64>,
    /// Serve the script debugger over DAP on 127.0.0.1 (default port 4711).
    #[arg(long, value_name = "PORT", num_args = 0..=1, default_missing_value = "4711")]
    debug: Option<u16>,
    #[command(flatten)]
    net: NetArgs,
    /// Lockstep, for checks (ADR-0050 §7): a server waits for N players
    /// (default 1) and then for each player's frame; a client waits for each
    /// server frame. As a replay does, it starts from `data/seed/` and keeps
    /// no data.
    #[arg(long, requires = "online", value_name = "N", num_args = 0..=1, default_missing_value = "1")]
    lockstep: Option<u32>,
}

/// Online play for a Roblox-style game (ADR-0050 §6), in `real run` and in a
/// packaged game's executable.
#[derive(clap::Args, Debug)]
#[command(group(clap::ArgGroup::new("online").args(["server", "host", "connect"])))]
struct NetArgs {
    /// Run a dedicated server: headless, in real time, until stopped or
    /// `--frames`. Players join over UDP, and from a browser over a WebSocket
    /// on the same port (TCP). The traffic is not encrypted.
    #[arg(long, conflicts_with_all = ["host", "connect"])]
    server: bool,
    /// Play and serve: the local player plus players who connect.
    #[arg(long, conflicts_with = "connect")]
    host: bool,
    /// Join the server at HOST:PORT (or HOST, on the `[net]` port) over UDP,
    /// or at ws://HOST:PORT/ over a WebSocket.
    #[arg(long, value_name = "HOST:PORT")]
    connect: Option<String>,
    /// The player's name on the server (default `Player<n>`).
    #[arg(long, requires = "connect")]
    name: Option<String>,
    /// The port to serve on (default `[net] port`, 47800).
    #[arg(long, requires = "online")]
    port: Option<u16>,
    /// The address to serve on (default 0.0.0.0, every interface).
    #[arg(long, requires = "online", value_name = "ADDR")]
    bind: Option<String>,
    /// The most players who may connect (default `[net] max_players`).
    #[arg(long, requires = "online", value_name = "N")]
    max_players: Option<u32>,
}

impl NetArgs {
    /// The online options these flags ask for, if any.
    fn options(&self, lockstep: Option<u32>) -> Option<re_app::NetOptions> {
        let role = if self.server {
            re_app::NetRole::Server
        } else if self.host {
            re_app::NetRole::Host
        } else {
            re_app::NetRole::Client(self.connect.clone()?)
        };
        Some(re_app::NetOptions {
            role,
            port: self.port,
            bind: self.bind.clone(),
            max_players: self.max_players,
            name: self.name.clone(),
            lockstep,
        })
    }
}

/// Puts `app` online before its first frame, and says where a server listens.
///
/// # Errors
///
/// As [`App::set_net`].
fn go_online(app: &mut App, options: re_app::NetOptions) -> Result<(), Diagnostic> {
    let serving = !matches!(options.role, re_app::NetRole::Client(_));
    let bound = app.set_net(options)?;
    if serving {
        let addr = bound.addr;
        if addr.ip().is_unspecified() {
            eprintln!(
                "serving on port {} of every address of this machine",
                addr.port()
            );
        } else {
            eprintln!("serving on {addr}");
        }
        if let Some(ws) = bound.websocket {
            if ws.ip().is_unspecified() {
                eprintln!(
                    "browsers join over a WebSocket at ws://ADDRESS:{}/ (TCP)",
                    ws.port()
                );
            } else {
                eprintln!("browsers join over a WebSocket at ws://{ws}/");
            }
        }
        eprintln!("note: online play is not encrypted; play with people you trust or on a LAN");
    }
    Ok(())
}

pub(crate) fn print_diagnostics(diagnostics: &[Diagnostic]) {
    if !diagnostics.is_empty() {
        eprint!("{}", re_core::render_human_all(diagnostics));
    }
}

fn fail(d: Diagnostic) -> ExitCode {
    print_diagnostics(std::slice::from_ref(&d));
    ExitCode::from(1)
}

fn open(project: &Path) -> Result<App, Diagnostic> {
    #[allow(unused_mut)]
    let mut app = App::open(project, AppConfig::load(project)?)?;
    #[cfg(feature = "tooling")]
    app.add_verify_stage(std::sync::Arc::new(tooling::check::ScanStage));
    if app.config().compat.is_some() {
        app.add_verify_stage(std::sync::Arc::new(net_stage::NetStage));
    }
    Ok(app)
}

/// The verify stages this build adds: the static scan (with `tooling`) and,
/// for a Roblox-style project, the networked replays (`net`).
fn extra_stages(config: &AppConfig) -> Vec<Box<dyn re_app::verify::VerifyStage>> {
    let mut stages: Vec<Box<dyn re_app::verify::VerifyStage>> = vec![
        #[cfg(feature = "tooling")]
        Box::new(tooling::check::ScanStage),
    ];
    if config.compat.is_some() {
        stages.push(Box::new(net_stage::NetStage));
    }
    stages
}

fn cmd_run(project: &Path, args: RunArgs) -> ExitCode {
    let mut package = None;
    let opened = match &args.package {
        Some(path) => open_built(
            path,
            args.data_dir.as_deref(),
            args.headless || args.net.server,
        )
        .map(|(app, archive)| {
            package = Some(archive);
            app
        }),
        None => open(project),
    };
    let mut app = match opened {
        Ok(a) => a,
        Err(d) => return fail(d),
    };
    // Kept until the run ends; dropping it closes the client's connection.
    let _dap = match args.debug {
        Some(port) => {
            let debugger = re_script::debug::Debugger::new();
            match re_script::debug::dap::DapServer::listen(debugger.clone(), port, project) {
                Ok(server) => {
                    eprintln!("debugger: DAP on {}", server.addr());
                    app.set_debugger(Some(debugger));
                    Some(server)
                }
                Err(d) => return fail(d),
            }
        }
        None => None,
    };
    let server = args.net.server;
    let paced = server && args.lockstep.is_none();
    if let Some(options) = args.net.options(args.lockstep)
        && let Err(d) = go_online(&mut app, options)
    {
        return fail(d);
    }
    let options = PlayOptions {
        // A dedicated server has no window.
        headless: args.headless || server,
        // and runs until stopped.
        frames: args.frames.or(server.then_some(u64::MAX)),
        replay: args.replay,
        screenshot: args.screenshot,
        exit_after: args.exit_after,
        json: args.json,
        // A replay must repeat its recording, so only a live run keeps data
        // and reloads changed files.
        live: if args.lockstep.is_some() {
            Live::Check
        } else if package.is_some() {
            Live::Packaged
        } else {
            Live::Development
        },
        paced,
        checkpoints: args.checkpoints,
    };
    match package {
        Some(archive) => play(app, options, &mut || archive.take_case_matches()),
        None => play(app, options, &mut Vec::new),
    }
}

/// Opens the game `real build` wrote at `path` (its folder, or its
/// `game.rpak`) for `real run --package`.
///
/// # Errors
///
/// As [`re_app::package::open_package`] and [`game::open_app`].
fn open_built(
    path: &Path,
    data_dir: Option<&Path>,
    headless: bool,
) -> Result<(App, std::sync::Arc<re_app::package::Archive>), Diagnostic> {
    let archive = if path.is_dir() {
        built_archive(path)
    } else {
        path.to_path_buf()
    };
    let opened = re_app::package::open_package(&archive, data_dir)?;
    let app = game::open_app(&opened, headless)?;
    Ok((app, opened.archive))
}

/// The archive of the game `real build` wrote in `dir`: its `game.rpak`, or
/// for a macOS build, the app's in `Contents/Resources/`, when `dir` is that
/// app or holds it alone. Otherwise it is `dir/game.rpak`, which reports as
/// missing (`RE0604`).
fn built_archive(dir: &Path) -> PathBuf {
    let in_app = |app: &Path| {
        app.join("Contents")
            .join("Resources")
            .join(re_app::package::ARCHIVE_NAME)
    };
    let beside = dir.join(re_app::package::ARCHIVE_NAME);
    if beside.is_file() {
        return beside;
    }
    if in_app(dir).is_file() {
        return in_app(dir);
    }
    let mut apps = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let app = entry.ok()?.path();
            let archive = in_app(&app);
            (app.extension().is_some_and(|e| e == "app") && archive.is_file()).then_some(archive)
        });
    match (apps.next(), apps.next()) {
        (Some(archive), None) => archive,
        _ => beside,
    }
}

/// What a live run (no replay) does beyond playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Live {
    /// `real run`: keep data and reload changed files.
    Development,
    /// A packaged game: keep data in a window or as a dedicated server; never
    /// reload (a packaged game reads its archive, which does not change while
    /// it runs).
    Packaged,
    /// A lockstep run (`--lockstep`), which is a check: as a replay does, it
    /// starts from `data/seed/` on the fixed clock and reloads nothing, so it
    /// repeats.
    Check,
}

impl Live {
    /// Whether a live run keeps its saved data (ADR-0048), `headless` and
    /// `paced` as in [`PlayOptions`]. Headless packaged runs other than a
    /// dedicated server are checks (`real verify --package`), which must
    /// repeat.
    fn keeps_data(self, headless: bool, paced: bool) -> bool {
        match self {
            Live::Development => true,
            Live::Packaged => !headless || paced,
            Live::Check => false,
        }
    }
}

/// How [`play`] runs an open app.
struct PlayOptions {
    headless: bool,
    frames: Option<u64>,
    replay: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    exit_after: bool,
    json: bool,
    live: Live,
    /// Headless frames keep real time (a dedicated server), instead of
    /// running as fast as they can.
    paced: bool,
    /// Write the state hash every this many frames (`real run --checkpoints`).
    checkpoints: Option<std::num::NonZeroU64>,
}

/// Runs `app` (headless or in a window), prints the report and returns the
/// exit code. `late` gives diagnostics to add once the run is over (a packaged
/// game's case-only matches).
fn play(mut app: App, options: PlayOptions, late: &mut dyn FnMut() -> Vec<Diagnostic>) -> ExitCode {
    let replay = match &options.replay {
        Some(path) => match re_input::load_replay(path) {
            Ok(doc) => Some(doc),
            Err(d) => return fail(d),
        },
        None => None,
    };
    // A watcher that cannot start is reported; the run goes on without reload.
    let mut watch_problem = None;
    if replay.is_none() {
        if options.live.keeps_data(options.headless, options.paced) {
            app.persist_data();
        }
        // A game build never links the watcher in (ADR-0037).
        if options.live == Live::Development && !cfg!(feature = "game") {
            watch_problem = app.enable_hot_reload().err();
        }
        // A live window never stalls a frame on a streamed cell; headless
        // runs wait for it, so they repeat (ADR-0042).
        if !options.headless {
            app.set_stream_lateness(re_proc::stream::Lateness::Late);
        }
    }
    let (frames, mut diagnostics, mut report) = if options.headless {
        let every = options.checkpoints.map(std::num::NonZeroU64::get);
        let mut checkpoints = Vec::new();
        let start = re_core::time::Instant::now();
        let step = app.step();
        let mut n = 0u32;
        let run = app.run_headless_observed(options.frames, replay, |app| {
            if let Some(k) = every
                && app.frame_count().is_multiple_of(k)
            {
                checkpoints.push(checkpoint(app));
            }
            if options.paced {
                n = n.saturating_add(1);
                let due = step.saturating_mul(n);
                if let Some(wait) = due.checked_sub(start.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
        });
        let net = net_report(&app);
        app.close_net();
        match run {
            Ok(r) => {
                let mut json = serde_json::to_value(&r).unwrap_or_default();
                if every.is_some() {
                    if checkpoints.last().map(|c| c["frame"].as_u64())
                        != Some(Some(app.frame_count()))
                    {
                        checkpoints.push(checkpoint(&app));
                    }
                    json["checkpoints"] = serde_json::Value::Array(checkpoints);
                }
                if let Some(net) = net {
                    json["net"] = net;
                }
                (r.frames, r.diagnostics.clone(), json)
            }
            Err(d) => return fail(d),
        }
    } else {
        let frames = options.frames.or(replay.as_ref().map(|r| r.frames));
        if let Some(doc) = replay
            && let Err(d) = app.play_replay(doc)
        {
            return fail(d);
        }
        let window = re_app::WindowOptions {
            frames,
            ..re_app::WindowOptions::default()
        };
        let run = re_app::run_windowed(&mut app, window);
        app.close_net();
        match run {
            Ok(r) => {
                let json = serde_json::json!({
                    "frames": r.frames,
                    "skipped": r.skipped,
                    "adapter": r.adapter,
                    "state_hash": app.state_hash().to_hex().to_string(),
                    "diagnostics": r.diagnostics,
                    "perf": app.perf_report(),
                });
                (r.frames, r.diagnostics, json)
            }
            Err(d) => return fail(d),
        }
    };
    diagnostics.extend(watch_problem);
    if let Some(path) = &options.screenshot {
        let path = std::env::current_dir().unwrap_or_default().join(path);
        match app.screenshot(Some(&path)) {
            Ok(_) => report["screenshot"] = serde_json::json!(path.display().to_string()),
            Err(d) => diagnostics.push(d),
        }
        diagnostics.extend(
            app.take_diagnostics()
                .into_iter()
                .filter(Diagnostic::is_error),
        );
    }
    let extra = late();
    if options.screenshot.is_some() || !extra.is_empty() {
        diagnostics.extend(extra);
        report["diagnostics"] = serde_json::to_value(&diagnostics).unwrap_or_default();
    }
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        print_diagnostics(&diagnostics);
    }
    let errors = diagnostics.iter().filter(|d| d.is_error()).count();
    if errors > 0 {
        if !options.json {
            print_diagnostics(&[Diagnostic::error(
                re_app::codes::RUN_ERRORS,
                format!(
                    "the run reported {errors} error{}",
                    if errors == 1 { "" } else { "s" }
                ),
            )]);
        }
        return ExitCode::from(1);
    }
    if options.exit_after {
        // After `--json` output, stdout stays one JSON document.
        if options.json {
            eprintln!("RUN OK");
        } else {
            println!("RUN OK");
        }
    } else if !options.json {
        println!(
            "ran {frames} frames; state hash {}",
            app.state_hash().to_hex()
        );
    }
    ExitCode::SUCCESS
}

/// An online run's connections and their traffic, for the JSON report.
fn net_report(app: &App) -> Option<serde_json::Value> {
    let stats = app.net_stats();
    (!stats.is_empty()).then(|| {
        serde_json::json!(
            stats
                .iter()
                .map(|(peer, s)| {
                    serde_json::json!({
                        "peer": peer,
                        "bytes_sent": s.bytes_sent,
                        "bytes_received": s.bytes_received,
                        "packets_sent": s.packets_sent,
                        "packets_received": s.packets_received,
                        "fragments_resent": s.fragments_resent,
                    })
                })
                .collect::<Vec<_>>()
        )
    })
}

/// One `checkpoints` entry: the frames run so far and the run's state hash
/// (the report's hash, animation graphs included).
fn checkpoint(app: &App) -> serde_json::Value {
    serde_json::json!({
        "frame": app.frame_count(),
        "state_hash": app.state_hash().to_hex().to_string(),
    })
}

fn cmd_describe(project: &Path, json: bool, luau_defs: bool) -> ExitCode {
    if luau_defs {
        print!("{}", re_script::definitions());
        return ExitCode::SUCCESS;
    }
    let mut app = match open(project) {
        Ok(a) => a,
        Err(d) => return fail(d),
    };
    explain::register(app.commands_mut());
    // The project's Luau tools are commands too (WP17); load problems go to
    // stderr so `--json` output stays one document.
    #[cfg(feature = "tooling")]
    {
        let (_tools, problems) = re_agent::luau_tools::register(app.commands_mut(), project);
        if !problems.is_empty() {
            eprint!("{}", re_core::render_human_all(&problems));
        }
    }
    let outcome = app.dispatch(
        "describe",
        serde_json::Value::Null,
        false,
        &re_command::AllowAll,
        re_command::Origin::User,
    );
    if !outcome.ok {
        print_diagnostics(&outcome.diagnostics);
        return ExitCode::from(1);
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome.data).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    let d = &outcome.data;
    let list_by = |key: &str, field: &str| -> Vec<String> {
        d.get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.get(field).and_then(|n| n.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let list = |key: &str| list_by(key, "name");
    println!("components: {}", list("components").join(", "));
    println!("generators: {}", list("generators").join(", "));
    println!("nodes: {}", list_by("nodes", "kind").join(", "));
    println!("scripts: {}", list_by("scripts", "path").join(", "));
    println!("commands:");
    for spec in app.commands().specs() {
        println!(
            "  {:<20} {:<12} {}",
            spec.name,
            spec.tier.as_str(),
            spec.summary
        );
    }
    ExitCode::SUCCESS
}

fn cmd_verify(project: &Path, json: bool, update_goldens: bool) -> ExitCode {
    let options = VerifyOptions {
        update_goldens,
        ..VerifyOptions::default()
    };
    let report = match AppConfig::load(project) {
        Ok(config) => {
            let stages = extra_stages(&config);
            let refs: Vec<&dyn re_app::verify::VerifyStage> = stages.iter().map(|s| &**s).collect();
            re_app::verify::run_project(project, &config, &options, &refs)
        }
        // Without `--json` this is a plain error; with it, still a report
        // (a failed schema stage), so tools always get one JSON document.
        Err(d) if !json => return fail(d),
        Err(d) => re_app::VerifyReport {
            ok: false,
            stages: vec![re_app::StageReport::new(
                "schema",
                vec![d],
                serde_json::Value::Null,
            )],
            perf: None,
        },
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        for stage in &report.stages {
            println!(
                "{:<8} {:<4} {:>9.1} ms",
                stage.name,
                if stage.ok { "ok" } else { "FAIL" },
                stage.duration_ms
            );
            print_diagnostics(&stage.diagnostics);
        }
        println!(
            "{}",
            if report.ok {
                "verify: ok"
            } else {
                "verify: FAILED"
            }
        );
    }
    ExitCode::from(report.exit_code())
}

fn cmd_explain(code: Option<String>) -> ExitCode {
    match code {
        None => {
            for d in explain::catalog() {
                println!("{}  {}", d.code, d.title);
            }
            ExitCode::SUCCESS
        }
        Some(code) => match explain::lookup(&code) {
            Ok(info) => {
                print!("{}", info.page);
                ExitCode::SUCCESS
            }
            Err(d) => fail(d),
        },
    }
}

fn cmd_schema(out: &Path) -> ExitCode {
    match schema::write_all(out) {
        Ok(files) => {
            println!("wrote {} schemas to {}", files.len(), out.display());
            ExitCode::SUCCESS
        }
        Err(d) => fail(d),
    }
}

/// Writes a crash report to the run's crash folder (`<project>/.real/crash/`)
/// on a panic (ARCHITECTURE §7).
fn install_crash_hook(crash: re_core::files::WritableDir) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        let stamp = re_core::time::SystemTime::now()
            .duration_since(re_core::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let report = format!(
            "real {} crashed\n\n{info}\n\nbacktrace:\n{}\n",
            env!("CARGO_PKG_VERSION"),
            std::backtrace::Backtrace::force_capture()
        );
        let name = format!("crash-{stamp}.txt");
        if crash.write(&name, report.as_bytes()).is_ok() {
            match crash.path() {
                Some(dir) => eprintln!(
                    "a crash report was written to {}",
                    dir.join(&name).display()
                ),
                None => eprintln!("a crash report was written ({name})"),
            }
        }
    }));
}

fn main() -> ExitCode {
    if cfg!(feature = "game") {
        game::main()
    } else {
        development_main()
    }
}

/// `main` of the `real` tool.
fn development_main() -> ExitCode {
    let cli = Cli::parse();
    install_crash_hook(re_core::files::WritableDirs::in_project(&cli.project).crash);
    // Warnings, plus what scripts `print`, on standard error (stdout stays for
    // results and JSON); `RUST_LOG` replaces this.
    if let Err(d) = re_core::init_logging(re_core::LogConfig {
        filter: "warn,script=info".into(),
        ..re_core::LogConfig::default()
    }) {
        print_diagnostics(&[d]);
    }
    let project = cli.project;
    match cli.command {
        Command::New {
            list_templates: true,
            json,
            ..
        } => match new::list_templates(json) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(d) => fail(d),
        },
        Command::New {
            dir,
            name,
            template,
            set,
            check,
            ..
        } => {
            let dir = dir.expect("clap requires DIR without --list-templates");
            let options = new::NewOptions {
                name: name.as_deref(),
                template: &template,
                set: &set,
                check,
            };
            match new::create_from(&dir, options) {
                Ok(files) => {
                    println!("created {} ({})", dir.display(), files.join(", "));
                    ExitCode::SUCCESS
                }
                Err(d) => fail(d),
            }
        }
        Command::Run(args) => cmd_run(&project, args),
        Command::Describe { json, luau_defs } => cmd_describe(&project, json, luau_defs),
        #[cfg(feature = "tooling")]
        Command::Verify {
            json,
            package: Some(dir),
            ..
        } => build::verify::run(&dir, &project, json),
        Command::Verify {
            json,
            update_goldens,
            ..
        } => cmd_verify(&project, json, update_goldens),
        Command::Explain { code } => cmd_explain(code),
        Command::Schema { out } => cmd_schema(&out),
        Command::Graph(command) => graph::run(command, &project),
        Command::MigrateGraph(args) => graph::migrate(args, &project),
        #[cfg(feature = "tooling")]
        Command::Check(args) => tooling_exit(tooling::check::run(args, &project)),
        #[cfg(feature = "tooling")]
        Command::Do(args) => tooling_exit(tooling::do_cmd::run(args, &project)),
        #[cfg(feature = "tooling")]
        Command::Mcp(args) => tooling_exit(tooling::mcp::run(args, &project)),
        #[cfg(feature = "tooling")]
        Command::Agent(args) => tooling_exit(tooling::agent::run(args, &project)),
        #[cfg(feature = "tooling")]
        Command::Editor(args) => tooling_exit(tooling::editor::run(args, &project)),
        #[cfg(feature = "tooling")]
        Command::Import(args) => tooling_exit(tooling::import::run(args, &project)),
        #[cfg(feature = "tooling")]
        Command::Build(args) => build::run(args, &project),
    }
}

#[cfg(feature = "tooling")]
fn tooling_exit(result: anyhow::Result<ExitCode>) -> ExitCode {
    result.unwrap_or_else(|e| {
        eprintln!("error: {e:#}");
        ExitCode::from(1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A built game keeps its data in a window and as a dedicated server
    /// (`--server`, paced); `real verify --package` and lockstep runs repeat.
    #[test]
    fn servers_and_windows_keep_their_data_and_checks_do_not() {
        assert!(Live::Packaged.keeps_data(true, true));
        assert!(Live::Packaged.keeps_data(false, false));
        assert!(!Live::Packaged.keeps_data(true, false));
        assert!(!Live::Check.keeps_data(true, false));
        assert!(Live::Development.keeps_data(true, false));
    }

    /// `real run --package DIR` finds a macOS build's archive in its app.
    #[test]
    fn a_build_folder_names_its_archive() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        let beside = out.join(re_app::package::ARCHIVE_NAME);
        assert_eq!(built_archive(out), beside);
        let app = out.join("Island Demo.app/Contents/Resources");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join(re_app::package::ARCHIVE_NAME), "").unwrap();
        assert_eq!(built_archive(out), app.join(re_app::package::ARCHIVE_NAME));
        assert_eq!(
            built_archive(&out.join("Island Demo.app")),
            app.join(re_app::package::ARCHIVE_NAME)
        );
        // With two apps the build is unclear, so the missing archive reports.
        let other = out.join("Other.app/Contents/Resources");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(re_app::package::ARCHIVE_NAME), "").unwrap();
        assert_eq!(built_archive(out), beside);
        std::fs::write(&beside, "").unwrap();
        assert_eq!(built_archive(out), beside);
    }
}
