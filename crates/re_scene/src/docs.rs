//! Open documents (ADR-0044, ADR-0064): project files, such as
//! `graphs/*.graph.json`, loaded into the World as JSON and edited in memory
//! by `re_command`'s `set_doc` operation, so every edit is undoable and
//! agents see it. The ordinary project save writes them back.
//!
//! * [`OpenDocs`]: the World resource holding each open document by
//!   project-relative path ([`doc_path`]), with a revision counter (bumped on
//!   every change; a view rebuilds when it changes) and a dirty flag;
//! * kinds by file suffix ([`World::register_doc_kind`]): a validator (a
//!   transaction whose resulting document does not validate is rejected)
//!   and a writer, which turns a content into its file's text;
//! * absent documents: an open document whose value is `null` has no file.
//!   Giving it content makes the file at the next save, and emptying it
//!   again removes the file, so making a file is undoable. Only a document
//!   opened absent may be emptied ([`OpenDoc::opened_absent`]), so a save
//!   removes only files the session made;
//! * recorded texts: the text a document was opened with, and the texts it
//!   was saved as since (within [`SAVED_TEXT_BUDGET`]). The save writes the
//!   recorded text of a content again, so undo gives a file back byte for
//!   byte, whatever its formatting;
//! * the world check ([`World::set_world_check`]): a rule the whole World
//!   keeps after every transaction while it is set, such as one root and no
//!   `Procedural` while a prefab is the edited world.
//!
//! Open documents are not entities: they are not part of
//! [`World::state_hash`] or of the scene file.

use std::collections::BTreeMap;

use bevy_ecs::resource::Resource;
use serde_json::Value as Json;

use crate::error::SceneError;
use crate::world::World;

/// Checks a document of one kind (registered by file suffix). The message
/// says what is wrong. It is never called on an absent document's `null`.
pub type DocValidator = fn(&Json) -> Result<(), String>;

/// Writes a content of one kind as its file's text. `base` is the last text
/// read from or written to the file (`None` when there is none), which a
/// format-preserving writer merges the content into.
pub type DocWriter = fn(&Json, Option<&str>) -> Result<String, String>;

/// A rule the whole World keeps after every transaction while it is set
/// ([`World::set_world_check`]). The message says what broke.
pub type WorldCheck = fn(&World) -> Result<(), String>;

/// How the documents of one kind are checked and written.
#[derive(Clone, Copy, Debug)]
pub struct DocKind {
    /// Checks a content.
    pub validate: DocValidator,
    /// Writes a content as its file's text.
    pub write: DocWriter,
}

/// The writer of the documents whose kind registers no other: pretty JSON
/// ending with a newline, as graph files are written.
pub fn pretty_json(value: &Json, _base: Option<&str>) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

/// How many bytes of saved texts an open document keeps (ADR-0064), besides
/// the text it was opened with, which it always keeps, and the last text
/// written, which it keeps even when that alone is larger. The oldest go
/// first; a content with no recorded text is written by its kind's writer.
pub const SAVED_TEXT_BUDGET: usize = 1 << 20;

/// What the project save does with an open document's file
/// ([`World::doc_file`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocFile {
    /// Write this text: the one recorded for the content, or the kind's
    /// writer's output.
    Text(String),
    /// The document is absent: the save removes its file, which it does only
    /// for a document [opened absent](OpenDoc::opened_absent).
    Absent,
}

/// One open document.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenDoc {
    /// The document's content: `null` when the document is absent (it has no
    /// file).
    pub value: Json,
    /// Bumped on every change (opening counts); unique among the World's
    /// documents, so a view compares it to know when to rebuild.
    pub revision: u64,
    /// True when the document differs from what it was when it was opened
    /// or last saved (an undo back to that content clears it again).
    pub dirty: bool,
    /// Which opening of this path the document is: bumped every time
    /// [`World::open_doc`] opens or replaces the path, and never reused for
    /// it (closing does not reset it). Undo history recorded against one
    /// epoch does not apply to another.
    pub epoch: u64,
    /// The hash of the content when it was opened or last saved.
    saved: [u8; 32],
    /// True when the path had no file when the document was opened.
    opened_absent: bool,
    /// The texts the document's file held.
    texts: Texts,
}

