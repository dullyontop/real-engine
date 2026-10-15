//! WP16's steps: the sample boots and plays through the runtime (`re_app`),
//! as `real verify` and `play.run` run it.
//!
//! * `app.boot`: opens the imported project headless and runs the boot stage
//!   of `real verify` for `frames` frames (default 600). Fails on an
//!   uncaught error or a thread still waiting in `WaitForChild` at the end,
//!   unless its message contains one of `allow`. `output` lists lines the
//!   output must hold, in order (`exact: true` for exactly those lines), and
//!   `unsupported_max` caps the distinct unsupported members reached.
//! * `app.play`: plays a fresh copy for `frames` frames (default 120), then
//!   `steps` (the `play.run` steps). `expect` is a list of `{ "step": i,
//!   "pointer": "/results/0", "equals": v }` (or `"contains": "text"`, for a
//!   string or the JSON text of the value) on the step results; `output`
//!   lists lines the output must contain. Uncaught errors fail it as in
//!   `app.boot`.
//!
//! Both read `players` (default 1) as `real.toml` `[compat] players`.

use re_app::verify::{self, VerifyOptions};
use re_app::{App, AppConfig};
use serde_json::Value as J;

use super::Ctx;

pub fn step(name: &str) -> Option<super::Step> {
    Some(match name {
        "app.boot" => boot,
        "app.play" => play,
        _ => return None,
    })
}

fn strings(v: &J) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The imported project's configuration for a headless run, with
/// `[compat] players` from `args`.
fn config(ctx: &mut Ctx, args: &J) -> Result<AppConfig, String> {
    ctx.import()?;
    let project = ctx.project();
    if let Some(n) = args["players"].as_u64() {
        let path = project.join("real.toml");
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut doc: toml::Table = text.parse().map_err(|e| format!("real.toml: {e}"))?;
        if let Some(toml::Value::Table(compat)) = doc.get_mut("compat") {
            compat.insert("players".into(), toml::Value::Integer(n as i64));
        }
        std::fs::write(&path, toml::to_string(&doc).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    let config = AppConfig::load(&project).map_err(|d| d.render_human())?;
    Ok(AppConfig {
        headless: true,
        persist_edits: false,
        ..config
    })
}

/// The failures `allow` does not excuse.
fn unexcused(messages: Vec<String>, allow: &[String]) -> Vec<String> {
    messages
        .into_iter()
        .filter(|m| !allow.iter().any(|a| m.contains(a.as_str())))
        .collect()
}

fn check_output(output: &[String], want: &[String], exact: bool) -> Result<(), String> {
    if exact {
        if output != want {
            return Err(format!(
                "the output is {} line(s), not the {} expected:\n{}",
                output.len(),
                want.len(),
                output.join("\n")
            ));
        }
        return Ok(());
    }
    let mut at = 0;
    for line in want {
        match output[at..].iter().position(|o| o == line) {
            Some(i) => at += i + 1,
            None => return Err(format!("the output lacks (in order) {line:?}")),
        }
    }
    Ok(())
}

fn boot(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let config = config(ctx, args)?;
    let options = VerifyOptions {
        compat_boot_frames: args["frames"].as_u64().unwrap_or(600),
        ..VerifyOptions::default()
    };
    let report = verify::run_project(&ctx.project(), &config, &options, &[]);
    let boot = report.stage("boot").ok_or("no boot stage")?;
    let allow = strings(&args["allow"]);
    let errors = unexcused(
        boot.diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.render_human())
            .collect(),
        &allow,
    );
    if !errors.is_empty() {
        return Err(format!(
            "{} error(s) in the boot:\n{}",
            errors.len(),
            errors.join("\n")
        ));
    }
    let output = strings(&boot.data["output"]);
    check_output(
        &output,
        &strings(&args["output"]),
        args["exact"].as_bool().unwrap_or(false),
    )?;
    if let Some(max) = args["unsupported_max"].as_u64() {
        let n = boot.data["unsupported"].as_array().map_or(0, Vec::len) as u64;
        if n > max {
            return Err(format!(
                "{n} unsupported member(s) reached, over {max}: {}",
                boot.data["unsupported"]
            ));
        }
    }
    Ok(())
}

fn play(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let config = config(ctx, args)?;
    let mut app = App::open(&ctx.project(), config).map_err(|d| d.render_human())?;
    let steps: Vec<re_app::play::PlayStep> = serde_json::from_value(args["steps"].clone())
        .or_else(|e| {
            if args["steps"].is_null() {
                Ok(Vec::new())
            } else {
                Err(format!("steps: {e}"))
            }
        })?;
    let frames = args["frames"].as_u64().unwrap_or(120);
    let (report, results, _) = app
        .play_copy(frames, None, &steps, false)
        .map_err(|d| d.render_human())?;
    let allow = strings(&args["allow"]);
    let errors = unexcused(
        report
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.render_human())
            .collect(),
        &allow,
    );
    if !errors.is_empty() {
        return Err(format!(
            "{} error(s) in the play:\n{}",
            errors.len(),
            errors.join("\n")
        ));
    }
    check_output(&report.output, &strings(&args["output"]), false)?;
    for (i, e) in args["expect"].as_array().into_iter().flatten().enumerate() {
        let step = e["step"].as_u64().ok_or(format!("expect[{i}]: no step"))? as usize;
        let result = results
            .get(step)
            .ok_or(format!("expect[{i}]: no step {step}"))?;
        let pointer = e["pointer"].as_str().unwrap_or("");
        let got = result.pointer(pointer).ok_or(format!(
            "expect[{i}]: step {step} has no {pointer}: {result}"
        ))?;
        if let Some(want) = e.get("equals")
            && got != want
        {
            return Err(format!(
                "expect[{i}]: step {step} {pointer} is {got}, not {want}"
            ));
        }
        if let Some(text) = e["contains"].as_str() {
            let hay = got.as_str().map_or_else(|| got.to_string(), str::to_string);
            if !hay.contains(text) {
                return Err(format!(
                    "expect[{i}]: step {step} {pointer} lacks {text:?}: {hay}"
                ));
            }
        }
    }
    Ok(())
}
