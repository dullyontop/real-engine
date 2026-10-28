//! Place and model files: both readers on files the test writer makes (every
//! value type, both compressions), malformed input (never a panic) and the
//! import end to end.

mod common;
#[path = "common/place_writer.rs"]
mod place_writer;

use std::collections::BTreeMap;
use std::path::Path;

use common::registry;
use place_writer::{Compression, Inst, attributes, binary, inst, s, xml};
use re_import::place::dom::{CFrame, Dom, Font, PlaceValue as V, rotation_of_id};
use re_import::place::read_dom;
use re_import::place::xml::MAX_DEPTH;
use re_import::{ImportOptions, InputFormat, codes, import};
use re_reflect::Value;
use re_scene::World;

fn rotated(id: u8, position: [f32; 3]) -> CFrame {
    CFrame {
        position,
        rotation: rotation_of_id(id).unwrap(),
    }
}

/// One instance of `Probe` per value type, so every type id is written.
fn every_type() -> Vec<Inst> {
    let free = CFrame {
        position: [1.5, -2.0, 3.25],
        rotation: [0.6, 0.0, 0.8, 0.0, 1.0, 0.0, -0.8, 0.0, 0.6],
    };
    let values = vec![
        s("text with <markup> & \"quotes\""),
        V::Bool(true),
        V::Int32(-123_456),
        V::Float32(-0.375),
        V::Float64(1e300),
        V::UDim(0.5, -12),
        V::UDim2(0.25, 10, -0.75, -20),
        V::Ray {
            origin: [1.0, 2.0, 3.0],
            direction: [0.0, -1.0, 0.0],
        },
        V::Faces(0b10_1010),
        V::Axes(0b101),
        V::BrickColor(194),
        V::Color3([0.25, 0.5, 1.0]),
        V::Vector2([-3.5, 8.0]),
        V::Vector3([1e-3, -7.0, 42.0]),
        V::CFrame(rotated(0x0D, [4.0, 5.0, 6.0])),
        V::CFrame(free),
        V::Enum(256),
        V::Vector3int16([-32768, 0, 32767]),
        V::NumberSequence(vec![[0.0, 1.0, 0.0], [1.0, 0.0, 0.25]]),
        V::ColorSequence(vec![(0.0, [1.0, 0.0, 0.0]), (1.0, [0.0, 0.0, 1.0])]),
        V::NumberRange(2.0, 5.0),
        V::Rect([0.0, 1.0, 2.0, 3.0]),
        V::PhysicalProperties(None),
        V::PhysicalProperties(Some([0.7, 0.3, 0.5, 1.0, 1.0, 0.2])),
        V::Color3uint8([255, 128, 0]),
        V::Int64(-9_000_000_000),
        V::SharedString(b"shared bytes \x00\x01".to_vec()),
        V::OptionalCFrame(None),
        V::OptionalCFrame(Some(rotated(0x02, [0.0, 10.0, 0.0]))),
        V::UniqueId(std::array::from_fn(|i| i as u8 * 17)),
        V::Font(Font {
            family: "rbxasset://fonts/families/GothamSSm.json".into(),
            weight: 700,
            style: "Italic".into(),
            cached_face: String::new(),
        }),
        V::SecurityCapabilities(12),
        V::Content(Some("rbxassetid://123".into())),
        V::Content(None),
    ];
    let mut out = vec![inst("Folder", None, vec![("Name", s("Root"))])];
    for (k, v) in values.into_iter().enumerate() {
        // One class per value, so each column holds one type.
        out.push(inst(
            &format!("Probe{k:02}"),
            Some(0),
            vec![("Name", s(&format!("P{k:02}"))), ("Value", v)],
        ));
    }
    // A reference to another instance and a null one.
    out.push(inst(
        "Link",
        Some(0),
        vec![("Name", s("L1")), ("Target", V::Ref(Some(1)))],
    ));
    out.push(inst(
        "Link",
        Some(0),
        vec![("Name", s("L2")), ("Target", V::Ref(None))],
    ));
    out
}