impl OpenDoc {
    /// True if `value` is the content the document was opened with or last
    /// saved as: a file holding it was written by that save, not changed
    /// by someone else.
    pub fn matches_saved(&self, value: &Json) -> bool {
        content_hash(value) == self.saved
    }

    /// True when the document is absent: its value is `null`, so it has no
    /// file, and a save removes the file it had.
    pub fn is_absent(&self) -> bool {
        self.value.is_null()
    }

    /// True when the path had no file when the document was opened (also
    /// when a document was reopened absent after its file was removed). Only
    /// such a document may be emptied again, so a save removes only files
    /// the session made (ADR-0064).
    pub fn opened_absent(&self) -> bool {
        self.opened_absent
    }

    /// The text recorded for the document's current content: what its file
    /// held when it was opened, or what a save wrote for that content.
    pub fn recorded_text(&self) -> Option<&str> {
        self.texts.get(&content_hash(&self.value))
    }
}

/// The texts an open document's file held, each with its content's hash.
#[derive(Clone, Debug, Default, PartialEq)]
struct Texts {
    /// The text the document was opened with: always kept.
    opened: Option<([u8; 32], String)>,
    /// The texts saved since, oldest first, within [`SAVED_TEXT_BUDGET`].
    saved: Vec<([u8; 32], String)>,
    /// The content hash of the last text read or written, while the file
    /// has one.
    last: Option<[u8; 32]>,
}

impl Texts {
    fn opened(hash: [u8; 32], text: String) -> Texts {
        Texts {
            opened: Some((hash, text)),
            saved: Vec::new(),
            last: Some(hash),
        }
    }

    /// The recorded text of the content with `hash`, the newest first.
    fn get(&self, hash: &[u8; 32]) -> Option<&str> {
        self.saved
            .iter()
            .rev()
            .chain(&self.opened)
            .find(|(h, _)| h == hash)
            .map(|(_, t)| t.as_str())
    }

    /// The last text read or written: the base a writer merges into.
    fn base(&self) -> Option<&str> {
        self.last.and_then(|h| self.get(&h))
    }

    /// Records the text a save wrote for the content with `hash`.
    fn record(&mut self, hash: [u8; 32], text: String) {
        self.last = Some(hash);
        self.saved.retain(|(h, _)| *h != hash);
        if self
            .opened
            .as_ref()
            .is_some_and(|(h, t)| *h == hash && *t == text)
        {
            return;
        }
        self.saved.push((hash, text));
        let mut total: usize = self.saved.iter().map(|(_, t)| t.len()).sum();
        while total > SAVED_TEXT_BUDGET && self.saved.len() > 1 {
            total -= self.saved.remove(0).1.len();
        }
    }
}

/// The hash `OpenDoc::saved` compares against: of the compact JSON text, so
/// a change of key order counts as a change.
fn content_hash(value: &Json) -> [u8; 32] {
    let text = serde_json::to_string(value).unwrap_or_default();
    *blake3::hash(text.as_bytes()).as_bytes()
}

/// The World's open documents by project-relative path, the document kinds
/// by file suffix, and the world check. Copied by [`World::fork`].
#[derive(Resource, Clone, Debug, Default)]
pub struct OpenDocs {
    docs: BTreeMap<String, OpenDoc>,
    kinds: Vec<(String, DocKind)>,
    revisions: u64,
    /// The last epoch of every path ever opened.
    epochs: BTreeMap<String, u64>,
    world_check: Option<WorldCheck>,
}

