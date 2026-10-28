//! [`AppConfig`]: a project's `real.toml` (ARCHITECTURE §8) plus the runtime
//! options a caller sets (headless or windowed, the frame size).

use std::collections::BTreeMap;
use std::path::Path;

use re_core::{Diagnostic, Location};
use re_types::audio::AudioGraphDesc;
use re_types::render::{ColorGrading, PostSettings, SunShadows};
use serde::{Deserialize, Serialize};

use crate::codes;

/// The file name of a project's configuration.
pub const CONFIG_FILE: &str = "real.toml";

/// How [`App::open`](crate::App::open) runs a project.
///
/// The file sections (`project`, `sim`, `render`, `scripts`, `proc`, `input`,
/// `audio`, `verify`)
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
    /// `[proc]`.
    #[serde(default)]
    pub proc: ProcConfig,
    /// `[input]`: the action bindings, read by `re_input::InputConfig`.
    #[serde(default)]
    pub input: re_input::InputConfig,
    /// `[audio]`: the mixer graph (ADR-0041), checked by `re_audio` when the
    /// app starts. Without it, the buses `music`, `sfx` and `ui`.
    #[serde(default, skip_serializing_if = "AudioGraphDesc::is_default")]
    pub audio: AudioGraphDesc,
    /// `[verify]`.
    #[serde(default)]
    pub verify: VerifyConfig,
    /// `[stream]`: world streaming's cell grid, radii and per-tick budgets
    /// (ADR-0042), read by `re_proc::stream`.
    #[serde(default)]
    pub stream: re_proc::stream::StreamConfig,
    /// `[assets]`: how imported textures are stored, block-compressed or
    /// RGBA8 (ADR-0047), read by `re_assets`.
    #[serde(default)]
    pub assets: re_assets::AssetsConfig,
    /// `[compat]`: present in a project in the Roblox-style compat layout
    /// (docs/M2_PLAN.md, WP9). Kept as written; `re_compat::CompatConfig`
    /// reads and checks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<toml::Table>,
    /// `[net]`: online play's port, players, rates and timeout (ADR-0050 §6).
    #[serde(default, skip_serializing_if = "crate::net::NetConfig::is_default")]
    pub net: crate::net::NetConfig,
    /// `[package]`: how `real build` packs the game (ADR-0037).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<crate::package::PackageConfig>,
    /// `[scenes]`: the scenes a game can load, by name, besides the entry
    /// scene (ADR-0059 §1; [`crate::scenes::SceneList`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scenes: BTreeMap<String, String>,
    /// `[saves]`: the data version save slots are written with, and whether
    /// a run that keeps data keeps them (ADR-0059 §7).
    #[serde(
        default,
        skip_serializing_if = "crate::scenes::saves::SavesConfig::is_default"
    )]
    pub saves: crate::scenes::saves::SavesConfig,
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
            proc: ProcConfig::default(),
            input: re_input::InputConfig::default(),
            audio: AudioGraphDesc::default(),
            verify: VerifyConfig::default(),
            stream: re_proc::stream::StreamConfig::default(),
            assets: re_assets::AssetsConfig::default(),
            compat: None,
            net: crate::net::NetConfig::default(),
            package: None,
            scenes: BTreeMap::new(),
            saves: crate::scenes::saves::SavesConfig::default(),
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
    /// `[render.post]`: exposure and the post-processing chain (ADR-0038). In a
    /// compat place, the exposure comes from its `Lighting`, and each kind of
    /// post effect the place has replaces this setting of that kind.
    #[serde(default)]
    pub post: PostSettings,
    /// `[render.shadows]`: the sun's shadow cascades (ADR-0038).
    #[serde(default)]
    pub shadows: SunShadows,
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
            post: PostSettings::default(),
            shadows: SunShadows::default(),
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
    /// The run's seed: `math.random` starts from it in the game context and
    /// from `seed + 1` in the tools context, so runs repeat. A replay's `seed`
    /// takes its place while the replay plays.
    #[serde(default)]
    pub seed: i64,
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
            seed: 0,
        }
    }
}

/// `[proc]`: the limits of Luau node kinds (ADR-0043 §4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcConfig {
    /// Safepoints one Luau node evaluation may use (`RE4101` past it).
    #[serde(default = "default_luau_budget")]
    pub luau_budget: u64,
    /// Memory limit of one Luau node evaluation, in MiB.
    #[serde(default = "default_luau_memory")]
    pub luau_memory_mb: u32,
}

