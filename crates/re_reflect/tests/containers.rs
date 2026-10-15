//! Container field types (ADR-0032 amendment): lists, maps and records, their
//! coercion, JSON form, text round trip, schema output, per-element diff and
//! change detection.

use std::collections::BTreeMap;

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use glam::{Quat, Vec3};
use re_reflect::{
    Color, ComponentOrigin, ComponentSchema, ComponentSchemaDoc, Doc, FieldSchema, FieldType,
    RecordField, RecordId, RecordSchema, ReflectError, Registry, Value, ValueChange, ValueError,
    component,
};

component! {
    /// A component with a list field and a map field.
    pub struct Bag {
        /// Weights, as f32.
        pub weights: Vec<f32> = vec![1.0, 2.0],
        /// Labels.
        pub labels: Vec<String> = Vec::new(),
        /// Anything.
        pub extra: BTreeMap<String, Value> = BTreeMap::new(),
    }
}

/// A test record: a 1D scale plus offset.
fn dim() -> RecordId {
    RecordSchema::new(
        "TestDim",
        vec![
            RecordField::new("scale", FieldType::F32),
            RecordField::new("offset", FieldType::I32),
        ],
    )
    .register()
    .unwrap()
}

/// A record with every scalar kind but enum.
fn wide() -> RecordId {
    RecordSchema::new(
        "TestWide",
        vec![
            RecordField::new("on", FieldType::Bool),
            RecordField::new("n", FieldType::F64),
            RecordField::new("at", FieldType::Vec3),
            RecordField::new("turn", FieldType::Quat),
            RecordField::new("tint", FieldType::Color),
            RecordField::new("label", FieldType::String),
        ],
    )
    .register()
    .unwrap()
}

fn rec(id: RecordId, values: Vec<Value>) -> Value {
    Value::Record(id, values)
}

fn json(text: &str) -> Doc {
    serde_json::from_str(text).unwrap()
}

fn no_ids(_: &str) -> Option<Entity> {
    None
}

/// Value -> Doc -> JSON text -> Doc -> Value, and the JSON text is stable.
fn round_trip(value: &Value, ty: &FieldType) -> String {
    let doc = Doc::from_value(value, ty, &|_| None);
    let text = serde_json::to_string(&doc).unwrap();
    let back: Doc = serde_json::from_str(&text).unwrap();
    let again = back.to_value(ty, &no_ids).unwrap();
    assert_eq!(&again, value, "{text}");
    assert_eq!(
        serde_json::to_string(&Doc::from_value(&again, ty, &|_| None)).unwrap(),
        text
    );
    text
}

#[test]
fn records_register_once_and_refuse_a_changed_shape() {
    let a = dim();
    assert_eq!(dim(), a, "identical registration returns the same id");
    assert_eq!(RecordId::by_name("TestDim"), Some(a));
    assert_eq!(a.name(), "TestDim");
    assert!(RecordId::all().contains(&a));
    let changed = RecordSchema::new("TestDim", vec![RecordField::new("scale", FieldType::F64)])
        .register()
        .unwrap_err();
    assert!(
        changed.to_string().contains("different fields"),
        "{changed}"
    );

    for (name, fields) in [
        ("list", vec![RecordField::new("x", FieldType::F32)]),
        ("Bad Name", vec![RecordField::new("x", FieldType::F32)]),
        ("NoFields", vec![]),
        ("HasEntity", vec![RecordField::new("e", FieldType::Entity)]),
        (
            "HasList",
            vec![RecordField::new(
                "l",
                FieldType::List(Box::new(FieldType::F32)),
            )],
        ),
        (
            "Twice",
            vec![
                RecordField::new("x", FieldType::F32),
                RecordField::new("x", FieldType::F32),
            ],
        ),
    ] {
        let e = RecordSchema::new(name, fields).register().unwrap_err();
        assert_eq!(e.code().to_string(), "RE1011", "{name}: {e}");
    }
}

