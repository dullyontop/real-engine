//! `set_doc` on open documents (ADR-0044): JSON-pointer edits with exact
//! inverses, validation of the resulting document, and undo, redo, dry runs
//! and the log working unchanged.

use re_command::{
    AllowAll, CommandRegistry, EditState, History, LogKind, Op, OpError, Origin, Transaction,
    register_builtin_commands,
};
use re_scene::World;
use serde_json::{Value as Json, json};

const DOC: &str = "graphs/a.graph.json";

fn tx(ops: Vec<Op>) -> Transaction {
    Transaction::new("doc edit", Origin::Agent("test-agent".into()), ops)
}

fn set(pointer: &str, value: Json) -> Op {
    Op::SetDoc {
        doc: DOC.into(),
        pointer: pointer.into(),
        value: Some(value),
    }
}

fn remove(pointer: &str) -> Op {
    Op::SetDoc {
        doc: DOC.into(),
        pointer: pointer.into(),
        value: None,
    }
}

/// Refuses a document whose `edges` name a node that is not in `nodes`.
fn edges_name_nodes(v: &Json) -> Result<(), String> {
    let ids: Vec<&Json> = v["nodes"]
        .as_array()
        .ok_or("`nodes` must be an array")?
        .iter()
        .map(|n| &n["id"])
        .collect();
    for (i, e) in v["edges"].as_array().into_iter().flatten().enumerate() {
        if !ids.contains(&&e["from"]) {
            return Err(format!("edge {i} names a missing node"));
        }
    }
    Ok(())
}

fn start() -> serde_json::Value {
    json!({
        "name": "a",
        "nodes": [{"id": "h", "params": {"size": 64, "octaves": 4, "gain": 0.5}}, {"id": "t", "params": {}}],
        "edges": [{"from": "h"}],
        "layout": {"h": [0.0, 1.0], "t": [2.0, 3.0]}
    })
}

fn world() -> World {
    let mut w = World::new();
    w.register_doc_validator(".graph.json", edges_name_nodes);
    w.open_doc(DOC, start()).unwrap();
    w
}

fn text(w: &World) -> String {
    w.doc_text(DOC).unwrap()
}

#[test]
fn edits_and_their_exact_inverses() {
    let mut w = world();
    let mut h = History::default();
    let original = text(&w);
    let edits = vec![
        // Replace an object member in place, add a new one (appended).
        vec![set("/nodes/0/params/size", json!(128))],
        vec![set("/nodes/1/params/scale", json!(2.5))],
        // Remove a member before others (key order must come back), and the
        // last member.
        vec![remove("/nodes/0/params/size")],
        vec![remove("/nodes/0/params/gain")],
        // Insert into an array, append with `-`, remove from the middle.
        vec![set("/nodes/1", json!({"id": "m", "params": {}}))],
        vec![set("/nodes/-", json!({"id": "z", "params": {}}))],
        vec![remove("/nodes/0/params/octaves"), remove("/layout/h")],
        // `null` is a value, not a removal; keys with `/` and `~` are escaped.
        vec![set("/layout/a~1b~0c", json!(null))],
        // Replace the whole document.
        vec![set("", json!({"nodes": [], "edges": []}))],
    ];
    let mut texts = vec![original.clone()];
    for ops in edits {
        h.execute(&mut w, tx(ops)).unwrap();
        texts.push(text(&w));
    }
    let v: Json = serde_json::from_str(&texts[8]).unwrap();
    assert_eq!(v["layout"]["a/b~c"], Json::Null);
    assert!(v["layout"].as_object().unwrap().contains_key("a/b~c"));
    let v: Json = serde_json::from_str(&texts[6]).unwrap();
    let ids: Vec<&str> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["h", "m", "t", "z"]);

    // Undo walks back through every state, byte for byte; redo forward.
    for expected in texts.iter().rev().skip(1) {
        assert!(h.undo(&mut w).unwrap());
        assert_eq!(&text(&w), expected);
    }
    assert_eq!(text(&w), original);
    for expected in texts.iter().skip(1) {
        assert!(h.redo(&mut w).unwrap());
        assert_eq!(&text(&w), expected);
    }

    // The append's inverse names the index it took.
    let mut w = world();
    let mut h = History::default();
    h.execute(&mut w, tx(vec![set("/nodes/-", json!({"id": "z"}))]))
        .unwrap();
    assert_eq!(h.peek_undo().unwrap(), [remove("/nodes/2")]);
}

