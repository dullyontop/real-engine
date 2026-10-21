//! [`AppConfig`]: a project's `real.toml` (ARCHITECTURE §8) plus the runtime
//! options a caller sets (headless or windowed, the frame size).

use std::path::Path;

use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};

use crate::codes;

/// The file name of a project's configuration.
pub const CONFIG_FILE: &str = "real.toml";

/// How [`App::open`](crate::App::open) runs a project.
///
/// The file sections (`project`, `sim`, `render`, `scripts`, `input`, `verify`)
/// come from `real.toml` through [`AppConfig::load`]; every field has a default,
/// so a minimal `real.toml` is `[project]` with a name. The remaining fields are
/// runtime options and are never read from the file.
///
/// Fields will be added: build one from [`AppConfig::load`] or
/// `AppConfig::default()` and set fields, or end a struct literal with
/// `..AppConfig::default()`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// `[project]`.
    #[serde(default)]
    pub project: ProjectConfig,
    /// `[sim]`.
    #[serde(default)]
    pub sim: SimConfig,
    /// `[render]`.
    #[serde(default)]
    pub render: RenderConfig,
    /// `[scripts]`.
    #[serde(default)]
    pub scripts: ScriptsConfig,
    /// `[input]`: the action bindings, read by `re_input::InputConfig`.
    #[serde(default)]
    pub input: re_input::InputConfig,
    /// `[verify]`.
    #[serde(default)]
    pub verify: VerifyConfig,
    /// `[compat]`: present in a project in the Roblox-style compat layout
    /// (docs/M2_PLAN.md, WP9). Kept as written; `re_compat::CompatConfig`
    /// reads and checks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<toml::Table>,
    /// Runtime option: run without a window. Headless runs use a fixed frame
    /// time of one simulation step.
    #[serde(skip)]
    pub headless: bool,
    /// Runtime option: write the scene back to its file after every command
    /// that changed it (what `real do`, `real mcp` and the editor want). Off by
    /// default, so tests and `play.run` copies never touch the project.
    #[serde(skip)]
    pub persist_edits: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            project: ProjectConfig::default(),
            sim: SimConfig::default(),
            render: RenderConfig::default(),
            scripts: ScriptsConfig::default(),
            input: re_input::InputConfig::default(),
            verify: VerifyConfig::default(),
            compat: None,
            headless: true,
            persist_edits: false,
        }
    }
}

/// `[project]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    /// The project's name.
    #[serde(default)]
    pub name: String,
    /// The scene loaded at start and by `world.restart()`, relative to the project.
    #[serde(default = "default_entry_scene")]
    pub entry_scene: String,
}

fn default_entry_scene() -> String {
    "scenes/main.scene.json".into()
}

impl Default for ProjectConfig {
    fn default() -> Self {
        ProjectConfig {
            name: String::new(),
            entry_scene: default_entry_scene(),
        }
    }
}

/// `[sim]`: the fixed step (ADR-0009).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimConfig {
    /// Simulation steps per second.
    #[serde(default = "default_fixed_hz")]
    pub fixed_hz: u32,
    /// The most catch-up steps one frame runs.
    #[serde(default = "default_max_steps")]
    pub max_steps_per_frame: u32,
    /// Gravity in m/s².
    #[serde(default = "default_gravity")]
    pub gravity: [f32; 3],
}

fn default_fixed_hz() -> u32 {
    60
}
fn default_max_steps() -> u32 {
    5
}
fn default_gravity() -> [f32; 3] {
    [0.0, -9.81, 0.0]
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            fixed_hz: default_fixed_hz(),
            max_steps_per_frame: default_max_steps(),
            gravity: default_gravity(),
        }
    }
}

/// `[render]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderConfig {
    /// wgpu backend preference, in order (`vulkan`, `gl`, `metal`, `dx12`). The
    /// `RE_BACKENDS` environment variable wins over this.
    #[serde(default = "default_backends")]
    pub backends: Vec<String>,
    /// Frame width in pixels (the window's initial size; headless frames use it).
    #[serde(default = "default_width")]
    pub width: u32,
    /// Frame height in pixels.
    #[serde(default = "default_height")]
    pub height: u32,
    /// Linear RGBA clear colour, used when the scene does not set one.
    #[serde(default = "default_clear")]
    pub clear: [f32; 4],
}

fn default_backends() -> Vec<String> {
    vec!["vulkan".into(), "gl".into()]
}
fn default_width() -> u32 {
    1280
}
fn default_height() -> u32 {
    720
}
fn default_clear() -> [f32; 4] {
    [0.45, 0.62, 0.85, 1.0]
}

