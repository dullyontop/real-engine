//! `real agent`: sets up and manages agent access (WP7, ADR-0020,
//! ADR-0025).
//!
//! * `real agent init` writes `.real/agent-policy.toml` (the `edit` tier
//!   within the default scopes: committing it is the opt-in), `AGENTS.md`,
//!   `CLAUDE.md` and each MCP client's config, and prints the Codex and Zed
//!   snippets;
//! * `real agent status` shows the policy, who holds the project, and the
//!   agent sessions;
//! * `real agent rollback [session]` returns the scene and every file an
//!   agent session touched to the session's start (or `--checkpoint`);
//! * `real agent eval SUITE` runs an evaluation suite (ADR-0046): a CLI
//!   subcommand, not a registered command, so no agent can start one.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use re_agent::eval::{
    AgentConfig, ReferenceCall, Score, Summary, Task, TaskProject, TaskSummary, config_paths,
    expand_command, find_agent, find_example, load_suite,
};
use re_agent::init::{InitOptions, init};
use re_agent::lock::LockState;
use re_agent::session::list_sessions;
use re_agent::templates::{CheckKind, evaluate_checks};
use re_agent::verify_diff::{self, DiffOptions};
use re_agent::{Holder, Policy, Writer, open_writer, probe};
use re_command::{AllowAll, CommandOutcome, Origin, Tier};
use re_core::Diagnostic;
use re_mcp::client::{HttpClient, StdioClient, wait_or_kill};
use re_mcp::record::read_transcript;
use serde_json::{Map, Value as Json, json};

use super::mcp::open_for_agents;

/// Arguments of `real agent`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct AgentArgs {
    /// What to do (default: `status`).
    #[command(subcommand)]
    pub command: Option<AgentCommand>,
}

/// The `real agent` subcommands.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum AgentCommand {
    /// Write the agent policy, AGENTS.md, CLAUDE.md and MCP client configs.
    Init {
        /// Replace an existing .real/agent-policy.toml.
        #[arg(long)]
        force: bool,
        /// The program MCP clients start.
        #[arg(long, default_value = "real")]
        program: String,
        /// Point Claude Code at `real mcp --http ADDR` (a loopback address
        /// with a fixed port; 127.0.0.1:8765 when no ADDR is given) instead
        /// of starting `real mcp` over stdio. The token comes from
        /// REAL_MCP_TOKEN.
        #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "127.0.0.1:8765")]
        http: Option<String>,
    },
    /// Show the agent policy, the project lock and the agent sessions.
    Status,
    /// Run an evaluation suite: copy each task's project, let the driver
    /// act on it over MCP, and score it with `real verify` and the task's
    /// checks. The engine calls no model: without `--agent` the tasks'
    /// reference calls are the driver.
    Eval {
        /// The suite: a directory of task files (`crates/re_agent/evals/NAME`).
        suite: PathBuf,
        /// An agent from `eval.toml` to run instead of the reference calls.
        #[arg(long)]
        agent: Option<String>,
        /// Runs of each task (agents are not deterministic).
        #[arg(long, default_value_t = 1)]
        runs: u32,
        /// Where the runs and the summary go.
        #[arg(long, default_value = "out/eval")]
        out: PathBuf,
        /// How MCP is served.
        #[arg(long, value_enum, default_value = "http")]
        transport: EvalTransport,
    },
    /// Return the scene and the files an agent session touched to a checkpoint.
    Rollback {
        /// The session (default: the latest).
        session: Option<String>,
        /// The checkpoint (default: the session's start).
        #[arg(long)]
        checkpoint: Option<String>,
    },
}

fn fail(d: &Diagnostic) -> ExitCode {
    eprint!("{}", re_core::render_human_all(std::slice::from_ref(d)));
    ExitCode::from(1)
}

