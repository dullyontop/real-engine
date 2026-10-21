//! `real build --target web` (ADR-0049 §7): the game folder a static web
//! server hosts. It holds the web runtime (wasm-bindgen's `real.js` and
//! `real_bg.wasm`, re_web's `boot.js`, `wasi.js` and `index.html`, and
//! `runtime.json` naming its versions) beside the same `game.rpak` as a desktop
//! build. The runtime is built with cargo from the engine's workspace, or taken
//! from `--runtime DIR`: a prebuilt web runtime, or an earlier web build.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use re_app::AppConfig;
use re_app::package::{ARCHIVE_NAME, RuntimeVersions};
use re_core::Diagnostic;

use super::BuildReport;
use crate::codes;

/// The Rust target of the web runtime.
pub const TARGET: &str = "wasm32-unknown-unknown";

/// The file naming a web runtime's versions.
pub const RUNTIME_INFO: &str = "runtime.json";

/// A web runtime's files, as a runtime folder and every web build hold them.
pub const RUNTIME_FILES: &[&str] = &[
    "index.html",
    "boot.js",
    "wasi.js",
    "real.js",
    "real_bg.wasm",
    RUNTIME_INFO,
];

/// The archive size above which a web build warns (`RE0974`).
pub const LARGE_ARCHIVE: u64 = 256 << 20;

/// The version keys a runtime must share with this engine.
const VERSION_KEYS: [&str; 4] = ["engine", "luau", "bytecode_version", "compiler"];

/// The wasm features the runtime uses, for `wasm-opt` (exnref exceptions for
/// Luau; the rest are what rustc emits by default).
const WASM_FEATURES: &[&str] = &[
    "--enable-exception-handling",
    "--enable-reference-types",
    "--enable-bulk-memory",
    "--enable-nontrapping-float-to-int",
    "--enable-sign-ext",
    "--enable-mutable-globals",
    "--enable-multivalue",
];

/// True when `target` asks for the web build.
pub fn is_web(target: &str) -> bool {
    target == "web" || target == TARGET
}

/// Lays out the web game folder in `out`, where the archive already is.
/// Diagnostics go to `report`; returns false when the build failed.
pub fn build(
    out: &Path,
    runtime: Option<&Path>,
    config: &AppConfig,
    report: &mut BuildReport,
) -> bool {
    let diagnostics = &mut report.pack.diagnostics;
    let runtime = match runtime {
        Some(dir) => check_runtime(dir).map(|()| dir.to_owned()),
        None => build_runtime(diagnostics),
    };
    let runtime = match runtime {
        Ok(dir) => dir,
        Err(d) => {
            diagnostics.push(d);
            return false;
        }
    };
    if let Err(d) = lay_out(out, &runtime, config, &mut report.files) {
        report.pack.diagnostics.push(d);
        return false;
    }
    let size = std::fs::metadata(out.join(ARCHIVE_NAME)).map_or(0, |m| m.len());
    if size > LARGE_ARCHIVE {
        report.pack.diagnostics.push(
            Diagnostic::warning(
                codes::WEB_ARCHIVE_LARGE,
                format!(
                    "{ARCHIVE_NAME} is {:.1} MiB; a web game downloads all of it before it starts",
                    size as f64 / f64::from(1 << 20)
                ),
            )
            .with_hint("leave out what the game does not need with `[package] exclude`"),
        );
    }
    report.notes.push(
        "the web build has no THIRD-PARTY-NOTICES.txt yet; the notices arrive with WP19 PR \"bundles\""
            .to_owned(),
    );
    true
}

