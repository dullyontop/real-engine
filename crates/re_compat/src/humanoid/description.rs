//! `HumanoidDescription`: applying body colours and scales to a rig, and the
//! copy a Humanoid keeps of the description it last applied.
//!
//! The applied description lives under the Humanoid as a child named
//! `HumanoidDescription`, which is where `GetAppliedDescription` copies it
//! from and where the default `Animate` script reads animation ids.

use re_reflect::Value;
use re_scene::{Entity, World};

use super::rig::{self, Scale};
use crate::anim::cframe_value;
use crate::anim::pose::cframe_prop;
use crate::class::ClassRegistry;
use crate::error::CompatError;
use crate::input::{get_own, set_own};
use crate::instance::tree;
use crate::scene::spawn_instance;

const DESC: &str = "HumanoidDescription";

/// Copies every property of description `from` into a new, unparented
/// description.
pub fn copy(w: &mut World, classes: &ClassRegistry, from: Entity) -> Result<Entity, CompatError> {
    let to = spawn_instance(w, classes, DESC, None, DESC, None)?;
    let names: Vec<String> = classes
        .class(DESC)?
        .properties
        .iter()
        .map(|p| p.name.clone())
        .collect();
    for n in names {
        if let Some(v) = get_own(w, DESC, from, &n) {
            set_own(w, DESC, to, &n, v);
        }
    }
    Ok(to)
}

/// The description a Humanoid last applied, if any.
pub fn applied(w: &World, classes: &ClassRegistry, humanoid: Entity) -> Option<Entity> {
    tree::children(w, humanoid)
        .into_iter()
        .find(|c| tree::name(w, *c) == DESC && tree::is_a(w, classes, *c, DESC))
}

/// Keeps a copy of `desc` under the Humanoid as its applied description.
pub fn remember(
    w: &mut World,
    classes: &ClassRegistry,
    humanoid: Entity,
    desc: Entity,
) -> Result<(), CompatError> {
    let copy = copy(w, classes, desc)?;
    if let Some(old) = applied(w, classes, humanoid) {
        tree::set_parent(w, old, None)?;
    }
    tree::set_parent(w, copy, Some(humanoid))
}

fn scale_pos(c: &mut crate::datatypes::CFrame, f: [f32; 3]) {
    c.p = [c.p[0] * f[0], c.p[1] * f[1], c.p[2] * f[2]];
}

/// Applies `desc` to the rig of `humanoid`: body colours by part, and the
/// change of scale from the description applied before (part sizes, joint and
/// attachment offsets, `HipHeight`). Returns true when the scale changed, so
/// the character's capsule must be rebuilt.
pub fn apply(
    w: &mut World,
    classes: &ClassRegistry,
    humanoid: Entity,
    desc: Entity,
) -> Result<bool, CompatError> {
    let rig = tree::parent(w, humanoid)
        .ok_or_else(|| CompatError::InvalidData("the Humanoid is not in a model".into()))?;
    let rig_type = match classes.get_property(w, humanoid, "RigType") {
        Ok(Value::Enum(s) | Value::Str(s)) => s,
        _ => "R15".to_string(),
    };
    let new = Scale::of(w, desc)?;
    let old = match applied(w, classes, humanoid) {
        Some(d) => Scale::of(w, d)?,
        None => Scale::default(),
    };
    // Colours, by the part each rig part takes its colour from.
    if let Some(data) = rig::data(&rig_type) {
        for p in &data.parts {
            if let (Some(color), Some(part)) = (&p.color, tree::find_child(w, rig, &p.name, false))
                && let Some(c) = get_own(w, DESC, desc, color)
            {
                classes.set_property(w, part, "Color", c)?;
            }
        }
    }
    let changed = new != old;
    if changed {
        let body = [
            new.width / old.width,
            new.height / old.height,
            new.depth / old.depth,
        ];
        let head = new.head / old.head;
        for d in tree::descendants(w, rig) {
            if tree::is_a(w, classes, d, "BasePart") {
                let f = if tree::name(w, d) == "Head" {
                    [head; 3]
                } else {
                    body
                };
                if let Ok(Value::Vec3(s)) = classes.get_property(w, d, "Size") {
                    let s = s * glam::Vec3::from_array(f);
                    classes.set_property(w, d, "Size", Value::Vec3(s))?;
                }
            } else if tree::is_a(w, classes, d, "Motor6D") {
                for prop in ["C0", "C1"] {
                    if let Some(mut c) = cframe_prop(w, classes, d, prop) {
                        scale_pos(&mut c, body);
                        classes.set_property(w, d, prop, cframe_value(&c))?;
                    }
                }
            } else if tree::is_a(w, classes, d, "Attachment") {
                if let Some(mut c) = cframe_prop(w, classes, d, "CFrame") {
                    scale_pos(&mut c, body);
                    classes.set_property(w, d, "CFrame", cframe_value(&c))?;
                }
            }
        }
        if let Ok(Value::Float(h)) = classes.get_property(w, humanoid, "HipHeight") {
            let h = h * f64::from(body[1]);
            classes.set_property(w, humanoid, "HipHeight", Value::Float(h))?;
        }
    }
    remember(w, classes, humanoid, desc)?;
    Ok(changed)
}
