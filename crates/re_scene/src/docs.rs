//! Open documents (ADR-0044): project files, such as `graphs/*.graph.json`,
//! loaded into the World as JSON and edited in memory by `re_command`'s
//! `set_doc` operation, so every edit is undoable and agents see it. The
//! ordinary project save writes them back.
//!
//! * [`OpenDocs`]: the World resource holding each open document by
//!   project-relative path ([`doc_path`]), with a revision counter (bumped on
//!   every change; a view rebuilds when it changes) and a dirty flag;
//! * validators by file suffix ([`World::register_doc_validator`]): a
//!   transaction whose resulting document does not validate is rejected.
//!
//! Open documents are not entities: they are not part of
//! [`World::state_hash`] or of the scene file.

use std::collections::BTreeMap;

use bevy_ecs::resource::Resource;
use serde_json::Value as Json;

use crate::error::SceneError;
use crate::world::World;

/// Checks a document of one kind (registered by file suffix). The message
/// says what is wrong.
pub type DocValidator = fn(&Json) -> Result<(), String>;

/// One open document.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenDoc {
    /// The document's content.
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
}

impl OpenDoc {
    /// True if `value` is the content the document was opened with or last
    /// saved as: a file holding it was written by that save, not changed
    /// by someone else.
    pub fn matches_saved(&self, value: &Json) -> bool {
        content_hash(value) == self.saved
    }
}

/// The hash `OpenDoc::saved` compares against: of the compact JSON text, so
/// a change of key order counts as a change.
fn content_hash(value: &Json) -> [u8; 32] {
    let text = serde_json::to_string(value).unwrap_or_default();
    *blake3::hash(text.as_bytes()).as_bytes()
}

/// The World's open documents by project-relative path, and the validators
/// by file suffix. Copied by [`World::fork`].
#[derive(Resource, Clone, Debug, Default)]
pub struct OpenDocs {
    docs: BTreeMap<String, OpenDoc>,
    validators: Vec<(String, DocValidator)>,
    revisions: u64,
    /// The last epoch of every path ever opened.
    epochs: BTreeMap<String, u64>,
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

    /// The validator for `path`: the one with the longest matching suffix. A
    /// suffix that does not start with `.` is a path and matches whole path
    /// segments only (`graphs/a.json` matches `graphs/a.json`, not
    /// `oldgraphs/a.json`).
    fn validator(&self, path: &str) -> Option<DocValidator> {
        let matches = |suffix: &str| {
            path.strip_suffix(suffix).is_some_and(|rest| {
                suffix.starts_with('.') || rest.is_empty() || rest.ends_with('/')
            })
        };
        self.validators
            .iter()
            .filter(|(suffix, _)| matches(suffix))
            .max_by_key(|(suffix, _)| suffix.len())
            .map(|(_, v)| *v)
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

    /// Registers the validator of the documents whose path ends with
    /// `suffix` (for example `.graph.json`), or of one document when `suffix`
    /// is a project-relative path (`graphs/island.json`, matched by whole
    /// segments), replacing an earlier one for the same suffix. When several
    /// match, the longest wins.
    pub fn register_doc_validator(&mut self, suffix: impl Into<String>, validator: DocValidator) {
        let suffix = suffix.into();
        let mut docs = self.open_docs_mut();
        docs.validators.retain(|(s, _)| *s != suffix);
        docs.validators.push((suffix, validator));
    }

    /// Checks the open document at `path` with its kind's validator: `Ok`
    /// when it validates or no validator matches its suffix, `Err` with the
    /// validator's message otherwise. `None` when the document is not open.
    pub fn validate_doc(&self, path: &str) -> Option<Result<(), String>> {
        let path = doc_path(path).ok()?;
        let docs = self.open_docs();
        let doc = docs.docs.get(&path)?;
        Some(docs.validator(&path).map_or(Ok(()), |v| v(&doc.value)))
    }

    /// Opens the document at the project-relative `path` with `value` (what
    /// the file holds), not dirty, with a new [epoch](OpenDoc::epoch). An
    /// open document of that path is replaced, which drops its unsaved
    /// changes; undo history recorded against the old epoch is refused.
    ///
    /// # Errors
    ///
    /// `SceneError::Invalid` for a path outside the project, or a value its
    /// kind's validator refuses (nothing is opened then).
    pub fn open_doc(&mut self, path: &str, value: Json) -> Result<(), SceneError> {
        let path = doc_path(path)?;
        if let Some(validate) = self.open_docs().validator(&path) {
            validate(&value)
                .map_err(|e| SceneError::Invalid(format!("{path} is not valid: {e}")))?;
        }
        let mut docs = self.open_docs_mut();
        let revision = docs.next_revision();
        let epoch = docs.epochs.get(&path).map_or(1, |e| e + 1);
        docs.epochs.insert(path.clone(), epoch);
        let saved = content_hash(&value);
        docs.docs.insert(
            path,
            OpenDoc {
                value,
                revision,
                dirty: false,
                epoch,
                saved,
            },
        );
        Ok(())
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

    /// The text to write for the open document at `path`: pretty JSON
    /// ending with a newline, as graph files are written.
    pub fn doc_text(&self, path: &str) -> Option<String> {
        let doc = self.doc(path)?;
        let mut text = serde_json::to_string_pretty(&doc.value).ok()?;
        text.push('\n');
        Some(text)
    }

    /// Marks the open document at `path` saved (not dirty), after its
    /// [`doc_text`](World::doc_text) was written to its file.
    pub fn mark_doc_saved(&mut self, path: &str) {
        if let Ok(path) = doc_path(path)
            && let Some(doc) = self.open_docs_mut().docs.get_mut(&path)
        {
            doc.dirty = false;
            doc.saved = content_hash(&doc.value);
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
