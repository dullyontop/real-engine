//! `cargo xtask d2`: the cross-machine determinism harness (D2, ADR-0036).
//!
//! ```text
//! cargo xtask d2 --project P [--project P2]... [--replay R]... [--checkpoints K] [--json] [--out FILE]
//! cargo xtask d2 compare REPORT... [--require linux-x86_64,windows-x86_64]
//! ```
//!
//! The first form builds `real` in release, runs every replay of each project
//! (`tests/replays/*.replay.json`, or the ones named with `--replay`) through
//! `real run --headless --replay R --checkpoints K --json`, and writes one report
//! with the `state_hash` at every checkpoint frame. It compares nothing: run it on
//! each platform and give the reports to `compare`.
//!
//! `compare` checks the reports replay by replay. Two required platforms that
//! differ, or a required platform with no report, fail it; the others are
//! reported only.
//!
//! The report (`schema` [`SCHEMA`]) is a frozen contract from WP18 PR "d2":
//! `{schema, platform: {os, arch, target}, rustc, projects: [{project, replays:
//! [{file, frames, checkpoints: [{frame, state_hash}]}]}]}`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{cargo, output};

/// The report format.
pub const SCHEMA: &str = "real-engine/d2/1";

/// Checkpoint spacing when `--checkpoints` is not given: one a second at 60 Hz.
const DEFAULT_EVERY: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub schema: String,
    pub platform: Platform,
    pub rustc: String,
    pub projects: Vec<ProjectRuns>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    /// `linux`, `windows` or `macos` (`std::env::consts::OS`).
    pub os: String,
    /// `x86_64` or `aarch64` (`std::env::consts::ARCH`).
    pub arch: String,
    /// The host triple `rustc -vV` reports.
    pub target: String,
}