impl Default for RenderConfig {
    fn default() -> Self {
        RenderConfig {
            backends: default_backends(),
            width: default_width(),
            height: default_height(),
            clear: default_clear(),
        }
    }
}

/// `[scripts]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptsConfig {
    /// The script contexts the project uses.
    #[serde(default = "default_contexts")]
    pub contexts: Vec<String>,
    /// CPU time one script callback may use before it is interrupted, in ms.
    #[serde(default = "default_cpu_budget")]
    pub cpu_budget_ms: f32,
    /// Memory limit of one script context, in MiB.
    #[serde(default = "default_memory_limit")]
    pub memory_limit_mb: u32,
    /// The external type checker: `luau-lsp`, or a path to a patched `luau-analyze`.
    #[serde(default = "default_checker")]
    pub checker: String,
}

fn default_contexts() -> Vec<String> {
    vec!["game".into(), "tools".into()]
}
fn default_cpu_budget() -> f32 {
    4.0
}
fn default_memory_limit() -> u32 {
    256
}
fn default_checker() -> String {
    "luau-lsp".into()
}

impl Default for ScriptsConfig {
    fn default() -> Self {
        ScriptsConfig {
            contexts: default_contexts(),
            cpu_budget_ms: default_cpu_budget(),
            memory_limit_mb: default_memory_limit(),
            checker: default_checker(),
        }
    }
}

/// `[verify]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyConfig {
    /// `[verify.perf]`: budgets that give warnings in M1 (ADR-0022).
    #[serde(default)]
    pub perf: PerfBudget,
    /// Per-channel tolerance for golden screenshots, in levels (0–255): a pixel
    /// that differs by more than this in some channel is an outlier.
    #[serde(default = "default_golden_tolerance")]
    pub golden_mean_tolerance: f32,
    /// The largest share of outlier pixels (0–1) a matching image may have.
    #[serde(default = "default_golden_outliers")]
    pub golden_max_outliers: f32,
}

fn default_golden_tolerance() -> f32 {
    2.0
}
fn default_golden_outliers() -> f32 {
    0.01
}

impl Default for VerifyConfig {
    fn default() -> Self {
        VerifyConfig {
            perf: PerfBudget::default(),
            golden_mean_tolerance: default_golden_tolerance(),
            golden_max_outliers: default_golden_outliers(),
        }
    }
}

/// `[verify.perf]`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerfBudget {
    /// The 95th percentile of the whole frame, in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_ms_p95: Option<f64>,
}

impl AppConfig {
    /// Reads `<project>/real.toml`.
    ///
    /// # Errors
    ///
    /// `RE0500` when the file does not exist, `RE0501` when it cannot be read or
    /// has an unknown key or a wrong type (located at the offending line).
    pub fn load(project: &Path) -> Result<AppConfig, Diagnostic> {
        let path = project.join(CONFIG_FILE);
        if !path.is_file() {
            return Err(Diagnostic::error(
                codes::PROJECT_NOT_FOUND,
                format!("{} is not a project: it has no {CONFIG_FILE}", project.display()),
            )
            .with_hint("pass the project directory with `--project DIR`, or create one with `real new DIR`"));
        }
        AppConfig::load_from(&re_core::files::ProjectFiles::project(project))
    }

    /// Reads `real.toml` through `files` (ADR-0037).
    ///
    /// # Errors
    ///
    /// `RE0501` when it cannot be read or has an unknown key or a wrong type.
    pub fn load_from(files: &re_core::files::ProjectFiles) -> Result<AppConfig, Diagnostic> {
        let text = files.read_str(CONFIG_FILE).map_err(|e| {
            Diagnostic::error(
                codes::CONFIG_INVALID,
                format!("cannot read {CONFIG_FILE}: {e}"),
            )
            .at(Location::file(CONFIG_FILE))
        })?;
        AppConfig::parse(&text)
    }

