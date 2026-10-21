//! Small file helpers shared by the sidecar, cache and material code.

use std::io;
use std::path::Path;

use re_core::files::ProjectFiles;
use re_core::{Diagnostic, Location};

use crate::codes;

pub(crate) use re_core::files::write_atomic;

/// A path with `/` separators, for messages.
pub(crate) fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Reads the source of project file `rel` through `files`, as
/// [`codes::READ_FAILED`] on error.
pub(crate) fn read_in(files: &ProjectFiles, rel: &str) -> Result<std::sync::Arc<[u8]>, Diagnostic> {
    files.read_source(rel).map_err(|e| {
        Diagnostic::error(codes::READ_FAILED, format!("cannot read `{rel}`: {e}"))
            .at(Location::file(rel))
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
