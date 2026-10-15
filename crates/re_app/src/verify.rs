//! `real verify` (ADR-0022): the JSON gate for a project.
//!
//! [`run`] runs the stages in order and returns a [`VerifyReport`]
//! (`{ ok, stages: [{ name, ok, diagnostics, duration_ms, data }], perf }`):
//!
//! 1. `schema`: every content file parses and loads (scenes, prefabs, graphs,
//!    replays);
//! 2. `compile`: every script compiles (tier 1);
//! 3. `scan`: the static API scan (tier 2), passed in by `re_cli` as a
//!    [`VerifyStage`] when it is built with `tooling`;
//! 4. `boot`: the project opens headless and runs its warm-up frames; a
//!    `[compat]` project runs [`VerifyOptions::compat_boot_frames`] (600) and
//!    fails on an uncaught script error or a thread still waiting in
//!    `WaitForChild` at the end (`RE0526`), and its data adds `uncaught`,
//!    `stuck_yields`, `unsupported` and `output`;
//! 5. `replays`: each `tests/replays/*.replay.json` runs twice from a fresh
//!    start, and both runs must end with the same `state_hash`;
//! 6. `goldens`: the replays' screenshot checks against
//!    `tests/goldens/<backend>/`;
//! 7. `perf`: the PerfReport of the boot run, checked against
//!    `real.toml [verify.perf]` (warnings only in M1).
//!
//! A stage this build cannot run is reported with `ok: true` and an `RE0512`
//! note saying why. Stages after a failed `boot` are skipped.

use std::path::{Path, PathBuf};
use std::time::Instant;

use re_core::{Diagnostic, Location, Severity};
use re_scene::{PrefabFile, World};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::app::App;
use crate::codes;
use crate::config::AppConfig;
use crate::perf::PerfReport;

/// The result of one stage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StageReport {
    /// The stage name (`schema`, `compile`, `scan`, `boot`, `replays`, `goldens`, `perf`).
    pub name: String,
    /// False when the stage found an error.
    pub ok: bool,
    /// Errors, warnings and notes.
    #[schemars(with = "Vec<Json>")]
    pub diagnostics: Vec<Diagnostic>,
    /// How long the stage took.
    pub duration_ms: f64,
    /// Stage-specific results.
    #[serde(default)]
    pub data: Json,
}

impl StageReport {
    /// A stage that ran: `ok` unless a diagnostic is an error.
    pub fn new(name: &str, diagnostics: Vec<Diagnostic>, data: Json) -> StageReport {
        StageReport {
            name: name.to_string(),
            ok: !diagnostics.iter().any(Diagnostic::is_error),
            diagnostics,
            duration_ms: 0.0,
            data,
        }
    }

    /// A stage that did not run, and why (an `RE0512` note; `ok` stays true).
    pub fn skipped(name: &str, why: impl Into<String>) -> StageReport {
        let why = why.into();
        StageReport {
            name: name.to_string(),
            ok: true,
            diagnostics: vec![Diagnostic::note(
                codes::STAGE_SKIPPED,
                format!("stage `{name}` skipped: {why}"),
            )],
            duration_ms: 0.0,
            data: json!({ "skipped": why }),
        }
    }
}

/// The whole report (`schemas/verify-report.schema.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VerifyReport {
    /// True when every stage is ok.
    pub ok: bool,
    /// The stages, in the order they ran.
    pub stages: Vec<StageReport>,
    /// The PerfReport of the boot run (absent when the project did not boot).
    pub perf: Option<PerfReport>,
}

impl VerifyReport {
    /// The stage named `name`.
    pub fn stage(&self, name: &str) -> Option<&StageReport> {
        self.stages.iter().find(|s| s.name == name)
    }

    /// The process exit code of ADR-0022: 0 without errors, 1 with errors.
    pub fn exit_code(&self) -> u8 {
        u8::from(!self.ok)
    }
}

/// What a stage gets to work with.
pub struct VerifyContext<'a> {
    /// The project directory.
    pub project: &'a Path,
    /// Its configuration.
    pub config: &'a AppConfig,
}

