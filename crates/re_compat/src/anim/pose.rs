//! Rig poses: the `Motor6D` joints of a model, and the world frames of its
//! parts worked out from the root part through those joints.
//!
//! A joint holds its `Part1` at `Part0.CFrame * C0 * Transform * C1:Inverse()`.
//! The Animator writes `Transform`; [`solve`] follows the joints out from the
//! root part, and [`apply`] writes the result to the parts' `CFrame`.

use std::collections::BTreeMap;

use re_reflect::Value;
use re_scene::{Entity, World};

use super::{cframe_value, value_cframe};
use crate::class::ClassRegistry;
use crate::datatypes::CFrame;
use crate::instance::tree;

/// One `Motor6D` of a rig.
#[derive(Clone, Copy, Debug)]
pub struct Joint {
    /// The `Motor6D` instance.
    pub motor: Entity,
    /// The part it hangs from.
    pub part0: Entity,
    /// The part it moves.
    pub part1: Entity,
    /// The joint's frame in `part0`.
    pub c0: CFrame,
    /// The joint's frame in `part1`.
    pub c1: CFrame,
    /// The animated offset between the two.
    pub transform: CFrame,
}

impl Joint {
    /// The world frame of the joint after `Transform`, given `part0`'s frame.
    pub fn world(&self, part0: &CFrame) -> CFrame {
        *part0 * self.c0 * self.transform
    }

    /// `part1`'s world frame, given `part0`'s.
    pub fn part1_world(&self, part0: &CFrame) -> CFrame {
        self.world(part0) * self.c1.inverse()
    }
}

/// A CFrame property of an instance, if it has one.
pub fn cframe_prop(w: &World, classes: &ClassRegistry, e: Entity, prop: &str) -> Option<CFrame> {
    value_cframe(&classes.get_property(w, e, prop).ok()?)
}

fn instance_prop(w: &World, classes: &ClassRegistry, e: Entity, prop: &str) -> Option<Entity> {
    match classes.get_property(w, e, prop) {
        Ok(Value::Entity(x)) => x,
        _ => None,
    }
}

/// The live `Motor6D` joints under `rig` that connect two parts.
pub fn joints(w: &World, classes: &ClassRegistry, rig: Entity) -> Vec<Joint> {
    let mut out = Vec::new();
    for m in tree::descendants(w, rig) {
        if !tree::is_a(w, classes, m, "Motor6D") || tree::is_destroyed(w, m) {
            continue;
        }
        let (Some(part0), Some(part1)) = (
            instance_prop(w, classes, m, "Part0"),
            instance_prop(w, classes, m, "Part1"),
        ) else {
            continue;
        };
        out.push(Joint {
            motor: m,
            part0,
            part1,
            c0: cframe_prop(w, classes, m, "C0").unwrap_or_default(),
            c1: cframe_prop(w, classes, m, "C1").unwrap_or_default(),
            transform: cframe_prop(w, classes, m, "Transform").unwrap_or_default(),
        });
    }
    out
}

/// The part a rig is posed from: its `HumanoidRootPart`, else its
/// `PrimaryPart`, else the first part no joint moves.
pub fn root_part(
    w: &World,
    classes: &ClassRegistry,
    rig: Entity,
    joints: &[Joint],
) -> Option<Entity> {
    if let Some(hrp) = tree::find_child(w, rig, "HumanoidRootPart", false)
        && tree::is_a(w, classes, hrp, "BasePart")
    {
        return Some(hrp);
    }
    if let Some(p) = instance_prop(w, classes, rig, "PrimaryPart") {
        return Some(p);
    }
    joints
        .iter()
        .map(|j| j.part0)
        .find(|p| !joints.iter().any(|j| j.part1 == *p))
}

/// The world frame of every part reachable from the rig's root part through
/// its joints, starting from the root part's `CFrame`. A part that two joints
/// move keeps the first one found.
pub fn solve(w: &World, classes: &ClassRegistry, rig: Entity) -> BTreeMap<Entity, CFrame> {
    let js = joints(w, classes, rig);
    let mut out = BTreeMap::new();
    let Some(root) = root_part(w, classes, rig, &js) else {
        return out;
    };
    out.insert(
        root,
        cframe_prop(w, classes, root, "CFrame").unwrap_or_default(),
    );
    solve_from(&js, &mut out, root);
    out
}

/// Extends `frames` outward from `from` through `joints`.
pub fn solve_from(joints: &[Joint], frames: &mut BTreeMap<Entity, CFrame>, from: Entity) {
    let mut by_part0: BTreeMap<Entity, Vec<&Joint>> = BTreeMap::new();
    for j in joints {
        by_part0.entry(j.part0).or_default().push(j);
    }
    let mut stack = vec![from];
    while let Some(p) = stack.pop() {
        let Some(base) = frames.get(&p).copied() else {
            continue;
        };
        for j in by_part0.get(&p).into_iter().flatten() {
            if frames.contains_key(&j.part1) {
                continue;
            }
            frames.insert(j.part1, j.part1_world(&base));
            stack.push(j.part1);
        }
    }
}

/// Moves each part of the rig, other than its root part, to where its joints
/// hold it.
pub fn apply(w: &mut World, classes: &ClassRegistry, rig: Entity) {
    let js = joints(w, classes, rig);
    let Some(root) = root_part(w, classes, rig, &js) else {
        return;
    };
    let frames = solve(w, classes, rig);
    for (part, cf) in frames {
        if part != root {
            let _ = classes.set_property(w, part, "CFrame", cframe_value(&cf));
        }
    }
}

/// The world frame of a part or an attachment (its parent part's frame times
/// its own `CFrame`), using `frames` for parts the joints place.
pub fn world_of(
    w: &World,
    classes: &ClassRegistry,
    frames: &BTreeMap<Entity, CFrame>,
    e: Entity,
) -> Option<CFrame> {
    let part_frame = |p: Entity| {
        frames
            .get(&p)
            .copied()
            .or_else(|| cframe_prop(w, classes, p, "CFrame"))
    };
    if tree::is_a(w, classes, e, "Attachment") {
        let parent = tree::parent(w, e)?;
        let local = cframe_prop(w, classes, e, "CFrame").unwrap_or_default();
        return Some(part_frame(parent)? * local);
    }
    part_frame(e)
}
