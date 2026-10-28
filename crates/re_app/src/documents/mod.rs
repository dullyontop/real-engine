//! Documents made and edited by commands (ADR-0064): materials
//! ([`material`]) and `real.toml` ([`config`]).
//!
//! Each command is one transaction of `set_doc` operations on its document,
//! so one undo step. The command opens the document when it is not open:
//! from its file, keeping the file's text so that undoing every edit writes
//! it back byte for byte, or absent when the command makes the file, so that
//! undo empties it again and the next save removes the file. The document's
//! kind checks every result ([`register_kinds`]); a refused one is `RE2008`,
//! and nothing changes. A dry run reports the document as it would be. The
//! project save writes the files.
//!
//! The module is built only off the web: the browser runtime never edits.

pub mod config;
pub mod material;

use re_command::{Call, CommandRegistry, Op, codes as command_codes};
use re_core::{Diagnostic, Location};
use re_scene::{DocKind, World};
use serde_json::Value as Json;

use crate::app::App;
use crate::codes;

/// Registers the document commands.
pub fn register(registry: &mut CommandRegistry<App>) {
    material::register(registry);
    config::register(registry);
}

/// Registers the kinds of the documents these commands make and edit on
/// `world`: how each is checked and written (idempotent).
pub fn register_kinds(world: &mut World) {
    world.register_doc_kind(
        material::SUFFIX,
        DocKind {
            validate: material::validate,
            write: scene_style,
        },
    );
    world.register_doc_kind(
        crate::config::CONFIG_FILE,
        DocKind {
            validate: config::validate,
            write: config::write,
        },
    );
}

/// The writer of materials, prefabs and scenes: the scene file style, with
/// lists of numbers on one line.
fn scene_style(value: &Json, _base: Option<&str>) -> Result<String, String> {
    Ok(re_scene::scene::to_pretty_json(value))
}

/// `path` as a project path ending with `suffix`, or `RE2052`.
fn kind_path(path: &str, suffix: &str, what: &str) -> Result<String, Diagnostic> {
    let rel = crate::commands::project_path(path, what, command_codes::DOCUMENT_PATH)?;
    if !rel.ends_with(suffix) || rel.len() == suffix.len() || rel.ends_with(&format!("/{suffix}")) {
        return Err(Diagnostic::error(
            command_codes::DOCUMENT_PATH,
            format!("`{rel}` is not {what} path: its name ends with `{suffix}`"),
        )
        .at(Location::file(rel.clone())));
    }
    Ok(rel)
}

/// The text of a document's file is turned into its content by this.
type Parse = fn(&str) -> Result<Json, String>;

/// Parses a JSON document's text.
fn parse_json(text: &str) -> Result<Json, String> {
    serde_json::from_str(text).map_err(|e| format!("it is not JSON: {e}"))
}

/// Opens document `path` from its file, read with `parse`, unless it is
/// open. Returns true when this call opened it, so the caller closes it again
/// when no edit is made.
fn open_file(app: &mut App, path: &str, parse: Parse) -> Result<bool, Diagnostic> {
    register_kinds(app.world_mut());
    if let Some(doc) = app.world().doc(path) {
        if doc.is_absent() {
            return Err(missing(path));
        }
        return Ok(false);
    }
    if !app.files().is_file(path) {
        return Err(missing(path));
    }
    let text = app.files().read_str(path).map_err(|e| {
        Diagnostic::error(codes::ASSET_UNAVAILABLE, format!("cannot read {path}: {e}"))
            .at(Location::file(path))
    })?;
    let value = parse(&text).map_err(|e| invalid(path, &e))?;
    app.world_mut()
        .open_doc_text(path, value, text)
        .map_err(|e| invalid(path, &e.to_string()))?;
    Ok(true)
}

/// Opens document `path` absent, to make its file: refused with `RE2051`
/// when the file exists or the document is open with content. Returns true
/// when this call opened it.
fn open_new(app: &mut App, path: &str) -> Result<bool, Diagnostic> {
    register_kinds(app.world_mut());
    if let Some(doc) = app.world().doc(path) {
        if doc.is_absent() {
            return Ok(false);
        }
        return Err(exists(path, "it is open with unsaved content"));
    }
    if app.files().is_file(path) {
        return Err(exists(path, "the file is there"));
    }
    if app.files().project_dir().is_none() {
        return Err(Diagnostic::error(
            codes::PACKAGED_WRITE,
            format!("cannot make {path}: this run has no project directory"),
        ));
    }
    app.world_mut()
        .open_doc(path, Json::Null)
        .map_err(|e| invalid(path, &e.to_string()))?;
    Ok(true)
}

fn missing(path: &str) -> Diagnostic {
    Diagnostic::error(codes::ASSET_UNAVAILABLE, format!("{path} does not exist"))
        .at(Location::file(path))
}

fn exists(path: &str, why: &str) -> Diagnostic {
    Diagnostic::error(
        command_codes::DOCUMENT_EXISTS,
        format!("{path} exists already ({why})"),
    )
    .at(Location::file(path))
}

fn invalid(path: &str, why: &str) -> Diagnostic {
    Diagnostic::error(
        command_codes::INVALID_DOCUMENT,
        format!("{path} is not valid: {why}"),
    )
    .at(Location::file(path))
    .with_hint("fix the file by hand, or undo the edit that broke it")
}

/// Runs `ops` on document `path` as one transaction labelled `label`, when
/// there are any. A document `opened` by this call is closed again unless the
/// edit was made, so a dry run, a refusal or an edit that changes nothing
/// leaves the World's documents as they were. Returns whether anything
/// changes.
fn transact(
    app: &mut App,
    call: &mut Call,
    path: &str,
    opened: bool,
    label: String,
    ops: Vec<Op>,
) -> Result<bool, Diagnostic> {
    let changes = !ops.is_empty();
    let result = if changes {
        call.transact(app, label, ops).map(|_| ())
    } else {
        Ok(())
    };
    let made = result.is_ok() && changes && !call.dry_run();
    if opened && !made {
        app.world_mut().close_doc(path);
    }
    result.map_err(|d| {
        let d = refusal(d);
        if d.location.is_some() {
            d
        } else {
            d.at(Location::file(path))
        }
    })?;
    Ok(changes)
}

/// A transaction the document's kind refused is that refusal (`RE2008`, or
/// `RE2050`), not the rejected transaction's `RE2001` around it.
fn refusal(d: Diagnostic) -> Diagnostic {
    let kinds = [
        command_codes::INVALID_DOCUMENT,
        command_codes::DOCUMENT_FILE_KEPT,
    ];
    match d.related.as_slice() {
        [inner] if d.code == command_codes::TRANSACTION_REJECTED && kinds.contains(&inner.code) => {
            inner.clone()
        }
        _ => d,
    }
}

/// One RFC 6901 pointer token.
fn token(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}
