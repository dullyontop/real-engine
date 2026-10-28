//! The probe: exact casts against a chosen set of bodies.
//!
//! Jolt's query filter leaves out one body at most, so a cast that must look past two
//! bodies it rejects, both around its start, cannot be asked of the world
//! ([`crate::query`]). The probe answers it. It gathers the bodies the query accepts
//! along the cast's path, with a capsule around the path that is wider than the cast
//! (and wider than the rounding of convex edges, which rays do not see), and casts
//! again in a private Jolt world that holds copies of only those bodies. A copy shares
//! its body's shape (Jolt shapes are counted references) and sits at the body's pose
//! in the same coordinates, so the answer is the one the world would give if it could
//! leave out every rejected body.
//!
//! The probe world is made on first use and kept, and holds bodies only while a cast
//! runs. It is never stepped, so it needs no threads.

use std::collections::BTreeMap;
use std::sync::PoisonError;

use glam::{Quat, Vec3};
use oxijolt::{
    BodyId, BodySettings, BroadPhaseLayer, CollideShape, CollisionLayers, ObjectLayer,
    PhysicsWorld, QueryFilter as JQueryFilter, RVec3, RayCast, Shape, ShapeCast, WorldSettings,
};
use re_core::Diagnostic;
use re_types::EntityId;

use crate::convert::{gv, jq, jv};
use crate::query::{QueryFilter, invalid, jolt_filter};
use crate::world::Physics;

/// The most copies the probe world holds at once; more bodies are cast in batches.
const CAPACITY: u32 = 1024;

/// How much wider than the cast the gathering capsule is, in metres: more than the
/// furthest a convex shape's rounded edges and corners (Jolt's default convex radius
/// is 0.05 m) lie inside the sharp faces a ray sees.
const MARGIN: f32 = 0.1;

/// The longest piece of path one gathering capsule covers, in metres, so a long
/// diagonal path is not gathered by one capsule whose bounds span half the world.
const PIECE: f32 = 50.0;

/// The probe's private world.
pub(crate) struct Probe {
    world: PhysicsWorld,
}

/// What the probe casts along the path.
pub(crate) enum Cast<'a> {
    /// A ray.
    Ray,
    /// A shape at rotation `rot`, reaching `reach` metres from its origin.
    Shape {
        shape: &'a Shape,
        rot: Quat,
        reach: f32,
    },
}

/// A hit the probe found.
pub(crate) struct ProbeHit {
    pub(crate) entity: EntityId,
    /// How far along the probed path, in metres.
    pub(crate) distance: f32,
    pub(crate) normal: Vec3,
    /// The contact point, in Jolt's coordinates (relative to the simulation origin).
    pub(crate) point: RVec3,
}

/// The rotation that turns +Y, the axis of a Jolt capsule, onto unit `dir`: the
/// shortest arc, built without trigonometry.
fn y_onto(dir: Vec3) -> Quat {
    let q = Quat::from_xyzw(dir.z, 0.0, -dir.x, 1.0 + dir.y);
    if q.length_squared() > 0.0 {
        q.normalize()
    } else {
        // `dir` is -Y: half a turn about X.
        Quat::from_xyzw(1.0, 0.0, 0.0, 0.0)
    }
}

impl Probe {
    fn new(physics: &Physics) -> Result<Probe, Diagnostic> {
        let mut layers = CollisionLayers::new(1);
        layers.add_object_layer(BroadPhaseLayer::new(0));
        let world = PhysicsWorld::new(
            WorldSettings::default()
                .layers(layers)
                .max_bodies(CAPACITY)
                .max_body_pairs(1)
                .max_contact_constraints(1)
                .temp_allocator_size(64 * 1024)
                .job_system(physics.jobs.clone()),
        )
        .map_err(|e| invalid(format!("the query probe could not be made: {e}")))?;
        Ok(Probe { world })
    }

    /// Casts against copies of `bodies` (each with the entity it stands for).
    fn cast_copies(
        &mut self,
        physics: &Physics,
        bodies: &[(BodyId, EntityId)],
        cast: &Cast<'_>,
        from: RVec3,
        motion: oxijolt::Vec3,
    ) -> Result<Option<ProbeHit>, Diagnostic> {
        let mut copies = Vec::with_capacity(bodies.len());
        let mut refused = None;
        for &(body, entity) in bodies {
            let (Some(shape), Ok(b)) =
                (physics.query_shape(body, entity), physics.world.body(body))
            else {
                continue;
            };
            let settings = BodySettings::new_static()
                .object_layer(ObjectLayer::new(0))
                .position(b.position())
                .rotation(b.rotation());
            match self.world.create_body(shape, &settings) {
                Ok(copy) => copies.push((copy, entity)),
                Err(e) => {
                    refused = Some(invalid(format!(
                        "the query probe refused a copy of entity {entity}: {e}"
                    )));
                    break;
                }
            }
        }
        let hit = match refused {
            Some(d) => Err(d),
            None => self.closest(cast, from, motion),
        };
        for &(copy, _) in &copies {
            // The copy was just made and nothing holds it.
            let _ = self.world.remove_body(copy);
        }
        Ok(hit?.and_then(|(copy, distance, normal, point)| {
            let entity = copies.iter().find(|(c, _)| *c == copy)?.1;
            Some(ProbeHit {
                entity,
                distance,
                normal,
                point,
            })
        }))
    }

