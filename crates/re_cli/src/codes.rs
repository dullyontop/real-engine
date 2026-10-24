//! The diagnostic codes `re_cli` owns (`RE0900`–`RE0999` of the runtime range;
//! `re_app` owns `RE0500`–`RE0899`).

use re_core::Code;

/// `real explain` was given something that is not a known code.
pub const UNKNOWN_CODE: Code = Code::new("RE0900");

/// `real new` was pointed at a directory that already holds a project or files.
pub const PROJECT_EXISTS: Code = Code::new("RE0901");

/// A file the command writes (a schema, a screenshot, a report) could not be written.
pub const WRITE_FAILED: Code = Code::new("RE0902");

/// Scripts use a global or member the engine does not provide (a warning from
/// `real check` and verify's `scan` stage).
pub const SCRIPT_API_GAP: Code = Code::new("RE0903");

/// `[package]` in `real.toml` has an invalid value, or a default `real build`
/// had to choose (no `identifier`: a warning).
pub const PACKAGE_CONFIG: Code = Code::new("RE0950");

/// `real build` stopped because the project has errors.
pub const BUILD_STOPPED: Code = Code::new("RE0951");

/// Two project paths `real build` would pack differ only by case.
pub const CASE_COLLISION: Code = Code::new("RE0952");

/// The game runtime could not be built, or a `--runtime` binary does not match
/// this engine.
pub const RUNTIME_FAILED: Code = Code::new("RE0953");

/// The third-party notices could not be gathered, or a license is not allowed.
pub const NOTICES_FAILED: Code = Code::new("RE0954");

/// A step of laying out the game folder failed (or `--out` is not a folder
/// `real build` may write).
pub const BUNDLE_FAILED: Code = Code::new("RE0955");

/// A packaged run ended differently from the project's run (`real verify
/// --package`).
pub const PACKAGE_DIFFERS: Code = Code::new("RE0956");

/// `real build --target web` needs a tool that is not installed: Rust's
/// `wasm32-unknown-unknown` target, wasi-sdk, or the wasm-bindgen CLI.
pub const WEB_TOOLCHAIN_MISSING: Code = Code::new("RE0970");

/// The web runtime could not be built, or a `--runtime` folder is not a web
/// runtime of this engine.
pub const WEB_RUNTIME_FAILED: Code = Code::new("RE0971");

/// The installed wasm-bindgen CLI is not the version the engine's lockfile
/// pins.
pub const WASM_BINDGEN_MISMATCH: Code = Code::new("RE0972");

/// binaryen's `wasm-opt` is not installed, so the web runtime is copied
/// unoptimised (a warning).
pub const BINARYEN_MISSING: Code = Code::new("RE0973");

/// The web build's archive is over 256 MiB, more than some browsers give a
/// page (a warning).
pub const WEB_ARCHIVE_LARGE: Code = Code::new("RE0974");