/// Runs `real agent` for the project at `project`.
pub fn run(args: AgentArgs, project: &Path) -> anyhow::Result<ExitCode> {
    Ok(match args.command.unwrap_or(AgentCommand::Status) {
        AgentCommand::Init {
            force,
            program,
            http,
        } => {
            let http = match http.as_deref().map(http_url).transpose() {
                Ok(url) => url,
                Err(d) => return Ok(fail(&d)),
            };
            cmd_init(
                project,
                InitOptions {
                    program,
                    force,
                    http,
                },
            )
        }
        AgentCommand::Status => cmd_status(project),
        AgentCommand::Eval {
            suite,
            agent,
            runs,
            out,
            transport,
        } => cmd_eval(&suite, agent, runs, &out, transport),
        AgentCommand::Rollback {
            session,
            checkpoint,
        } => cmd_rollback(project, session, checkpoint),
    })
}

/// The URL of `real mcp --http ADDR`: a loopback address with a fixed port,
/// since client configs name it (`RE8502`).
fn http_url(addr: &str) -> Result<String, Diagnostic> {
    let addr = re_mcp::http::parse_addr(addr)?;
    if addr.port() == 0 {
        return Err(Diagnostic::error(
            re_mcp::codes::NOT_LOOPBACK,
            "client configs need a fixed port, not 0",
        )
        .with_hint(
            "pass an address such as 127.0.0.1:8765, and start `real mcp --http` with the same one",
        ));
    }
    Ok(format!("http://{addr}{}", re_mcp::http::ENDPOINT))
}

fn cmd_init(project: &Path, options: InitOptions) -> ExitCode {
    // AGENTS.md lists the commands an agent will see, so open the project.
    let app = match open_for_agents(project) {
        Ok(x) => x.app,
        Err(d) => return fail(&d),
    };
    // The Codex and Zed snippets name the project by its full path.
    let root = std::path::absolute(project).unwrap_or_else(|_| project.to_path_buf());
    let report = match init(&root, app.commands().specs(), &options) {
        Ok(r) => r,
        Err(d) => return fail(&d),
    };
    for path in &report.written {
        println!("wrote {path}");
    }
    for (path, why) in &report.kept {
        println!("kept {path}: {why}");
    }
    for (client, place, snippet) in &report.snippets {
        println!("\n{client} ({place}):\n{snippet}");
    }
    println!(
        "Agents may now edit within .real/agent-policy.toml's scopes; commit it to keep that choice. \
         Restart MCP clients that are already running so `real mcp` reads the new policy."
    );
    ExitCode::SUCCESS
}

fn cmd_status(project: &Path) -> ExitCode {
    match Policy::load(project) {
        Ok(p) => {
            println!("policy: max tier {}", p.max_tier.as_str());
            println!("  allow: {}", p.scopes.allow.join(", "));
            if !p.scopes.deny.is_empty() {
                println!("  deny: {}", p.scopes.deny.join(", "));
            }
            let b = p.budgets();
            println!(
                "  budgets: {} frames, {} ms, {} MB, {} operations per transaction",
                b.frames, b.milliseconds, b.memory_mb, b.entities_per_transaction
            );
        }
        Err(d) => return fail(&d),
    }
    match probe(project) {
        LockState::Free => println!("project: not open"),
        LockState::Editor(socket) => {
            println!("project: open in the editor ({})", socket.display());
        }
        LockState::Headless(0) => println!("project: open by a headless writer"),
        LockState::Headless(pid) => println!("project: open by a headless writer (pid {pid})"),
    }
    let sessions = list_sessions(project);
    if sessions.is_empty() {
        println!("sessions: none");
    } else {
        println!("sessions: {}", sessions.join(", "));
    }
    ExitCode::SUCCESS
}