#[test]
fn undo_and_redo_mark_the_document_and_the_log_records_set_doc() {
    let mut w = world();
    let mut h = History::default();
    let r0 = w.doc(DOC).unwrap().revision;
    assert!(w.unsaved_docs().is_empty());
    h.execute(&mut w, tx(vec![set("/layout/t", json!([5.0, 6.0]))]))
        .unwrap();
    let r1 = w.doc(DOC).unwrap().revision;
    assert!(r1 > r0);
    assert_eq!(w.unsaved_docs(), [DOC]);
    w.mark_doc_saved(DOC);
    h.undo(&mut w).unwrap();
    let d = w.doc(DOC).unwrap();
    assert!(d.dirty && d.revision > r1);
    h.redo(&mut w).unwrap();

    let kinds: Vec<LogKind> = h.log().iter().map(|e| e.kind).collect();
    assert_eq!(kinds, [LogKind::Do, LogKind::Undo, LogKind::Redo]);
    let line: Json = serde_json::from_str(h.log_since_jsonl(0).lines().next().unwrap()).unwrap();
    assert_eq!(
        line["ops"][0],
        json!({"op": "set_doc", "doc": DOC, "pointer": "/layout/t", "value": [5.0, 6.0]})
    );
    let undo: Json = serde_json::from_str(h.log_since_jsonl(1).lines().next().unwrap()).unwrap();
    assert_eq!(undo["ops"][0]["value"], json!([2.0, 3.0]));
}

#[test]
fn ops_are_plain_json() {
    let removal: Op =
        serde_json::from_value(json!({"op": "set_doc", "doc": DOC, "pointer": "/layout/h"}))
            .unwrap();
    assert_eq!(removal, remove("/layout/h"));
    // An explicit `null` sets null; it does not remove.
    let null: Op = serde_json::from_value(
        json!({"op": "set_doc", "doc": DOC, "pointer": "/layout/h", "value": null}),
    )
    .unwrap();
    assert_eq!(null, set("/layout/h", Json::Null));
    for op in [removal, null] {
        let back: Op = serde_json::from_value(serde_json::to_value(&op).unwrap()).unwrap();
        assert_eq!(back, op);
    }
    assert!(
        serde_json::from_value::<Op>(json!({"op": "set_doc", "doc": DOC, "pointer": "", "x": 1}))
            .is_err()
    );
}

#[test]
fn a_document_that_does_not_validate_rejects_the_transaction() {
    let mut w = world();
    let mut h = History::default();
    let before = w.doc(DOC).unwrap().clone();
    // Removing a node its edge names leaves the document invalid.
    let err = h
        .execute(
            &mut w,
            tx(vec![
                set("/layout/t", json!([9.0, 9.0])),
                remove("/nodes/0"),
            ]),
        )
        .unwrap_err();
    assert_eq!(err.index, 1);
    assert!(
        matches!(err.error, OpError::InvalidDocument { .. }),
        "{err}"
    );
    assert_eq!(
        err.to_string(),
        "command 1: graphs/a.graph.json is not valid: edge 0 names a missing node"
    );
    let d = err.to_diagnostic();
    assert_eq!(d.code.to_string(), "RE2001");
    assert_eq!(d.related[0].code.to_string(), "RE2008");
    // Content, revision and dirty flag are as they were.
    assert_eq!(w.doc(DOC).unwrap(), &before);
    assert!(!h.can_undo());
    assert_eq!(h.log().last().unwrap().kind, LogKind::Rejected);

    // The document is checked after the whole transaction: removing the
    // edge in the same transaction is fine.
    h.execute(&mut w, tx(vec![remove("/edges/0"), remove("/nodes/0")]))
        .unwrap();
    h.undo(&mut w).unwrap();
    assert_eq!(w.doc(DOC).unwrap().value, before.value);
}

