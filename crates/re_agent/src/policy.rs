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
//! * every file a call that is not a dry run would write, whatever its tier
//!   (ADR-0020's amendment for WP44), passes [`Policy::check_path`]
//!   (`RE8002`): the `write` paths its spec declares
//!   ([`re_command::CommandSpec::paths`]) and the files a handler names
//!   itself ([`re_command::Call::check_write`]); its `output` paths
//!   (screenshots) pass [`Policy::check_output`], which also allows any
//!   `.png` under `.real/out/`. The rules hold on case-insensitive and
//!   Windows file systems, whatever the platform;
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

use std::path::{Component, Path, PathBuf};

use re_command::{CommandSpec, PathAccess, PolicyCheck, Tier};
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
    "anims/**",
    "prefabs/**",
    "materials/**",
    "assets/**",
];

/// Directories no policy can open to agents: the engine's own state (which
/// holds this policy) and version control.
pub const PROTECTED_DIRS: &[&str] = &[".real", ".git"];

/// The engine's output folder: an image an agent asks for may go anywhere
/// under it, whatever the scopes.
pub const OUTPUT_DIR: &str = ".real/out/";

/// Commands whose `frames` argument the `frames` budget caps.
pub const FRAME_BUDGETED: &[&str] = &["play.run", "graph.generate"];

/// `s` as a TOML basic string, quoted and escaped.
pub(crate) fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// The code points HFS+ ignores in names (Git's `core.protectHFS`), so that
/// `.g\u{200C}it` is `.git` there.
fn hfs_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{200C}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{206A}'..='\u{206F}' | '\u{FEFF}'
    )
}