/// The revision and dirty flag of every open document, taken by
/// [`World::doc_marks`] and put back by [`World::restore_doc_marks`].
#[derive(Clone, Debug, PartialEq)]
pub struct DocMarks {
    marks: BTreeMap<String, (u64, bool)>,
    revisions: u64,
}

impl OpenDocs {
    fn next_revision(&mut self) -> u64 {
        self.revisions += 1;
        self.revisions
    }

    /// The kind of `path`: the one with the longest matching suffix. A
    /// suffix that does not start with `.` is a path and matches whole path
    /// segments only (`graphs/a.json` matches `graphs/a.json`, not
    /// `oldgraphs/a.json`).
    fn kind(&self, path: &str) -> Option<DocKind> {
        let matches = |suffix: &str| {
            path.strip_suffix(suffix).is_some_and(|rest| {
                suffix.starts_with('.') || rest.is_empty() || rest.ends_with('/')
            })
        };
        self.kinds
            .iter()
            .filter(|(suffix, _)| matches(suffix))
            .max_by_key(|(suffix, _)| suffix.len())
            .map(|(_, k)| *k)
    }

    /// The validator for `path`, from its kind.
    fn validator(&self, path: &str) -> Option<DocValidator> {
        self.kind(path).map(|k| k.validate)
    }

    fn insert(&mut self, path: String, value: Json, opened_absent: bool, texts: Texts) {
        let revision = self.next_revision();
        let epoch = self.epochs.get(&path).map_or(1, |e| e + 1);
        self.epochs.insert(path.clone(), epoch);
        let saved = content_hash(&value);
        self.docs.insert(
            path,
            OpenDoc {
                value,
                revision,
                dirty: false,
                epoch,
                saved,
                opened_absent,
                texts,
            },
        );
    }
}

/// `path` as the key of an open document: a project-relative path with `/`
/// separators and no `.` segments. Absolute paths, `..` segments and empty
/// paths are refused, so a document is always written inside the project.
pub fn doc_path(path: &str) -> Result<String, SceneError> {
    let unified = path.replace('\\', "/");
    let bad = |why: &str| SceneError::Invalid(format!("document path {path:?} {why}"));
    if unified.starts_with('/') || unified.as_bytes().get(1) == Some(&b':') {
        return Err(bad("must be relative to the project"));
    }
    let mut parts = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(bad("must not contain `..`")),
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Err(bad("is empty"));
    }
    Ok(parts.join("/"))
}

impl World {
    fn open_docs(&self) -> &OpenDocs {
        self.ecs.resource::<OpenDocs>()
    }

