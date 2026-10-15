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
