//! The runtime commands `re_app` registers (ARCHITECTURE §6): `verify`,
//! `screenshot`, `play.run` and `asset.import`, and a `describe` that adds the
//! procedural node kinds and the project's scripts to the built-in one.

use std::path::{Path, PathBuf};
use std::time::Instant;

use re_command::{CommandRegistry, NoArgs, Tier};
use re_core::{Diagnostic, Location};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::app::App;
use crate::codes;
use crate::verify::{self, VerifyOptions};

/// The most frames one `play.run` may play (a minute at 60 Hz).
pub const PLAY_MAX_FRAMES: u64 = 3600;

/// Arguments of `verify`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyArgs {
    /// Frames the boot stage runs (default 120; 600 in a `[compat]` project).
    #[serde(default)]
    pub boot_frames: Option<u64>,
}

/// Arguments of `screenshot`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotArgs {
    /// Where to write the PNG, relative to the project (default
    /// `.real/out/screenshot-<frame>.png`). Must stay inside the project.
    #[serde(default)]
    pub path: Option<String>,
}

/// Arguments of `play.run`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlayArgs {
    /// Frames to play (1 to 3600; default 120, two seconds).
    #[serde(default = "default_play_frames")]
    pub frames: u64,
    /// A replay file to drive input with (`tests/replays/x.replay.json`).
    #[serde(default)]
    pub replay: Option<String>,
    /// Capture the last frame (default true); written under `.real/out/play/`.
    #[serde(default = "yes")]
    pub screenshot: bool,
    /// Steps to run in order after the first `frames`: `{"frames": n}`,
    /// `"add_player"`, `{"say": {...}}`, `{"invoke": {...}}` and
    /// `{"dump_tree": {...}}` (all but `frames` and `dump_tree` need a
    /// `[compat]` project). Their frames count toward the 3600.
    #[serde(default)]
    pub steps: Vec<crate::play::PlayStep>,
}

fn default_play_frames() -> u64 {
    120
}

fn yes() -> bool {
    true
}

/// Arguments of `asset.import`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportArgs {
    /// The file to import: an absolute path, or relative to the project.
    pub source: String,
    /// Where it goes, relative to the project and under `assets/` (default
    /// `assets/<file name>`).
    #[serde(default)]
    pub dest: Option<String>,
    /// Replace an existing file (its sidecar, and so its asset id, is kept).
    #[serde(default)]
    pub overwrite: bool,
}

/// A project-relative path that must stay inside the project (no `..`, not
/// absolute), with `/` separators.
pub(crate) fn project_path(
    path: &str,
    what: &str,
    code: re_core::Code,
) -> Result<String, Diagnostic> {
    let clean = path.replace('\\', "/");
    let p = Path::new(&clean);
    if clean.is_empty()
        || p.is_absolute()
        || p.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(Diagnostic::error(
            code,
            format!("{what} {path:?} must be a path inside the project"),
        ));
    }
    Ok(clean.trim_start_matches("./").to_string())
}