/// A stage supplied by the caller. `re_cli` passes the static scan (stage 3,
/// named `scan`) this way when it is built with `tooling`, so `re_app` never
/// links full_moon. Extra stages run after `compile`, in the order given.
pub trait VerifyStage {
    /// The stage's name in the report.
    fn name(&self) -> &str;
    /// Runs the stage.
    fn run(&self, ctx: &VerifyContext<'_>) -> StageReport;
}

/// Options for [`run`].
#[derive(Clone, Debug)]
pub struct VerifyOptions {
    /// Frames the boot stage runs (and measures for the PerfReport).
    pub boot_frames: u64,
    /// Frames the boot stage runs for a `[compat]` project instead of
    /// `boot_frames`.
    pub compat_boot_frames: u64,
    /// Frames at the start of the boot run that are not measured.
    pub warmup_frames: u64,
    /// Write missing or differing golden images from this run instead of
    /// failing (`real verify --update-goldens`); the stage reports what it wrote.
    pub update_goldens: bool,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        VerifyOptions {
            boot_frames: 120,
            compat_boot_frames: 600,
            warmup_frames: 10,
            update_goldens: false,
        }
    }
}

/// Every file under `dir` (recursively) whose name ends with `suffix`, sorted,
/// as paths relative to `root` with `/` separators.
pub fn content_files(root: &Path, dir: &str, suffix: &str) -> Vec<String> {
    fn walk(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, suffix, out);
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(suffix))
            {
                out.push(p);
            }
        }
    }
    let mut found = Vec::new();
    walk(&root.join(dir), suffix, &mut found);
    let mut rel: Vec<String> = found
        .iter()
        .filter_map(|p| p.strip_prefix(root).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    rel.sort();
    rel
}

fn invalid(file: &str, msg: impl Into<String>, cause: Option<Diagnostic>) -> Diagnostic {
    let mut d = Diagnostic::error(codes::CONTENT_INVALID, msg).at(Location::file(file));
    if let Some(c) = cause {
        d = d.with_related(c);
    }
    d
}

fn read(project: &Path, file: &str) -> Result<String, Diagnostic> {
    std::fs::read_to_string(project.join(file))
        .map_err(|e| invalid(file, format!("cannot read {file}: {e}"), None))
}

/// Stage 1: every content file parses and loads.
/// Every `scripts/**/*.luau` compiles (validation tier 1).
fn stage_compile(project: &Path) -> StageReport {
    let scripts = content_files(project, "scripts", ".luau");
    let mut diags = Vec::new();
    for file in &scripts {
        match std::fs::read_to_string(project.join(file)) {
            Ok(source) => diags.extend(re_script::compile_check(&source, file)),
            Err(e) => diags.push(
                Diagnostic::error(
                    re_script::codes::SCRIPT_READ,
                    format!("cannot read {file}: {e}"),
                )
                .at(Location::file(file)),
            ),
        }
    }
    StageReport::new("compile", diags, json!({ "scripts": scripts.len() }))
}

fn stage_schema(project: &Path, config: &AppConfig) -> StageReport {
    let mut diags = Vec::new();
    let mut counts = serde_json::Map::new();

    let scenes = content_files(project, "scenes", ".scene.json");
    for file in &scenes {
        let checked = read(project, file).and_then(|text| {
            let doc = re_scene::scene::from_json(&text).map_err(|e| {
                invalid(
                    file,
                    format!("{file} is not a valid scene"),
                    Some(e.to_diagnostic()),
                )
            })?;
            World::new().load(&doc).map(|_| ()).map_err(|e| {
                invalid(
                    file,
                    format!("{file} does not load"),
                    Some(e.to_diagnostic()),
                )
            })
        });
        diags.extend(checked.err());
    }
    // Scenes outside `scenes/` are allowed; boot loads (and so checks) the entry.
    if !project.join(&config.project.entry_scene).is_file() {
        diags.push(invalid(
            &config.project.entry_scene,
            format!(
                "the entry scene {} named in real.toml is missing",
                config.project.entry_scene
            ),
            None,
        ));
    }
    counts.insert("scenes".into(), scenes.len().into());

    let prefabs = content_files(project, "prefabs", ".prefab.json");
    for file in &prefabs {
        let checked = read(project, file).and_then(|text| {
            let p = PrefabFile::from_json(&text).map_err(|e| {
                invalid(
                    file,
                    format!("{file} is not a valid prefab"),
                    Some(e.to_diagnostic()),
                )
            })?;
            let mut w = World::new();
            w.declare_script_components(&p.script_components)
                .and_then(|()| p.instantiate(&w.registry, "verify"))
                .map(|_| ())
                .map_err(|e| {
                    invalid(
                        file,
                        format!("{file} does not instantiate"),
                        Some(e.to_diagnostic()),
                    )
                })
        });
        diags.extend(checked.err());
    }
    counts.insert("prefabs".into(), prefabs.len().into());

    let graphs = content_files(project, "graphs", ".graph.json");
    for file in &graphs {
        let checked = read(project, file).and_then(|text| {
            serde_json::from_str::<re_types::GraphDoc>(&text)
                .map(|_| ())
                .map_err(|e| invalid(file, format!("{file} is not a valid graph: {e}"), None))
        });
        diags.extend(checked.err());
    }
    counts.insert("graphs".into(), graphs.len().into());

    let replays = content_files(project, "tests/replays", ".replay.json");
    let layout = config.input.to_action_map().map(|m| m.layout());
    for file in &replays {
        let checked = read(project, file).and_then(|text| {
            let doc: re_types::ReplayDoc = serde_json::from_str(&text)
                .map_err(|e| invalid(file, format!("{file} is not a valid replay: {e}"), None))?;
            if let Ok(layout) = &layout {
                re_input::Replay::new(doc, layout)
                    .map(|_| ())
                    .map_err(|ds| {
                        let mut d = invalid(
                            file,
                            format!("{file} does not match the project's actions"),
                            None,
                        );
                        for r in ds {
                            d = d.with_related(r);
                        }
                        d
                    })?;
            }
            Ok(())
        });
        diags.extend(checked.err());
    }
    counts.insert("replays".into(), replays.len().into());

    StageReport::new("schema", diags, Json::Object(counts))
}

