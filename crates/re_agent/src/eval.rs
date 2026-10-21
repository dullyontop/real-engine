//! Evaluation tasks and scores for `real agent eval` (ADR-0046). The engine
//! never calls a model: a task is solved by its own reference calls, or by
//! an agent the person configured in `eval.toml`, and scored by `real
//! verify` and the task's checks.
//!
//! A task (`<suite>/<task>.toml`):
//!
//! ```toml
//! prompt = "Add a box that falls onto the ground."
//! project = { template = "native" }          # or { example = "island" }, { path = "..." }
//! budget = { tool_calls = 20, seconds = 300 }
//!
//! [[checks]]
//! no_regressions = true
//!
//! [[reference]]
//! command = "recipe.apply"
//! args = { recipe = "falling-box" }
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

use crate::codes;
use crate::templates::{Check, CheckReport};

/// Where a task's project comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum TaskProject {
    /// A directory under the repository's `examples/` (found by walking up
    /// from the suite).
    Example {
        /// Its name (`island`).
        example: String,
    },
    /// A template, with parameters.
    Template {
        /// A built-in template's name.
        template: String,
        /// Its parameters.
        #[serde(default)]
        params: Map<String, Json>,
    },
    /// A project directory, relative to the task file.
    Path {
        /// The directory.
        path: String,
    },
}

/// What a run may spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Budget {
    /// The most tool calls; more fails the run.
    pub tool_calls: u64,
    /// The most wall-clock seconds; the agent is stopped then.
    pub seconds: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            tool_calls: 50,
            seconds: 600,
        }
    }
}

/// One call of a task's reference solution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCall {
    /// The command (`recipe.apply`); its MCP tool is `recipe_apply`.
    pub command: String,
    /// Its arguments.
    #[serde(default = "empty_object")]
    pub args: Json,
}

fn empty_object() -> Json {
    Json::Object(Map::new())
}

/// An evaluation task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// The request, in natural language, as a person would make it.
    pub prompt: String,
    /// The project the task starts from.
    pub project: TaskProject,
    /// What a run may spend.
    #[serde(default)]
    pub budget: Budget,
    /// What must hold afterwards.
    pub checks: Vec<Check>,
    /// Calls that solve the task (the `reference` driver).
    #[serde(default)]
    pub reference: Vec<ReferenceCall>,
}

fn invalid(file: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::INVALID_EVAL_TASK, message).at(Location::file(file))
}

/// Parses a task file.
///
/// # Errors
///
/// `RE8025` when it does not parse, has no checks, or a check is malformed.
pub fn parse_task(file: &str, text: &str) -> Result<Task, Diagnostic> {
    let task: Task = toml::from_str(text).map_err(|e| invalid(file, format!("{file}: {e}")))?;
    if task.checks.is_empty() {
        return Err(invalid(
            file,
            format!("{file}: a task needs at least one check"),
        ));
    }
    for check in &task.checks {
        check
            .kind()
            .map_err(|e| invalid(file, format!("{file}: {e}")))?;
    }
    Ok(task)
}

/// The tasks of the suite in `dir` (each `*.toml`, by name), in name order.
///
/// # Errors
///
/// `RE8025` when the directory cannot be read, holds no task, or a task is
/// invalid.
pub fn load_suite(dir: &Path) -> Result<Vec<(String, Task)>, Diagnostic> {
    let shown = dir.display().to_string();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| invalid(&shown, format!("cannot read the suite {shown}: {e}")))?;
    let mut tasks = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| invalid(&shown, format!("cannot read the suite {shown}: {e}")))?
            .path();
        if path.extension().is_none_or(|e| e != "toml") || !path.is_file() {
            continue;
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = path.display().to_string();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| invalid(&file, format!("cannot read {file}: {e}")))?;
        tasks.push((name, parse_task(&file, &text)?));
    }
    if tasks.is_empty() {
        return Err(invalid(&shown, format!("{shown} holds no task (`*.toml`)")));
    }
    tasks.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(tasks)
}

