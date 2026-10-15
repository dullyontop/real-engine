//! The R6 and R15 character rigs, built from the data in `assets/rigs/`.
//!
//! A rig file lists the parts (size and position in the model, with the
//! model's origin on the floor under the root part), the `Motor6D` joints
//! (with `C0` and `C1`) and the attachments. [`build`] makes the model: its
//! parts, joints and attachments, a `Humanoid` with an `Animator`, coloured
//! and scaled by a `HumanoidDescription` when one is given.

use std::sync::OnceLock;

use re_reflect::Value;
use re_scene::{Entity, World};
use serde::Deserialize;

use crate::anim::cframe_value;
use crate::class::ClassRegistry;
use crate::codes;
use crate::datatypes::CFrame;
use crate::error::CompatError;
use crate::input::{get_own, set_own};
use crate::instance::tree;
use crate::scene::spawn_instance;

const R6: &str = include_str!("../../assets/rigs/r6.json");
const R15: &str = include_str!("../../assets/rigs/r15.json");

/// A rig file (`real-engine/compat-rig` version 1).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RigData {
    /// Always `real-engine/compat-rig`.
    pub format: String,
    /// Always 1.
    pub version: u32,
    /// `R6` or `R15`.
    pub rig: String,
    /// What the rig is.
    pub doc: String,
    /// The Humanoid's `HipHeight` for this rig.
    pub hip_height: f32,
    /// The parts.
    pub parts: Vec<RigPart>,
    /// The joints.
    pub joints: Vec<RigJoint>,
    /// The attachments.
    pub attachments: Vec<RigAttachment>,
}

/// One part of a rig.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RigPart {
    /// Its name.
    pub name: String,
    /// Its size, in studs.
    pub size: [f32; 3],
    /// Its centre, in the model (origin on the floor under the root part).
    pub position: [f32; 3],
    /// Its transparency (the root part is invisible).
    #[serde(default)]
    pub transparency: f32,
    /// The `HumanoidDescription` colour property it takes, if any.
    #[serde(default)]
    pub color: Option<String>,
}

/// One `Motor6D` of a rig.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RigJoint {
    /// Its name.
    pub name: String,
    /// The part it sits in.
    pub parent: String,
    /// The part it hangs from.
    pub part0: String,
    /// The part it moves.
    pub part1: String,
    /// `C0`: position then the rotation matrix by rows.
    pub c0: [f32; 12],
    /// `C1`, as `c0`.
    pub c1: [f32; 12],
}

/// One attachment of a rig.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RigAttachment {
    /// Its name.
    pub name: String,
    /// The part it sits in.
    pub part: String,
    /// Its position in the part.
    pub position: [f32; 3],
}

impl RigData {
    /// The height of the root part's centre above the floor.
    pub fn root_height(&self) -> f32 {
        self.part("HumanoidRootPart").map_or(0.0, |p| p.position[1])
    }

    /// The part named `name`.
    pub fn part(&self, name: &str) -> Option<&RigPart> {
        self.parts.iter().find(|p| p.name == name)
    }

    fn check(&self) -> Result<(), String> {
        if self.format != "real-engine/compat-rig" || self.version != 1 {
            return Err(format!("{}: not a version 1 rig file", self.rig));
        }
        if self.part("HumanoidRootPart").is_none() {
            return Err(format!("{}: no HumanoidRootPart", self.rig));
        }
        for j in &self.joints {
            for p in [&j.parent, &j.part0, &j.part1] {
                if self.part(p).is_none() {
                    return Err(format!(
                        "{}: joint {} names no part {p:?}",
                        self.rig, j.name
                    ));
                }
            }
        }
        for a in &self.attachments {
            if self.part(&a.part).is_none() {
                return Err(format!(
                    "{}: attachment {} names no part {:?}",
                    self.rig, a.name, a.part
                ));
            }
        }
        Ok(())
    }
}

fn parse(text: &str) -> RigData {
    let r: RigData = serde_json::from_str(text).expect("the built-in rig files parse");
    if let Err(e) = r.check() {
        panic!("built-in rig file: {e}");
    }
    r
}