fn play(app: &mut App, args: PlayArgs) -> Result<serde_json::Value, Diagnostic> {
    let frames = args.frames.clamp(1, PLAY_MAX_FRAMES);
    let replay = match &args.replay {
        Some(path) => {
            let rel = project_path(path, "the replay", codes::REPLAY_FAILED)?;
            let text = std::fs::read_to_string(app.project().join(&rel)).map_err(|e| {
                Diagnostic::error(codes::REPLAY_FAILED, format!("cannot read {rel}: {e}"))
                    .at(Location::file(&rel))
            })?;
            Some(serde_json::from_str(&text).map_err(|e| {
                Diagnostic::error(codes::REPLAY_FAILED, format!("{rel} is not a replay: {e}"))
                    .at(Location::file_at(&rel, e.line() as u32, e.column() as u32))
            })?)
        }
        None => None,
    };
    let started = Instant::now();
    let step_frames: u64 = args.steps.iter().map(crate::play::PlayStep::frames).sum();
    if frames.saturating_add(step_frames) > PLAY_MAX_FRAMES {
        return Err(Diagnostic::error(
            codes::PLAY_STEP_FAILED,
            format!(
                "play.run plays at most {PLAY_MAX_FRAMES} frames; frames and steps ask for {}",
                frames.saturating_add(step_frames)
            ),
        ));
    }
    let (report, steps, image) = app.play_copy(frames, replay, &args.steps, args.screenshot)?;
    let screenshot = match image {
        Some(image) => {
            let rel = format!(".real/out/play/play-{}.png", app.frame_count());
            crate::gfx::save_png(&image, &app.project().join(&rel))?;
            Some(rel)
        }
        None => None,
    };
    Ok(json!({
        "ok": report.ok(),
        "frames": report.frames,
        "state_hash": report.state_hash,
        "diagnostics": report.diagnostics,
        "perf": report.perf,
        "output": report.output,
        "compat": report.compat,
        "steps": steps,
        "screenshot": screenshot,
        "wall_ms": started.elapsed().as_secs_f64() * 1000.0,
        "notes": [
            "played on a copy of the world; nothing reached the live world",
            "contacts started fresh, so the copy's first step reports Begin for touching bodies",
            "scripts start from their entry points in fresh VMs (in-VM state is not copied)",
        ],
    }))
}

fn import(app: &mut App, args: ImportArgs, dry_run: bool) -> Result<serde_json::Value, Diagnostic> {
    let fail = |msg: String| Diagnostic::error(codes::ASSET_IMPORT_FAILED, msg);
    let source = PathBuf::from(&args.source);
    let source = if source.is_absolute() {
        source
    } else {
        app.project().join(source)
    };
    if !source.is_file() {
        return Err(fail(format!("cannot import {}: no such file", args.source)));
    }
    let file_name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dest = match &args.dest {
        Some(d) => project_path(d, "the destination", codes::ASSET_IMPORT_FAILED)?,
        None => format!("assets/{file_name}"),
    };
    if !dest.starts_with("assets/") {
        return Err(fail(format!("the destination {dest} is not under assets/")));
    }
    let lower = dest.to_ascii_lowercase();
    if ![".gltf", ".glb", ".png", ".jpg", ".jpeg", ".material.json"]
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        return Err(fail(format!("no importer for {dest}"))
            .with_hint("supported files are .gltf, .glb, .png, .jpg, .jpeg and .material.json"));
    }
    let target = app.project().join(&dest);
    if target.exists() && !args.overwrite {
        return Err(fail(format!("{dest} already exists"))
            .at(Location::file(&dest))
            .with_hint("pass \"overwrite\": true to replace it (its asset id is kept)"));
    }
    if dry_run {
        return Ok(json!({ "path": dest, "dry_run": true }));
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| fail(format!("cannot create {}: {e}", dir.display())))?;
    }
    if source != target {
        std::fs::copy(&source, &target)
            .map_err(|e| fail(format!("cannot copy {} to {dest}: {e}", args.source)))?;
    }
    let meta = re_assets::AssetMeta::read_or_create(&target, &dest, json!({}))?;
    // Import it now, so a broken file is reported here rather than on first use;
    // a replaced file is re-imported (and uploaded again where it is drawn).
    let broken =
        |d: Diagnostic| fail(format!("{dest} was copied but does not import")).with_related(d);
    let problems = app.apply_file_changes(&[re_assets::AssetChange::Modified(dest.clone())]);
    if let Some(d) = problems.into_iter().next() {
        return Err(broken(d));
    }
    app.media_mut().server().load(&dest).map_err(broken)?;
    Ok(json!({ "path": dest, "id": meta.id().to_string() }))
}

