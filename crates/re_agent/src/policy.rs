//! The agent policy (ADR-0020): `.real/agent-policy.toml`.
//!
//! ```toml
//! max_tier = "edit"
//!
//! [scopes]
//! allow = ["scripts/**", "scenes/**"]
//! deny = []
//!
//! [budgets]
//! frames = 600
//! milliseconds = 10000
//! memory_mb = 512
//! entities_per_transaction = 2000
//! ```
//!
//! [`Policy`] implements [`PolicyCheck`], so every call an agent makes
//! (through MCP or the editor socket) is checked before it runs:
//!
//! * the command's tier must not exceed `max_tier` (`RE8001`); a dry run
//!   needs only `read`;
//! * a file path in the arguments of a mutating call (`path`, `dest`, any
//!   `*_path`, each string of `paths`; see [`is_path_arg`]) must be relative,
//!   stay inside the project, avoid `.real/` and `.git/`, match an `allow`
//!   glob and no `deny` glob (`RE8002`). Commands that write files name their
//!   path arguments this way, and write through
//!   [`crate::commands::write_file`], which also refuses symbolic links;
//! * budgets (`RE8008`): `frames` caps the `frames` argument of `play.run`
//!   and `graph.generate` (a `frames` that is not a whole number is refused);
//!   `entities_per_transaction` caps the number of operations in a
//!   `scene.apply`. An omitted `frames` takes the command's own default,
//!   which its host caps with [`Policy::budgets`], as it does `milliseconds`
//!   and `memory_mb`.
//!
//! With no policy file, the maximum tier is `read`. `real agent init` writes
//! [`Policy::init_default`], which grants `edit` within the default scopes;
//! committing that file is the opt-in. `destructive` is never generated: a
//! person adds it.

use std::path::{Component, Path};

use re_command::{CommandSpec, PolicyCheck, Tier};
use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::{codes, glob, lock};

/// The policy file of a project (`<project>/.real/agent-policy.toml`).
pub fn policy_path(project: &Path) -> std::path::PathBuf {
    lock::state_dir(project).join("agent-policy.toml")
}

/// The path scopes `real agent init` grants (ADR-0020).
pub const DEFAULT_SCOPES: &[&str] = &[
    "scripts/**",
    "scenes/**",
    "graphs/**",
    "prefabs/**",
    "materials/**",
    "assets/**",
];

/// Directories no policy can open to agents: the engine's own state (which
/// holds this policy) and version control.
pub const PROTECTED_DIRS: &[&str] = &[".real", ".git"];

/// Commands whose `frames` argument the `frames` budget caps.
pub const FRAME_BUDGETED: &[&str] = &["play.run", "graph.generate"];

/// Argument names that hold a project file path.
pub const PATH_ARGS: &[&str] = &["path", "dest"];

/// `s` as a TOML basic string, quoted and escaped.
pub(crate) fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// Whether a top-level argument names a project path that the scopes check:
/// [`PATH_ARGS`], or any key ending in `_path` (a `paths` array has each of
/// its strings checked).
pub fn is_path_arg(key: &str) -> bool {
    PATH_ARGS.contains(&key) || key.ends_with("_path")
}

/// `rel` (a [`normalize`]d path) inside `project`, refused if any part of it
/// that exists is a symbolic link, so that a link in a project can't lead a
/// write into `.real/`, `.git/` or out of the project.
pub fn no_links(project: &Path, rel: &str) -> Result<std::path::PathBuf, &'static str> {
    let mut path = project.to_path_buf();
    let mut exists = true;
    for part in rel.split('/') {
        path.push(part);
        if !exists {
            continue;
        }
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err("the path goes through a symbolic link");
            }
            Ok(_) => {}
            Err(_) => exists = false,
        }
    }
    Ok(path)
}

/// Glob allow and deny lists for file-writing commands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scopes {
    /// A written path must match one of these.
    #[serde(default)]
    pub allow: Vec<String>,
    /// A written path must match none of these.
    #[serde(default)]
    pub deny: Vec<String>,
}

/// Budgets for agent calls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Budgets {
    /// The most frames one `play.run` or `graph.generate` may step.
    pub frames: u64,
    /// The most wall-clock milliseconds one budgeted command may take.
    pub milliseconds: u64,
    /// The most memory, in MiB, one budgeted command's world copy may use.
    pub memory_mb: u64,
    /// The most operations one transaction (`scene.apply`) may hold.
    pub entities_per_transaction: u64,
}

