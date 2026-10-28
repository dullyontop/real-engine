//! The tool-call transcript (ADR-0046): one JSON line per `tools/call`,
//! written by the server, so it has the same shape whatever the client.
//! `real mcp --record FILE` and every `real agent eval` run write one.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use re_core::Diagnostic;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::codes;

/// One tool call in a transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    /// Milliseconds from the server's start to the call.
    pub t_ms: u64,
    /// The MCP tool name.
    pub tool: String,
    /// The command it ran (`None` for an unknown tool).
    pub command: Option<String>,
    /// The arguments, as sent.
    pub arguments: Json,
    /// Whether the command succeeded.
    pub ok: bool,
    /// The first error's code, when it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Whether the result was a tool error (`isError`) or a protocol error.
    pub is_error: bool,
    /// How long the call took, in milliseconds.
    pub duration_ms: u64,
}

/// Writes a transcript, one flushed line per call, so a killed server
/// leaves every call it answered.
#[derive(Debug)]
pub struct Recorder {
    file: Mutex<std::fs::File>,
    start: Instant,
}

impl Recorder {
    /// Opens the transcript at `path` for appending, creating it if
    /// needed. Appending, not truncating, so a second server that is
    /// refused the project lock cannot cut the first one's transcript.
    ///
    /// # Errors
    ///
    /// When the file cannot be created.
    pub fn create(path: &Path) -> std::io::Result<Recorder> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Recorder {
            file: Mutex::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?,
            ),
            start: Instant::now(),
        })
    }

    /// The time since the server started, for [`TranscriptEntry::t_ms`].
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Appends `entry`. A write that fails is reported on stderr once per
    /// call, as an `RE8505` warning, and otherwise ignored: the call itself
    /// already happened.
    pub fn record(&self, entry: &TranscriptEntry) {
        let mut line = serde_json::to_string(entry).unwrap_or_default();
        line.push('\n');
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = file.write_all(line.as_bytes()).and_then(|()| file.flush()) {
            let warning = Diagnostic::warning(
                codes::RECORD_FAILED,
                format!("could not write a call to the transcript: {e}"),
            );
            eprint!("{}", warning.render_human());
        }
    }
}

/// Reads a transcript written by [`Recorder`]; lines that do not parse
/// (a write cut off by a kill) are skipped.
///
/// # Errors
///
/// When the file cannot be read.
pub fn read_transcript(path: &Path) -> std::io::Result<Vec<TranscriptEntry>> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// Milliseconds in `d`, saturating.
pub(crate) fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transcript_reads_back_and_skips_a_cut_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t/transcript.jsonl");
        let rec = Recorder::create(&path).unwrap();
        let entry = TranscriptEntry {
            t_ms: 3,
            tool: "entity_spawn".into(),
            command: Some("entity.spawn".into()),
            arguments: serde_json::json!({ "name": "A" }),
            ok: false,
            code: Some("RE8502".into()),
            is_error: true,
            duration_ms: 1,
        };
        rec.record(&entry);
        rec.record(&entry);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"t_ms\":")
            .unwrap();
        assert_eq!(read_transcript(&path).unwrap(), vec![entry.clone(), entry]);
    }
}
