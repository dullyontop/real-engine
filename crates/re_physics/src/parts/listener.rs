//! The contact listener that gives parts their collision rules: collision
//! groups, parts that touch without colliding, and how two parts' friction
//! and elasticity combine.
//!
//! Jolt calls it on worker threads during a step, so it reads only data that
//! is fixed for the step: the group table and each assembly body's part table,
//! both changed between steps under a write lock.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use oxijolt::{
    BodyId, ContactCandidate, ContactListener, ContactManifold, ContactSettings, Shape, SubShapeId,
    ValidateResult,
};

use super::GroupTable;

/// What the listener knows about one part of an assembly body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GatePart {
    pub(crate) group: u8,
    pub(crate) collide: bool,
    pub(crate) touch: bool,
    pub(crate) friction: f32,
    pub(crate) elasticity: f32,
    pub(crate) friction_weight: f32,
    pub(crate) elasticity_weight: f32,
}

impl Default for GatePart {
    /// What a body that is not an assembly counts as: the default group,
    /// colliding and touching, with Jolt's own friction left alone.
    fn default() -> Self {
        GatePart {
            group: 0,
            collide: true,
            touch: true,
            friction: f32::NAN,
            elasticity: f32::NAN,
            friction_weight: 1.0,
            elasticity_weight: 1.0,
        }
    }
}

/// An assembly body's compound shape (to decode sub-shape ids) and its parts,
/// in child order.
pub(crate) struct GateBody {
    pub(crate) compound: Arc<Shape>,
    pub(crate) parts: Vec<GatePart>,
}

impl GateBody {
    pub(crate) fn child(&self, sub: SubShapeId) -> Option<usize> {
        self.compound
            .compound_sub_shape(sub)
            .map(|c| c.index as usize)
            .filter(|i| *i < self.parts.len())
    }

    fn part(&self, sub: SubShapeId) -> GatePart {
        self.child(sub).map(|i| self.parts[i]).unwrap_or_default()
    }
}

#[derive(Default)]
pub(crate) struct PartGate {
    pub(crate) groups: RwLock<GroupTable>,
    pub(crate) bodies: RwLock<HashMap<u32, Arc<GateBody>>>,
}

impl PartGate {
    pub(crate) fn set_body(&self, body: BodyId, entry: Arc<GateBody>) {
        self.bodies
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(body.to_raw(), entry);
    }

    pub(crate) fn forget(&self, body: BodyId) {
        self.bodies
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&body.to_raw());
    }

    pub(crate) fn body(&self, body: BodyId) -> Option<Arc<GateBody>> {
        self.bodies
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&body.to_raw())
            .cloned()
    }

    fn part(&self, body: BodyId, sub: SubShapeId) -> GatePart {
        self.bodies
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&body.to_raw())
            .map(|b| b.part(sub))
            .unwrap_or_default()
    }

    fn collides(&self, a: u8, b: u8) -> bool {
        self.groups
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .collides(a, b)
    }

    /// The parts' combined friction and elasticity, as weighted means.
    fn combine(&self, manifold: &ContactManifold, settings: &mut ContactSettings) {
        let pair = manifold.pair;
        let a = self.part(pair.body1, pair.sub_shape1);
        let b = self.part(pair.body2, pair.sub_shape2);
        if !(a.collide && b.collide) {
            let _ = settings.set_is_sensor(true);
            return;
        }
        let mix = |x: f32, wx: f32, y: f32, wy: f32| {
            let w = wx + wy;
            if w > 0.0 {
                (x * wx + y * wy) / w
            } else {
                (x + y) * 0.5
            }
        };
        if a.friction.is_finite() && b.friction.is_finite() {
            let f = mix(a.friction, a.friction_weight, b.friction, b.friction_weight);
            let _ = settings.set_combined_friction(f.max(0.0));
        }
        if a.elasticity.is_finite() && b.elasticity.is_finite() {
            let e = mix(
                a.elasticity,
                a.elasticity_weight,
                b.elasticity,
                b.elasticity_weight,
            );
            let _ = settings.set_combined_restitution(e.clamp(0.0, 1.0));
        }
    }
}

/// The listener Jolt holds; it forwards to the shared [`PartGate`].
pub(crate) struct PartListener(pub(crate) Arc<PartGate>);

impl ContactListener for PartListener {
    fn contact_validate(&self, c: &ContactCandidate) -> ValidateResult {
        let a = self.0.part(c.body1, c.sub_shape1);
        let b = self.0.part(c.body2, c.sub_shape2);
        if !self.0.collides(a.group, b.group) {
            return ValidateResult::RejectContact;
        }
        if !(a.collide && b.collide) && !(a.touch && b.touch) {
            // Neither collides nor touches: nothing to report.
            return ValidateResult::RejectContact;
        }
        ValidateResult::AcceptContact
    }

    fn contact_added(&self, manifold: &ContactManifold, settings: &mut ContactSettings) {
        self.0.combine(manifold, settings);
    }

    fn contact_persisted(&self, manifold: &ContactManifold, settings: &mut ContactSettings) {
        self.0.combine(manifold, settings);
    }
}
