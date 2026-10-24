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
    /// Serve the script debugger over DAP on 127.0.0.1 (default port 4711).
    #[arg(long, value_name = "PORT", num_args = 0..=1, default_missing_value = "4711")]
    debug: Option<u16>,
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
    Ok(app)
}

/// The verify stages this build adds (the static scan, with `tooling`).
fn extra_stages() -> Vec<Box<dyn re_app::verify::VerifyStage>> {
    vec![
        #[cfg(feature = "tooling")]
        Box::new(tooling::check::ScanStage),
    ]
}

fn cmd_run(project: &Path, args: RunArgs) -> ExitCode {
    let mut app = match open(project) {
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
    let options = PlayOptions {
        headless: args.headless,
        frames: args.frames,
        replay: args.replay,
        screenshot: args.screenshot,
        exit_after: args.exit_after,
        json: args.json,
        // A replay must repeat its recording, so only a live run keeps data
        // and reloads changed files.
        live: Live::Development,
    };
    play(app, options, &mut Vec::new)
}

/// What a live run (no replay) does beyond playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Live {
    /// `real run`: keep data and reload changed files.
    Development,
    /// A packaged game: keep data only in a window; never reload (a packaged
    /// game reads its archive, which does not change while it runs).
    Packaged,
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
        match options.live {
            Live::Development => {
                app.persist_data();
                // A game build never links the watcher in (ADR-0037).
                if !cfg!(feature = "game") {
                    watch_problem = app.enable_hot_reload().err();
                }
            }
            // Headless packaged runs are checks (`real verify --package`),
            // which must repeat; a played game keeps its data.
            Live::Packaged if !options.headless => app.persist_data(),
            Live::Packaged => {}
        }
        // A live window never stalls a frame on a streamed cell; headless
        // runs wait for it, so they repeat (ADR-0042).
        if !options.headless {
            app.set_stream_lateness(re_proc::stream::Lateness::Late);
        }
    }
    let (frames, mut diagnostics, mut report) = if options.headless {
        match app.run_headless(options.frames, replay) {
            Ok(r) => (
                r.frames,
                r.diagnostics.clone(),
                serde_json::to_value(&r).unwrap_or_default(),
            ),
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
        match re_app::run_windowed(&mut app, window) {
            Ok(r) => {
                let json = serde_json::json!({
                    "frames": r.frames,
                    "skipped": r.skipped,
                    "adapter": r.adapter,
                    "state_hash": app.world().state_hash().to_hex().to_string(),
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
            app.world().state_hash().to_hex()
        );
    }
    ExitCode::SUCCESS
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
            let stages = extra_stages();
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
