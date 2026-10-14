//! Script errors: what failed, where, and the Luau traceback, mapped to `RE3xxx`
//! [`Diagnostic`]s (ARCHITECTURE §7).
//!
//! mlua wraps errors in layers (callback errors, context, "runtime error:"
//! prefixes, an appended "stack traceback:"). [`ScriptError::from_mlua`] unwraps
//! them down to the Luau message, removes every "runtime error: " prefix, and
//! splits `chunk:line: message`. When the message carries no location (errors
//! raised from Rust, budget timeouts at the top of a chunk), the line of the first
//! Luau frame of the traceback is used instead.
//!
//! Engine functions raise errors whose message starts with a bracketed code, such
//! as `[RE1101] ...`; the diagnostic then carries that code instead of the generic
//! [`RUNTIME`](crate::codes::RUNTIME).

use std::fmt;

use mlua::{Function, Lua, Thread};
use re_core::{Code, Diagnostic, Location};

use crate::codes;

/// Registry key of `debug.traceback`, saved before the sandbox freezes globals.
pub(crate) const TRACEBACK_FN: &str = "re_script.debug_traceback";

/// The message the CPU budget raises.
pub const TIMEOUT_MESSAGE: &str = "script exceeded its time budget";

/// What kind of failure a [`ScriptError`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The source did not compile.
    Syntax,
    /// An error raised while running.
    Runtime,
    /// An allocation failed against the context's memory limit.
    Memory,
    /// The CPU budget stopped the script.
    Timeout,
}

/// A script failure with its location and traceback.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptError {
    /// What kind of failure.
    pub kind: ErrorKind,
    /// The diagnostic code: from the kind, or the `[RE####]` code an engine
    /// function put at the start of its message.
    pub code: Code,
    /// What was running: a script path, `"defer#3"`, `"signal jump"`, ...
    pub context: String,
    /// The chunk the error points at, such as `scripts/game/player.luau`
    /// (without mlua's leading `@` or `=`).
    pub chunk: Option<String>,
    /// The 1-based line in `chunk`.
    pub line: Option<u32>,
    /// The message, without location, code or "runtime error" prefixes.
    pub message: String,
    /// The Luau traceback (may be empty).
    pub traceback: String,
}

impl ScriptError {
    /// Builds an error from an mlua error raised while running `context`.
    pub fn from_mlua(context: &str, err: &mlua::Error) -> ScriptError {
        let (kind, raw, traceback) = flatten(err);
        let raw = strip_runtime_prefixes(&raw);
        let (mut chunk, mut line, message) = split_location(&raw);
        let (code, message) = split_code(&message);
        let kind = if message.contains(TIMEOUT_MESSAGE) {
            ErrorKind::Timeout
        } else {
            kind
        };
        if line.is_none()
            && let Some((c, l)) = first_luau_frame(&traceback)
        {
            chunk = Some(c);
            line = Some(l);
        }
        let code = code.unwrap_or(match kind {
            ErrorKind::Syntax => codes::SYNTAX,
            ErrorKind::Runtime => codes::RUNTIME,
            ErrorKind::Memory => codes::MEMORY,
            ErrorKind::Timeout => codes::TIMEOUT,
        });
        ScriptError {
            kind,
            code,
            context: context.to_string(),
            chunk,
            line,
            message,
            traceback,
        }
    }

    /// Builds an error raised inside a script thread. A dead coroutine keeps its
    /// stack, so `debug.traceback(thread)` still shows where it failed.
    pub fn from_thread(
        lua: &Lua,
        thread: &Thread,
        context: &str,
        err: &mlua::Error,
    ) -> ScriptError {
        let mut e = ScriptError::from_mlua(context, err);
        if let Ok(tb) = lua.named_registry_value::<Function>(TRACEBACK_FN)
            && let Ok(s) = tb.call::<String>(thread)
        {
            let s = s.trim_end().to_string();
            if e.line.is_none()
                && let Some((c, l)) = first_luau_frame(&s)
            {
                e.chunk = Some(c);
                e.line = Some(l);
            }
            if !s.is_empty() {
                e.traceback = s;
            }
        }
        e
    }