impl Platform {
    /// `os-arch`, the name `--require` uses.
    pub fn key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRuns {
    /// The project directory as given, with `/` separators.
    pub project: String,
    pub replays: Vec<ReplayRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayRun {
    /// The replay path relative to the project, with `/` separators.
    pub file: String,
    pub frames: u64,
    pub checkpoints: Vec<Checkpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub frame: u64,
    pub state_hash: String,
}

pub fn run(root: &Path, args: &[&str]) -> Result<()> {
    match args {
        ["compare", rest @ ..] => compare_cmd(rest),
        _ => run_cmd(root, args),
    }
}

struct RunOptions {
    projects: Vec<String>,
    replays: Vec<String>,
    every: u64,
    json: bool,
    out: Option<PathBuf>,
}

fn parse_run(args: &[&str]) -> Result<RunOptions> {
    let mut o = RunOptions {
        projects: Vec::new(),
        replays: Vec::new(),
        every: DEFAULT_EVERY,
        json: false,
        out: None,
    };
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        let mut value = || {
            it.next()
                .map(|v| v.to_string())
                .with_context(|| format!("{a} needs a value"))
        };
        match a {
            "--project" => o.projects.push(value()?),
            "--replay" => o.replays.push(value()?),
            "--checkpoints" => {
                let v = value()?;
                o.every =
                    v.parse().ok().filter(|k| *k > 0).with_context(|| {
                        format!("--checkpoints takes a positive number, got {v:?}")
                    })?;
            }
            "--json" => o.json = true,
            "--out" => o.out = Some(PathBuf::from(value()?)),
            other => bail!("unknown argument {other:?} (see `cargo xtask help`)"),
        }
    }
    if o.projects.is_empty() {
        bail!("d2 needs at least one --project");
    }
    if !o.replays.is_empty() && o.projects.len() > 1 {
        bail!("--replay names replays of one project; give a single --project with it");
    }
    Ok(o)
}

fn run_cmd(root: &Path, args: &[&str]) -> Result<()> {
    let o = parse_run(args)?;
    let mut build = cargo();
    build.args(["build", "--release", "--locked", "-p", "re_cli"]);
    let status = build
        .current_dir(root)
        .status()
        .context("running cargo build")?;
    if !status.success() {
        bail!("cargo build --release -p re_cli failed ({status})");
    }
    let real = target_dir(root)?
        .join("release")
        .join(format!("real{}", std::env::consts::EXE_SUFFIX));

    let mut projects = Vec::new();
    for project in &o.projects {
        let dir = root.join(project);
        let files = if o.replays.is_empty() {
            replay_files(&dir)?
        } else {
            o.replays.clone()
        };
        if files.is_empty() {
            bail!("{project} has no tests/replays/*.replay.json");
        }
        let mut replays = Vec::new();
        for file in files {
            eprintln!("d2: {project}: {file}");
            replays.push(run_replay(&real, &dir, &file, o.every)?);
        }
        projects.push(ProjectRuns {
            project: project.replace('\\', "/"),
            replays,
        });
    }
    let report = Report {
        schema: SCHEMA.to_string(),
        platform: host_platform(root)?,
        rustc: rustc_version(root)?,
        projects,
    };
    let text = serde_json::to_string_pretty(&report)?;
    if let Some(path) = &o.out {
        std::fs::write(path, format!("{text}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
    }
    if o.json {
        println!("{text}");
    } else {
        for p in &report.projects {
            for r in &p.replays {
                let last = r.checkpoints.last().map_or("-", |c| c.state_hash.as_str());
                println!(
                    "{} {}: {} frames, {} checkpoints, final {last}",
                    p.project,
                    r.file,
                    r.frames,
                    r.checkpoints.len()
                );
            }
        }
    }
    Ok(())
}

/// `tests/replays/*.replay.json` of a project, relative to it, sorted.
fn replay_files(project: &Path) -> Result<Vec<String>> {
    let dir = project.join("tests/replays");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".replay.json") {
            files.push(format!("tests/replays/{name}"));
        }
    }
    files.sort();
    Ok(files)
}

fn run_replay(real: &Path, project: &Path, file: &str, every: u64) -> Result<ReplayRun> {
    let mut cmd = Command::new(real);
    cmd.arg("run")
        .arg("--headless")
        .arg("--project")
        .arg(project)
        .arg("--replay")
        .arg(project.join(file))
        .args(["--checkpoints", &every.to_string(), "--json"]);
    let out = cmd
        .output()
        .with_context(|| format!("running {}", real.display()))?;
    if !out.status.success() {
        bail!(
            "real run failed on {file} ({}):\n{}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    parse_run_output(file, &out.stdout)
}

/// Reads `real run --json`'s `frames` and `checkpoints`.
fn parse_run_output(file: &str, stdout: &[u8]) -> Result<ReplayRun> {
    #[derive(Deserialize)]
    struct RunJson {
        frames: u64,
        checkpoints: Vec<Checkpoint>,
    }
    let run: RunJson = serde_json::from_slice(stdout)
        .with_context(|| format!("reading real run's report for {file}"))?;
    if run.checkpoints.is_empty() {
        bail!("real run reported no checkpoints for {file}");
    }
    Ok(ReplayRun {
        file: file.to_string(),
        frames: run.frames,
        checkpoints: run.checkpoints,
    })
}

fn target_dir(root: &Path) -> Result<PathBuf> {
    let mut cmd = cargo();
    cmd.args(["metadata", "--format-version", "1", "--no-deps"]);
    let meta: serde_json::Value = serde_json::from_str(&output(cmd, root)?)?;
    meta["target_directory"]
        .as_str()
        .map(PathBuf::from)
        .context("cargo metadata has no target_directory")
}

fn rustc_vv(root: &Path) -> Result<String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut cmd = Command::new(rustc);
    cmd.arg("-vV");
    output(cmd, root)
}

fn rustc_version(root: &Path) -> Result<String> {
    let vv = rustc_vv(root)?;
    Ok(vv.lines().next().unwrap_or_default().to_string())
}

fn host_platform(root: &Path) -> Result<Platform> {
    let vv = rustc_vv(root)?;
    let target = vv
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .context("rustc -vV printed no host")?;
    Ok(Platform {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        target: target.to_string(),
    })
}

fn compare_cmd(args: &[&str]) -> Result<()> {
    let mut files = Vec::new();
    let mut required: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--require" => {
                let v = it.next().context("--require needs a value")?;
                required.extend(v.split(',').filter(|s| !s.is_empty()).map(String::from));
            }
            other if other.starts_with("--") => bail!("unknown argument {other:?}"),
            path => files.push(path),
        }
    }
    if files.is_empty() {
        bail!("d2 compare needs at least one report");
    }
    let mut reports = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(f).with_context(|| format!("reading {f}"))?;
        let r: Report = serde_json::from_str(&text).with_context(|| format!("reading {f}"))?;
        if r.schema != SCHEMA {
            bail!("{f} is a {:?} report; this xtask reads {SCHEMA}", r.schema);
        }
        reports.push(r);
    }
    let outcome = compare(&reports, &required);
    for line in &outcome.lines {
        println!("{line}");
    }
    if outcome.failures.is_empty() {
        println!("d2: required platforms agree ({})", required.join(", "));
        Ok(())
    } else {
        for f in &outcome.failures {
            eprintln!("error: {f}");
        }
        bail!(
            "d2: {} difference(s) between required platforms",
            outcome.failures.len()
        )
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// One line per platform and replay, for the log.
    pub lines: Vec<String>,
    /// What fails the comparison.
    pub failures: Vec<String>,
}