/// Instances by name: class, parent name, properties (a reference as the
/// target's name).
type Shape = BTreeMap<String, (String, Option<String>, Vec<(String, String)>)>;

fn name_of(props: &[(String, V)], fallback: &str) -> String {
    props
        .iter()
        .find(|(k, _)| k == "Name")
        .map(|(_, v)| match v {
            V::String(b) => String::from_utf8_lossy(b).into_owned(),
            other => format!("{other:?}"),
        })
        .unwrap_or_else(|| fallback.to_string())
}

fn shape(classes: &[String], parents: &[Option<usize>], props: &[Vec<(String, V)>]) -> Shape {
    let names: Vec<String> = props.iter().map(|p| name_of(p, "?")).collect();
    (0..classes.len())
        .map(|i| {
            let ps = props[i]
                .iter()
                .map(|(k, v)| {
                    let v = match v {
                        V::Ref(r) => format!("Ref({:?})", r.map(|t| names[t].clone())),
                        other => format!("{other:?}"),
                    };
                    (k.clone(), v)
                })
                .collect();
            (
                names[i].clone(),
                (classes[i].clone(), parents[i].map(|p| names[p].clone()), ps),
            )
        })
        .collect()
}

fn shape_of_input(insts: &[Inst]) -> Shape {
    shape(
        &insts.iter().map(|i| i.class.clone()).collect::<Vec<_>>(),
        &insts.iter().map(|i| i.parent).collect::<Vec<_>>(),
        &insts.iter().map(|i| i.props.clone()).collect::<Vec<_>>(),
    )
}

fn shape_of_dom(dom: &Dom) -> Shape {
    shape(
        &dom.instances
            .iter()
            .map(|i| i.class.clone())
            .collect::<Vec<_>>(),
        &dom.instances.iter().map(|i| i.parent).collect::<Vec<_>>(),
        &dom.instances
            .iter()
            .map(|i| i.properties.clone())
            .collect::<Vec<_>>(),
    )
}

#[test]
fn the_binary_reader_reads_every_value_type_in_every_compression() {
    let insts = every_type();
    let want = shape_of_input(&insts);
    for c in [Compression::None, Compression::Lz4, Compression::Zstd] {
        let bytes = binary(&insts, c);
        let dom = read_dom(&bytes, "every.rbxm").unwrap_or_else(|d| panic!("{c:?}: {d:?}"));
        assert!(dom.warnings.is_empty(), "{c:?}: {:?}", dom.warnings);
        assert_eq!(shape_of_dom(&dom), want, "{c:?}");
        assert_eq!(dom.meta, [("ExplicitAutoJoints".into(), "true".into())]);
        assert_eq!(dom.roots.len(), 1);
    }
}

#[test]
fn the_xml_reader_reads_every_value_type() {
    let insts = every_type();
    let text = xml(&insts);
    let dom = read_dom(text.as_bytes(), "every.rbxmx").unwrap_or_else(|d| panic!("{d:?}"));
    assert!(dom.warnings.is_empty(), "{:?}", dom.warnings);
    // XML keeps no separate bytecode type; the rest reads back the same.
    assert_eq!(shape_of_dom(&dom), shape_of_input(&insts));
}

#[test]
fn every_rotation_id_round_trips_through_a_binary_cframe() {
    let insts: Vec<Inst> = (2..=0x23u8)
        .filter(|&id| rotation_of_id(id).is_some())
        .map(|id| {
            inst(
                "Part",
                None,
                vec![
                    ("Name", s(&format!("R{id:02x}"))),
                    ("CFrame", V::CFrame(rotated(id, [id as f32, 0.0, 0.0]))),
                ],
            )
        })
        .collect();
    assert_eq!(insts.len(), 24);
    let dom = read_dom(&binary(&insts, Compression::None), "rot.rbxm").unwrap();
    assert_eq!(shape_of_dom(&dom), shape_of_input(&insts));
}