/// The directory of `examples/<name>`, looked for from `from` upwards.
///
/// # Errors
///
/// `RE8025` when no ancestor has it.
pub fn find_example(from: &Path, name: &str) -> Result<PathBuf, Diagnostic> {
    let start = std::fs::canonicalize(from).unwrap_or_else(|_| from.to_path_buf());
    start
        .ancestors()
        .map(|a| a.join("examples").join(name))
        .find(|p| p.join("real.toml").is_file())
        .ok_or_else(|| {
            invalid(
                &from.display().to_string(),
                format!("no examples/{name} above {}", from.display()),
            )
            .with_hint("run `real agent eval` inside the repository that has the example")
        })
}

/// One agent in `eval.toml`: `[agents.NAME]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// The program and its arguments. `{prompt}`, `{mcp_config}` (the
    /// generated `.mcp.json`) and `{project}` are replaced.
    pub command: Vec<String>,
    /// Extra environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The most seconds a run may take (the task's budget applies too).
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// `eval.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalConfig {
    /// The agents, by name.
    #[serde(default)]
    pub agents: BTreeMap<String, AgentConfig>,
}

/// The `eval.toml` files looked at, first match wins: the working
/// directory's, then the user's (`$XDG_CONFIG_HOME/real`,
/// `~/.config/real`, or `%APPDATA%\real` on Windows).
pub fn config_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("eval.toml")];
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(dir).join("real/eval.toml"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".config/real/eval.toml"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        paths.push(PathBuf::from(appdata).join("real").join("eval.toml"));
    }
    paths
}

/// The agent called `name`, from the first of `paths` that defines it.
///
/// # Errors
///
/// `RE8026` when a file does not parse or no file defines the agent.
pub fn find_agent(name: &str, paths: &[PathBuf]) -> Result<AgentConfig, Diagnostic> {
    let mut known = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let file = path.display().to_string();
        let config: EvalConfig = toml::from_str(&text).map_err(|e| {
            Diagnostic::error(codes::UNKNOWN_EVAL_AGENT, format!("{file}: {e}"))
                .at(Location::file(file.clone()))
        })?;
        if let Some(agent) = config.agents.get(name) {
            if agent.command.is_empty() {
                return Err(Diagnostic::error(
                    codes::UNKNOWN_EVAL_AGENT,
                    format!("{file}: agent `{name}` has an empty command"),
                ));
            }
            return Ok(agent.clone());
        }
        known.extend(config.agents.into_keys());
    }
    let looked: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let d = Diagnostic::error(
        codes::UNKNOWN_EVAL_AGENT,
        format!("no eval.toml defines the agent `{name}`"),
    );
    Err(
        match re_reflect::suggest(name, known.iter().map(String::as_str)) {
            Some(s) => d.with_hint(format!("did you mean `{s}`?")),
            None if !known.is_empty() => {
                d.with_hint(format!("the agents defined are: {}", known.join(", ")))
            }
            None => d.with_hint(format!(
                "add `[agents.{name}] command = [...]` to one of: {}",
                looked.join(", ")
            )),
        },
    )
}

/// Replaces `{prompt}`, `{mcp_config}` and `{project}` in an agent's
/// command.
pub fn expand_command(
    command: &[String],
    prompt: &str,
    mcp_config: &Path,
    project: &Path,
) -> Vec<String> {
    command
        .iter()
        .map(|a| {
            a.replace("{prompt}", prompt)
                .replace("{mcp_config}", &mcp_config.display().to_string())
                .replace("{project}", &project.display().to_string())
        })
        .collect()
}

/// The score of one run (`score.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Score {
    /// The task.
    pub task: String,
    /// The run, from 1.
    pub run: u32,
    /// `reference` or the agent's name.
    pub driver: String,
    /// Every check held, the budget held, and the driver finished.
    pub passed: bool,
    /// Each check.
    pub checks: Vec<CheckReport>,
    /// Tool calls the server answered.
    pub tool_calls: u64,
    /// Failed calls, by their first error code.
    pub refused: BTreeMap<String, u64>,
    /// Calls that returned an error (any code, or none).
    pub errors: u64,
    /// The run's wall-clock time.
    pub wall_ms: u64,
    /// Why it did not pass, besides failed checks (budget, timeout, the
    /// driver's own failure).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

impl Score {
    /// Counts one tool call of the transcript: `ok`, or failed with its
    /// first error `code`.
    pub fn count_call(&mut self, ok: bool, code: Option<&str>) {
        self.tool_calls += 1;
        if !ok {
            self.errors += 1;
            if let Some(code) = code {
                *self.refused.entry(code.to_string()).or_default() += 1;
            }
        }
    }
}

