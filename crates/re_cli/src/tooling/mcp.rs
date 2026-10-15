//! `real mcp`: serves the project's commands to MCP clients over stdio
//! (ADR-0025, ADR-0026; WP7).
//!
//! When the editor holds the project, `real mcp` connects to its socket and
//! serves MCP over that handle, with the agent policy checked here before a
//! call reaches the editor. Otherwise it takes the project lock itself, opens
//! the project, starts an agent session (its start checkpoint is what
//! `checkpoint.restore` returns to), and serves MCP on a background thread
//! while this thread runs the engine. Edits are saved to the scene as they
//! are made. A second headless writer gets `RE8003`.
//!
//! The agent-facing registry adds, beyond the engine's commands: the agent
//! commands (`script.write`, `checkpoint.*`, `file.delete`), the resources
//! (`resource.list`, `resource.read`, also served as MCP resources),
//! `compat.coverage`, `scene.dump_tree`, `verify.diff`, `project.import`
//! (a dry run unless `write` is set), the static scan stage
//! of `verify`, and last the project's Luau tools
//! (`scripts/tools/commands/`).
//!
//! stdout carries JSON-RPC, so everything else goes to stderr.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use re_agent::resources::ResourceSet;
use re_agent::session::Session;
use re_agent::verify_diff::{DiffOptions, VerifyDiff, diff};
use re_agent::{Agent, Holder, Policy, Writer, guard, open_writer};
use re_app::verify::{self, VerifyOptions, VerifyStage};
use re_app::{App, AppConfig};
use re_command::{History, Origin, Tier, Transaction, channel};
use re_core::{Diagnostic, Location};
use re_mcp::McpOptions;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

/// Arguments of `real mcp`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct McpArgs {}

/// The origin of every MCP call in the history.
pub const ORIGIN: &str = "mcp";

fn fail(d: &Diagnostic) -> ExitCode {
    eprint!("{}", re_core::render_human_all(std::slice::from_ref(d)));
    ExitCode::from(1)
}

/// The resources agents can read: every explain page this build links, the
/// engine documents, `engine.d.luau` and `roblox.d.luau` (the Roblox-style
/// API's types).
pub(crate) fn resources() -> Arc<ResourceSet> {
    let mut definitions = vec![("engine.d.luau", re_script::definitions())];
    match super::import::classes() {
        Ok(classes) => definitions.push((
            "roblox.d.luau",
            re_script_check::roblox::definitions(&classes),
        )),
        Err(d) => eprint!("{}", d.render_human()),
    }
    Arc::new(ResourceSet::standard(
        &crate::explain::catalog(),
        &definitions,
    ))
}

/// The project opened for agents.
pub(crate) struct AgentApp {
    /// The app, with the agent-facing commands registered.
    pub app: App,
    /// The agent state (sessions and checkpoints).
    pub agent: Agent,
    /// The resources, for the MCP server.
    pub resources: Arc<ResourceSet>,
}

/// Opens the project for agents: the app with every agent-facing command
/// (see the module docs), and edits saved as they are made. Luau tools that
/// fail to load are reported on stderr and left out.
pub(crate) fn open_for_agents(project: &Path) -> Result<AgentApp, Diagnostic> {
    let mut config = AppConfig::load(project)?;
    config.persist_edits = true;
    let mut app = App::open(project, config)?;
    let agent = Agent::new(project);
    let resources = register_agent_tools(&mut app, &agent);
    Ok(AgentApp {
        app,
        agent,
        resources,
    })
}

/// Registers every agent-facing command on `app` (see the module docs) and
/// the scan verify stage, for `real mcp` and `real editor` alike, so the two
/// serve the same tools. Luau tools that fail to load are reported on
/// stderr and left out. Returns the resources, for the MCP server.
pub(crate) fn register_agent_tools(app: &mut App, agent: &Agent) -> Arc<ResourceSet> {
    let project = app.project().to_path_buf();
    let resources = resources();
    let commands = app.commands_mut();
    crate::explain::register(commands);
    re_agent::register_commands(commands, agent);
    re_agent::resources::register_commands(commands, Arc::clone(&resources));
    re_agent::compat::register_commands(commands);
    register_verify_diff(commands, agent);
    // An unreadable policy grants agents nothing (`real mcp` refuses to start
    // on one anyway).
    let policy = Policy::load(&project).unwrap_or_else(|_| Policy::read_only());
    super::mcp_import::register(commands, agent, policy);
    app.add_verify_stage(Arc::new(super::check::ScanStage));
    // Last, so a tool cannot take an engine command's name.
    let (_tools, problems) = re_agent::luau_tools::register(app.commands_mut(), &project);
    if !problems.is_empty() {
        eprint!("{}", re_core::render_human_all(&problems));
    }
    resources
}

