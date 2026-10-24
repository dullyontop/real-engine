//! Animation clips: produced by `re_assets` from glTF animations and sampled by
//! `re_anim`.
//!
//! A clip animates named targets (glTF nodes, matched to entities by name below
//! the animated root). Each target has optional translation, rotation and scale
//! channels.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

/// An animation clip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipData {
    /// The clip's name (the glTF animation name, or `animation_<index>`).
    pub name: String,
    /// The clip length in seconds (the latest keyframe time over all channels).
    pub duration: f32,
    /// The animated targets, in source order.
    pub targets: Vec<TargetChannels>,
}

/// The channels that animate one target.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TargetChannels {
    /// The target's name (the glTF node name).
    pub target: String,
    /// Translation keyframes.
    pub translation: Option<Channel<Vec3>>,
    /// Rotation keyframes (unit quaternions).
    pub rotation: Option<Channel<Quat>>,
    /// Scale keyframes.
    pub scale: Option<Channel<Vec3>>,
}

/// How values between two keyframes are computed (glTF sampler interpolation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Interpolation {
    /// Hold the previous keyframe's value.
    Step,
    /// Linear interpolation (spherical for rotations).
    #[default]
    Linear,
    /// Cubic Hermite spline. `values` holds three entries per keyframe:
    /// in-tangent, value, out-tangent.
    CubicSpline,
}

/// Keyframes of one property.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Channel<T> {
    /// How to interpolate between keyframes.
    pub interpolation: Interpolation,
    /// Keyframe times in seconds, strictly increasing.
    pub times: Vec<f32>,
    /// Keyframe values: one per time, or three per time for
    /// [`Interpolation::CubicSpline`].
    pub values: Vec<T>,
}

impl<T> Channel<T> {
    /// True when `values` has the length `times` and `interpolation` require.
    pub fn is_well_formed(&self) -> bool {
        let per_key = match self.interpolation {
            Interpolation::CubicSpline => 3,
            Interpolation::Step | Interpolation::Linear => 1,
        };
        self.values.len() == self.times.len() * per_key
            && self.times.windows(2).all(|w| w[0] < w[1])
    }
}

/// A translation, rotation and scale: a joint's local pose.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trs {
    /// Translation.
    pub translation: Vec3,
    /// Rotation (a unit quaternion).
    pub rotation: Quat,
    /// Scale.
    pub scale: Vec3,
}

impl Trs {
    /// No translation or rotation, unit scale.
    pub const IDENTITY: Trs = Trs {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// The matrix `translation * rotation * scale`.
    pub fn to_mat4(&self) -> glam::Mat4 {
        glam::Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

impl Default for Trs {
    fn default() -> Self {
        Trs::IDENTITY
    }
}

/// One joint of a [`SkinData`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JointDesc {
    /// The joint's name: the glTF node name that clip targets use.
    pub name: String,
    /// The parent joint, always an earlier index; `None` for a root joint.
    pub parent: Option<u16>,
    /// The rest pose, relative to the parent joint (or to the skin's space for a
    /// root joint) after `parent_offset`.
    pub rest: Trs,
    /// The transform of the glTF nodes between the parent joint and this one
    /// that are not joints (for a root joint: of every ancestor node up to the
    /// scene root). `None` means identity, the usual case.
    #[serde(default)]
    pub parent_offset: Option<glam::Mat4>,
}

/// A skin: a skeleton of joints with their inverse bind matrices (ADR-0039).
///
/// A joint's model matrix is `model(parent) * parent_offset * rest` (with an
/// animated pose in place of `rest`), and its palette matrix is
/// `model * inverse_bind`. The skin's space is the glTF scene's root space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkinData {
    /// The glTF skin name, or `skin_<index>`.
    pub name: String,
    /// The joints, every parent before its children; at most 65,535.
    pub joints: Vec<JointDesc>,
    /// One inverse bind matrix per joint.
    pub inverse_bind: Vec<glam::Mat4>,
    /// Per joint: the bounds, in the mesh's bind space, of the vertices the joint
    /// influences, or `None` when it influences none.
    pub joint_bounds: Vec<Option<crate::mesh::Aabb>>,
}

impl SkinData {
    /// True when the joint, inverse bind and bounds lists have one entry per
    /// joint, every parent comes before its child and there are at most 65,535
    /// joints.
    pub fn is_well_formed(&self) -> bool {
        let n = self.joints.len();
        n <= u16::MAX as usize
            && self.inverse_bind.len() == n
            && self.joint_bounds.len() == n
            && self
                .joints
                .iter()
                .enumerate()
                .all(|(i, j)| j.parent.is_none_or(|p| (p as usize) < i))
    }

