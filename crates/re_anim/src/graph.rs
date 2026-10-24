//! Animation graphs (ADR-0039): parameters, a blend tree, layered state
//! machines and joint masks, evaluated onto a skin's [`Pose`] each fixed step.
//!
//! [`validate`] checks an [`AnimGraph`] on its own (ids, references, types
//! and ranges). [`BoundGraph::bind`] resolves a valid graph against a skin
//! (clips by path, masks by joint name), once; a [`GraphPlayer`] holds one
//! character's parameters and state machines and steps them:
//!
//! ```
//! use std::sync::Arc;
//! use re_anim::graph::{BoundGraph, GraphPlayer, Param};
//! use re_anim::skin::Pose;
//! # use re_types::anim::{AnimGraph, SkinData, JointDesc, Trs, ClipData};
//! # let skin = SkinData { name: "s".into(), joints: vec![JointDesc { name: "root".into(),
//! #     parent: None, rest: Trs::IDENTITY, parent_offset: None }],
//! #     inverse_bind: vec![glam::Mat4::IDENTITY], joint_bounds: vec![None] };
//! let graph: AnimGraph = serde_json::from_str(r#"{
//!   "format": "real-engine/animgraph", "version": 1,
//!   "params": [{ "id": "speed", "type": "float" }],
//!   "nodes": [{ "id": "walk", "kind": "clip", "clip": "hero.glb#walk" }],
//!   "layers": [{ "id": "base", "entry": "move", "states": [{ "id": "move", "node": "walk" }] }]
//! }"#).unwrap();
//! let (bound, warnings) = BoundGraph::bind(&graph, &skin, &mut |_path| {
//!     Ok(Arc::new(re_anim::Sampler::new(ClipData::default())?))
//! })?;
//! let mut player = GraphPlayer::new(Arc::new(bound));
//! player.set("speed", Param::Float(3.2))?;
//! let mut pose = Pose::default();
//! let motion = player.step(1.0 / 60.0, &mut pose);
//! assert_eq!(player.state(None), Some("move"));
//! # Ok::<(), re_core::Diagnostic>(())
//! ```
//!
//! Evaluation is deterministic: the same graph, parameters and steps give the
//! same bits. Blending uses no platform maths function (no trigonometry):
//! translation and scale lerp, rotation nlerps along the shorter arc.
//!
//! ## Time
//!
//! Each active state has a time in seconds since it was entered and a
//! normalised time (1 is one pass through its node). A blend space with
//! `sync` plays its children at the same normalised time, its duration the
//! weighted mean of theirs; without `sync`, each child plays by seconds at its
//! own speed. A node of zero length counts one pass per step.
//!
//! ## Transitions
//!
//! Each step, after the times advance, a layer checks its transitions in file
//! order and takes the first whose conditions all hold. `from: "any"` matches
//! every state but the target. With an `exit_time` below 1 the transition
//! waits for the state's normalised time to pass `exit_time + k` (once per
//! pass); at 1 or more it holds from that time on. Triggers it tests are
//! consumed when it fires. While a crossfade runs, transitions are checked
//! only if the running one is `interruptible`; an interruption freezes the
//! blended pose and fades from it.
//!
//! ## Root motion
//!
//! A clip node with `root_motion` takes the root joint's (joint 0's)
//! horizontal translation and yaw out of the pose, keeping its first frame's,
//! and reports the motion since the previous step, wrapping correctly when the
//! clip loops. Motions blend with the same weights as poses; additive nodes
//! and layers add none. Vertical motion stays in the pose.

use std::sync::Arc;

use glam::{Quat, Vec2, Vec3};
use re_core::Diagnostic;
use re_types::anim::{
    AnimGraph, CompareOp, Condition, LayerBlend, NodeKind, ParamValue, SkinData, Trs,
};

use crate::codes;
use crate::sampler::Sampler;
use crate::skin::{BoundClip, Pose, blend, check_skin, nlerp};

/// A parameter value, as scripts and commands read and write it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Param {
    /// A float parameter's value.
    Float(f32),
    /// A bool parameter's value.
    Bool(bool),
    /// Whether a trigger parameter is set.
    Trigger(bool),
}

impl Param {
    /// The type's name as written in graph files.
    pub fn type_name(&self) -> &'static str {
        match self {
            Param::Float(_) => "float",
            Param::Bool(_) => "bool",
            Param::Trigger(_) => "trigger",
        }
    }
}

/// The root joint's motion over a step, in the skin's space: a horizontal
/// translation and a rotation about +Y (yaw). Apply the translation first,
/// in the entity's frame, then the rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RootMotion {
    /// Horizontal (`y` is 0).
    pub translation: Vec3,
    /// About +Y.
    pub rotation: Quat,
}

impl RootMotion {
    /// No motion.
    pub const IDENTITY: RootMotion = RootMotion {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
    };

    /// This motion followed by `next` (measured in the frame this one ends in).
    pub fn then(self, next: RootMotion) -> RootMotion {
        RootMotion {
            translation: self.translation + self.rotation * next.translation,
            rotation: (self.rotation * next.rotation).normalize(),
        }
    }

    /// `self` moved `w` of the way toward `other`.
    pub fn blend(self, other: RootMotion, w: f32) -> RootMotion {
        RootMotion {
            translation: self.translation.lerp(other.translation, w),
            rotation: nlerp(self.rotation, other.rotation, w),
        }
    }

    /// True when there is no motion.
    pub fn is_identity(&self) -> bool {
        *self == RootMotion::IDENTITY
    }
}

impl Default for RootMotion {
    fn default() -> Self {
        RootMotion::IDENTITY
    }
}

// --- Validation -----------------------------------------------------------

