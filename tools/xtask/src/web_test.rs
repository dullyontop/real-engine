//! `cargo xtask web-test`: the browser build's checks (ADR-0049 §8), as CI's
//! `build-web` job runs them. It builds `real`, builds the island and the plaza with
//! `real build --target web` into `target/web/` (failing when `real_bg.wasm` gzips
//! above the 8 MB budget), and in headless Chromium with WebGPU plays each one's
//! replays from its game.rpak and starts the island's game page, with
//! `tools/web-test`'s scripts. Each replay's `state_hash` must equal the native run's,
//! or with `--reported` is only printed (until WP18 PR "d2").
//!
//! It needs what `real build --target web` needs (Rust's wasm32 target, wasi-sdk with
//! `WASI_SDK_PATH` set, the wasm-bindgen CLI, and binaryen for the budget), and
//! Node with `npm ci` and `npx playwright install chromium` run in `tools/web-test`.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The web runtime's budget: `real_bg.wasm` gzipped (ADR-0049).
const BUDGET: u64 = 8_000_000;

/// The projects and the replays each plays.
const PROJECTS: [(&str, &[&str]); 2] = [
    (
        "island",
        &[
            "tests/replays/walk_collect.replay.json",
            "tests/replays/full_game.replay.json",
            "tests/replays/regenerate.replay.json",
        ],
    ),
    (
        "plaza",
        &[
            "tests/replays/idle.replay.json",
            "tests/replays/walk.replay.json",
        ],
    ),
];

fn run_cmd(root: &Path, cmd: &mut Command) -> Result<()> {
    let shown = format!("{cmd:?}");
    let status = cmd
        .current_dir(root)
        .status()
        .with_context(|| format!("running {shown}"))?;
    if !status.success() {
        bail!("{shown} failed ({status})");
    }
    Ok(())
}

fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

/// `bytes` gzipped at level 9, as CI's `gzip -9` measures it.
fn gzipped_size(root: &Path, file: &Path) -> Result<u64> {
    let out = Command::new("gzip")
        .args(["-9", "-c"])
        .arg(file)
        .current_dir(root)
        .output()
        .context("running gzip")?;
    if !out.status.success() {
        bail!("gzip could not read {}", file.display());
    }
    Ok(out.stdout.len() as u64)
}

pub fn run(root: &Path, reported: bool) -> Result<()> {
    run_cmd(
        root,
        cargo().args(["build", "--locked", "--release", "-p", "re_cli"]),
    )?;
    let real = root.join("target/release/real");
    let out = root.join("target/web");
    std::fs::create_dir_all(&out).context("creating target/web")?;
    for (i, (name, _)) in PROJECTS.iter().enumerate() {
        let mut cmd = Command::new(&real);
        cmd.arg("--project")
            .arg(root.join("examples").join(name))
            .args(["build", "--target", "web", "--out"])
            .arg(out.join(name));
        // The runtime is built once; later projects take the first build's.
        if i > 0 {
            cmd.arg("--runtime").arg(out.join(PROJECTS[0].0));
        }
        run_cmd(root, &mut cmd)?;
    }
    let wasm = out.join(PROJECTS[0].0).join("real_bg.wasm");
    let size = gzipped_size(root, &wasm)?;
    println!("real_bg.wasm: {size} bytes gzipped (budget {BUDGET})");
    if size > BUDGET {
        bail!("real_bg.wasm is {size} bytes gzipped, over the 8 MB budget (ADR-0049)");
    }
    for (name, replays) in PROJECTS {
        let project = root.join("examples").join(name);
        let expected = out.join(format!("{name}.jsonl"));
        let native = cargo()
            .args([
                "run",
                "--locked",
                "--release",
                "-p",
                "re_web",
                "--example",
                "replay",
                "--",
            ])
            .arg(&project)
            .args(replays)
            .current_dir(root)
            .output()
            .context("running re_web's replay example")?;
        if !native.status.success() {
            bail!(
                "the native replay run failed:\n{}",
                String::from_utf8_lossy(&native.stderr)
            );
        }
        std::fs::write(&expected, &native.stdout).context("writing the native runs")?;
        let mut cmd = Command::new("node");
        cmd.arg("tools/web-test/browser.mjs")
            .arg("--expect")
            .arg(&expected);
        if reported {
            cmd.arg("--reported");
        }
        run_cmd(root, cmd.arg(out.join(name)).arg(&project).args(replays))?;
    }
    run_cmd(
        root,
        Command::new("node")
            .arg("tools/web-test/play.mjs")
            .arg(out.join(PROJECTS[0].0)),
    )
}