/// Copies the runtime into `out`, with the page titled after the game.
fn lay_out(
    out: &Path,
    runtime: &Path,
    config: &AppConfig,
    files: &mut Vec<String>,
) -> Result<(), Diagnostic> {
    let failed = |what: &str, e: std::io::Error| {
        Diagnostic::error(codes::BUNDLE_FAILED, format!("cannot write {what}: {e}"))
    };
    for name in RUNTIME_FILES {
        let from = runtime.join(name);
        if *name == "index.html" {
            let page = std::fs::read_to_string(&from).map_err(|e| failed(name, e))?;
            let title = config
                .package
                .as_ref()
                .and_then(|p| p.display_name.clone())
                .unwrap_or_else(|| config.project.name.clone());
            std::fs::write(out.join(name), titled(&page, &title)).map_err(|e| failed(name, e))?;
        } else if from != out.join(name) {
            std::fs::copy(&from, out.join(name)).map_err(|e| failed(name, e))?;
        }
        files.push((*name).to_owned());
    }
    Ok(())
}

/// `page` with its `<title>` replaced by `title`, escaped.
fn titled(page: &str, title: &str) -> String {
    let escaped = title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    match (page.find("<title>"), page.find("</title>")) {
        (Some(start), Some(end)) if start < end => {
            format!("{}<title>{escaped}{}", &page[..start], &page[end..])
        }
        _ => page.to_owned(),
    }
}

/// The versions a runtime built now would carry, as `runtime.json` holds them.
fn runtime_info() -> serde_json::Value {
    let mut info = serde_json::to_value(RuntimeVersions::current()).unwrap_or_default();
    info["target"] = TARGET.into();
    info
}

/// Checks that `dir` holds a web runtime of this engine.
fn check_runtime(dir: &Path) -> Result<(), Diagnostic> {
    let failed = |msg: String| Diagnostic::error(codes::WEB_RUNTIME_FAILED, msg);
    let missing: Vec<&str> = RUNTIME_FILES
        .iter()
        .copied()
        .filter(|f| !dir.join(f).is_file())
        .collect();
    if !missing.is_empty() {
        return Err(failed(format!(
            "{} is not a web runtime: it has no {}",
            dir.display(),
            missing.join(", ")
        ))
        .with_hint(
            "pass the folder of a prebuilt web runtime or of an earlier `real build --target web`",
        ));
    }
    let text = std::fs::read_to_string(dir.join(RUNTIME_INFO)).map_err(|e| {
        failed(format!(
            "cannot read {}: {e}",
            dir.join(RUNTIME_INFO).display()
        ))
    })?;
    let info: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        failed(format!(
            "{} is not a web runtime's {RUNTIME_INFO}: {e}",
            dir.join(RUNTIME_INFO).display()
        ))
    })?;
    let ours = runtime_info();
    let differ: Vec<String> = VERSION_KEYS
        .iter()
        .filter(|k| info.get(*k) != ours.get(*k))
        .map(|k| {
            format!(
                "{k} {} (this build needs {})",
                info.get(*k).unwrap_or(&serde_json::Value::Null),
                ours[*k]
            )
        })
        .collect();
    if differ.is_empty() {
        Ok(())
    } else {
        Err(failed(format!(
            "the web runtime in {} does not match: {}",
            dir.display(),
            differ.join(", ")
        )))
    }
}

/// The version of `package` in the lockfile at `lock`.
fn locked_version(lock: &str, package: &str) -> Option<String> {
    let name = format!("name = \"{package}\"");
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() == name {
            let version = lines.next()?.trim().strip_prefix("version = \"")?;
            return Some(version.trim_end_matches('"').to_owned());
        }
    }
    None
}

/// The first line a tool prints for `--version`, or None when it does not run.
fn tool_version(tool: &str) -> Option<String> {
    let out = Command::new(tool).arg("--version").output().ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_owned()
    })
}

/// The environment wasi-sdk at `sdk` needs (the same as `tools/ci/wasi-sdk.sh`).
fn wasi_env(sdk: &Path) -> Vec<(String, String)> {
    let sysroot = sdk.join("share/wasi-sysroot");
    let lib = sysroot.join("lib/wasm32-wasip1");
    let bin = |t: &str| sdk.join("bin").join(t).display().to_string();
    vec![
        ("CC_wasm32_unknown_unknown".into(), bin("clang")),
        ("CXX_wasm32_unknown_unknown".into(), bin("clang++")),
        ("AR_wasm32_unknown_unknown".into(), bin("llvm-ar")),
        ("CXXSTDLIB_wasm32_unknown_unknown".into(), "c++".into()),
        (
            "CXXFLAGS_wasm32_unknown_unknown".into(),
            format!(
                "--target=wasm32-wasip1 --sysroot={} -fwasm-exceptions -mllvm -wasm-use-legacy-eh=false",
                sysroot.display()
            ),
        ),
        (
            "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS".into(),
            format!(
                "-L {} -L {} -l static=c++abi -l static=unwind -l static=c -l static=wasi-emulated-process-clocks",
                lib.join("eh").display(),
                lib.display()
            ),
        ),
    ]
}