fn default_luau_budget() -> u64 {
    re_script::generator::DEFAULT_BUDGET
}
fn default_luau_memory() -> u32 {
    re_script::generator::DEFAULT_MEMORY_MB as u32
}

impl Default for ProcConfig {
    fn default() -> Self {
        ProcConfig {
            luau_budget: default_luau_budget(),
            luau_memory_mb: default_luau_memory(),
        }
    }
}

impl ProcConfig {
    /// The limits every Luau node evaluation runs with.
    pub fn limits(&self) -> re_script::generator::Limits {
        re_script::generator::Limits {
            budget: self.luau_budget,
            memory_mb: self.luau_memory_mb as usize,
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
        check_render_keys(text)?;
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
        if let Some(problem) = render_range_problem(&self.render) {
            return bad(problem);
        }
        // A bad `[stream]` value is `RE4300`, from re_proc.
        self.stream
            .validate()
            .map_err(|d| d.at(Location::file(CONFIG_FILE)))?;
        self.scene_list()?;
        if let Some(problem) = self.saves.problem() {
            return Err(Diagnostic::error(codes::SAVES_SETTINGS_INVALID, problem)
                .at(Location::file(CONFIG_FILE)));
        }
        Ok(())
    }

    /// The scenes the game can load: `[scenes]` and the entry scene
    /// (ADR-0059 §1).
    ///
    /// # Errors
    ///
    /// `RE0572` when `[scenes]` is wrong.
    pub fn scene_list(&self) -> Result<crate::scenes::SceneList, Diagnostic> {
        crate::scenes::SceneList::new(&self.project, &self.scenes).map_err(|msg| {
            Diagnostic::error(codes::SCENES_INVALID, msg).at(Location::file(CONFIG_FILE))
        })
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

/// The first value of `[render.post]` or `[render.shadows]` outside the range
/// the renderer draws, as a message. Values are reported, never clamped.
fn render_range_problem(render: &RenderConfig) -> Option<String> {
    let (post, shadows) = (&render.post, &render.shadows);
    let at_least_0 = |v: f32| v.is_finite() && v >= 0.0;
    let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
    let finite = |v: f32| v.is_finite();
    let grading = post.grading.unwrap_or_default();
    let floats: [(&str, f32, bool, &str); 14] = [
        (
            "post] exposure",
            post.exposure,
            at_least_0(post.exposure),
            "0 or more",
        ),
        (
            "post] blur_radius",
            post.blur_radius,
            at_least_0(post.blur_radius),
            "0 or more",
        ),
        (
            "post.bloom] intensity",
            post.bloom.intensity,
            at_least_0(post.bloom.intensity),
            "0 or more",
        ),
        (
            "post.bloom] threshold",
            post.bloom.threshold,
            at_least_0(post.bloom.threshold),
            "0 or more",
        ),
        (
            "post.bloom] knee",
            post.bloom.knee,
            unit(post.bloom.knee),
            "0 to 1",
        ),
        (
            "post.bloom] radius",
            post.bloom.radius,
            at_least_0(post.bloom.radius),
            "0 or more",
        ),
        (
            "post.sun_rays] intensity",
            post.sun_rays.intensity,
            at_least_0(post.sun_rays.intensity),
            "0 or more",
        ),
        (
            "post.sun_rays] spread",
            post.sun_rays.spread,
            unit(post.sun_rays.spread),
            "0 to 1",
        ),
        (
            "post.grading] brightness",
            grading.brightness,
            finite(grading.brightness),
            "a finite number",
        ),
        (
            "post.grading] contrast",
            grading.contrast,
            grading.contrast.is_finite() && grading.contrast >= -1.0,
            "-1 or more",
        ),
        (
            "post.grading] saturation",
            grading.saturation,
            grading.saturation.is_finite() && grading.saturation >= -1.0,
            "-1 or more",
        ),
        (
            "shadows] distance",
            shadows.distance,
            shadows.distance.is_finite() && shadows.distance > 0.0,
            "more than 0",
        ),
        (
            "shadows] split_lambda",
            shadows.split_lambda,
            unit(shadows.split_lambda),
            "0 to 1",
        ),
        (
            "shadows] blend",
            shadows.blend,
            shadows.blend.is_finite() && (0.0..=0.5).contains(&shadows.blend),
            "0 to 0.5",
        ),
    ];
    if let Some((name, value, _, range)) = floats.iter().find(|f| !f.2) {
        let (section, key) = name.split_once("] ").expect("section and key");
        return Some(format!(
            "[render.{section}] {key} = {value} is out of range ({range})"
        ));
    }
    if let Some(c) = grading.tint.iter().find(|c| !at_least_0(**c)) {
        return Some(format!(
            "[render.post.grading] tint has {c}; each channel must be 0 or more"
        ));
    }
    if !(1..=4).contains(&shadows.cascades) {
        return Some(format!(
            "[render.shadows] cascades = {} is out of range (1 to 4)",
            shadows.cascades
        ));
    }
    None
}

/// Rejects unknown keys in `[render.post]` and `[render.shadows]`. Their types
/// come from `re_types`, which ignores unknown keys so snapshots stay readable
/// across versions; in `real.toml` a misspelt key must not pass silently.
fn check_render_keys(text: &str) -> Result<(), Diagnostic> {
    // Syntax errors were reported by the typed parse.
    let Ok(root) = toml::de::DeTable::parse(text) else {
        return Ok(());
    };
    let reference = |value: Result<toml::Table, toml::ser::Error>| value.unwrap_or_default();
    let known = [
        (
            "post",
            reference(toml::Table::try_from(PostSettings {
                grading: Some(ColorGrading::default()),
                ..PostSettings::default()
            })),
        ),
        (
            "shadows",
            reference(toml::Table::try_from(SunShadows::default())),
        ),
    ];
    let Some(toml::de::DeValue::Table(render)) = child(root.get_ref(), "render") else {
        return Ok(());
    };
    for (name, reference) in &known {
        let Some(toml::de::DeValue::Table(table)) = child(render, name) else {
            continue;
        };
        if let Some((section, key, offset, expected)) =
            unknown_key(table, reference, &format!("render.{name}"))
        {
            let (line, column) = line_col(text, offset);
            return Err(Diagnostic::error(
                codes::CONFIG_INVALID,
                format!(
                    "invalid {CONFIG_FILE}: unknown field `{key}` in [{section}], expected one of {expected}"
                ),
            )
            .at(Location::file_at(CONFIG_FILE, line, column))
            .with_hint("see ARCHITECTURE §8 for the sections and their keys"));
        }
    }
    Ok(())
}

/// The value of `key` in a parsed table.
fn child<'a, 'i>(table: &'a toml::de::DeTable<'i>, key: &str) -> Option<&'a toml::de::DeValue<'i>> {
    table
        .iter()
        .find(|(k, _)| k.get_ref().as_ref() == key)
        .map(|(_, v)| v.get_ref())
}

/// The first key of `table` (at any depth) that `reference` lacks: its
/// section, name, byte offset and the keys that section has.
fn unknown_key(
    table: &toml::de::DeTable<'_>,
    reference: &toml::Table,
    section: &str,
) -> Option<(String, String, usize, String)> {
    for (key, value) in table {
        let name: &str = key.get_ref().as_ref();
        match (reference.get(name), value.get_ref()) {
            (None, _) => {
                let expected = reference
                    .keys()
                    .map(|k| format!("`{k}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Some((
                    section.to_string(),
                    name.to_string(),
                    key.span().start,
                    expected,
                ));
            }
            (Some(toml::Value::Table(inner)), toml::de::DeValue::Table(sub)) => {
                if let Some(found) = unknown_key(sub, inner, &format!("{section}.{name}")) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
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

    /// ADR-0041's example `[audio]` section.
    #[test]
    fn audio_section_parses() {
        let c = AppConfig::parse(
            r#"
[project]
name = "p"

[audio]
default_bus = "sfx"

[audio.buses.music]
volume_db = -6.0

[audio.buses.sfx]
sends = { world_reverb = -9.0 }
effects = [
  { kind = "compressor", threshold_db = -18.0, ratio = 3.0 },
]

[audio.buses.footsteps]
parent = "sfx"
effects = [{ kind = "filter", mode = "high_pass", cutoff_hz = 120.0 }]

[audio.sends.world_reverb]
effects = [{ kind = "reverb", feedback = 0.85, damping = 0.3, mix = 1.0 }]

[audio.snapshots.underwater]
buses = { music = -12.0, sfx = -3.0 }
effects = [{ bus = "sfx", effect = 0, cutoff_hz = 600.0 }]
"#,
        )
        .unwrap();
        assert_eq!(c.audio.buses.len(), 3);
        assert_eq!(c.audio.buses["footsteps"].parent, "sfx");
        assert_eq!(c.audio.sends["world_reverb"].effects[0].kind(), "reverb");
        let snap = &c.audio.snapshots["underwater"];
        assert_eq!(
            snap.effects[0].params["cutoff_hz"],
            re_types::audio::EffectParam::Number(600.0)
        );
        assert!(
            AppConfig::parse("[project]\nname = \"p\"\n")
                .unwrap()
                .audio
                .is_default()
        );
        let e = AppConfig::parse("[audio.buses.sfx]\nvolume = 1.0\n").unwrap_err();
        assert_eq!(e.code, codes::CONFIG_INVALID);
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
    fn saves_take_defaults_and_refuse_version_0() {
        use crate::scenes::saves::{SaveMode, SavesConfig};
        let c = AppConfig::parse("[project]\nname = \"x\"\n").unwrap();
        assert!(c.saves.is_default());
        assert_eq!((c.saves.version, c.saves.mode), (1, SaveMode::Persist));
        let c = AppConfig::parse("[saves]\nversion = 3\nmode = \"session\"\n").unwrap();
        assert_eq!(
            c.saves,
            SavesConfig {
                version: 3,
                mode: SaveMode::Session
            }
        );
        let e = AppConfig::parse("[saves]\nversion = 0\n").unwrap_err();
        assert_eq!(e.code, codes::SAVES_SETTINGS_INVALID);
        assert_eq!(e.message, "[saves] version is 0; data versions start at 1");
        let e = AppConfig::parse("[saves]\nmode = \"cloud\"\n").unwrap_err();
        assert_eq!(e.code, codes::CONFIG_INVALID);
        // The explain page's example is a valid section.
        let page = include_str!("../explain/RE0576.md");
        let example = page
            .split("```toml\n")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .expect("a toml example");
        assert_eq!(AppConfig::parse(example).unwrap().saves.version, 2);
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

    #[test]
    fn post_and_shadow_tables_parse_with_defaults() {
        let c = AppConfig::parse(
            "[project]\nname = \"p\"\n\n[render.post]\nexposure = 2.0\ntone_mapping = \"AgX\"\n\n\
             [render.post.grading]\nsaturation = 0.2\n\n[render.shadows]\ncascades = 3\n",
        )
        .unwrap();
        assert_eq!(c.render.post.exposure, 2.0);
        assert_eq!(
            c.render.post.tone_mapping,
            re_types::render::ToneMapping::AgX
        );
        let grading = c.render.post.grading.unwrap();
        assert_eq!((grading.saturation, grading.tint), (0.2, [1.0; 3]));
        assert_eq!(c.render.shadows.cascades, 3);
        assert_eq!(c.render.shadows.distance, SunShadows::default().distance);
        // Without the tables: the defaults, and so the renderer's defaults.
        let plain = AppConfig::parse("[project]\nname = \"p\"\n").unwrap();
        assert_eq!(plain.render.post, PostSettings::default());
        assert_eq!(plain.render.shadows, SunShadows::default());
    }

    /// Every field `re_types` writes for the post and shadow settings is a key
    /// the config accepts: a new `Option` or skipped field there would
    /// otherwise be refused as unknown.
    #[test]
    fn every_post_and_shadow_field_is_a_known_key() {
        let post = PostSettings {
            grading: Some(re_types::render::ColorGrading::default()),
            ..PostSettings::default()
        };
        let mut render = toml::Table::new();
        render.insert("post".into(), toml::Value::try_from(post).unwrap());
        render.insert(
            "shadows".into(),
            toml::Value::try_from(SunShadows::default()).unwrap(),
        );
        let mut root = toml::Table::new();
        root.insert("render".into(), toml::Value::Table(render));
        let text = format!(
            "[project]\nname = \"p\"\n\n{}",
            toml::to_string(&root).unwrap()
        );
        for key in ["sun_rays", "bloom", "grading", "brightness", "split_lambda"] {
            assert!(text.contains(key), "{key} missing from\n{text}");
        }
        let c = AppConfig::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(c.render.post, post);
        assert_eq!(c.render.shadows, SunShadows::default());
    }

    #[test]
    fn unknown_post_and_shadow_keys_are_located() {
        for (text, key, section, line) in [
            (
                "[render.post]\nexposur = 1.0\n",
                "exposur",
                "render.post",
                5,
            ),
            (
                "[render.post.bloom]\nintensity = 1.0\nradiuss = 0.1\n",
                "radiuss",
                "render.post.bloom",
                6,
            ),
            (
                "[render.post]\ngrading = { tnt = [1.0, 1.0, 1.0] }\n",
                "tnt",
                "render.post.grading",
                5,
            ),
            (
                "[render]\nshadows = { cascade = 2 }\n",
                "cascade",
                "render.shadows",
                5,
            ),
        ] {
            let err = AppConfig::parse(&format!("[project]\nname = \"p\"\n\n{text}")).unwrap_err();
            assert_eq!(err.code, codes::CONFIG_INVALID);
            assert!(
                err.message
                    .contains(&format!("unknown field `{key}` in [{section}]")),
                "{}",
                err.message
            );
            let Some(Location::File { line: at, .. }) = err.location else {
                panic!("no file location: {err:?}");
            };
            assert_eq!(at, Some(line), "{text}");
        }
    }

    #[test]
    fn assets_choose_how_textures_are_stored() {
        let c = AppConfig::parse("[project]\nname = \"p\"\n").unwrap();
        assert_eq!(c.assets, re_assets::AssetsConfig::default());
        let c = AppConfig::parse(
            "[project]\nname = \"p\"\n\n[assets]\ntexture_compression = \"none\"\ntexture_quality = \"high\"\n",
        )
        .unwrap();
        assert_eq!(
            c.assets.texture_compression,
            re_assets::TextureCompression::None
        );
        assert_eq!(c.assets.texture_quality, re_assets::TextureQuality::High);
        for (text, line) in [
            ("[assets]\ntexture_compression = \"bc1\"\n", 5),
            ("[assets]\ntexture_qualty = \"high\"\n", 5),
        ] {
            let err = AppConfig::parse(&format!("[project]\nname = \"p\"\n\n{text}")).unwrap_err();
            assert_eq!(err.code, codes::CONFIG_INVALID, "{text}");
            let Some(Location::File { line: at, .. }) = err.location else {
                panic!("no file location: {err:?}");
            };
            assert_eq!(at, Some(line), "{text}");
        }
    }

    #[test]
    fn out_of_range_post_and_shadow_values_are_reported() {
        for (table, want) in [
            (
                "[render.post]\nexposure = -1.0",
                "[render.post] exposure = -1 is out of range (0 or more)",
            ),
            (
                "[render.post]\nexposure = nan",
                "[render.post] exposure = NaN is out of range (0 or more)",
            ),
            (
                "[render.post.bloom]\nknee = 2.0",
                "[render.post.bloom] knee = 2 is out of range (0 to 1)",
            ),
            (
                "[render.post.grading]\ntint = [1.0, -0.5, 1.0]",
                "[render.post.grading] tint has -0.5",
            ),
            (
                "[render.shadows]\ncascades = 5",
                "[render.shadows] cascades = 5 is out of range (1 to 4)",
            ),
            (
                "[render.shadows]\ncascades = 0",
                "[render.shadows] cascades = 0 is out of range (1 to 4)",
            ),
            (
                "[render.shadows]\ndistance = 0.0",
                "[render.shadows] distance = 0 is out of range (more than 0)",
            ),
            (
                "[render.shadows]\nsplit_lambda = 1.5",
                "[render.shadows] split_lambda = 1.5 is out of range (0 to 1)",
            ),
            (
                "[render.shadows]\nblend = 0.6",
                "[render.shadows] blend = 0.6 is out of range (0 to 0.5)",
            ),
        ] {
            let err =
                AppConfig::parse(&format!("[project]\nname = \"p\"\n\n{table}\n")).unwrap_err();
            assert_eq!(err.code, codes::CONFIG_INVALID);
            assert!(err.message.starts_with(want), "{table}: {}", err.message);
        }
    }

    #[test]
    fn the_explain_page_example_parses() {
        let page = include_str!("../explain/RE0501.md");
        let example = page
            .split("```toml\n")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .expect("a toml example");
        let c = AppConfig::parse(&format!("[project]\nname = \"p\"\n\n{example}")).unwrap();
        assert_eq!(
            c.render.post.tone_mapping,
            re_types::render::ToneMapping::PbrNeutral
        );
    }
}