/// `describe`: the built-in description (components, generators, commands)
/// plus `nodes`, the node kinds procedural graphs can use (ports, parameters),
/// and `scripts`, the project's `.luau` files under `scripts/`.
fn describe(app: &mut App, specs: &[re_command::CommandSpec]) -> serde_json::Value {
    let world = app.world();
    let mut d = world.describe();
    let nodes = world.ecs.get_resource::<re_proc::GraphStore>().map_or_else(
        || re_proc::NodeRegistry::m1().describe(),
        |store| store.evaluator().registry().describe(),
    );
    let scripts: Vec<serde_json::Value> = verify::content_files(app.project(), "scripts", ".luau")
        .into_iter()
        .map(|path| json!({ "path": path }))
        .collect();
    if let Some(obj) = d.as_object_mut() {
        obj.insert("nodes".into(), nodes);
        obj.insert("scripts".into(), json!(scripts));
        obj.insert(
            "commands".into(),
            serde_json::to_value(specs).unwrap_or_default(),
        );
    }
    d
}

/// Registers the runtime commands on `registry`. Its `describe` replaces the
/// built-in one, so register the built-ins first.
pub fn register(registry: &mut CommandRegistry<App>) {
    registry.register_typed::<NoArgs, _>(
        "describe",
        "Describe what exists: every component (fields, types, defaults, docs), every generator, every procedural node kind, the project's scripts and every command.",
        Tier::Read,
        true,
        |app, _args, call| Ok(describe(app, call.specs())),
    );
    registry.register_typed::<ScreenshotArgs, _>(
        "screenshot",
        "Capture the current view (the scene and the HUD) as a PNG in the project; returns its path, size and graphics backend.",
        Tier::Read,
        false,
        |app, args, _call| {
            let rel = match &args.path {
                Some(p) => project_path(p, "the screenshot path", codes::SCREENSHOT_FAILED)?,
                None => format!(".real/out/screenshot-{}.png", app.frame_count()),
            };
            let image = app.screenshot(Some(Path::new(&rel)))?;
            let backend = app.graphics().map_or("none", |g| g.backend());
            Ok(json!({ "path": rel, "width": image.width, "height": image.height, "backend": backend }))
        },
    );
    registry.register_typed::<PlayArgs, _>(
        "play.run",
        "Play a copy of the world for N frames (at most 3600), optionally driven by a replay, then optional steps (more frames; in a Roblox-style project, add_player, say, invoke a BindableFunction; dump_tree), and return a report (state hash, diagnostics, timings, script output, step results) and a screenshot of the last frame. The live world is not changed.",
        Tier::Read,
        false,
        |app, args, _call| play(app, args),
    );
    registry.register_typed::<ImportArgs, _>(
        "asset.import",
        "Copy a file (glTF, PNG, JPEG, material) into the project's assets/ folder, give it a .meta.json sidecar with a stable asset id, and import it once to check it.",
        Tier::Edit,
        false,
        |app, args, call| import(app, args, call.dry_run()),
    );
    registry.register_typed::<VerifyArgs, _>(
        "verify",
        "Check the project as saved on disk: content files, scripts, a headless boot, replays (twice, comparing state hashes), golden screenshots and frame times. Returns the verify report.",
        Tier::Read,
        true,
        |app, args, _call| {
            let mut options = VerifyOptions::default();
            if let Some(n) = args.boot_frames {
                options.boot_frames = n.max(1);
        options.compat_boot_frames = options.boot_frames;
                options.warmup_frames = options.warmup_frames.min(options.boot_frames - 1);
            }
            let stages = app.verify_stages();
            let refs: Vec<&dyn verify::VerifyStage> =
                stages.iter().map(|s| &**s as &dyn verify::VerifyStage).collect();
            let report = verify::run(app, &options, &refs);
            serde_json::to_value(report).map_err(|e| {
                Diagnostic::error(
                    crate::codes::BOOT_FAILED,
                    format!("the verify report could not be encoded: {e}"),
                )
            })
        },
    );
}
