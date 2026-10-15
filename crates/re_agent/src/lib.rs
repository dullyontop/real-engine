//! `re_agent`: agent tooling for Real Engine (ADR-0020, ADR-0026, ADR-0027).
//!
//! * [`policy`]: [`Policy`], `.real/agent-policy.toml` (tiers, path scopes,
//!   budgets), the [`re_command::PolicyCheck`] for agent calls;
//! * [`session`]: [`Session`] and [`Checkpoint`]: a checkpoint at the start
//!   of each agent session, covering the scene and the files it touches;
//! * [`commands`]: [`register_commands`] (`checkpoint.restore`,
//!   `file.delete`) and the shared [`Agent`] state;
//! * [`guard`]: [`guard::guarded`], the agent policy in front of a handle
//!   whose engine applies another policy (the editor's);
//! * [`init`]: `real agent init`: [`generate_agents_md`], `CLAUDE.md`, the
//!   default policy and each MCP client's configuration;
//! * [`lock`]: the single-writer project lock (`.real/editor.lock`), used by
//!   every writer (`real do`, `real mcp`, `real editor`); code `RE8003`;
//! * [`resources`]: the explain pages, engine documents and Luau definitions
//!   as read-only resources, with `resource.list` and `resource.read`;
//! * [`luau_tools`]: commands defined by Luau scripts in the `tools` context
//!   (`scripts/tools/commands/`), capped at `edit`;
//! * [`compat`]: `compat.coverage` (the Roblox-style coverage table) and
//!   `scene.dump_tree` (the instance tree with classes, attributes and tags);
//! * [`verify_diff`]: which verify results a change altered, for
//!   `verify.diff`;
//! * [`remote`]: the editor socket (`.real/editor.sock`): the protocol, the
//!   server loop the editor hosts and the client that other writers use.
//!
//! `lock` and `remote` are frozen contracts from WP7's first PR
//! (docs/M1_PLAN.md, "Cross-WP contracts").
//!
//! This is a tooling crate: `re_cli` links it only with its `tooling` feature,
//! and it never depends on tokio.

#![warn(missing_docs)]

pub mod codes;
pub mod commands;
pub mod compat;
mod glob;
pub mod guard;
pub mod init;
pub mod lock;
pub mod luau_tools;
pub mod policy;
pub mod remote;
pub mod resources;
pub mod session;
pub mod verify_diff;

pub use commands::{Agent, register_commands};
pub use init::generate_agents_md;
pub use lock::{Holder, LockState, ProjectLock, acquire, probe};
pub use policy::Policy;
pub use remote::{Writer, open_writer};
pub use session::{Checkpoint, Session};

use re_core::DiagInfo;

/// Every diagnostic code `re_agent` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::TIER_REFUSED,
        title: "Tier refused by the agent policy",
        page: include_str!("../explain/RE8001.md"),
    },
    DiagInfo {
        code: codes::OUTSIDE_SCOPE,
        title: "Path outside the agent policy's scopes",
        page: include_str!("../explain/RE8002.md"),
    },
    DiagInfo {
        code: codes::PROJECT_LOCKED,
        title: "Project locked by another writer",
        page: include_str!("../explain/RE8003.md"),
    },
    DiagInfo {
        code: codes::SOCKET_UNSUPPORTED,
        title: "Editor socket not available on this platform",
        page: include_str!("../explain/RE8004.md"),
    },
    DiagInfo {
        code: codes::SOCKET_IO,
        title: "Editor socket connection failed",
        page: include_str!("../explain/RE8005.md"),
    },
    DiagInfo {
        code: codes::SOCKET_PROTOCOL,
        title: "Invalid editor socket message",
        page: include_str!("../explain/RE8006.md"),
    },
    DiagInfo {
        code: codes::LOCK_IO,
        title: "Project lock file error",
        page: include_str!("../explain/RE8007.md"),
    },
    DiagInfo {
        code: codes::OVER_BUDGET,
        title: "Over the agent budget",
        page: include_str!("../explain/RE8008.md"),
    },
    DiagInfo {
        code: codes::INVALID_POLICY,
        title: "Invalid agent policy",
        page: include_str!("../explain/RE8009.md"),
    },
    DiagInfo {
        code: codes::CHECKPOINT_IO,
        title: "Checkpoint file error",
        page: include_str!("../explain/RE8010.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_CHECKPOINT,
        title: "Unknown session or checkpoint",
        page: include_str!("../explain/RE8011.md"),
    },
    DiagInfo {
        code: codes::NO_SESSION,
        title: "No agent session",
        page: include_str!("../explain/RE8012.md"),
    },
    DiagInfo {
        code: codes::FILE_ERROR,
        title: "File command failed",
        page: include_str!("../explain/RE8013.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_RESOURCE,
        title: "Unknown resource",
        page: include_str!("../explain/RE8014.md"),
    },
    DiagInfo {
        code: codes::INVALID_TOOL,
        title: "Invalid Luau tool",
        page: include_str!("../explain/RE8015.md"),
    },
    DiagInfo {
        code: codes::TOOL_FAILED,
        title: "Luau tool changed the world directly",
        page: include_str!("../explain/RE8016.md"),
    },
    DiagInfo {
        code: codes::TOOL_OUTPUT,
        title: "Luau tool output",
        page: include_str!("../explain/RE8017.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_COMPAT_NAME,
        title: "Unknown name in the coverage table",
        page: include_str!("../explain/RE8018.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 8000..=8099;
