//! Ragdolls (ADR-0040): the animation skeleton a ragdoll follows, its parts, its
//! modes, its readout and the animated/simulated blend, as plain data.
//!
//! `re_physics` never sees an animation crate's types: `re_app` fills a
//! [`RagdollSkeleton`] from a skin, and [`RagdollDesc::from_skeleton`] puts parts on
//! it. Poses are rigid ([`JointPose`]) and given in the skeleton's **model space**:
//! one transform per animation joint, in the space a skin's joint matrices use,
//! placed in the world by a frame (`pos`, `rot`) that travels with each command.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use super::{GroupMask, Layer, ShapeDesc};

/// The most joints a ragdoll's skeleton may have (oxijolt's skeleton limit).
pub const MAX_SKELETON_JOINTS: usize = 1024;

/// A rigid transform of one joint: a position and a unit rotation, no scale.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JointPose {
    /// Position.
    pub pos: Vec3,
    /// Rotation, a unit quaternion.
    pub rot: Quat,
}

impl Default for JointPose {
    fn default() -> Self {
        JointPose::IDENTITY
    }
}

impl JointPose {
    /// At the origin, not rotated.
    pub const IDENTITY: JointPose = JointPose {
        pos: Vec3::ZERO,
        rot: Quat::IDENTITY,
    };

    /// A pose at `pos` with rotation `rot`.
    pub const fn new(pos: Vec3, rot: Quat) -> JointPose {
        JointPose { pos, rot }
    }

    /// `self` followed by `local`: `local` given in `self`'s space.
    pub fn mul(&self, local: &JointPose) -> JointPose {
        JointPose {
            pos: self.pos + self.rot * local.pos,
            rot: (self.rot * local.rot).normalize(),
        }
    }

    /// The inverse transform.
    pub fn inverse(&self) -> JointPose {
        let rot = self.rot.conjugate();
        JointPose {
            pos: rot * -self.pos,
            rot,
        }
    }

    /// True when the position is finite and the rotation a finite unit quaternion
    /// (within 1e-3 of length 1).
    pub fn is_valid(&self) -> bool {
        self.pos.is_finite() && self.rot.is_finite() && (self.rot.length() - 1.0).abs() <= 1e-3
    }
}

/// One joint of a [`RagdollSkeleton`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkeletonJoint {
    /// The joint's name, unique in the skeleton; parts name their joint by it.
    pub name: String,
    /// The parent joint, always an earlier index; `None` only for joint 0, the root.
    pub parent: Option<u16>,
    /// The joint in the skeleton's neutral pose, model space. Parts are built in this
    /// pose, so their shapes and joint limits are measured from it.
    pub neutral: JointPose,
}

/// The animation skeleton a ragdoll follows: joint names, parents and a neutral
/// pose in model space. `re_app` fills it from a skin; it may be more detailed than
/// the ragdoll, whose parts follow some of its joints.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RagdollSkeleton {
    /// The joints, every parent before its children, joint 0 the only root.
    pub joints: Vec<SkeletonJoint>,
}

impl RagdollSkeleton {
    /// The index of the joint called `name`.
    pub fn joint(&self, name: &str) -> Option<usize> {
        self.joints.iter().position(|j| j.name == name)
    }

    /// Why the skeleton cannot carry a ragdoll, or `None` when it can: 1 to
    /// [`MAX_SKELETON_JOINTS`] joints, joint 0 the only root, every other parent
    /// before its child, unique names without NUL, and a valid neutral pose.
    pub fn problem(&self) -> Option<String> {
        if self.joints.is_empty() || self.joints.len() > MAX_SKELETON_JOINTS {
            return Some(format!(
                "it has {} joints; a ragdoll skeleton has 1 to {MAX_SKELETON_JOINTS}",
                self.joints.len()
            ));
        }
        let mut names = std::collections::BTreeSet::new();
        for (i, j) in self.joints.iter().enumerate() {
            let parent_ok = match j.parent {
                None => i == 0,
                Some(p) => (p as usize) < i,
            };
            if !parent_ok {
                return Some(if j.parent.is_none() {
                    format!(
                        "joint `{}` is a second root; joint 0 must be the only one",
                        j.name
                    )
                } else {
                    format!("joint `{}` comes before its parent", j.name)
                });
            }
            if j.name.is_empty() || j.name.contains('\0') || !names.insert(j.name.as_str()) {
                return Some(format!(
                    "joint name `{}` is empty, has a NUL or is used twice",
                    j.name
                ));
            }
            if !j.neutral.is_valid() {
                return Some(format!(
                    "joint `{}` has a neutral pose that is not finite or not a unit rotation",
                    j.name
                ));
            }
        }
        None
    }

