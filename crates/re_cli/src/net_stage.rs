//! `real verify`'s `net` stage (ADR-0050 §7): each replay of a compat
//! project played by a dedicated server and one client, as child processes
//! of this `real` over loopback in lockstep, must end with the server world
//! the in-process run ends with. Each replay is played twice: the client
//! joins over UDP, then over a WebSocket (ADR-0050's amendment for PR
//! "web"). Only a compat project has the stage.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

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
            let local = match in_process(ctx.project, ctx.config, file) {
                Ok(l) => l,
                Err(d) => {
                    diags.push(d.at(Location::file(file.clone())));
                    continue;
                }
            };
            for transport in [Transport::Udp, Transport::WebSocket] {
                match check(&exe, ctx.project, file, &local, transport) {
                    Ok(run) => {
                        if run["same"] != json!(true) {
                            diags.push(
                                Diagnostic::error(
                                    NET_DIFFERS,
                                    format!(
                                        "{file}: the networked server (a client over {}) ended with state hash {} and the in-process run with {}",
                                        transport.name(),
                                        run["server_hash"],
                                        run["state_hash"]
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
        }
        StageReport::new("net", diags, json!({ "runs": runs }))
    }
}

/// How the client reaches its server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
    Udp,
    WebSocket,
}

impl Transport {
    fn name(self) -> &'static str {
        match self {
            Transport::Udp => "udp",
            Transport::WebSocket => "websocket",
        }
    }

    /// The client's `--connect` address for a server on loopback at `port`.
    fn address(self, port: u16) -> String {
        match self {
            Transport::Udp => format!("127.0.0.1:{port}"),
            Transport::WebSocket => format!("ws://127.0.0.1:{port}/"),
        }
    }
}

/// A replay played in process: its frames and its `state_hash`.
struct InProcess {
    frames: u64,
    state_hash: String,
}

fn in_process(project: &Path, config: &AppConfig, file: &str) -> Result<InProcess, Diagnostic> {
    let doc = re_input::load_replay(&project.join(file))?;
    let frames = doc.frames;
    let config = AppConfig {
        headless: true,
        persist_edits: false,
        ..config.clone()
    };
    let state_hash = App::open(project, config)?
        .run_headless(None, Some(doc))?
        .state_hash;
    Ok(InProcess { frames, state_hash })
}

fn differs(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error(NET_DIFFERS, msg)
}

/// One replay networked, with the client over `transport`, against the
/// in-process run `local`.
fn check(
    exe: &Path,
    project: &Path,
    file: &str,
    local: &InProcess,
    transport: Transport,
) -> Result<Json, Diagnostic> {
    let path = project.join(file);
    let frames = local.frames;
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
    wait_until_serving(&mut server, &err_path);
    let client = Command::new(exe)
        .arg("--project")
        .arg(project)
        .args(["run", "--headless", "--lockstep", "--json", "--replay"])
        .arg(&path)
        .args(["--connect", &transport.address(port)])
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
        "transport": transport.name(),
        "frames": s["frames"],
        "state_hash": local.state_hash,
        "server_hash": remote,
        "same": local.state_hash == remote && s["frames"] == json!(frames),
        "server_errors": errors(&s),
        "client_errors": errors(&c),
    }))
}

/// Waits until the `server` writing its stderr to `err_path` says it is
/// serving, or it exits, for at most a minute: a WebSocket client's connect
/// fails at once when nothing listens yet (a UDP client's handshake waits).
fn wait_until_serving(server: &mut Child, err_path: &Path) {
    let start = re_core::time::Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        let said = std::fs::read_to_string(err_path).unwrap_or_default();
        if said.contains("serving on ") || !matches!(server.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A port on loopback that nothing uses now, for UDP and for TCP (the
/// server's WebSocket listener).
fn free_port() -> Result<u16, Diagnostic> {
    let mut failed = None;
    for _ in 0..16 {
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").and_then(|s| s.local_addr());
        match udp.and_then(|a| std::net::TcpListener::bind(a).map(|_| a.port())) {
            Ok(port) => return Ok(port),
            Err(e) => failed = Some(e),
        }
    }
    let e = failed.expect("tried at least once");
    Err(differs(format!("no free port on loopback: {e}")))
}