/// Compares the reports' checkpoints. The reference for each replay is the first
/// required platform that has it (or the first report when none is required).
pub fn compare(reports: &[Report], required: &[String]) -> Outcome {
    let mut out = Outcome::default();
    let mut by_platform: BTreeMap<String, &Report> = BTreeMap::new();
    for r in reports {
        if by_platform.insert(r.platform.key(), r).is_some() {
            out.failures
                .push(format!("two reports for platform {}", r.platform.key()));
        }
    }
    for p in required {
        if !by_platform.contains_key(p) {
            out.failures
                .push(format!("no report for required platform {p}"));
        }
    }
    let reference_key = required
        .iter()
        .find(|p| by_platform.contains_key(*p))
        .cloned()
        .or_else(|| reports.first().map(|r| r.platform.key()));
    let Some(reference_key) = reference_key else {
        return out;
    };
    let reference = by_platform[&reference_key];

    // Every (project, replay) any report has.
    let mut keys: Vec<(String, String)> = reports
        .iter()
        .flat_map(|r| {
            r.projects.iter().flat_map(|p| {
                p.replays
                    .iter()
                    .map(|rr| (p.project.clone(), rr.file.clone()))
            })
        })
        .collect();
    keys.sort();
    keys.dedup();

    for (project, file) in &keys {
        let base = find(reference, project, file);
        for (key, report) in &by_platform {
            let is_required = required.contains(key);
            let tag = if is_required { "required" } else { "reported" };
            let label = format!("{key} ({tag}) {project} {file}");
            let mine = find(report, project, file);
            let verdict = match (base, mine) {
                (_, None) => Err(format!("{label}: no run")),
                (None, Some(_)) => {
                    Err(format!("{label}: the reference {reference_key} has no run"))
                }
                (Some(_), Some(m)) if key == &reference_key => {
                    let last = m.checkpoints.last().map_or("-", |c| c.state_hash.as_str());
                    Ok(format!(
                        "{label}: reference, {} frames, final {last}",
                        m.frames
                    ))
                }
                (Some(b), Some(m)) => diff(b, m).map_or_else(
                    || {
                        Ok(format!(
                            "{label}: matches {reference_key} at {} checkpoints",
                            m.checkpoints.len()
                        ))
                    },
                    |d| Err(format!("{label}: differs from {reference_key}: {d}")),
                ),
            };
            match verdict {
                Ok(line) => out.lines.push(line),
                Err(line) if is_required => {
                    out.lines.push(line.clone());
                    out.failures.push(line);
                }
                Err(line) => out.lines.push(line),
            }
        }
    }
    out
}

fn find<'a>(r: &'a Report, project: &str, file: &str) -> Option<&'a ReplayRun> {
    r.projects
        .iter()
        .find(|p| p.project == project)?
        .replays
        .iter()
        .find(|rr| rr.file == file)
}