    /// Parses the text of a `real.toml`.
    ///
    /// # Errors
    ///
    /// `RE0501`, located at the offending line, for syntax errors, unknown keys and
    /// wrong types.
    pub fn parse(text: &str) -> Result<AppConfig, Diagnostic> {
        let config: AppConfig = toml::from_str(text).map_err(|e| {
            let location = match e.span() {
                Some(span) => {
                    let (line, column) = line_col(text, span.start);
                    Location::file_at(CONFIG_FILE, line, column)
                }
                None => Location::file(CONFIG_FILE),
            };
            Diagnostic::error(
                codes::CONFIG_INVALID,
                format!("invalid {CONFIG_FILE}: {}", e.message()),
            )
            .at(location)
            .with_hint("see ARCHITECTURE §8 for the sections and their keys")
        })?;
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> Result<(), Diagnostic> {
        let bad = |msg: String| {
            Err(Diagnostic::error(codes::CONFIG_INVALID, msg).at(Location::file(CONFIG_FILE)))
        };
        if !(1..=1000).contains(&self.sim.fixed_hz) {
            return bad(format!(
                "[sim] fixed_hz = {} is out of range (1 to 1000)",
                self.sim.fixed_hz
            ));
        }
        if !(1..=60).contains(&self.sim.max_steps_per_frame) {
            return bad(format!(
                "[sim] max_steps_per_frame = {} is out of range (1 to 60)",
                self.sim.max_steps_per_frame
            ));
        }
        if self.render.width == 0 || self.render.height == 0 {
            return bad("[render] width and height must be positive".into());
        }
        if self.project.entry_scene.is_empty() {
            return bad("[project] entry_scene is empty".into());
        }
        Ok(())
    }

    /// The `real.toml` text for a new project named `name` (what `real new` writes).
    pub fn template(name: &str) -> String {
        format!(
            r#"[project]
name = "{name}"
entry_scene = "scenes/main.scene.json"

[sim]
fixed_hz = 60
max_steps_per_frame = 5

[render]
backends = ["vulkan", "gl"]

[scripts]
contexts = ["game", "tools"]
cpu_budget_ms = 4
memory_limit_mb = 256
checker = "luau-lsp"

[input.buttons]
jump = ["key:Space", "pad:South"]

[input.axes]
move = [{{ composite = ["key:KeyW", "key:KeyS", "key:KeyA", "key:KeyD"] }}, {{ stick = "left", dead_zone = 0.15 }}]
look = [{{ mouse_delta = 0.002 }}, {{ stick = "right", dead_zone = 0.15 }}]

[verify.perf]
frame_ms_p95 = 33.0
"#
        )
    }
}

/// 1-based line and column of a byte offset.
pub(crate) fn line_col(text: &str, offset: usize) -> (u32, u32) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() as u32 + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) as u32 + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compat_section_is_kept_as_written() {
        let c =
            AppConfig::parse("[project]\nname = \"p\"\n\n[compat]\napi = \"roblox\"\n").unwrap();
        let compat = c.compat.unwrap();
        assert_eq!(compat.get("api").and_then(|v| v.as_str()), Some("roblox"));
        assert!(
            AppConfig::parse("[project]\nname = \"p\"\n")
                .unwrap()
                .compat
                .is_none()
        );
        // The compat layer's fixture project loads as a project.
        let fixture = include_str!("../../re_compat/tests/fixtures/scene/place/real.toml");
        assert!(AppConfig::parse(fixture).unwrap().compat.is_some());
    }

    #[test]
    fn minimal_file_takes_defaults() {
        let c = AppConfig::parse("[project]\nname = \"x\"\n").unwrap();
        assert_eq!(c.project.name, "x");
        assert_eq!(c.project.entry_scene, "scenes/main.scene.json");
        assert_eq!(c.sim.fixed_hz, 60);
        assert_eq!(c.sim.max_steps_per_frame, 5);
    }

    #[test]
    fn template_parses_with_its_input() {
        let c = AppConfig::parse(&AppConfig::template("demo")).unwrap();
        assert_eq!(c.project.name, "demo");
        assert!(c.input.buttons.contains_key("jump"));
        assert!(c.input.to_action_map().is_ok());
        assert_eq!(c.verify.perf.frame_ms_p95, Some(33.0));
    }

    #[test]
    fn unknown_key_is_located() {
        let e = AppConfig::parse("[project]\nname = \"x\"\n[sim]\nfixed_hz = 60\nbogus = 1\n")
            .unwrap_err();
        assert_eq!(e.code, codes::CONFIG_INVALID);
        match e.location {
            Some(Location::File { line, .. }) => assert_eq!(line, Some(5)),
            other => panic!("unexpected location {other:?}"),
        }
    }

    #[test]
    fn out_of_range_step_rate_is_refused() {
        let e = AppConfig::parse("[sim]\nfixed_hz = 0\n").unwrap_err();
        assert!(e.message.contains("fixed_hz"));
    }
}