/// Checks `graph` on its own: format and version, unique ids, references
/// between elements, parameter types, value ranges and blend-tree cycles.
/// Returns every problem found, in file order; an empty list means valid.
///
/// Problems are [`codes::GRAPH_INVALID`] errors (and [`codes::GRAPH_CYCLE`]
/// for a cycle), each naming the element by its path in the file, such as
/// `layers[0].transitions[2].to`.
pub fn validate(graph: &AnimGraph) -> Vec<Diagnostic> {
    let mut c = Checker::default();
    if graph.format != AnimGraph::FORMAT {
        c.bad(
            "format",
            format!("is `{}`, expected `{}`", graph.format, AnimGraph::FORMAT),
        );
    }
    if graph.version != AnimGraph::VERSION {
        c.bad(
            "version",
            format!(
                "is {}, this engine reads version {}",
                graph.version,
                AnimGraph::VERSION
            ),
        );
    }
    unique(&mut c, "params", graph.params.iter().map(|p| p.id.as_str()));
    unique(&mut c, "nodes", graph.nodes.iter().map(|n| n.id.as_str()));
    unique(&mut c, "layers", graph.layers.iter().map(|l| l.id.as_str()));
    unique(&mut c, "masks", graph.masks.iter().map(|m| m.id.as_str()));

    for (i, p) in graph.params.iter().enumerate() {
        if let ParamValue::Float { default } = p.value
            && !default.is_finite()
        {
            c.bad(format!("params[{i}].default"), "is not finite");
        }
    }
    let param = |c: &mut Checker, path: String, id: &str, want: &str| match graph.param(id) {
        None => c.bad(
            path,
            format!("names parameter `{id}`, which the graph lacks"),
        ),
        Some(p) if p.value.type_name() != want => c.bad(
            path,
            format!(
                "names parameter `{id}`, a {}, where a {want} is needed",
                p.value.type_name()
            ),
        ),
        Some(_) => {}
    };
    let node = |c: &mut Checker, path: String, id: &str| {
        if graph.node(id).is_none() {
            c.bad(path, format!("names node `{id}`, which the graph lacks"));
        }
    };

    for (i, n) in graph.nodes.iter().enumerate() {
        let at = format!("nodes[{i}]");
        match &n.kind {
            NodeKind::Clip { clip, speed, .. } => {
                if clip.is_empty() {
                    c.bad(format!("{at}.clip"), "is empty");
                }
                if !speed.is_finite() {
                    c.bad(format!("{at}.speed"), "is not finite");
                } else if *speed == 0.0 {
                    c.bad(
                        format!("{at}.speed"),
                        "is 0 (pause a graph with `Animator.playing` or `Animator.speed`)",
                    );
                }
            }
            NodeKind::Blend1d {
                param: p, children, ..
            } => {
                param(&mut c, format!("{at}.param"), p, "float");
                if children.is_empty() {
                    c.bad(format!("{at}.children"), "is empty");
                }
                for (k, child) in children.iter().enumerate() {
                    node(&mut c, format!("{at}.children[{k}].node"), &child.node);
                    if !child.threshold.is_finite() {
                        c.bad(format!("{at}.children[{k}].threshold"), "is not finite");
                    } else if k > 0 && child.threshold < children[k - 1].threshold {
                        c.bad(
                            format!("{at}.children[{k}].threshold"),
                            "is below the one before it (children are sorted by threshold)",
                        );
                    }
                }
            }
            NodeKind::Blend2d { x, y, children, .. } => {
                param(&mut c, format!("{at}.x"), x, "float");
                param(&mut c, format!("{at}.y"), y, "float");
                if children.is_empty() {
                    c.bad(format!("{at}.children"), "is empty");
                }
                for (k, child) in children.iter().enumerate() {
                    node(&mut c, format!("{at}.children[{k}].node"), &child.node);
                    if !child.point.iter().all(|v| v.is_finite()) {
                        c.bad(format!("{at}.children[{k}].point"), "is not finite");
                    } else if children[..k].iter().any(|o| o.point == child.point) {
                        c.bad(
                            format!("{at}.children[{k}].point"),
                            "repeats an earlier child's point",
                        );
                    }
                }
            }
            NodeKind::Additive {
                base,
                additive,
                weight,
                weight_param,
            } => {
                node(&mut c, format!("{at}.base"), base);
                node(&mut c, format!("{at}.additive"), additive);
                if !weight.is_finite() {
                    c.bad(format!("{at}.weight"), "is not finite");
                }
                if let Some(p) = weight_param {
                    param(&mut c, format!("{at}.weight_param"), p, "float");
                }
            }
        }
    }

    for (i, l) in graph.layers.iter().enumerate() {
        let at = format!("layers[{i}]");
        if !(0.0..=1.0).contains(&l.weight) {
            c.bad(format!("{at}.weight"), "must be from 0 to 1");
        }
        if let Some(m) = &l.mask
            && graph.mask(m).is_none()
        {
            c.bad(
                format!("{at}.mask"),
                format!("names mask `{m}`, which the graph lacks"),
            );
        }
        if l.reference.is_some() && l.blend != LayerBlend::Additive {
            c.bad(
                format!("{at}.reference"),
                "is only used by an additive layer",
            );
        }
        if l.reference.as_ref().is_some_and(|r| r.is_empty()) {
            c.bad(format!("{at}.reference"), "is empty");
        }
        if l.states.is_empty() {
            c.bad(format!("{at}.states"), "is empty");
        }
        unique(
            &mut c,
            &format!("{at}.states"),
            l.states.iter().map(|s| s.id.as_str()),
        );
        unique(
            &mut c,
            &format!("{at}.transitions"),
            l.transitions.iter().map(|t| t.id.as_str()),
        );
        let state = |id: &str| l.states.iter().any(|s| s.id == id);
        if !state(&l.entry) {
            c.bad(
                format!("{at}.entry"),
                format!("names state `{}`, which the layer lacks", l.entry),
            );
        }
        for (k, s) in l.states.iter().enumerate() {
            if s.id == re_types::anim::Transition::ANY {
                c.bad(
                    format!("{at}.states[{k}].id"),
                    "`any` is reserved for transitions",
                );
            }
            node(&mut c, format!("{at}.states[{k}].node"), &s.node);
        }
        for (k, t) in l.transitions.iter().enumerate() {
            let tp = format!("{at}.transitions[{k}]");
            if t.from != re_types::anim::Transition::ANY && !state(&t.from) {
                c.bad(
                    format!("{tp}.from"),
                    format!("names state `{}`, which the layer lacks", t.from),
                );
            }
            if !state(&t.to) {
                c.bad(
                    format!("{tp}.to"),
                    format!("names state `{}`, which the layer lacks", t.to),
                );
            }
            if !(t.duration.is_finite() && t.duration >= 0.0) {
                c.bad(format!("{tp}.duration"), "must be 0 or more");
            }
            if t.exit_time.is_some_and(|e| !(e.is_finite() && e >= 0.0)) {
                c.bad(format!("{tp}.exit_time"), "must be 0 or more");
            }
            for (q, cond) in t.conditions.iter().enumerate() {
                let cp = format!("{tp}.conditions[{q}].param");
                match cond {
                    Condition::Compare {
                        param: p, value, ..
                    } => {
                        param(&mut c, cp, p, "float");
                        if !value.is_finite() {
                            c.bad(format!("{tp}.conditions[{q}].value"), "is not finite");
                        }
                    }
                    Condition::Bool { param: p, .. } => param(&mut c, cp, p, "bool"),
                    Condition::Trigger { param: p } => param(&mut c, cp, p, "trigger"),
                }
            }
        }
    }

    for (i, m) in graph.masks.iter().enumerate() {
        for (k, j) in m.joints.iter().enumerate() {
            if !(0.0..=1.0).contains(&j.weight) {
                c.bad(
                    format!("masks[{i}].joints[{k}].weight"),
                    "must be from 0 to 1",
                );
            }
        }
    }

    if let Some(cycle) = find_cycle(graph) {
        c.errors.push(
            Diagnostic::error(
                codes::GRAPH_CYCLE,
                format!(
                    "the blend tree has a cycle: {}",
                    cycle
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(" -> ")
                ),
            )
            .with_hint("a node cannot use itself, directly or through other nodes"),
        );
    }
    c.errors
}

