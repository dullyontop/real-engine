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
    contract::<render::PostSettings>();
    contract::<render::ToneMapping>();
    contract::<render::SunShadows>();
    contract::<render::WorldQuad>();
    contract::<render::QuadDepth>();
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
    contract::<AudioGraphDesc>();
    contract::<EffectDesc>();
    contract::<EffectParam>();
    contract::<BusLevel>();
    contract::<ActionFrame>();
    contract::<ButtonState>();
    contract::<GraphDoc>();
    contract::<KindDoc>();
    contract::<ParamDoc>();
    contract::<PortDoc>();
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
        post: render::PostSettings {
            exposure: 2.0,
            tone_mapping: render::ToneMapping::AgX,
            bloom: render::Bloom {
                intensity: 0.5,
                ..render::Bloom::default()
            },
            blur_radius: 0.01,
            sun_rays: render::SunRays {
                intensity: 0.25,
                spread: 0.5,
            },
            grading: Some(render::ColorGrading {
                saturation: -0.5,
                ..render::ColorGrading::default()
            }),
        },
        shadows: render::SunShadows {
            cascades: 2,
            ..render::SunShadows::default()
        },
        quads: vec![
            render::WorldQuad {
                entity: EntityId(4),
                world: mat(),
                size: Vec2::new(2.0, 1.0),
                texture: TextureHandle::new(7, 1),
                depth: render::QuadDepth::Tested { bias: 0.01 },
                light_influence: 1.0,
                brightness: 1.0,
            },
            render::WorldQuad {
                entity: EntityId(5),
                world: Mat4::IDENTITY,
                size: Vec2::ONE,
                texture: TextureHandle::new(8, 1),
                depth: render::QuadDepth::OnTop,
                light_influence: 0.0,
                brightness: 0.5,
            },
        ],
        skinned: vec![SkinnedDrawItem {
            entity: EntityId(10),
            mesh: MeshHandle::new(5, 1),
            material: MaterialHandle::new(3, 4),
            world: mat(),
            flags: DrawFlags::CAST_SHADOWS,
            palette: 0,
            joints: 2,
            bounds: Aabb::new(Vec3::NEG_ONE, Vec3::ONE),
        }],
        joint_palette: vec![mat(), Mat4::IDENTITY],
    };
    round_trip(&snapshot);
    // A snapshot written before skinning existed still loads.
    let mut old = serde_json::to_value(RenderSnapshot::default()).unwrap();
    old.as_object_mut().unwrap().remove("skinned");
    old.as_object_mut().unwrap().remove("joint_palette");
    assert_eq!(
        serde_json::from_value::<RenderSnapshot>(old).unwrap(),
        RenderSnapshot::default()
    );
    round_trip(&RenderSnapshot::default());
    // Snapshots written before the post settings existed still load.
    let old: RenderSnapshot = serde_json::from_str(
        r#"{"camera":{"world":[1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1],"fov_y_radians":1.0,"near":0.1,"far":10.0},"lights":[],"items":[],"debug_lines":[],"clear":[0,0,0,1]}"#,
    )
    .unwrap();
    assert_eq!(old.post, render::PostSettings::default());
    assert_eq!(old.post.exposure, 1.0);
    assert!(old.quads.is_empty());
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
        levels: Vec::new(),
    };
    assert!(texture.is_well_formed());
    round_trip(&texture);
    // Texture JSON from before block-compressed formats still reads.
    let old: TextureData = serde_json::from_str(
        r#"{"width":2,"height":1,"format":"Rgba8Unorm","pixels":[0,1,2,3,4,5,6,7]}"#,
    )
    .unwrap();
    assert_eq!(old, texture);
    assert_eq!(round_trip(&DrawFlags::SELECTED), "4");
}

