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
//! 4. `boot`: the project opens headless and runs its warm-up frames;
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
    /// Frames at the start of the boot run that are not measured.
    pub warmup_frames: u64,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        VerifyOptions {
            boot_frames: 120,
            warmup_frames: 10,
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
    if !scenes.contains(&config.project.entry_scene) {
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
) -> Result<crate::app::RunReport, Diagnostic> {
    let mut app = App::open(project, headless_config(config))?;
    app.run_headless(None, Some(doc.clone()))
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
        let doc = match read(project, file).and_then(|text| {
            serde_json::from_str::<re_types::ReplayDoc>(&text)
                .map_err(|e| invalid(file, format!("{file} is not a valid replay: {e}"), None))
        }) {
            Ok(d) => d,
            Err(d) => {
                diags.push(d);
                continue;
            }
        };
        let first = replay_once(project, config, &doc);
        let second = replay_once(project, config, &doc);
        match (first, second) {
            (Ok(a), Ok(b)) => {
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
                }));
            }
            (Err(d), _) | (_, Err(d)) => diags.push(d),
        }
    }
    StageReport::new("replays", diags, json!({ "runs": runs }))
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
    stages.push(StageReport::skipped(
        "compile",
        "script compilation joins verify when scripting is integrated into the runtime",
    ));
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
    let boot = timed(|| match App::open(project, headless_config(config)) {
        Err(d) => StageReport::new(
            "boot",
            vec![Diagnostic::error(codes::BOOT_FAILED, "the project did not boot").with_related(d)],
            Json::Null,
        ),
        Ok(mut app) => {
            let warm = options.warmup_frames.min(options.boot_frames);
            let mut diags = Vec::new();
            match app.run_headless(Some(warm), None) {
                Ok(r) => diags.extend(r.diagnostics),
                Err(d) => diags.push(d),
            }
            app.reset_perf();
            match app.run_headless(Some(options.boot_frames - warm), None) {
                Ok(r) => {
                    diags.extend(r.diagnostics);
                    let data = json!({
                        "frames": options.boot_frames,
                        "entities": r.perf.counts.entities,
                        "bodies": r.perf.counts.bodies,
                        "characters": r.perf.counts.characters,
                        "state_hash": r.state_hash,
                    });
                    perf = Some(r.perf);
                    StageReport::new("boot", diags, data)
                }
                Err(d) => {
                    diags.push(d);
                    StageReport::new("boot", diags, Json::Null)
                }
            }
        }
    });
    let booted = boot.ok;
    stages.push(boot);

    if booted {
        stages.push(timed(|| stage_replays(project, config)));
        stages.push(StageReport::skipped(
            "goldens",
            "golden screenshots need the renderer, which joins verify when it is integrated into the runtime",
        ));
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
