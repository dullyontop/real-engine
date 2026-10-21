//! `re_mcp`: the Real Engine MCP server (ADR-0025, ADR-0026).
//!
//! It turns every registered command into one MCP tool and answers tool
//! calls through an [`EngineHandle`]: `re_cli` opens the project (or
//! connects to the editor) and hands the handle in, so this crate never
//! touches the world itself.
//!
//! * Tool names are command names with `_` for `.` (`entity.spawn` →
//!   `entity_spawn`), because several clients only accept
//!   `[A-Za-z0-9_-]` in tool names.
//! * Schema hygiene: the input schema's root is an object with no
//!   `anyOf`/`oneOf`/`allOf`; `format` and `$schema` keywords are removed;
//!   descriptions start with the command's summary and stay under 4096
//!   characters; all four annotation hints are set from the tier.
//! * Every tool has an `outputSchema`: the outcome envelope
//!   `{ ok, changes, diagnostics, data }`, with `data` described by the
//!   command's own output schema when it has one. A successful call returns
//!   that object as `structuredContent` too, and as JSON text for clients
//!   that read only text.
//! * Resources: the [`ResourceSet`] in [`McpOptions`] (explain pages, engine
//!   documents, Luau definitions) is served by `resources/list` and
//!   `resources/read`. The same items are reachable through the
//!   `resource.list` and `resource.read` tools.
//! * A failed command is an `isError` result whose text is JSON:
//!   `{ "code", "message", "location", "hint", "explain", ... }`. JSON-RPC
//!   errors are only for protocol problems (an unknown tool is `-32602`).
//! * A result whose data names a `.png` file (`screenshot`, `play.run`) also
//!   carries the image as image content; the path stays in the text, for
//!   clients that don't show images.
//! * Protocol revision 2025-11-25 is the baseline; 2026-07-28 (stateless,
//!   `server/discover`) is accepted. rmcp serves both on one connection.
//! * Two transports serve the same server: stdio ([`serve_stdio`]) and
//!   streamable HTTP on a loopback address with a bearer token
//!   ([`serve_http`], ADR-0046).
//!
//! Codes `RE8500`–`RE8599`.

#![warn(missing_docs)]

pub mod client;
pub mod codes;
pub mod http;
pub mod record;
mod server;

use std::path::PathBuf;

use std::sync::Arc;

use re_agent::resources::ResourceSet;
use re_command::EngineHandle;
use re_core::DiagInfo;

pub use http::{HttpListener, serve_http, serve_http_async};
pub use server::{
    MAX_DESCRIPTION, RealEngineServer, command_name, error_json, output_schema, sanitize_schema,
    tool_for, tool_name,
};

/// What the server says about itself.
#[derive(Clone, Debug)]
pub struct McpOptions {
    /// The project root: relative image paths in results are read from here.
    pub project: PathBuf,
    /// The server name in `serverInfo` (`real-engine`).
    pub name: String,
    /// The `instructions` sent at initialization (and by `server/discover`).
    pub instructions: String,
    /// The resources served (none by default).
    pub resources: Arc<ResourceSet>,
    /// Where each tool call is recorded (`real mcp --record`), if anywhere.
    pub recorder: Option<Arc<record::Recorder>>,
}

impl McpOptions {
    /// The default name and instructions for `project`.
    pub fn new(project: impl Into<PathBuf>) -> McpOptions {
        McpOptions {
            project: project.into(),
            name: re_agent::init::SERVER_NAME.to_string(),
            instructions: DEFAULT_INSTRUCTIONS.to_string(),
            resources: Arc::new(ResourceSet::new()),
            recorder: None,
        }
    }

    /// The same options, serving `resources`.
    pub fn with_resources(mut self, resources: Arc<ResourceSet>) -> McpOptions {
        self.resources = resources;
        self
    }

    /// The same options, recording every tool call with `recorder`.
    pub fn with_recorder(mut self, recorder: Arc<record::Recorder>) -> McpOptions {
        self.recorder = Some(recorder);
        self
    }
}

/// The default server instructions (kept short: clients truncate them).
pub const DEFAULT_INSTRUCTIONS: &str = "Real Engine project tools. Call `describe` first to see components, generators and commands. \
Every tool that changes the project accepts `dry_run: true` to preview its changes; dry runs are allowed at any tier. \
Use `scene_apply` for multi-step edits (one atomic, undoable transaction), `verify` and `screenshot` to check your work, \
`verify_diff` to see what a change altered since the last checkpoint, and `undo` or `checkpoint_restore` to go back. \
Explain pages, the engine docs, engine.d.luau and roblox.d.luau are resources (or `resource_list` and `resource_read`). Wait for a call's result before a call that depends on it. Refused calls (RE8001 tier, RE8002 path, RE8008 budget) changed nothing; \
the project's .real/agent-policy.toml decides what agents may do. AGENTS.md in the project explains the operation JSON.";

/// Serves MCP over stdin and stdout until the client closes stdin. Runs its
/// own tokio runtime, so call it from a plain thread. Logging must go to
/// stderr: stdout carries JSON-RPC.
pub fn serve_stdio(handle: EngineHandle, options: McpOptions) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(server::serve(
        RealEngineServer::new(handle, options),
        (tokio::io::stdin(), tokio::io::stdout()),
    ))
}

/// Serves MCP on any byte stream pair (tests use an in-memory duplex). Must
/// run inside a multi-threaded tokio runtime.
pub async fn serve_io<R, W>(
    handle: EngineHandle,
    options: McpOptions,
    reader: R,
    writer: W,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    server::serve(RealEngineServer::new(handle, options), (reader, writer)).await
}

/// Every diagnostic code `re_mcp` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::IMAGE_UNREADABLE,
        title: "Result image unreadable",
        page: include_str!("../explain/RE8501.md"),
    },
    DiagInfo {
        code: codes::NOT_LOOPBACK,
        title: "HTTP address is not loopback",
        page: include_str!("../explain/RE8502.md"),
    },
    DiagInfo {
        code: codes::WEAK_TOKEN,
        title: "HTTP token too short",
        page: include_str!("../explain/RE8503.md"),
    },
    DiagInfo {
        code: codes::BIND_FAILED,
        title: "Cannot listen on the HTTP address",
        page: include_str!("../explain/RE8504.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 8500..=8599;