fn timed(f: impl FnOnce() -> StageReport) -> StageReport {
    let t = Instant::now();
    let mut r = f();
    r.duration_ms = t.elapsed().as_secs_f64() * 1000.0;
    r
}

fn headless_config(config: &AppConfig) -> AppConfig {
    AppConfig {
        headless: true,
        persist_edits: false,
        ..config.clone()
    }
}

/// One replay run, from a fresh start.
fn replay_once(
    project: &Path,
    config: &AppConfig,
    doc: &re_types::ReplayDoc,
) -> Result<(crate::app::RunReport, RegenWatch), Diagnostic> {
    let mut app = App::open(project, headless_config(config))?;
    let mut watch = RegenWatch::default();
    let report =
        app.run_headless_observed(None, Some(doc.clone()), |app| watch.after_frame(app))?;
    Ok((report, watch))
}

/// Frames a regenerated character has to find its feet again.
const SETTLE_FRAMES: u64 = 60;

/// One regeneration seen during a replay, and what it kept.
#[derive(Debug, Serialize)]
struct Regeneration {
    /// The frame the new terrain appeared.
    frame: u64,
    /// Every character kept its `Entity`.
    characters_kept: bool,
    /// Every character stood on the ground again within `SETTLE_FRAMES`.
    grounded: bool,
    /// Items removed by a game script (collected) that stayed removed.
    collected_kept: usize,
    /// Errors reported after it (a script's stale handle shows up as one).
    script_errors: usize,
    #[serde(skip)]
    errors_before: usize,
    #[serde(skip)]
    waiting: std::collections::BTreeSet<String>,
}

/// Watches a replay for regenerations of the terrain (the set of derived
/// heightfields colliders use changes) and checks the demo's promises: the
/// characters keep their `Entity`, stand on the new ground, collected items
/// stay collected and scripts keep working.
#[derive(Default)]
struct RegenWatch {
    ground: Option<std::collections::BTreeSet<String>>,
    characters: std::collections::BTreeMap<String, re_scene::Entity>,
    restarts: u64,
    seen: Vec<Regeneration>,
    problems: Vec<Diagnostic>,
}

impl RegenWatch {
    fn characters(world: &mut World) -> std::collections::BTreeMap<String, re_scene::Entity> {
        let found: Vec<re_scene::Entity> = world
            .ecs
            .query_filtered::<re_scene::Entity, re_scene::ecs::query::With<re_scene::CharacterController>>()
            .iter(&world.ecs)
            .collect();
        found
            .into_iter()
            .filter_map(|e| world.id_of(e).map(|id| (id.to_string(), e)))
            .collect()
    }

    fn broke(&mut self, frame: u64, message: String) {
        self.problems.push(Diagnostic::error(
            codes::REGENERATION_BROKE,
            format!("frame {frame}: {message}"),
        ));
    }

