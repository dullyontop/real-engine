//! Queries over parts: rays, shape casts and overlaps.
//!
//! They read the parts' bounding-volume tree and exact primitive geometry, not
//! Jolt, so a filter can accept or skip each part on its own (an assembly may
//! mix groups and flags). A query sees a part when the part collides or can be
//! queried, and the filter accepts it. Answers are deterministic: the nearest
//! hit (the smaller entity on a tie), or overlaps sorted by entity.

use glam::Vec3;
use re_types::EntityId;

use super::PartView;
use super::geom::{self, PartGeom, Placed, Pose};
use super::tree::Aabb;
use crate::world::Physics;

/// Decides, part by part, what a query sees.
pub trait PartFilter {
    /// Whether the query sees `part`.
    fn accept(&mut self, part: &PartView) -> bool;
}

impl<F: FnMut(&PartView) -> bool> PartFilter for F {
    fn accept(&mut self, part: &PartView) -> bool {
        self(part)
    }
}

/// The nearest part a ray or cast met.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartHit {
    /// The part.
    pub part: EntityId,
    /// Where the ray or cast shape met it.
    pub position: Vec3,
    /// The part's outward surface normal there.
    pub normal: Vec3,
    /// How far the ray or shape travelled.
    pub distance: f32,
}

/// How close a cast shape comes before it counts as touching.
const CAST_TOLERANCE: f64 = 1e-4;
/// How far an overlap must go before two solids count as overlapping, so faces
/// that only touch do not.
const OVERLAP_SLACK: f64 = 1e-3;

impl Physics {
    fn view_of(&self, part: EntityId) -> Option<PartView> {
        let (key, child) = *self.parts.slots.get(&part)?;
        let asm = self.parts.assemblies.get(&key)?;
        Some(asm.parts[child as usize].view(key))
    }

    /// Casts a ray from `origin` along `direction` (whose length is the
    /// ray's reach). Parts the ray starts inside are not hit.
    pub fn raycast_parts(
        &self,
        origin: Vec3,
        direction: Vec3,
        filter: &mut dyn PartFilter,
    ) -> Option<PartHit> {
        let reach = direction.length();
        if !(origin.is_finite() && direction.is_finite()) || reach <= 0.0 {
            return None;
        }
        let dir = direction / reach;
        let mut best: Option<PartHit> = None;
        self.parts.tree.ray(origin, dir, reach, |id, max_t| {
            let Some(view) = self.view_of(id) else {
                return max_t;
            };
            if !view.can_query {
                return max_t;
            }
            let local_o = view.world.inv_point(origin);
            let local_d = view.world.rot.inverse() * dir;
            let Some((t, n)) = view.geom.sanitized().ray_entry(local_o, local_d, max_t) else {
                return max_t;
            };
            let closer = match &best {
                None => true,
                Some(b) => t < b.distance || (t == b.distance && id < b.part),
            };
            if !closer || !filter.accept(&view) {
                return max_t;
            }
            best = Some(PartHit {
                part: id,
                position: origin + dir * t,
                normal: (view.world.rot * n).normalize_or_zero(),
                distance: t,
            });
            t
        });
        best
    }

    /// Sweeps a solid of `geom` from `from` along `direction` (whose length is
    /// the reach), without turning. Parts it starts in are not hit.
    pub fn shapecast_parts(
        &self,
        geom: PartGeom,
        from: Pose,
        direction: Vec3,
        filter: &mut dyn PartFilter,
    ) -> Option<PartHit> {
        let reach = direction.length();
        if !(from.pos.is_finite() && from.rot.is_finite() && direction.is_finite())
            || reach <= 0.0
            || !geom.is_valid()
        {
            return None;
        }
        let from = Pose::new(from.pos, from.rot.normalize());
        let moving = Placed::new(geom.sanitized(), &from);
        let (min0, max0) = geom.sanitized().world_aabb(&from);
        let (min1, max1) = (min0 + direction, max0 + direction);
        let sweep = Aabb::new(min0.min(min1), max0.max(max1));
        let mut candidates: Vec<EntityId> = Vec::new();
        self.parts.tree.query(&sweep, |id| candidates.push(id));
        candidates.sort_unstable();
        let motion = direction.as_dvec3();
        let mut best: Option<(f64, PartHit)> = None;
        for id in candidates {
            let Some(view) = self.view_of(id) else {
                continue;
            };
            if !view.can_query {
                continue;
            }
            let target = Placed::new(view.geom.sanitized(), &view.world);
            let Some((frac, point, normal)) =
                geom::shape_cast(&moving, &target, motion, CAST_TOLERANCE)
            else {
                continue;
            };
            if best.as_ref().is_some_and(|(f, _)| frac >= *f) || !filter.accept(&view) {
                continue;
            }
            best = Some((
                frac,
                PartHit {
                    part: id,
                    position: point.as_vec3(),
                    normal: normal.as_vec3().normalize_or_zero(),
                    distance: (frac * motion.length()) as f32,
                },
            ));
        }
        best.map(|(_, h)| h)
    }