impl Default for Budgets {
    /// Moderate budgets: 10 s of game time at 60 Hz, 10 s of wall time.
    fn default() -> Self {
        Budgets {
            frames: 600,
            milliseconds: 10_000,
            memory_mb: 512,
            entities_per_transaction: 2_000,
        }
    }
}

/// The agent policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// The highest tier an agent may call (dry runs need only `read`).
    pub max_tier: Tier,
    /// Where file-writing commands may write.
    #[serde(default)]
    pub scopes: Scopes,
    /// Budgets.
    #[serde(default)]
    pub budgets: Budgets,
}

impl Default for Policy {
    /// The policy with no policy file: `read` only.
    fn default() -> Self {
        Policy::read_only()
    }
}

impl Policy {
    /// `read` only, no scopes, default budgets.
    pub fn read_only() -> Policy {
        Policy {
            max_tier: Tier::Read,
            scopes: Scopes::default(),
            budgets: Budgets::default(),
        }
    }

    /// What `real agent init` writes: `edit` within [`DEFAULT_SCOPES`].
    pub fn init_default() -> Policy {
        Policy {
            max_tier: Tier::Edit,
            scopes: Scopes {
                allow: DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect(),
                deny: Vec::new(),
            },
            budgets: Budgets::default(),
        }
    }

    /// Loads `.real/agent-policy.toml`, or [`Policy::read_only`] if there is
    /// none. A file that doesn't parse is `RE8009`.
    pub fn load(project: &Path) -> Result<Policy, Diagnostic> {
        let path = policy_path(project);
        match std::fs::read_to_string(&path) {
            Ok(text) => Policy::parse(&text).map_err(|d| d.at(Location::file(rel(&path, project)))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Policy::read_only()),
            Err(e) => Err(Diagnostic::error(
                codes::INVALID_POLICY,
                format!("could not read the agent policy: {e}"),
            )
            .at(Location::file(rel(&path, project)))),
        }
    }

    /// Parses policy TOML.
    pub fn parse(text: &str) -> Result<Policy, Diagnostic> {
        toml::from_str(text).map_err(|e| {
            Diagnostic::error(
                codes::INVALID_POLICY,
                format!("invalid agent policy: {}", e.message()),
            )
            .with_hint(
                "the keys are `max_tier` (\"read\", \"edit\" or \"destructive\"), \
                 `[scopes]` allow/deny and `[budgets]`; `real agent init` writes a valid file",
            )
        })
    }

    /// The policy as the TOML `real agent init` writes, with comments.
    pub fn to_toml(&self) -> String {
        let list = |v: &[String]| {
            let items: Vec<String> = v.iter().map(|s| toml_string(s)).collect();
            format!("[{}]", items.join(", "))
        };
        format!(
            "# Agent policy (ADR-0020). Committing this file lets agents edit the project.\n\
             # max_tier: \"read\", \"edit\" or \"destructive\" (destructive is never generated).\n\
             max_tier = \"{}\"\n\
             \n\
             # Where file-writing commands (script.write, file.delete, asset.import) may write.\n\
             # `**` matches any number of directories. `.real/` and `.git/` are always refused.\n\
             [scopes]\n\
             allow = {}\n\
             deny = {}\n\
             \n\
             [budgets]\n\
             # Frames one play.run or graph.generate may step.\n\
             frames = {}\n\
             # Wall-clock milliseconds and MiB of memory one budgeted command may use.\n\
             milliseconds = {}\n\
             memory_mb = {}\n\
             # Operations one transaction (scene.apply) may hold.\n\
             entities_per_transaction = {}\n",
            self.max_tier.as_str(),
            list(&self.scopes.allow),
            list(&self.scopes.deny),
            self.budgets.frames,
            self.budgets.milliseconds,
            self.budgets.memory_mb,
            self.budgets.entities_per_transaction,
        )
    }

