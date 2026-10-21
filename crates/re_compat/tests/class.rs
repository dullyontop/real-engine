//! The class layer: data checks, inheritance, member lookup and property
//! access through every storage kind.

mod common;

use common::{TEST_CLASSES, test_builder, test_registry};
use glam::Vec3;
use re_compat::data::{ClassFile, EnumFile, LibraryFile};
use re_compat::{ClassRegistry, ClassRegistryBuilder, CompatError, Member, Status, codes, scene};
use re_reflect::Value;
use re_scene::World;

fn code(e: &CompatError) -> String {
    e.code().to_string()
}

#[test]
fn builtin_registry_builds_with_the_core_tree() {
    let r = ClassRegistry::builtin().unwrap();
    for c in [
        "Instance",
        "Folder",
        "Model",
        "Configuration",
        "ModuleScript",
        "Script",
        "LocalScript",
        "DataModel",
    ] {
        assert!(r.try_class(c).is_some(), "{c}");
    }
    assert!(r.is_a("LocalScript", "Script"));
    assert!(r.is_a("LocalScript", "BaseScript"));
    assert!(r.is_a("LocalScript", "Instance"));
    assert!(r.is_a("Folder", "Folder"));
    assert!(!r.is_a("Folder", "Model"));
    assert!(!r.is_a("Nope", "Instance"));
    let names: Vec<&str> = r
        .ancestry("ModuleScript")
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, ["ModuleScript", "LuaSourceContainer", "Instance"]);
    // Scripts start, stop and start again with Enabled (WP11 "runtime",
    // WP27 "reload"); modules load; Source is not built.
    for c in ["BaseScript", "Script", "LocalScript"] {
        assert_eq!(r.class(c).unwrap().status, Status::Implemented, "{c}");
    }
    for c in ["LuaSourceContainer", "ModuleScript"] {
        assert_eq!(r.class(c).unwrap().status, Status::Partial, "{c}");
    }
}

#[test]
fn members_resolve_through_superclasses_with_did_you_mean() {
    let r = test_registry();
    let m = r.member("LocalScript", "RunContext").unwrap();
    assert_eq!(m.class.name, "BaseScript");
    assert_eq!(m.path(), "BaseScript.RunContext");
    assert!(matches!(m.member, Member::Property(_)));

    let e = r.member("TestBlock", "Positon").unwrap_err();
    assert_eq!(code(&e), "RE9001");
    assert!(e.to_string().contains("TestBlock.Positon"), "{e}");
    assert!(e.to_string().contains("did you mean \"Position\""), "{e}");
    assert!(e.to_diagnostic().hint.is_some());

    // Inherited from Instance through the test file's extension.
    let m = r.member("TestBlock", "Archivable").unwrap();
    assert_eq!(m.class.name, "Instance");

    let e = r.class("Foldr").unwrap_err();
    assert_eq!(code(&e), "RE9001");
    assert!(e.to_string().contains("did you mean \"Folder\""), "{e}");
}

#[test]
fn deprecated_members_warn_with_their_replacement() {
    let r = test_registry();
    let m = r.member("Folder", "children").unwrap();
    let d = m.deprecation().unwrap();
    assert_eq!(d.code, codes::DEPRECATED_MEMBER);
    assert!(!d.is_error());
    assert_eq!(d.hint.as_deref(), Some("use GetChildren"));
    assert!(
        r.member("Folder", "Archivable")
            .unwrap()
            .deprecation()
            .is_none()
    );
}

fn block(world: &mut World, r: &ClassRegistry) -> re_scene::Entity {
    r.register_components(world).unwrap();
    let game = scene::spawn_instance(world, r, "DataModel", Some("game"), "game", None).unwrap();
    scene::spawn_instance(world, r, "TestBlock", Some("block"), "Block", Some(game)).unwrap()
}

