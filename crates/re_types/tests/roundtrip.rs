//! Every data type of the contract round-trips through serde_json, and every one
//! is `Send + Sync + Clone + Serialize + Deserialize`.

use std::collections::BTreeMap;
use std::fmt::Debug;

use glam::{Mat4, Quat, Vec2, Vec3};
use re_types::*;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Serializes, parses back, compares, and checks the second serialization is
/// byte-identical to the first.
fn round_trip<T>(value: &T) -> String
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    assert_eq!(&back, value, "{text}");
    assert_eq!(serde_json::to_string(&back).unwrap(), text);
    text
}

// Compile-time: every data type is a plain, thread-safe value.
const _: () = {
    const fn contract<T: Send + Sync + Clone + Serialize + DeserializeOwned + 'static>() {}
    contract::<EntityId>();
    contract::<RenderSnapshot>();
    contract::<Camera>();
    contract::<Light>();
    contract::<LightKind>();
    contract::<DrawItem>();
    contract::<DrawFlags>();
    contract::<DebugLine>();
    contract::<MeshHandle>();
    contract::<MaterialHandle>();
    contract::<TextureHandle>();
    contract::<TextureRef>();
    contract::<MaterialDesc>();
    contract::<AlphaMode>();
    contract::<TextureData>();
    contract::<TextureFormat>();
    contract::<MeshData>();
    contract::<Vertex>();
    contract::<Aabb>();
    contract::<ClipData>();
    contract::<TargetChannels>();
    contract::<Channel<Vec3>>();
    contract::<Interpolation>();
    contract::<PhysicsCmd>();
    contract::<BodyDesc>();
    contract::<BodyKind>();
    contract::<ShapeDesc>();
    contract::<Layer>();
    contract::<GroupMask>();
    contract::<BodyPose>();
    contract::<ContactEvent>();
    contract::<RayHit>();
    contract::<CharacterDesc>();
    contract::<CharacterState>();
    contract::<ConstraintDesc>();
    contract::<ConstraintKind>();
    contract::<GroundState>();
    contract::<AudioCmd>();
    contract::<ActionFrame>();
    contract::<ButtonState>();
    contract::<GraphDoc>();
    contract::<NodeDoc>();
    contract::<EdgeDoc>();
    contract::<ReplayDoc>();
    contract::<ReplayInput>();
    contract::<ReplayCommand>();
    contract::<ReplayCheck>();
};

// The RenderSnapshot `Send` assertion the plan asks for, on its own.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<RenderSnapshot>();
};

fn mat() -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::new(1.0, 2.0, 0.5),
        Quat::from_xyzw(0.0, 0.6, 0.0, 0.8),
        Vec3::new(3.0, -1.25, 7.5),
    )
}

fn rot() -> Quat {
    Quat::from_xyzw(0.0, 0.6, 0.0, 0.8)
}

#[test]
fn ids() {
    assert_eq!(round_trip(&EntityId(42)), "42");
    assert_eq!(EntityId::from_bits(7).to_bits(), 7);
}