/// How many runs of one task passed (`summary.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    /// The task.
    pub task: String,
    /// Runs that passed.
    pub passed: u32,
    /// Runs.
    pub runs: u32,
}

/// `summary.json`: the suite's result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// The suite directory.
    pub suite: String,
    /// `reference` or the agent's name.
    pub driver: String,
    /// `http` or `stdio`.
    pub transport: String,
    /// Each task.
    pub tasks: Vec<TaskSummary>,
}

impl Summary {
    /// Whether every run of every task passed.
    pub fn all_passed(&self) -> bool {
        self.tasks.iter().all(|t| t.passed == t.runs)
    }

    /// A short table for the terminal.
    pub fn table(&self) -> String {
        let width = self
            .tasks
            .iter()
            .map(|t| t.task.len())
            .max()
            .unwrap_or(4)
            .max(4);
        let mut out = format!("{:width$}  passed\n", "task");
        for t in &self.tasks {
            out.push_str(&format!("{:width$}  {}/{}\n", t.task, t.passed, t.runs));
        }
        let (p, r) = self
            .tasks
            .iter()
            .fold((0, 0), |(p, r), t| (p + t.passed, r + t.runs));
        out.push_str(&format!(
            "{p}/{r} runs passed ({} over {})\n",
            self.driver, self.transport
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_parse_and_need_checks() {
        let task = parse_task(
            "t.toml",
            r#"
prompt = "Add a box."
project = { template = "native", params = {} }
budget = { tool_calls = 5 }

[[checks]]
no_regressions = true

[[reference]]
command = "recipe.apply"
args = { recipe = "falling-box" }
"#,
        )
        .unwrap();
        assert_eq!(
            task.budget,
            Budget {
                tool_calls: 5,
                seconds: 600
            }
        );
        assert!(matches!(task.project, TaskProject::Template { .. }));
        let island = parse_task(
            "t.toml",
            "prompt = \"x\"\nproject = { example = \"island\" }\n[[checks]]\nverify = \"ok\"\n",
        )
        .unwrap();
        assert_eq!(
            island.project,
            TaskProject::Example {
                example: "island".into()
            }
        );
        let none = parse_task(
            "t.toml",
            "prompt = \"x\"\nproject = { path = \".\" }\nchecks = []\n",
        );
        assert_eq!(none.unwrap_err().code, codes::INVALID_EVAL_TASK);
        let bad = parse_task(
            "t.toml",
            "prompt = \"x\"\nproject = { path = \".\" }\n[[checks]]\n",
        );
        assert_eq!(bad.unwrap_err().code, codes::INVALID_EVAL_TASK);
    }

    #[test]
    fn agents_come_from_the_first_file_that_has_them() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.toml");
        let b = dir.path().join("b.toml");
        std::fs::write(&a, "[agents.one]\ncommand = [\"one\", \"{prompt}\"]\n").unwrap();
        std::fs::write(&b, "[agents.two]\ncommand = [\"two\"]\ntimeout_s = 9\n").unwrap();
        let paths = [dir.path().join("missing.toml"), a, b];
        assert_eq!(find_agent("two", &paths).unwrap().timeout_s, Some(9));
        let err = find_agent("on", &paths).unwrap_err();
        assert_eq!(err.code, codes::UNKNOWN_EVAL_AGENT);
        assert!(err.hint.unwrap().contains("one"));
        let cmd = expand_command(
            &find_agent("one", &paths).unwrap().command,
            "do it",
            Path::new("m.json"),
            Path::new("p"),
        );
        assert_eq!(cmd, ["one", "do it"]);
    }

    #[test]
    fn scores_count_calls_and_summaries_add_up() {
        let mut score = Score::default();
        score.count_call(true, None);
        score.count_call(false, Some("RE8001"));
        score.count_call(false, Some("RE8001"));
        score.count_call(false, None);
        assert_eq!((score.tool_calls, score.errors), (4, 3));
        assert_eq!(score.refused["RE8001"], 2);
        let summary = Summary {
            suite: "s".into(),
            driver: "reference".into(),
            transport: "http".into(),
            tasks: vec![TaskSummary {
                task: "a".into(),
                passed: 2,
                runs: 2,
            }],
        };
        assert!(summary.all_passed());
        assert!(summary.table().contains("2/2 runs passed"));
    }
}