#[test]
fn properties_read_and_write_through_every_storage() {
    let r = test_registry();
    let mut w = World::new();
    let e = block(&mut w, &r);

    // Own storage, with data-file defaults.
    assert_eq!(r.get_property(&w, e, "Count").unwrap(), Value::Int(3));
    assert_eq!(
        r.get_property(&w, e, "Archivable").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        r.get_property(&w, e, "Mode").unwrap(),
        Value::Enum("Server".into())
    );
    r.set_property(&mut w, e, "Count", Value::Int(7)).unwrap();
    assert_eq!(
        w.get("block", "Compat_TestBlock", "Count").unwrap(),
        Value::Int(7)
    );
    r.set_property(&mut w, e, "Archivable", Value::Bool(false))
        .unwrap();
    assert_eq!(
        w.get("block", "Compat_Instance", "Archivable").unwrap(),
        Value::Bool(false)
    );

    // Alias storage, plain and through a converter.
    r.set_property(&mut w, e, "Position", Value::Vec3(Vec3::new(1.0, 2.0, 3.0)))
        .unwrap();
    assert_eq!(
        w.get("block", "Transform", "translation").unwrap(),
        Value::Vec3(Vec3::new(1.0, 2.0, 3.0))
    );
    r.set_property(&mut w, e, "Size", Value::Vec3(Vec3::splat(4.0)))
        .unwrap();
    assert_eq!(
        w.get("block", "Transform", "scale").unwrap(),
        Value::Vec3(Vec3::splat(2.0))
    );
    assert_eq!(
        r.get_property(&w, e, "Size").unwrap(),
        Value::Vec3(Vec3::splat(4.0))
    );

    // Builtin storage through the accessor; it has no setter.
    assert_eq!(
        r.get_property(&w, e, "ClassName").unwrap(),
        Value::Str("TestBlock".into())
    );
    let err = r
        .set_property(&mut w, e, "ClassName", Value::Str("Part".into()))
        .unwrap_err();
    assert_eq!(code(&err), "RE9005");

    // Read-only own property.
    let err = r
        .set_property(&mut w, e, "Locked", Value::Bool(true))
        .unwrap_err();
    assert_eq!(code(&err), "RE9005");
    assert_eq!(err.to_string(), "TestBlock.Locked is read-only");

    // A value of the wrong type is the reflection error.
    let err = r
        .set_property(&mut w, e, "Count", Value::Str("x".into()))
        .unwrap_err();
    assert_eq!(code(&err), "RE1003");
}

#[test]
fn unimplemented_members_and_non_properties_raise_re9001() {
    let r = test_registry();
    let mut w = World::new();
    let e = block(&mut w, &r);
    let err = r.get_property(&w, e, "Secret").unwrap_err();
    assert_eq!(code(&err), "RE9001");
    assert!(
        err.to_string()
            .contains("Instance.Secret is not implemented yet (missing)"),
        "{err}"
    );
    let err = r.get_property(&w, e, "Destroy").unwrap_err();
    assert_eq!(code(&err), "RE9001");
    assert!(
        err.to_string().contains("is a method, not a property"),
        "{err}"
    );
    let err = r.get_property(&w, e, "Nonsense").unwrap_err();
    assert_eq!(code(&err), "RE9001");

    // An entity that is not an instance.
    let plain = w.ecs.spawn_empty().id();
    let err = r.get_property(&w, plain, "Count").unwrap_err();
    assert_eq!(code(&err), "RE9004");
}

#[test]
fn instance_components_follow_the_class_chain() {
    let r = test_registry();
    assert_eq!(
        r.instance_components("TestBlock").unwrap(),
        ["Compat_Instance", "Transform", "Compat_TestBlock"]
    );
    assert_eq!(
        r.instance_components("LocalScript").unwrap(),
        ["Compat_Instance", "Compat_BaseScript"]
    );
    assert_eq!(code(&r.instance_components("Nope").unwrap_err()), "RE9001");
}