    /// The neutral pose of every joint, in joint order.
    pub fn neutral_pose(&self) -> Vec<JointPose> {
        self.joints.iter().map(|j| j.neutral).collect()
    }

    /// The joints a generated humanoid ragdoll puts parts on, in joint order: the
    /// hips, the topmost spine joint, the head, and on each side the upper arm,
    /// forearm, thigh and shin, found by words in the joint names (Blender, Mixamo
    /// and Roblox R15 spellings; `L`/`R` and `Left`/`Right` sides). Joints whose names
    /// end a chain (`end`, `top`, `tip`, `nub`) or are twist helpers are skipped. Each
    /// kind takes its topmost match; kinds without one are left out.
    pub fn humanoid_parts(&self) -> Vec<&str> {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        enum Kind {
            Forearm,
            UpperArm,
            Thigh,
            Shin,
            Hips,
            Spine,
            Head,
        }
        let kind = |name: &str| -> Option<Kind> {
            let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
            if has(&["end", "top", "tip", "nub", "twist", "roll", "armature"]) {
                return None;
            }
            Some(if has(&["forearm", "lowerarm", "lower_arm", "elbow"]) {
                Kind::Forearm
            } else if has(&["upperarm", "upper_arm", "arm"]) {
                Kind::UpperArm
            } else if has(&["thigh", "upleg", "upperleg", "upper_leg"]) {
                Kind::Thigh
            } else if has(&["shin", "calf", "lowerleg", "lower_leg", "knee", "leg"]) {
                Kind::Shin
            } else if has(&["hips", "pelvis", "lowertorso"]) {
                Kind::Hips
            } else if has(&["spine", "chest", "torso"]) {
                Kind::Spine
            } else if has(&["head"]) {
                Kind::Head
            } else {
                return None;
            })
        };
        let side = |name: &str| -> i8 {
            let ends = |s: &[&str]| s.iter().any(|e| name.ends_with(e));
            let starts = |s: &[&str]| s.iter().any(|e| name.starts_with(e));
            if name.contains("left") || ends(&["_l", ".l", "-l", " l"]) || starts(&["l_", "l."]) {
                -1
            } else if name.contains("right")
                || ends(&["_r", ".r", "-r", " r"])
                || starts(&["r_", "r."])
            {
                1
            } else {
                0
            }
        };
        let depth = |mut j: usize| {
            let mut d = 0;
            while let Some(p) = self.joints[j].parent {
                j = usize::from(p);
                d += 1;
            }
            d
        };
        let mut best: std::collections::BTreeMap<(Kind, i8), (usize, usize)> =
            std::collections::BTreeMap::new();
        for (i, j) in self.joints.iter().enumerate() {
            let name = j.name.to_ascii_lowercase();
            let Some(k) = kind(&name) else {
                continue;
            };
            let s = side(&name);
            let sided = matches!(k, Kind::Forearm | Kind::UpperArm | Kind::Thigh | Kind::Shin);
            if sided == (s == 0) {
                continue;
            }
            let d = depth(i);
            let slot = best.entry((k, s)).or_insert((d, i));
            if (d, i) < *slot {
                *slot = (d, i);
            }
        }
        let mut picked: Vec<usize> = best.values().map(|&(_, i)| i).collect();
        picked.sort_unstable();
        picked
            .into_iter()
            .map(|i| self.joints[i].name.as_str())
            .collect()
    }

    /// True when joint `ancestor` is `joint` or above it.
    pub fn is_ancestor(&self, ancestor: usize, joint: usize) -> bool {
        let mut at = Some(joint);
        while let Some(j) = at {
            if j == ancestor {
                return true;
            }
            at = self.joints.get(j).and_then(|j| j.parent).map(usize::from);
        }
        false
    }
}