#[test]
fn list_coercion_rounds_items_and_names_the_bad_index() {
    let ty = FieldType::List(Box::new(FieldType::F32));
    assert_eq!(
        ty.coerce(Value::List(vec![Value::Int(1), Value::Float(0.1)]))
            .unwrap(),
        Value::List(vec![Value::Float(1.0), Value::Float(0.1f32 as f64)])
    );
    let e = ty
        .coerce(Value::List(vec![Value::Float(1.0), Value::Str("x".into())]))
        .unwrap_err();
    assert_eq!(e.to_string(), "at [1]: expected f32, found string");
    assert_eq!(e.code().to_string(), "RE1003");
    let e = ty.coerce(Value::Float(1.0)).unwrap_err();
    assert_eq!(e.to_string(), "expected list of f32, found number");
}

#[test]
fn nesting_rules_are_checked() {
    assert!(
        FieldType::List(Box::new(FieldType::String))
            .validate()
            .is_ok()
    );
    assert!(
        FieldType::List(Box::new(FieldType::Record(dim())))
            .validate()
            .is_ok()
    );
    for bad in [
        FieldType::List(Box::new(FieldType::Entity)),
        FieldType::List(Box::new(FieldType::Map)),
        FieldType::List(Box::new(FieldType::List(Box::new(FieldType::F32)))),
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
        let mut world = World::new();
        let mut registry = Registry::default();
        let schema = ComponentSchema {
            name: "Nested".into(),
            doc: String::new(),
            fields: vec![FieldSchema {
                name: "x".into(),
                ty: bad.clone(),
                doc: String::new(),
                default: Value::List(vec![]),
            }],
            origin: ComponentOrigin::Script,
        };
        let e = registry.register_script(&mut world, schema).unwrap_err();
        assert_eq!(e.code().to_string(), "RE1011", "{e}");
    }
}

#[test]
fn map_values_are_dynamic_with_integers_as_floats() {
    let id = dim();
    let input = Value::Map(BTreeMap::from([
        ("n".to_string(), Value::Int(5)),
        (
            "q".to_string(),
            Value::Quat(Quat::from_xyzw(0.0, 0.0, 0.0, 2.0)),
        ),
        (
            "r".to_string(),
            rec(id, vec![Value::Int(1), Value::Float(3.0)]),
        ),
        (
            "l".to_string(),
            Value::List(vec![Value::Int(1), Value::Str("a".into())]),
        ),
    ]));
    let Value::Map(out) = FieldType::Map.coerce(input).unwrap() else {
        panic!()
    };
    assert_eq!(out["n"], Value::Float(5.0));
    assert_eq!(out["q"], Value::Quat(Quat::IDENTITY));
    assert_eq!(out["r"], rec(id, vec![Value::Float(1.0), Value::Int(3)]));
    assert_eq!(
        out["l"],
        Value::List(vec![Value::Float(1.0), Value::Str("a".into())])
    );

    let refused = |v: Value, msg: &str| {
        let e = FieldType::Map
            .coerce(Value::Map(BTreeMap::from([("k".to_string(), v)])))
            .unwrap_err();
        assert!(e.to_string().contains(msg), "{e}");
        assert!(e.to_string().starts_with("at [\"k\"]"), "{e}");
    };
    refused(Value::Map(BTreeMap::new()), "cannot hold another map");
    refused(Value::Entity(None), "entity reference");
    refused(Value::Float(f64::NAN), "finite");
    refused(
        Value::List(vec![Value::List(vec![])]),
        "cannot hold another list",
    );
}

#[test]
fn record_coercion_checks_type_arity_and_fields() {
    let id = dim();
    let ty = FieldType::Record(id);
    assert_eq!(
        ty.coerce(rec(id, vec![Value::Float(0.5), Value::Float(4.0)]))
            .unwrap(),
        rec(id, vec![Value::Float(0.5), Value::Int(4)])
    );
    let e = ty
        .coerce(rec(id, vec![Value::Float(0.5), Value::Float(4.5)]))
        .unwrap_err();
    assert_eq!(e.to_string(), "at .offset: expected an integer, found 4.5");
    assert_eq!(e.code().to_string(), "RE1005");
    let e = ty.coerce(rec(id, vec![Value::Float(0.5)])).unwrap_err();
    assert!(e.to_string().contains("has 2 fields"), "{e}");
    let other = wide();
    let e = ty.coerce(other_default(other)).unwrap_err();
    assert_eq!(
        e,
        ValueError::TypeMismatch {
            expected: "record TestDim".into(),
            found: "record TestWide".into()
        }
    );
    assert_eq!(
        ty.default_value(),
        rec(id, vec![Value::Float(0.0), Value::Int(0)])
    );
}