// ---- verify.diff -----------------------------------------------------------

/// Arguments of `verify.diff`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyDiffArgs {
    /// The checkpoint to compare against (default: the agent session's
    /// latest; `checkpoint.create` takes a new one).
    #[serde(default)]
    pub checkpoint: Option<String>,
    /// Frames the boot stage runs (default 120), as for `verify`.
    #[serde(default)]
    pub boot_frames: Option<u64>,
    /// Compare timings too (`duration_ms` and every `*_ms` value), which
    /// differ on every run (default false).
    #[serde(default)]
    pub include_timings: bool,
}

/// The result of `verify.diff`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct VerifyDiffOutput {
    /// The checkpoint compared against.
    pub checkpoint: String,
    /// Where the baseline report came from: `stored` (an earlier
    /// `verify.diff` against this checkpoint), `current` (the project is
    /// still at the checkpoint, so this run is the baseline) or `rebuilt`
    /// (verify ran on a copy of the project restored to the checkpoint).
    pub baseline: String,
    /// True when the new report passes.
    pub ok: bool,
    /// What changed.
    pub diff: VerifyDiff,
}

/// The verify options of `verify` for `boot_frames`.
fn verify_options(boot_frames: Option<u64>) -> VerifyOptions {
    let mut options = VerifyOptions::default();
    if let Some(n) = boot_frames {
        options.boot_frames = n.max(1);
        options.compat_boot_frames = options.boot_frames;
        options.warmup_frames = options.warmup_frames.min(options.boot_frames - 1);
    }
    options
}

fn report_json(report: &re_app::VerifyReport) -> Result<Json, Diagnostic> {
    serde_json::to_value(report).map_err(|e| {
        Diagnostic::error(
            re_app::codes::BOOT_FAILED,
            format!("the verify report could not be encoded: {e}"),
        )
    })
}

