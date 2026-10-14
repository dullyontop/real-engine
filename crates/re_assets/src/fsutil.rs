//! Small file helpers shared by the sidecar, cache and material code.

use std::io;
use std::path::Path;

use re_core::{Diagnostic, Location};

use crate::codes;

/// Writes `bytes` to `path` through a temporary file in the same directory and a
/// rename, so readers never see half a file. Creates missing directories.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A path with `/` separators, for messages.
pub(crate) fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Reads a file, as [`codes::READ_FAILED`] on error. `shown` is the path used in
/// the diagnostic (project-relative where possible).
pub(crate) fn read(path: &Path, shown: &str) -> Result<Vec<u8>, Diagnostic> {
    std::fs::read(path).map_err(|e| {
        Diagnostic::error(codes::READ_FAILED, format!("cannot read `{shown}`: {e}"))
            .at(Location::file(shown))
    })
}

/// A write failure as [`codes::WRITE_FAILED`].
pub(crate) fn write_failed(shown: &str, e: &io::Error) -> Diagnostic {
    Diagnostic::error(codes::WRITE_FAILED, format!("cannot write `{shown}`: {e}"))
        .at(Location::file(shown))
}

/// The 1-based line and column of the first occurrence of `needle` in `text`.
pub(crate) fn find_line(text: &str, needle: &str) -> Option<(u32, u32)> {
    let at = text.find(needle)?;
    let before = &text[..at];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    Some((line as u32, column as u32))
}