#[test]
fn registering_components_is_idempotent_and_survives_a_scene_round_trip() {
    let r = test_registry();
    let mut w = World::new();
    let e = block(&mut w, &r);
    r.register_components(&mut w).unwrap();
    r.set_property(&mut w, e, "Count", Value::Int(11)).unwrap();
    let doc = w.save(re_scene::SaveOptions::default());

    // A fresh world registers the classes, then loads the file, whose
    // declarations of the own components match.
    let mut w2 = World::new();
    r.register_components(&mut w2).unwrap();
    w2.load(&doc).unwrap();
    r.register_components(&mut w2).unwrap();
    let e2 = w2.entity("block").unwrap();
    assert_eq!(r.get_property(&w2, e2, "Count").unwrap(), Value::Int(11));
    assert_eq!(r.class_of(&w2, e2).unwrap().name, "TestBlock");
    assert_eq!(
        re_scene::scene::to_json(&w2.save(re_scene::SaveOptions::default())),
        re_scene::scene::to_json(&doc)
    );

    // An own component declared with another shape is refused.
    let mut w3 = World::new();
    w3.registry
        .register_script(
            &mut w3.ecs,
            re_reflect::ComponentSchema {
                name: "Compat_TestBlock".into(),
                doc: String::new(),
                fields: vec![re_reflect::FieldSchema {
                    name: "Count".into(),
                    ty: re_reflect::FieldType::Bool,
                    doc: String::new(),
                    default: Value::Bool(false),
                }],
                origin: re_reflect::ComponentOrigin::Script,
            },
        )
        .unwrap();
    let err = r.register_components(&mut w3).unwrap_err();
    assert_eq!(code(&err), "RE9004");
}

#[test]
fn bad_data_is_refused_with_re9003() {
    fn classes(json: &str) -> ClassFile {
        ClassFile::parse("t.json", json).unwrap()
    }
    let base = || ClassRegistryBuilder::builtin().unwrap();
    let cases: Vec<(&str, ClassRegistryBuilder, &str)> = vec![
        (
            "duplicate class",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"Folder","superclass":"Instance","status":"missing"}]}"#),
            ),
            "already declared",
        ),
        (
            "unknown superclass",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"A","superclass":"Nope","status":"missing"}]}"#),
            ),
            "unknown superclass Nope",
        ),
        (
            "no superclass",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"A","status":"missing"}]}"#),
            ),
            "has no superclass",
        ),
        (
            "cycle",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"A","superclass":"B","status":"missing"},{"name":"B","superclass":"A","status":"missing"}]}"#),
            ),
            "cycle",
        ),
        (
            "partial without note",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"A","superclass":"Instance","status":"partial"}]}"#),
            ),
            "has no note",
        ),
        (
            "extend unknown",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"Nope","extend":true}]}"#),
            ),
            "names no declared class",
        ),
        (
            "extend sets class fields",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"Folder","extend":true,"creatable":true}]}"#),
            ),
            "may only add",
        ),
        (
            "member declared twice",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"Folder","extend":true,"methods":[{"name":"X","status":"missing"}],"events":[{"name":"X","status":"missing"}]}]}"#),
            ),
            "Folder.X is declared twice",
        ),
        (
            "kind clash with an inherited member",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"Instance","extend":true,"methods":[{"name":"X","status":"missing"}]},{"name":"Folder","extend":true,"events":[{"name":"X","status":"missing"}]}]}"#),
            ),
            "Folder.X is a event but Instance.X is a method",
        ),
        (
            "unknown enum for own storage",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"Folder","extend":true,"properties":[{"name":"M","type":"Enum.Nope","storage":{"kind":"own"},"status":"missing"}]}]}"#),
            ),
            "unknown enum type Enum.Nope",
        ),
        (
            "unresolved own type",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"Folder","extend":true,"properties":[{"name":"M","type":"NotARecord","storage":{"kind":"own"},"status":"missing"}]}]}"#),
            ),
            "has no field type",
        ),
        (
            "default of the wrong type",
            base().classes(
                "t.json",
                classes(r#"{"format":"real-engine/compat-classes","version":1,"classes":[{"name":"Folder","extend":true,"properties":[{"name":"M","type":"bool","storage":{"kind":"own"},"default":"yes","status":"missing"}]}]}"#),
            ),
            "Folder.M: default",
        ),
        (
            "missing converter",
            common::instance_accessors(
                ClassRegistryBuilder::builtin()
                    .unwrap()
                    .classes("t.json", ClassFile::parse("t.json", TEST_CLASSES).unwrap()),
            ),
            "no converter named double",
        ),
        (
            "missing accessor",
            ClassRegistryBuilder::builtin()
                .unwrap()
                .classes("t.json", ClassFile::parse("t.json", TEST_CLASSES).unwrap())
                .converter("double", common::double()),
            "is implemented but no accessor",
        ),
        (
            "accessor for a non-builtin",
            test_builder().accessor("Folder", "Archivable", common::class_name()),
            "not a builtin property",
        ),
    ];
    for (what, builder, want) in cases {
        let err = builder.build().expect_err(what);
        assert_eq!(code(&err), "RE9003", "{what}: {err}");
        assert!(err.to_string().contains(want), "{what}: {err}");
    }
}