/// Checks the toolchain the web runtime's build needs; returns wasi-sdk's path.
fn check_toolchain(ws: &Path) -> Result<PathBuf, Diagnostic> {
    let missing = |msg: String, hint: &str| {
        Diagnostic::error(codes::WEB_TOOLCHAIN_MISSING, msg).with_hint(hint.to_owned())
    };
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let sysroot = Command::new(&rustc)
        .current_dir(ws)
        .args(["--print", "sysroot"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    if !sysroot.is_some_and(|s| s.join("lib/rustlib").join(TARGET).is_dir()) {
        return Err(missing(
            format!("Rust's {TARGET} target is not installed"),
            "run `rustup target add wasm32-unknown-unknown`, or pass `--runtime DIR`",
        ));
    }
    let Some(sdk) = std::env::var_os("WASI_SDK_PATH").map(PathBuf::from) else {
        return Err(missing(
            "wasi-sdk was not found: WASI_SDK_PATH is not set".to_owned(),
            "install wasi-sdk 34 and set WASI_SDK_PATH (the engine's tools/ci/wasi-sdk.sh does both), or pass `--runtime DIR`",
        ));
    };
    if !sdk.join("bin/clang++").is_file() && !sdk.join("bin/clang++.exe").is_file() {
        return Err(missing(
            format!("WASI_SDK_PATH {} holds no bin/clang++", sdk.display()),
            "point WASI_SDK_PATH at the unpacked wasi-sdk 34 folder",
        ));
    }
    let lock = std::fs::read_to_string(ws.join("Cargo.lock")).unwrap_or_default();
    let want = locked_version(&lock, "wasm-bindgen").unwrap_or_default();
    let install = format!("cargo install --locked wasm-bindgen-cli --version {want}");
    match tool_version("wasm-bindgen") {
        None => Err(missing(
            "the wasm-bindgen CLI was not found".to_owned(),
            &format!("run `{install}`"),
        )),
        Some(line) if line.split_whitespace().nth(1) != Some(want.as_str()) => {
            Err(Diagnostic::error(
                codes::WASM_BINDGEN_MISMATCH,
                format!(
                    "{} is installed; this engine needs wasm-bindgen {want}",
                    line.trim()
                ),
            )
            .with_hint(format!("run `{install}`")))
        }
        Some(_) => Ok(sdk),
    }
}

/// Builds the web runtime with cargo in the engine workspace, into
/// `target/web-runtime/`. A missing `wasm-opt` is a warning in `diagnostics`.
fn build_runtime(diagnostics: &mut Vec<Diagnostic>) -> Result<PathBuf, Diagnostic> {
    let failed = |msg: String| Diagnostic::error(codes::WEB_RUNTIME_FAILED, msg);
    let Some(ws) = super::engine_workspace() else {
        return Err(failed(
            "this `real` cannot build the web runtime: the engine's source is not where it was built".into(),
        )
        .with_hint("pass `--runtime DIR` with a web runtime of this engine version"));
    };
    let sdk = check_toolchain(&ws)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    eprintln!("building the web runtime (cargo --profile web --target {TARGET})...");
    let status = Command::new(cargo)
        .current_dir(&ws)
        .args([
            "build",
            "--locked",
            "--profile",
            "web",
            "--target",
            TARGET,
            "-p",
            "re_web",
        ])
        .env("WASI_SDK_PATH", &sdk)
        .envs(wasi_env(&sdk))
        .stdout(Stdio::null())
        .status()
        .map_err(|e| failed(format!("cannot run cargo: {e}")))?;
    if !status.success() {
        return Err(failed(format!(
            "cargo could not build the web runtime ({status})"
        )));
    }
    let module = ws.join("target").join(TARGET).join("web/re_web.wasm");
    let stage = ws.join("target/web-runtime");
    let _ = std::fs::remove_dir_all(&stage);
    let status = Command::new("wasm-bindgen")
        .args(["--target", "web", "--out-name", "real", "--out-dir"])
        .arg(&stage)
        .arg(&module)
        .stdout(Stdio::null())
        .status()
        .map_err(|e| failed(format!("cannot run wasm-bindgen: {e}")))?;
    if !status.success() {
        return Err(failed(format!("wasm-bindgen failed ({status})")));
    }
    let wasm = stage.join("real_bg.wasm");
    if tool_version("wasm-opt").is_some() {
        eprintln!("optimising real_bg.wasm (wasm-opt -Oz)...");
        let status = Command::new("wasm-opt")
            .args(["-Oz", "--strip-debug", "--strip-producers"])
            .args(WASM_FEATURES)
            .arg(&wasm)
            .arg("-o")
            .arg(&wasm)
            .status()
            .map_err(|e| failed(format!("cannot run wasm-opt: {e}")))?;
        if !status.success() {
            return Err(failed(format!("wasm-opt failed ({status})")));
        }
    } else {
        diagnostics.push(
            Diagnostic::warning(
                codes::BINARYEN_MISSING,
                "wasm-opt was not found, so real_bg.wasm is not optimised",
            )
            .with_hint("install binaryen 133 for a smaller download"),
        );
    }
    let web = ws.join("crates/re_web/web");
    for name in ["index.html", "boot.js", "wasi.js"] {
        std::fs::copy(web.join(name), stage.join(name))
            .map_err(|e| failed(format!("cannot copy {name}: {e}")))?;
    }
    let info = serde_json::to_string_pretty(&runtime_info()).unwrap_or_default();
    std::fs::write(stage.join(RUNTIME_INFO), info + "\n")
        .map_err(|e| failed(format!("cannot write {RUNTIME_INFO}: {e}")))?;
    Ok(stage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_takes_the_game_s_title() {
        let page = "<head><title>Real Engine</title></head>";
        assert_eq!(
            titled(page, "Rock & <Roll>"),
            "<head><title>Rock &amp; &lt;Roll&gt;</title></head>"
        );
        assert_eq!(titled("<p>no title</p>", "x"), "<p>no title</p>");
    }

    #[test]
    fn the_lockfile_names_wasm_bindgen_s_version() {
        let lock = "[[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.129\"\n\n[[package]]\nname = \"wasm-bindgen-futures\"\nversion = \"0.4.79\"\n";
        assert_eq!(
            locked_version(lock, "wasm-bindgen").as_deref(),
            Some("0.2.129")
        );
        assert_eq!(
            locked_version(lock, "wasm-bindgen-futures").as_deref(),
            Some("0.4.79")
        );
        assert_eq!(locked_version(lock, "nothing"), None);
    }

    #[test]
    fn a_runtime_folder_is_checked_against_this_engine() {
        let dir = tempfile::tempdir().unwrap();
        let err = check_runtime(dir.path()).unwrap_err();
        assert_eq!(err.code, codes::WEB_RUNTIME_FAILED);
        assert!(err.message.contains("real_bg.wasm"), "{}", err.message);
        for f in RUNTIME_FILES {
            std::fs::write(dir.path().join(f), "").unwrap();
        }
        std::fs::write(dir.path().join(RUNTIME_INFO), runtime_info().to_string()).unwrap();
        check_runtime(dir.path()).unwrap();
        let mut old = runtime_info();
        old["engine"] = "0.0.1".into();
        std::fs::write(dir.path().join(RUNTIME_INFO), old.to_string()).unwrap();
        let err = check_runtime(dir.path()).unwrap_err();
        assert!(err.message.contains("engine \"0.0.1\""), "{}", err.message);
    }
}