#[derive(Default)]
struct Checker {
    errors: Vec<Diagnostic>,
}

impl Checker {
    fn bad(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(
            Diagnostic::error(
                codes::GRAPH_INVALID,
                format!("animation graph: `{}` {}", path.into(), message.into()),
            )
            .with_hint("fix the element named, or run `real explain RE7607`"),
        );
    }
}

fn unique<'a>(c: &mut Checker, list: &str, ids: impl Iterator<Item = &'a str>) {
    let ids: Vec<&str> = ids.collect();
    for (i, id) in ids.iter().enumerate() {
        if id.is_empty() {
            c.bad(format!("{list}[{i}].id"), "is empty");
        } else if ids[..i].contains(id) {
            c.bad(
                format!("{list}[{i}].id"),
                format!("`{id}` is already used in `{list}`"),
            );
        }
    }
}

fn children_of(kind: &NodeKind) -> Vec<&str> {
    match kind {
        NodeKind::Clip { .. } => Vec::new(),
        NodeKind::Blend1d { children, .. } => children.iter().map(|c| c.node.as_str()).collect(),
        NodeKind::Blend2d { children, .. } => children.iter().map(|c| c.node.as_str()).collect(),
        NodeKind::Additive { base, additive, .. } => vec![base.as_str(), additive.as_str()],
    }
}

/// The first cycle in file order, as node ids from its first node back to it.
fn find_cycle(graph: &AnimGraph) -> Option<Vec<String>> {
    // 0 unvisited, 1 on the stack, 2 done.
    let mut mark = vec![0u8; graph.nodes.len()];
    let index = |id: &str| graph.nodes.iter().position(|n| n.id == id);
    fn visit(
        graph: &AnimGraph,
        i: usize,
        mark: &mut [u8],
        stack: &mut Vec<usize>,
        index: &dyn Fn(&str) -> Option<usize>,
    ) -> Option<Vec<String>> {
        mark[i] = 1;
        stack.push(i);
        for child in children_of(&graph.nodes[i].kind) {
            let Some(j) = index(child) else { continue };
            match mark[j] {
                1 => {
                    let from = stack.iter().position(|&s| s == j).expect("on the stack");
                    let mut cycle: Vec<String> = stack[from..]
                        .iter()
                        .map(|&s| graph.nodes[s].id.clone())
                        .collect();
                    cycle.push(graph.nodes[j].id.clone());
                    return Some(cycle);
                }
                0 => {
                    if let Some(c) = visit(graph, j, mark, stack, index) {
                        return Some(c);
                    }
                }
                _ => {}
            }
        }
        stack.pop();
        mark[i] = 2;
        None
    }
    let mut stack = Vec::new();
    for i in 0..graph.nodes.len() {
        if mark[i] == 0
            && let Some(c) = visit(graph, i, &mut mark, &mut stack, &index)
        {
            return Some(c);
        }
    }
    None
}

// --- Binding --------------------------------------------------------------

#[derive(Clone, Debug)]
enum Node {
    Clip {
        clip: usize,
        speed: f32,
        looping: bool,
        root_motion: bool,
    },
    Blend1d {
        param: usize,
        children: Vec<(usize, f32)>,
        sync: bool,
    },
    Blend2d {
        x: usize,
        y: usize,
        children: Vec<(usize, Vec2)>,
        sync: bool,
    },
    Additive {
        base: usize,
        additive: usize,
        weight: f32,
        weight_param: Option<usize>,
    },
}

#[derive(Clone, Debug)]
enum Cond {
    Compare {
        param: usize,
        op: CompareOp,
        value: f32,
    },
    Bool {
        param: usize,
        value: bool,
    },
    Trigger {
        param: usize,
    },
}

#[derive(Clone, Debug)]
struct Edge {
    /// `None` for `any`.
    from: Option<usize>,
    to: usize,
    duration: f32,
    exit_time: Option<f32>,
    conditions: Vec<Cond>,
    interruptible: bool,
}

#[derive(Clone, Debug)]
struct Layer {
    id: String,
    additive: bool,
    weight: f32,
    /// One weight per joint.
    mask: Option<Vec<f32>>,
    /// A clip whose first frame additive motion is measured from.
    reference: Option<usize>,
    entry: usize,
    /// (state id, node).
    states: Vec<(String, usize)>,
    edges: Vec<Edge>,
}

/// An [`AnimGraph`] resolved against one skin: clips bound to its joints and
/// masks turned into per-joint weights. Immutable; share it between the
/// characters that use the same graph and skin.
#[derive(Clone, Debug)]
pub struct BoundGraph {
    params: Vec<(String, ParamValue)>,
    nodes: Vec<Node>,
    clips: Vec<BoundClip>,
    layers: Vec<Layer>,
    rest: Pose,
}