/// The built-in rig of a rig type (`R6` or `R15`).
pub fn data(rig_type: &str) -> Option<&'static RigData> {
    static R6_DATA: OnceLock<RigData> = OnceLock::new();
    static R15_DATA: OnceLock<RigData> = OnceLock::new();
    match rig_type {
        "R6" => Some(R6_DATA.get_or_init(|| parse(R6))),
        "R15" => Some(R15_DATA.get_or_init(|| parse(R15))),
        _ => None,
    }
}

/// A CFrame from a rig file's 12 numbers.
pub fn cframe12(v: &[f32; 12]) -> CFrame {
    CFrame::from_parts(
        [v[0], v[1], v[2]],
        [[v[3], v[4], v[5]], [v[6], v[7], v[8]], [v[9], v[10], v[11]]],
    )
}

/// Body scale factors from a `HumanoidDescription`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    /// Along X.
    pub width: f32,
    /// Along Y.
    pub height: f32,
    /// Along Z.
    pub depth: f32,
    /// The head, on every axis.
    pub head: f32,
}

impl Default for Scale {
    fn default() -> Scale {
        Scale {
            width: 1.0,
            height: 1.0,
            depth: 1.0,
            head: 1.0,
        }
    }
}

impl Scale {
    /// The scale stored in a `HumanoidDescription`.
    pub fn of(w: &World, desc: Entity) -> Result<Scale, CompatError> {
        let get = |p: &str| -> Result<f32, CompatError> {
            let v = match get_own(w, "HumanoidDescription", desc, p) {
                Some(Value::Float(x)) => x as f32,
                Some(Value::Int(i)) => i as f32,
                _ => 1.0,
            };
            if !(v.is_finite() && v > 0.0) {
                return Err(CompatError::Runtime {
                    code: codes::INVALID_RIG,
                    message: format!("HumanoidDescription.{p} must be a positive number, got {v}"),
                });
            }
            Ok(v)
        };
        Ok(Scale {
            width: get("WidthScale")?,
            height: get("HeightScale")?,
            depth: get("DepthScale")?,
            head: get("HeadScale")?,
        })
    }

    /// Scales a vector in the body's frame.
    pub fn apply(&self, v: [f32; 3]) -> [f32; 3] {
        [v[0] * self.width, v[1] * self.height, v[2] * self.depth]
    }
}

fn set(
    w: &mut World,
    classes: &ClassRegistry,
    e: Entity,
    prop: &str,
    v: Value,
) -> Result<(), CompatError> {
    classes.set_property(w, e, prop, v)
}

fn vec3(v: [f32; 3]) -> Value {
    Value::Vec3(glam::Vec3::from_array(v))
}