#[test]
fn unknown_value_types_and_chunks_are_skipped_with_a_warning() {
    let insts = vec![inst(
        "Part",
        None,
        vec![("Name", s("A")), ("Anchored", V::Bool(true))],
    )];
    let mut bytes = binary(&insts, Compression::None);
    // Retag the Anchored column with an unknown type id.
    let at = bytes.windows(8).position(|w| w == b"Anchored").unwrap();
    assert_eq!(bytes[at + 8], 0x02);
    bytes[at + 8] = 0x7f;
    let dom = read_dom(&bytes, "t.rbxm").unwrap();
    assert_eq!(dom.instances[0].get("Anchored"), None);
    assert_eq!(dom.warnings.len(), 1);
    assert_eq!(dom.warnings[0].code, codes::PLACE_VALUE_TYPE);
    assert!(dom.warnings[0].message.contains("Part.Anchored"));

    let text = xml(&insts)
        .replace("<bool name=\"Anchored\">", "<Future name=\"Anchored\">")
        .replace("true</bool>", "true</Future>");
    let dom = read_dom(text.as_bytes(), "t.rbxmx").unwrap();
    assert_eq!(dom.instances[0].get("Anchored"), None);
    assert_eq!(dom.warnings[0].code, codes::PLACE_VALUE_TYPE);
}

/// Every prefix and many single-byte corruptions of a file read to `Ok` or
/// `RE9809`, never a panic.
fn hammer(bytes: &[u8], label: &str) {
    let step = (bytes.len() / 2000).max(1);
    for n in (0..bytes.len()).step_by(step) {
        if let Err(d) = read_dom(&bytes[..n], label) {
            assert_eq!(d.code, codes::PLACE_MALFORMED, "prefix {n}: {d:?}");
        }
    }
    let step = (bytes.len() / 1500).max(1);
    for i in (0..bytes.len()).step_by(step) {
        for flip in [0xffu8, 0x01, 0x80] {
            let mut b = bytes.to_vec();
            b[i] ^= flip;
            if let Err(d) = read_dom(&b, label) {
                assert!(
                    d.code == codes::PLACE_MALFORMED,
                    "byte {i} ^ {flip:#x}: {d:?}"
                );
            }
        }
    }
}

#[test]
fn truncated_and_corrupted_binary_files_never_panic() {
    let insts = every_type();
    hammer(&binary(&insts, Compression::None), "bad.rbxm");
    hammer(&binary(&insts, Compression::Lz4), "bad.rbxm");
    hammer(&binary(&insts, Compression::Zstd), "bad.rbxm");
}

#[test]
fn truncated_and_corrupted_xml_files_never_panic() {
    hammer(xml(&every_type()).as_bytes(), "bad.rbxmx");
}

