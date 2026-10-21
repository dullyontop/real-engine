//! `real mcp`: serves the project's commands to MCP clients over stdio
//! (ADR-0025, ADR-0026; WP7), or with `--http` over streamable HTTP on a
//! loopback address with a bearer token (ADR-0046). The HTTP server writes
//! its URL and token to `.real/mcp-http.json` (readable by the user only)
//! and stops when its stdin closes (when stdin is a pipe: a parent process
//! such as a test or the eval harness) or when it is interrupted. A killed
//! server leaves the file behind; the next server replaces it.
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
//! (a dry run unless `write` is set), `template.list`, `recipe.list` and
//! `recipe.apply` (ADR-0046), the static scan stage
//! of `verify`, and last the project's Luau tools
//! (`scripts/tools/commands/`).
//!
//! Over stdio, stdout carries JSON-RPC, so everything else goes to stderr.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use re_agent::resources::ResourceSet;
use re_agent::session::Session;
use re_agent::templates::{ApplyOptions, Library, Listing, RecipeReport, apply_recipe};
use re_agent::verify_diff::{DiffOptions, VerifyDiff, diff};
use re_agent::{Agent, Holder, Policy, Writer, guard, open_writer};
use re_app::verify::{self, VerifyOptions, VerifyStage};
use re_app::{App, AppConfig};
use re_command::{EngineHandle, History, Origin, Tier, Transaction, channel};
use re_core::{Diagnostic, Location};
use re_mcp::McpOptions;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

/// Arguments of `real mcp`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct McpArgs {
    /// Serve streamable HTTP on ADDR instead of stdio: a loopback address,
    /// 127.0.0.1:0 (any free port) when no ADDR is given. Clients send
    /// `Authorization: Bearer <token>`; the token comes from REAL_MCP_TOKEN,
    /// or is generated, and is written with the URL to .real/mcp-http.json.
    #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = re_mcp::http::DEFAULT_ADDR)]
    pub http: Option<String>,
}

/// Where `real mcp --http` writes its URL and token (local state).
pub const HTTP_STATE: &str = ".real/mcp-http.json";

/// How `real mcp` serves: stdio, or HTTP on a bound listener.
enum Transport {
    Stdio,
    Http(re_mcp::HttpListener),
}

impl Transport {
    /// The transport `args` ask for, with the HTTP address checked and the
    /// token read (`RE8502`, `RE8503`); nothing is bound yet.
    fn check(args: &McpArgs) -> Result<Option<(std::net::SocketAddr, String)>, Diagnostic> {
        let Some(addr) = &args.http else {
            return Ok(None);
        };
        let addr = re_mcp::http::parse_addr(addr)?;
        let token = re_mcp::http::token_from_env()?;
        Ok(Some((addr, token)))
    }

    /// Binds the HTTP listener (`RE8504`) and writes the state file.
    fn open(
        project: &Path,
        http: Option<(std::net::SocketAddr, String)>,
    ) -> Result<Transport, Diagnostic> {
        let Some((addr, token)) = http else {
            return Ok(Transport::Stdio);
        };
        let listener = re_mcp::HttpListener::bind(addr, &token)?;
        write_http_state(project, &listener)?;
        let stops = if re_mcp::http::stdin_is_a_pipe() {
            "stops when stdin closes"
        } else {
            "Ctrl-C stops it"
        };
        eprintln!(
            "real mcp: serving MCP at {} (the token is in {HTTP_STATE}); {stops}",
            listener.url()
        );
        Ok(Transport::Http(listener))
    }

    /// Serves until the client closes stdin (stdio), or until stdin closes
    /// (HTTP).
    fn serve(self, handle: EngineHandle, options: McpOptions) -> anyhow::Result<()> {
        match self {
            Transport::Stdio => re_mcp::serve_stdio(handle, options),
            Transport::Http(listener) => {
                let project = options.project.clone();
                let result =
                    re_mcp::serve_http(handle, options, listener, re_mcp::http::stdin_closed());
                remove_http_state(&project);
                result
            }
        }
    }
}

/// Removes [`HTTP_STATE`] if this process wrote it (another `real mcp
/// --http` serving through the editor may have replaced it since).
fn remove_http_state(project: &Path) {
    let path = project.join(HTTP_STATE);
    let ours = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Json>(&t).ok())
        .is_some_and(|v| v["pid"] == std::process::id());
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

