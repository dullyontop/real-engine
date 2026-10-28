//! `proc.revert` (the ADR-0044 amendment, §8): a generated entity's field
//! overrides go and its fields take what the source generates, and an entity
//! the source keeps a deletion override for comes back as the source
//! generates it. Each is one undo step that puts the overrides and the
//! values back exactly, and nothing regenerates: what the command puts back
//! is what a regeneration makes.

use std::sync::Arc;

use re_command::{AllowAll, CommandOutcome, CommandRegistry, EditState, LogKind};
use re_proc::register_commands;
use re_reflect::{Value, component};
use re_scene::ecs as bevy_ecs;
use re_scene::{GenNode, GeneratorInfo, SaveOptions, World, hierarchy};
use serde_json::{Value as Json, json};

component! {
    /// A component some generated entities carry.
    pub struct Glow {
        /// How bright.
        pub intensity: f64 = 1.0,
    }
}

component! {
    /// The test generator's parameters.
    pub struct Rows {
        /// Whether `a` (and its child `a/x`) is generated.
        pub with_a: bool = true,
    }
}

fn node(key: &str, parent: Option<&str>, x: f32) -> GenNode {
    GenNode {
        key: key.into(),
        name: key.to_uppercase(),
        parent_key: parent.map(Into::into),
        components: vec![(
            "Transform".into(),
            vec![("translation".into(), Value::Vec3([x, 0.0, 0.0].into()))],
        )],
    }
}

/// An edit state whose source `src` generates `a` (with child `a/x`), `b`
/// (glowing at 2) and `c` (glowing at the default), and whose source `far`
/// is streamed.
fn world() -> EditState {
    let mut st = EditState::new();
    st.world.registry.register::<Rows>(&mut st.world.ecs);
    st.world.registry.register::<Glow>(&mut st.world.ecs);
    st.world.generators.register(GeneratorInfo {
        name: "rows".into(),
        doc: String::new(),
        params_component: "Rows".into(),
        run: Arc::new(|ctx| {
            let mut out = Vec::new();
            if ctx.param::<bool>("with_a")? {
                out.push(node("a", None, 0.0));
                out.push(node("a/x", Some("a"), 0.5));
            }
            let mut b = node("b", None, 1.0);
            b.components
                .push(("Glow".into(), vec![("intensity".into(), Value::Float(2.0))]));
            out.push(b);
            let mut c = node("c", None, 2.0);
            c.components.push(("Glow".into(), Vec::new()));
            out.push(c);
            Ok(out)
        }),
        fingerprint_extra: None,
        version: 1,
        oldest: 1,
    });
    let scene = json!({
        "format": "real-engine/scene",
        "version": 1,
        "entities": [
            {
                "id": "src",
                "components": {
                    "Procedural": { "generator": "rows" },
                    "Rows": {},
                    "Transform": {}
                }
            },
            {
                "id": "far",
                "components": {
                    "Procedural": { "generator": "rows" },
                    "Rows": {},
                    "Streamed": {},
                    "Transform": {}
                }
            }
        ]
    });
    st.world
        .load(&re_scene::scene::from_json(&scene.to_string()).unwrap())
        .unwrap();
    let report = st.world.regenerate_changed();
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    st
}

fn run(st: &mut EditState, command: &str, args: Json, dry_run: bool) -> CommandOutcome {
    let mut r = CommandRegistry::new();
    re_command::register_builtin_commands(&mut r);
    register_commands(&mut r);
    r.dispatch(st, command, args, dry_run, &AllowAll)
}

fn ok(st: &mut EditState, command: &str, args: Json) -> Json {
    let o = run(st, command, args, false);
    assert!(o.ok, "{command}: {:#?}", o.diagnostics);
    o.data
}

fn revert(st: &mut EditState, args: Json) -> Json {
    ok(st, "proc.revert", args)
}

fn set(st: &mut EditState, id: &str, component: &str, field: &str, value: Json) {
    ok(
        st,
        "entity.set",
        json!({ "id": id, "component": component, "field": field, "value": value }),
    );
}

fn delete(st: &mut EditState, id: &str) {
    ok(st, "entity.delete", json!({ "id": id }));
}

fn saved(w: &mut World) -> String {
    re_scene::scene::to_json(&w.save(SaveOptions::default()))
}

/// The ids of `id`'s children, in order.
fn children(w: &World, id: &str) -> Vec<String> {
    let e = w.entity(id).unwrap();
    hierarchy::children(&w.ecs, e)
        .iter()
        .map(|c| w.id_of(*c).unwrap().to_string())
        .collect()
}

