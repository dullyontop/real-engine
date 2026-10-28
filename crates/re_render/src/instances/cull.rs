//! Culling an instance set's clusters: each part's cluster boxes in the render
//! origin's space, kept between frames, and the part's distance range.

use glam::{Mat4, Vec3};
use re_types::mesh::Aabb;

use crate::frame;

/// Placements whose roots lie together, as the renderer keeps them: a range of
/// the set's buffer, the box around their roots and their largest scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Cluster {
    pub(super) first: u32,
    pub(super) count: u32,
    pub(super) bounds: Aabb,
    pub(super) max_scale: f32,
}

/// The eight corners of `b`.
fn corners(b: &Aabb) -> impl Iterator<Item = Vec3> + '_ {
    (0..8).map(|i| {
        Vec3::new(
            if i & 1 == 0 { b.min.x } else { b.max.x },
            if i & 2 == 0 { b.min.y } else { b.max.y },
            if i & 4 == 0 { b.min.z } else { b.max.z },
        )
    })
}

/// How far a part reaches from a placement's root before the placement's
/// scale: the farthest corner of its mesh's bounds `aabb`, moved by `part`.
pub(super) fn reach(aabb: &Aabb, part: &Mat4) -> f32 {
    corners(aabb)
        .map(|p| part.transform_point3(p).length())
        .fold(0.0, f32::max)
}

/// One part's cluster boxes in the render origin's space, kept while the
/// set's transform, the part's transform and its mesh's bounds stay the same.
#[derive(Debug)]
pub(super) struct PartBoxes {
    /// What the boxes were made for: the set's transform, the part's and the
    /// mesh's bounds.
    key: Option<(Mat4, Mat4, Aabb)>,
    /// The box around every cluster's.
    pub(super) whole: Aabb,
    /// Each cluster's.
    pub(super) clusters: Vec<Aabb>,
}

impl Default for PartBoxes {
    fn default() -> Self {
        PartBoxes {
            key: None,
            whole: Aabb::new(Vec3::ZERO, Vec3::ZERO),
            clusters: Vec::new(),
        }
    }
}

impl PartBoxes {
    /// The boxes of `clusters` (at least one) for a part whose mesh has
    /// bounds `aabb`, placed by `part` in a set placed by `set`: each
    /// cluster's roots grown by the part's reach times the cluster's largest
    /// scale, then moved by `set`.
    pub(super) fn update(
        &mut self,
        clusters: &[Cluster],
        set: &Mat4,
        part: &Mat4,
        aabb: &Aabb,
    ) -> &PartBoxes {
        let key = (*set, *part, *aabb);
        if self.key != Some(key) {
            let reach = reach(aabb, part);
            self.clusters.clear();
            self.clusters.extend(clusters.iter().map(|c| {
                let grow = Vec3::splat(reach * c.max_scale);
                frame::transform_aabb(&Aabb::new(c.bounds.min - grow, c.bounds.max + grow), set)
            }));
            if let Some(first) = self.clusters.first() {
                self.whole = self.clusters[1..]
                    .iter()
                    .fold(*first, |whole, b| whole.union(b));
            }
            self.key = Some(key);
        }
        self
    }
}

/// A part's distance range from the camera (`InstancePart`'s `min_distance`
/// and `max_distance`), as squared distances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Distances {
    near: f32,
    far: Option<f32>,
}

impl Distances {
    /// From `min` to `max` metres: a negative or NaN `min` is 0, and a `max`
    /// of 0 (or below, or NaN) is no limit.
    pub(super) fn new(min: f32, max: f32) -> Distances {
        let min = if min.is_nan() { 0.0 } else { min.max(0.0) };
        Distances {
            near: min * min,
            far: (max > 0.0).then_some(max * max),
        }
    }

    /// True when the nearest point of the cluster box `b` is in the range,
    /// seen from `eye`.
    pub(super) fn holds(&self, b: &Aabb, eye: Vec3) -> bool {
        let d = nearest(b, eye);
        d >= self.near && self.far.is_none_or(|far| d < far)
    }

    /// False when no cluster box inside `whole` can be in the range, seen
    /// from `eye`.
    pub(super) fn may_hold(&self, whole: &Aabb, eye: Vec3) -> bool {
        farthest(whole, eye) >= self.near && self.far.is_none_or(|far| nearest(whole, eye) < far)
    }
}

/// The squared distance from `p` to the nearest point of `b`.
fn nearest(b: &Aabb, p: Vec3) -> f32 {
    p.clamp(b.min, b.max).distance_squared(p)
}