/// Writes `{ url, token, pid }` to [`HTTP_STATE`], readable by the user only
/// on Unix. A file that cannot be written is `RE8504`, like a port that
/// cannot be bound: either way no client could connect.
fn write_http_state(project: &Path, listener: &re_mcp::HttpListener) -> Result<(), Diagnostic> {
    use std::io::Write as _;
    let path = project.join(HTTP_STATE);
    let text = serde_json::to_string_pretty(&json!({
        "url": listener.url(),
        "token": listener.token(),
        "pid": std::process::id(),
    }))
    .expect("the state serializes");
    let fail = |e: std::io::Error| {
        Diagnostic::error(
            re_mcp::codes::BIND_FAILED,
            format!("cannot write {HTTP_STATE}: {e}"),
        )
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    // Replace any file a previous server left, so the mode below applies.
    let _ = std::fs::remove_file(&path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path).map_err(fail)?;
    file.write_all(text.as_bytes()).map_err(fail)?;
    file.write_all(b"\n").map_err(fail)
}

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
    super::mcp_import::register(commands, agent);
    register_recipes(commands, &project);
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

/// Registers `verify.diff` (read tier) for `agent`; `real do` registers it
/// too, since recipe checks use it.
pub(crate) fn register_verify_diff(registry: &mut re_command::CommandRegistry<App>, agent: &Agent) {
    let agent = agent.clone();
    registry.register_typed_output::<VerifyDiffArgs, VerifyDiffOutput, _>(
        "verify.diff",
        "Run verify and report what changed since the agent session's latest checkpoint (or `checkpoint`): stages that now pass or fail, diagnostics that appeared or went away, and values that changed. Timings are left out unless `include_timings` is set.",
        Tier::Read,
        false,
        move |app, args, _call| verify_diff(app, &agent, args),
    );
}

// ---- templates and recipes (ADR-0046) --------------------------------------

/// Arguments of `template.list` and `recipe.list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {}

/// The result of `template.list` and `recipe.list`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ListOutput {
    /// Each template or recipe, by name.
    pub items: Vec<Listing>,
}

/// Arguments of `recipe.apply`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecipeApplyArgs {
    /// The recipe (`recipe.list` lists them).
    pub recipe: String,
    /// Its parameters; a missing one takes its default.
    #[serde(default)]
    pub args: serde_json::Map<String, Json>,
}

/// The policy for a recipe's steps: nothing above `edit`, and the agent
/// policy when an agent applied the recipe.
struct RecipePolicy<'a> {
    agent: Option<&'a Policy>,
}

impl re_command::PolicyCheck for RecipePolicy<'_> {
    fn check(
        &self,
        spec: &re_command::CommandSpec,
        args: &Json,
        dry_run: bool,
    ) -> Result<(), Diagnostic> {
        // Only recipe.apply itself takes and restores its checkpoint:
        // apply_recipe refuses `checkpoint.*` as a step.
        let checkpointing = matches!(
            spec.name.as_str(),
            "checkpoint.create" | "checkpoint.restore"
        );
        if spec.tier > Tier::Edit && !checkpointing {
            return Err(Diagnostic::error(
                re_agent::codes::RECIPE_STEP_REFUSED,
                format!(
                    "a recipe may not run `{}` ({} tier)",
                    spec.name,
                    spec.tier.as_str()
                ),
            ));
        }
        match self.agent {
            Some(p) if !checkpointing => p.check(spec, args, dry_run),
            _ => Ok(()),
        }
    }
}

fn library() -> Result<Library, Diagnostic> {
    Library::builtin()
}