/// The joint between a ragdoll part and its parent part, with its limits in degrees
/// (ADR-0040).
///
/// Its frame sits at the part's joint. The twist axis is the part's `twist_axis`
/// and the plane axis its `plane_axis`, both in the joint's space in the neutral
/// pose; a hinge turns about the plane axis.
///
/// Serialized with a `type` tag, for example
/// `{"type": "hinge", "min_deg": 0, "max_deg": 140}`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RagdollConstraint {
    /// Swing within a cone around the twist axis, and twist about it: spine, neck,
    /// shoulders, hips.
    SwingTwist {
        /// The cone's half angle, 0 to 180.
        cone_deg: f32,
        /// Least twist, -180 to `twist_max_deg`.
        twist_min_deg: f32,
        /// Most twist, up to 180.
        twist_max_deg: f32,
    },
    /// Rotation about the plane axis only: elbows, knees.
    Hinge {
        /// Least angle, -180 to `max_deg`.
        min_deg: f32,
        /// Most angle, up to 180.
        max_deg: f32,
    },
    /// Independent ranges about the twist axis (X), the plane axis (Y) and their
    /// normal (Z), without translation; a pyramid instead of a cone.
    SixDof {
        /// Twist range about X.
        twist_min_deg: f32,
        /// See `twist_min_deg`.
        twist_max_deg: f32,
        /// Swing range about Y.
        swing_y_min_deg: f32,
        /// See `swing_y_min_deg`.
        swing_y_max_deg: f32,
        /// Swing range about Z.
        swing_z_min_deg: f32,
        /// See `swing_z_min_deg`.
        swing_z_max_deg: f32,
    },
}

impl Default for RagdollConstraint {
    /// A swing-twist joint with a 30° cone and ±20° of twist, stiff enough for a
    /// spine segment.
    fn default() -> Self {
        RagdollConstraint::SwingTwist {
            cone_deg: 30.0,
            twist_min_deg: -20.0,
            twist_max_deg: 20.0,
        }
    }
}

impl RagdollConstraint {
    /// True when every angle is finite, within ±180° (cones 0 to 180°), and each
    /// minimum is at most its maximum.
    pub fn is_valid(&self) -> bool {
        let range = |lo: f32, hi: f32| {
            lo.is_finite()
                && hi.is_finite()
                && (-180.0..=180.0).contains(&lo)
                && lo <= hi
                && hi <= 180.0
        };
        match *self {
            RagdollConstraint::SwingTwist {
                cone_deg,
                twist_min_deg,
                twist_max_deg,
            } => range(0.0, cone_deg) && range(twist_min_deg, twist_max_deg),
            RagdollConstraint::Hinge { min_deg, max_deg } => range(min_deg, max_deg),
            RagdollConstraint::SixDof {
                twist_min_deg,
                twist_max_deg,
                swing_y_min_deg,
                swing_y_max_deg,
                swing_z_min_deg,
                swing_z_max_deg,
            } => {
                range(twist_min_deg, twist_max_deg)
                    && range(swing_y_min_deg, swing_y_max_deg)
                    && range(swing_z_min_deg, swing_z_max_deg)
            }
        }
    }
}

/// One body of a ragdoll, following one animation joint (ADR-0040).
///
/// The body's origin is the joint: its shape is placed in the joint's space by
/// `shape_pos` and `shape_rot`, so a capsule along a bone sits halfway down it.
/// The part's parent is the part on the nearest ancestor joint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RagdollPartDesc {
    /// The animation joint the part follows, by name.
    pub joint: String,
    /// The collision shape: a capsule, box, sphere or convex hull.
    pub shape: ShapeDesc,
    /// Where the shape's origin is, in the joint's space.
    pub shape_pos: Vec3,
    /// How the shape is turned, in the joint's space.
    pub shape_rot: Quat,
    /// Mass in kg, above 0.
    pub mass: f32,
    /// The joint to the parent part; ignored for the root part.
    pub constraint: RagdollConstraint,
    /// The constraint's twist axis, in the joint's space (usually along the bone).
    pub twist_axis: Vec3,
    /// The constraint's plane axis (a hinge's axis), in the joint's space; made
    /// perpendicular to `twist_axis`.
    pub plane_axis: Vec3,
    /// Contact friction.
    pub friction: f32,
    /// Torque the joint's friction applies while no motor drives it, N·m; it lets a
    /// limp ragdoll come to rest.
    pub joint_friction: f32,
    /// The spring frequency of the joint's motors in `Powered` mode at full
    /// strength, Hz.
    pub motor_hz: f32,
}

impl Default for RagdollPartDesc {
    fn default() -> Self {
        RagdollPartDesc {
            joint: String::new(),
            shape: ShapeDesc::Capsule {
                half_height: 0.1,
                radius: 0.05,
            },
            shape_pos: Vec3::ZERO,
            shape_rot: Quat::IDENTITY,
            mass: 5.0,
            constraint: RagdollConstraint::default(),
            twist_axis: Vec3::Y,
            plane_axis: Vec3::X,
            friction: 0.8,
            joint_friction: 1.0,
            motor_hz: 4.0,
        }
    }
}