#[test]
fn block_compressed_textures_carry_their_mip_chain() {
    assert_eq!(
        TextureData::mip_sizes(8, 4),
        vec![(8, 4), (4, 2), (2, 1), (1, 1)]
    );
    assert_eq!(TextureData::mip_sizes(1, 1), vec![(1, 1)]);
    // 16 bytes a 4×4 block, a partial block counting whole; 4 bytes a pixel.
    assert_eq!(TextureFormat::Bc7Srgb.level_bytes(8, 4), 32);
    assert_eq!(TextureFormat::Bc5Unorm.level_bytes(2, 1), 16);
    assert_eq!(TextureFormat::Rgba8Srgb.level_bytes(2, 1), 8);
    assert!(TextureFormat::Bc5Unorm.is_compressed() && !TextureFormat::Rgba8Unorm.is_compressed());
    assert!(TextureFormat::Bc7Srgb.is_srgb() && !TextureFormat::Bc7Unorm.is_srgb());

    let bc = TextureData {
        width: 8,
        height: 4,
        format: TextureFormat::Bc7Srgb,
        pixels: vec![7; 32],
        levels: vec![vec![1; 16], vec![2; 16], vec![3; 16]],
    };
    assert!(bc.is_well_formed());
    round_trip(&bc);
    let mut missing = bc.clone();
    missing.levels.pop();
    assert!(!missing.is_well_formed());
    let mut short = bc.clone();
    short.levels[0].pop();
    assert!(!short.is_well_formed());
    // Block-compressed textures need a width and height that are multiples of 4.
    let unaligned = TextureData {
        width: 6,
        levels: vec![vec![0; 16], vec![0; 16]],
        ..bc.clone()
    };
    assert!(!unaligned.is_well_formed());
    // RGBA8 mips are made at upload, so RGBA8 data carries none.
    let rgba = TextureData {
        width: 1,
        height: 1,
        format: TextureFormat::Rgba8Unorm,
        pixels: vec![0; 4],
        levels: vec![vec![0; 4]],
    };
    assert!(!rgba.is_well_formed());
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
            precision: None,
        },
        ShapeDesc::Heightfield {
            n: 2,
            samples: vec![0.0, 1.0, 2.0, 3.0],
            spacing: 1.5,
            offset: Vec3::ZERO,
            precision: Some(HeightfieldPrecision::FINE),
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
        PhysicsCmd::RemoveRagdoll { id },
        PhysicsCmd::AddRagdoll {
            id,
            desc: RagdollDesc {
                skeleton: RagdollSkeleton {
                    joints: vec![SkeletonJoint {
                        name: "hips".into(),
                        parent: None,
                        neutral: JointPose::new(Vec3::Y, Quat::IDENTITY),
                    }],
                },
                parts: vec![RagdollPartDesc {
                    joint: "hips".into(),
                    constraint: RagdollConstraint::Hinge {
                        min_deg: 0.0,
                        max_deg: 140.0,
                    },
                    ..RagdollPartDesc::default()
                }],
                ..RagdollDesc::default()
            },
            pos: Vec3::X,
            rot: rot(),
            pose: Some(vec![JointPose::IDENTITY]),
        },
        PhysicsCmd::SetRagdollMode {
            id,
            mode: RagdollMode::Powered { strength: 0.5 },
        },
        PhysicsCmd::SetRagdollTarget {
            id,
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            pose: vec![JointPose::new(Vec3::ONE, rot())],
        },
        PhysicsCmd::ApplyRagdollImpulse {
            id,
            joint: "hips".into(),
            impulse: Vec3::Z,
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
fn removals_apply_before_creations_of_every_kind() {
    let mut commands = vec![
        PhysicsCmd::AddBody {
            id: EntityId(5),
            desc: BodyDesc::default(),
        },
        PhysicsCmd::RemoveRagdoll { id: EntityId(5) },
        PhysicsCmd::AddBody {
            id: EntityId(6),
            desc: BodyDesc::default(),
        },
        PhysicsCmd::RemoveVehicle { id: EntityId(6) },
        PhysicsCmd::RemoveBody { id: EntityId(7) },
    ];
    PhysicsCmd::sort(&mut commands);
    let kinds: Vec<(bool, u64)> = commands
        .iter()
        .map(|c| (c.is_removal(), c.id().0))
        .collect();
    assert_eq!(
        kinds,
        [(true, 7), (true, 6), (true, 5), (false, 5), (false, 6)]
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
            offset: Vec3::ZERO,
            precision: None,
        }
        .is_well_formed()
    );
    assert!(
        !ShapeDesc::Heightfield {
            n: 2,
            samples: vec![0.0; 4],
            spacing: 1.0,
            offset: Vec3::ZERO,
            precision: Some(HeightfieldPrecision {
                bits_per_sample: 17,
                block_size: 4,
            }),
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
            id: None,
        },
        AudioCmd::PlayOneShot {
            clip: "assets/sfx/ui.wav".into(),
            position: None,
            volume: 1.0,
            id: Some(3),
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
        AudioCmd::SetSourceBus {
            entity: id,
            bus: "footsteps".into(),
        },
        AudioCmd::PlayOneShotOnBus {
            clip: "assets/sfx/click.wav".into(),
            position: None,
            volume: 1.0,
            bus: "ui".into(),
            id: Some(4),
        },
        AudioCmd::SetBusVolume {
            bus: "music".into(),
            volume_db: -12.0,
            seconds: 0.5,
        },
        AudioCmd::SetEffectParam {
            bus: "sfx".into(),
            effect: 1,
            param: "cutoff_hz".into(),
            value: EffectParam::Number(600.0),
            seconds: 0.25,
        },
        AudioCmd::SetEffectParam {
            bus: "sfx".into(),
            effect: 1,
            param: "mode".into(),
            value: EffectParam::Choice("high_pass".into()),
            seconds: 0.0,
        },
        AudioCmd::ApplySnapshot {
            name: "underwater".into(),
            seconds: 1.0,
        },
        AudioCmd::SoundControl {
            id: 3,
            op: SoundOp::Pause { fade: 0.25 },
        },
        AudioCmd::SoundControl {
            id: 4,
            op: SoundOp::Rate {
                rate: 1.5,
                fade: 0.0,
            },
        },
        AudioCmd::SourceControl {
            entity: id,
            op: SoundOp::Start,
        },
        AudioCmd::SourceControl {
            entity: id,
            op: SoundOp::Seek { seconds: 12.5 },
        },
        AudioCmd::SourceControl {
            entity: id,
            op: SoundOp::Volume {
                volume: 0.5,
                fade: 2.0,
            },
        },
        AudioCmd::SetSourceSpatial {
            entity: id,
            spatial: Spatial {
                rolloff: Rolloff::Inverse,
                min_distance: 2.0,
                max_distance: 40.0,
                doppler: 1.0,
            },
        },
    ];
    for cmd in &commands {
        round_trip(cmd);
    }
    // A one-shot without an id is written as it was before ids existed.
    assert_eq!(
        round_trip(&commands[0]),
        r#"{"cmd":"play_one_shot","clip":"assets/sfx/pickup.wav","position":[1.0,1.0,1.0],"volume":0.8}"#
    );
    // The op and its fields sit beside the command's own.
    assert_eq!(
        round_trip(&AudioCmd::SourceControl {
            entity: id,
            op: SoundOp::Stop { fade: 0.5 },
        }),
        r#"{"cmd":"source_control","entity":11,"op":"stop","fade":0.5}"#
    );
    // Left out, a fade is 0 and spatial settings take their defaults.
    let read: AudioCmd =
        serde_json::from_str(r#"{"cmd":"sound_control","id":9,"op":"resume"}"#).unwrap();
    assert_eq!(
        read,
        AudioCmd::SoundControl {
            id: 9,
            op: SoundOp::Resume { fade: 0.0 },
        }
    );
    let read: AudioCmd =
        serde_json::from_str(r#"{"cmd":"set_source_spatial","entity":11,"doppler":1.0}"#).unwrap();
    assert_eq!(
        read,
        AudioCmd::SetSourceSpatial {
            entity: id,
            spatial: Spatial {
                doppler: 1.0,
                ..Spatial::default()
            },
        }
    );
    assert_eq!(round_trip(&AudioCmd::StopAll), r#"{"cmd":"stop_all"}"#);
    assert_eq!(
        round_trip(&AudioCmd::ApplySnapshot {
            name: "default".into(),
            seconds: 0.0
        }),
        r#"{"cmd":"apply_snapshot","name":"default","seconds":0.0}"#
    );
}

/// The `[audio]` section of ADR-0041's `real.toml` example, as JSON (re_types
/// does not depend on toml; `re_app` parses the TOML).
#[test]
fn audio_graph() {
    let graph: AudioGraphDesc = serde_json::from_str(
        r#"{
            "default_bus": "sfx",
            "buses": {
                "music": { "volume_db": -6.0 },
                "sfx": {
                    "sends": { "world_reverb": -9.0 },
                    "effects": [{ "kind": "compressor", "threshold_db": -18.0, "ratio": 3.0 }]
                },
                "footsteps": {
                    "parent": "sfx",
                    "effects": [{ "kind": "filter", "mode": "high_pass", "cutoff_hz": 120.0 }]
                }
            },
            "sends": {
                "world_reverb": {
                    "effects": [{ "kind": "reverb", "feedback": 0.85, "damping": 0.3, "mix": 1.0 }]
                }
            },
            "snapshots": {
                "underwater": {
                    "buses": { "music": -12.0, "sfx": -3.0 },
                    "effects": [{ "bus": "sfx", "effect": 0, "cutoff_hz": 600.0 }]
                }
            }
        }"#,
    )
    .unwrap();
    assert_eq!(graph.buses.len(), 3);
    assert_eq!(graph.buses["footsteps"].parent, "sfx");
    assert_eq!(
        graph.buses["footsteps"].effects,
        [EffectDesc::Filter(audio::Filter {
            mode: audio::FilterMode::HighPass,
            cutoff_hz: 120.0,
            ..Default::default()
        })]
    );
    assert_eq!(
        graph.buses["sfx"].effects,
        [EffectDesc::Compressor(audio::Compressor {
            threshold_db: -18.0,
            ratio: 3.0,
            ..Default::default()
        })]
    );
    let snap = &graph.snapshots["underwater"];
    assert_eq!(
        snap.effects[0].params["cutoff_hz"],
        EffectParam::Number(600.0)
    );
    let back: AudioGraphDesc =
        serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
    assert_eq!(back, graph);

    // A missing `buses` keeps the default three; an unknown key is refused.
    let only_default: AudioGraphDesc = serde_json::from_str(r#"{ "default_bus": "ui" }"#).unwrap();
    assert_eq!(only_default.buses.len(), 3);
    assert!(serde_json::from_str::<AudioGraphDesc>(r#"{ "busses": {} }"#).is_err());
    assert!(
        serde_json::from_str::<EffectDesc>(r#"{ "kind": "filter", "cutof_hz": 1.0 }"#).is_err()
    );
    assert!(AudioGraphDesc::default().is_default());
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
fn graph_layout_is_optional_and_omitted_when_empty() {
    // A file without `layout` writes back without one, byte for byte.
    let doc = GraphDoc::from_json(GRAPH).unwrap();
    assert!(doc.layout.is_empty());
    let pretty = doc.to_json();
    assert!(!pretty.contains("layout"), "{pretty}");
    assert_eq!(GraphDoc::from_json(&pretty).unwrap().to_json(), pretty);

    let with = GRAPH.replacen(
        r#""edges""#,
        r#""layout": { "t": [240.0, 0.0], "h": [0.0, -12.5] }, "edges""#,
        1,
    );
    let doc = GraphDoc::from_json(&with).unwrap();
    assert_eq!(doc.layout["h"], [0.0, -12.5]);
    assert_eq!(doc.layout["t"], [240.0, 0.0]);
    let pretty = doc.to_json();
    assert_eq!(GraphDoc::from_json(&pretty).unwrap(), doc);
    assert_eq!(GraphDoc::from_json(&pretty).unwrap().to_json(), pretty);
    round_trip(&doc);
    // Positions are pairs of numbers.
    let bad = GRAPH.replacen(r#""edges""#, r#""layout": { "h": [1.0] }, "edges""#, 1);
    assert!(matches!(
        GraphDoc::from_json(&bad),
        Err(GraphDocError::Parse(_))
    ));
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
    let newer = GRAPH.replacen(r#""version": 1, "name""#, r#""version": 3, "name""#, 1);
    assert_eq!(
        GraphDoc::from_json(&newer),
        Err(GraphDocError::UnsupportedVersion(3))
    );
    // A version 1 file keeps any id but one starting with `$`.
    let spaced = GRAPH.replace(r#""t""#, r#""my mesh""#);
    assert!(GraphDoc::from_json(&spaced).is_ok());
    let dollar = GRAPH.replace(r#""t""#, r#""$t""#);
    assert_eq!(
        GraphDoc::from_json(&dollar),
        Err(GraphDocError::InvalidNodeId("$t".into()))
    );
}

/// A kind graph (ADR-0061): a painted post with two params and one output.
const GRAPH_V2: &str = r#"{ "format": "real-engine/graph", "version": 2, "name": "post", "seed": 0,
  "kind": { "name": "project.post", "version": 2, "doc": "A painted post.",
    "migrations": [ { "from": 1, "note": "`tall` became `height`.", "rename": { "tall": "height" } } ] },
  "params": [
    { "name": "height", "type": "number", "default": 4, "min": 2, "max": 12, "doc": "Height, in metres." },
    { "name": "material", "type": "file", "default": "materials/iron.material.json", "extensions": [".material.json"] }
  ],
  "inputs": [ { "name": "ground", "type": "field", "required": true } ],
  "outputs": [ { "name": "post", "type": "fragment", "doc": "The post's entity." } ],
  "nodes": [
    { "id": "pole", "kind": "mesh.primitive", "version": 1, "params": { "shape": "cylinder", "height": { "$param": "height" } } },
    { "id": "painted", "kind": "material.assign", "version": 1, "params": { "material": { "$param": "material" } } },
    { "id": "out", "kind": "fragment.merge", "version": 1, "params": {} }
  ],
  "edges": [
    { "from": ["pole", "mesh"], "to": ["painted", "mesh"] },
    { "from": ["painted", "mesh"], "to": ["out", "pole"] },
    { "from": ["out", "fragment"], "to": ["$out", "post"] }
  ] }"#;

#[test]
fn graph_version_2() {
    let doc = GraphDoc::from_json(GRAPH_V2).unwrap();
    let kind = doc.kind.as_ref().unwrap();
    assert_eq!(
        (kind.name.as_str(), kind.version, kind.oldest),
        ("project.post", 2, 1)
    );
    assert_eq!(kind.migrations[0].rename["tall"], "height");
    assert!(!kind.migrations[0].changes_output);
    assert_eq!(doc.params[0].ty, ParamType::Number);
    assert_eq!(doc.params[1].ty, ParamType::File);
    assert_eq!(doc.params[1].doc, "");
    assert!(doc.inputs[0].required && !doc.outputs[0].required);
    let height = &doc.node("pole").unwrap().params["height"];
    assert_eq!(GraphDoc::param_ref(height), Some("height"));
    assert_eq!(
        GraphDoc::param_ref(&serde_json::json!({ "$param": 1 })),
        None
    );
    assert_eq!(
        GraphDoc::param_ref(&serde_json::json!({ "$param": "a", "b": 1 })),
        None
    );
    round_trip(&doc);
    // Written in field order, with defaults and empty fields left out.
    let pretty = doc.to_json();
    let order: Vec<usize> = [
        "\"seed\"",
        "\"kind\"",
        "\"params\"",
        "\"inputs\"",
        "\"outputs\"",
        "\"nodes\"",
    ]
    .iter()
    .map(|k| pretty.find(k).unwrap())
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{pretty}");
    for absent in [
        "oldest",
        "changes_output",
        "\"remove\"",
        "\"set\"",
        "\"values\"",
        "\"required\": false",
    ] {
        assert!(!pretty.contains(absent), "{absent} in {pretty}");
    }
    assert_eq!(GraphDoc::from_json(&pretty).unwrap(), doc);
    assert_eq!(GraphDoc::from_json(&pretty).unwrap().to_json(), pretty);
    round_trip(&ParamType::Str);
    assert_eq!(
        serde_json::to_string(&ParamType::Str).unwrap(),
        "\"string\""
    );
    assert_eq!(serde_json::to_string(&ParamType::Vec2).unwrap(), "\"vec2\"");
}

#[test]
fn graph_versions_and_ids() {
    // A new document is version 1 until it uses a version 2 field.
    let mut doc = GraphDoc::from_json(GRAPH).unwrap();
    assert_eq!(GraphDoc::new("g", 0).version, 1);
    assert_eq!(doc.version_2_use(), None);
    doc.raise_version();
    assert_eq!(doc.version, 1);
    doc.outputs.push(PortDoc {
        name: "ground".into(),
        ty: "mesh".into(),
        required: false,
        doc: String::new(),
    });
    assert_eq!(
        doc.validate(),
        Err(GraphDocError::NeedsVersion2("`outputs`".into()))
    );
    doc.raise_version();
    assert_eq!(doc.version, 2);
    assert_eq!(doc.validate(), Ok(()));
    doc.outputs.clear();
    doc.raise_version();
    assert_eq!(doc.version, 2, "nothing lowers a version");

    // Each version 2 use needs version 2.
    let v1 =
        |text: &str| GraphDoc::from_json(&text.replacen(r#""version": 2"#, r#""version": 1"#, 1));
    assert!(matches!(v1(GRAPH_V2), Err(GraphDocError::NeedsVersion2(_))));
    let bound = GRAPH.replace(r#"{ "size": 256 }"#, r#"{ "size": { "$param": "size" } }"#);
    assert!(matches!(
        GraphDoc::from_json(&bound),
        Err(GraphDocError::NeedsVersion2(_))
    ));
    // A reference inside a value is data in version 1, and `re_proc`'s to judge in version 2.
    let nested = GRAPH.replace(r#"[1, 2]"#, r#"[{ "$param": "a" }]"#);
    assert!(GraphDoc::from_json(&nested).is_ok());

    // Version 2 ids are letters, digits, `_` and `-`.
    let v2 =
        |text: &str| GraphDoc::from_json(&text.replacen(r#""version": 1"#, r#""version": 2"#, 1));
    assert!(v2(GRAPH).is_ok());
    for bad in ["my mesh", "a/b", "$t", "", "t.1"] {
        let renamed = GRAPH.replace(r#""t""#, &format!("{bad:?}"));
        assert_eq!(
            v2(&renamed),
            Err(GraphDocError::InvalidNodeId(bad.into())),
            "{bad}"
        );
    }
    assert!(GraphDoc::valid_node_id("street_row-2"));

    // `$in` only starts edges, `$out` only ends them.
    let wrong_end = GRAPH_V2.replace(r#"["$out", "post"] }"#, r#"["$in", "post"] }"#);
    assert_eq!(
        GraphDoc::from_json(&wrong_end),
        Err(GraphDocError::MisplacedPortNode {
            edge: 2,
            node: "$in".into()
        })
    );
    let wrong_start = GRAPH_V2.replace(
        r#"{ "from": ["pole", "mesh"]"#,
        r#"{ "from": ["$out", "mesh"]"#,
    );
    assert_eq!(
        GraphDoc::from_json(&wrong_start),
        Err(GraphDocError::MisplacedPortNode {
            edge: 0,
            node: "$out".into()
        })
    );
    let from_input = GRAPH_V2.replace(
        r#"{ "from": ["pole", "mesh"]"#,
        r#"{ "from": ["$in", "ground"]"#,
    );
    assert!(GraphDoc::from_json(&from_input).is_ok());
    let unknown = GRAPH_V2.replace(r#"["painted", "mesh"] },"#, r#"["paint", "mesh"] },"#);
    assert_eq!(
        GraphDoc::from_json(&unknown),
        Err(GraphDocError::UnknownNode {
            edge: 0,
            node: "paint".into()
        })
    );
    // Unknown keys in the new blocks are errors too.
    let extra = GRAPH_V2.replace(
        r#""doc": "A painted post.","#,
        r#""doc": "A painted post.", "colour": 1,"#,
    );
    assert!(matches!(
        GraphDoc::from_json(&extra),
        Err(GraphDocError::Parse(_))
    ));
    let bad_type = GRAPH_V2.replace(r#""type": "number""#, r#""type": "float""#);
    assert!(matches!(
        GraphDoc::from_json(&bad_type),
        Err(GraphDocError::Parse(_))
    ));
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

#[test]
fn skins() {
    let skin = SkinData {
        name: "hero".into(),
        joints: vec![
            JointDesc {
                name: "hips".into(),
                parent: None,
                rest: Trs::IDENTITY,
                parent_offset: Some(mat()),
            },
            JointDesc {
                name: "spine".into(),
                parent: Some(0),
                rest: Trs {
                    translation: Vec3::Y,
                    ..Trs::IDENTITY
                },
                parent_offset: None,
            },
        ],
        inverse_bind: vec![Mat4::IDENTITY, Mat4::from_translation(Vec3::NEG_Y)],
        joint_bounds: vec![None, Some(Aabb::new(Vec3::ZERO, Vec3::ONE))],
    };
    round_trip(&skin);
    assert!(skin.is_well_formed());
    assert_eq!(skin.joint("spine"), Some(1));
    let mut bad = skin.clone();
    bad.joints[0].parent = Some(1);
    assert!(!bad.is_well_formed(), "a parent after its child");
    bad = skin.clone();
    bad.inverse_bind.pop();
    assert!(!bad.is_well_formed());

    let mesh = MeshData {
        vertices: vec![Vertex::default(); 2],
        indices: Vec::new(),
    };
    let weights = SkinWeights {
        joints: vec![[0, 1, 0, 0], [1, 0, 0, 0]],
        weights: vec![[32768, 32767, 0, 0], [65535, 0, 0, 0]],
    };
    round_trip(&weights);
    assert!(weights.is_well_formed(&mesh, 2));
    assert!(!weights.is_well_formed(&mesh, 1), "joint 1 out of range");
    let mut short = weights.clone();
    short.weights[1][0] = 65534;
    assert!(!short.is_well_formed(&mesh, 2), "weights must sum to 65535");
    assert_eq!(Trs::IDENTITY.to_mat4(), Mat4::IDENTITY);
    let moved = Aabb::new(Vec3::ZERO, Vec3::ONE).transformed(&Mat4::from_translation(Vec3::X));
    assert_eq!(moved, Aabb::new(Vec3::X, Vec3::new(2.0, 1.0, 1.0)));
}
