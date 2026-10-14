//! Diagnostics: the one user-facing error shape (ADR-0004, ARCHITECTURE §7).
//!
//! Library crates use their own error enums internally and convert them to a
//! [`Diagnostic`] at their public boundary. A diagnostic carries a stable
//! [`Code`] (`RE0001` to `RE9999`), a [`Severity`], a message, an optional
//! [`Location`] and hint, and a list of related diagnostics.
//!
//! Output comes in two forms with the same data:
//! * [`Diagnostic::render_human`], which looks like rustc
//!   (`error[RE3004]: ...` then `  --> scripts/game/player.luau:12:5`);
//! * [`Diagnostic::render_json`], one JSON object per line.

use std::fmt;
use std::ops::RangeInclusive;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A stable diagnostic code such as `RE1001`.
///
/// Codes are written as string literals in source (`Code::new("RE1001")`) so that
/// `cargo xtask explain` can find them and check that each one has an explain
/// page, lies in its crate's range and is used by one crate only. `Code::new` is a
/// `const fn`: a malformed literal in a `const` is a compile error.
///
/// Serialized as its text form (`"RE1001"`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Code(u16);

impl Code {
    /// Parses a code literal of the form `RE` followed by exactly four digits.
    ///
    /// # Panics
    ///
    /// Panics if `code` is not of that form. In a `const` context the panic is a
    /// compile-time error.
    pub const fn new(code: &'static str) -> Code {
        match Self::parse_bytes(code.as_bytes()) {
            Some(code) => code,
            None => panic!("a diagnostic code is `RE` followed by four digits"),
        }
    }

    /// Parses `RE####`; returns `None` for anything else.
    pub fn parse(text: &str) -> Option<Code> {
        Self::parse_bytes(text.as_bytes())
    }

    /// Builds a code from its number (`1001` gives `RE1001`), if it is at most 9999.
    pub const fn from_number(number: u16) -> Option<Code> {
        if number <= 9999 {
            Some(Code(number))
        } else {
            None
        }
    }

    /// The numeric part of the code (`RE1001` gives `1001`).
    pub const fn number(self) -> u16 {
        self.0
    }

    const fn parse_bytes(b: &[u8]) -> Option<Code> {
        if b.len() != 6 || b[0] != b'R' || b[1] != b'E' {
            return None;
        }
        let mut n: u16 = 0;
        let mut i = 2;
        while i < 6 {
            let d = b[i];
            if !d.is_ascii_digit() {
                return None;
            }
            n = n * 10 + (d - b'0') as u16;
            i += 1;
        }
        Some(Code(n))
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RE{:04}", self.0)
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Code(RE{:04})", self.0)
    }
}

impl Serialize for Code {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Code {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Code::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format_args!(
                "invalid diagnostic code `{text}` (expected `RE` followed by four digits)"
            ))
        })
    }
}

/// How serious a [`Diagnostic`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The operation failed.
    Error,
    /// The operation succeeded, but something is probably wrong.
    Warning,
    /// Extra information, usually attached to another diagnostic as `related`.
    Note,
}

impl Severity {
    /// The lowercase name used in human and JSON output (`error`, `warning`, `note`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a [`Diagnostic`] points: a place in a file, an entity or a command.
///
/// Serialized with a `kind` tag: `{"kind": "file", "file": "...", "line": 12, "column": 5}`,
/// `{"kind": "entity", "entity": "e7kq2m9x4tb"}` or `{"kind": "command", "command": "entity.set"}`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Location {
    /// A file, optionally with a 1-based line and column.
    File {
        /// The path, relative to the project root where possible, with `/` separators.
        file: String,
        /// 1-based line number.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        /// 1-based column number (only meaningful with a line).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column: Option<u32>,
    },
    /// An entity, by its stable id (the `id` of the scene file).
    Entity {
        /// The entity's stable id.
        entity: String,
    },
    /// A command of the command registry, by name (for example `entity.set`).
    Command {
        /// The command name.
        command: String,
    },
}

impl Location {
    /// A whole file.
    pub fn file(file: impl Into<String>) -> Location {
        Location::File {
            file: file.into(),
            line: None,
            column: None,
        }
    }

    /// A line and column in a file (both 1-based).
    pub fn file_at(file: impl Into<String>, line: u32, column: u32) -> Location {
        Location::File {
            file: file.into(),
            line: Some(line),
            column: Some(column),
        }
    }

    /// An entity, by stable id.
    pub fn entity(id: impl Into<String>) -> Location {
        Location::Entity { entity: id.into() }
    }

    /// A command, by name.
    pub fn command(name: impl Into<String>) -> Location {
        Location::Command {
            command: name.into(),
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::File { file, line, column } => {
                f.write_str(file)?;
                if let Some(line) = line {
                    write!(f, ":{line}")?;
                    if let Some(column) = column {
                        write!(f, ":{column}")?;
                    }
                }
                Ok(())
            }
            Location::Entity { entity } => write!(f, "entity `{entity}`"),
            Location::Command { command } => write!(f, "command `{command}`"),
        }
    }
}

