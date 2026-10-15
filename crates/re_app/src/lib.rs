//! `re_app`: the Real Engine runtime.
//!
//! * [`App`]: a project opened for running and editing. [`App::open`] loads
//!   `real.toml` ([`AppConfig`]) and the entry scene; [`App::dispatch`] and
//!   [`App::serve`] edit it through the command registry (the CLI, the MCP
//!   server and the editor); [`App::frame`] and [`App::run_headless`] play it
//!   through the phases of ADR-0009.
//! * [`sync`]: the glue between the World and the subsystems (ADR-0006):
//!   physics commands and poses, the render snapshot, audio commands.
//! * [`host`]: the [`HostServices`](re_types::HostServices) scripts call.
//! * [`verify`]: `real verify` (ADR-0022), with [`verify::VerifyStage`] for
//!   stages the caller adds.
//! * [`perf`]: the PerfReport.
//! * [`commands`]: the runtime commands, starting with `verify`.
//!
//! `App::open`, `App::serve`, `AppConfig` and `verify::{run, VerifyStage}` are a
//! frozen contract from WP6 milestone "app" (docs/M1_PLAN.md): WP7 and WP8 build
//! on them. Diagnostic codes are `RE0500`–`RE0899` ([`codes`]).

#![warn(missing_docs)]

pub mod app;
pub mod codes;
pub mod commands;
pub mod config;
pub mod host;
pub mod perf;
pub mod sync;
pub mod verify;

pub use app::{App, RunReport, ScreenshotRequest};
pub use config::{AppConfig, CONFIG_FILE};
pub use host::{Host, Pending};
pub use perf::{PerfReport, TimeStats};
pub use verify::{StageReport, VerifyOptions, VerifyReport, VerifyStage};

use re_core::DiagInfo;

/// Every diagnostic code `re_app` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::PROJECT_NOT_FOUND,
        title: "Not a project directory",
        page: include_str!("../explain/RE0500.md"),
    },
    DiagInfo {
        code: codes::CONFIG_INVALID,
        title: "Invalid real.toml",
        page: include_str!("../explain/RE0501.md"),
    },
    DiagInfo {
        code: codes::ENTRY_SCENE_FAILED,
        title: "Entry scene failed to load",
        page: include_str!("../explain/RE0502.md"),
    },
    DiagInfo {
        code: codes::HOST_NO_SUCH_THING,
        title: "Script asked for something an entity does not have",
        page: include_str!("../explain/RE0503.md"),
    },
    DiagInfo {
        code: codes::HOST_INVALID_ARGUMENT,
        title: "Invalid argument to a runtime service",
        page: include_str!("../explain/RE0504.md"),
    },
    DiagInfo {
        code: codes::PHYSICS_FIELD_OUT_OF_RANGE,
        title: "Physics field out of range",
        page: include_str!("../explain/RE0505.md"),
    },
    DiagInfo {
        code: codes::SHAPE_SOURCE_MISSING,
        title: "Collider source not available",
        page: include_str!("../explain/RE0506.md"),
    },
    DiagInfo {
        code: codes::REPLAY_FAILED,
        title: "Replay cannot be played",
        page: include_str!("../explain/RE0507.md"),
    },
    DiagInfo {
        code: codes::NONDETERMINISTIC,
        title: "Replay is not deterministic",
        page: include_str!("../explain/RE0508.md"),
    },
    DiagInfo {
        code: codes::GOLDEN_MISMATCH,
        title: "Screenshot differs from its golden image",
        page: include_str!("../explain/RE0509.md"),
    },
    DiagInfo {
        code: codes::GOLDEN_MISSING,
        title: "Golden image missing",
        page: include_str!("../explain/RE0510.md"),
    },
    DiagInfo {
        code: codes::PERF_BUDGET,
        title: "Frame time over budget",
        page: include_str!("../explain/RE0511.md"),
    },
    DiagInfo {
        code: codes::STAGE_SKIPPED,
        title: "Verify stage skipped",
        page: include_str!("../explain/RE0512.md"),
    },
    DiagInfo {
        code: codes::CONTENT_INVALID,
        title: "Invalid content file",
        page: include_str!("../explain/RE0513.md"),
    },
    DiagInfo {
        code: codes::BOOT_FAILED,
        title: "Project did not boot",
        page: include_str!("../explain/RE0514.md"),
    },
    DiagInfo {
        code: codes::REPLAY_COMMAND_FAILED,
        title: "Replay command failed",
        page: include_str!("../explain/RE0515.md"),
    },
    DiagInfo {
        code: codes::SCREENSHOT_FAILED,
        title: "Screenshot cannot be taken",
        page: include_str!("../explain/RE0516.md"),
    },
    DiagInfo {
        code: codes::SCENE_SAVE_FAILED,
        title: "Scene could not be saved",
        page: include_str!("../explain/RE0518.md"),
    },
    DiagInfo {
        code: codes::RUN_ERRORS,
        title: "Run reported errors",
        page: include_str!("../explain/RE0519.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7; `re_cli` owns RE0900–0999).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 500..=899;