    fn after_frame(&mut self, app: &mut App) {
        // The frame that just ran (the counter has moved past it).
        let frame = app.frame_count().saturating_sub(1);
        if app.restarts() != self.restarts {
            // A fresh World: its characters start over and are not the ones
            // a regeneration was waiting on.
            for r in &mut self.seen {
                r.waiting.clear();
            }
        }
        let errors = app
            .pending_diagnostics()
            .iter()
            .filter(|d| d.is_error())
            .count();
        for r in &mut self.seen {
            r.script_errors = errors.saturating_sub(r.errors_before);
        }
        // Characters waiting to land.
        for i in 0..self.seen.len() {
            let waiting = std::mem::take(&mut self.seen[i].waiting);
            let mut still = std::collections::BTreeSet::new();
            for id in waiting {
                let landed = app
                    .world()
                    .entity(&id)
                    .ok()
                    .and_then(|e| app.physics().character(re_scene::entity_id(e)));
                if !landed.is_some_and(|c| c.ground == re_types::GroundState::OnGround) {
                    still.insert(id);
                }
            }
            let start = self.seen[i].frame;
            if !still.is_empty() && frame >= start + SETTLE_FRAMES {
                self.seen[i].grounded = false;
                let ids = still.into_iter().collect::<Vec<_>>().join(", ");
                self.broke(
                    frame,
                    format!(
                        "after the regeneration at frame {start}, {ids} did not stand on the ground"
                    ),
                );
            } else {
                self.seen[i].waiting = still;
            }
        }
        let ground = crate::procedural::ground(app.world_mut());
        let restarted = app.restarts() != self.restarts;
        let world = app.world_mut();
        if !restarted
            && let Some(old) = &self.ground
            && *old != ground
            && !ground.is_empty()
        {
            let now = Self::characters(world);
            let mut kept = true;
            let lost: Vec<String> = self
                .characters
                .iter()
                // A character a script removed is gone, not lost.
                .filter(|(id, entity)| now.get(*id).is_some_and(|e| e != *entity))
                .map(|(id, _)| id.clone())
                .collect();
            for id in &lost {
                kept = false;
                self.broke(
                    frame,
                    format!(
                        "the character {id:?} lost its Entity when the terrain was regenerated"
                    ),
                );
            }
            let collected: Vec<String> = world.suppressions().iter().map(str::to_string).collect();
            let back: Vec<&String> = collected
                .iter()
                .filter(|id| world.entity(id).is_ok())
                .collect();
            if !back.is_empty() {
                let list = back
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.broke(
                    frame,
                    format!("collected items came back with the new terrain: {list}"),
                );
            }
            self.seen.push(Regeneration {
                frame,
                characters_kept: kept,
                grounded: true,
                collected_kept: collected.len() - back.len(),
                script_errors: 0,
                errors_before: errors,
                waiting: self.characters.keys().cloned().collect(),
            });
        }
        self.restarts = app.restarts();
        self.characters = Self::characters(app.world_mut());
        self.ground = Some(ground);
    }

    /// At the end of the run: characters still in the air fail.
    fn finish(mut self, frame: u64) -> (Vec<Regeneration>, Vec<Diagnostic>) {
        for r in &mut self.seen {
            if !r.waiting.is_empty() {
                r.grounded = false;
                let ids = std::mem::take(&mut r.waiting)
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ");
                self.problems.push(Diagnostic::error(
                    codes::REGENERATION_BROKE,
                    format!(
                        "frame {frame}: the replay ended before {ids} stood on the ground after the regeneration at frame {}",
                        r.frame
                    ),
                ));
            }
        }
        (self.seen, self.problems)
    }
}

