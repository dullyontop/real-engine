//! Scene queries: rays, shape casts and sphere overlaps, filtered by group mask.
//!
//! Jolt's query filter selects by object layer and can exclude one body; it has no
//! per-body callback. Group masks and sensors are therefore filtered here: when the
//! closest hit is rejected, the query continues past it from the hit point.

use glam::{Quat, Vec3};
use oxijolt::{CollideShape, QueryFilter as JQueryFilter, RayCast, Shape, ShapeCast};
use re_core::Diagnostic;
use re_types::EntityId;
use re_types::physics::{GroupMask, RayHit, ShapeDesc};

use crate::codes;
use crate::convert::{grv, gv, jq, jv, rv, unit_rotation};
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

/// How many rejected hits a ray or shape cast steps past before it gives up.
const MAX_SKIPS: usize = 64;

fn invalid(what: String) -> Diagnostic {
    Diagnostic::error(codes::INVALID_ARGUMENT, what)
        .with_hint("query positions must be finite and inside the world, directions non-zero")
}

impl Physics {
    /// Whether `filter` lets a query see the body `body`.
    fn sees(&self, body: oxijolt::BodyId, filter: &QueryFilter) -> Option<EntityId> {
        let id = *self.entity_of.get(&body)?;
        if filter.exclude == Some(id) {
            return None;
        }
        match self.bodies.get(&id) {
            Some(entry) => (entry.groups.intersects(filter.groups)
                && (filter.sensors || !entry.sensor))
                .then_some(id),
            // A character's inner body: groups were checked when it was registered.
            None => Some(id),
        }
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
        let mut start = 0.0f32;
        let mut skip = None;
        for _ in 0..MAX_SKIPS {
            let from = origin + dir * start;
            let ray = RayCast::new(rv(from), jv(dir * (max_dist - start)));
            let mut jf = JQueryFilter::new();
            if let Some(body) = skip {
                jf = jf.exclude_body(body);
            }
            let Some(hit) = self
                .world
                .cast_ray(&ray, &jf)
                .map_err(|e| invalid(format!("raycast refused: {e}")))?
            else {
                return Ok(None);
            };
            let distance = start + hit.distance;
            if let Some(entity) = self.sees(hit.body, filter) {
                return Ok(Some(RayHit {
                    entity,
                    point: origin + dir * distance,
                    normal: gv(hit.normal),
                    distance,
                }));
            }
            // Step past the rejected body; nudge forward when the ray starts inside it.
            start = if hit.distance > 0.0 {
                distance
            } else {
                distance + 1e-4
            };
            if start >= max_dist {
                return Ok(None);
            }
            skip = Some(hit.body);
        }
        Ok(None)
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
        if shapes::is_static_only(shape) {
            return Err(invalid(format!(
                "a {} cannot be cast; use a box, sphere, capsule or convex hull",
                shapes::kind_name(shape)
            )));
        }
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
        let mut start = 0.0f32;
        let mut skip = None;
        for _ in 0..MAX_SKIPS {
            let from = pos + dir * start;
            let cast = ShapeCast::new(&built.shape, rv(from), jq(rot), jv(dir * (length - start)));
            let mut jf = JQueryFilter::new();
            if let Some(body) = skip {
                jf = jf.exclude_body(body);
            }
            let Some(hit) = self
                .world
                .cast_shape(&cast, &jf)
                .map_err(|e| invalid(format!("shape cast refused: {e}")))?
            else {
                return Ok(None);
            };
            let distance = start + hit.distance;
            if let Some(entity) = self.sees(hit.body, filter) {
                return Ok(Some(RayHit {
                    entity,
                    point: grv(hit.point),
                    normal: gv(hit.normal),
                    distance,
                }));
            }
            start = if hit.distance > 0.0 {
                distance
            } else {
                distance + 1e-4
            };
            if start >= length {
                return Ok(None);
            }
            skip = Some(hit.body);
        }
        Ok(None)
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
                &CollideShape::new(&sphere, rv(center), jq(Quat::IDENTITY)),
                &JQueryFilter::new(),
            )
            .map_err(|e| invalid(format!("overlap query refused: {e}")))?;
        out.extend(hits.iter().filter_map(|h| self.sees(h.body, filter)));
        out.sort_unstable();
        out.dedup();
        Ok(())
    }
}
