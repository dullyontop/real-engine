//! The diagnostic codes `re_mcp` owns (range `RE8500`–`RE8599`).

use re_core::Code;

/// A result named a PNG file that could not be read, so it was not attached
/// as image content (a warning; the path is still in the result).
pub const IMAGE_UNREADABLE: Code = Code::new("RE8501");