/// Everything needed to build a ragdoll for an entity, from
/// [`PhysicsCmd::AddRagdoll`](super::PhysicsCmd::AddRagdoll): the animation
/// skeleton and the parts on it (ADR-0040).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RagdollDesc {
    /// The animation skeleton the parts follow.
    pub skeleton: RagdollSkeleton,
    /// The parts, in any order; one joint has at most one part, and one part (the
    /// root part) is an ancestor of all the others.
    pub parts: Vec<RagdollPartDesc>,
    /// Keep each parent/child mass ratio within [0.8, 1.2] while keeping every
    /// chain's mass, which keeps chains stiff (Jolt's `Stabilize`; the default).
    /// `false` keeps the authored masses.
    pub stabilize: bool,
    /// The parts' object layer.
    pub layer: Layer,
    /// The parts' collision groups, for characters and queries.
    pub groups: GroupMask,
    /// The parts' linear damping.
    pub linear_damping: f32,
    /// The parts' angular damping.
    pub angular_damping: f32,
}

impl RagdollDesc {
    /// An empty ragdoll on `skeleton` with the default settings.
    fn on(skeleton: &RagdollSkeleton) -> RagdollDesc {
        RagdollDesc {
            skeleton: skeleton.clone(),
            parts: Vec::new(),
            stabilize: true,
            layer: Layer::default(),
            groups: GroupMask::default(),
            linear_damping: 0.1,
            angular_damping: 0.2,
        }
    }

    /// The procedural default ragdoll (ADR-0040): one capsule per joint of `joints`
    /// along its bone, a swing-twist joint with humanoid limits chosen by the joint's
    /// name, and `total_mass` kg split by capsule volume. The result is plain data
    /// a project edits part by part.
    ///
    /// A part's bone runs from its joint to its nearest chosen descendants (their
    /// mean position, when several branch off far from it), else to its first child
    /// joint, else on along its parent bone for half that bone's length. A part whose
    /// children spread around it (a pelvis) is a sphere among them.
    ///
    /// # Errors
    ///
    /// A message naming the problem when the skeleton is malformed, a name is not a
    /// joint or is given twice, no joint is given, the chosen joints have no single
    /// topmost joint, or `total_mass` is not positive and finite.
    pub fn from_skeleton(
        skeleton: &RagdollSkeleton,
        joints: &[&str],
        total_mass: f32,
    ) -> Result<RagdollDesc, String> {
        if let Some(why) = skeleton.problem() {
            return Err(format!("the skeleton cannot carry a ragdoll: {why}"));
        }
        if !(total_mass.is_finite() && total_mass > 0.0) {
            return Err(format!("total mass {total_mass} kg must be positive"));
        }
        if joints.is_empty() {
            return Err("no joint was chosen for a part".into());
        }
        let mut chosen = Vec::with_capacity(joints.len());
        for name in joints {
            let j = skeleton
                .joint(name)
                .ok_or_else(|| format!("`{name}` is not a joint of the skeleton"))?;
            if chosen.contains(&j) {
                return Err(format!("joint `{name}` is chosen twice"));
            }
            chosen.push(j);
        }
        chosen.sort_unstable();
        let parent_part = |j: usize| -> Option<usize> {
            let mut at = skeleton.joints[j].parent.map(usize::from);
            while let Some(a) = at {
                if chosen.contains(&a) {
                    return Some(a);
                }
                at = skeleton.joints[a].parent.map(usize::from);
            }
            None
        };
        let roots: Vec<&str> = chosen
            .iter()
            .filter(|&&j| parent_part(j).is_none())
            .map(|&j| skeleton.joints[j].name.as_str())
            .collect();
        if roots.len() != 1 {
            return Err(format!(
                "the chosen joints {roots:?} have no part above them; one chosen joint must \
                 be above all the others"
            ));
        }
        let pos = |j: usize| skeleton.joints[j].neutral.pos;
        let mut shapes = Vec::with_capacity(chosen.len());
        for &j in &chosen {
            let start = pos(j);
            let children: Vec<Vec3> = chosen
                .iter()
                .filter(|&&c| parent_part(c) == Some(j))
                .map(|&c| pos(c))
                .collect();
            let end = if !children.is_empty() {
                Some(children.iter().copied().sum::<Vec3>() / children.len() as f32)
            } else if let Some(c) = (j + 1..skeleton.joints.len())
                .find(|&c| skeleton.joints[c].parent.map(usize::from) == Some(j))
            {
                Some(pos(c))
            } else {
                parent_part(j)
                    .or(skeleton.joints[j].parent.map(usize::from))
                    .map(|p| start + (start - pos(p)) * 0.5)
            };
            let reach = if children.is_empty() {
                end.map_or(0.0, |e| e.distance(start))
            } else {
                children.iter().map(|c| c.distance(start)).sum::<f32>() / children.len() as f32
            };
            shapes.push(Bone::new(start, end, reach, children.len()));
        }
        let volume: f32 = shapes.iter().map(Bone::volume).sum();
        let mut desc = RagdollDesc::on(skeleton);
        for (&j, bone) in chosen.iter().zip(&shapes) {
            let joint = &skeleton.joints[j];
            let to_joint = joint.neutral.rot.conjugate();
            // A sphere has no bone: its joint keeps the joint's own +Y as its twist axis.
            let dir = bone.dir.map_or(Vec3::Y, |d| to_joint * d);
            desc.parts.push(RagdollPartDesc {
                joint: joint.name.clone(),
                shape: bone.shape(),
                shape_pos: to_joint * (bone.centre - bone.start),
                shape_rot: Quat::from_rotation_arc(Vec3::Y, dir),
                mass: total_mass * bone.volume() / volume,
                constraint: humanoid_limits(&joint.name),
                twist_axis: dir,
                plane_axis: any_perpendicular(dir),
                ..RagdollPartDesc::default()
            });
        }
        Ok(desc)
    }
}