fn cmd_rollback(project: &Path, session: Option<String>, checkpoint: Option<String>) -> ExitCode {
    let Some(session) = session.or_else(|| list_sessions(project).pop()) else {
        return fail(
            &Diagnostic::error(
                re_agent::codes::NO_SESSION,
                "this project has no agent sessions",
            )
            .with_hint("sessions start when an agent connects through `real mcp`"),
        );
    };
    let lock = match open_writer(project, Holder::headless("real agent rollback")) {
        Ok(Writer::Local(lock)) => lock,
        Ok(Writer::Remote(_)) => {
            return fail(
                &Diagnostic::error(
                    re_agent::codes::PROJECT_LOCKED,
                    "the editor has the project open",
                )
                .with_hint("close the editor first, then run `real agent rollback` again"),
            );
        }
        Err(d) => return fail(&d),
    };
    let (mut app, agent) = match open_for_agents(project) {
        Ok(x) => (x.app, x.agent),
        Err(d) => return fail(&d),
    };
    if let Err(d) = agent.resume_session(&session) {
        return fail(&d);
    }
    let outcome = app.dispatch(
        "checkpoint.restore",
        json!({ "checkpoint": checkpoint }),
        false,
        &AllowAll,
        Origin::User,
    );
    drop(lock);
    if !outcome.ok {
        eprint!("{}", re_core::render_human_all(&outcome.diagnostics));
        return ExitCode::from(1);
    }
    let d = &outcome.data;
    println!(
        "restored {} of session {session}; the state before is checkpoint {} (`real agent rollback {session} --checkpoint {}` goes back)",
        d["restored"].as_str().unwrap_or("?"),
        d["saved"].as_str().unwrap_or("?"),
        d["saved"].as_str().unwrap_or("?"),
    );
    ExitCode::SUCCESS
}

// ---- real agent eval (ADR-0046) --------------------------------------------

/// The transport `real agent eval` serves MCP on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum EvalTransport {
    /// Streamable HTTP on a loopback port, with a token.
    Http,
    /// stdio: the reference driver starts `real mcp`; an agent's client
    /// starts it from the generated `.mcp.json`.
    Stdio,
}

impl EvalTransport {
    fn as_str(self) -> &'static str {
        match self {
            EvalTransport::Http => "http",
            EvalTransport::Stdio => "stdio",
        }
    }
}

/// Who acts in a run.
enum Driver {
    /// The task's reference calls, sent as an MCP client.
    Reference,
    /// The agent configured in `eval.toml`, as a child process.
    Command(String, AgentConfig),
}

impl Driver {
    fn name(&self) -> &str {
        match self {
            Driver::Reference => "reference",
            Driver::Command(name, _) => name,
        }
    }
}

/// What every run of a suite shares.
struct EvalSetup<'a> {
    /// The `real` binary that serves MCP.
    exe: &'a Path,
    suite: &'a Path,
    driver: &'a Driver,
    transport: EvalTransport,
}

fn run_failed(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(re_agent::codes::EVAL_RUN_FAILED, message)
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), Diagnostic> {
    let text = serde_json::to_string_pretty(value).expect("plain data serializes");
    std::fs::write(path, text + "\n")
        .map_err(|e| run_failed(format!("cannot write {}: {e}", path.display())))
}