/// Stage 5: each replay twice, with matching state hashes.
fn stage_replays(project: &Path, config: &AppConfig) -> StageReport {
    let files = content_files(project, "tests/replays", ".replay.json");
    if files.is_empty() {
        return StageReport::skipped("replays", "the project has no tests/replays/*.replay.json");
    }
    let mut diags = Vec::new();
    let mut runs = Vec::new();
    for file in &files {
        let doc = match read_replay(project, file) {
            Ok(d) => d,
            Err(d) => {
                diags.push(d);
                continue;
            }
        };
        let first = replay_once(project, config, &doc);
        let second = replay_once(project, config, &doc).map(|(b, _)| b);
        match (first, second) {
            (Ok((a, watch)), Ok(b)) => {
                let (regenerations, problems) = watch.finish(a.frames);
                for d in problems {
                    diags.push(d.at(Location::file(file.clone())));
                }
                for d in a
                    .diagnostics
                    .iter()
                    .filter(|d| d.severity != Severity::Note)
                {
                    diags.push(d.clone().with_related(Diagnostic::note(
                        codes::REPLAY_FAILED,
                        format!("reported while replaying {file}"),
                    )));
                }
                let same = a.state_hash == b.state_hash;
                if !same {
                    diags.push(
                        Diagnostic::error(
                            codes::NONDETERMINISTIC,
                            format!(
                                "{file}: two runs ended with different state hashes ({} and {})",
                                a.state_hash, b.state_hash
                            ),
                        )
                        .at(Location::file(file.clone())),
                    );
                }
                runs.push(json!({
                    "file": file,
                    "frames": a.frames,
                    "state_hash": a.state_hash,
                    "deterministic": same,
                    "screenshots": a.screenshots.len(),
                    "regenerations": regenerations,
                }));
            }
            (Err(d), _) | (_, Err(d)) => diags.push(d),
        }
    }
    StageReport::new("replays", diags, json!({ "runs": runs }))
}

fn read_replay(project: &Path, file: &str) -> Result<re_types::ReplayDoc, Diagnostic> {
    read(project, file).and_then(|text| {
        serde_json::from_str::<re_types::ReplayDoc>(&text)
            .map_err(|e| invalid(file, format!("{file} is not a valid replay: {e}"), None))
    })
}