/// The capsule or sphere of one generated part, in model space.
struct Bone {
    start: Vec3,
    centre: Vec3,
    /// Unit direction of the bone; `None` for a sphere.
    dir: Option<Vec3>,
    half_height: f32,
    radius: f32,
}

impl Bone {
    fn new(start: Vec3, end: Option<Vec3>, reach: f32, children: usize) -> Bone {
        let sphere = |centre: Vec3, radius: f32| Bone {
            start,
            centre,
            dir: None,
            half_height: 0.0,
            radius,
        };
        let Some(end) = end else {
            return sphere(start, 0.1);
        };
        let length = end.distance(start);
        // Children that branch off in different directions (a pelvis to its spine and
        // legs) average out near the joint: a sphere among them fits better.
        if length < 1e-3 || (children > 1 && length < 0.25 * reach) {
            let radius = (0.8 * reach).clamp(0.03, 0.2);
            return sphere((start + end) * 0.5, radius);
        }
        let radius = (0.2 * length).clamp(0.02, 0.15);
        Bone {
            start,
            centre: (start + end) * 0.5,
            dir: Some((end - start) / length),
            half_height: (0.5 * length - radius).max(0.0),
            radius,
        }
    }

    fn shape(&self) -> ShapeDesc {
        if self.half_height > 0.0 {
            ShapeDesc::Capsule {
                half_height: self.half_height,
                radius: self.radius,
            }
        } else {
            ShapeDesc::Sphere {
                radius: self.radius,
            }
        }
    }

    fn volume(&self) -> f32 {
        let r = self.radius;
        std::f32::consts::PI * r * r * (2.0 * self.half_height + 4.0 / 3.0 * r)
    }
}

/// A unit vector perpendicular to the unit vector `v`.
fn any_perpendicular(v: Vec3) -> Vec3 {
    let other = if v.x.abs() < 0.9 { Vec3::X } else { Vec3::Z };
    (other - v * v.dot(other)).normalize()
}

/// The default limits of a generated joint, from words in its name (Blender, Mixamo
/// and Roblox spellings), else a spine segment's.
fn humanoid_limits(name: &str) -> RagdollConstraint {
    let name = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    let swing = |cone_deg: f32, twist_deg: f32| RagdollConstraint::SwingTwist {
        cone_deg,
        twist_min_deg: -twist_deg,
        twist_max_deg: twist_deg,
    };
    if has(&["forearm", "lowerarm", "lower_arm", "elbow"]) {
        swing(70.0, 30.0)
    } else if has(&["shin", "calf", "knee", "lowerleg", "lower_leg"]) {
        swing(60.0, 10.0)
    } else if has(&["hand", "wrist", "foot", "ankle", "toe"]) {
        swing(40.0, 15.0)
    } else if has(&["arm", "shoulder", "clavicle"]) {
        swing(80.0, 45.0)
    } else if has(&["thigh", "upleg", "upper_leg", "upperleg", "leg", "hip"]) {
        swing(60.0, 20.0)
    } else if has(&["head", "neck"]) {
        swing(45.0, 45.0)
    } else {
        RagdollConstraint::default()
    }
}

