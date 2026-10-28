//! The diagnostic codes `re_mcp` owns (range `RE8500`–`RE8599`).

use re_core::Code;

/// A result named a PNG file that could not be read, so it was not attached
/// as image content (a warning; the path is still in the result).
pub const IMAGE_UNREADABLE: Code = Code::new("RE8501");

/// `real mcp --http` was given an address that is not loopback (or not an
/// address at all). The HTTP transport has no TLS, so it serves this machine
/// only (ADR-0046).
pub const NOT_LOOPBACK: Code = Code::new("RE8502");

/// The token set in `REAL_MCP_TOKEN` is too short or has characters an HTTP
/// header cannot carry.
pub const WEAK_TOKEN: Code = Code::new("RE8503");

/// `real mcp --http` could not listen on its address.
pub const BIND_FAILED: Code = Code::new("RE8504");

/// `real mcp --record FILE` cannot create or write the transcript: an error
/// when the file cannot be created at the start, a warning when a later line
/// cannot be written (the call itself already happened).
pub const RECORD_FAILED: Code = Code::new("RE8505");

/// `real agent init --http ADDR` was given port 0. A client config names the
/// server's URL, so the port must be fixed.
pub const PORT_NOT_FIXED: Code = Code::new("RE8506");

/// The thread serving MCP stopped with a panic (a bug in the engine; the
/// crash report is in `.real/crash/`).
pub const SERVER_PANICKED: Code = Code::new("RE8507");