    /// The error as a [`Diagnostic`]: the code, the message, the file and line,
    /// and the traceback and context as a hint.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let mut d = Diagnostic::error(self.code, self.message.clone());
        if let Some(chunk) = &self.chunk {
            d = d.at(Location::File {
                file: chunk.clone(),
                line: self.line,
                column: None,
            });
        }
        let mut hint = format!("while running {}", self.context);
        if !self.traceback.is_empty() {
            hint.push('\n');
            hint.push_str(&self.traceback);
        }
        d.with_hint(hint)
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] ", self.code)?;
        match (&self.chunk, self.line) {
            (Some(c), Some(l)) => write!(f, "{c}:{l}: ")?,
            (Some(c), None) => write!(f, "{c}: ")?,
            _ => {}
        }
        write!(f, "{} (in {})", self.message, self.context)?;
        if !self.traceback.is_empty() {
            write!(f, "\n{}", self.traceback)?;
        }
        Ok(())
    }
}

impl std::error::Error for ScriptError {}

impl From<ScriptError> for Diagnostic {
    fn from(e: ScriptError) -> Diagnostic {
        e.to_diagnostic()
    }
}

/// Unwraps mlua's wrappers down to the Luau message, keeping any traceback.
fn flatten(err: &mlua::Error) -> (ErrorKind, String, String) {
    match err {
        mlua::Error::SyntaxError { message, .. } => {
            (ErrorKind::Syntax, message.clone(), String::new())
        }
        mlua::Error::RuntimeError(m) => match m.split_once("\nstack traceback:\n") {
            Some((msg, tb)) => (ErrorKind::Runtime, msg.to_string(), tb.to_string()),
            None => (ErrorKind::Runtime, m.clone(), String::new()),
        },
        mlua::Error::MemoryError(m) => (ErrorKind::Memory, m.clone(), String::new()),
        mlua::Error::CallbackError { traceback, cause } => {
            let (k, m, tb) = flatten(cause);
            let tb = if tb.is_empty() {
                traceback
                    .strip_prefix("stack traceback:\n")
                    .unwrap_or(traceback)
                    .to_string()
            } else {
                tb
            };
            (k, m, tb)
        }
        mlua::Error::WithContext { context, cause } => {
            let (k, m, tb) = flatten(cause);
            (k, format!("{context}: {m}"), tb)
        }
        other => (ErrorKind::Runtime, other.to_string(), String::new()),
    }
}

/// Removes mlua's leading "runtime error: " prefixes (one per wrapping layer).
/// Text further into the message is the script's own and is kept.
fn strip_runtime_prefixes(s: &str) -> String {
    let mut out = s.trim_start();
    while let Some(rest) = out.strip_prefix("runtime error: ") {
        out = rest.trim_start();
    }
    out.to_string()
}