    fn open_docs_mut(&mut self) -> bevy_ecs::world::Mut<'_, OpenDocs> {
        self.ecs.resource_mut::<OpenDocs>()
    }

    /// Registers the kind of the documents whose path ends with `suffix`
    /// (for example `.material.json`), or of one document when `suffix` is a
    /// project-relative path (`real.toml`, matched by whole segments),
    /// replacing an earlier one for the same suffix. When several match, the
    /// longest wins.
    pub fn register_doc_kind(&mut self, suffix: impl Into<String>, kind: DocKind) {
        let suffix = suffix.into();
        let mut docs = self.open_docs_mut();
        docs.kinds.retain(|(s, _)| *s != suffix);
        docs.kinds.push((suffix, kind));
    }

    /// Registers the validator of the documents whose path ends with
    /// `suffix` (for example `.graph.json`), or of one document when `suffix`
    /// is a project-relative path (`graphs/island.json`, matched by whole
    /// segments), replacing an earlier one for the same suffix. When several
    /// match, the longest wins. Such documents are written as
    /// [`pretty_json`].
    pub fn register_doc_validator(&mut self, suffix: impl Into<String>, validator: DocValidator) {
        self.register_doc_kind(
            suffix,
            DocKind {
                validate: validator,
                write: pretty_json,
            },
        );
    }

    /// Checks the open document at `path` with its kind's validator: `Ok`
    /// when it validates, is absent, or no validator matches its suffix,
    /// `Err` with the validator's message otherwise. `None` when the
    /// document is not open.
    pub fn validate_doc(&self, path: &str) -> Option<Result<(), String>> {
        let path = doc_path(path).ok()?;
        let docs = self.open_docs();
        let doc = docs.docs.get(&path)?;
        if doc.is_absent() {
            return Some(Ok(()));
        }
        Some(docs.validator(&path).map_or(Ok(()), |v| v(&doc.value)))
    }

    /// Opens the document at the project-relative `path` with `value` (what
    /// the file holds), not dirty, with a new [epoch](OpenDoc::epoch). An
    /// open document of that path is replaced, which drops its unsaved
    /// changes; undo history recorded against the old epoch is refused.
    ///
    /// A `null` value opens the path [absent](OpenDoc::is_absent): the
    /// caller has checked that it has no file. Giving the document content
    /// then makes the file at the next save, and emptying it again removes
    /// it. Use [`World::open_doc_text`] for a document read from a file, so
    /// that its text is written back exactly.
    ///
    /// # Errors
    ///
    /// `SceneError::Invalid` for a path outside the project, or a value its
    /// kind's validator refuses (nothing is opened then).
    pub fn open_doc(&mut self, path: &str, value: Json) -> Result<(), SceneError> {
        let path = doc_path(path)?;
        let absent = value.is_null();
        if !absent {
            self.check_new_doc(&path, &value)?;
        }
        self.open_docs_mut()
            .insert(path, value, absent, Texts::default());
        Ok(())
    }

    /// Opens the document at `path` as [`World::open_doc`] does, from a file
    /// that holds `text`, whose content is `value`. The text is kept, so a
    /// save of the same content writes it again byte for byte.
    ///
    /// # Errors
    ///
    /// As [`World::open_doc`], and `SceneError::Invalid` for a `null` value
    /// (a file holds content; an absent document has no file).
    pub fn open_doc_text(
        &mut self,
        path: &str,
        value: Json,
        text: impl Into<String>,
    ) -> Result<(), SceneError> {
        let path = doc_path(path)?;
        if value.is_null() {
            return Err(SceneError::Invalid(format!(
                "{path} holds `null`, which is no document"
            )));
        }
        self.check_new_doc(&path, &value)?;
        let texts = Texts::opened(content_hash(&value), text.into());
        self.open_docs_mut().insert(path, value, false, texts);
        Ok(())
    }

    fn check_new_doc(&self, path: &str, value: &Json) -> Result<(), SceneError> {
        match self.open_docs().validator(path) {
            Some(validate) => validate(value)
                .map_err(|e| SceneError::Invalid(format!("{path} is not valid: {e}"))),
            None => Ok(()),
        }
    }

    /// Closes the document at `path` without saving it; returns it, if it was open.
    pub fn close_doc(&mut self, path: &str) -> Option<OpenDoc> {
        let path = doc_path(path).ok()?;
        self.open_docs_mut().docs.remove(&path)
    }

    /// The [epoch](OpenDoc::epoch) of the open document at `path`, if it is open.
    pub fn doc_epoch(&self, path: &str) -> Option<u64> {
        self.doc(path).map(|d| d.epoch)
    }

    /// The open document at `path`, if any.
    pub fn doc(&self, path: &str) -> Option<&OpenDoc> {
        let path = doc_path(path).ok()?;
        self.open_docs().docs.get(&path)
    }

    /// Every open document, by path.
    pub fn docs(&self) -> impl Iterator<Item = (&str, &OpenDoc)> {
        self.open_docs().docs.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// The paths of the open documents with unsaved changes, sorted.
    pub fn unsaved_docs(&self) -> Vec<String> {
        self.docs()
            .filter(|(_, d)| d.dirty)
            .map(|(p, _)| p.to_string())
            .collect()
    }

    /// Edits the open document at `path` in place with `edit`. When `edit`
    /// returns `Ok`, its revision is bumped and it is dirty unless its
    /// content is back to what was opened or last saved;
    /// when it returns `Err`, it must have left the value unchanged, and the
    /// document is left as it was. `None` when the document is not open.
    pub fn edit_doc<R, E>(
        &mut self,
        path: &str,
        edit: impl FnOnce(&mut Json) -> Result<R, E>,
    ) -> Option<Result<R, E>> {
        let path = doc_path(path).ok()?;
        let mut docs = self.open_docs_mut();
        let docs = &mut *docs;
        let doc = docs.docs.get_mut(&path)?;
        let result = edit(&mut doc.value);
        if result.is_ok() {
            docs.revisions += 1;
            doc.revision = docs.revisions;
            doc.dirty = content_hash(&doc.value) != doc.saved;
        }
        Some(result)
    }

    /// What the project save does with the file of the open document at
    /// `path`: write the text recorded for its content, or else its kind's
    /// writer's output (pretty JSON when the kind registers no writer), or
    /// remove the file of an absent document. `None` when the document is
    /// not open; `Some(Err)` with the writer's message when it fails.
    pub fn doc_file(&self, path: &str) -> Option<Result<DocFile, String>> {
        let path = doc_path(path).ok()?;
        let docs = self.open_docs();
        let doc = docs.docs.get(&path)?;
        if doc.is_absent() {
            return Some(Ok(DocFile::Absent));
        }
        if let Some(text) = doc.recorded_text() {
            return Some(Ok(DocFile::Text(text.to_string())));
        }
        let write = docs
            .kind(&path)
            .map_or(pretty_json as DocWriter, |k| k.write);
        Some(write(&doc.value, doc.texts.base()).map(DocFile::Text))
    }

    /// The text the save writes for the open document at `path`
    /// ([`World::doc_file`]); `None` when it is not open, is absent, or its
    /// writer fails.
    pub fn doc_text(&self, path: &str) -> Option<String> {
        match self.doc_file(path)? {
            Ok(DocFile::Text(text)) => Some(text),
            Ok(DocFile::Absent) | Err(_) => None,
        }
    }

    /// Marks the open document at `path` saved (not dirty), after what
    /// [`doc_file`](World::doc_file) gives was done to its file: the text
    /// written is recorded for the content, or the file is gone.
    pub fn mark_doc_saved(&mut self, path: &str) {
        let Ok(path) = doc_path(path) else {
            return;
        };
        let file = self.doc_file(&path);
        if let Some(doc) = self.open_docs_mut().docs.get_mut(&path) {
            let hash = content_hash(&doc.value);
            doc.dirty = false;
            doc.saved = hash;
            match file {
                Some(Ok(DocFile::Text(text))) => doc.texts.record(hash, text),
                Some(Ok(DocFile::Absent)) => doc.texts.last = None,
                _ => {}
            }
        }
    }

    /// Sets the rule the whole World keeps after every transaction, which
    /// `re_command`'s history checks with the documents' validators
    /// (ADR-0064); `None` clears it. There is one at a time: setting one
    /// replaces the last.
    pub fn set_world_check(&mut self, check: Option<WorldCheck>) {
        self.open_docs_mut().world_check = check;
    }

    /// Runs the world check: `Ok` when none is set, the rule's message when
    /// the World breaks it.
    pub fn check_world(&self) -> Result<(), String> {
        match self.open_docs().world_check {
            Some(check) => check(self),
            None => Ok(()),
        }
    }

    /// The revision and dirty flag of every open document, so a change that
    /// is undone again (a rejected transaction, a dry run) can leave them as
    /// they were.
    pub fn doc_marks(&self) -> DocMarks {
        let docs = self.open_docs();
        DocMarks {
            marks: docs
                .docs
                .iter()
                .map(|(k, d)| (k.clone(), (d.revision, d.dirty)))
                .collect(),
            revisions: docs.revisions,
        }
    }

    /// Puts back the revisions and dirty flags `marks` recorded, for the
    /// documents still open.
    pub fn restore_doc_marks(&mut self, marks: &DocMarks) {
        let mut docs = self.open_docs_mut();
        docs.revisions = marks.revisions;
        for (path, doc) in docs.docs.iter_mut() {
            if let Some((revision, dirty)) = marks.marks.get(path) {
                doc.revision = *revision;
                doc.dirty = *dirty;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths_are_project_relative() {
        assert_eq!(
            doc_path("graphs/a.graph.json").unwrap(),
            "graphs/a.graph.json"
        );
        assert_eq!(doc_path("./graphs\\a.json").unwrap(), "graphs/a.json");
        assert_eq!(doc_path("graphs//./a.json").unwrap(), "graphs/a.json");
        for bad in ["", ".", "/etc/x.json", "C:/x.json", "graphs/../../x.json"] {
            assert!(doc_path(bad).is_err(), "{bad}");
        }
    }

    fn no_b(v: &Json) -> Result<(), String> {
        match v.get("b") {
            Some(_) => Err("`b` is not allowed".into()),
            None => Ok(()),
        }
    }

    #[test]
    fn open_edit_save_and_marks() {
        let mut w = World::new();
        w.register_doc_validator(".t.json", no_b);
        assert!(w.open_doc("x.t.json", json!({"b": 1})).is_err());
        assert!(w.doc("x.t.json").is_none());
        w.open_doc("./x.t.json", json!({"a": 1})).unwrap();
        let r0 = w.doc("x.t.json").unwrap().revision;
        assert!(!w.doc("x.t.json").unwrap().dirty);
        assert!(w.unsaved_docs().is_empty());

        let marks = w.doc_marks();
        let out: Option<Result<(), ()>> = w.edit_doc("x.t.json", |v| {
            v["b"] = json!(2);
            Ok(())
        });
        assert_eq!(out, Some(Ok(())));
        let d = w.doc("x.t.json").unwrap();
        assert!(d.dirty && d.revision > r0);
        assert_eq!(w.unsaved_docs(), ["x.t.json"]);
        assert_eq!(
            w.validate_doc("x.t.json"),
            Some(Err("`b` is not allowed".into()))
        );
        assert_eq!(w.validate_doc("y.t.json"), None);
        let _: Option<Result<(), ()>> = w.edit_doc("x.t.json", |v| {
            v.as_object_mut().unwrap().shift_remove("b");
            Ok(())
        });
        w.restore_doc_marks(&marks);
        assert_eq!(w.doc("x.t.json").unwrap().revision, r0);
        assert!(w.unsaved_docs().is_empty());

        // A failed edit changes nothing.
        let out: Option<Result<(), &str>> = w.edit_doc("x.t.json", |_| Err("no"));
        assert_eq!(out, Some(Err("no")));
        assert_eq!(w.doc("x.t.json").unwrap().revision, r0);
        assert!(w.edit_doc::<(), ()>("y.json", |_| Ok(())).is_none());

        let _: Option<Result<(), ()>> = w.edit_doc("x.t.json", |v| {
            v["c"] = json!([1.5]);
            Ok(())
        });
        assert_eq!(
            w.doc_text("x.t.json").unwrap(),
            "{\n  \"a\": 1,\n  \"c\": [\n    1.5\n  ]\n}\n"
        );
        w.mark_doc_saved("x.t.json");
        assert!(w.unsaved_docs().is_empty());

        // Editing back to the saved content clears the dirty flag.
        let _: Option<Result<(), ()>> = w.edit_doc("x.t.json", |v| {
            v["c"] = json!(2);
            Ok(())
        });
        assert_eq!(w.unsaved_docs(), ["x.t.json"]);
        let _: Option<Result<(), ()>> = w.edit_doc("x.t.json", |v| {
            v["c"] = json!([1.5]);
            Ok(())
        });
        assert!(w.unsaved_docs().is_empty());

        // Every opening of a path has a new epoch, even after a close.
        let e = w.doc_epoch("x.t.json").unwrap();
        w.open_doc("x.t.json", json!({"a": 2})).unwrap();
        assert_eq!(w.doc_epoch("x.t.json"), Some(e + 1));
        w.close_doc("x.t.json");
        assert_eq!(w.doc_epoch("x.t.json"), None);
        w.open_doc("x.t.json", json!({"a": 1, "c": [1.5]})).unwrap();
        assert_eq!(w.doc_epoch("x.t.json"), Some(e + 2));

        // A world copy carries the open documents and validators.
        let copy = w.fork();
        assert_eq!(copy.doc("x.t.json"), w.doc("x.t.json"));
        assert!(copy.open_docs().validator("y.t.json").is_some());
        assert!(w.close_doc("x.t.json").is_some());
        assert!(w.doc("x.t.json").is_none());
    }

    fn edit(w: &mut World, path: &str, value: Json) {
        let out: Option<Result<(), ()>> = w.edit_doc(path, |v| {
            *v = value;
            Ok(())
        });
        assert_eq!(out, Some(Ok(())));
    }

    #[test]
    fn an_absent_document_has_no_file_until_it_has_content() {
        let mut w = World::new();
        w.register_doc_validator(".t.json", no_b);
        w.open_doc("new.t.json", Json::Null).unwrap();
        let d = w.doc("new.t.json").unwrap();
        assert!(d.is_absent() && d.opened_absent() && !d.dirty);
        // Validators are not called on `null`.
        assert_eq!(w.validate_doc("new.t.json"), Some(Ok(())));
        assert_eq!(w.doc_file("new.t.json"), Some(Ok(DocFile::Absent)));
        assert_eq!(w.doc_text("new.t.json"), None);

        // Content makes the file at the next save.
        edit(&mut w, "new.t.json", json!({"a": 1}));
        assert_eq!(w.unsaved_docs(), ["new.t.json"]);
        assert_eq!(
            w.doc_file("new.t.json"),
            Some(Ok(DocFile::Text("{\n  \"a\": 1\n}\n".into())))
        );
        w.mark_doc_saved("new.t.json");
        assert!(w.unsaved_docs().is_empty());

        // Emptying it again removes the file at the next save.
        edit(&mut w, "new.t.json", Json::Null);
        assert_eq!(w.unsaved_docs(), ["new.t.json"]);
        assert_eq!(w.doc_file("new.t.json"), Some(Ok(DocFile::Absent)));
        w.mark_doc_saved("new.t.json");
        assert!(w.unsaved_docs().is_empty());
        assert!(w.doc("new.t.json").unwrap().opened_absent());

        // A document read from a file was not opened absent, and a file
        // cannot hold `null`.
        w.open_doc("old.t.json", json!({"a": 1})).unwrap();
        assert!(!w.doc("old.t.json").unwrap().opened_absent());
        assert!(w.open_doc_text("x.t.json", Json::Null, "null\n").is_err());
        assert!(w.open_doc_text("x.t.json", json!({"b": 1}), "{}").is_err());
        assert!(w.doc("x.t.json").is_none());
    }

    #[test]
    fn a_recorded_text_is_written_back_for_its_content() {
        let mut w = World::new();
        let text = "{ \"base_color\": [0.8, 0.8, 0.8, 1.0],\n  \"roughness\": 0.5 }\n";
        let value: Json = serde_json::from_str(text).unwrap();
        w.open_doc_text("m.material.json", value.clone(), text)
            .unwrap();
        assert_eq!(
            w.doc("m.material.json").unwrap().recorded_text(),
            Some(text)
        );
        assert_eq!(w.doc_text("m.material.json").as_deref(), Some(text));

        // An edit is written by the writer; undoing it gives the text back.
        edit(&mut w, "m.material.json", json!({"roughness": 0.25}));
        let edited = w.doc_text("m.material.json").unwrap();
        assert_eq!(edited, "{\n  \"roughness\": 0.25\n}\n");
        w.mark_doc_saved("m.material.json");
        edit(&mut w, "m.material.json", value.clone());
        assert_eq!(w.doc_text("m.material.json").as_deref(), Some(text));
        w.mark_doc_saved("m.material.json");
        edit(&mut w, "m.material.json", json!({"roughness": 0.25}));
        assert_eq!(w.doc_text("m.material.json"), Some(edited));

        // Saved texts beyond the budget go, oldest first; the opening text
        // and the last one written stay.
        let big = |i: usize| json!({ "i": i, "pad": "x".repeat(SAVED_TEXT_BUDGET / 3) });
        for i in 0..6 {
            edit(&mut w, "m.material.json", big(i));
            w.mark_doc_saved("m.material.json");
        }
        let doc = w.doc("m.material.json").unwrap();
        let kept: usize = doc.texts.saved.iter().map(|(_, t)| t.len()).sum();
        assert!(kept <= SAVED_TEXT_BUDGET && doc.texts.saved.len() == 2);
        assert_eq!(doc.texts.get(&content_hash(&value)), Some(text));
        assert!(doc.texts.get(&content_hash(&big(0))).is_none());
        assert!(doc.texts.get(&content_hash(&big(5))).is_some());
    }

    fn base_writer(value: &Json, base: Option<&str>) -> Result<String, String> {
        if value.get("fail").is_some() {
            return Err("cannot write `fail`".into());
        }
        Ok(format!("{}|{value}", base.unwrap_or("-")))
    }

    #[test]
    fn a_kind_writes_with_the_last_text_as_its_base() {
        let mut w = World::new();
        w.register_doc_kind(
            "real.toml",
            DocKind {
                validate: no_b,
                write: base_writer,
            },
        );
        w.open_doc_text("real.toml", json!({"a": 1}), "A").unwrap();
        assert!(w.open_doc("real.toml", json!({"b": 1})).is_err());
        edit(&mut w, "real.toml", json!({"a": 2}));
        assert_eq!(w.doc_text("real.toml").as_deref(), Some(r#"A|{"a":2}"#));
        w.mark_doc_saved("real.toml");
        edit(&mut w, "real.toml", json!({"a": 3}));
        assert_eq!(
            w.doc_text("real.toml").as_deref(),
            Some(r#"A|{"a":2}|{"a":3}"#)
        );
        edit(&mut w, "real.toml", json!({"fail": true}));
        assert_eq!(
            w.doc_file("real.toml"),
            Some(Err("cannot write `fail`".into()))
        );
        assert_eq!(w.doc_text("real.toml"), None);
        // A document with no kind is pretty JSON.
        w.open_doc("notes/a.json", json!({"x": [1]})).unwrap();
        assert_eq!(
            w.doc_text("notes/a.json").as_deref(),
            Some("{\n  \"x\": [\n    1\n  ]\n}\n")
        );
    }

    fn never(_: &World) -> Result<(), String> {
        Err("never".into())
    }

    #[test]
    fn the_world_check_is_one_at_a_time() {
        let mut w = World::new();
        assert_eq!(w.check_world(), Ok(()));
        w.set_world_check(Some(never));
        assert_eq!(w.check_world(), Err("never".into()));
        assert_eq!(w.fork().check_world(), Err("never".into()));
        w.set_world_check(None);
        assert_eq!(w.check_world(), Ok(()));
    }

    #[test]
    fn a_path_validator_matches_whole_segments() {
        let mut w = World::new();
        w.register_doc_validator("graphs/a.json", no_b);
        let docs = w.open_docs();
        assert!(docs.validator("graphs/a.json").is_some());
        assert!(docs.validator("x/graphs/a.json").is_some());
        assert!(docs.validator("oldgraphs/a.json").is_none());
        assert!(docs.validator("graphs/b.json").is_none());
    }
}