/// The overrides `src` keeps, as its scene file writes them.
fn overrides(w: &mut World) -> Json {
    let doc: Json = serde_json::from_str(&saved(w)).unwrap();
    doc["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "src")
        .map_or(Json::Null, |e| e["overrides"].clone())
}

fn get(w: &World, id: &str, component: &str, field: &str) -> Value {
    w.get(id, component, field).unwrap()
}

/// How many transactions were applied: each is an undo step.
fn steps(st: &EditState) -> usize {
    st.history
        .log()
        .iter()
        .filter(|e| e.kind == LogKind::Do)
        .count()
}

/// Nothing regenerates, and a forced regeneration changes nothing: what
/// `proc.revert` put back is what the source generates.
fn generated_as_is(st: &mut EditState) {
    assert!(st.world.regenerate_changed().sources.is_empty());
    let before = saved(&mut st.world);
    let hash = st.world.state_hash();
    let src = st.world.entity("src").unwrap();
    let report = st.world.regenerate(src);
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    assert_eq!(saved(&mut st.world), before);
    assert_eq!(st.world.state_hash(), hash);
}

fn code(o: &CommandOutcome) -> String {
    assert!(!o.ok, "expected a refusal: {:#?}", o.data);
    o.diagnostics[0].code.to_string()
}

#[test]
fn field_overrides_go_back_to_what_the_source_generates_and_undo_puts_them_back() {
    let mut st = world();
    let before = saved(&mut st.world);
    let hash = st.world.state_hash();
    set(
        &mut st,
        "src/b",
        "Transform",
        "translation",
        json!([9, 0, 0]),
    );
    set(&mut st, "src/b", "Transform", "scale", json!([2, 2, 2]));
    set(&mut st, "src/c", "Glow", "intensity", json!(5.0));
    set(&mut st, "src/b", "Glow", "intensity", json!(3.0));
    let edited = saved(&mut st.world);
    let edited_hash = st.world.state_hash();
    assert_eq!(overrides(&mut st.world).as_array().unwrap().len(), 4);
    let edits = steps(&st);

    // A dry run says what it would put back, and changes nothing.
    let one = json!({ "id": "src/b", "component": "Transform", "field": "translation" });
    let o = run(&mut st, "proc.revert", one.clone(), true);
    assert!(o.ok, "{:#?}", o.diagnostics);
    assert_eq!(o.data["dry_run"], true);
    assert_eq!(st.world.state_hash(), edited_hash);
    assert_eq!(steps(&st), edits);

    // One field.
    let data = revert(&mut st, one);
    assert_eq!(
        data,
        json!({
            "id": "src/b",
            "source": "src",
            "key": "b",
            "reverted": [
                { "component": "Transform", "field": "translation", "value": [1.0, 0.0, 0.0] }
            ]
        })
    );
    assert_eq!(
        get(&st.world, "src/b", "Transform", "translation"),
        Value::Vec3([1.0, 0.0, 0.0].into())
    );
    assert_eq!(
        overrides(&mut st.world),
        json!([
            { "target": "b", "component": "Transform", "field": "scale", "value": [2.0, 2.0, 2.0] },
            { "target": "c", "component": "Glow", "field": "intensity", "value": 5.0 },
            { "target": "b", "component": "Glow", "field": "intensity", "value": 3.0 }
        ])
    );
    assert_eq!(steps(&st), edits + 1, "one undo step");

    // Every override of an entity: a field the source does not set takes
    // its default.
    let data = revert(&mut st, json!({ "id": "src/b" }));
    assert_eq!(
        data["reverted"],
        json!([
            { "component": "Transform", "field": "scale", "value": [1.0, 1.0, 1.0] },
            { "component": "Glow", "field": "intensity", "value": 2.0 }
        ])
    );
    assert_eq!(
        get(&st.world, "src/b", "Transform", "scale"),
        Value::Vec3([1.0, 1.0, 1.0].into())
    );
    assert_eq!(
        get(&st.world, "src/b", "Glow", "intensity"),
        Value::Float(2.0)
    );
    generated_as_is(&mut st);

    // One component's.
    revert(&mut st, json!({ "id": "src/c", "component": "Glow" }));
    assert_eq!(
        get(&st.world, "src/c", "Glow", "intensity"),
        Value::Float(1.0)
    );
    assert_eq!(overrides(&mut st.world), Json::Null);
    assert_eq!(saved(&mut st.world), before);
    assert_eq!(st.world.state_hash(), hash);
    generated_as_is(&mut st);

    // Undo puts every override back where it was in the list, with its value.
    for _ in 0..3 {
        assert!(st.history.undo(&mut st.world).unwrap());
    }
    assert_eq!(saved(&mut st.world), edited);
    assert_eq!(st.world.state_hash(), edited_hash);
    for _ in 0..3 {
        assert!(st.history.redo(&mut st.world).unwrap());
    }
    assert_eq!(st.world.state_hash(), hash);
}

#[test]
fn a_deleted_entity_comes_back_as_the_source_generates_it() {
    let mut st = world();
    // An authored child of the source comes before the generated ones.
    ok(
        &mut st,
        "entity.spawn",
        json!({ "id": "sign", "parent": "src", "components": { "Transform": {} } }),
    );
    let src = st.world.entity("src").unwrap();
    st.world.regenerate(src);
    assert_eq!(
        children(&st.world, "src"),
        ["sign", "src/a", "src/b", "src/c"]
    );
    set(
        &mut st,
        "src/a/x",
        "Transform",
        "translation",
        json!([7, 0, 0]),
    );
    let before = saved(&mut st.world);
    let hash = st.world.state_hash();

    for (id, key) in [("src/a", "a"), ("src/b", "b"), ("src/c", "c")] {
        delete(&mut st, id);
        let deleted = saved(&mut st.world);
        let deleted_hash = st.world.state_hash();
        let o = run(
            &mut st,
            "proc.revert",
            json!({ "source": "src", "key": key }),
            true,
        );
        assert!(o.ok, "{:#?}", o.diagnostics);
        assert_eq!(
            st.world.state_hash(),
            deleted_hash,
            "a dry run changes nothing"
        );

        let steps_before = steps(&st);
        let data = revert(&mut st, json!({ "source": "src", "key": key }));
        assert_eq!(data["id"], id);
        assert_eq!(steps(&st), steps_before + 1, "one undo step");
        assert_eq!(
            children(&st.world, "src"),
            ["sign", "src/a", "src/b", "src/c"]
        );
        assert_eq!(saved(&mut st.world), before, "{id}");
        assert_eq!(st.world.state_hash(), hash, "{id}");
        generated_as_is(&mut st);

        assert!(st.history.undo(&mut st.world).unwrap());
        assert_eq!(saved(&mut st.world), deleted, "{id}");
        assert_eq!(st.world.state_hash(), deleted_hash, "{id}");
        assert!(st.history.redo(&mut st.world).unwrap());
        assert_eq!(st.world.state_hash(), hash, "{id}");
    }

    // The deleted entity's descendants come back with it, with their field
    // overrides.
    delete(&mut st, "src/a");
    let data = revert(&mut st, json!({ "source": "src", "key": "a" }));
    assert_eq!(data["restored"], json!(["src/a", "src/a/x"]));
    assert_eq!(children(&st.world, "src/a"), ["src/a/x"]);
    assert_eq!(
        get(&st.world, "src/a/x", "Transform", "translation"),
        Value::Vec3([7.0, 0.0, 0.0].into())
    );
    assert_eq!(st.world.state_hash(), hash);
}

#[test]
fn a_descendant_deleted_too_stays_deleted_until_it_is_reverted() {
    let mut st = world();
    let before = saved(&mut st.world);
    delete(&mut st, "src/a/x");
    delete(&mut st, "src/a");

    // `a/x` cannot come back while `a` is deleted.
    let hash = st.world.state_hash();
    let o = run(
        &mut st,
        "proc.revert",
        json!({ "source": "src", "key": "a/x" }),
        false,
    );
    assert_eq!(code(&o), "RE4093");
    assert!(
        o.diagnostics[0].message.contains("restore `a` first"),
        "{}",
        o.diagnostics[0].message
    );
    assert_eq!(st.world.state_hash(), hash);

    // `a` comes back without it, and then it comes back under `a`.
    let data = revert(&mut st, json!({ "source": "src", "key": "a" }));
    assert_eq!(data["restored"], json!(["src/a"]));
    assert!(children(&st.world, "src/a").is_empty());
    generated_as_is(&mut st);
    revert(&mut st, json!({ "source": "src", "key": "a/x" }));
    assert_eq!(children(&st.world, "src/a"), ["src/a/x"]);
    assert_eq!(saved(&mut st.world), before);
    generated_as_is(&mut st);

    // Suppressed for the play session: a regeneration would not make it.
    delete(&mut st, "src/a/x");
    st.world.suppress("src/a").unwrap();
    let o = run(
        &mut st,
        "proc.revert",
        json!({ "source": "src", "key": "a/x" }),
        false,
    );
    assert_eq!(code(&o), "RE4093");
    assert!(
        o.diagnostics[0].message.contains("suppressed"),
        "{}",
        o.diagnostics[0].message
    );
}

#[test]
fn a_moved_entity_comes_back_where_its_structural_override_puts_it() {
    let mut st = world();
    ok(
        &mut st,
        "proc.move",
        json!({ "id": "src/b", "parent": "src/a", "index": 0 }),
    );
    assert_eq!(children(&st.world, "src/a"), ["src/b", "src/a/x"]);
    set(
        &mut st,
        "src/b",
        "Transform",
        "translation",
        json!([4, 0, 0]),
    );
    let moved = saved(&mut st.world);

    // The generated value is the source's, with its structure.
    revert(
        &mut st,
        json!({ "id": "src/b", "component": "Transform", "field": "translation" }),
    );
    assert_eq!(
        get(&st.world, "src/b", "Transform", "translation"),
        Value::Vec3([1.0, 0.0, 0.0].into())
    );
    generated_as_is(&mut st);
    assert!(st.history.undo(&mut st.world).unwrap());
    assert_eq!(saved(&mut st.world), moved);

    delete(&mut st, "src/b");
    assert_eq!(children(&st.world, "src/a"), ["src/a/x"]);
    revert(&mut st, json!({ "source": "src", "key": "b" }));
    assert_eq!(children(&st.world, "src/a"), ["src/b", "src/a/x"]);
    assert_eq!(saved(&mut st.world), moved);
    generated_as_is(&mut st);
}

#[test]
fn what_cannot_be_reverted_is_refused_and_nothing_changes() {
    let mut st = world();
    set(
        &mut st,
        "src/b",
        "Transform",
        "translation",
        json!([9, 0, 0]),
    );
    delete(&mut st, "src/a");
    ok(
        &mut st,
        "scene.apply",
        json!({ "ops": [
            { "op": "set_override", "source": "far", "target": "c/0_0/x", "deleted": true }
        ] }),
    );
    let hash = st.world.state_hash();
    let edits = steps(&st);
    let cases = [
        // No override of that field, component or entity.
        (json!({ "id": "src/c" }), "RE4092"),
        (json!({ "id": "src/b", "component": "Glow" }), "RE4092"),
        (
            json!({ "id": "src/b", "component": "Transform", "field": "scale" }),
            "RE4092",
        ),
        // Not generated: an authored entity keeps no overrides.
        (json!({ "id": "src" }), "RE4092"),
        // No deletion override, or not a procedural source.
        (json!({ "source": "src", "key": "b" }), "RE4092"),
        (json!({ "source": "src/b", "key": "x" }), "RE4092"),
        // An entity of a stream cell.
        (json!({ "source": "far", "key": "c/0_0/x" }), "RE4093"),
        // Unknown ids.
        (json!({ "id": "src/nope" }), "RE1200"),
        (json!({ "source": "nope", "key": "a" }), "RE1200"),
        // Arguments that do not make one of the two forms.
        (json!({}), "RE2005"),
        (json!({ "id": "src/b", "field": "translation" }), "RE2005"),
        (json!({ "source": "src" }), "RE2005"),
        (
            json!({ "id": "src/b", "source": "src", "key": "a" }),
            "RE2005",
        ),
        (
            json!({ "source": "src", "key": "a", "component": "Transform" }),
            "RE2005",
        ),
    ];
    for (args, expected) in cases {
        let o = run(&mut st, "proc.revert", args.clone(), false);
        assert_eq!(code(&o), expected, "{args}: {:#?}", o.diagnostics);
        assert_eq!(steps(&st), edits, "{args}");
        assert_eq!(st.world.state_hash(), hash, "{args}");
    }

    // The source no longer generates the key: the override is an orphan.
    st.world
        .set("src", "Rows", "with_a", Value::Bool(false))
        .unwrap();
    st.world.regenerate_changed();
    let hash = st.world.state_hash();
    let o = run(
        &mut st,
        "proc.revert",
        json!({ "source": "src", "key": "a" }),
        false,
    );
    assert_eq!(code(&o), "RE4093");
    assert_eq!(st.world.state_hash(), hash);
}