/// `s` as case-insensitive file systems compare names: ASCII lowercase,
/// without the code points HFS+ ignores.
pub fn fold(s: &str) -> String {
    s.chars()
        .filter(|&c| !hfs_ignorable(c))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether `part`, the first part of a project path, is `.real` or `.git`
/// on some file system: compared [`fold`]ed, without an NTFS stream (`:…`)
/// or the trailing dots and spaces Windows drops, and including their 8.3
/// short names (`GIT~1`, `REAL~1`), as Git's `core.protectNTFS` knows them.
pub fn is_protected_dir(part: &str) -> bool {
    let mut name = fold(part);
    if let Some(stream) = name.find(':') {
        name.truncate(stream);
    }
    let name = name.trim_end_matches(['.', ' ']);
    PROTECTED_DIRS.iter().any(|dir| {
        name == *dir
            || name
                .strip_prefix(&dir[1..])
                .and_then(|rest| rest.strip_prefix('~'))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Why a part of a project path is refused on every platform, if it is: a
/// spelling some file system reads as another name. Windows drops a
/// trailing dot or space (`.git.` is `.git`) and reads `:` as a drive or a
/// stream of another file (`key.txt::$DATA` is `key.txt`); an 8.3 short
/// name can stand for any long name (`GIT~1` for `.git`, or a denied
/// folder's); and a device name (`CON`, `NUL`, `COM1`, with or without an
/// extension) is not a file.
pub fn odd_spelling(part: &str) -> Option<&'static str> {
    if part.ends_with(['.', ' ']) {
        return Some("a part of it ends in `.` or a space, which Windows drops");
    }
    if part.contains(':') {
        return Some("a part of it contains `:`, which Windows reads as a drive or a stream");
    }
    if short_name_shape(part) {
        return Some("a part of it has the shape of a Windows short name, such as `GIT~1`");
    }
    let stem = part.split('.').next().unwrap_or("").trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    let numbered = |prefix: &str| {
        upper.strip_prefix(prefix).is_some_and(|n| {
            let mut chars = n.chars();
            matches!(
                (chars.next(), chars.next()),
                (Some('0'..='9' | '¹' | '²' | '³'), None)
            )
        })
    };
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT")
    {
        return Some("a part of it is a name Windows keeps for a device");
    }
    None
}

/// Whether `part` could be an 8.3 short name Windows gave a longer name:
/// `~` and a digit, in at most eight characters and one `.` and three more
/// (`GIT~1`, `SECRET~1.TXT`). A longer name never is one, such as the
/// importer's `Rose~2.luau` for a second script called `Rose`.
fn short_name_shape(part: &str) -> bool {
    let (stem, ext) = part.split_once('.').unwrap_or((part, ""));
    stem.chars().count() <= 8
        && ext.chars().count() <= 3
        && !ext.contains('.')
        && part
            .as_bytes()
            .windows(2)
            .any(|w| w[0] == b'~' && w[1].is_ascii_digit())
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

/// Glob allow and deny lists for the files commands write.
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
    /// Where commands may write files.
    #[serde(default)]
    pub scopes: Scopes,
    /// Budgets.
    #[serde(default)]
    pub budgets: Budgets,
    /// The project the policy guards, for the checks that look at the files
    /// themselves (links, an image's target). [`Policy::load`] sets it; a
    /// policy that does not know it refuses every write.
    #[serde(skip)]
    project: Option<PathBuf>,
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
            project: None,
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
            project: None,
        }
    }

    /// The policy, guarding the project at `project` ([`Policy::load`] does
    /// this for the policy it reads).
    pub fn in_project(mut self, project: &Path) -> Policy {
        self.project = Some(project.to_path_buf());
        self
    }

    /// The project the policy guards, if it knows it.
    pub fn project(&self) -> Option<&Path> {
        self.project.as_deref()
    }

    /// Loads `.real/agent-policy.toml`, or [`Policy::read_only`] if there is
    /// none, guarding `project`. A file that doesn't parse is `RE8009`.
    pub fn load(project: &Path) -> Result<Policy, Diagnostic> {
        let path = policy_path(project);
        match std::fs::read_to_string(&path) {
            Ok(text) => Policy::parse(&text)
                .map(|p| p.in_project(project))
                .map_err(|d| d.at(Location::file(rel(&path, project)))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Policy::read_only().in_project(project))
            }
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
             # Where commands may write files (script.write, graph edits, asset.import, an\n\
             # agent's screenshot outside .real/out/...). `**` matches any number of\n\
             # directories. `.real/` and `.git/` are always refused, and `deny` ignores case.\n\
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

    /// Checks a project-relative path a command would write, and returns it
    /// normalised (with `/`), or refuses it with `RE8002`. The rules are
    /// those of the strictest file system a project can sit on, whatever
    /// the platform:
    ///
    /// * the path is relative, with no `..` ([`normalize`]), and no part of
    ///   it has an [`odd_spelling`];
    /// * its first part is not `.real` or `.git`, however spelled
    ///   ([`is_protected_dir`]);
    /// * it matches no `deny` scope, compared [`fold`]ed (a denied folder in
    ///   another case is still denied), and an `allow` scope as written (a
    ///   path in another case than its scope is refused);
    /// * no part of it that exists is a symbolic link ([`no_links`]), which
    ///   needs the policy to know its project.
    pub fn check_path(&self, command: &str, path: &str) -> Result<String, Diagnostic> {
        let normal = self.spelled(command, path)?;
        let refuse = |why: String| self.refusal(command, path, why);
        let first = normal.split('/').next().unwrap_or("");
        if is_protected_dir(first) {
            return Err(refuse(format!(
                "`{first}/` is `.real/` or `.git/`, which agents never write"
            )));
        }
        let folded = fold(&normal);
        if let Some(g) = self
            .scopes
            .deny
            .iter()
            .find(|g| glob::matches(&fold(g), &folded))
        {
            return Err(refuse(format!("it matches the `deny` scope {g:?}")));
        }
        if !self.scopes.allow.iter().any(|g| glob::matches(g, &normal)) {
            return Err(refuse("it matches no `allow` scope".to_string()));
        }
        self.unlinked(command, path, &normal)?;
        Ok(normal)
    }

    /// Checks the path of an image a command would write for an agent (a
    /// screenshot), and returns it normalised, or refuses it with `RE8002`.
    /// It must end in `.png`. Under [`OUTPUT_DIR`] it needs only what every
    /// path needs (relative, no odd spellings, no links); anywhere else it
    /// is checked as a write ([`Policy::check_path`]) and must not exist
    /// yet, so that an image never replaces a file.
    pub fn check_output(&self, command: &str, path: &str) -> Result<String, Diagnostic> {
        let hint = |d: Diagnostic| {
            d.with_hint(format!(
                "leave the path out to write under {OUTPUT_DIR}, where images may always go, or \
                 name a new `.png` file inside the policy's scopes ({})",
                self.allowed()
            ))
        };
        let normal = self.spelled(command, path).map_err(hint)?;
        if !normal.to_ascii_lowercase().ends_with(".png") {
            let why = "an image goes to a `.png` file".to_string();
            return Err(hint(self.refusal(command, path, why)));
        }
        if normal.starts_with(OUTPUT_DIR) {
            self.unlinked(command, path, &normal).map_err(hint)?;
            return Ok(normal);
        }
        let normal = self.check_path(command, path).map_err(hint)?;
        let exists = self
            .project
            .as_deref()
            .is_some_and(|p| std::fs::symlink_metadata(p.join(&normal)).is_ok());
        if exists {
            let why = "it exists, and an agent's image never replaces a file".to_string();
            return Err(hint(self.refusal(command, path, why)));
        }
        Ok(normal)
    }

    /// `path` [`normalize`]d, refused if a part of it has an
    /// [`odd_spelling`].
    fn spelled(&self, command: &str, path: &str) -> Result<String, Diagnostic> {
        let normal = normalize(path).map_err(|why| self.refusal(command, path, why.into()))?;
        if let Some(why) = normal.split('/').find_map(odd_spelling) {
            return Err(self.refusal(command, path, why.into()));
        }
        Ok(normal)
    }

    /// Refuses `normal` if a part of it that exists is a symbolic link, or
    /// if the policy does not know its project.
    fn unlinked(&self, command: &str, path: &str, normal: &str) -> Result<(), Diagnostic> {
        let Some(project) = self.project.as_deref() else {
            return Err(self.refusal(
                command,
                path,
                "the agent policy was not loaded from a project, so it cannot check the files"
                    .into(),
            ));
        };
        no_links(project, normal)
            .map(drop)
            .map_err(|why| self.refusal(command, path, why.into()))
    }

    /// `RE8002`: `command` may not write `path`, with the scopes in the
    /// hint.
    fn refusal(&self, command: &str, path: &str, why: String) -> Diagnostic {
        Diagnostic::error(
            codes::OUTSIDE_SCOPE,
            format!("{command} may not write {path:?}: {why}"),
        )
        .at(Location::command(command.to_string()))
        .with_hint(format!(
            "write inside the policy's scopes ({}); a person can widen `[scopes]` in \
             .real/agent-policy.toml",
            self.allowed()
        ))
    }

    /// The `allow` scopes, for hints.
    fn allowed(&self) -> String {
        if self.scopes.allow.is_empty() {
            "none are granted".to_string()
        } else {
            self.scopes.allow.join(", ")
        }
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
        // Every file the call would write, whatever its tier; a dry run
        // writes nothing.
        if !dry_run {
            for arg in &spec.paths {
                for path in arg.values(args) {
                    match arg.access {
                        PathAccess::Write => {
                            self.check_path(name, path)?;
                        }
                        PathAccess::Output => {
                            self.check_output(name, path)?;
                        }
                        PathAccess::Read => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn check_write(&self, command: &str, path: &str) -> Result<(), Diagnostic> {
        self.check_path(command, path).map(drop)
    }
}