    /// The budgets, for the hosts of budgeted commands.
    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    /// Checks a project-relative path a command would write. Returns the
    /// normalised path (with `/`), or `RE8002`.
    pub fn check_path(&self, command: &str, path: &str) -> Result<String, Diagnostic> {
        let refuse = |why: String| {
            Diagnostic::error(
                codes::OUTSIDE_SCOPE,
                format!("{command} may not write {path:?}: {why}"),
            )
            .at(Location::command(command.to_string()))
            .with_hint(format!(
                "write inside the policy's scopes ({}); a person can widen `[scopes]` in \
                 .real/agent-policy.toml",
                if self.scopes.allow.is_empty() {
                    "none are granted".to_string()
                } else {
                    self.scopes.allow.join(", ")
                }
            ))
        };
        let normal = normalize(path).map_err(|why| refuse(why.to_string()))?;
        let first = normal.split('/').next().unwrap_or("");
        if PROTECTED_DIRS.contains(&first) {
            return Err(refuse(format!("`{first}/` is never writable by agents")));
        }
        if self.scopes.deny.iter().any(|g| glob::matches(g, &normal)) {
            return Err(refuse("it matches a `deny` scope".to_string()));
        }
        if !self.scopes.allow.iter().any(|g| glob::matches(g, &normal)) {
            return Err(refuse("it matches no `allow` scope".to_string()));
        }
        Ok(normal)
    }
}

/// A project-relative path with `/` separators, refusing absolute paths,
/// `..` and empty paths.
pub fn normalize(path: &str) -> Result<String, &'static str> {
    let unified = path.replace('\\', "/");
    if unified.starts_with('/') || (unified.len() > 1 && unified.as_bytes()[1] == b':') {
        return Err("the path must be relative to the project root");
    }
    let mut parts = Vec::new();
    for c in Path::new(&unified).components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => return Err("the path must not contain `..`"),
            Component::RootDir | Component::Prefix(_) => {
                return Err("the path must be relative to the project root");
            }
        }
    }
    if parts.is_empty() {
        return Err("the path is empty");
    }
    Ok(parts.join("/"))
}

fn rel(path: &Path, project: &Path) -> String {
    path.strip_prefix(project)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn budget_error(command: &str, what: String) -> Diagnostic {
    Diagnostic::error(
        codes::OVER_BUDGET,
        format!("{command} is over the agent budget: {what}"),
    )
    .at(Location::command(command.to_string()))
    .with_hint("ask for less (fewer frames or operations), or a person can raise `[budgets]` in .real/agent-policy.toml")
}

impl PolicyCheck for Policy {
    fn check(&self, spec: &CommandSpec, args: &Json, dry_run: bool) -> Result<(), Diagnostic> {
        let name = spec.name.as_str();
        let needed = if dry_run { Tier::Read } else { spec.tier };
        if needed > self.max_tier {
            return Err(Diagnostic::error(
                codes::TIER_REFUSED,
                format!(
                    "{name} needs the {} tier, but the agent policy allows only {}",
                    spec.tier.as_str(),
                    self.max_tier.as_str()
                ),
            )
            .at(Location::command(name.to_string()))
            .with_hint(if spec.tier == Tier::Destructive {
                "call it with `dry_run: true` to see what it would do; only a person can grant \
                 `destructive` in .real/agent-policy.toml"
                    .to_string()
            } else {
                "call it with `dry_run: true` to see what it would do, or ask the user to run \
                 `real agent init` (which grants `edit`) and commit .real/agent-policy.toml"
                    .to_string()
            }));
        }
        if FRAME_BUDGETED.contains(&name)
            && let Some(frames) = args.get("frames")
        {
            // A number that isn't a whole count is refused here rather than
            // left to the handler, so the budget can't be slipped.
            match frames.as_u64() {
                Some(n) if n <= self.budgets.frames => {}
                Some(n) => {
                    return Err(budget_error(
                        name,
                        format!("{n} frames asked, {} allowed", self.budgets.frames),
                    ));
                }
                None => {
                    return Err(budget_error(
                        name,
                        format!("`frames` must be a whole number, got {frames}"),
                    ));
                }
            }
        }
        if let Some(ops) = args.get("ops").and_then(Json::as_array)
            && ops.len() as u64 > self.budgets.entities_per_transaction
        {
            return Err(budget_error(
                name,
                format!(
                    "{} operations in one transaction, {} allowed",
                    ops.len(),
                    self.budgets.entities_per_transaction
                ),
            ));
        }
        if !dry_run
            && spec.tier > Tier::Read
            && let Some(obj) = args.as_object()
        {
            for (key, value) in obj {
                if is_path_arg(key)
                    && let Some(path) = value.as_str()
                {
                    self.check_path(name, path)?;
                }
                if key == "paths"
                    && let Some(paths) = value.as_array()
                {
                    for path in paths.iter().filter_map(Json::as_str) {
                        self.check_path(name, path)?;
                    }
                }
            }
        }
        Ok(())
    }
}