fn cmd_eval(
    suite: &Path,
    agent: Option<String>,
    runs: u32,
    out: &Path,
    transport: EvalTransport,
) -> ExitCode {
    let tasks = match load_suite(suite) {
        Ok(t) => t,
        Err(d) => return fail(&d),
    };
    let driver = match agent {
        Some(name) => match find_agent(&name, &config_paths()) {
            Ok(config) => Driver::Command(name, config),
            Err(d) => return fail(&d),
        },
        None => Driver::Reference,
    };
    if matches!(driver, Driver::Reference)
        && let Some((name, _)) = tasks.iter().find(|(_, t)| t.reference.is_empty())
    {
        return fail(
            &Diagnostic::error(
                re_agent::codes::INVALID_EVAL_TASK,
                format!("task `{name}` has no reference calls to run"),
            )
            .with_hint("add `[[reference]]` calls, or run an agent with `--agent NAME`"),
        );
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail(&run_failed(format!("cannot find the `real` binary: {e}"))),
    };
    let suite_name = std::fs::canonicalize(suite)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "suite".into());
    let suite_out = out.join(&suite_name);
    let setup = EvalSetup {
        exe: &exe,
        suite,
        driver: &driver,
        transport,
    };
    let mut summary = Summary {
        suite: suite.display().to_string(),
        driver: driver.name().to_string(),
        transport: transport.as_str().to_string(),
        tasks: Vec::new(),
    };
    let mut summary_failed_runs = 0;
    for (name, task) in &tasks {
        let mut passed = 0;
        for run in 1..=runs.max(1) {
            let dir = suite_out.join(name).join(format!("run-{run}"));
            // A directory the harness may not use gets no score.json, so a
            // later run never takes it for one of its own.
            if let Err(d) = prepare_run_dir(&dir) {
                eprint!("{}", re_core::render_human_all(std::slice::from_ref(&d)));
                println!("{name} run {run}: FAILED (not run)");
                summary_failed_runs += 1;
                continue;
            }
            let score = run_task(&setup, name, task, run, &dir).unwrap_or_else(|d| {
                eprint!("{}", re_core::render_human_all(std::slice::from_ref(&d)));
                Score {
                    task: name.clone(),
                    run,
                    driver: driver.name().to_string(),
                    problems: vec![format!("{} {}", d.code, d.message)],
                    ..Score::default()
                }
            });
            if let Err(d) = write_json(&dir.join("score.json"), &score) {
                eprint!("{}", re_core::render_human_all(&[d]));
            }
            let failed: Vec<String> = score
                .checks
                .iter()
                .filter(|c| c.ok != Some(true))
                .map(|c| format!("check {}: {}", c.check, c.detail))
                .chain(score.problems.iter().cloned())
                .collect();
            println!(
                "{name} run {run}: {} ({} tool calls)",
                if score.passed { "passed" } else { "FAILED" },
                score.tool_calls
            );
            for line in &failed {
                println!("  {line}");
            }
            passed += u32::from(score.passed);
        }
        summary.tasks.push(TaskSummary {
            task: name.clone(),
            passed,
            runs: runs.max(1),
        });
    }
    if let Err(d) = std::fs::create_dir_all(&suite_out)
        .map_err(|e| run_failed(format!("cannot create {}: {e}", suite_out.display())))
        .and_then(|()| write_json(&suite_out.join("summary.json"), &summary))
    {
        return fail(&d);
    }
    print!("{}", summary.table());
    println!("results in {}", suite_out.display());
    if summary.all_passed() && summary_failed_runs == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Runs one task once in `dir` and scores it.
fn run_task(
    setup: &EvalSetup<'_>,
    name: &str,
    task: &Task,
    run: u32,
    dir: &Path,
) -> Result<Score, Diagnostic> {
    let started = Instant::now();
    // Absolute, since the agent runs in the project and the client config
    // and server arguments name these paths.
    let dir = &std::fs::canonicalize(dir)
        .map_err(|e| run_failed(format!("cannot resolve {}: {e}", dir.display())))?;
    let project = dir.join("project");
    make_project(setup.suite, name, &task.project, &project)?;
    let policy_dir = project.join(".real");
    std::fs::create_dir_all(&policy_dir)
        .and_then(|()| {
            std::fs::write(
                policy_dir.join("agent-policy.toml"),
                Policy::init_default().to_toml(),
            )
        })
        .map_err(|e| run_failed(format!("cannot write the agent policy: {e}")))?;
    let baseline = {
        let mut opened = open_for_agents(&project)?;
        // A check must only read; refuse it before anything runs.
        let specs = opened.app.commands().specs();
        for check in &task.checks {
            if let Ok(CheckKind::Command { command, .. }) = check.kind()
                && specs.iter().find(|s| s.name == command).map(|s| s.tier) != Some(Tier::Read)
            {
                return Err(Diagnostic::error(
                    re_agent::codes::INVALID_EVAL_TASK,
                    format!(
                        "task `{name}` checks with `{command}`, which is not a read-tier command"
                    ),
                ));
            }
        }
        super::mcp::verify_json(&mut opened.app)?
    };

    let mut score = Score {
        task: name.to_string(),
        run,
        driver: setup.driver.name().to_string(),
        ..Score::default()
    };
    let transcript = dir.join("transcript.jsonl");
    let budget = Duration::from_secs(task.budget.seconds.max(1));
    let serve = Serve {
        exe: setup.exe,
        project: &project,
        transcript: &transcript,
        dir,
    };
    match setup.driver {
        Driver::Reference => drive_reference(
            &serve,
            setup.transport,
            &task.reference,
            budget,
            &mut score.problems,
        )?,
        Driver::Command(_, agent) => drive_command(
            &serve,
            setup.transport,
            agent,
            &task.prompt,
            budget,
            &mut score.problems,
        )?,
    }

    for entry in read_transcript(&transcript).unwrap_or_default() {
        score.count_call(entry.ok, entry.code.as_deref());
    }
    if score.tool_calls > task.budget.tool_calls {
        score.problems.push(format!(
            "{} tool calls, over the budget of {}",
            score.tool_calls, task.budget.tool_calls
        ));
    }

    let mut opened = open_for_agents(&project)?;
    let app = &mut opened.app;
    let current = super::mcp::verify_json(app)?;
    write_json(&dir.join("verify.json"), &current)?;
    let diff = verify_diff::diff(&baseline, &current, DiffOptions::default());
    write_json(&dir.join("verify-diff.json"), &diff)?;
    let ok = |data: Json| CommandOutcome {
        ok: true,
        changes: Vec::new(),
        diagnostics: Vec::new(),
        data,
    };
    let diff_json = json!({ "diff": diff });
    let mut runner = |command: &str, args: Json, dry_run: bool| match command {
        "verify" => ok(current.clone()),
        "verify.diff" => ok(diff_json.clone()),
        _ => app.dispatch(command, args, dry_run, &AllowAll, Origin::User),
    };
    score.checks = evaluate_checks(&task.checks, &Map::new(), Some("baseline"), &mut runner);
    score.passed = score.problems.is_empty() && score.checks.iter().all(|c| c.ok == Some(true));
    score.wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(score)
}