fn other_default(id: RecordId) -> Value {
    FieldType::Record(id).default_value()
}

#[test]
fn json_forms_round_trip_through_text() {
    let id = dim();
    let w = wide();
    let list = FieldType::List(Box::new(FieldType::F32));
    assert_eq!(
        round_trip(
            &Value::List(vec![Value::Float(0.1f32 as f64), Value::Float(2.0)]),
            &list
        ),
        "[0.1,2.0]"
    );
    let records = FieldType::List(Box::new(FieldType::Record(id)));
    assert_eq!(
        round_trip(
            &Value::List(vec![rec(id, vec![Value::Float(0.5), Value::Int(-3)])]),
            &records
        ),
        r#"[{"scale":0.5,"offset":-3}]"#
    );
    let wide_value = rec(
        w,
        vec![
            Value::Bool(true),
            Value::Float(0.1),
            Value::Vec3(Vec3::new(1.0, 2.0, 3.0)),
            Value::Quat(Quat::IDENTITY),
            Value::Color(Color::rgba(0.25, 0.5, 1.0, 1.0)),
            Value::Str("hi".into()),
        ],
    );
    round_trip(&wide_value, &FieldType::Record(w));

    let map = Value::Map(BTreeMap::from([
        ("b".to_string(), Value::Bool(false)),
        ("n".to_string(), Value::Float(5.0)),
        ("s".to_string(), Value::Str("x".into())),
        ("v".to_string(), Value::Vec3(Vec3::new(1.0, 0.5, -2.0))),
        ("q".to_string(), Value::Quat(Quat::IDENTITY)),
        ("c".to_string(), Value::Color(Color::WHITE)),
        ("e".to_string(), Value::Enum("Neon".into())),
        (
            "l".to_string(),
            Value::List(vec![
                Value::Float(1.0),
                rec(id, vec![Value::Float(1.0), Value::Int(2)]),
            ]),
        ),
        ("r".to_string(), wide_value.clone()),
    ]));
    let text = round_trip(&map, &FieldType::Map);
    assert!(text.contains(r#""n":5.0"#), "{text}");
    assert!(
        text.contains(r#""v":{"$type":"vec3","value":[1.0,0.5,-2.0]}"#),
        "{text}"
    );
    assert!(
        text.contains(r#""e":{"$type":"enum","value":"Neon"}"#),
        "{text}"
    );
    assert!(
        text.contains(r#""r":{"$type":"record","name":"TestWide","value":{"on":true"#),
        "{text}"
    );
}

#[test]
fn json_input_forms_and_errors() {
    let id = dim();
    let ty = FieldType::Record(id);
    let want = rec(id, vec![Value::Float(1.0), Value::Int(2)]);
    assert_eq!(json("[1, 2]").to_value(&ty, &no_ids).unwrap(), want);
    assert_eq!(
        json(r#"{"offset": 2, "scale": 1}"#)
            .to_value(&ty, &no_ids)
            .unwrap(),
        want
    );
    let e = json(r#"{"scale": 1, "ofset": 2}"#)
        .to_value(&ty, &no_ids)
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        r#"record TestDim has no field "ofset" (did you mean "offset"?)"#
    );
    let e = json(r#"{"scale": 1}"#).to_value(&ty, &no_ids).unwrap_err();
    assert!(e.to_string().contains("needs field \"offset\""), "{e}");

    // Integers in a map are floats.
    assert_eq!(
        json(r#"{"n": 3}"#)
            .to_value(&FieldType::Map, &no_ids)
            .unwrap(),
        Value::Map(BTreeMap::from([("n".to_string(), Value::Float(3.0))]))
    );
    for (text, msg) in [
        (r#"{"k": [1, 2]}"#, "object with `$type`"),
        (r#"{"k": null}"#, "found null"),
        (r#"{"k": {"x": 1}}"#, "object with `$type`"),
        (
            r#"{"k": {"$type": "vec4", "value": 1}}"#,
            "unknown map value type",
        ),
        (r#"{"k": {"$type": "vec3"}}"#, "needs `value`"),
        (
            r#"{"k": {"$type": "vec3", "value": [1, 2, 3], "x": 1}}"#,
            "unexpected key",
        ),
        (
            r#"{"k": {"$type": "record", "name": "Nope", "value": {}}}"#,
            "no record type",
        ),
        (
            r#"{"k": {"$type": "list", "value": [{"$type": "list", "value": []}]}}"#,
            "lists cannot nest",
        ),
    ] {
        let e = json(text).to_value(&FieldType::Map, &no_ids).unwrap_err();
        assert!(e.to_string().contains(msg), "{text}: {e}");
        assert!(e.to_string().starts_with("at [\"k\"]"), "{text}: {e}");
    }
    let e = json("[1, \"a\"]")
        .to_value(&FieldType::List(Box::new(FieldType::I32)), &no_ids)
        .unwrap_err();
    assert_eq!(e.to_string(), "at [1]: expected i32, found string \"a\"");
}

#[test]
fn diff_reports_indices_keys_and_fields() {
    let id = dim();
    let l = |v: &[f64]| Value::List(v.iter().map(|x| Value::Float(*x)).collect());
    assert_eq!(l(&[1.0, 2.0]).diff(&l(&[1.0, 2.0])), None);
    assert_eq!(
        l(&[1.0, 2.0, 3.0]).diff(&l(&[1.0, 5.0])),
        Some(ValueChange::Indices(vec![1, 2]))
    );
    assert_eq!(l(&[]).diff(&l(&[7.0])), Some(ValueChange::Indices(vec![0])));
    let m = |pairs: &[(&str, f64)]| {
        Value::Map(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), Value::Float(*v)))
                .collect(),
        )
    };
    assert_eq!(
        m(&[("a", 1.0), ("b", 2.0), ("c", 3.0)]).diff(&m(&[("a", 1.0), ("b", 9.0), ("d", 4.0)])),
        Some(ValueChange::Keys(vec!["b".into(), "c".into(), "d".into()]))
    );
    assert_eq!(
        rec(id, vec![Value::Float(1.0), Value::Int(2)])
            .diff(&rec(id, vec![Value::Float(1.0), Value::Int(3)])),
        Some(ValueChange::Fields(vec![1]))
    );
    assert_eq!(
        Value::Float(1.0).diff(&Value::Float(2.0)),
        Some(ValueChange::Whole)
    );
    assert_eq!(l(&[]).diff(&m(&[])), Some(ValueChange::Whole));
}

#[test]
fn native_container_fields_read_write_and_skip_equal_writes() {
    use bevy_ecs::query::Changed;
    let mut world = World::new();
    let mut registry = Registry::default();
    let key = registry.register::<Bag>(&mut world);
    let e = world.spawn(Bag::default()).id();
    assert_eq!(
        registry.get_field(&world, e, key, 0),
        Some(Value::List(vec![Value::Float(1.0), Value::Float(2.0)]))
    );
    world.clear_trackers();
    let mut q = world.query_filtered::<Entity, Changed<Bag>>();
    registry
        .set_field(
            &mut world,
            e,
            key,
            0,
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        )
        .unwrap();
    assert_eq!(q.iter(&world).count(), 0, "an equal list trips nothing");
    registry
        .set_field(
            &mut world,
            e,
            key,
            2,
            Value::Map(BTreeMap::from([("hp".to_string(), Value::Int(10))])),
        )
        .unwrap();
    assert_eq!(q.iter(&world).count(), 1);
    assert_eq!(
        world.get::<Bag>(e).unwrap().extra["hp"],
        Value::Float(10.0),
        "coerced on the way in"
    );
    world.clear_trackers();
    registry
        .set_field(
            &mut world,
            e,
            key,
            2,
            Value::Map(BTreeMap::from([("hp".to_string(), Value::Float(10.0))])),
        )
        .unwrap();
    assert_eq!(q.iter(&world).count(), 0, "an equal map trips nothing");
    let err = registry
        .set_field(&mut world, e, key, 1, Value::List(vec![Value::Bool(true)]))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Bag.labels: at [0]: expected string, found bool"
    );
    assert!(matches!(err, ReflectError::Value { .. }));
}

#[test]
fn script_components_with_containers_round_trip_their_schema() {
    let id = dim();
    let schema = ComponentSchema {
        name: "Layout".into(),
        doc: "A test layout.".into(),
        fields: vec![
            FieldSchema {
                name: "size".into(),
                ty: FieldType::Record(id),
                doc: "The size.".into(),
                default: rec(id, vec![Value::Float(1.0), Value::Int(0)]),
            },
            FieldSchema {
                name: "points".into(),
                ty: FieldType::List(Box::new(FieldType::Record(id))),
                doc: String::new(),
                default: Value::List(vec![]),
            },
            FieldSchema {
                name: "meta".into(),
                ty: FieldType::Map,
                doc: String::new(),
                default: Value::Map(BTreeMap::from([("k".into(), Value::Int(1))])),
            },
        ],
        origin: ComponentOrigin::Script,
    };
    let doc = ComponentSchemaDoc::from(&schema);
    let text = serde_json::to_string(&doc).unwrap();
    assert!(
        text.contains(r#""type":{"kind":"list","item":{"kind":"record","record":"TestDim"}}"#),
        "{text}"
    );
    let back: ComponentSchemaDoc = serde_json::from_str(&text).unwrap();
    let mut world = World::new();
    let mut registry = Registry::default();
    let key = registry
        .register_script(&mut world, back.to_schema().unwrap())
        .unwrap();
    let registered = registry.schema(key);
    assert!(
        registered.same_shape(&ComponentSchema {
            fields: schema
                .fields
                .iter()
                .map(|f| FieldSchema {
                    default: f.ty.coerce(f.default.clone()).unwrap(),
                    ..f.clone()
                })
                .collect(),
            ..schema.clone()
        })
    );
    let e = world.spawn_empty().id();
    registry
        .insert(&mut world, e, key, registry.schema(key).defaults())
        .unwrap();
    registry
        .set_field(
            &mut world,
            e,
            key,
            1,
            Value::List(vec![rec(id, vec![Value::Float(2.0), Value::Int(4)])]),
        )
        .unwrap();
    assert_eq!(
        registry.get_field(&world, e, key, 1),
        Some(Value::List(vec![rec(
            id,
            vec![Value::Float(2.0), Value::Int(4)]
        )]))
    );

    // Bad type documents.
    for (text, msg) in [
        (r#"{"kind": "list"}"#, "needs `item`"),
        (r#"{"kind": "record"}"#, "needs `record`"),
        (r#"{"kind": "record", "record": "Nope"}"#, "no record type"),
        (
            r#"{"kind": "f32", "item": {"kind": "f32"}}"#,
            "only valid for a list",
        ),
        (
            r#"{"kind": "list", "item": {"kind": "entity"}}"#,
            "a list holds scalars or records",
        ),
    ] {
        let doc: re_reflect::FieldTypeDoc = serde_json::from_str(text).unwrap();
        let e = doc.to_field_type().unwrap_err();
        assert!(e.contains(msg), "{text}: {e}");
    }
}

#[test]
fn describe_and_scene_schema_cover_the_new_types() {
    let id = dim();
    let mut world = World::new();
    let mut registry = Registry::default();
    registry.register::<Bag>(&mut world);
    let d = registry.describe();
    let bag = &d["components"][0];
    assert_eq!(
        bag["fields"][0]["type"],
        serde_json::json!({"kind": "list", "item": {"kind": "f32"}})
    );
    assert_eq!(bag["fields"][0]["default"], serde_json::json!([1.0, 2.0]));
    assert_eq!(bag["fields"][2]["type"], serde_json::json!({"kind": "map"}));
    let records = d["records"].as_array().unwrap();
    let dim_doc = records
        .iter()
        .find(|r| r["name"] == id.name())
        .expect("records are described");
    assert_eq!(dim_doc["fields"][1]["type"]["kind"], "i32");

    let js = registry.scene_json_schema();
    let props = &js["$defs"]["Bag"]["properties"];
    assert_eq!(props["weights"]["type"], "array");
    assert_eq!(props["weights"]["items"]["type"], "number");
    assert_eq!(props["extra"]["type"], "object");
    let kinds = &js["properties"]["script_components"]["items"]["properties"]["fields"]["items"]["properties"]
        ["type"]["properties"]["kind"]["enum"];
    for k in ["list", "map", "record"] {
        assert!(kinds.as_array().unwrap().iter().any(|x| x == k), "{k}");
    }
}
