//! `project.import` for agents (docs/M2_PLAN.md, WP17): `real import` as an
//! MCP tool, a dry run unless `write` is set.
//!
//! The input must be inside the project. A write goes through the server
//! that holds the project lock: the files the import writes are recorded in
//! the agent session first (so `checkpoint.restore` puts them back), then
//! the World is brought to the imported scene as one undoable transaction.

use std::path::Path;

use re_agent::session::{Session, snapshot_roots};
use re_agent::{Agent, Policy, codes as agent_codes};
use re_app::{App, AppConfig};
use re_command::{Call, CommandRegistry, Origin, Tier};
use re_core::{Diagnostic, Location};
use re_import::{ImportOptions, ImportReport};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

const COMMAND: &str = "project.import";

/// Arguments of `project.import`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportArgs {
    /// The input, relative to the project: a `*.project.json` file, a
    /// directory holding `default.project.json`, or a Luau loader script.
    pub input: String,
    /// Write the import into the project (default false: only report what
    /// would change).
    #[serde(default)]
    pub write: bool,
    /// Keep instances an earlier import created that the input no longer
    /// declares (default false: they are removed, which needs the
    /// `destructive` tier from an agent).
    #[serde(default)]
    pub keep_stale: bool,
}

/// The result of `project.import`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ImportOutput {
    /// True when the import was written into the project.
    pub written: bool,
    /// True when every instance was imported and nothing is an error.
    pub ok: bool,
    /// What the import does (or did).
    pub report: ImportReport,
}

fn refuse(code: re_core::Code, message: String, hint: &str) -> Diagnostic {
    Diagnostic::error(code, message)
        .at(Location::command(COMMAND))
        .with_hint(hint.to_string())
}

/// The input as a path inside the project, refused when it leaves the
/// project or passes through a link.
fn input_path(project: &Path, input: &str) -> Result<std::path::PathBuf, Diagnostic> {
    let hint =
        "copy the source project into this project, or have a person run `real import <INPUT>`";
    let rel = re_agent::policy::normalize(input).map_err(|why| {
        refuse(
            agent_codes::OUTSIDE_SCOPE,
            format!("{COMMAND} may not read {input:?}: {why}"),
            hint,
        )
    })?;
    re_agent::policy::no_links(project, &rel).map_err(|why| {
        refuse(
            agent_codes::OUTSIDE_SCOPE,
            format!("{COMMAND} may not read {input:?}: {why}"),
            hint,
        )
    })
}

fn classes() -> Result<re_compat::ClassRegistry, Diagnostic> {
    super::import::classes()
}

fn project_import(
    app: &mut App,
    agent: &Agent,
    policy: &Policy,
    args: ImportArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let project = app.project().to_path_buf();
    let input = input_path(&project, &args.input)?;
    let classes = classes()?;
    let mut opts = ImportOptions {
        dry_run: true,
        keep_stale: args.keep_stale,
    };
    let plan = re_import::import(&input, &project, &classes, &opts)?;
    if !args.write || call.dry_run() {
        let ok = plan.ok();
        return Ok(json!(ImportOutput {
            written: false,
            ok,
            report: plan,
        }));
    }

    if matches!(call.origin(), Origin::Agent(_)) {
        for path in plan.files.written.iter().chain(&plan.files.deleted) {
            policy.check_path(COMMAND, path)?;
        }
        if !plan.files.deleted.is_empty() && policy.max_tier < Tier::Destructive {
            return Err(refuse(
                agent_codes::TIER_REFUSED,
                format!(
                    "the import removes {} file(s) of instances the input no longer declares, which needs the destructive tier; the agent policy allows only {}",
                    plan.files.deleted.len(),
                    policy.max_tier.as_str()
                ),
                "pass `keep_stale: true` to keep them; only a person can grant `destructive` in .real/agent-policy.toml",
            ));
        }
    }

    // Record every file before it changes, so a checkpoint can put it back.
    agent.with_session(app.world(), |s| {
        for path in plan.files.written.iter().chain(&plan.files.deleted) {
            s.touch(path)?;
        }
        Ok(())
    })?;
    opts.dry_run = false;
    let report = re_import::import(&input, &project, &classes, &opts)?;

    // Bring the World to the scene the import wrote, as one transaction.
    let imported = App::open(&project, AppConfig::load(&project)?)?;
    let ops = Session::restore_roots_ops(app.world(), &snapshot_roots(imported.world()));
    drop(imported);
    call.transact(app, format!("import {}", args.input), ops)?;
    app.edit_frame();
    if report.files.written.iter().any(|f| f == "real.toml") {
        call.push_diagnostic(
            Diagnostic::warning(
                re_import::codes::PROJECT_UNSUITABLE,
                "the import changed real.toml; restart `real mcp` to load the new configuration",
            )
            .at(Location::file("real.toml")),
        );
    }
    let ok = report.ok();
    Ok(json!(ImportOutput {
        written: true,
        ok,
        report,
    }))
}

/// Registers `project.import` (edit tier) for `agent`, held to `policy` when
/// an agent calls it.
pub(crate) fn register(registry: &mut CommandRegistry<App>, agent: &Agent, policy: Policy) {
    let agent = agent.clone();
    registry.register_typed_output::<ImportArgs, ImportOutput, _>(
        COMMAND,
        "Import a Rojo-style project or a Studio loader script from inside the project into this compat project. Only reports what would change unless `write` is true; a write is one undoable step and is recorded in the agent session.",
        Tier::Edit,
        true,
        move |app, args, call| project_import(app, &agent, &policy, args, call),
    );
}