    /// The index of the joint called `name`.
    pub fn joint(&self, name: &str) -> Option<usize> {
        self.joints.iter().position(|j| j.name == name)
    }
}

/// An animation graph: parameters, a blend tree of nodes, layered state
/// machines and joint masks, saved as `*.animgraph.json` (ADR-0039).
///
/// ```json
/// { "format": "real-engine/animgraph", "version": 1,
///   "params": [{ "id": "speed", "type": "float", "default": 0.0 }],
///   "nodes": [{ "id": "walk", "kind": "clip", "clip": "assets/hero.glb#walk" }],
///   "layers": [{ "id": "base", "entry": "move",
///                "states": [{ "id": "move", "node": "walk" }] }] }
/// ```
///
/// Every element has an `id` unique within its kind, which `animgraph.edit`
/// addresses. Lists keep file order and unknown keys are errors. `re_anim`
/// validates the references between elements when the graph binds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AnimGraph {
    /// Always [`AnimGraph::FORMAT`].
    pub format: String,
    /// The file format version, [`AnimGraph::VERSION`].
    pub version: u32,
    /// The parameters scripts and transitions read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<GraphParam>,
    /// The blend tree's nodes. States and other nodes refer to them by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<GraphNode>,
    /// The layers, bottom first. The first layer gives the base pose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<GraphLayer>,
    /// Joint masks that layers refer to by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<JointMask>,
}

impl AnimGraph {
    /// The value of the `format` key.
    pub const FORMAT: &'static str = "real-engine/animgraph";
    /// The current file format version.
    pub const VERSION: u32 = 1;

    /// An empty graph with the current format and version.
    pub fn new() -> AnimGraph {
        AnimGraph {
            format: Self::FORMAT.to_string(),
            version: Self::VERSION,
            params: Vec::new(),
            nodes: Vec::new(),
            layers: Vec::new(),
            masks: Vec::new(),
        }
    }

    /// The parameter `id`.
    pub fn param(&self, id: &str) -> Option<&GraphParam> {
        self.params.iter().find(|p| p.id == id)
    }

    /// The node `id`.
    pub fn node(&self, id: &str) -> Option<&GraphNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The layer `id`.
    pub fn layer(&self, id: &str) -> Option<&GraphLayer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// The mask `id`.
    pub fn mask(&self, id: &str) -> Option<&JointMask> {
        self.masks.iter().find(|m| m.id == id)
    }
}

impl Default for AnimGraph {
    fn default() -> Self {
        AnimGraph::new()
    }
}

/// A graph parameter with its default value. In files it is one object,
/// `{"id": "speed", "type": "float", "default": 0}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "ParamRepr", into = "ParamRepr")]
pub struct GraphParam {
    /// Unique among the parameters; scripts use it (`anim.set(e, "speed", 3)`).
    pub id: String,
    /// The type and default.
    pub value: ParamValue,
}

/// A parameter's type and value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamValue {
    /// A number, compared by transitions and read by blend spaces.
    Float {
        /// The value at start.
        default: f32,
    },
    /// A flag.
    Bool {
        /// The value at start.
        default: bool,
    },
    /// Set by `anim.trigger` and cleared when a transition consumes it.
    Trigger,
}

// `#[serde(flatten)]` cannot be combined with `deny_unknown_fields`, so the
// file form of a parameter and of a node is a tagged enum carrying the id.