#[test]
fn bad_pointers_and_closed_documents_are_invalid() {
    let mut w = world();
    let mut h = History::default();
    let before = text(&w);
    let cases = [
        (set("layout", json!(1)), "must be empty or start with `/`"),
        (
            set("/layout/~2", json!(1)),
            "`~` must be followed by `0` or `1`",
        ),
        (set("/nope/x", json!(1)), "no value at /nope"),
        (remove("/layout/q"), "no value at /layout/q"),
        (set("/nodes/3", json!(1)), "is not a position where"),
        (set("/nodes/01", json!(1)), "is not a position where"),
        (remove("/nodes/-"), "no value at /nodes/-"),
        (
            set("/name/x", json!(1)),
            "/name is neither an object nor an array",
        ),
        (remove(""), "the whole document cannot be removed"),
    ];
    for (op, message) in cases {
        // A valid first operation is rolled back too.
        let err = h
            .execute(&mut w, tx(vec![set("/layout/t", json!([0, 0])), op]))
            .unwrap_err();
        assert!(matches!(err.error, OpError::Invalid(_)), "{err}");
        assert!(err.to_string().contains(message), "{err}");
        assert_eq!(err.error.code().to_string(), "RE2005");
        assert_eq!(text(&w), before);
    }
    let closed = Op::SetDoc {
        doc: "graphs/other.graph.json".into(),
        pointer: "/name".into(),
        value: Some(json!("x")),
    };
    let err = h.execute(&mut w, tx(vec![closed])).unwrap_err();
    assert!(err.to_string().contains("is not open"), "{err}");
    assert!(w.unsaved_docs().is_empty());
}

#[test]
fn dry_runs_leave_the_document_untouched() {
    let mut s = EditState {
        world: world(),
        history: History::default(),
    };
    let mut reg: CommandRegistry<EditState> = CommandRegistry::new();
    register_builtin_commands(&mut reg);
    let before = s.world.doc(DOC).unwrap().clone();
    let args = json!({"ops": [
        {"op": "set_doc", "doc": DOC, "pointer": "/nodes/0/params/size"},
        {"op": "set_doc", "doc": DOC, "pointer": "/nodes/-", "value": {"id": "z"}}
    ]});
    let dry = reg.dispatch(&mut s, "scene.apply", args.clone(), true, &AllowAll);
    assert!(dry.ok, "{:?}", dry.diagnostics);
    assert_eq!(dry.changes.len(), 2);
    assert_eq!(s.world.doc(DOC).unwrap(), &before);
    assert!(s.history.log().is_empty());

    // A dry run that would not validate reports it and changes nothing.
    let bad = json!({"ops": [{"op": "set_doc", "doc": DOC, "pointer": "/nodes/0"}]});
    let out = reg.dispatch(&mut s, "scene.apply", bad, true, &AllowAll);
    assert!(!out.ok);
    assert_eq!(out.diagnostics[0].related[0].code.to_string(), "RE2008");
    assert_eq!(s.world.doc(DOC).unwrap(), &before);

    // The real call matches the dry run; project.status reports it unsaved.
    let status = reg.dispatch(&mut s, "project.status", json!({}), false, &AllowAll);
    assert_eq!(status.data["unsaved"], json!([]));
    let real = reg.dispatch(&mut s, "scene.apply", args, false, &AllowAll);
    assert!(real.ok);
    assert_eq!(real.changes, dry.changes);
    let status = reg.dispatch(&mut s, "project.status", json!({}), false, &AllowAll);
    assert_eq!(status.data["unsaved"], json!([DOC]));
    assert_eq!(status.data["open"][0]["path"], DOC);
    assert_eq!(status.data["open"][0]["dirty"], true);
    let undo = reg.dispatch(&mut s, "undo", json!({}), false, &AllowAll);
    assert!(undo.ok);
    assert_eq!(s.world.doc(DOC).unwrap().value, before.value);
    assert_eq!(text(&s.world), {
        let mut t = serde_json::to_string_pretty(&before.value).unwrap();
        t.push('\n');
        t
    });
}