#[test]
fn render() {
    let snapshot = RenderSnapshot {
        camera: Camera {
            world: mat(),
            fov_y_radians: 1.0,
            near: 0.1,
            far: 500.0,
        },
        lights: vec![
            Light::default(),
            Light {
                kind: LightKind::Directional,
                direction: Vec3::new(0.0, -1.0, 0.0),
                intensity: 3.5,
                shadows: true,
                ..Light::default()
            },
        ],
        items: vec![DrawItem {
            entity: EntityId(9),
            mesh: MeshHandle::new(1, 2),
            material: MaterialHandle::new(3, 4),
            world: mat(),
            flags: DrawFlags::CAST_SHADOWS | DrawFlags::RECEIVE_SHADOWS,
        }],
        debug_lines: vec![DebugLine {
            a: Vec3::ZERO,
            b: Vec3::ONE,
            color: [1.0, 0.0, 0.0, 1.0],
        }],
        clear: [0.1, 0.2, 0.3, 1.0],
    };
    round_trip(&snapshot);
    round_trip(&RenderSnapshot::default());
    round_trip(&TextureHandle::new(5, 6));
    round_trip(&MaterialDesc::default());
    assert!(MaterialDesc::default().is_resolved());
    let resolved = MaterialDesc {
        base_color_texture: Some(TextureHandle::new(1, 1).into()),
        normal_texture: Some(TextureHandle::new(2, 1).into()),
        metallic_roughness_texture: Some(TextureRef::Handle(TextureHandle::new(3, 1))),
        alpha_mode: AlphaMode::Mask { cutoff: 0.5 },
        double_sided: true,
        ..MaterialDesc::default()
    };
    round_trip(&resolved);
    assert!(resolved.is_resolved());
    // What re_assets produces: texture references by asset id, resolved by re_app.
    let from_assets = MaterialDesc {
        base_color_texture: Some(TextureRef::Asset("tex-1".into())),
        normal_texture: Some(TextureHandle::new(2, 1).into()),
        ..MaterialDesc::default()
    };
    round_trip(&from_assets);
    assert!(!from_assets.is_resolved());
    assert_eq!(
        round_trip(&TextureRef::Asset("tex-1".into())),
        r#"{"asset":"tex-1"}"#
    );
    assert_eq!(
        round_trip(&TextureRef::Handle(TextureHandle::new(1, 0))),
        r#"{"handle":{"index":1,"generation":0}}"#
    );
    assert_eq!(TextureRef::Asset("a".into()).asset(), Some("a"));
    assert_eq!(TextureRef::Asset("a".into()).handle(), None);
    assert_eq!(
        TextureRef::from(TextureHandle::new(4, 2)).handle(),
        Some(TextureHandle::new(4, 2))
    );
    round_trip(&AlphaMode::Blend);
    let texture = TextureData {
        width: 2,
        height: 1,
        format: TextureFormat::Rgba8Unorm,
        pixels: vec![0, 1, 2, 3, 4, 5, 6, 7],
    };
    assert!(texture.is_well_formed());
    round_trip(&texture);
    assert_eq!(round_trip(&DrawFlags::SELECTED), "4");
}

#[test]
fn draw_flags_and_camera_helpers() {
    let f = DrawFlags::CAST_SHADOWS.with(DrawFlags::SELECTED);
    assert!(f.contains(DrawFlags::SELECTED) && !f.contains(DrawFlags::RECEIVE_SHADOWS));
    assert!(f.without(f).is_empty());
    let cam = Camera {
        world: Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0)),
        ..Camera::default()
    };
    assert_eq!(cam.position(), Vec3::new(1.0, 2.0, 3.0));
    assert!((cam.view() * cam.world).abs_diff_eq(Mat4::IDENTITY, 1e-6));
}

#[test]
fn mesh() {
    let v = |x: f32| Vertex {
        pos: [x, -x, 2.0 * x],
        normal: [0.0, 1.0, 0.0],
        uv: [x, 1.0 - x],
        color: [1.0, 1.0, 1.0, 1.0],
    };
    let mesh = MeshData {
        vertices: vec![v(0.0), v(0.5), v(1.0)],
        indices: vec![0, 1, 2],
    };
    round_trip(&mesh);
    assert!(mesh.is_well_formed());
    assert_eq!(mesh.triangle_count(), 1);
    let aabb = mesh.aabb().unwrap();
    assert_eq!(
        aabb,
        Aabb::new(Vec3::new(0.0, -1.0, 0.0), Vec3::new(1.0, 0.0, 2.0))
    );
    round_trip(&aabb);
    assert!(aabb.contains(aabb.center()));
    assert_eq!(aabb.half_extents(), Vec3::new(0.5, 0.5, 1.0));
    assert_eq!(std::mem::size_of::<Vertex>(), 48);
    assert!(MeshData::default().aabb().is_none());
    assert!(
        !MeshData {
            vertices: vec![],
            indices: vec![0, 1, 2]
        }
        .is_well_formed()
    );
}