/// Finds `chunk:line:` at the start of `text`: the first `:` that is directly
/// followed by digits and another `:`. Chunk names may contain spaces and drive
/// letters (`C:/x.luau`), while a message such as `bad argument #1: 3: x` (a space
/// after the colon) is not mistaken for a location. Returns the chunk (without
/// mlua's leading `@` or `=`), the line and the rest after the second colon.
fn find_location(text: &str) -> Option<(&str, u32, &str)> {
    let bytes = text.as_bytes();
    for (i, _) in text.match_indices(':') {
        let digits = bytes[i + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits == 0 || bytes.get(i + 1 + digits) != Some(&b':') {
            continue;
        }
        let line = text[i + 1..i + 1 + digits].parse::<u32>().ok()?;
        let chunk = text[..i].trim_start_matches(['@', '=']);
        return Some((chunk, line, &text[i + 2 + digits..]));
    }
    None
}

/// Splits `scripts/a.luau:12: boom` into chunk, line and message.
fn split_location(raw: &str) -> (Option<String>, Option<u32>, String) {
    let (first, rest) = match raw.split_once('\n') {
        Some((f, r)) => (f, Some(r)),
        None => (raw, None),
    };
    if let Some((chunk, line, msg)) = find_location(first) {
        let mut message = msg.trim().to_string();
        if let Some(rest) = rest {
            message.push('\n');
            message.push_str(rest);
        }
        let chunk = (!chunk.is_empty()).then(|| chunk.to_string());
        return (chunk, Some(line), message);
    }
    (None, None, raw.to_string())
}

/// Takes a leading `[RE####] ` off a message.
fn split_code(message: &str) -> (Option<Code>, String) {
    if let Some(rest) = message.strip_prefix('[')
        && let Some((code, msg)) = rest.split_once(']')
        && let Some(code) = Code::parse(code)
    {
        return (Some(code), msg.trim_start().to_string());
    }
    (None, message.to_string())
}

/// The chunk and line of the first Luau frame in a traceback, skipping `[C]` frames:
/// `\tscripts/a.luau:2: in function 'f'` gives `("scripts/a.luau", 2)`.
pub(crate) fn first_luau_frame(traceback: &str) -> Option<(String, u32)> {
    traceback.lines().find_map(|l| {
        let l = l.trim();
        if l.starts_with('[') {
            return None;
        }
        let (chunk, line, _) = find_location(l)?;
        (!chunk.is_empty()).then(|| (chunk.to_string(), line))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_error_prefix_is_removed_everywhere() {
        let e = mlua::Error::RuntimeError(
            "runtime error: runtime error: scripts/a.luau:3: boom".into(),
        );
        let s = ScriptError::from_mlua("t", &e);
        assert_eq!(s.message, "boom");
        assert_eq!(s.chunk.as_deref(), Some("scripts/a.luau"));
        assert_eq!(s.line, Some(3));
        assert_eq!(s.code, codes::RUNTIME);
    }

    #[test]
    // mlua's own `CallbackError` holds an `Arc<Error>`; this builds one the same way.
    #[allow(clippy::arc_with_non_send_sync)]
    fn embedded_code_is_taken_and_line_comes_from_the_traceback() {
        let e = mlua::Error::CallbackError {
            traceback: "stack traceback:\n\t[C]: in function 'f'\n\tscripts/b.luau:2: in function <scripts/b.luau:1>".into(),
            cause: std::sync::Arc::new(mlua::Error::RuntimeError(format!("[{}] unsupported API member: world.nope", codes::MEMORY))),
        };
        let s = ScriptError::from_mlua("t", &e);
        assert_eq!(s.code, codes::MEMORY);
        assert_eq!(s.message, "unsupported API member: world.nope");
        assert_eq!(s.chunk.as_deref(), Some("scripts/b.luau"));
        assert_eq!(s.line, Some(2));
        let d = s.to_diagnostic();
        assert!(
            d.render_human().contains("scripts/b.luau:2"),
            "{}",
            d.render_human()
        );
    }

    #[test]
    fn messages_with_colons_are_not_locations() {
        let (c, l, m) = split_location("bad argument #1: 3: x");
        assert_eq!((c, l), (None, None));
        assert_eq!(m, "bad argument #1: 3: x");
    }

    #[test]
    fn chunk_names_with_spaces_and_drive_letters_keep_their_location() {
        let lua = mlua::Lua::new();
        let e = lua
            .load("local function f() error('boom') end\nf()")
            .set_name("@scripts/x y.luau")
            .exec()
            .unwrap_err();
        let s = ScriptError::from_mlua("t", &e);
        assert_eq!(s.chunk.as_deref(), Some("scripts/x y.luau"));
        assert_eq!(s.line, Some(1));
        assert_eq!(s.message, "boom");
        let (c, l, m) = split_location("C:/proj/a.luau:7: oops");
        assert_eq!(
            (c.as_deref(), l, m.as_str()),
            (Some("C:/proj/a.luau"), Some(7), "oops")
        );
    }

    #[test]
    fn script_text_mentioning_runtime_error_is_kept() {
        let e = mlua::Error::RuntimeError(
            "runtime error: scripts/a.luau:1: a: runtime error: b".into(),
        );
        let s = ScriptError::from_mlua("t", &e);
        assert_eq!(s.message, "a: runtime error: b");
    }
}