/// The squared distance from `p` to the farthest point of `b`.
fn farthest(b: &Aabb, p: Vec3) -> f32 {
    (p - b.min).abs().max((b.max - p).abs()).length_squared()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> Aabb {
        Aabb::new(Vec3::splat(-0.5), Vec3::splat(0.5))
    }

    #[test]
    fn a_part_reaches_to_its_farthest_corner() {
        assert!((reach(&unit(), &Mat4::IDENTITY) - 0.75f32.sqrt()).abs() < 1e-6);
        // A trunk: 0.5 x 3.2 x 0.5, standing on the root.
        let trunk = Mat4::from_translation(Vec3::new(0.0, 1.6, 0.0))
            * Mat4::from_scale(Vec3::new(0.5, 3.2, 0.5));
        let top_corner = Vec3::new(0.25, 3.2, 0.25).length();
        assert!((reach(&unit(), &trunk) - top_corner).abs() < 1e-6);
    }

    #[test]
    fn boxes_grow_by_the_reach_and_move_with_the_set_until_an_input_changes() {
        let clusters = [
            Cluster {
                first: 0,
                count: 3,
                bounds: Aabb::new(Vec3::ZERO, Vec3::new(10.0, 0.0, 10.0)),
                max_scale: 2.0,
            },
            Cluster {
                first: 3,
                count: 1,
                bounds: Aabb::new(Vec3::new(64.0, 1.0, 0.0), Vec3::new(64.0, 1.0, 0.0)),
                max_scale: 1.0,
            },
        ];
        let set = Mat4::from_translation(Vec3::new(256.0, 0.0, -512.0));
        let part = Mat4::from_translation(Vec3::new(0.0, 0.5, 0.0));
        let mut boxes = PartBoxes::default();
        boxes.update(&clusters, &set, &part, &unit());
        // The part reaches the corner (0.5, 1, 0.5) of the moved cube.
        let r = 1.5f32.sqrt();
        let first = Aabb::new(
            Vec3::new(256.0 - 2.0 * r, -2.0 * r, -512.0 - 2.0 * r),
            Vec3::new(266.0 + 2.0 * r, 2.0 * r, -502.0 + 2.0 * r),
        );
        assert!(boxes.clusters[0].min.abs_diff_eq(first.min, 1e-4));
        assert!(boxes.clusters[0].max.abs_diff_eq(first.max, 1e-4));
        assert!((boxes.clusters[1].max.x - (320.0 + r)).abs() < 1e-4);
        assert_eq!(boxes.whole, boxes.clusters[0].union(&boxes.clusters[1]));

        // The same inputs keep the boxes; a moved set moves them.
        let kept = boxes.clusters.clone();
        boxes.update(&clusters, &set, &part, &unit());
        assert_eq!(boxes.clusters, kept);
        let moved = Mat4::from_translation(Vec3::new(0.0, 10.0, 0.0)) * set;
        boxes.update(&clusters, &moved, &part, &unit());
        assert!((boxes.clusters[0].min.y - kept[0].min.y - 10.0).abs() < 1e-4);
    }

    #[test]
    fn distance_ranges_take_the_nearest_point_of_a_cluster() {
        let b = Aabb::new(Vec3::new(100.0, 0.0, -10.0), Vec3::new(120.0, 5.0, 10.0));
        let eye = Vec3::ZERO;
        assert!(Distances::new(0.0, 0.0).holds(&b, eye), "no limit");
        assert!(Distances::new(0.0, 101.0).holds(&b, eye));
        assert!(
            !Distances::new(0.0, 100.0).holds(&b, eye),
            "nearer than max"
        );
        assert!(Distances::new(100.0, 0.0).holds(&b, eye), "at least min");
        assert!(!Distances::new(100.5, 0.0).holds(&b, eye));
        // A camera inside the box is at distance 0.
        assert!(Distances::new(0.0, 1.0).holds(&b, Vec3::new(110.0, 1.0, 0.0)));
        // Unusable values: NaN or negative min is 0, NaN or 0 max no limit.
        assert_eq!(
            Distances::new(f32::NAN, f32::NAN),
            Distances::new(-3.0, 0.0)
        );
        assert!(!Distances::new(f32::INFINITY, 0.0).holds(&b, eye));

        // The whole set's box rules out ranges no cluster inside can meet.
        assert!(!Distances::new(0.0, 100.0).may_hold(&b, eye));
        assert!(Distances::new(0.0, 100.5).may_hold(&b, eye));
        let far_corner = Vec3::new(120.0, 5.0, 10.0).length();
        assert!(Distances::new(far_corner - 0.01, 0.0).may_hold(&b, eye));
        assert!(!Distances::new(far_corner + 0.01, 0.0).may_hold(&b, eye));
    }
}
