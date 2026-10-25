//! `real verify`'s `net` stage (ADR-0050 §7): each replay of a compat
//! project played by a dedicated server and one client, as child processes
//! of this `real` over loopback in lockstep, must end with the server world
//! the in-process run ends with. Only a compat project has the stage.

use std::path::Path;
use std::process::{Command, Stdio};

use re_app::verify::{VerifyContext, VerifyStage, content_files};
use re_app::{App, AppConfig, StageReport};
use re_compat::codes::NET_DIFFERS;
use re_core::{Diagnostic, Location};
use serde_json::{Value as Json, json};

/// The `net` stage.
pub struct NetStage;

impl VerifyStage for NetStage {
    fn name(&self) -> &str {
        "net"
    }

    fn run(&self, ctx: &VerifyContext<'_>) -> StageReport {
        let replays = content_files(ctx.project, "tests/replays", ".replay.json");
        if replays.is_empty() {
            return StageReport::skipped("net", "the project has no tests/replays/*.replay.json");
        }
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                return StageReport::skipped("net", format!("cannot find this program: {e}"));
            }
        };
        let mut diags = Vec::new();
        let mut runs = Vec::new();
        for file in &replays {
            match check(&exe, ctx.project, ctx.config, file) {
                Ok(run) => {
                    if run["same"] != json!(true) {
                        diags.push(
                            Diagnostic::error(
                                NET_DIFFERS,
                                format!(
                                    "{file}: the networked server ended with state hash {} and the in-process run with {}",
                                    run["server_hash"], run["state_hash"]
                                ),
                            )
                            .at(Location::file(file.clone())),
                        );
                    }
                    runs.push(run);
                }
                Err(d) => diags.push(d.at(Location::file(file.clone()))),
            }
        }
        StageReport::new("net", diags, json!({ "runs": runs }))
    }
}

fn differs(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error(NET_DIFFERS, msg)
}

/// One replay: in process, then networked.
fn check(exe: &Path, project: &Path, config: &AppConfig, file: &str) -> Result<Json, Diagnostic> {
    let path = project.join(file);
    let doc = re_input::load_replay(&path)?;
    let frames = doc.frames;
    let config = AppConfig {
        headless: true,
        persist_edits: false,
        ..config.clone()
    };
    let local = App::open(project, config)?
        .run_headless(None, Some(doc))?
        .state_hash;

    let port = free_port()?;
    // The server writes to files, not pipes: nobody reads a pipe while the
    // client runs, and a full one would stop the server mid-frame.
    let base = std::env::temp_dir().join(format!("real-verify-net-{}-{port}", std::process::id()));
    let (out_path, err_path) = (base.with_extension("json"), base.with_extension("err"));
    let create = |p: &Path| {
        std::fs::File::create(p).map_err(|e| differs(format!("cannot write {}: {e}", p.display())))
    };
    let mut server = Command::new(exe)
        .arg("--project")
        .arg(project)
        .args([
            "run",
            "--server",
            "--lockstep",
            "--bind",
            "127.0.0.1",
            "--json",
        ])
        .args(["--port", &port.to_string(), "--frames", &frames.to_string()])
        .stdout(create(&out_path)?)
        .stderr(create(&err_path)?)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| differs(format!("cannot start the server ({}): {e}", exe.display())))?;
    let client = Command::new(exe)
        .arg("--project")
        .arg(project)
        .args(["run", "--headless", "--lockstep", "--json", "--replay"])
        .arg(&path)
        .args(["--connect", &format!("127.0.0.1:{port}")])
        .output();
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            let _ = server.kill();
            let _ = server.wait();
            return Err(differs(format!("cannot start the client: {e}")));
        }
    };
    server
        .wait()
        .map_err(|e| differs(format!("the server did not finish: {e}")))?;
    let read = |p: &Path| std::fs::read(p).unwrap_or_default();
    let (server_out, server_err) = (read(&out_path), read(&err_path));
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
    let report = |out: &[u8]| serde_json::from_slice::<Json>(out).unwrap_or(Json::Null);
    let (s, c) = (report(&server_out), report(&client.stdout));
    let Some(remote) = s["state_hash"].as_str().map(str::to_string) else {
        let mut d = differs("the networked server printed no report");
        for (who, out) in [("server", &server_err), ("client", &client.stderr)] {
            let text = String::from_utf8_lossy(out);
            if !text.trim().is_empty() {
                d = d.with_related(differs(format!("{who}: {}", text.trim())));
            }
        }
        return Err(d);
    };
    let errors = |r: &Json| -> Vec<Json> {
        r["diagnostics"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|d| d["severity"] == json!("error"))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(json!({
        "replay": file,
        "frames": s["frames"],
        "state_hash": local,
        "server_hash": remote,
        "same": local == remote && s["frames"] == json!(frames),
        "server_errors": errors(&s),
        "client_errors": errors(&c),
    }))
}

/// A UDP port on loopback that nothing uses now.
fn free_port() -> Result<u16, Diagnostic> {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .map_err(|e| differs(format!("no free port on loopback: {e}")))
}