    /// The closest hit of `cast` in the probe world.
    fn closest(
        &self,
        cast: &Cast<'_>,
        from: RVec3,
        motion: oxijolt::Vec3,
    ) -> Result<Option<(BodyId, f32, Vec3, RVec3)>, Diagnostic> {
        let all = JQueryFilter::new();
        match cast {
            Cast::Ray => {
                let ray = RayCast::new(from, motion);
                let hit = self
                    .world
                    .cast_ray(&ray, &all)
                    .map_err(|e| invalid(format!("raycast refused: {e}")))?;
                Ok(hit.map(|h| (h.body, h.distance, gv(h.normal), ray.point_at(h.fraction))))
            }
            Cast::Shape { shape, rot, .. } => {
                let hit = self
                    .world
                    .cast_shape(&ShapeCast::new(shape, from, jq(*rot), motion), &all)
                    .map_err(|e| invalid(format!("shape cast refused: {e}")))?;
                Ok(hit.map(|h| (h.body, h.distance, gv(h.normal), h.point)))
            }
        }
    }
}

impl Physics {
    /// The closest hit of `cast` from `from` along unit `dir` for `length` metres among
    /// the bodies `filter` accepts, whatever bodies it rejects on the way.
    pub(crate) fn probe(
        &self,
        cast: &Cast<'_>,
        from: Vec3,
        dir: Vec3,
        length: f32,
        filter: &QueryFilter,
    ) -> Result<Option<ProbeHit>, Diagnostic> {
        let reach = match cast {
            Cast::Ray => 0.0,
            Cast::Shape { reach, .. } => *reach,
        };
        let bodies = self.gather(from, dir, length, reach + MARGIN, filter)?;
        if bodies.is_empty() {
            return Ok(None);
        }
        let mut slot = self.probe.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(Probe::new(self)?);
        }
        let Some(probe) = slot.as_mut() else {
            return Ok(None);
        };
        let (start, motion) = (self.jolt_point(from), jv(dir * length));
        let mut best: Option<ProbeHit> = None;
        for batch in bodies.chunks(CAPACITY as usize) {
            let Some(hit) = probe.cast_copies(self, batch, cast, start, motion)? else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|b| (hit.distance, hit.entity) < (b.distance, b.entity))
            {
                best = Some(hit);
            }
        }
        Ok(best)
    }

    /// The bodies `filter` accepts that touch a capsule of `radius` around the path
    /// from `from` along unit `dir` for `length` metres, in body id order.
    fn gather(
        &self,
        from: Vec3,
        dir: Vec3,
        length: f32,
        radius: f32,
        filter: &QueryFilter,
    ) -> Result<Vec<(BodyId, EntityId)>, Diagnostic> {
        let pieces = (length / PIECE).ceil().max(1.0);
        let piece = length / pieces;
        let shape = if piece * 0.5 > 1e-4 {
            Shape::new_capsule(piece * 0.5, radius)
        } else {
            Shape::new_sphere(radius + piece * 0.5)
        }
        .map_err(|e| invalid(format!("a query along {length} m reaching {radius} m: {e}")))?;
        let rot = jq(y_onto(dir));
        let layers = jolt_filter(self.query_layers(filter), None);
        let mut found = BTreeMap::new();
        // Whole pieces: `pieces` is at least 1 and at most `length / PIECE + 1`.
        for i in 0..pieces as u32 {
            let center = from + dir * (piece * (i as f32 + 0.5));
            let hits = self
                .world
                .collide_shape(
                    &CollideShape::new(&shape, self.jolt_point(center), rot)
                        .collide_with_back_faces(true),
                    &layers,
                )
                .map_err(|e| invalid(format!("query refused: {e}")))?;
            for h in hits {
                if let Some(entity) = self.sees(h.body, filter) {
                    found.insert(h.body.to_raw(), (h.body, entity));
                }
            }
        }
        Ok(found.into_values().collect())
    }
}