#[test]
fn anim() {
    let clip = ClipData {
        name: "walk".into(),
        duration: 1.0,
        targets: vec![
            TargetChannels {
                target: "hips".into(),
                translation: Some(Channel {
                    interpolation: Interpolation::Linear,
                    times: vec![0.0, 1.0],
                    values: vec![Vec3::ZERO, Vec3::Y],
                }),
                rotation: Some(Channel {
                    interpolation: Interpolation::Step,
                    times: vec![0.0],
                    values: vec![rot()],
                }),
                scale: None,
            },
            TargetChannels {
                target: "arm".into(),
                translation: None,
                rotation: None,
                scale: Some(Channel {
                    interpolation: Interpolation::CubicSpline,
                    times: vec![0.0],
                    values: vec![Vec3::ZERO, Vec3::ONE, Vec3::ZERO],
                }),
            },
        ],
    };
    round_trip(&clip);
    assert!(clip.targets[1].scale.as_ref().unwrap().is_well_formed());
}

fn all_shapes() -> Vec<ShapeDesc> {
    vec![
        ShapeDesc::Box {
            half_extents: Vec3::splat(0.5),
        },
        ShapeDesc::Sphere { radius: 0.25 },
        ShapeDesc::Capsule {
            half_height: 0.5,
            radius: 0.4,
        },
        ShapeDesc::ConvexHull {
            points: vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z],
        },
        ShapeDesc::TriangleMesh {
            vertices: vec![Vec3::ZERO, Vec3::X, Vec3::Z],
            triangles: vec![[0, 2, 1]],
        },
        ShapeDesc::Heightfield {
            n: 2,
            samples: vec![0.0, 1.0, 2.0, 3.0],
            spacing: 1.5,
            offset: Vec3::new(-1.0, 0.0, -1.0),
        },
    ]
}

