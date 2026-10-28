//! Scene queries: rays, shape casts and sphere overlaps, filtered by group mask.
//!
//! Jolt's query filter selects bodies by object layer and can leave out one body; it
//! has no per-body callback. Sensors have object layers of their own, so a query that
//! leaves sensors out (the default) never meets one. Group masks and the excluded
//! entity are filtered here: when the closest hit is rejected, the query goes on from
//! it with that body left out. A cast that starts inside two rejected bodies can leave
//! out neither, since each hides the other at distance 0; the probe answers it
//! ([`crate::probe`]), from copies of only the bodies the query accepts.

use glam::{Quat, Vec3};
use oxijolt::{
    BodyId, CollideShape, ObjectLayer, QueryFilter as JQueryFilter, RayCast, Shape, ShapeCast,
};
use re_core::Diagnostic;
use re_types::EntityId;
use re_types::physics::{GroupMask, RayHit, ShapeDesc};

use crate::codes;
use crate::convert::{gv, jq, jv, unit_rotation};
use crate::probe::Cast;
use crate::shapes;
use crate::world::Physics;

/// Which bodies a query sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryFilter {
    /// Only bodies whose groups intersect this mask are seen. Default: every group.
    pub groups: GroupMask,
    /// An entity to ignore, typically the querying actor itself.
    pub exclude: Option<EntityId>,
    /// Whether sensors are seen. Default `false`.
    pub sensors: bool,
}

impl Default for QueryFilter {
    fn default() -> Self {
        QueryFilter {
            groups: GroupMask::ALL,
            exclude: None,
            sensors: false,
        }
    }
}

impl QueryFilter {
    /// The default filter (all groups, no exclusion, no sensors) ignoring `id`.
    pub fn excluding(id: EntityId) -> QueryFilter {
        QueryFilter {
            exclude: Some(id),
            ..QueryFilter::default()
        }
    }
}

/// How many rejected hits a ray or shape cast steps past before the probe answers it.
const MAX_SKIPS: usize = 64;

pub(crate) fn invalid(what: String) -> Diagnostic {
    Diagnostic::error(codes::INVALID_ARGUMENT, what)
        .with_hint("query positions must be finite and inside the world, directions non-zero")
}

/// Jolt's filter for the object layers `layers` (all when `None`) without `skip`.
pub(crate) fn jolt_filter(
    layers: Option<&[ObjectLayer]>,
    skip: Option<BodyId>,
) -> JQueryFilter<'_> {
    let mut filter = JQueryFilter::new();
    if let Some(layers) = layers {
        filter = filter.object_layers(layers);
    }
    if let Some(body) = skip {
        filter = filter.exclude_body(body);
    }
    filter
}

/// How a cast that steps past rejected bodies ends.
enum Stepped<H> {
    /// It hit `entity` this far along, with the cast's own details.
    Hit(EntityId, f32, H),
    /// It hit nothing it accepts.
    Miss,
    /// It stopped this far along, inside two rejected bodies (or after too many).
    Blocked(f32),
}

impl Physics {
    /// Whether `filter` lets a query see the body `body`, and its entity if so.
    pub(crate) fn sees(&self, body: BodyId, filter: &QueryFilter) -> Option<EntityId> {
        let id = *self.entity_of.get(&body)?;
        if filter.exclude == Some(id) {
            return None;
        }
        let seen = match self.bodies.get(&id) {
            Some(entry) => {
                entry.groups.intersects(filter.groups) && (filter.sensors || !entry.sensor)
            }
            // A character's inner body stands for the character and a ragdoll part for
            // the ragdoll; both take their owner's groups.
            None => self
                .characters
                .get(&id)
                .map(|ch| ch.groups)
                .or_else(|| self.ragdolls.get(&id).map(|r| r.groups))
                .is_some_and(|groups| groups.intersects(filter.groups)),
        };
        seen.then_some(id)
    }

    /// The object layers a query with `filter` looks at: all of them, or all but the
    /// sensors' when it leaves sensors out.
    pub(crate) fn query_layers(&self, filter: &QueryFilter) -> Option<&[ObjectLayer]> {
        (!filter.sensors).then_some(self.solid_layers.as_slice())
    }

    /// The body of the excluded entity that a cast leaves out from the start (a
    /// character's inner body), so a query from inside the actor's own body does not
    /// spend its one exclusion on it.
    fn excluded_body(&self, filter: &QueryFilter) -> Option<BodyId> {
        let id = filter.exclude?;
        match self.bodies.get(&id) {
            Some(entry) => Some(entry.id),
            None => self.characters.get(&id)?.inner,
        }
    }