impl BoundGraph {
    /// Validates `graph` and binds it to `skin`, loading each distinct clip
    /// path once through `clip`. Returns the bound graph and its warnings
    /// ([`codes::GRAPH_MASK_JOINT_MISSING`] for each mask joint the skin
    /// lacks).
    ///
    /// # Errors
    ///
    /// The first problem [`validate`] finds (the others attached as related),
    /// [`codes::GRAPH_CLIP_MISSING`] for a clip that cannot be loaded, and
    /// [`codes::SKIN_MALFORMED`] for a skin that fails [`check_skin`].
    pub fn bind(
        graph: &AnimGraph,
        skin: &SkinData,
        clip: &mut dyn FnMut(&str) -> Result<Arc<Sampler>, Diagnostic>,
    ) -> Result<(BoundGraph, Vec<Diagnostic>), Diagnostic> {
        let mut problems = validate(graph).into_iter();
        if let Some(first) = problems.next() {
            return Err(problems.fold(first, Diagnostic::with_related));
        }
        check_skin(skin)?;
        let param = |id: &str| {
            graph
                .params
                .iter()
                .position(|p| p.id == id)
                .expect("validated")
        };
        let node = |id: &str| {
            graph
                .nodes
                .iter()
                .position(|n| n.id == id)
                .expect("validated")
        };

        let mut paths: Vec<String> = Vec::new();
        let mut clips = Vec::new();
        let mut load = |path: &str, owner: &str| -> Result<usize, Diagnostic> {
            if let Some(i) = paths.iter().position(|p| p == path) {
                return Ok(i);
            }
            let sampler = clip(path).map_err(|d| {
                Diagnostic::error(
                    codes::GRAPH_CLIP_MISSING,
                    format!("animation graph: {owner} plays clip `{path}`, which cannot be loaded"),
                )
                .with_hint("check the model path and clip name, or run `real explain RE7608`")
                .with_related(d)
            })?;
            clips.push(BoundClip::bind_shared(sampler, skin)?);
            paths.push(path.to_string());
            Ok(paths.len() - 1)
        };

        let mut nodes = Vec::with_capacity(graph.nodes.len());
        for n in &graph.nodes {
            nodes.push(match &n.kind {
                NodeKind::Clip {
                    clip,
                    speed,
                    looping,
                    root_motion,
                } => Node::Clip {
                    clip: load(clip, &format!("node `{}`", n.id))?,
                    speed: *speed,
                    looping: *looping,
                    root_motion: *root_motion,
                },
                NodeKind::Blend1d {
                    param: p,
                    children,
                    sync,
                } => Node::Blend1d {
                    param: param(p),
                    children: children
                        .iter()
                        .map(|c| (node(&c.node), c.threshold))
                        .collect(),
                    sync: *sync,
                },
                NodeKind::Blend2d {
                    x,
                    y,
                    children,
                    sync,
                } => Node::Blend2d {
                    x: param(x),
                    y: param(y),
                    children: children
                        .iter()
                        .map(|c| (node(&c.node), Vec2::from(c.point)))
                        .collect(),
                    sync: *sync,
                },
                NodeKind::Additive {
                    base,
                    additive,
                    weight,
                    weight_param,
                } => Node::Additive {
                    base: node(base),
                    additive: node(additive),
                    weight: *weight,
                    weight_param: weight_param.as_deref().map(param),
                },
            });
        }

        let mut warnings = Vec::new();
        let mut layers = Vec::with_capacity(graph.layers.len());
        for (i, l) in graph.layers.iter().enumerate() {
            let state = |id: &str| l.states.iter().position(|s| s.id == id).expect("validated");
            let mask = l.mask.as_deref().map(|id| {
                let mask = graph.mask(id).expect("validated");
                let mut weights = vec![0.0f32; skin.joints.len()];
                for entry in &mask.joints {
                    let Some(j) = skin.joint(&entry.joint) else {
                        warnings.push(Diagnostic::warning(
                            codes::GRAPH_MASK_JOINT_MISSING,
                            format!(
                                "animation graph: mask `{id}` names joint `{}`, which skin `{}` lacks",
                                entry.joint, skin.name
                            ),
                        ));
                        continue;
                    };
                    let mut covered = vec![false; skin.joints.len()];
                    covered[j] = true;
                    if entry.subtree {
                        // Parents come before children (check_skin).
                        for k in j + 1..skin.joints.len() {
                            covered[k] = skin.joints[k].parent.is_some_and(|p| covered[p as usize]);
                        }
                    }
                    for (w, c) in weights.iter_mut().zip(covered) {
                        if c {
                            *w = entry.weight;
                        }
                    }
                }
                weights
            });
            let reference = match &l.reference {
                Some(r) => Some(load(r, &format!("layer `{}`", l.id))?),
                None => None,
            };
            layers.push(Layer {
                id: l.id.clone(),
                additive: i > 0 && l.blend == LayerBlend::Additive,
                weight: l.weight,
                mask,
                reference,
                entry: state(&l.entry),
                states: l
                    .states
                    .iter()
                    .map(|s| (s.id.clone(), node(&s.node)))
                    .collect(),
                edges: l
                    .transitions
                    .iter()
                    .map(|t| Edge {
                        from: (t.from != re_types::anim::Transition::ANY).then(|| state(&t.from)),
                        to: state(&t.to),
                        duration: t.duration,
                        exit_time: t.exit_time,
                        conditions: t
                            .conditions
                            .iter()
                            .map(|c| match c {
                                Condition::Compare {
                                    param: p,
                                    op,
                                    value,
                                } => Cond::Compare {
                                    param: param(p),
                                    op: *op,
                                    value: *value,
                                },
                                Condition::Bool { param: p, value } => Cond::Bool {
                                    param: param(p),
                                    value: *value,
                                },
                                Condition::Trigger { param: p } => {
                                    Cond::Trigger { param: param(p) }
                                }
                            })
                            .collect(),
                        interruptible: t.interruptible,
                    })
                    .collect(),
            });
        }

        Ok((
            BoundGraph {
                params: graph
                    .params
                    .iter()
                    .map(|p| (p.id.clone(), p.value))
                    .collect(),
                nodes,
                clips,
                layers,
                rest: Pose::rest(skin),
            },
            warnings,
        ))
    }

    /// The parameters, as `(id, type and default)`, in file order.
    pub fn params(&self) -> impl Iterator<Item = (&str, ParamValue)> {
        self.params.iter().map(|(id, v)| (id.as_str(), *v))
    }