#[test]
fn physics() {
    for shape in all_shapes() {
        assert!(shape.is_well_formed(), "{shape:?}");
        round_trip(&shape);
    }
    assert_eq!(
        round_trip(&ShapeDesc::Sphere { radius: 0.5 }),
        r#"{"shape":"sphere","radius":0.5}"#
    );
    let id = EntityId(3);
    let commands = vec![
        PhysicsCmd::AddBody {
            id,
            desc: BodyDesc {
                kind: BodyKind::Dynamic,
                layer: Layer(1),
                groups: GroupMask::group(4).unwrap(),
                sensor: true,
                ..BodyDesc::default()
            },
        },
        PhysicsCmd::RemoveBody { id },
        PhysicsCmd::SetTransform {
            id,
            pos: Vec3::ONE,
            rot: rot(),
        },
        PhysicsCmd::SetKinematicTarget {
            id,
            pos: Vec3::X,
            rot: Quat::IDENTITY,
        },
        PhysicsCmd::ApplyImpulse {
            id,
            impulse: Vec3::Y,
        },
        PhysicsCmd::SetVelocity {
            id,
            linear: Vec3::X,
            angular: Vec3::Z,
        },
        PhysicsCmd::SetShape {
            id,
            shape: ShapeDesc::Sphere { radius: 1.0 },
        },
        PhysicsCmd::AddCharacter {
            id,
            desc: CharacterDesc::default(),
            pos: Vec3::new(0.0, 10.0, 0.0),
            rot: Quat::IDENTITY,
        },
        PhysicsCmd::RemoveCharacter { id },
        PhysicsCmd::MoveCharacter {
            id,
            desired_velocity: Vec3::X,
            jump: true,
        },
        PhysicsCmd::AddConstraint {
            id,
            desc: ConstraintDesc {
                a: EntityId(1),
                b: EntityId(2),
                kind: ConstraintKind::Hinge {
                    pivot: Vec3::ONE,
                    axis: Vec3::Y,
                    limits: Some([-1.0, 1.0]),
                },
            },
        },
        PhysicsCmd::RemoveConstraint { id },
        PhysicsCmd::RemoveVehicle { id },
        PhysicsCmd::AddVehicle {
            id,
            desc: VehicleDesc {
                kind: VehicleKind::Motorcycle,
                tester: WheelTester::CastSphere { radius: 0.2 },
                ..VehicleDesc::car(Vec3::new(0.9, -0.1, 1.4), 0.35)
            },
        },
        PhysicsCmd::SetVehicleInput {
            id,
            input: VehicleInput {
                throttle: 0.5,
                steer: -0.25,
                brake: 0.0,
                handbrake: 1.0,
            },
        },
    ];
    let ranks: std::collections::BTreeSet<u8> =
        commands.iter().map(PhysicsCmd::kind_rank).collect();
    assert_eq!(
        ranks.len(),
        commands.len(),
        "every command kind has its own rank"
    );
    for cmd in &commands {
        round_trip(cmd);
        assert_eq!(cmd.id(), id);
    }
    assert_eq!(
        round_trip(&PhysicsCmd::RemoveBody { id }),
        r#"{"cmd":"remove_body","id":3}"#
    );
    round_trip(&BodyPose {
        id,
        pos: Vec3::ONE,
        rot: rot(),
    });
    round_trip(&ContactEvent::Begin {
        a: EntityId(1),
        b: EntityId(2),
        point: Vec3::ONE,
        normal: Vec3::Y,
        sensor: false,
    });
    round_trip(&ContactEvent::End {
        a: EntityId(1),
        b: EntityId(2),
        sensor: true,
    });
    round_trip(&RayHit {
        entity: id,
        point: Vec3::ONE,
        normal: Vec3::Y,
        distance: 4.5,
    });
    round_trip(&CharacterDesc::default());
    round_trip(&CharacterState::default());
    round_trip(&CharacterState {
        pos: Vec3::ONE,
        rot: rot(),
        velocity: Vec3::X,
        ground: GroundState::OnGround,
        ground_normal: Vec3::Y,
        ground_entity: Some(EntityId(8)),
    });
    for kind in [BodyKind::Static, BodyKind::Dynamic, BodyKind::Kinematic] {
        round_trip(&kind);
    }
    round_trip(&GroupMask::ALL);
}

#[test]
fn physics_commands_sort_deterministically() {
    let mut commands = vec![
        PhysicsCmd::ApplyImpulse {
            id: EntityId(2),
            impulse: Vec3::X,
        },
        PhysicsCmd::AddBody {
            id: EntityId(5),
            desc: BodyDesc::default(),
        },
        PhysicsCmd::ApplyImpulse {
            id: EntityId(1),
            impulse: Vec3::Y,
        },
        PhysicsCmd::RemoveBody { id: EntityId(9) },
        PhysicsCmd::ApplyImpulse {
            id: EntityId(1),
            impulse: Vec3::Z,
        },
        PhysicsCmd::AddBody {
            id: EntityId(4),
            desc: BodyDesc::default(),
        },
    ];
    PhysicsCmd::sort(&mut commands);
    let keys: Vec<(u8, u64)> = commands.iter().map(|c| (c.kind_rank(), c.id().0)).collect();
    assert_eq!(keys, [(1, 9), (3, 4), (3, 5), (10, 1), (10, 1), (10, 2)]);
    // Equal keys keep their order.
    assert_eq!(
        commands[3],
        PhysicsCmd::ApplyImpulse {
            id: EntityId(1),
            impulse: Vec3::Y
        }
    );
}