/// Registers `template.list`, `recipe.list` and `recipe.apply`, for
/// `real mcp` and `real do` (which also need `verify.diff` for the checks).
pub(crate) fn register_recipes(registry: &mut re_command::CommandRegistry<App>, project: &Path) {
    registry.register_typed_output::<ListArgs, ListOutput, _>(
        "template.list",
        "List the project templates `real new --template` offers, with their parameters and the checks a new project passes.",
        Tier::Read,
        true,
        |_app, ListArgs {}, _call| {
            Ok(json!({ "items": library()?.template_listings() }))
        },
    );
    registry.register_typed_output::<ListArgs, ListOutput, _>(
        "recipe.list",
        "List the generator recipes `recipe.apply` runs: what each makes, its parameters, the commands it runs and what it checks afterwards.",
        Tier::Read,
        true,
        |_app, ListArgs {}, _call| Ok(json!({ "items": library()?.recipe_listings() })),
    );
    // The agent policy is read once, as the Luau tools read it.
    let policy = Policy::load(project).unwrap_or_else(|_| Policy::read_only());
    registry.register_typed_output::<RecipeApplyArgs, RecipeReport, _>(
        "recipe.apply",
        "Apply a generator recipe: run its edit commands with `args`, then its checks (verify stages, read commands, `verify.diff`). All or nothing: a failed step or check restores the checkpoint taken first. A dry run previews each step against the project as it is.",
        Tier::Edit,
        false,
        move |app, args, call| {
            let lib = library()?;
            let recipe = lib.recipe(&args.recipe)?;
            let origin = call.origin().clone();
            let agent = matches!(origin, Origin::Agent(_)).then_some(&policy);
            let nested = RecipePolicy { agent };
            let specs = call.specs().to_vec();
            let options = ApplyOptions {
                dry_run: call.dry_run(),
                // All or nothing: a checkpoint first, restored on failure.
                atomic: true,
            };
            let mut run = |name: &str, args: Json, dry_run: bool| {
                app.dispatch(name, args, dry_run, &nested, origin.clone())
            };
            let report = apply_recipe(recipe, &args.args, &specs, options, &mut run)?;
            serde_json::to_value(&report).map_err(|e| {
                Diagnostic::error(
                    re_agent::codes::RECIPE_STEP_FAILED,
                    format!("the recipe report could not be encoded: {e}"),
                )
            })
        },
    );
}