    /// The layer ids, bottom first.
    pub fn layers(&self) -> impl Iterator<Item = &str> {
        self.layers.iter().map(|l| l.id.as_str())
    }

    /// The number of joints of the skin it was bound to.
    pub fn joints(&self) -> usize {
        self.rest.joints.len()
    }

    fn param_error(&self, id: &str, problem: String) -> Diagnostic {
        let list = if self.params.is_empty() {
            "none".to_string()
        } else {
            self.params
                .iter()
                .map(|(id, v)| format!("`{id}` ({})", v.type_name()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        Diagnostic::error(
            codes::GRAPH_PARAM_UNKNOWN,
            format!("animation graph parameter `{id}`: {problem}; the graph's parameters: {list}"),
        )
        .with_hint("run `real explain RE7609`")
    }

    /// Duration in seconds of one pass through `node`.
    fn duration(&self, node: usize, params: &[Param]) -> f32 {
        match &self.nodes[node] {
            Node::Clip { clip, speed, .. } => {
                let d = self.clips[*clip].duration();
                if *speed == 0.0 || d <= 0.0 {
                    0.0
                } else {
                    d / speed.abs()
                }
            }
            Node::Blend1d { .. } | Node::Blend2d { .. } => {
                let mut weights = Vec::new();
                self.weights(node, params, &mut weights);
                weights
                    .iter()
                    .map(|&(c, w)| w * self.duration(c, params))
                    .sum()
            }
            Node::Additive { base, .. } => self.duration(*base, params),
        }
    }

    /// The children of a blend node with non-zero weights, in child order;
    /// the weights sum to 1.
    fn weights(&self, node: usize, params: &[Param], out: &mut Vec<(usize, f32)>) {
        out.clear();
        match &self.nodes[node] {
            Node::Blend1d {
                param, children, ..
            } => {
                let p = float(params, *param);
                blend1d_weights(children, p, out);
            }
            Node::Blend2d { x, y, children, .. } => {
                let p = Vec2::new(float(params, *x), float(params, *y));
                blend2d_weights(children, p, out);
            }
            _ => {}
        }
    }
}

fn float(params: &[Param], i: usize) -> f32 {
    match params[i] {
        Param::Float(v) => v,
        _ => 0.0,
    }
}

/// Piecewise-linear weights over children sorted by threshold, clamped at the
/// ends. A NaN parameter counts as the first threshold.
pub(crate) fn blend1d_weights(children: &[(usize, f32)], p: f32, out: &mut Vec<(usize, f32)>) {
    let last = children.len() - 1;
    if p.is_nan() || p <= children[0].1 {
        out.push((children[0].0, 1.0));
        return;
    }
    if p >= children[last].1 {
        out.push((children[last].0, 1.0));
        return;
    }
    // First child strictly above p; p is inside (t[0], t[last]).
    let next = children.partition_point(|c| c.1 <= p);
    let (a, ta) = children[next - 1];
    let (b, tb) = children[next];
    let w = (p - ta) / (tb - ta);
    if w < 1.0 {
        out.push((a, 1.0 - w));
    }
    if w > 0.0 {
        out.push((b, w));
    }
}

/// Gradient-band (freeform cartesian) weights: each child's influence is the
/// smallest, over the other children, of `1 - (p - pi)·(pj - pi) / |pj - pi|²`,
/// clamped at 0, then all are normalised.
pub(crate) fn blend2d_weights(children: &[(usize, Vec2)], p: Vec2, out: &mut Vec<(usize, f32)>) {
    if children.len() == 1 || !p.is_finite() {
        out.push((children[0].0, 1.0));
        return;
    }
    let mut total = 0.0;
    for (i, &(c, pi)) in children.iter().enumerate() {
        let mut h = 1.0f32;
        for (j, &(_, pj)) in children.iter().enumerate() {
            if i == j {
                continue;
            }
            let d = pj - pi;
            h = h.min(1.0 - (p - pi).dot(d) / d.length_squared());
        }
        if h > 0.0 {
            out.push((c, h));
            total += h;
        }
    }
    if total <= 0.0 {
        // Unreachable for distinct points; fall back to the nearest child.
        out.clear();
        let nearest = children
            .iter()
            .min_by(|a, b| a.1.distance_squared(p).total_cmp(&b.1.distance_squared(p)))
            .expect("not empty");
        out.push((nearest.0, 1.0));
        return;
    }
    for (_, w) in out.iter_mut() {
        *w /= total;
    }
}

// --- Playing --------------------------------------------------------------

/// The clocks a node is evaluated at: the previous and current normalised
/// time of the nearest synced ancestor (or the state) and seconds since the
/// state was entered.
#[derive(Clone, Copy, Debug)]
struct Clock {
    norm: [f64; 2],
    time: [f64; 2],
    synced: bool,
}

impl Clock {
    const START: Clock = Clock {
        norm: [0.0, 0.0],
        time: [0.0, 0.0],
        synced: true,
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Active {
    state: usize,
    /// Seconds since entered.
    time: f64,
    /// Passes through the node since entered.
    norm: f64,
    prev_time: f64,
    prev_norm: f64,
    /// No step has advanced it yet.
    fresh: bool,
}

impl Active {
    fn enter(state: usize) -> Active {
        Active {
            state,
            time: 0.0,
            norm: 0.0,
            prev_time: 0.0,
            prev_norm: 0.0,
            fresh: true,
        }
    }

    fn clock(&self) -> Clock {
        Clock {
            norm: [self.prev_norm, self.norm],
            time: [self.prev_time, self.time],
            synced: true,
        }
    }
}

#[derive(Clone, Debug)]
enum Source {
    /// The state being left keeps playing.
    Live(Active),
    /// An interrupted crossfade's pose (and, for an additive layer, its
    /// reference).
    Frozen { pose: Pose, reference: Pose },
}

#[derive(Clone, Debug)]
struct Fade {
    source: Source,
    elapsed: f32,
    duration: f32,
    interruptible: bool,
}

#[derive(Clone, Debug)]
struct LayerState {
    current: Active,
    fade: Option<Fade>,
}

/// One character's graph: its parameters, each layer's state machine and the
/// scratch poses evaluation reuses.
#[derive(Clone, Debug)]
pub struct GraphPlayer {
    graph: Arc<BoundGraph>,
    params: Vec<Param>,
    layers: Vec<LayerState>,
    scratch: Vec<Pose>,
    weights: Vec<(usize, f32)>,
}

impl GraphPlayer {
    /// Parameters at their defaults, every layer in its entry state.
    pub fn new(graph: Arc<BoundGraph>) -> GraphPlayer {
        let params = graph
            .params
            .iter()
            .map(|(_, v)| match v {
                ParamValue::Float { default } => Param::Float(*default),
                ParamValue::Bool { default } => Param::Bool(*default),
                ParamValue::Trigger => Param::Trigger(false),
            })
            .collect();
        let layers = graph
            .layers
            .iter()
            .map(|l| LayerState {
                current: Active::enter(l.entry),
                fade: None,
            })
            .collect();
        GraphPlayer {
            graph,
            params,
            layers,
            scratch: Vec::new(),
            weights: Vec::new(),
        }
    }

    /// The bound graph.
    pub fn graph(&self) -> &Arc<BoundGraph> {
        &self.graph
    }

    fn index(&self, id: &str) -> Result<usize, Diagnostic> {
        self.graph
            .params
            .iter()
            .position(|(p, _)| p == id)
            .ok_or_else(|| {
                self.graph
                    .param_error(id, "the graph has no such parameter".into())
            })
    }

    /// Sets a float or bool parameter. A [`Param::Trigger`] sets (`true`) or
    /// clears a trigger.
    ///
    /// # Errors
    ///
    /// [`codes::GRAPH_PARAM_UNKNOWN`] for a parameter the graph lacks or a
    /// value of the wrong type (a non-finite float included); the message
    /// lists the graph's parameters.
    pub fn set(&mut self, id: &str, value: Param) -> Result<(), Diagnostic> {
        let i = self.index(id)?;
        let slot = &mut self.params[i];
        if std::mem::discriminant(slot) != std::mem::discriminant(&value) {
            let have = slot.type_name();
            return Err(self.graph.param_error(
                id,
                format!("it is a {have}, but was given a {}", value.type_name()),
            ));
        }
        if let Param::Float(v) = value
            && !v.is_finite()
        {
            return Err(self
                .graph
                .param_error(id, format!("{v} is not a finite number")));
        }
        *slot = value;
        Ok(())
    }

    /// Sets a trigger, which stays set until a transition consumes it.
    ///
    /// # Errors
    ///
    /// [`codes::GRAPH_PARAM_UNKNOWN`] when `id` is not a trigger of the graph.
    pub fn trigger(&mut self, id: &str) -> Result<(), Diagnostic> {
        self.set(id, Param::Trigger(true))
    }

    /// A parameter's value.
    ///
    /// # Errors
    ///
    /// [`codes::GRAPH_PARAM_UNKNOWN`] for a parameter the graph lacks.
    pub fn get(&self, id: &str) -> Result<Param, Diagnostic> {
        Ok(self.params[self.index(id)?])
    }

    /// The state a layer (by id; `None` for the first) is in, or fading into.
    pub fn state(&self, layer: Option<&str>) -> Option<&str> {
        let i = match layer {
            None => 0,
            Some(id) => self.graph.layers.iter().position(|l| l.id == id)?,
        };
        let state = self.layers.get(i)?.current.state;
        Some(self.graph.layers[i].states[state].0.as_str())
    }

    /// A layer's normalised time in its current state.
    pub fn normalised_time(&self, layer: Option<&str>) -> Option<f64> {
        let i = match layer {
            None => 0,
            Some(id) => self.graph.layers.iter().position(|l| l.id == id)?,
        };
        Some(self.layers.get(i)?.current.norm)
    }

    /// Appends the state that determinism checks hash: parameter values,
    /// and each layer's states, times and crossfade, as exact bits. Poses are
    /// not included; they follow from this.
    pub fn write_state(&self, out: &mut Vec<u8>) {
        for p in &self.params {
            match *p {
                Param::Float(v) => {
                    out.push(0);
                    out.extend_from_slice(&v.to_bits().to_le_bytes());
                }
                Param::Bool(v) => out.extend_from_slice(&[1, v as u8]),
                Param::Trigger(v) => out.extend_from_slice(&[2, v as u8]),
            }
        }
        let active = |out: &mut Vec<u8>, a: &Active| {
            out.extend_from_slice(&(a.state as u32).to_le_bytes());
            out.extend_from_slice(&a.time.to_bits().to_le_bytes());
            out.extend_from_slice(&a.norm.to_bits().to_le_bytes());
        };
        for l in &self.layers {
            active(out, &l.current);
            match &l.fade {
                None => out.push(0),
                Some(f) => {
                    out.push(1);
                    out.extend_from_slice(&f.elapsed.to_bits().to_le_bytes());
                    out.extend_from_slice(&f.duration.to_bits().to_le_bytes());
                    match &f.source {
                        Source::Live(a) => {
                            out.push(0);
                            active(out, a);
                        }
                        Source::Frozen { pose, .. } => {
                            out.push(1);
                            for j in &pose.joints {
                                for v in j
                                    .translation
                                    .to_array()
                                    .into_iter()
                                    .chain(j.rotation.to_array())
                                    .chain(j.scale.to_array())
                                {
                                    out.extend_from_slice(&v.to_bits().to_le_bytes());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Advances every layer by `dt` seconds (negative or non-finite counts as
    /// 0), fires transitions, and writes the blended pose into `pose`.
    /// Returns the root motion of the step.
    pub fn step(&mut self, dt: f32, pose: &mut Pose) -> RootMotion {
        let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        let graph = Arc::clone(&self.graph);
        for (i, layer) in graph.layers.iter().enumerate() {
            self.advance_layer(&graph, i, layer, dt);
        }

        pose.joints.clone_from(&graph.rest.joints);
        let mut motion = RootMotion::IDENTITY;
        let mut layer_pose = Pose::default();
        let mut layer_ref = Pose::default();
        for (i, layer) in graph.layers.iter().enumerate() {
            let m = self.eval_layer(&graph, i, layer, &mut layer_pose, &mut layer_ref);
            let mask = |j: usize| layer.mask.as_ref().map_or(1.0, |m| m[j]);
            if layer.additive {
                for (j, (out, (add, reference))) in pose
                    .joints
                    .iter_mut()
                    .zip(layer_pose.joints.iter().zip(&layer_ref.joints))
                    .enumerate()
                {
                    add_delta(out, add, reference, layer.weight * mask(j));
                }
            } else {
                for (j, (out, l)) in pose.joints.iter_mut().zip(&layer_pose.joints).enumerate() {
                    *out = blend(out, l, layer.weight * mask(j));
                }
                if !pose.joints.is_empty() {
                    motion = motion.blend(m, layer.weight * mask(0));
                }
            }
        }
        motion
    }

    /// Advances layer `i`'s times and crossfade, then fires its first passing
    /// transition.
    fn advance_layer(&mut self, graph: &BoundGraph, i: usize, layer: &Layer, dt: f32) {
        let advance = |a: &mut Active, params: &[Param]| {
            a.prev_time = a.time;
            a.prev_norm = a.norm;
            a.time += f64::from(dt);
            let d = graph.duration(layer.states[a.state].1, params);
            if d > 0.0 {
                a.norm += f64::from(dt) / f64::from(d);
            } else if dt > 0.0 {
                a.norm = a.norm.floor() + 1.0;
            }
        };
        let params = &self.params;
        let state = &mut self.layers[i];
        let was_fresh = state.current.fresh;
        advance(&mut state.current, params);
        state.current.fresh = false;
        if let Some(fade) = &mut state.fade {
            if let Source::Live(a) = &mut fade.source {
                advance(a, params);
            }
            fade.elapsed += dt;
            if fade.elapsed >= fade.duration {
                state.fade = None;
            }
        }
        if state.fade.as_ref().is_some_and(|f| !f.interruptible) {
            return;
        }

        let current = state.current;
        let Some(edge) = layer.edges.iter().find(|e| {
            (match e.from {
                Some(from) => from == current.state,
                None => e.to != current.state,
            }) && e
                .exit_time
                .is_none_or(|x| exit_passed(x, &current, was_fresh))
                && e.conditions.iter().all(|c| match *c {
                    Cond::Compare { param, op, value } => op.test(float(params, param), value),
                    Cond::Bool { param, value } => params[param] == Param::Bool(value),
                    Cond::Trigger { param } => params[param] == Param::Trigger(true),
                })
        }) else {
            return;
        };
        for c in &edge.conditions {
            if let Cond::Trigger { param } = *c {
                self.params[param] = Param::Trigger(false);
            }
        }
        let source = if self.layers[i].fade.is_some() {
            let mut pose = Pose::default();
            let mut reference = Pose::default();
            self.eval_layer(graph, i, layer, &mut pose, &mut reference);
            Source::Frozen { pose, reference }
        } else {
            Source::Live(current)
        };
        let state = &mut self.layers[i];
        state.current = Active::enter(edge.to);
        state.fade = (edge.duration > 0.0).then_some(Fade {
            source,
            elapsed: 0.0,
            duration: edge.duration,
            interruptible: edge.interruptible,
        });
    }

    /// Layer `i`'s pose (and, for an additive layer, its reference) with its
    /// crossfade applied; returns its root motion.
    fn eval_layer(
        &mut self,
        graph: &BoundGraph,
        i: usize,
        layer: &Layer,
        pose: &mut Pose,
        reference: &mut Pose,
    ) -> RootMotion {
        let current = self.layers[i].current;
        let node = layer.states[current.state].1;
        let mut motion = self.eval_node(graph, node, current.clock(), pose);
        if layer.additive {
            self.eval_reference(graph, layer, node, reference);
        }
        let Some(fade) = self.layers[i].fade.clone() else {
            return motion;
        };
        let w = (fade.elapsed / fade.duration).clamp(0.0, 1.0);
        let mut from = Pose::default();
        let mut from_ref = Pose::default();
        let from_motion = match &fade.source {
            Source::Live(a) => {
                let node = layer.states[a.state].1;
                let m = self.eval_node(graph, node, a.clock(), &mut from);
                if layer.additive {
                    self.eval_reference(graph, layer, node, &mut from_ref);
                }
                m
            }
            Source::Frozen {
                pose: p,
                reference: r,
            } => {
                from.joints.clone_from(&p.joints);
                from_ref.joints.clone_from(&r.joints);
                RootMotion::IDENTITY
            }
        };
        for (a, b) in from.joints.iter_mut().zip(&pose.joints) {
            *a = blend(a, b, w);
        }
        std::mem::swap(pose, &mut from);
        if layer.additive {
            for (a, b) in from_ref.joints.iter_mut().zip(&reference.joints) {
                *a = blend(a, b, w);
            }
            std::mem::swap(reference, &mut from_ref);
        }
        motion = from_motion.blend(motion, w);
        motion
    }

    fn eval_reference(&mut self, graph: &BoundGraph, layer: &Layer, node: usize, out: &mut Pose) {
        match layer.reference {
            Some(clip) => {
                out.joints.clone_from(&graph.rest.joints);
                graph.clips[clip].sample(0.0, out);
            }
            None => {
                self.eval_node(graph, node, Clock::START, out);
            }
        }
    }

    /// Evaluates `node` into `out`; returns its root motion.
    fn eval_node(
        &mut self,
        graph: &BoundGraph,
        node: usize,
        clock: Clock,
        out: &mut Pose,
    ) -> RootMotion {
        match &graph.nodes[node] {
            &Node::Clip {
                clip,
                speed,
                looping,
                root_motion,
            } => {
                let bound = &graph.clips[clip];
                let d = bound.duration();
                let x = |k: usize| -> f64 {
                    if clock.synced {
                        clock.norm[k] * f64::from(d)
                    } else {
                        clock.time[k] * f64::from(speed.abs())
                    }
                };
                let (x0, x1) = (x(0), x(1));
                let t1 = clip_time(x1, d, looping, speed < 0.0);
                out.joints.clone_from(&graph.rest.joints);
                bound.sample(t1, out);
                if !root_motion || out.joints.is_empty() {
                    return RootMotion::IDENTITY;
                }
                let rest = graph.rest.joints[0];
                let at = |t: f32| bound.sample_joint(0, t, rest);
                let first = at(0.0);
                let y0 = yaw(first.rotation);
                let root = &mut out.joints[0];
                let yt = yaw(root.rotation);
                root.rotation = (y0 * yt.conjugate() * root.rotation).normalize();
                root.translation.x = first.translation.x;
                root.translation.z = first.translation.z;
                let m = clip_motion(&at, x0, x1, d, looping, speed < 0.0);
                RootMotion {
                    translation: y0 * m.translation,
                    rotation: m.rotation,
                }
            }
            Node::Blend1d { sync, .. } | Node::Blend2d { sync, .. } => {
                let sync = *sync;
                let mut weights = std::mem::take(&mut self.weights);
                graph.weights(node, &self.params, &mut weights);
                let child_clock = if sync {
                    let norm = if clock.synced {
                        clock.norm
                    } else {
                        let d = f64::from(graph.duration(node, &self.params));
                        if d > 0.0 {
                            [clock.time[0] / d, clock.time[1] / d]
                        } else {
                            [0.0, 0.0]
                        }
                    };
                    Clock {
                        norm,
                        time: clock.time,
                        synced: true,
                    }
                } else {
                    Clock {
                        synced: false,
                        ..clock
                    }
                };
                let mut tmp = self.take_scratch();
                let mut motion = RootMotion::IDENTITY;
                let mut total = 0.0;
                for (k, &(child, w)) in weights.iter().enumerate() {
                    let m = self.eval_node(graph, child, child_clock, &mut tmp);
                    total += w;
                    if k == 0 {
                        std::mem::swap(out, &mut tmp);
                        motion = m;
                    } else {
                        let f = w / total;
                        out.blend_toward(&tmp, f);
                        motion = motion.blend(m, f);
                    }
                }
                self.scratch.push(tmp);
                weights.clear();
                self.weights = weights;
                motion
            }
            &Node::Additive {
                base,
                additive,
                weight,
                weight_param,
            } => {
                let w = weight_param.map_or(weight, |p| float(&self.params, p));
                let motion = self.eval_node(graph, base, clock, out);
                let mut add = self.take_scratch();
                let mut reference = self.take_scratch();
                self.eval_node(graph, additive, clock, &mut add);
                self.eval_node(graph, additive, Clock::START, &mut reference);
                for ((o, a), r) in out
                    .joints
                    .iter_mut()
                    .zip(&add.joints)
                    .zip(&reference.joints)
                {
                    add_delta(o, a, r, w);
                }
                self.scratch.push(add);
                self.scratch.push(reference);
                motion
            }
        }
    }

    fn take_scratch(&mut self) -> Pose {
        self.scratch.pop().unwrap_or_default()
    }
}

/// Whether a transition waiting for normalised time `exit` may fire after
/// `a` advanced this step (from `prev_norm` to `norm`; `fresh` on the step
/// the state was entered, whose start counts as before 0).
fn exit_passed(exit: f32, a: &Active, fresh: bool) -> bool {
    let exit = f64::from(exit);
    if exit >= 1.0 {
        return a.norm >= exit;
    }
    // The latest exit + k (k >= 0) reached so far; it fires once, on the step
    // that passes it.
    let k = (a.norm - exit).floor();
    k >= 0.0 && (fresh || exit + k > a.prev_norm)
}

/// The clip time for unwrapped clip seconds `x` (forward from the start).
fn clip_time(x: f64, d: f32, looping: bool, reverse: bool) -> f32 {
    if d <= 0.0 {
        return 0.0;
    }
    let d64 = f64::from(d);
    let t = if looping {
        x.rem_euclid(d64)
    } else {
        x.clamp(0.0, d64)
    } as f32;
    if reverse { d - t } else { t }
}

/// The rotation about +Y in `q` (its twist about the vertical axis).
fn yaw(q: Quat) -> Quat {
    let v = glam::Vec4::new(0.0, q.y, 0.0, q.w);
    let len2 = v.length_squared();
    if len2 > 1e-12 {
        Quat::from_vec4(v / len2.sqrt())
    } else {
        Quat::IDENTITY
    }
}

/// The root's motion from clip time `a` to `b`, in the frame (yaw) it has at
/// `a`.
fn segment(at: &dyn Fn(f32) -> Trs, a: f32, b: f32) -> RootMotion {
    if a == b {
        return RootMotion::IDENTITY;
    }
    let ra = at(a);
    let rb = at(b);
    let ya = yaw(ra.rotation);
    let mut d = rb.translation - ra.translation;
    d.y = 0.0;
    let mut translation = ya.conjugate() * d;
    translation.y = 0.0;
    RootMotion {
        translation,
        rotation: (ya.conjugate() * yaw(rb.rotation)).normalize(),
    }
}

/// The root's motion as unwrapped clip seconds go from `x0` to `x1`, through
/// every wrap of a looping clip.
fn clip_motion(
    at: &dyn Fn(f32) -> Trs,
    x0: f64,
    x1: f64,
    d: f32,
    looping: bool,
    reverse: bool,
) -> RootMotion {
    if d <= 0.0 || x1 <= x0 {
        return RootMotion::IDENTITY;
    }
    let a = clip_time(x0, d, looping, reverse);
    let b = clip_time(x1, d, looping, reverse);
    if !looping {
        return segment(at, a, b);
    }
    let d64 = f64::from(d);
    let wraps = ((x1 / d64).floor() - (x0 / d64).floor()) as i64;
    if wraps <= 0 {
        return segment(at, a, b);
    }
    let (start, end) = if reverse { (d, 0.0) } else { (0.0, d) };
    let full = segment(at, start, end);
    let mut m = segment(at, a, end);
    for _ in 1..wraps {
        m = m.then(full);
    }
    m.then(segment(at, start, b))
}

/// Adds `w` of `add`'s difference from `reference` onto `base`: translation
/// by difference, rotation by the local delta `reference⁻¹ · add` (nlerped
/// from identity), scale by ratio.
pub(crate) fn add_delta(base: &mut Trs, add: &Trs, reference: &Trs, w: f32) {
    if w == 0.0 {
        return;
    }
    base.translation += (add.translation - reference.translation) * w;
    let delta = reference.rotation.conjugate() * add.rotation;
    base.rotation = (base.rotation * nlerp(Quat::IDENTITY, delta, w)).normalize();
    let ratio = Vec3::select(
        reference.scale.cmpeq(Vec3::ZERO),
        Vec3::ONE,
        add.scale / reference.scale,
    );
    base.scale *= Vec3::ONE.lerp(ratio, w);
}