/// Stage 6: every replay screenshot check against its golden image, in the
/// golden set of the backend this run draws with (`{backend}` in the path).
fn stage_goldens(project: &Path, config: &AppConfig, options: &VerifyOptions) -> StageReport {
    let mut checked = Vec::new();
    for file in content_files(project, "tests/replays", ".replay.json") {
        // Unreadable replays are already reported by the schema stage.
        if let Ok(doc) = read_replay(project, &file)
            && doc.checks.iter().any(|c| c.screenshot.is_some())
        {
            checked.push((file, doc));
        }
    }
    if checked.is_empty() {
        return StageReport::skipped("goldens", "no replay has a screenshot check");
    }
    let tolerance = re_render::imgdiff::Tolerance {
        max_channel_delta: config
            .verify
            .golden_mean_tolerance
            .clamp(0.0, 255.0)
            .round() as u8,
        outlier_fraction: f64::from(config.verify.golden_max_outliers.clamp(0.0, 1.0)),
    };
    let mut diags = Vec::new();
    let mut results = Vec::new();
    let mut backend = "none";
    for (file, doc) in checked {
        let mut app = match App::open(project, headless_config(config)) {
            Ok(a) => a,
            Err(d) => {
                diags.push(d);
                continue;
            }
        };
        if let Err(d) = app.ensure_graphics() {
            diags.push(d);
            break;
        }
        backend = app.graphics().map_or("none", |g| g.backend());
        let wanted: Vec<(u64, String)> = doc
            .checks
            .iter()
            .filter_map(|c| c.screenshot.clone().map(|s| (c.frame, s)))
            .collect();
        match app.run_headless(None, Some(doc)) {
            Ok(run) => diags.extend(run.diagnostics.into_iter().filter(Diagnostic::is_error)),
            Err(d) => {
                diags.push(d);
                continue;
            }
        }
        let captures = app.take_captures();
        for (frame, golden) in &wanted {
            let taken = captures
                .iter()
                .any(|(r, _)| r.frame == *frame && r.golden.as_ref() == Some(golden));
            if !taken {
                diags.push(
                    Diagnostic::error(
                        codes::SCREENSHOT_FAILED,
                        format!("{file}: the screenshot check at frame {frame} was not taken"),
                    )
                    .at(Location::file(file.clone()))
                    .with_hint(
                        "a check's frame must be below the replay's `frames` (frames count from 0)",
                    ),
                );
            }
        }
        for (request, image) in captures {
            let Some(golden) = request.golden else {
                continue;
            };
            let golden = match crate::commands::project_path(
                &golden.replace("{backend}", backend),
                "the golden image",
                codes::CONTENT_INVALID,
            ) {
                Ok(g) => g,
                Err(d) => {
                    diags.push(d.at(Location::file(file.clone())));
                    continue;
                }
            };
            let out = project.join(".real/out/goldens").join(&golden);
            let path = project.join(&golden);
            let at = Location::file(golden.clone());
            let mut result = json!({ "replay": file, "frame": request.frame, "golden": golden });
            let expected = path.is_file().then(|| re_render::Image::load_png(&path));
            match expected {
                None | Some(Err(_)) if options.update_goldens => {
                    match crate::gfx::save_png(&image, &path) {
                        Ok(()) => result["written"] = json!(true),
                        Err(d) => diags.push(d),
                    }
                }
                None => {
                    let _ = crate::gfx::save_png(&image, &out);
                    diags.push(
                        Diagnostic::error(
                            codes::GOLDEN_MISSING,
                            format!("{file}: there is no golden image {golden} for frame {}", request.frame),
                        )
                        .at(at)
                        .with_hint(format!(
                            "this run's image is in .real/out/goldens/{golden}; review it and copy it there, or run `real verify --update-goldens`"
                        )),
                    );
                    result["missing"] = json!(true);
                }
                Some(Err(e)) => diags.push(invalid(
                    &golden,
                    format!("{golden} cannot be read"),
                    Some(e),
                )),
                Some(Ok(expected)) => {
                    match re_render::imgdiff::compare(&expected, &image, tolerance) {
                        Ok(diff) => {
                            result["summary"] = json!(diff.summary());
                            result["matches"] = json!(diff.matches());
                            if !diff.matches() {
                                if options.update_goldens {
                                    match crate::gfx::save_png(&image, &path) {
                                        Ok(()) => result["written"] = json!(true),
                                        Err(d) => diags.push(d),
                                    }
                                    results.push(result);
                                    continue;
                                }
                                let _ = crate::gfx::save_png(&image, &out);
                                if let Ok(d) =
                                    re_render::imgdiff::diff_image(&expected, &image, tolerance)
                                {
                                    let _ =
                                        crate::gfx::save_png(&d, &out.with_extension("diff.png"));
                                }
                                diags.push(
                                    Diagnostic::error(
                                        codes::GOLDEN_MISMATCH,
                                        format!("{file}: frame {} differs from {golden}: {}", request.frame, diff.summary()),
                                    )
                                    .at(at)
                                    .with_hint("this run's image and a diff picture are in .real/out/goldens/; if the change is intended, run `real verify --update-goldens`".to_string()),
                                );
                            }
                        }
                        Err(d) => diags.push(
                            Diagnostic::error(
                                codes::GOLDEN_MISMATCH,
                                format!(
                                    "{file}: frame {} cannot be compared with {golden}",
                                    request.frame
                                ),
                            )
                            .at(at)
                            .with_related(d),
                        ),
                    }
                }
            }
            results.push(result);
        }
        diags.extend(
            app.take_diagnostics()
                .into_iter()
                .filter(Diagnostic::is_error),
        );
    }
    StageReport::new(
        "goldens",
        diags,
        json!({ "backend": backend, "checks": results }),
    )
}

/// Stage 4: the project opens headless and runs its boot frames; the
/// measured part of the run (after the warm-up) is the PerfReport.
fn stage_boot(
    project: &Path,
    config: &AppConfig,
    options: &VerifyOptions,
    perf: &mut Option<PerfReport>,
) -> StageReport {
    let mut app = match App::open(project, headless_config(config)) {
        Ok(app) => app,
        Err(d) => {
            return StageReport::new(
                "boot",
                vec![
                    Diagnostic::error(codes::BOOT_FAILED, "the project did not boot")
                        .with_related(d),
                ],
                Json::Null,
            );
        }
    };
    let compat = config.compat.is_some();
    let frames = if compat {
        options.compat_boot_frames
    } else {
        options.boot_frames
    }
    .max(1);
    let warm = options.warmup_frames.min(frames - 1);
    let mut diags = Vec::new();
    let mut output = Vec::new();
    match app.run_headless(Some(warm), None) {
        Ok(r) => {
            diags.extend(r.diagnostics);
            output.extend(r.output);
        }
        Err(d) => diags.push(d),
    }
    app.reset_perf();
    let r = match app.run_headless(Some(frames - warm), None) {
        Ok(r) => r,
        Err(d) => {
            diags.push(d);
            return StageReport::new("boot", diags, Json::Null);
        }
    };
    diags.extend(r.diagnostics);
    output.extend(r.output);
    let mut data = json!({
        "frames": frames,
        "entities": r.perf.counts.entities,
        "bodies": r.perf.counts.bodies,
        "characters": r.perf.counts.characters,
        "state_hash": r.state_hash,
    });
    if let Some(info) = &r.compat {
        diags.extend(compat_boot_checks(info, frames));
        data["uncaught"] = json!(
            info.uncaught
                .iter()
                .map(|(context, d)| json!({
                    "context": context,
                    "code": d.code.to_string(),
                    "message": d.message,
                }))
                .collect::<Vec<_>>()
        );
        data["stuck_yields"] = json!(
            info.stuck_yields
                .iter()
                .map(|(context, wait, thread)| json!({
                    "context": context,
                    "wait": wait,
                    "thread": thread,
                }))
                .collect::<Vec<_>>()
        );
        data["unsupported"] = json!(
            info.unsupported
                .iter()
                .map(|(member, count)| json!({ "member": member, "count": count }))
                .collect::<Vec<_>>()
        );
        data["output"] = json!(output);
    }
    *perf = Some(r.perf);
    StageReport::new("boot", diags, data)
}

