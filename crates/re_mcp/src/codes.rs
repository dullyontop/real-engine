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