/// How a ragdoll moves (ADR-0040).
///
/// Serialized with a `mode` tag, for example `{"mode": "powered", "strength": 0.5}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RagdollMode {
    /// Kinematic parts driven to the target pose every step: the ragdoll pushes the
    /// world but does not fall. A new ragdoll starts animated.
    #[default]
    Animated,
    /// Dynamic parts with the motors off: a limp ragdoll.
    Simulated,
    /// Dynamic parts whose joint motors drive them to the target pose: an active
    /// ragdoll that holds the animation against hits.
    Powered {
        /// Scales every motor's spring frequency between 0 (limp) and the part's
        /// `motor_hz` (1).
        strength: f32,
    },
}

impl RagdollMode {
    /// True when the parts are dynamic (simulated or powered).
    pub fn is_dynamic(&self) -> bool {
        !matches!(self, RagdollMode::Animated)
    }

    /// True when `strength` (if any) is finite and in `[0, 1]`.
    pub fn is_valid(&self) -> bool {
        match *self {
            RagdollMode::Powered { strength } => (0.0..=1.0).contains(&strength),
            _ => true,
        }
    }
}

/// A ragdoll's state after the last step, from `re_physics::Physics::ragdoll`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RagdollState {
    /// The mode last set.
    pub mode: RagdollMode,
    /// The frame `pose` is given in: the world position of the skeleton's model
    /// space, from the last `AddRagdoll` or `SetRagdollTarget`.
    pub pos: Vec3,
    /// The frame's world rotation.
    pub rot: Quat,
    /// The root part's world position.
    pub root_pos: Vec3,
    /// The root part's world rotation.
    pub root_rot: Quat,
    /// True once every part has stayed slower than 0.05 m/s and 0.1 rad/s for 30
    /// steps (or sleeps).
    pub calm: bool,
    /// The simulated pose of every animation joint in model space, relative to the
    /// frame: joints with a part follow it, the joints between keep their target's
    /// local pose and turn to point at the parts below them.
    pub pose: Vec<JointPose>,
}

/// The weight of a ragdoll's simulated pose against the animated one, and how fast
/// it moves (ADR-0040): 0 shows the animation, 1 the ragdoll.
///
/// It lives in `re_app` next to the animation, never in physics, so render-rate
/// blending cannot change a replay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RagdollBlend {
    /// The current weight, 0 to 1.
    pub weight: f32,
    /// Seconds a full ramp from 0 to 1 (or back) takes; 0 or less jumps.
    pub seconds: f32,
}

impl RagdollBlend {
    /// Moves `weight` toward `target` (clamped to `[0, 1]`) by `dt / seconds`.
    pub fn advance(&mut self, target: f32, dt: f32) {
        let target = target.clamp(0.0, 1.0);
        if self.seconds <= 0.0 || !dt.is_finite() {
            self.weight = target;
            return;
        }
        let step = (dt / self.seconds).max(0.0);
        self.weight = if self.weight < target {
            (self.weight + step).min(target)
        } else {
            (self.weight - step).max(target)
        };
    }
}