    /// Runs `cast` (the closest hit along the path from `start` metres to its end,
    /// given the filter of layers and the one body to leave out) and steps past the
    /// hits `filter` rejects.
    fn step_past<H>(
        &self,
        length: f32,
        filter: &QueryFilter,
        mut cast: impl FnMut(f32, &JQueryFilter<'_>) -> Result<Option<(BodyId, f32, H)>, Diagnostic>,
    ) -> Result<Stepped<H>, Diagnostic> {
        let layers = self.query_layers(filter);
        let mut start = 0.0f32;
        let mut skip = self.excluded_body(filter);
        for _ in 0..MAX_SKIPS {
            let Some((body, along, hit)) = cast(start, &jolt_filter(layers, skip))? else {
                return Ok(Stepped::Miss);
            };
            let distance = start + along;
            if let Some(entity) = self.sees(body, filter) {
                return Ok(Stepped::Hit(entity, distance, hit));
            }
            if along > 0.0 {
                // Step to the rejected body and go on without it.
                start = distance;
                if start >= length {
                    return Ok(Stepped::Miss);
                }
            } else if skip.is_some() {
                // The cast starts inside this body and the one already left out.
                return Ok(Stepped::Blocked(start));
            }
            skip = Some(body);
        }
        Ok(Stepped::Blocked(start))
    }

    /// The closest body hit by a ray from `origin` along `dir` (any length, normalised
    /// here) within `max_dist` metres.
    pub fn raycast(
        &self,
        origin: Vec3,
        dir: Vec3,
        max_dist: f32,
        filter: &QueryFilter,
    ) -> Result<Option<RayHit>, Diagnostic> {
        let dir = dir.normalize_or_zero();
        if !origin.is_finite() || dir == Vec3::ZERO || !(max_dist.is_finite() && max_dist > 0.0) {
            return Err(invalid(format!(
                "raycast from {origin} along {dir} for {max_dist} m is not a valid ray"
            )));
        }
        let found = self.step_past(max_dist, filter, |start, jf| {
            let ray = RayCast::new(
                self.jolt_point(origin + dir * start),
                jv(dir * (max_dist - start)),
            );
            let hit = self
                .world
                .cast_ray(&ray, jf)
                .map_err(|e| invalid(format!("raycast refused: {e}")))?;
            Ok(hit.map(|h| (h.body, h.distance, gv(h.normal))))
        })?;
        let hit = match found {
            Stepped::Hit(entity, distance, normal) => Some((entity, distance, normal)),
            Stepped::Miss => None,
            Stepped::Blocked(start) => self
                .probe(
                    &Cast::Ray,
                    origin + dir * start,
                    dir,
                    max_dist - start,
                    filter,
                )?
                .map(|h| (h.entity, start + h.distance, h.normal)),
        };
        Ok(hit.map(|(entity, distance, normal)| RayHit {
            entity,
            point: origin + dir * distance,
            normal,
            distance,
        }))
    }

    /// The first body a convex `shape` at `pos`/`rot` hits when moved by `motion`
    /// (direction and length). `distance` in the result is how far the shape moved.
    ///
    /// Triangle meshes and heightfields cannot be cast (`RE5016`).
    pub fn shape_cast(
        &self,
        shape: &ShapeDesc,
        pos: Vec3,
        rot: Quat,
        motion: Vec3,
        filter: &QueryFilter,
    ) -> Result<Option<RayHit>, Diagnostic> {
        let Some(reach) = shapes::reach(shape) else {
            return Err(invalid(format!(
                "a {} cannot be cast; use a box, sphere, capsule or convex hull",
                shapes::kind_name(shape)
            )));
        };
        let rot = unit_rotation(rot)
            .ok_or_else(|| invalid("shape cast rotation is zero or not finite".into()))?;
        let length = motion.length();
        if !pos.is_finite() || !(length.is_finite() && length > 0.0) {
            return Err(invalid(format!(
                "shape cast from {pos} by {motion} is not a valid cast"
            )));
        }
        let dir = motion / length;
        let built = shapes::build(shape)?;
        let found = self.step_past(length, filter, |start, jf| {
            let cast = ShapeCast::new(
                &built.shape,
                self.jolt_point(pos + dir * start),
                jq(rot),
                jv(dir * (length - start)),
            );
            let hit = self
                .world
                .cast_shape(&cast, jf)
                .map_err(|e| invalid(format!("shape cast refused: {e}")))?;
            Ok(hit.map(|h| (h.body, h.distance, (h.point, gv(h.normal)))))
        })?;
        let hit = match found {
            Stepped::Hit(entity, distance, contact) => Some((entity, distance, contact)),
            Stepped::Miss => None,
            Stepped::Blocked(start) => {
                let cast = Cast::Shape {
                    shape: &built.shape,
                    rot,
                    reach,
                };
                self.probe(&cast, pos + dir * start, dir, length - start, filter)?
                    .map(|h| (h.entity, start + h.distance, (h.point, h.normal)))
            }
        };
        Ok(hit.map(|(entity, distance, (point, normal))| RayHit {
            entity,
            point: self.world_point(point),
            normal,
            distance,
        }))
    }

    /// Every entity whose body overlaps the sphere at `center` of `radius`, written into
    /// `out` (cleared first) sorted by entity, each once.
    pub fn overlap_sphere(
        &self,
        center: Vec3,
        radius: f32,
        filter: &QueryFilter,
        out: &mut Vec<EntityId>,
    ) -> Result<(), Diagnostic> {
        out.clear();
        if !center.is_finite() || !(radius.is_finite() && radius > 0.0) {
            return Err(invalid(format!(
                "sphere at {center} with radius {radius} is not a valid overlap query"
            )));
        }
        let sphere = Shape::new_sphere(radius)
            .map_err(|e| invalid(format!("sphere radius {radius}: {e}")))?;
        let hits = self
            .world
            .collide_shape(
                &CollideShape::new(&sphere, self.jolt_point(center), jq(Quat::IDENTITY)),
                &jolt_filter(self.query_layers(filter), None),
            )
            .map_err(|e| invalid(format!("overlap query refused: {e}")))?;
        out.extend(hits.iter().filter_map(|h| self.sees(h.body, filter)));
        out.sort_unstable();
        out.dedup();
        Ok(())
    }
}