#[test]
fn lying_counts_are_refused_before_allocating() {
    let insts = vec![inst("Part", None, vec![("Name", s("A"))])];
    let good = binary(&insts, Compression::None);
    // The header's instance count.
    let mut b = good.clone();
    b[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    let d = read_dom(&b, "big.rbxm").unwrap_err();
    assert_eq!(d.code, codes::PLACE_MALFORMED);
    assert!(d.message.contains("instances"), "{}", d.message);
    // A chunk that says it expands to 4 GB.
    let mut b = good.clone();
    let meta = b.windows(4).position(|w| w == b"META").unwrap();
    b[meta + 8..meta + 12].copy_from_slice(&u32::MAX.to_le_bytes());
    let d = read_dom(&b, "big.rbxm").unwrap_err();
    assert_eq!(d.code, codes::PLACE_MALFORMED);
    // A parent cycle.
    let insts = vec![
        inst("Folder", Some(1), vec![("Name", s("A"))]),
        inst("Folder", Some(0), vec![("Name", s("B"))]),
    ];
    let d = read_dom(&binary(&insts, Compression::None), "loop.rbxm").unwrap_err();
    assert!(d.message.contains("its own ancestor"), "{}", d.message);
    let d = read_dom(xml(&insts).as_bytes(), "loop.rbxmx");
    // The XML form nests by construction; a cycle has no roots to write.
    assert!(d.is_ok());
}

#[test]
fn deep_xml_nesting_is_refused() {
    let mut text = String::from("<roblox version=\"4\">");
    for _ in 0..3000 {
        text.push_str("<Item class=\"Folder\">");
    }
    let d = read_dom(text.as_bytes(), "deep.rbxmx").unwrap_err();
    assert_eq!(d.code, codes::PLACE_MALFORMED);
    assert!(d.message.contains("deeper"), "{}", d.message);
}

/// `depth` folders, each the parent of the next.
fn chain(depth: usize) -> Vec<Inst> {
    (0..depth)
        .map(|i| {
            inst(
                "Folder",
                i.checked_sub(1),
                vec![("Name", s(&format!("F{i}")))],
            )
        })
        .collect()
}

#[test]
fn deep_binary_nesting_is_refused() {
    // A small file whose instances form one long parent chain overflowed the
    // stack in the import's recursive walks: an abort, with no crash report.
    for compression in [Compression::None, Compression::Zstd] {
        let bytes = binary(&chain(100_000), compression);
        let d = read_dom(&bytes, "deep.rbxm").unwrap_err();
        assert_eq!(d.code, codes::PLACE_MALFORMED);
        assert!(d.message.contains("deeper"), "{}", d.message);
    }
    let d = read_dom(
        &binary(&chain(MAX_DEPTH + 1), Compression::None),
        "deep.rbxm",
    )
    .unwrap_err();
    assert!(d.message.contains("deeper"), "{}", d.message);
}

// ---- The import end to end -------------------------------------------------

fn load(project: &Path) -> World {
    let r = registry();
    let mut w = World::new();
    r.register_components(&mut w).unwrap();
    let text = std::fs::read_to_string(project.join("scenes/main.scene.json")).unwrap();
    w.load(&re_scene::scene::from_json(&text).unwrap()).unwrap();
    w
}

fn id_of(report: &re_import::ImportReport, path: &str) -> String {
    report
        .created
        .iter()
        .chain(&report.adopted)
        .find(|i| re_import::display_path(&i.path) == path)
        .unwrap_or_else(|| panic!("{path} not imported: {}", report.render_human()))
        .id
        .clone()
}

fn prop(w: &World, id: &str, name: &str) -> Value {
    let e = w.entity(id).unwrap();
    registry().get_property(w, e, name).unwrap()
}

/// A model: a part with most of what a saved part carries, a script under
/// it, and the model's primary part.
fn model() -> Vec<Inst> {
    vec![
        inst(
            "Model",
            None,
            vec![
                ("Name", s("Tower")),
                ("PrimaryPart", V::Ref(Some(1))),
                ("UniqueId", V::UniqueId([7; 16])),
            ],
        ),
        inst(
            "Part",
            Some(0),
            vec![
                ("Name", s("Base")),
                ("size", V::Vector3([4.0, 1.0, 2.0])),
                ("CFrame", V::CFrame(rotated(0x0D, [0.0, 3.0, 0.0]))),
                ("Color3uint8", V::Color3uint8([255, 0, 0])),
                ("Anchored", V::Bool(true)),
                ("Material", V::Enum(288)),
                ("Transparency", V::Float32(0.25)),
                ("Tags", s("Spawn\0Base")),
                ("AttributesSerialize", V::String(attributes())),
                ("FormFactorWeird", V::Int32(3)),
            ],
        ),
        inst(
            "Script",
            Some(1),
            vec![
                ("Name", s("Spin")),
                ("Source", s("print(\"spin\")\n")),
                ("Disabled", V::Bool(false)),
            ],
        ),
    ]
}

#[test]
fn a_model_file_imports_under_workspace_with_its_properties() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Tower.rbxm");
    std::fs::write(&input, binary(&model(), Compression::Lz4)).unwrap();
    let project = dir.path().join("game");
    let r = registry();
    let report = import(&input, &project, &r, &ImportOptions::default()).unwrap();
    assert!(report.ok(), "{}", report.render_human());
    assert_eq!(report.format, InputFormat::Place);

    let w = load(&project);
    let base = id_of(&report, "Workspace.Tower.Base");
    assert_eq!(
        prop(&w, &base, "Size"),
        Value::Vec3(glam::Vec3::new(4.0, 1.0, 2.0))
    );
    assert_eq!(prop(&w, &base, "Anchored"), Value::Bool(true));
    assert_eq!(prop(&w, &base, "Material"), Value::Enum("Neon".into()));
    assert_eq!(prop(&w, &base, "Transparency"), Value::Float(0.25));
    match prop(&w, &base, "Color") {
        Value::Color(c) => assert_eq!((c.r, c.g, c.b), (1.0, 0.0, 0.0)),
        other => panic!("{other:?}"),
    }
    match prop(&w, &base, "Position") {
        Value::Vec3(p) => assert!((p - glam::Vec3::new(0.0, 3.0, 0.0)).length() < 1e-5),
        other => panic!("{other:?}"),
    }
    let e = w.entity(&base).unwrap();
    let tags = &w.ecs.get::<re_scene::Tags>(e).unwrap().tags;
    assert_eq!(tags, &["Spawn", "Base"]);
    let attrs = &w.ecs.get::<re_scene::Attributes>(e).unwrap().values;
    assert_eq!(attrs.get("Label"), Some(&Value::Str("gold".into())));
    assert_eq!(attrs.get("Open"), Some(&Value::Bool(true)));
    assert_eq!(attrs.get("Speed"), Some(&Value::Float(2.5)));
    assert_eq!(
        attrs.get("Spawn"),
        Some(&Value::Vec3(glam::Vec3::new(1.0, 2.0, 3.0)))
    );

    let tower = id_of(&report, "Workspace.Tower");
    assert_eq!(
        prop(&w, &tower, "PrimaryPart"),
        Value::Entity(Some(w.entity(&base).unwrap()))
    );
    let spin = id_of(&report, "Workspace.Tower.Base.Spin");
    let sr = w
        .ecs
        .get::<re_scene::ScriptRef>(w.entity(&spin).unwrap())
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(project.join(&sr.path)).unwrap(),
        "print(\"spin\")\n"
    );

    // What is not imported is reported once per class and property; the
    // file's metadata (UniqueId) is not.
    let unmapped: Vec<&str> = report
        .diagnostics
        .iter()
        .filter(|d| d.code == codes::PLACE_UNMAPPED)
        .map(|d| d.message.as_str())
        .collect();
    assert!(
        unmapped
            .iter()
            .any(|m| m.contains("Part.FormFactorWeird is not imported (1 instance)")),
        "{unmapped:#?}"
    );
    assert!(
        unmapped.iter().any(|m| m.contains("attribute \"Tint\"")),
        "{unmapped:#?}"
    );
    assert!(
        !unmapped.iter().any(|m| m.contains("UniqueId")),
        "{unmapped:#?}"
    );

    // Importing again changes nothing.
    let again = import(&input, &project, &r, &ImportOptions::default()).unwrap();
    assert!(again.created.is_empty(), "{}", again.render_human());
    assert!(again.removed.is_empty(), "{}", again.render_human());
}