/// A graph parameter as written in files.
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ParamRepr {
    /// A number, compared by transitions and read by blend spaces.
    Float {
        /// Unique among the parameters.
        id: String,
        /// The value at start.
        #[serde(default)]
        default: f32,
    },
    /// A flag.
    Bool {
        /// Unique among the parameters.
        id: String,
        /// The value at start.
        #[serde(default)]
        default: bool,
    },
    /// Set by `anim.trigger` and cleared when a transition consumes it.
    Trigger {
        /// Unique among the parameters.
        id: String,
    },
}

impl From<ParamRepr> for GraphParam {
    fn from(r: ParamRepr) -> GraphParam {
        let (id, value) = match r {
            ParamRepr::Float { id, default } => (id, ParamValue::Float { default }),
            ParamRepr::Bool { id, default } => (id, ParamValue::Bool { default }),
            ParamRepr::Trigger { id } => (id, ParamValue::Trigger),
        };
        GraphParam { id, value }
    }
}

impl From<GraphParam> for ParamRepr {
    fn from(p: GraphParam) -> ParamRepr {
        let id = p.id;
        match p.value {
            ParamValue::Float { default } => ParamRepr::Float { id, default },
            ParamValue::Bool { default } => ParamRepr::Bool { id, default },
            ParamValue::Trigger => ParamRepr::Trigger { id },
        }
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for GraphParam {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "GraphParam".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        ParamRepr::json_schema(generator)
    }
}

impl ParamValue {
    /// The type's name as written in files: `float`, `bool` or `trigger`.
    pub fn type_name(&self) -> &'static str {
        match self {
            ParamValue::Float { .. } => "float",
            ParamValue::Bool { .. } => "bool",
            ParamValue::Trigger => "trigger",
        }
    }
}

/// One node of the blend tree. In files it is one object,
/// `{"id": "walk", "kind": "clip", "clip": "models/hero.glb#Walk"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "NodeRepr", into = "NodeRepr")]
pub struct GraphNode {
    /// Unique among the nodes.
    pub id: String,
    /// What the node computes.
    pub kind: NodeKind,
}

fn one() -> f32 {
    1.0
}

fn yes() -> bool {
    true
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// The kinds of blend tree node.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    /// Plays one clip.
    Clip {
        /// `<model path>#<clip name>`.
        clip: String,
        /// Playback speed; the node lasts `duration / speed`.
        speed: f32,
        /// Wraps at the end (otherwise holds the last frame).
        looping: bool,
        /// Takes the root joint's horizontal motion out of the pose and moves
        /// the entity with it.
        root_motion: bool,
    },
    /// Blends children placed along one float parameter, piecewise linearly
    /// and clamped at the ends.
    Blend1d {
        /// The float parameter.
        param: String,
        /// The children, sorted by threshold.
        children: Vec<Blend1dChild>,
        /// Keeps the children in phase by normalised time.
        sync: bool,
    },
    /// Blends children placed at points over two float parameters, by
    /// gradient-band (freeform cartesian) interpolation.
    Blend2d {
        /// The parameter on the first axis.
        x: String,
        /// The parameter on the second axis.
        y: String,
        /// The children at their points.
        children: Vec<Blend2dChild>,
        /// Keeps the children in phase by normalised time.
        sync: bool,
    },
    /// Adds an additive child's motion, relative to its first frame, onto a
    /// base node.
    Additive {
        /// The base node.
        base: String,
        /// The node whose motion is added.
        additive: String,
        /// The weight of the added motion, when `weight_param` is not set.
        weight: f32,
        /// A float parameter giving the weight.
        weight_param: Option<String>,
    },
}