/// The boot failure only a compat run has: threads still waiting in
/// `WaitForChild`. (Uncaught script errors are already among the run's
/// diagnostics, as errors.)
fn compat_boot_checks(info: &crate::compat::CompatRunInfo, frames: u64) -> Vec<Diagnostic> {
    info.stuck_yields
        .iter()
        .map(|(context, wait, thread)| {
            Diagnostic::error(
                codes::STUCK_YIELD,
                format!("a {context} script still waits in {wait} after {frames} frames"),
            )
            .with_hint(format!("waiting: {thread}"))
        })
        .collect()
}

/// Stage 7: the perf budget.
fn stage_perf(perf: &PerfReport, config: &AppConfig) -> StageReport {
    let mut diags = Vec::new();
    if let Some(budget) = config.verify.perf.frame_ms_p95
        && perf.frame.p95_ms > budget
    {
        diags.push(
            Diagnostic::warning(
                codes::PERF_BUDGET,
                format!(
                    "frame time p95 is {:.2} ms, over the {budget:.2} ms budget",
                    perf.frame.p95_ms
                ),
            )
            .at(Location::file(crate::config::CONFIG_FILE))
            .with_hint("budgets are warnings in M1, because CI renders in software"),
        );
    }
    StageReport::new(
        "perf",
        diags,
        json!({ "frame_ms_p95": perf.frame.p95_ms, "budget_ms_p95": config.verify.perf.frame_ms_p95 }),
    )
}

/// Runs every stage of ADR-0022 against the project `app` was opened from.
///
/// The app itself is not changed: boot and replay runs open fresh copies of
/// the project from disk, so `verify` checks what is saved.
pub fn run(app: &App, options: &VerifyOptions, extra: &[&dyn VerifyStage]) -> VerifyReport {
    run_project(app.project(), app.config(), options, extra)
}

/// [`run`] for a project that is not open.
pub fn run_project(
    project: &Path,
    config: &AppConfig,
    options: &VerifyOptions,
    extra: &[&dyn VerifyStage],
) -> VerifyReport {
    let ctx = VerifyContext { project, config };
    let mut stages = Vec::new();
    stages.push(timed(|| stage_schema(project, config)));
    stages.push(timed(|| stage_compile(project)));
    for stage in extra {
        stages.push(timed(|| stage.run(&ctx)));
    }
    if !extra.iter().any(|s| s.name() == "scan") {
        stages.push(StageReport::skipped(
            "scan",
            "the static API scan needs a `real` built with the `tooling` feature",
        ));
    }

    let mut perf = None;
    let boot = timed(|| stage_boot(project, config, options, &mut perf));
    let booted = boot.ok;
    stages.push(boot);

    if booted {
        stages.push(timed(|| stage_replays(project, config)));
        stages.push(timed(|| stage_goldens(project, config, options)));
    } else {
        stages.push(StageReport::skipped("replays", "the project did not boot"));
        stages.push(StageReport::skipped("goldens", "the project did not boot"));
    }
    match &perf {
        Some(p) => stages.push(stage_perf(p, config)),
        None => stages.push(StageReport::skipped("perf", "the project did not boot")),
    }

    VerifyReport {
        ok: stages.iter().all(|s| s.ok),
        stages,
        perf,
    }
}