#[test]
fn a_model_goes_under_the_parent_path_and_owns_only_its_roots() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Tower.rbxmx");
    std::fs::write(&input, xml(&model())).unwrap();
    let project = dir.path().join("game");
    let r = registry();
    let opts = ImportOptions {
        parent: Some("ReplicatedStorage.Maps".into()),
        ..ImportOptions::default()
    };
    let report = import(&input, &project, &r, &opts).unwrap();
    assert!(report.ok(), "{}", report.render_human());
    id_of(&report, "ReplicatedStorage.Maps");
    id_of(&report, "ReplicatedStorage.Maps.Tower.Base.Spin");

    // A second model beside it is not removed by re-importing the first.
    let other = dir.path().join("Hut.rbxm");
    let mut hut = model();
    hut[0].props[0].1 = s("Hut");
    std::fs::write(&other, binary(&hut, Compression::Zstd)).unwrap();
    import(&other, &project, &r, &opts).unwrap();
    let again = import(&input, &project, &r, &opts).unwrap();
    assert!(again.removed.is_empty(), "{}", again.render_human());

    // A parent is refused for a place file and for other inputs.
    let place = dir.path().join("p.rbxl");
    std::fs::write(&place, binary(&hut, Compression::None)).unwrap();
    let d = import(&place, &project, &r, &opts).unwrap_err();
    assert_eq!(d.code, codes::INPUT_UNREADABLE);
}