/// Makes `dir` empty: a finished run (it has `score.json`) is replaced;
/// anything else that is not empty is refused, so nothing else is deleted.
fn prepare_run_dir(dir: &Path) -> Result<(), Diagnostic> {
    if dir.join("score.json").is_file() {
        std::fs::remove_dir_all(dir)
            .map_err(|e| run_failed(format!("cannot clear {}: {e}", dir.display())))?;
    } else if std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some()) {
        return Err(run_failed(format!(
            "{} exists and is not a finished run; choose another --out",
            dir.display()
        )));
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| run_failed(format!("cannot create {}: {e}", dir.display())))
}

/// Folders a project copy leaves out: local state, build output, history.
const NOT_COPIED: &[&str] = &[".real", ".git", "out", "target"];

fn copy_project(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if NOT_COPIED.iter().any(|n| name == *n) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &to.join(&name))?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), to.join(&name))?;
        }
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Makes the task's project at `project`.
fn make_project(
    suite: &Path,
    name: &str,
    spec: &TaskProject,
    project: &Path,
) -> Result<(), Diagnostic> {
    let copy = |from: &Path| {
        copy_project(from, project).map_err(|e| {
            run_failed(format!(
                "cannot copy {} for task `{name}`: {e}",
                from.display()
            ))
        })
    };
    match spec {
        TaskProject::Example { example } => copy(&find_example(suite, example)?),
        TaskProject::Path { path } => copy(&suite.join(path)),
        TaskProject::Template { template, params } => {
            let set: Vec<String> = params
                .iter()
                .map(|(k, v)| {
                    let text = match v {
                        Json::String(s) => s.clone(),
                        Json::Array(a) => {
                            a.iter().map(Json::to_string).collect::<Vec<_>>().join(",")
                        }
                        other => other.to_string(),
                    };
                    format!("{k}={text}")
                })
                .collect();
            let options = crate::new::NewOptions {
                name: Some(name),
                template,
                set: &set,
                check: false,
            };
            crate::new::create_from(project, options).map(|_| ())
        }
    }
}

/// How a run's `real mcp` is started.
struct Serve<'a> {
    exe: &'a Path,
    project: &'a Path,
    transcript: &'a Path,
    /// The run directory, for the server's stderr and the client config.
    dir: &'a Path,
}