#[test]
fn malformed_shapes_are_detected() {
    assert!(!ShapeDesc::Sphere { radius: 0.0 }.is_well_formed());
    assert!(
        !ShapeDesc::Heightfield {
            n: 3,
            samples: vec![0.0; 8],
            spacing: 1.0,
            offset: Vec3::ZERO
        }
        .is_well_formed()
    );
    assert!(
        !ShapeDesc::TriangleMesh {
            vertices: vec![Vec3::ZERO],
            triangles: vec![[0, 1, 2]]
        }
        .is_well_formed()
    );
    assert!(GroupMask::group(32).is_none());
    assert!(
        GroupMask::ALL.contains_group(31)
            && GroupMask::default().intersects(GroupMask::group(0).unwrap())
    );
}

#[test]
fn audio() {
    let id = EntityId(11);
    let commands = vec![
        AudioCmd::PlayOneShot {
            clip: "assets/sfx/pickup.wav".into(),
            position: Some(Vec3::ONE),
            volume: 0.8,
        },
        AudioCmd::PlayOneShot {
            clip: "assets/sfx/ui.wav".into(),
            position: None,
            volume: 1.0,
        },
        AudioCmd::StartSource {
            entity: id,
            clip: "assets/sfx/wind.wav".into(),
            volume: 0.5,
            looping: true,
            spatial: true,
            position: Vec3::X,
        },
        AudioCmd::StopSource { entity: id },
        AudioCmd::SetSourceVolume {
            entity: id,
            volume: 0.25,
        },
        AudioCmd::SetSourcePosition {
            entity: id,
            position: Vec3::Z,
        },
        AudioCmd::SetListener {
            position: Vec3::ZERO,
            rotation: rot(),
        },
        AudioCmd::SetMasterVolume { volume: 0.9 },
        AudioCmd::StopAll,
    ];
    for cmd in &commands {
        round_trip(cmd);
    }
    assert_eq!(round_trip(&AudioCmd::StopAll), r#"{"cmd":"stop_all"}"#);
}

#[test]
fn input() {
    let mut frame = ActionFrame {
        frame: 12,
        ..ActionFrame::default()
    };
    frame.buttons.insert(
        "jump".into(),
        ButtonState {
            down: true,
            pressed: true,
            released: false,
        },
    );
    frame.axes.insert("move".into(), Vec2::new(0.0, 1.0));
    round_trip(&frame);
    assert!(frame.pressed("jump") && frame.is_down("jump") && !frame.released("jump"));
    assert_eq!(frame.axis("move"), Vec2::Y);
    assert_eq!(frame.axis("look"), Vec2::ZERO);
    assert!(!frame.is_down("unknown"));
    round_trip(&ButtonState::default());
    let _: BTreeMap<String, ButtonState> = frame.buttons;
}

const GRAPH: &str = r#"{ "format": "real-engine/graph", "version": 1, "name": "island", "seed": 1234, "nodes": [{ "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 256 } }, { "id": "t", "kind": "terrain.mesh", "version": 1, "params": { "zeta": 1, "alpha": [1, 2] } }], "edges": [{ "from": ["h", "field"], "to": ["t", "a"] }] }"#;

#[test]
fn graph() {
    let doc = GraphDoc::from_json(GRAPH).unwrap();
    assert_eq!(doc.nodes.len(), 2);
    assert_eq!(doc.edges[0].from, ("h".to_string(), "field".to_string()));
    // Parameters keep file order (serde_json's preserve_order).
    let keys: Vec<&String> = doc.node("t").unwrap().params.keys().collect();
    assert_eq!(keys, ["zeta", "alpha"]);
    let text = round_trip(&doc);
    assert!(
        text.contains(r#""edges":[{"from":["h","field"],"to":["t","a"]}]"#),
        "{text}"
    );
    round_trip(&doc.nodes[0]);
    round_trip(&doc.edges[0]);
    let pretty = doc.to_json();
    assert!(pretty.ends_with('\n'));
    assert_eq!(GraphDoc::from_json(&pretty).unwrap(), doc);
    round_trip(&GraphDoc::new("empty", 0));
}

#[test]
fn graph_rules() {
    let unknown_key = GRAPH.replacen(r#""seed""#, r#""colour": 1, "seed""#, 1);
    assert!(matches!(
        GraphDoc::from_json(&unknown_key),
        Err(GraphDocError::Parse(_))
    ));
    let dup = GRAPH.replace(r#""id": "t""#, r#""id": "h""#);
    assert_eq!(
        GraphDoc::from_json(&dup),
        Err(GraphDocError::DuplicateNode("h".into()))
    );
    let missing = GRAPH.replace(r#"["t", "a"]"#, r#"["x", "a"]"#);
    assert_eq!(
        GraphDoc::from_json(&missing),
        Err(GraphDocError::UnknownNode {
            edge: 0,
            node: "x".into()
        })
    );
    let wrong = GRAPH.replace("real-engine/graph", "real-engine/scene");
    assert!(matches!(
        GraphDoc::from_json(&wrong),
        Err(GraphDocError::WrongFormat(_))
    ));
    let newer = GRAPH.replacen(r#""version": 1, "name""#, r#""version": 2, "name""#, 1);
    assert_eq!(
        GraphDoc::from_json(&newer),
        Err(GraphDocError::UnsupportedVersion(2))
    );
}

const REPLAY: &str = r#"{ "$schema": "../../schemas/replay.schema.json", "version": 1, "seed": 1234, "frames": 600,
  "input": [ { "frame": 30, "action": "move", "axis": [0, 1] }, { "frame": 90, "action": "jump", "down": true } ],
  "commands": [ { "frame": 300, "name": "graph.regenerate", "args": { "source": "island", "seed": 99 } } ],
  "checks": [ { "frame": 600, "screenshot": "tests/goldens/{backend}/island_600.png" } ] }"#;

#[test]
fn replay() {
    let doc: ReplayDoc = serde_json::from_str(REPLAY).unwrap();
    assert_eq!(
        doc.schema.as_deref(),
        Some("../../schemas/replay.schema.json")
    );
    assert_eq!(doc.input[0].axis, Some([0.0, 1.0]));
    assert!(doc.input.iter().all(ReplayInput::is_well_formed));
    assert_eq!(doc.commands[0].args["seed"], 99);
    let text = round_trip(&doc);
    assert!(text.starts_with(r#"{"$schema":"#));
    round_trip(&doc.input[1]);
    round_trip(&doc.commands[0]);
    round_trip(&doc.checks[0]);
    let minimal: ReplayDoc =
        serde_json::from_str(r#"{ "version": 1, "seed": 0, "frames": 10 }"#).unwrap();
    assert_eq!(minimal, ReplayDoc::new(0, 10));
    round_trip(&minimal);
    let no_args: ReplayCommand = serde_json::from_str(r#"{ "frame": 1, "name": "undo" }"#).unwrap();
    assert_eq!(no_args.args, serde_json::json!({}));
    assert!(
        serde_json::from_str::<ReplayDoc>(
            r#"{ "version": 1, "seed": 0, "frames": 10, "fps": 60 }"#
        )
        .is_err()
    );
}

#[test]
fn host_error_serializes() {
    let e = HostError::new("RE0501", "entity #7 has no rigid body");
    assert_eq!(e.to_string(), "[RE0501] entity #7 has no rigid body");
    assert_eq!(
        serde_json::to_string(&e).unwrap(),
        r#"{"code":"RE0501","message":"entity #7 has no rigid body"}"#
    );
}

#[test]
fn constraints() {
    let (a, b) = (EntityId(1), EntityId(2));
    let fixed = ConstraintDesc {
        a,
        b,
        kind: ConstraintKind::Fixed,
    };
    assert!(fixed.is_well_formed());
    assert_eq!(
        round_trip(&fixed),
        r#"{"a":1,"b":2,"kind":{"type":"fixed"}}"#
    );
    let distance = ConstraintDesc {
        a,
        b,
        kind: ConstraintKind::Distance {
            point_a: Vec3::ZERO,
            point_b: Vec3::X,
            min: 0.5,
            max: 2.0,
        },
    };
    assert!(distance.is_well_formed());
    round_trip(&distance);
    let hinge = ConstraintDesc {
        a,
        b,
        kind: ConstraintKind::Hinge {
            pivot: Vec3::ZERO,
            axis: Vec3::Y,
            limits: None,
        },
    };
    assert!(hinge.is_well_formed());
    round_trip(&hinge);
    // Malformed: the same body twice, a zero axis, unordered limits or distances.
    assert!(!ConstraintDesc { b: a, ..fixed }.is_well_formed());
    let bad_kinds = [
        ConstraintKind::Hinge {
            pivot: Vec3::ZERO,
            axis: Vec3::ZERO,
            limits: None,
        },
        ConstraintKind::Hinge {
            pivot: Vec3::ZERO,
            axis: Vec3::Y,
            limits: Some([1.0, -1.0]),
        },
        ConstraintKind::Distance {
            point_a: Vec3::ZERO,
            point_b: Vec3::X,
            min: 2.0,
            max: 1.0,
        },
        ConstraintKind::Distance {
            point_a: Vec3::ZERO,
            point_b: Vec3::X,
            min: -1.0,
            max: 1.0,
        },
    ];
    for kind in bad_kinds {
        assert!(!ConstraintDesc { a, b, kind }.is_well_formed(), "{kind:?}");
    }
    // Removing a constraint comes before removing its bodies; adding one comes
    // after adding them.
    let mut commands = vec![
        PhysicsCmd::AddConstraint {
            id: EntityId(10),
            desc: fixed,
        },
        PhysicsCmd::RemoveBody { id: a },
        PhysicsCmd::AddBody {
            id: b,
            desc: BodyDesc::default(),
        },
        PhysicsCmd::RemoveConstraint { id: EntityId(11) },
    ];
    PhysicsCmd::sort(&mut commands);
    let ids: Vec<u64> = commands.iter().map(|c| c.id().0).collect();
    assert_eq!(ids, [11, 1, 2, 10]);
}

#[test]
fn vehicle_types_take_defaults_and_check_input() {
    let desc: VehicleDesc =
        serde_json::from_str(r#"{"wheels": [{"pos": [0.9, -0.1, 1.4], "driven": true}]}"#).unwrap();
    assert_eq!(desc.kind, VehicleKind::Wheeled);
    assert_eq!(desc.tester, WheelTester::Ray);
    assert_eq!(desc.wheels[0].radius, WheelDesc::default().radius);
    assert!(desc.wheels[0].driven);
    assert_eq!(desc.transmission, TransmissionDesc::default());
    round_trip(&desc);
    round_trip(&VehicleState {
        speed: 3.0,
        rpm: 2500.0,
        gear: 2,
        input: VehicleInput::default(),
        wheels: vec![WheelState {
            contact: Some(EntityId(4)),
            on_ground: true,
            ..WheelState::default()
        }],
    });
    round_trip(&WheelPose {
        id: EntityId(2),
        wheel: 1,
        pos: Vec3::ONE,
        rot: Quat::IDENTITY,
    });
    assert!(VehicleInput::default().is_valid());
    for bad in [
        VehicleInput {
            throttle: 1.5,
            ..VehicleInput::default()
        },
        VehicleInput {
            steer: f32::NAN,
            ..VehicleInput::default()
        },
        VehicleInput {
            brake: -0.1,
            ..VehicleInput::default()
        },
        VehicleInput {
            handbrake: 2.0,
            ..VehicleInput::default()
        },
    ] {
        assert!(!bad.is_valid(), "{bad:?}");
    }
    let car = VehicleDesc::car(Vec3::new(0.9, -0.1, 1.4), 0.35);
    assert_eq!(car.wheels.len(), 4);
    assert_eq!(car.wheels.iter().filter(|w| w.driven).count(), 2);
}