    fn overlap_candidates(&self, aabb: Aabb) -> Vec<PartView> {
        let mut ids: Vec<EntityId> = Vec::new();
        self.parts.tree.query(&aabb, |id| ids.push(id));
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .filter_map(|id| self.view_of(id))
            .filter(|v| v.can_query)
            .collect()
    }

    fn collect(
        views: Vec<PartView>,
        mut test: impl FnMut(&PartView) -> bool,
        filter: &mut dyn PartFilter,
        max: usize,
    ) -> Vec<EntityId> {
        let mut out = Vec::new();
        for v in views {
            if out.len() >= max {
                break;
            }
            if test(&v) && filter.accept(&v) {
                out.push(v.id);
            }
        }
        out
    }

    /// The parts whose bounding boxes (their whole size, whatever their shape)
    /// overlap the box of `size` at `pose`, sorted by entity, at most `max`.
    pub fn parts_in_box(
        &self,
        pose: Pose,
        size: Vec3,
        filter: &mut dyn PartFilter,
        max: usize,
    ) -> Vec<EntityId> {
        if !(pose.pos.is_finite() && pose.rot.is_finite() && size.is_finite()) {
            return Vec::new();
        }
        let pose = Pose::new(pose.pos, pose.rot.normalize());
        let half = size.abs() * 0.5;
        let (min, max_b) = PartGeom::new(geom::PartShape::Block, size.abs()).box_aabb(&pose);
        let views = self.overlap_candidates(Aabb::new(min, max_b));
        Self::collect(
            views,
            |v| geom::obb_overlap(&v.world, v.geom.half(), &pose, half),
            filter,
            max,
        )
    }

    /// The parts whose bounding boxes overlap the sphere of `radius` at
    /// `center`, sorted by entity, at most `max`.
    pub fn parts_in_radius(
        &self,
        center: Vec3,
        radius: f32,
        filter: &mut dyn PartFilter,
        max: usize,
    ) -> Vec<EntityId> {
        if !(center.is_finite() && radius.is_finite()) || radius < 0.0 {
            return Vec::new();
        }
        let r = Vec3::splat(radius);
        let views = self.overlap_candidates(Aabb::new(center - r, center + r));
        Self::collect(
            views,
            |v| geom::obb_sphere_overlap(&v.world, v.geom.half(), center, radius),
            filter,
            max,
        )
    }

    /// The parts whose solids overlap the solid of `geom` at `pose` (faces that
    /// only touch do not count), sorted by entity, at most `max`.
    pub fn parts_in_solid(
        &self,
        geom: PartGeom,
        pose: Pose,
        filter: &mut dyn PartFilter,
        max: usize,
    ) -> Vec<EntityId> {
        if !(pose.pos.is_finite() && pose.rot.is_finite()) || !geom.is_valid() {
            return Vec::new();
        }
        let pose = Pose::new(pose.pos, pose.rot.normalize());
        let g = geom.sanitized();
        let (min, max_b) = g.world_aabb(&pose);
        let views = self.overlap_candidates(Aabb::new(min, max_b));
        let mut query = Placed::new(g, &pose);
        let shrink = OVERLAP_SLACK.min(f64::from(g.size.min_element()) * 0.25);
        query.margin = -shrink;
        Self::collect(
            views,
            |v| {
                let target = Placed::new(v.geom.sanitized(), &v.world);
                geom::overlaps(&query, &target)
            },
            filter,
            max,
        )
    }

    /// Every part's view, sorted by entity (for brute-force checks and tools).
    pub fn all_parts(&self) -> Vec<PartView> {
        let mut v: Vec<PartView> = self
            .parts
            .assemblies
            .iter()
            .flat_map(|(k, a)| a.parts.iter().map(move |p| p.view(*k)))
            .collect();
        v.sort_by_key(|p| p.id);
        v
    }
}