/// A blend tree node as written in files.
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum NodeRepr {
    /// Plays one clip.
    Clip {
        /// Unique among the nodes.
        id: String,
        /// `<model path>#<clip name>`.
        clip: String,
        /// Playback speed; the node lasts `duration / speed`.
        #[serde(default = "one", skip_serializing_if = "is_one")]
        speed: f32,
        /// Wraps at the end (otherwise holds the last frame).
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        looping: bool,
        /// Takes the root joint's horizontal motion out of the pose and moves
        /// the entity with it.
        #[serde(default, skip_serializing_if = "is_false")]
        root_motion: bool,
    },
    /// Blends children placed along one float parameter, piecewise linearly
    /// and clamped at the ends.
    Blend1d {
        /// Unique among the nodes.
        id: String,
        /// The float parameter.
        param: String,
        /// The children, sorted by threshold.
        children: Vec<Blend1dChild>,
        /// Keeps the children in phase by normalised time.
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        sync: bool,
    },
    /// Blends children placed at points over two float parameters, by
    /// gradient-band (freeform cartesian) interpolation.
    Blend2d {
        /// Unique among the nodes.
        id: String,
        /// The parameter on the first axis.
        x: String,
        /// The parameter on the second axis.
        y: String,
        /// The children at their points.
        children: Vec<Blend2dChild>,
        /// Keeps the children in phase by normalised time.
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        sync: bool,
    },
    /// Adds an additive child's motion, relative to its first frame, onto a
    /// base node.
    Additive {
        /// Unique among the nodes.
        id: String,
        /// The base node.
        base: String,
        /// The node whose motion is added.
        additive: String,
        /// The weight of the added motion, when `weight_param` is not set.
        #[serde(default = "one", skip_serializing_if = "is_one")]
        weight: f32,
        /// A float parameter giving the weight.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight_param: Option<String>,
    },
}

impl From<NodeRepr> for GraphNode {
    fn from(r: NodeRepr) -> GraphNode {
        let (id, kind) = match r {
            NodeRepr::Clip {
                id,
                clip,
                speed,
                looping,
                root_motion,
            } => (
                id,
                NodeKind::Clip {
                    clip,
                    speed,
                    looping,
                    root_motion,
                },
            ),
            NodeRepr::Blend1d {
                id,
                param,
                children,
                sync,
            } => (
                id,
                NodeKind::Blend1d {
                    param,
                    children,
                    sync,
                },
            ),
            NodeRepr::Blend2d {
                id,
                x,
                y,
                children,
                sync,
            } => (
                id,
                NodeKind::Blend2d {
                    x,
                    y,
                    children,
                    sync,
                },
            ),
            NodeRepr::Additive {
                id,
                base,
                additive,
                weight,
                weight_param,
            } => (
                id,
                NodeKind::Additive {
                    base,
                    additive,
                    weight,
                    weight_param,
                },
            ),
        };
        GraphNode { id, kind }
    }
}

impl From<GraphNode> for NodeRepr {
    fn from(n: GraphNode) -> NodeRepr {
        let id = n.id;
        match n.kind {
            NodeKind::Clip {
                clip,
                speed,
                looping,
                root_motion,
            } => NodeRepr::Clip {
                id,
                clip,
                speed,
                looping,
                root_motion,
            },
            NodeKind::Blend1d {
                param,
                children,
                sync,
            } => NodeRepr::Blend1d {
                id,
                param,
                children,
                sync,
            },
            NodeKind::Blend2d {
                x,
                y,
                children,
                sync,
            } => NodeRepr::Blend2d {
                id,
                x,
                y,
                children,
                sync,
            },
            NodeKind::Additive {
                base,
                additive,
                weight,
                weight_param,
            } => NodeRepr::Additive {
                id,
                base,
                additive,
                weight,
                weight_param,
            },
        }
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for GraphNode {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "GraphNode".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        NodeRepr::json_schema(generator)
    }
}

/// A child of a [`NodeKind::Blend1d`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Blend1dChild {
    /// The child node.
    pub node: String,
    /// The parameter value at which the child has full weight.
    pub threshold: f32,
}

/// A child of a [`NodeKind::Blend2d`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Blend2dChild {
    /// The child node.
    pub node: String,
    /// The `[x, y]` parameter values at which the child has full weight.
    pub point: [f32; 2],
}