/// A user-facing problem: the one error type of Real Engine (ADR-0004).
///
/// Optional parts are left out of the JSON form when empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    /// The stable code, from the owning crate's range (ARCHITECTURE §7).
    pub code: Code,
    /// How serious it is.
    pub severity: Severity,
    /// One sentence saying what is wrong.
    pub message: String,
    /// Where it is, if there is one place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    /// How to fix it, if there is a likely fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Further diagnostics that explain this one (for example every place of a duplicate).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Diagnostic>,
}

impl Diagnostic {
    /// A diagnostic with no location, hint or related diagnostics.
    pub fn new(code: Code, severity: Severity, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity,
            message: message.into(),
            location: None,
            hint: None,
            related: Vec::new(),
        }
    }

    /// An [`Severity::Error`] diagnostic.
    pub fn error(code: Code, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(code, Severity::Error, message)
    }

    /// A [`Severity::Warning`] diagnostic.
    pub fn warning(code: Code, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(code, Severity::Warning, message)
    }

    /// A [`Severity::Note`] diagnostic.
    pub fn note(code: Code, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(code, Severity::Note, message)
    }

    /// Sets the location.
    #[must_use]
    pub fn at(mut self, location: Location) -> Diagnostic {
        self.location = Some(location);
        self
    }

    /// Sets the hint.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Diagnostic {
        self.hint = Some(hint.into());
        self
    }

    /// Appends a related diagnostic.
    #[must_use]
    pub fn with_related(mut self, related: Diagnostic) -> Diagnostic {
        self.related.push(related);
        self
    }

    /// True for [`Severity::Error`].
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Renders the diagnostic like rustc, ending with a newline:
    ///
    /// ```text
    /// error[RE0001]: invalid log filter `info,=`
    ///   --> real.toml:12:5
    ///   = hint: write directives like `info,re_render=debug`
    /// ```
    ///
    /// Related diagnostics follow, indented by two spaces per level.
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        self.render_human_into(&mut out, 0);
        out
    }

    fn render_human_into(&self, out: &mut String, depth: usize) {
        use std::fmt::Write as _;
        let indent = "  ".repeat(depth);
        // Writing into a String cannot fail.
        let _ = writeln!(
            out,
            "{indent}{}[{}]: {}",
            self.severity, self.code, self.message
        );
        if let Some(location) = &self.location {
            let _ = writeln!(out, "{indent}  --> {location}");
        }
        if let Some(hint) = &self.hint {
            let _ = writeln!(out, "{indent}  = hint: {hint}");
        }
        for related in &self.related {
            related.render_human_into(out, depth + 1);
        }
    }

    /// Renders the diagnostic as one line of JSON (no trailing newline).
    pub fn render_json(&self) -> String {
        serde_json::to_string(self).expect("a Diagnostic always serializes")
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]: {}", self.severity, self.code, self.message)?;
        if let Some(location) = &self.location {
            write!(f, " (at {location})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostic {}

/// Renders several diagnostics in human form, one after the other.
pub fn render_human_all(diagnostics: &[Diagnostic]) -> String {
    diagnostics.iter().map(Diagnostic::render_human).collect()
}

/// Renders several diagnostics as JSON lines (each line ends with a newline).
pub fn render_json_lines(diagnostics: &[Diagnostic]) -> String {
    let mut out = String::new();
    for diagnostic in diagnostics {
        out.push_str(&diagnostic.render_json());
        out.push('\n');
    }
    out
}

/// One entry of a crate's diagnostic catalogue: what `real explain RE####` shows.
///
/// Each crate exports `pub const DIAGNOSTICS: &[DiagInfo]`, with pages built by
/// `include_str!("../explain/RE####.md")`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagInfo {
    /// The code.
    pub code: Code,
    /// A short title, in sentence case without a final full stop.
    pub title: &'static str,
    /// The Markdown explain page.
    pub page: &'static str,
}

/// Checks a crate's catalogue: no duplicate codes, every code inside `range`,
/// a non-empty title, and a page whose first line is the `# RE####` heading.
///
/// Returns one message per problem (an empty list when the catalogue is valid).
/// Crates call it from a test.
pub fn validate_catalog(catalog: &[DiagInfo], range: RangeInclusive<u16>) -> Vec<String> {
    let mut problems = Vec::new();
    for (i, info) in catalog.iter().enumerate() {
        if !range.contains(&info.code.number()) {
            problems.push(format!(
                "{} is outside the crate's range RE{:04}-RE{:04}",
                info.code,
                range.start(),
                range.end()
            ));
        }
        if catalog[..i].iter().any(|other| other.code == info.code) {
            problems.push(format!("{} is listed twice", info.code));
        }
        if info.title.trim().is_empty() {
            problems.push(format!("{} has an empty title", info.code));
        }
        let heading = format!("# {}", info.code);
        if !info
            .page
            .lines()
            .next()
            .is_some_and(|l| l.starts_with(&heading))
        {
            problems.push(format!(
                "the explain page of {} does not start with `{heading}`",
                info.code
            ));
        }
    }
    problems
}