impl Serve<'_> {
    /// `real mcp` on the project, recording to the transcript.
    fn args(&self) -> Vec<String> {
        vec![
            "mcp".into(),
            "--project".into(),
            self.project.display().to_string(),
            "--record".into(),
            self.transcript.display().to_string(),
        ]
    }

    fn command(&self) -> Result<Command, Diagnostic> {
        let stderr = std::fs::File::create(self.dir.join("server.stderr"))
            .map_err(|e| run_failed(format!("cannot write server.stderr: {e}")))?;
        let mut command = Command::new(self.exe);
        command.args(self.args()).stderr(stderr);
        Ok(command)
    }

    /// Starts `real mcp --http` on a free loopback port and waits for its
    /// state file. Returns the server (stdin open: closing it stops the
    /// server), the URL and the token.
    fn start_http(&self) -> Result<(Child, String, String), Diagnostic> {
        let token = re_mcp::http::generate_token();
        let state = self.project.join(super::mcp::HTTP_STATE);
        let mut child = self
            .command()?
            .args(["--http", re_mcp::http::DEFAULT_ADDR])
            .env(re_mcp::http::TOKEN_ENV, &token)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| run_failed(format!("cannot start `real mcp --http`: {e}")))?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Ok(text) = std::fs::read_to_string(&state)
                && let Ok(json) = serde_json::from_str::<Json>(&text)
                && json["token"] == token.as_str()
                && let Some(url) = json["url"].as_str()
            {
                return Ok((child, url.to_string(), token));
            }
            if let Ok(Some(status)) = child.try_wait() {
                return Err(run_failed(format!(
                    "`real mcp --http` ended ({status}) before serving; see {}",
                    self.dir.join("server.stderr").display()
                )));
            }
            if Instant::now() >= deadline {
                let _ = wait_or_kill(&mut child, Duration::ZERO);
                return Err(run_failed("`real mcp --http` did not start serving"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Waits up to `limit` for `child`; `None` when it is still running.
fn wait_until(
    child: &mut Child,
    limit: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Starts the agent in a process group of its own (Unix), so
/// [`kill_tree`] reaches what it starts.
fn own_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// Kills `child` and every process it started that is still running: its
/// process group on Unix, its process tree (`taskkill /T`) on Windows.
fn kill_tree(child: &mut Child) {
    let pid = child.id().to_string();
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stderr(Stdio::null())
        .status();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// Stops a `real mcp --http` server by closing its stdin.
fn stop_http(mut child: Child) {
    drop(child.stdin.take());
    let _ = wait_or_kill(&mut child, Duration::from_secs(30));
}

/// An MCP client session over either transport.
enum Session {
    Http(HttpClient),
    Stdio(StdioClient),
}

impl Session {
    fn request(&mut self, id: u64, method: &str, params: Json) -> anyhow::Result<Json> {
        match self {
            Session::Http(c) => c.request(id, method, params),
            Session::Stdio(c) => c.request(id, method, params),
        }
    }

    fn notify(&mut self, method: &str, params: Json) -> anyhow::Result<()> {
        match self {
            Session::Http(c) => c.notify(method, params),
            Session::Stdio(c) => c.notify(method, params),
        }
    }

    fn set_timeout(&mut self, timeout: Duration) {
        match self {
            Session::Http(c) => c.set_timeout(timeout),
            Session::Stdio(c) => c.set_timeout(timeout),
        }
    }
}

/// Sends the reference calls, within `budget`. A call that fails, or one
/// the budget cuts off, is a problem of the run.
fn drive_reference(
    serve: &Serve<'_>,
    transport: EvalTransport,
    calls: &[ReferenceCall],
    budget: Duration,
    problems: &mut Vec<String>,
) -> Result<(), Diagnostic> {
    let deadline = Instant::now() + budget;
    let (mut session, server) = match transport {
        EvalTransport::Http => {
            let (child, url, token) = serve.start_http()?;
            let client =
                HttpClient::new(&url, Some(&token)).map_err(|e| run_failed(format!("{e:#}")))?;
            (Session::Http(client), Some(child))
        }
        EvalTransport::Stdio => {
            let client = StdioClient::spawn(serve.command()?)
                .map_err(|e| run_failed(format!("cannot start `real mcp`: {e:#}")))?;
            (Session::Stdio(client), None)
        }
    };
    let result = (|| -> anyhow::Result<()> {
        session.set_timeout(deadline.saturating_duration_since(Instant::now()));
        let init = session.request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "real-agent-eval", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        anyhow::ensure!(init.get("result").is_some(), "initialize failed: {init}");
        session.notify("notifications/initialized", json!({}))?;
        for (i, call) in calls.iter().enumerate() {
            let left = deadline.saturating_duration_since(Instant::now());
            anyhow::ensure!(
                !left.is_zero(),
                "the time budget ran out before call {}",
                i + 1
            );
            session.set_timeout(left);
            let reply = session.request(
                i as u64 + 2,
                "tools/call",
                json!({ "name": re_mcp::tool_name(&call.command), "arguments": call.args }),
            )?;
            let failed = reply.get("error").is_some() || reply["result"]["isError"] == true;
            if failed {
                let detail = reply["result"]["content"][0]["text"]
                    .as_str()
                    .map_or_else(|| reply.to_string(), str::to_string);
                anyhow::bail!(
                    "reference call {} (`{}`) failed: {detail}",
                    i + 1,
                    call.command
                );
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        problems.push(format!("{e:#}"));
    }
    match (session, server) {
        (Session::Stdio(client), _) => {
            let _ = client.finish(Duration::from_secs(30));
        }
        (Session::Http(_), Some(child)) => stop_http(child),
        (Session::Http(_), None) => {}
    }
    Ok(())
}

/// Runs the configured agent in the project, with the MCP endpoint and a
/// client config, until it exits or the budget (or its `timeout_s`) runs
/// out.
fn drive_command(
    serve: &Serve<'_>,
    transport: EvalTransport,
    agent: &AgentConfig,
    prompt: &str,
    budget: Duration,
    problems: &mut Vec<String>,
) -> Result<(), Diagnostic> {
    let limit = agent
        .timeout_s
        .map_or(budget, |s| budget.min(Duration::from_secs(s)));
    let config_path = serve.dir.join("mcp.json");
    let mut server = None;
    let mut env: Vec<(String, String)> = vec![("REAL_EVAL_PROMPT".into(), prompt.into())];
    let entry = match transport {
        EvalTransport::Http => {
            let (child, url, token) = serve.start_http()?;
            server = Some(child);
            env.push(("REAL_MCP_URL".into(), url.clone()));
            env.push((re_mcp::http::TOKEN_ENV.into(), token));
            json!({
                "type": "http",
                "url": url,
                "headers": { "Authorization": format!("Bearer ${{{}}}", re_mcp::http::TOKEN_ENV) }
            })
        }
        EvalTransport::Stdio => json!({
            "command": serve.exe.display().to_string(),
            "args": serve.args(),
        }),
    };
    let config = json!({ "mcpServers": { re_agent::init::SERVER_NAME: entry } });
    let outcome = (|| -> Result<(), Diagnostic> {
        write_json(&config_path, &config)?;
        let command = expand_command(&agent.command, prompt, &config_path, serve.project);
        let file = |name: &str| {
            std::fs::File::create(serve.dir.join(name))
                .map_err(|e| run_failed(format!("cannot write {name}: {e}")))
        };
        let mut agent_command = Command::new(&command[0]);
        own_process_group(&mut agent_command);
        let mut child = agent_command
            .args(&command[1..])
            .current_dir(serve.project)
            .envs(env)
            .envs(&agent.env)
            .stdin(Stdio::null())
            .stdout(file("agent.stdout")?)
            .stderr(file("agent.stderr")?)
            .spawn()
            .map_err(|e| run_failed(format!("cannot start the agent `{}`: {e}", command[0])))?;
        let finished = wait_until(&mut child, limit)
            .map_err(|e| run_failed(format!("waiting for the agent failed: {e}")))?;
        // Whatever the agent started (tools, a stdio `real mcp`) stops with
        // it, before the project is scored.
        kill_tree(&mut child);
        match finished {
            None => problems.push(format!("the agent was stopped after {}s", limit.as_secs())),
            Some(status) if !status.success() => {
                problems.push(format!("the agent exited with {status}"));
            }
            Some(_) => {}
        }
        Ok(())
    })();
    if let Some(child) = server {
        stop_http(child);
    }
    outcome
}