/// Blends two poses joint by joint (ADR-0040): positions linearly, rotations by
/// normalised lerp on the shorter arc, `weight` 0 giving `animated` and 1
/// `simulated`. Normalised lerp needs no trigonometry, as in ADR-0039's blending.
/// The result has as many joints as the shorter pose.
pub fn blend_pose(animated: &[JointPose], simulated: &[JointPose], weight: f32) -> Vec<JointPose> {
    let w = weight.clamp(0.0, 1.0);
    animated
        .iter()
        .zip(simulated)
        .map(|(a, s)| {
            let b = if a.rot.dot(s.rot) < 0.0 {
                -s.rot
            } else {
                s.rot
            };
            JointPose {
                pos: a.pos.lerp(s.pos, w),
                rot: (a.rot * (1.0 - w) + b * w).normalize(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1.8 m humanoid standing along +Y at the origin, every joint unrotated.
    fn humanoid() -> RagdollSkeleton {
        let joint = |name: &str, parent: Option<u16>, pos: [f32; 3]| SkeletonJoint {
            name: name.into(),
            parent,
            neutral: JointPose::new(Vec3::from(pos), Quat::IDENTITY),
        };
        RagdollSkeleton {
            joints: vec![
                joint("hips", None, [0.0, 1.0, 0.0]),
                joint("spine", Some(0), [0.0, 1.2, 0.0]),
                joint("head", Some(1), [0.0, 1.55, 0.0]),
                joint("head_end", Some(2), [0.0, 1.8, 0.0]),
                joint("upper_arm_l", Some(1), [0.2, 1.45, 0.0]),
                joint("forearm_l", Some(4), [0.5, 1.45, 0.0]),
                joint("hand_l", Some(5), [0.75, 1.45, 0.0]),
                joint("thigh_l", Some(0), [0.1, 0.95, 0.0]),
                joint("shin_l", Some(7), [0.1, 0.5, 0.0]),
                joint("foot_l", Some(8), [0.1, 0.08, 0.0]),
                joint("thigh_r", Some(0), [-0.1, 0.95, 0.0]),
            ],
        }
    }

    const PARTS: [&str; 8] = [
        "hips",
        "spine",
        "head",
        "upper_arm_l",
        "forearm_l",
        "thigh_l",
        "shin_l",
        "thigh_r",
    ];

    #[test]
    fn from_skeleton_puts_capsules_along_the_bones_and_splits_the_mass() {
        let s = humanoid();
        let d = RagdollDesc::from_skeleton(&s, &PARTS, 70.0).unwrap();
        assert_eq!(d.parts.len(), 8);
        let total: f32 = d.parts.iter().map(|p| p.mass).sum();
        assert!((total - 70.0).abs() < 1e-3, "{total}");
        let part = |name: &str| d.parts.iter().find(|p| p.joint == name).unwrap();
        // The shin runs to its first child (the foot): down 0.42 m, centred halfway.
        let shin = part("shin_l");
        assert!(shin.shape_pos.abs_diff_eq(Vec3::new(0.0, -0.21, 0.0), 1e-5));
        assert!((shin.shape_rot * Vec3::Y).abs_diff_eq(-Vec3::Y, 1e-5));
        assert!(shin.twist_axis.abs_diff_eq(-Vec3::Y, 1e-5));
        assert!(shin.plane_axis.dot(shin.twist_axis).abs() < 1e-6);
        let ShapeDesc::Capsule {
            half_height,
            radius,
        } = shin.shape
        else {
            panic!("{:?}", shin.shape)
        };
        assert!((half_height + radius - 0.21).abs() < 1e-5);
        // The forearm is a leaf part: its bone ends at the hand joint.
        assert!(
            part("forearm_l")
                .shape_pos
                .abs_diff_eq(Vec3::new(0.125, 0.0, 0.0), 1e-5)
        );
        // The hips' children (spine and both thighs) average out near the hips.
        assert!(matches!(part("hips").shape, ShapeDesc::Sphere { .. }));
        // The default limits come from the joint names.
        assert_eq!(
            part("forearm_l").constraint,
            RagdollConstraint::SwingTwist {
                cone_deg: 70.0,
                twist_min_deg: -30.0,
                twist_max_deg: 30.0
            }
        );
        // Mass follows volume: a thigh outweighs a forearm.
        assert!(part("thigh_l").mass > part("forearm_l").mass);
        // The result round-trips as data.
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<RagdollDesc>(&json).unwrap(), d);
    }

    #[test]
    fn humanoid_parts_are_found_by_name() {
        assert_eq!(humanoid().humanoid_parts(), PARTS);
        // Mixamo's spellings, with a prefix, an armature root and helper joints.
        let names = [
            "Armature",
            "mixamorig:Hips",
            "mixamorig:Spine",
            "mixamorig:Spine1",
            "mixamorig:Neck",
            "mixamorig:Head",
            "mixamorig:HeadTop_End",
            "mixamorig:LeftShoulder",
            "mixamorig:LeftArm",
            "mixamorig:LeftForeArm",
            "mixamorig:LeftHand",
            "mixamorig:RightUpLeg",
            "mixamorig:RightLeg",
            "mixamorig:RightFoot",
        ];
        let parents = [
            None,
            Some(0),
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(3),
            Some(7),
            Some(8),
            Some(9),
            Some(1),
            Some(11),
            Some(12),
        ];
        let s = RagdollSkeleton {
            joints: names
                .iter()
                .zip(parents)
                .map(|(n, p)| SkeletonJoint {
                    name: (*n).into(),
                    parent: p,
                    neutral: JointPose::IDENTITY,
                })
                .collect(),
        };
        assert_eq!(
            s.humanoid_parts(),
            [
                "mixamorig:Hips",
                "mixamorig:Spine",
                "mixamorig:Head",
                "mixamorig:LeftArm",
                "mixamorig:LeftForeArm",
                "mixamorig:RightUpLeg",
                "mixamorig:RightLeg"
            ]
        );
    }

    #[test]
    fn from_skeleton_measures_in_the_joint_space() {
        // The same skeleton turned a quarter turn about Z: parts keep their joint-space
        // shapes.
        let mut s = humanoid();
        let turn = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        for j in &mut s.joints {
            j.neutral = JointPose::new(turn * j.neutral.pos, turn);
        }
        let a = RagdollDesc::from_skeleton(&humanoid(), &PARTS, 70.0).unwrap();
        let b = RagdollDesc::from_skeleton(&s, &PARTS, 70.0).unwrap();
        for (p, q) in a.parts.iter().zip(&b.parts) {
            assert!(p.shape_pos.abs_diff_eq(q.shape_pos, 1e-5), "{}", p.joint);
            assert!(p.twist_axis.abs_diff_eq(q.twist_axis, 1e-5), "{}", p.joint);
            assert!((p.mass - q.mass).abs() < 1e-4);
        }
    }

    #[test]
    fn from_skeleton_refuses_bad_choices() {
        let s = humanoid();
        let err =
            |joints: &[&str], mass: f32| RagdollDesc::from_skeleton(&s, joints, mass).unwrap_err();
        assert!(err(&["hips", "elbow"], 70.0).contains("`elbow` is not a joint"));
        assert!(err(&["hips", "hips"], 70.0).contains("chosen twice"));
        assert!(err(&[], 70.0).contains("no joint"));
        assert!(err(&["thigh_l", "thigh_r"], 70.0).contains("no part above them"));
        assert!(err(&["hips"], 0.0).contains("must be positive"));
        let mut two_roots = humanoid();
        two_roots.joints[3].parent = None;
        assert!(
            RagdollDesc::from_skeleton(&two_roots, &["hips"], 70.0)
                .unwrap_err()
                .contains("second root")
        );
        // A single joint is a sphere.
        let one = RagdollDesc::from_skeleton(&s, &["head_end"], 5.0).unwrap();
        assert!(matches!(
            one.parts[0].shape,
            ShapeDesc::Capsule { .. } | ShapeDesc::Sphere { .. }
        ));
    }

    #[test]
    fn blend_pose_lerps_positions_and_nlerps_rotations_on_the_short_arc() {
        let a = [JointPose::new(Vec3::ZERO, Quat::IDENTITY)];
        let turn = Quat::from_rotation_y(1.0);
        // The same rotation with a negated sign blends as the short arc.
        let s = [JointPose::new(Vec3::new(2.0, 0.0, 0.0), -turn)];
        assert_eq!(blend_pose(&a, &s, 0.0)[0], a[0]);
        let full = blend_pose(&a, &s, 1.0)[0];
        assert!(full.rot.abs_diff_eq(turn, 1e-6));
        let half = blend_pose(&a, &s, 0.5)[0];
        assert!(half.pos.abs_diff_eq(Vec3::new(1.0, 0.0, 0.0), 1e-6));
        assert!(half.rot.angle_between(Quat::from_rotation_y(0.5)) < 1e-4);
        assert!((half.rot.length() - 1.0).abs() < 1e-6);
        assert_eq!(blend_pose(&a, &s, 7.0)[0].pos, s[0].pos);
    }

    #[test]
    fn a_blend_ramps_over_its_seconds() {
        let mut b = RagdollBlend {
            weight: 0.0,
            seconds: 0.5,
        };
        b.advance(1.0, 0.25);
        assert!((b.weight - 0.5).abs() < 1e-6);
        b.advance(1.0, 1.0);
        assert_eq!(b.weight, 1.0);
        b.advance(0.0, 0.1);
        assert!((b.weight - 0.8).abs() < 1e-6);
        let mut jump = RagdollBlend::default();
        jump.advance(1.0, 0.01);
        assert_eq!(jump.weight, 1.0);
    }

    #[test]
    fn joint_poses_compose_and_invert() {
        let a = JointPose::new(Vec3::new(1.0, 2.0, 3.0), Quat::from_rotation_x(0.3));
        let b = JointPose::new(Vec3::new(-1.0, 0.5, 0.0), Quat::from_rotation_z(-0.7));
        let ab = a.mul(&b);
        let back = a.inverse().mul(&ab);
        assert!(back.pos.abs_diff_eq(b.pos, 1e-5));
        assert!(back.rot.abs_diff_eq(b.rot, 1e-5));
        assert!(!JointPose::new(Vec3::NAN, Quat::IDENTITY).is_valid());
        assert!(!JointPose::new(Vec3::ZERO, Quat::from_xyzw(0.0, 0.0, 0.0, 2.0)).is_valid());
    }

    #[test]
    fn modes_serialize_with_a_mode_tag() {
        let m = RagdollMode::Powered { strength: 0.5 };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"mode":"powered","strength":0.5}"#
        );
        assert!(!RagdollMode::Powered { strength: 1.5 }.is_valid());
        assert!(!RagdollMode::Powered { strength: f32::NAN }.is_valid());
        assert!(RagdollMode::Simulated.is_dynamic());
    }
}