#[test]
fn a_place_file_imports_its_services() {
    let insts = vec![
        inst(
            "Workspace",
            None,
            vec![("Name", s("Workspace")), ("Gravity", V::Float32(100.0))],
        ),
        inst(
            "Part",
            Some(0),
            vec![
                ("Name", s("Floor")),
                ("size", V::Vector3([64.0, 1.0, 64.0])),
            ],
        ),
        inst(
            "Lighting",
            None,
            vec![
                ("Name", s("Lighting")),
                ("Ambient", V::Color3([0.5, 0.5, 0.5])),
                ("ClockTime", V::Float32(18.0)),
            ],
        ),
        inst(
            "ServerScriptService",
            None,
            vec![("Name", s("ServerScriptService"))],
        ),
        inst(
            "Script",
            Some(3),
            vec![("Name", s("Main")), ("Source", s("print(1)\n"))],
        ),
        // An empty service the class layer does not know is left alone.
        inst(
            "MaterialService",
            None,
            vec![("Name", s("MaterialService"))],
        ),
    ];
    for (name, bytes) in [
        ("Game.rbxl", binary(&insts, Compression::Zstd)),
        ("Game.rbxlx", xml(&insts).into_bytes()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join(name);
        std::fs::write(&input, bytes).unwrap();
        let project = dir.path().join("game");
        let r = registry();
        let report = import(&input, &project, &r, &ImportOptions::default()).unwrap();
        assert!(report.ok(), "{name}: {}", report.render_human());
        let w = load(&project);
        // Services are found by class, so their ids are their class names.
        let ws = "Workspace";
        assert_eq!(prop(&w, ws, "Gravity"), Value::Float(100.0), "{name}");
        assert_eq!(
            prop(&w, "Lighting", "ClockTime"),
            Value::Float(18.0),
            "{name}"
        );
        let floor = id_of(&report, "Workspace.Floor");
        assert_eq!(
            prop(&w, &floor, "Size"),
            Value::Vec3(glam::Vec3::new(64.0, 1.0, 64.0))
        );
        id_of(&report, "ServerScriptService.Main");
    }
}

#[test]
fn a_malformed_place_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("broken.rbxl");
    let bytes = binary(&model(), Compression::Lz4);
    std::fs::write(&input, &bytes[..bytes.len() / 2]).unwrap();
    let project = dir.path().join("game");
    let d = import(&input, &project, &registry(), &ImportOptions::default()).unwrap_err();
    assert_eq!(d.code, codes::PLACE_MALFORMED);
    assert!(!project.exists());
}

#[test]
fn a_model_nested_as_deep_as_the_limit_imports() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Deep.rbxm");
    std::fs::write(&input, binary(&chain(MAX_DEPTH), Compression::None)).unwrap();
    let project = dir.path().join("game");
    let report = import(&input, &project, &registry(), &ImportOptions::default()).unwrap();
    assert!(report.ok(), "{}", report.render_human());
    let w = load(&project);
    let deepest = id_of(
        &report,
        &format!(
            "Workspace.{}",
            (0..MAX_DEPTH)
                .map(|i| format!("F{i}"))
                .collect::<Vec<_>>()
                .join(".")
        ),
    );
    assert!(w.entity(&deepest).is_ok());
}
