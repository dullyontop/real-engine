//! The document lifecycle (ADR-0064) through transactions: absent documents
//! made and emptied by `set_doc`, the refusal to empty a document opened from
//! a file, recorded texts coming back on undo, the world check, and undo
//! stacks set aside per edited world.

use re_command::{History, Op, OpError, Origin, Transaction, codes};
use re_scene::{DocFile, World};
use serde_json::{Value as Json, json};

const NEW: &str = "materials/new.material.json";
const OLD: &str = "materials/old.material.json";

fn tx(ops: Vec<Op>) -> Transaction {
    Transaction::new("doc edit", Origin::Agent("test-agent".into()), ops)
}

fn set(doc: &str, pointer: &str, value: Json) -> Op {
    Op::SetDoc {
        doc: doc.into(),
        pointer: pointer.into(),
        value: Some(value),
    }
}

/// Refuses a material whose `roughness` is outside 0 to 1.
fn roughness_in_range(v: &Json) -> Result<(), String> {
    match v.get("roughness").and_then(Json::as_f64) {
        Some(r) if !(0.0..=1.0).contains(&r) => Err(format!("roughness {r} is outside 0 to 1")),
        _ => Ok(()),
    }
}

fn world() -> World {
    let mut w = World::new();
    w.register_doc_validator(".material.json", roughness_in_range);
    w
}

#[test]
fn making_a_file_is_one_undo_step() {
    let mut w = world();
    let mut h = History::default();
    w.open_doc(NEW, Json::Null).unwrap();
    h.execute(&mut w, tx(vec![set(NEW, "", json!({"roughness": 0.5}))]))
        .unwrap();
    assert_eq!(
        w.doc_file(NEW),
        Some(Ok(DocFile::Text("{\n  \"roughness\": 0.5\n}\n".into())))
    );
    w.mark_doc_saved(NEW);

    // Undo empties the document again, so the next save removes the file.
    assert!(h.undo(&mut w).unwrap());
    assert!(w.doc(NEW).unwrap().is_absent());
    assert_eq!(w.doc_file(NEW), Some(Ok(DocFile::Absent)));
    assert_eq!(w.unsaved_docs(), [NEW]);
    w.mark_doc_saved(NEW);
    assert!(w.unsaved_docs().is_empty());

    // Redo gives it its content back, and its text.
    assert!(h.redo(&mut w).unwrap());
    assert_eq!(
        w.doc_text(NEW).as_deref(),
        Some("{\n  \"roughness\": 0.5\n}\n")
    );
    assert_eq!(w.unsaved_docs(), [NEW]);
}

#[test]
fn an_invalid_content_is_refused_and_null_is_never_validated() {
    let mut w = world();
    let mut h = History::default();
    w.open_doc(NEW, Json::Null).unwrap();
    let err = h
        .execute(&mut w, tx(vec![set(NEW, "", json!({"roughness": 2.0}))]))
        .unwrap_err();
    assert!(matches!(err.error, OpError::InvalidDocument { .. }));
    assert_eq!(err.error.code(), codes::INVALID_DOCUMENT);
    assert!(w.doc(NEW).unwrap().is_absent());
    assert!(w.unsaved_docs().is_empty());
    // Emptying a document the session made is allowed, and not validated.
    h.execute(&mut w, tx(vec![set(NEW, "", json!({"roughness": 0.1}))]))
        .unwrap();
    h.execute(&mut w, tx(vec![set(NEW, "", Json::Null)]))
        .unwrap();
    assert!(w.doc(NEW).unwrap().is_absent());
}

#[test]
fn a_document_opened_from_a_file_cannot_be_emptied() {
    let mut w = world();
    let mut h = History::default();
    let text = "{ \"roughness\": 0.5 }\n";
    w.open_doc_text(OLD, serde_json::from_str(text).unwrap(), text)
        .unwrap();
    for op in [
        set(OLD, "", Json::Null),
        set(&format!("./{OLD}"), "", Json::Null),
    ] {
        let err = h.execute(&mut w, tx(vec![op.clone()])).unwrap_err();
        assert_eq!(
            err.error,
            OpError::DocumentFileKept { doc: OLD.into() },
            "{op:?}"
        );
        assert_eq!(err.error.code(), codes::DOCUMENT_FILE_KEPT);
        assert!(History::dry_run(&mut w, &tx(vec![op])).is_err());
    }
    assert!(!w.doc(OLD).unwrap().is_absent());
    assert!(w.unsaved_docs().is_empty());
    // `null` inside the document is an ordinary value.
    h.execute(&mut w, tx(vec![set(OLD, "/note", Json::Null)]))
        .unwrap();

    // Undoing every edit gives the file's own text back.
    h.execute(&mut w, tx(vec![set(OLD, "/roughness", json!(0.25))]))
        .unwrap();
    assert_ne!(w.doc_text(OLD).as_deref(), Some(text));
    while h.undo(&mut w).unwrap() {}
    assert_eq!(w.doc_text(OLD).as_deref(), Some(text));
    assert!(w.unsaved_docs().is_empty());
}

/// The rule of a prefab being edited, cut down: at most one entity.
fn one_entity(w: &World) -> Result<(), String> {
    match w.ids().iter().count() {
        0 | 1 => Ok(()),
        n => Err(format!(
            "a prefab has exactly one root entity; this edit leaves {n}"
        )),
    }
}

fn spawn(id: &str) -> Op {
    serde_json::from_value(json!({"op": "spawn", "id": id})).unwrap()
}

#[test]
fn the_world_check_rolls_back_what_breaks_it() {
    let mut w = world();
    let mut h = History::default();
    w.set_world_check(Some(one_entity));
    h.execute(&mut w, tx(vec![spawn("root")])).unwrap();
    let hash = w.state_hash();
    let err = h.execute(&mut w, tx(vec![spawn("other")])).unwrap_err();
    assert_eq!(err.error.code(), codes::WORLD_RULE);
    assert!(err.to_string().contains("exactly one root"), "{err}");
    assert_eq!(w.state_hash(), hash);
    assert!(History::dry_run(&mut w, &tx(vec![spawn("other")])).is_err());
    assert_eq!(w.state_hash(), hash);
    assert_eq!(h.log().last().unwrap().kind, re_command::LogKind::Rejected);

    // Cleared, the same transaction applies.
    w.set_world_check(None);
    h.execute(&mut w, tx(vec![spawn("other")])).unwrap();
}

#[test]
fn stacks_set_aside_come_back_whole() {
    let mut w = world();
    let mut h = History::default();
    h.execute(&mut w, tx(vec![spawn("a")])).unwrap();
    h.execute(&mut w, tx(vec![spawn("b")])).unwrap();
    assert!(h.undo(&mut w).unwrap());
    let first = w.state_hash();

    let aside = h.take_stacks();
    assert!(!aside.is_empty());
    assert!(!h.can_undo() && !h.can_redo());
    // Another world is edited with a history of its own.
    let mut other = world();
    h.execute(&mut other, tx(vec![spawn("x")])).unwrap();
    let log = h.log().len();

    let theirs = h.take_stacks();
    h.put_stacks(aside);
    assert!(h.can_undo() && h.can_redo());
    assert!(h.redo(&mut w).unwrap());
    assert!(h.undo(&mut w).unwrap());
    assert_eq!(w.state_hash(), first);
    assert!(h.undo(&mut w).unwrap());
    assert!(!h.can_undo());
    assert_eq!(h.log().len(), log + 3);
    assert!(!theirs.is_empty());
}