/// One layer: a state machine whose pose is laid over the layers below it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GraphLayer {
    /// Unique among the layers; `anim.state(e, layer)` names it.
    pub id: String,
    /// How the pose combines with the layers below. The first layer's blend is
    /// ignored.
    #[serde(default, skip_serializing_if = "LayerBlend::is_override")]
    pub blend: LayerBlend,
    /// How much of the layer applies, from 0 to 1.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub weight: f32,
    /// A mask limiting the joints the layer affects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<String>,
    /// For an additive layer: a clip (`<model>#<clip>`) whose first frame the
    /// motion is measured from. Without it, each state's node at time 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The state entered at start.
    pub entry: String,
    /// The states.
    pub states: Vec<GraphState>,
    /// The transitions, checked in order; the first that passes wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<Transition>,
}

/// How a layer combines with the layers below it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LayerBlend {
    /// Blends toward the layer's pose by its weight.
    #[default]
    Override,
    /// Adds the layer's motion, relative to its reference, scaled by its weight.
    Additive,
}

impl LayerBlend {
    fn is_override(&self) -> bool {
        *self == LayerBlend::Override
    }
}

/// A state of a layer's state machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GraphState {
    /// Unique within the layer. `any` is reserved for transitions.
    pub id: String,
    /// The node the state plays.
    pub node: String,
}

/// A transition between two states of a layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Transition {
    /// Unique within the layer.
    pub id: String,
    /// The state it leaves, or [`Transition::ANY`] for every other state.
    pub from: String,
    /// The state it enters.
    pub to: String,
    /// The crossfade time in seconds.
    #[serde(default)]
    pub duration: f32,
    /// The normalised time of the `from` state (1 is one pass through its
    /// node) the transition waits for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_time: Option<f32>,
    /// All of them must hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Whether another transition may interrupt its crossfade.
    #[serde(default, skip_serializing_if = "is_false")]
    pub interruptible: bool,
}

impl Transition {
    /// The `from` value that matches every state but the target.
    pub const ANY: &'static str = "any";
}

/// A test on a parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "test", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    /// Compares a float parameter with a value.
    Compare {
        /// The float parameter.
        param: String,
        /// The comparison.
        op: CompareOp,
        /// The value compared with.
        value: f32,
    },
    /// Tests a bool parameter.
    Bool {
        /// The bool parameter.
        param: String,
        /// The value it must have.
        value: bool,
    },
    /// Passes when the trigger is set, and clears it when the transition fires.
    Trigger {
        /// The trigger parameter.
        param: String,
    },
}

impl Condition {
    /// The parameter the condition reads.
    pub fn param(&self) -> &str {
        match self {
            Condition::Compare { param, .. }
            | Condition::Bool { param, .. }
            | Condition::Trigger { param } => param,
        }
    }
}

/// A comparison of a float parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum CompareOp {
    /// Less than.
    #[serde(rename = "<")]
    Lt,
    /// Less than or equal.
    #[serde(rename = "<=")]
    Le,
    /// Greater than.
    #[serde(rename = ">")]
    Gt,
    /// Greater than or equal.
    #[serde(rename = ">=")]
    Ge,
}

impl CompareOp {
    /// `a op b`.
    pub fn test(self, a: f32, b: f32) -> bool {
        match self {
            CompareOp::Lt => a < b,
            CompareOp::Le => a <= b,
            CompareOp::Gt => a > b,
            CompareOp::Ge => a >= b,
        }
    }
}

/// Per-joint weights for a layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct JointMask {
    /// Unique among the masks.
    pub id: String,
    /// The joints, by name. A later entry overrides an earlier one for the
    /// joints both cover. Joints no entry covers have weight 0.
    pub joints: Vec<MaskJoint>,
}

/// One entry of a [`JointMask`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MaskJoint {
    /// The joint's name. A name the skeleton lacks is a warning when the graph
    /// binds, so one mask fits several skeletons.
    pub joint: String,
    /// The weight, from 0 to 1.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub weight: f32,
    /// Also covers the joint's descendants.
    #[serde(default, skip_serializing_if = "is_false")]
    pub subtree: bool,
}