#[test]
fn bad_files_are_refused_with_re9003() {
    let cases = [
        (
            ClassFile::parse("c.json", r#"{"format":"x","version":1,"classes":[]}"#).err(),
            "expected \"real-engine/compat-classes\"",
        ),
        (
            ClassFile::parse("c.json", r#"{"format":"real-engine/compat-classes","version":2,"classes":[]}"#).err(),
            "version 2 is not supported",
        ),
        (
            ClassFile::parse("c.json", r#"{"format":"real-engine/compat-classes","version":1,"classes":[],"extra":1}"#).err(),
            "unknown field",
        ),
        (
            EnumFile::parse("e.json", r#"{"format":"real-engine/compat-enums","version":1,"enums":[{"name":"E","status":"missing","items":[{"name":"A","value":0},{"name":"B","value":0}]}]}"#).err(),
            "repeats the value 0",
        ),
        (
            EnumFile::parse("e.json", r#"{"format":"real-engine/compat-enums","version":1,"enums":[{"name":"E","status":"missing","items":[]}]}"#).err(),
            "has no items",
        ),
        (
            LibraryFile::parse("l.json", r#"{"format":"real-engine/compat-libraries","version":1,"libraries":[{"name":"game","class":"DataModel","status":"missing","members":[{"name":"x","status":"missing"}]}]}"#).err(),
            "members come from the class",
        ),
        (
            LibraryFile::parse("l.json", r#"{"format":"real-engine/compat-libraries","version":1,"libraries":[{"name":"task","status":"missing","members":[{"name":"wait","status":"partial"}]}]}"#).err(),
            "task.wait is partial but has no note",
        ),
    ];
    for (err, want) in cases {
        let err = err.expect(want);
        assert_eq!(code(&err), "RE9003", "{err}");
        assert!(err.to_string().contains(want), "{want}: {err}");
    }
}

#[test]
fn saved_scenes_survive_new_own_properties_and_omit_host_schemas() {
    let r = test_registry();
    let mut w = World::new();
    block(&mut w, &r);
    let doc = w.save(re_scene::SaveOptions::default());
    assert!(
        doc.script_components.is_empty(),
        "host components are not declared in the file"
    );

    // The data grows: a new own property on an existing class.
    let more = ClassFile::parse(
        "more.json",
        r#"{"format":"real-engine/compat-classes","version":1,"classes":[
            {"name":"TestBlock","extend":true,"properties":[
              {"name":"Label","type":"string","storage":{"kind":"own"},"default":"hi","status":"implemented"}]}]}"#,
    )
    .unwrap();
    let r2 = test_builder().classes("more.json", more).build().unwrap();
    let mut w2 = World::new();
    r2.register_components(&mut w2).unwrap();
    w2.load(&doc).unwrap();
    let e = w2.entity("block").unwrap();
    assert_eq!(
        r2.get_property(&w2, e, "Label").unwrap(),
        Value::Str("hi".into())
    );
    assert_eq!(r2.get_property(&w2, e, "Count").unwrap(), Value::Int(3));
}

#[test]
fn library_records_give_own_properties_a_stored_form() {
    let libs = LibraryFile::parse(
        "dims.json",
        r#"{"format":"real-engine/compat-libraries","version":1,"libraries":[
            {"name":"TestDim","status":"missing",
             "members":[{"name":"new","params":[{"name":"scale","type":"number"}],"returns":["TestDim"],"status":"missing"},
                        {"name":"zero","kind":"constant","type":"TestDim","status":"missing"}],
             "value_members":[{"name":"Scale","kind":"property","type":"number","status":"missing"},
                              {"name":"Lerp","kind":"method","status":"missing"}],
             "record":[{"name":"Scale","type":"number"},{"name":"Offset","type":"int"}]}]}"#,
    )
    .unwrap();
    let classes = ClassFile::parse(
        "c.json",
        r#"{"format":"real-engine/compat-classes","version":1,"classes":[
            {"name":"Folder","extend":true,"properties":[
              {"name":"Pad","type":"TestDim","storage":{"kind":"own"},"default":{"Scale":0.5,"Offset":2},"status":"implemented"}]}]}"#,
    )
    .unwrap();
    let r = test_builder()
        .libraries("dims.json", libs)
        .classes("c.json", classes)
        .build()
        .unwrap();
    let mut w = World::new();
    r.register_components(&mut w).unwrap();
    let game = scene::spawn_instance(&mut w, &r, "DataModel", Some("game"), "game", None).unwrap();
    let f = scene::spawn_instance(&mut w, &r, "Folder", Some("f"), "F", Some(game)).unwrap();
    let Value::Record(id, fields) = r.get_property(&w, f, "Pad").unwrap() else {
        panic!("a record");
    };
    assert_eq!(id.name(), "TestDim");
    assert_eq!(fields, [Value::Float(0.5), Value::Int(2)]);

    let bad = |json: &str| LibraryFile::parse("l.json", json).unwrap_err().to_string();
    assert!(bad(r#"{"format":"real-engine/compat-libraries","version":1,"libraries":[{"name":"T","status":"missing","members":[{"name":"x","kind":"method","status":"missing"}]}]}"#).contains("member T.x cannot be a method"));
    assert!(bad(r#"{"format":"real-engine/compat-libraries","version":1,"libraries":[{"name":"T","status":"missing","value_members":[{"name":"x","status":"missing"}]}]}"#).contains("value member T.x cannot be a function"));
    assert!(bad(r#"{"format":"real-engine/compat-libraries","version":1,"libraries":[{"name":"g","class":"DataModel","status":"missing","record":[{"name":"a","type":"number"}]}]}"#).contains("names a class"));
}

#[test]
fn a_data_only_registry_refuses_unconverted_access() {
    let r = ClassRegistryBuilder::builtin()
        .unwrap()
        .classes("t.json", ClassFile::parse("t.json", TEST_CLASSES).unwrap())
        .build_data_only()
        .unwrap();
    let mut w = World::new();
    let e = block(&mut w, &r);
    let err = r.get_property(&w, e, "Size").unwrap_err();
    assert_eq!(code(&err), "RE9001");
    let err = r
        .set_property(&mut w, e, "Size", Value::Vec3(Vec3::ONE))
        .unwrap_err();
    assert_eq!(code(&err), "RE9001");
}

#[test]
fn aliases_must_name_registered_fields() {
    let bad = ClassFile::parse(
        "b.json",
        r#"{"format":"real-engine/compat-classes","version":1,"classes":[
            {"name":"Folder","extend":true,"properties":[
              {"name":"Where","type":"Vector3","storage":{"kind":"alias","component":"Transform","field":"positon"},"status":"implemented"}]}]}"#,
    )
    .unwrap();
    let r = test_builder().classes("b.json", bad).build().unwrap();
    let mut w = World::new();
    let err = r.register_components(&mut w).unwrap_err();
    assert_eq!(code(&err), "RE9003");
    assert!(err.to_string().contains("Transform.positon"), "{err}");
    // Nothing was registered.
    assert!(w.registry.try_key("ClassMarker").is_none());
}

#[test]
fn a_scene_with_host_components_fails_loudly_without_them() {
    let r = test_registry();
    let mut w = World::new();
    block(&mut w, &r);
    let doc = w.save(re_scene::SaveOptions::default());
    // A world that never registered the class layer cannot guess the schemas.
    let err = World::new().load(&doc).unwrap_err();
    assert!(
        err.to_string().contains("ClassMarker") || err.to_string().contains("Compat_"),
        "{err}"
    );
}
