//! The diagnostic codes `re_script_check` owns: `RE3080`–`RE3099` (ARCHITECTURE
//! §7). Each has an explain page in `crates/re_script_check/explain/` and an entry
//! in [`DIAGNOSTICS`](crate::DIAGNOSTICS).

use re_core::Code;

/// The static scan could not parse (or read) a file.
pub const SCAN_PARSE: Code = Code::new("RE3080");

/// The type checker reported an error.
pub const TYPE_ERROR: Code = Code::new("RE3081");

/// The type checker reported a warning (a lint).
pub const TYPE_WARNING: Code = Code::new("RE3082");

/// A `^` or `^=` whose exponent is not 2, 0.5 or 3: it calls the platform's
/// `pow`, so the result can differ between platforms (a warning).
pub const PLATFORM_POW: Code = Code::new("RE3083");

/// No type checker was found.
pub const CHECKER_NOT_FOUND: Code = Code::new("RE3090");

/// The type checker failed to run or crashed.
pub const CHECKER_FAILED: Code = Code::new("RE3091");