/// Where two runs of one replay part, or `None` when they agree.
fn diff(a: &ReplayRun, b: &ReplayRun) -> Option<String> {
    if a.frames != b.frames {
        return Some(format!("{} against {} frames", a.frames, b.frames));
    }
    let mut last_match = None;
    for (x, y) in a.checkpoints.iter().zip(&b.checkpoints) {
        if x.frame != y.frame {
            return Some(format!(
                "checkpoint frames differ ({} against {})",
                x.frame, y.frame
            ));
        }
        if x.state_hash != y.state_hash {
            let since = last_match.map_or("the start".to_string(), |f| format!("frame {f}"));
            return Some(format!(
                "first difference at frame {} (same at {since})",
                x.frame
            ));
        }
        last_match = Some(x.frame);
    }
    (a.checkpoints.len() != b.checkpoints.len()).then(|| {
        format!(
            "{} against {} checkpoints",
            a.checkpoints.len(),
            b.checkpoints.len()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(os: &str, hashes: &[&str]) -> Report {
        Report {
            schema: SCHEMA.into(),
            platform: Platform {
                os: os.into(),
                arch: "x86_64".into(),
                target: format!("x86_64-{os}"),
            },
            rustc: "rustc 1.97.0".into(),
            projects: vec![ProjectRuns {
                project: "examples/island".into(),
                replays: vec![ReplayRun {
                    file: "tests/replays/a.replay.json".into(),
                    frames: 60 * hashes.len() as u64,
                    checkpoints: hashes
                        .iter()
                        .enumerate()
                        .map(|(i, h)| Checkpoint {
                            frame: 60 * (i as u64 + 1),
                            state_hash: h.to_string(),
                        })
                        .collect(),
                }],
            }],
        }
    }

    fn req() -> Vec<String> {
        vec!["linux-x86_64".into(), "windows-x86_64".into()]
    }

    #[test]
    fn equal_required_platforms_pass() {
        let o = compare(
            &[report("linux", &["a", "b"]), report("windows", &["a", "b"])],
            &req(),
        );
        assert!(o.failures.is_empty(), "{:?}", o.failures);
        assert_eq!(o.lines.len(), 2);
    }

    #[test]
    fn a_difference_names_the_first_frame_and_the_last_match() {
        let o = compare(
            &[
                report("linux", &["a", "b", "c"]),
                report("windows", &["a", "x", "y"]),
            ],
            &req(),
        );
        assert_eq!(o.failures.len(), 1);
        assert!(
            o.failures[0].contains("first difference at frame 120 (same at frame 60)"),
            "{}",
            o.failures[0]
        );
    }

    #[test]
    fn an_unrequired_platform_only_reports() {
        let o = compare(
            &[
                report("linux", &["a"]),
                report("windows", &["a"]),
                report("macos", &["z"]),
            ],
            &req(),
        );
        assert!(o.failures.is_empty(), "{:?}", o.failures);
        assert!(
            o.lines
                .iter()
                .any(|l| l.starts_with("macos-x86_64 (reported)") && l.contains("differs"))
        );
    }

    #[test]
    fn a_missing_required_platform_fails() {
        let o = compare(&[report("linux", &["a"])], &req());
        assert_eq!(
            o.failures,
            vec!["no report for required platform windows-x86_64"]
        );
    }

    #[test]
    fn a_missing_replay_on_a_required_platform_fails() {
        let mut w = report("windows", &["a"]);
        w.projects[0].replays.clear();
        let o = compare(&[report("linux", &["a"]), w], &req());
        assert_eq!(o.failures.len(), 1);
        assert!(o.failures[0].contains("no run"), "{}", o.failures[0]);
    }

    #[test]
    fn run_output_needs_checkpoints() {
        let ok = parse_run_output(
            "r",
            br#"{"frames": 120, "state_hash": "h", "checkpoints": [{"frame": 60, "state_hash": "a"}, {"frame": 120, "state_hash": "h"}]}"#,
        )
        .unwrap();
        assert_eq!(ok.checkpoints.len(), 2);
        assert!(parse_run_output("r", br#"{"frames": 1, "checkpoints": []}"#).is_err());
    }

    #[test]
    fn arguments() {
        let o = parse_run(&[
            "--project",
            "examples/island",
            "--checkpoints",
            "30",
            "--json",
        ])
        .unwrap();
        assert_eq!((o.every, o.json, o.projects.len()), (30, true, 1));
        assert!(parse_run(&[]).is_err());
        assert!(parse_run(&["--project", "a", "--checkpoints", "0"]).is_err());
        assert!(parse_run(&["--project", "a", "--project", "b", "--replay", "r"]).is_err());
    }
}