/// Runs `real mcp` for the project at `project`.
pub fn run(args: McpArgs, project: &Path) -> anyhow::Result<ExitCode> {
    let http = match Transport::check(&args) {
        Ok(http) => http,
        Err(d) => return Ok(fail(&d)),
    };
    let policy = match Policy::load(project) {
        Ok(p) => p,
        Err(d) => return Ok(fail(&d)),
    };
    let origin = Origin::Agent(ORIGIN.to_string());
    let lock = match open_writer(project, Holder::headless("real mcp")) {
        Ok(Writer::Local(lock)) => lock,
        Ok(Writer::Remote(handle)) => {
            eprintln!("real mcp: the editor has the project open; serving through it");
            let transport = match Transport::open(project, http) {
                Ok(t) => t,
                Err(d) => return Ok(fail(&d)),
            };
            let handle = guard::guarded(handle, Arc::new(policy), origin);
            let options = McpOptions::new(project).with_resources(resources());
            return Ok(finish(transport.serve(handle, options)));
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
    // Last, so no error after it leaves the state file (and its token) behind.
    let transport = match Transport::open(project, http) {
        Ok(t) => t,
        Err(d) => return Ok(fail(&d)),
    };
    let (unnamed, receiver) = channel();
    let handle = unnamed.with_origin(origin);
    // `serve` returns when every handle is gone, so keep only the one the
    // server thread owns.
    drop(unnamed);
    let server = std::thread::Builder::new()
        .name("real-mcp".to_string())
        .spawn(move || transport.serve(handle, options))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use re_command::{AllowAll, CommandOutcome};

    /// A new project from `template`, open for an agent with the policy
    /// `real agent init` writes, and an agent session.
    fn agent_project(template: &str, policy: bool) -> (tempfile::TempDir, AgentApp) {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("game");
        let options = crate::new::NewOptions {
            name: None,
            template,
            set: &[],
            check: false,
        };
        crate::new::create_from(&project, options).unwrap();
        if policy {
            std::fs::create_dir_all(project.join(".real")).unwrap();
            std::fs::write(
                project.join(".real/agent-policy.toml"),
                Policy::init_default().to_toml(),
            )
            .unwrap();
        }
        let opened = open_for_agents(&project).unwrap();
        opened.agent.start_session(opened.app.world()).unwrap();
        (dir, opened)
    }

    fn call(app: &mut App, name: &str, args: Json, dry_run: bool) -> CommandOutcome {
        app.dispatch(name, args, dry_run, &AllowAll, Origin::Agent(ORIGIN.into()))
    }

    fn checks_ok(out: &CommandOutcome) {
        assert!(out.ok, "{:?}", out.diagnostics);
        let checks = out.data["checks"].as_array().unwrap();
        assert!(!checks.is_empty());
        for c in checks {
            assert_eq!(c["ok"], true, "{c}");
        }
    }

    #[test]
    fn templates_and_recipes_are_listed() {
        let (_dir, mut opened) = agent_project("native", true);
        let app = &mut opened.app;
        let names = |out: CommandOutcome| -> Vec<String> {
            assert!(out.ok, "{:?}", out.diagnostics);
            out.data["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            names(call(app, "template.list", json!({}), false)),
            ["native", "roblox", "terrain"]
        );
        assert_eq!(
            names(call(app, "recipe.list", json!({}), false)),
            ["falling-box", "spinner", "terrain-shape"]
        );
    }

    #[test]
    fn recipes_apply_and_pass_their_checks() {
        let (_dir, mut opened) = agent_project("native", true);
        let app = &mut opened.app;
        let out = call(
            app,
            "recipe.apply",
            json!({ "recipe": "falling-box", "args": { "label": "Crate", "x": 2.0 } }),
            false,
        );
        checks_ok(&out);
        assert!(out.data["checkpoint"].is_string());
        let out = call(
            app,
            "recipe.apply",
            json!({ "recipe": "spinner", "args": { "label": "Fan" } }),
            false,
        );
        checks_ok(&out);
        assert!(app.project().join("scripts/game/spinner.luau").is_file());

        let (_dir, mut opened) = agent_project("terrain", true);
        let out = call(
            &mut opened.app,
            "recipe.apply",
            json!({ "recipe": "terrain-shape", "args": { "source": "terrain", "height": 30.0 } }),
            false,
        );
        checks_ok(&out);
    }

    #[test]
    fn a_failed_recipe_changes_nothing() {
        let (_dir, mut opened) = agent_project("native", true);
        let app = &mut opened.app;
        let before = app.world().state_hash();
        let scene = std::fs::read(app.project().join("scenes/main.scene.json")).unwrap();
        // The native project has no graph source to shape.
        let out = call(
            app,
            "recipe.apply",
            json!({ "recipe": "terrain-shape", "args": { "source": "terrain" } }),
            false,
        );
        assert!(!out.ok);
        assert_eq!(out.diagnostics[0].code, re_agent::codes::RECIPE_STEP_FAILED);
        assert_eq!(app.world().state_hash(), before);
        assert_eq!(
            std::fs::read(app.project().join("scenes/main.scene.json")).unwrap(),
            scene
        );
        // A dry run changes nothing either, and runs no checks.
        let out = call(
            app,
            "recipe.apply",
            json!({ "recipe": "falling-box", "args": { "label": "Crate" } }),
            true,
        );
        assert!(out.ok, "{:?}", out.diagnostics);
        assert_eq!(app.world().state_hash(), before);
        assert!(
            out.data["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["ok"].is_null())
        );
    }

    #[test]
    fn the_agent_policy_holds_inside_a_recipe() {
        // No policy file: an agent may only read, even inside a recipe.
        let (_dir, mut opened) = agent_project("native", false);
        let app = &mut opened.app;
        let before = app.world().state_hash();
        let out = call(
            app,
            "recipe.apply",
            json!({ "recipe": "falling-box", "args": { "label": "Crate" } }),
            false,
        );
        assert!(!out.ok);
        let message = &out.diagnostics[0].message;
        assert!(
            message.contains(&re_agent::codes::TIER_REFUSED.to_string()),
            "{message}"
        );
        assert_eq!(app.world().state_hash(), before);
    }

    #[test]
    fn bad_recipe_arguments_are_reported() {
        let (_dir, mut opened) = agent_project("native", true);
        let app = &mut opened.app;
        let code = |out: CommandOutcome| out.diagnostics[0].code;
        let out = call(
            app,
            "recipe.apply",
            json!({ "recipe": "falling-bx" }),
            false,
        );
        assert_eq!(code(out), re_agent::codes::UNKNOWN_TEMPLATE);
        let out = call(
            app,
            "recipe.apply",
            json!({ "recipe": "falling-box", "args": { "label": "A", "mass": "heavy" } }),
            false,
        );
        assert_eq!(code(out), re_agent::codes::BAD_PARAM_VALUE);
    }
}