/// Runs verify on a copy of the project restored to `cp`, in the session's
/// directory, and returns its report with the copy's path written as the
/// project's.
fn rebuild_baseline(
    app: &App,
    session: &Session,
    cp: &re_agent::Checkpoint,
    options: &VerifyOptions,
    stages: &[&dyn VerifyStage],
) -> Result<Json, Diagnostic> {
    let dir = session.dir().join(format!("baseline-{}", cp.id));
    let _ = std::fs::remove_dir_all(&dir);
    let result = (|| {
        session.materialize(cp, &dir)?;
        let config = AppConfig::load(&dir)?;
        {
            let mut copy = App::open(&dir, config.clone())?;
            let ops = Session::restore_ops(copy.world(), cp);
            let tx = Transaction::new(format!("restore {}", cp.id), Origin::User, ops);
            History::default()
                .execute(copy.world_mut(), tx)
                .map_err(|e| e.to_diagnostic())?;
            copy.edit_frame();
            copy.save_scene()?;
        }
        let report = verify::run_project(&dir, &config, options, stages);
        let text = report_json(&report)?.to_string();
        // Paths in the report name the copy; make them name the project.
        let from = serde_json::to_string(&dir.display().to_string()).unwrap_or_default();
        let to = serde_json::to_string(&app.project().display().to_string()).unwrap_or_default();
        let text = text.replace(from.trim_matches('"'), to.trim_matches('"'));
        serde_json::from_str::<Json>(&text).map_err(|e| {
            Diagnostic::error(
                re_app::codes::BOOT_FAILED,
                format!("the baseline report could not be decoded: {e}"),
            )
        })
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result.map_err(|d| {
        let mut d = d;
        if d.location.is_none() {
            d.location = Some(Location::command("verify.diff"));
        }
        d.with_hint(format!(
            "the baseline is verify on the project as it was at checkpoint {}; `checkpoint.create` starts a new baseline from now",
            cp.id
        ))
    })
}

fn verify_diff(app: &mut App, agent: &Agent, args: VerifyDiffArgs) -> Result<Json, Diagnostic> {
    let options = verify_options(args.boot_frames);
    let key = options.boot_frames.to_string();
    let extra = app.verify_stages();
    let stages: Vec<&dyn VerifyStage> = extra.iter().map(|s| &**s as &dyn VerifyStage).collect();
    let report = verify::run(app, &options, &stages);
    let current = report_json(&report)?;
    let world: &re_scene::World = app.world();
    let (baseline, how, checkpoint) = agent.with_session(world, |session| {
        let id = args
            .checkpoint
            .clone()
            .unwrap_or_else(|| session.latest_checkpoint());
        let cp = session.load_checkpoint(&id)?;
        let (baseline, how) = match session.baseline(&cp.id, &key) {
            Some(b) => (b, "stored"),
            None if session.is_current(&cp, world)? => (current.clone(), "current"),
            None => (
                rebuild_baseline(app, session, &cp, &options, &stages)?,
                "rebuilt",
            ),
        };
        if how != "stored" {
            session.save_baseline(&cp.id, &key, &baseline)?;
        }
        Ok((baseline, how, cp.id))
    })?;
    let opts = DiffOptions {
        include_timings: args.include_timings,
    };
    let out = VerifyDiffOutput {
        checkpoint,
        baseline: how.to_string(),
        ok: report.ok,
        diff: diff(&baseline, &current, opts),
    };
    Ok(json!(out))
}

/// Registers `verify.diff` (read tier) for `agent`.
fn register_verify_diff(registry: &mut re_command::CommandRegistry<App>, agent: &Agent) {
    let agent = agent.clone();
    registry.register_typed_output::<VerifyDiffArgs, VerifyDiffOutput, _>(
        "verify.diff",
        "Run verify and report what changed since the agent session's latest checkpoint (or `checkpoint`): stages that now pass or fail, diagnostics that appeared or went away, and values that changed. Timings are left out unless `include_timings` is set.",
        Tier::Read,
        false,
        move |app, args, _call| verify_diff(app, &agent, args),
    );
}

/// Runs `real mcp` for the project at `project`.
pub fn run(args: McpArgs, project: &Path) -> anyhow::Result<ExitCode> {
    let McpArgs {} = args;
    let policy = match Policy::load(project) {
        Ok(p) => p,
        Err(d) => return Ok(fail(&d)),
    };
    let origin = Origin::Agent(ORIGIN.to_string());
    let lock = match open_writer(project, Holder::headless("real mcp")) {
        Ok(Writer::Local(lock)) => lock,
        Ok(Writer::Remote(handle)) => {
            eprintln!("real mcp: the editor has the project open; serving through it");
            let handle = guard::guarded(handle, Arc::new(policy), origin);
            let options = McpOptions::new(project).with_resources(resources());
            return Ok(finish(re_mcp::serve_stdio(handle, options)));
        }
        Err(d) => return Ok(fail(&d)),
    };

    let AgentApp {
        mut app,
        agent,
        resources,
    } = match open_for_agents(project) {
        Ok(x) => x,
        Err(d) => return Ok(fail(&d)),
    };
    let options = McpOptions::new(project).with_resources(resources);
    match agent.start_session(app.world()) {
        Ok(id) => eprintln!(
            "real mcp: agent session {id} (`real agent rollback {id}` returns to its start)"
        ),
        Err(d) => return Ok(fail(&d)),
    }
    let (unnamed, receiver) = channel();
    let handle = unnamed.with_origin(origin);
    // `serve` returns when every handle is gone, so keep only the one the
    // server thread owns.
    drop(unnamed);
    let server = std::thread::Builder::new()
        .name("real-mcp".to_string())
        .spawn(move || re_mcp::serve_stdio(handle, options))?;
    // Returns once the server thread drops its handle (the client closed
    // stdin).
    app.serve(receiver, &policy);
    drop(lock);
    match server.join() {
        Ok(result) => Ok(finish(result)),
        Err(_) => anyhow::bail!("the MCP server thread panicked"),
    }
}

/// The exit code of a finished server: a client that closes the connection
/// (even before initializing) is a normal end.
fn finish(result: anyhow::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if format!("{e:#}").contains("connection closed") => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("real mcp: {e:#}");
            ExitCode::from(1)
        }
    }
}