/// Builds a character model of `rig_type` with its root part's floor point at
/// `at`, named `name` and not parented. Colours and scales come from `desc`
/// when given.
pub fn build(
    w: &mut World,
    classes: &ClassRegistry,
    rig_type: &str,
    name: &str,
    desc: Option<Entity>,
    at: CFrame,
) -> Result<Entity, CompatError> {
    let data = data(rig_type).ok_or_else(|| CompatError::Runtime {
        code: codes::INVALID_RIG,
        message: format!("unknown rig type {rig_type:?}: R6 or R15 expected"),
    })?;
    let scale = match desc {
        Some(d) => Scale::of(w, d)?,
        None => Scale::default(),
    };
    let model = spawn_instance(w, classes, "Model", None, name, None)?;
    let mut parts = std::collections::HashMap::new();
    for p in &data.parts {
        let e = spawn_instance(w, classes, "Part", None, &p.name, Some(model))?;
        let s = if p.name == "Head" {
            [
                p.size[0] * scale.head,
                p.size[1] * scale.head,
                p.size[2] * scale.head,
            ]
        } else {
            scale.apply(p.size)
        };
        set(w, classes, e, "Size", vec3(s))?;
        let pos = scale.apply(p.position);
        set(
            w,
            classes,
            e,
            "CFrame",
            cframe_value(&(at * CFrame::from_position(pos))),
        )?;
        set(
            w,
            classes,
            e,
            "Transparency",
            Value::Float(f64::from(p.transparency)),
        )?;
        if let (Some(color), Some(d)) = (&p.color, desc)
            && let Some(c) = get_own(w, "HumanoidDescription", d, color)
        {
            set(w, classes, e, "Color", c)?;
        }
        parts.insert(p.name.as_str(), e);
    }
    for j in &data.joints {
        let parent = parts[j.parent.as_str()];
        let m = spawn_instance(w, classes, "Motor6D", None, &j.name, Some(parent))?;
        let mut c0 = cframe12(&j.c0);
        let mut c1 = cframe12(&j.c1);
        c0.p = scale.apply(c0.p);
        c1.p = scale.apply(c1.p);
        set(
            w,
            classes,
            m,
            "Part0",
            Value::Entity(Some(parts[j.part0.as_str()])),
        )?;
        set(
            w,
            classes,
            m,
            "Part1",
            Value::Entity(Some(parts[j.part1.as_str()])),
        )?;
        set(w, classes, m, "C0", cframe_value(&c0))?;
        set(w, classes, m, "C1", cframe_value(&c1))?;
    }
    for a in &data.attachments {
        let parent = parts[a.part.as_str()];
        let e = spawn_instance(w, classes, "Attachment", None, &a.name, Some(parent))?;
        let pos = scale.apply(a.position);
        set(
            w,
            classes,
            e,
            "CFrame",
            cframe_value(&CFrame::from_position(pos)),
        )?;
    }
    let humanoid = spawn_instance(w, classes, "Humanoid", None, "Humanoid", Some(model))?;
    set(
        w,
        classes,
        humanoid,
        "RigType",
        Value::Enum(data.rig.clone()),
    )?;
    set(
        w,
        classes,
        humanoid,
        "HipHeight",
        Value::Float(f64::from(data.hip_height * scale.height)),
    )?;
    let root = parts.get("HumanoidRootPart").copied();
    set_own(w, "Humanoid", humanoid, "RootPart", Value::Entity(root));
    spawn_instance(w, classes, "Animator", None, "Animator", Some(humanoid))?;
    if let Some(d) = desc {
        super::description::remember(w, classes, humanoid, d)?;
    }
    debug_assert!(tree::find_child(w, model, "HumanoidRootPart", false).is_some());
    Ok(model)
}

/// Fills `model`, a character model with no `Humanoid` (a starter character
/// a project names but does not ship), with a rig of `rig_type` at the
/// origin. Its own children (an `Animate` script, say) stay. Returns `false`,
/// changing nothing, when `model` already has a `Humanoid`.
pub fn fill(
    w: &mut World,
    classes: &ClassRegistry,
    model: Entity,
    rig_type: &str,
) -> Result<bool, CompatError> {
    let has_humanoid = tree::children(w, model)
        .into_iter()
        .any(|c| tree::is_a(w, classes, c, "Humanoid"));
    if has_humanoid {
        return Ok(false);
    }
    let name = tree::name(w, model);
    let rig = build(w, classes, rig_type, &name, None, CFrame::IDENTITY)?;
    for c in tree::children(w, rig) {
        tree::set_parent(w, c, Some(model))?;
    }
    if classes.member("Model", "PrimaryPart").is_ok()
        && let Some(root) = tree::find_child(w, model, "HumanoidRootPart", false)
    {
        classes.set_property(w, model, "PrimaryPart", Value::Entity(Some(root)))?;
    }
    re_scene::hierarchy::despawn_recursive(&mut w.ecs, rig);
    Ok(true)
}

/// The rig a starter character gets when the project names one but ships no
/// rig in it.
pub const STARTER_STAND_IN: &str = "R6";

/// Fills `StarterPlayer.StarterCharacter` under `game` with the
/// [`STARTER_STAND_IN`] rig when it is a `Model` with no `Humanoid` (an
/// import that implied the container without its rig), so players and the
/// scripts that clone it get a character. Returns whether it filled one:
/// `false` when there is no such model, it has a `Humanoid`, or the class
/// layer has no parts.
pub fn fill_starter_character(
    w: &mut World,
    classes: &ClassRegistry,
    game: Entity,
) -> Result<bool, CompatError> {
    // A rig is made of parts; without the part classes there is none.
    if classes.class("Part").is_err() {
        return Ok(false);
    }
    let Some(model) = tree::find_child(w, game, "StarterPlayer", false)
        .and_then(|sp| tree::find_child(w, sp, "StarterCharacter", false))
        .filter(|m| tree::class(w, *m) == Some("Model"))
    else {
        return Ok(false);
    };
    fill(w, classes, model, STARTER_STAND_IN)
}
