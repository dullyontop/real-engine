//! Per-frame GPU data and the CPU-side light, culling and shadow set-up.
//!
//! The `#[repr(C)]` structs here must match the WGSL declarations in
//! `shaders/common.wgsl` and `shaders/forward.wgsl`.

use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Mat4, Vec3, Vec4};
use re_core::Diagnostic;
use re_types::mesh::Aabb;
use re_types::render::{Camera, Light, LightKind, SunShadows};

use crate::codes;

/// The most point lights one frame draws.
pub const MAX_POINT_LIGHTS: usize = 8;

/// The most shadow cascades (ADR-0038).
pub const MAX_CASCADES: usize = 4;

/// `struct PointLight` in common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub(crate) struct PointLightRaw {
    pub position_range: [f32; 4],
    pub color: [f32; 4],
}

/// `struct SpotLight` in common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub(crate) struct SpotLightRaw {
    pub position_range: [f32; 4],
    /// w: the cosine where the fade ends (full light inside).
    pub color: [f32; 4],
    /// w: the cosine of the half angle (no light outside).
    pub direction: [f32; 4],
}

/// `struct Frame` in common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct FrameUniform {
    pub view_proj: [[f32; 4]; 4],
    /// World to each cascade's shadow clip space.
    pub cascades: [[[f32; 4]; 4]; MAX_CASCADES],
    pub inv_view_proj: [[f32; 4]; 4],
    pub camera_pos: [f32; 4],
    /// xyz: the camera's forward direction.
    pub camera_forward: [f32; 4],
    pub sun_dir: [f32; 4],
    pub sun_color: [f32; 4],
    pub ambient_sky: [f32; 4],
    pub ambient_ground: [f32; 4],
    pub params: [f32; 4],
    /// The view depth where each cascade ends.
    pub cascade_far: [f32; 4],
    /// Each cascade's normal offset, in world units.
    pub cascade_offset: [f32; 4],
    /// x: cascade count (0 without shadows), y: blend fraction.
    pub shadow_params: [f32; 4],
    /// rgb: fog colour, w: fog start.
    pub fog_color: [f32; 4],
    /// x: fog end, y: 1 with fog, z: spot light count.
    pub fog_params: [f32; 4],
    /// rgb: zenith, w: 1 with a sky.
    pub sky_zenith: [f32; 4],
    pub sky_horizon: [f32; 4],
    pub sky_ground: [f32; 4],
    /// xyz: towards the sun disc, w: 1 with a disc.
    pub sky_sun_dir: [f32; 4],
    pub sky_sun_color: [f32; 4],
    pub points: [PointLightRaw; MAX_POINT_LIGHTS],
    pub spots: [SpotLightRaw; crate::extras::MAX_SPOT_LIGHTS],
}

/// `struct Instance` in common.wgsl: one drawn instance, 128 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct InstanceRaw {
    pub model: [[f32; 4]; 4],
    pub normal: [[f32; 4]; 3],
    pub flags: [f32; 4],
}

impl InstanceRaw {
    pub fn new(world: Mat4, receives_shadows: bool, selected: bool) -> InstanceRaw {
        let linear = Mat3::from_mat4(world);
        // The inverse transpose keeps normals perpendicular under non-uniform scale.
        let normal = if linear.determinant().abs() > 1e-12 {
            linear.inverse().transpose()
        } else {
            linear
        };
        InstanceRaw {
            model: world.to_cols_array_2d(),
            normal: [
                normal.x_axis.extend(0.0).to_array(),
                normal.y_axis.extend(0.0).to_array(),
                normal.z_axis.extend(0.0).to_array(),
            ],
            flags: [
                f32::from(u8::from(receives_shadows)),
                f32::from(u8::from(selected)),
                0.0,
                0.0,
            ],
        }
    }

    /// Sets a skinned instance's first joint palette matrix (`flags.w`, read by
    /// skin.wgsl). The renderer keeps palettes below 2^24 matrices, so it is exact.
    pub fn with_palette(mut self, palette: Option<u32>) -> InstanceRaw {
        if let Some(first) = palette {
            self.flags[3] = first as f32;
        }
        self
    }
}

/// `struct Material` in forward.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct MaterialUniform {
    pub base_color: [f32; 4],
    pub emissive: [f32; 4],
    pub params: [f32; 4],
    pub flags: [f32; 4],
}

/// One vertex of a debug line.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct LineVertex {
    pub position: [f32; 3],
    pub color: [f32; 4],
}

/// The sun of a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sun {
    /// The direction the light travels, normalised.
    pub direction: Vec3,
    /// Colour times intensity times exposure.
    pub radiance: Vec3,
    /// Whether it casts shadows.
    pub shadows: bool,
}

/// The lights a frame draws.
#[derive(Debug, Default)]
pub(crate) struct LightSetup {
    pub sun: Option<Sun>,
    pub points: Vec<PointLightRaw>,
    pub warnings: Vec<Diagnostic>,
}

fn finite3(v: [f32; 3]) -> bool {
    v.iter().all(|c| c.is_finite())
}

/// Picks the lights to draw: one directional light (the first that casts shadows,
/// else the first) and the eight point lights nearest the camera. Lights with
/// non-finite values or no intensity are left out. Ignored lights give an `RE6017`
/// warning.
pub(crate) fn select_lights(lights: &[Light], camera_pos: Vec3, exposure: f32) -> LightSetup {
    let mut setup = LightSetup::default();
    let usable = |l: &&Light| {
        l.intensity.is_finite()
            && l.intensity > 0.0
            && finite3(l.color)
            && l.position.is_finite()
            && l.direction.is_finite()
    };
    let directional: Vec<&Light> = lights
        .iter()
        .filter(|l| l.kind == LightKind::Directional)
        .filter(usable)
        .filter(|l| l.direction.length_squared() > 1e-12)
        .collect();
    if let Some(sun) = directional
        .iter()
        .find(|l| l.shadows)
        .or_else(|| directional.first())
    {
        setup.sun = Some(Sun {
            direction: sun.direction.normalize(),
            radiance: Vec3::from_array(sun.color) * sun.intensity * exposure,
            shadows: sun.shadows,
        });
    }
    if directional.len() > 1 {
        setup.warnings.push(
            Diagnostic::warning(
                codes::TOO_MANY_LIGHTS,
                format!(
                    "{} directional lights in the frame; only one is drawn",
                    directional.len()
                ),
            )
            .with_hint("the one that casts shadows (or the first) is used"),
        );
    }
    let mut points: Vec<&Light> = lights
        .iter()
        .filter(|l| l.kind == LightKind::Point)
        .filter(usable)
        .filter(|l| l.range.is_finite() && l.range > 0.0)
        .collect();
    if points.len() > MAX_POINT_LIGHTS {
        setup.warnings.push(
            Diagnostic::warning(
                codes::TOO_MANY_LIGHTS,
                format!(
                    "{} point lights in the frame; only the {MAX_POINT_LIGHTS} nearest the camera are drawn",
                    points.len()
                ),
            )
            .with_hint("merge lights, or turn off the ones far from the action"),
        );
        // A stable sort keeps equal distances in snapshot order (deterministic).
        points.sort_by(|a, b| {
            let da = (a.position - camera_pos).length() - a.range;
            let db = (b.position - camera_pos).length() - b.range;
            da.total_cmp(&db)
        });
        points.truncate(MAX_POINT_LIGHTS);
    }
    let lumens_to_candela = 1.0 / (4.0 * std::f32::consts::PI);
    setup.points = points
        .into_iter()
        .map(|l| PointLightRaw {
            position_range: l.position.extend(l.range).to_array(),
            color: (Vec3::from_array(l.color) * l.intensity * lumens_to_candela * exposure)
                .extend(0.0)
                .to_array(),
        })
        .collect();
    setup
}

/// Picks the spot lights to draw: the [`MAX_SPOT_LIGHTS`](crate::extras::MAX_SPOT_LIGHTS)
/// nearest the camera, leaving out lights with non-finite values, no intensity
/// or no cone. More lights give an `RE6017` warning.
pub(crate) fn select_spots(
    spots: &[crate::extras::SpotLight],
    camera_pos: Vec3,
    exposure: f32,
    warnings: &mut Vec<Diagnostic>,
) -> Vec<SpotLightRaw> {
    use crate::extras::MAX_SPOT_LIGHTS;
    let mut usable: Vec<&crate::extras::SpotLight> = spots
        .iter()
        .filter(|l| {
            l.intensity.is_finite()
                && l.intensity > 0.0
                && finite3(l.color)
                && l.position.is_finite()
                && l.direction.is_finite()
                && l.direction.length_squared() > 1e-12
                && l.range.is_finite()
                && l.range > 0.0
                && l.half_angle.is_finite()
                && l.half_angle > 0.0
        })
        .collect();
    if usable.len() > MAX_SPOT_LIGHTS {
        warnings.push(
            Diagnostic::warning(
                codes::TOO_MANY_LIGHTS,
                format!(
                    "{} spot lights in the frame; only the {MAX_SPOT_LIGHTS} nearest the camera are drawn",
                    usable.len()
                ),
            )
            .with_hint("merge lights, or turn off the ones far from the action"),
        );
        usable.sort_by(|a, b| {
            let da = (a.position - camera_pos).length() - a.range;
            let db = (b.position - camera_pos).length() - b.range;
            da.total_cmp(&db)
        });
        usable.truncate(MAX_SPOT_LIGHTS);
    }
    let lumens_to_candela = 1.0 / (4.0 * std::f32::consts::PI);
    usable
        .into_iter()
        .map(|l| {
            let half = l.half_angle.min(std::f32::consts::FRAC_PI_2 * 0.999);
            let outer = libm::cosf(half);
            let inner = libm::cosf(half * 0.8);
            SpotLightRaw {
                position_range: l.position.extend(l.range).to_array(),
                color: (Vec3::from_array(l.color) * l.intensity * lumens_to_candela * exposure)
                    .extend(inner)
                    .to_array(),
                direction: l.direction.normalize().extend(outer).to_array(),
            }
        })
        .collect()
}

/// Checks a camera and returns its projection for `aspect` (glam's right-handed,
/// DirectX-depth perspective: NDC depth 0..1 and Y up, the WebGPU convention).
pub(crate) fn projection(camera: &Camera, aspect: f32) -> Result<Mat4, Diagnostic> {
    let ok = camera.fov_y_radians.is_finite()
        && camera.fov_y_radians > 0.0
        && camera.fov_y_radians < std::f32::consts::PI
        && camera.near.is_finite()
        && camera.near > 0.0
        && camera.far.is_finite()
        && camera.far > camera.near
        && camera.world.is_finite()
        && camera.world.determinant().abs() > 1e-12;
    if !ok {
        return Err(Diagnostic::error(
            codes::INVALID_CAMERA,
            format!(
                "the camera cannot be drawn from: fov_y {} rad, near {}, far {}",
                camera.fov_y_radians, camera.near, camera.far
            ),
        )
        .with_hint(
            "use a field of view between 0 and pi, 0 < near < far, and an invertible transform",
        ));
    }
    Ok(glam::camera::rh::proj::directx::perspective(
        camera.fov_y_radians,
        aspect,
        camera.near,
        camera.far,
    ))
}

/// The world-space box around `aabb` transformed by `world`.
pub(crate) fn transform_aabb(aabb: &Aabb, world: &Mat4) -> Aabb {
    let corners = (0..8).map(|i| {
        let p = Vec3::new(
            if i & 1 == 0 { aabb.min.x } else { aabb.max.x },
            if i & 2 == 0 { aabb.min.y } else { aabb.max.y },
            if i & 4 == 0 { aabb.min.z } else { aabb.max.z },
        );
        world.transform_point3(p)
    });
    Aabb::from_points(corners).expect("eight corners")
}

/// The planes of a view frustum, for culling: `dot(plane.xyz, p) + plane.w >= 0`
/// inside.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frustum([Vec4; 6]);

impl Frustum {
    /// The frustum of a DirectX-depth (0..1) `view_proj`.
    pub fn new(view_proj: &Mat4) -> Frustum {
        let r = view_proj.transpose();
        let (x, y, z, w) = (r.x_axis, r.y_axis, r.z_axis, r.w_axis);
        Frustum([w + x, w - x, w + y, w - y, z, w - z])
    }

    /// False only when `aabb` is wholly outside one plane.
    pub fn intersects(&self, aabb: &Aabb) -> bool {
        self.0.iter().all(|p| {
            let n = p.truncate();
            // The corner furthest along the plane's normal.
            let corner = Vec3::select(n.cmpge(Vec3::ZERO), aabb.max, aabb.min);
            n.dot(corner) + p.w >= 0.0
        })
    }
}

/// The view depths where the cascades end: the practical split scheme (Zhang et
/// al. 2006), `lambda` of the logarithmic split plus `1 - lambda` of the uniform
/// one, between `near` and `far`. Entries past `count` repeat `far`.
pub(crate) fn cascade_splits(
    near: f32,
    far: f32,
    count: usize,
    lambda: f32,
) -> [f32; MAX_CASCADES] {
    let count = count.clamp(1, MAX_CASCADES);
    let mut splits = [far; MAX_CASCADES];
    // The logarithmic split as products of one ratio, so it needs no platform
    // maths (ADR-0036): near * ratio^i, with ratio^count = far / near.
    let ratio = nth_root(far / near, count as u32);
    let mut log = near;
    for (i, split) in splits.iter_mut().enumerate().take(count - 1) {
        log *= ratio;
        let uniform = near + (far - near) * (i + 1) as f32 / count as f32;
        *split = lambda * log + (1.0 - lambda) * uniform;
    }
    splits
}

/// `x^(1/n)` for `x >= 1`, by bisection: `+ - * /` only, so it is the same
/// on every platform (ADR-0036).
fn nth_root(x: f32, n: u32) -> f32 {
    let (mut lo, mut hi) = (1.0f32, x.max(1.0));
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        if (0..n).fold(1.0f32, |p, _| p * mid) > x {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lo
}

/// The sun's direction as a light view (looking down -Z) and the depth along it.
pub(crate) fn light_view(direction: Vec3) -> Mat4 {
    let up = if direction.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    glam::camera::rh::view::look_to_mat4(Vec3::ZERO, direction, up)
}

/// One cascade's square in light space, before its depth range is known.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CascadeBox {
    /// The light-space centre; x and y are snapped to whole texels.
    pub center: Vec3,
    /// Half the side of the square, in world units.
    pub radius: f32,
    /// One shadow-map texel in world units.
    pub texel_world: f32,
}

/// Fits the cascade that covers view depths `near..far` of `camera`, whose
/// projection is `projection`, for the light view `view` and a `map_size`
/// square map.
///
/// The square holds the slice's bounding sphere, whose radius depends only on
/// the field of view, the aspect and the depths, so it keeps its size when the
/// camera turns; its centre is snapped to whole texels, so it does not shimmer
/// when the camera moves (ADR-0038).
pub(crate) fn fit_cascade(
    view: &Mat4,
    camera: &Camera,
    projection: &Mat4,
    near: f32,
    far: f32,
    map_size: u32,
) -> CascadeBox {
    // The squared tangent of the half diagonal field of view, from the
    // projection's scales (1 / tan(fov_y / 2) and that over the aspect).
    let (tx, ty) = (projection.x_axis.x.recip(), projection.y_axis.y.recip());
    let k2 = tx * tx + ty * ty;
    // The smallest sphere through the slice's near and far corners has its
    // centre on the view axis, where both are equally far; past the far plane,
    // the far corners alone decide.
    let along = (0.5 * (far + near) * (1.0 + k2)).min(far);
    let radius = ((far - along) * (far - along) + far * far * k2).sqrt();
    // Rounded up to 1/64 of a unit, so rounding never changes the size.
    let radius = ((radius * 64.0).ceil() / 64.0).max(1.0 / 64.0);
    let center = camera.world.transform_point3(Vec3::new(0.0, 0.0, -along));
    let texel_world = 2.0 * radius / map_size as f32;
    let mut c = view.transform_point3(center);
    c.x = (c.x / texel_world).floor() * texel_world;
    c.y = (c.y / texel_world).floor() * texel_world;
    CascadeBox {
        center: c,
        radius,
        texel_world,
    }
}

impl CascadeBox {
    /// True when a caster with light-space bounds `ls` can shadow the slice:
    /// it overlaps the square and is not wholly beyond the far end.
    pub fn takes_caster(&self, ls: &Aabb) -> bool {
        let c = self.center;
        ls.max.x >= c.x - self.radius
            && ls.min.x <= c.x + self.radius
            && ls.max.y >= c.y - self.radius
            && ls.min.y <= c.y + self.radius
            // Depth along the light is -z.
            && -ls.max.z <= -c.z + self.radius
    }

    /// The light's projection with the depth range reaching back towards the
    /// sun to `caster_depth` (the nearest depth of the cascade's casters).
    /// Both ends are snapped to steps of an eighth of the radius, so the range
    /// moves in whole steps and depths keep their values in between.
    pub fn light_view_proj(&self, view: &Mat4, caster_depth: Option<f32>) -> Mat4 {
        let c = self.center;
        let step = self.radius * 0.125;
        let near = (-c.z - self.radius).min(caster_depth.unwrap_or(f32::INFINITY));
        let near = (near / step).floor() * step - step;
        let far = ((-c.z + self.radius) / step).ceil() * step + step;
        let proj = glam::camera::rh::proj::directx::orthographic(
            c.x - self.radius,
            c.x + self.radius,
            c.y - self.radius,
            c.y + self.radius,
            near,
            far,
        );
        proj * *view
    }
}

/// The frame's shadow settings with unusable values replaced by the defaults
/// and the rest clamped to their ranges.
pub(crate) fn sanitize_shadows(s: &SunShadows) -> SunShadows {
    let d = SunShadows::default();
    let finite_or = |v: f32, or: f32| if v.is_finite() { v } else { or };
    SunShadows {
        cascades: s.cascades.clamp(1, MAX_CASCADES as u8),
        distance: if s.distance.is_finite() && s.distance > 0.0 {
            s.distance
        } else {
            d.distance
        },
        split_lambda: finite_or(s.split_lambda, d.split_lambda).clamp(0.0, 1.0),
        blend: finite_or(s.blend, d.blend).clamp(0.0, 0.5),
    }
}

/// `v` as an RGBA array with `w` in the last channel.
pub(crate) fn rgb_w(v: [f32; 3], w: f32) -> [f32; 4] {
    Vec4::new(v[0], v[1], v[2], w).to_array()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(kind: LightKind, position: Vec3, shadows: bool) -> Light {
        Light {
            kind,
            position,
            direction: Vec3::new(0.0, -1.0, -0.5),
            shadows,
            ..Light::default()
        }
    }

    #[test]
    fn struct_sizes_match_the_shaders() {
        assert_eq!(std::mem::size_of::<PointLightRaw>(), 32);
        assert_eq!(std::mem::size_of::<SpotLightRaw>(), 48);
        assert_eq!(
            std::mem::size_of::<FrameUniform>(),
            6 * 64 + 17 * 16 + 8 * 32 + 8 * 48
        );
        assert_eq!(std::mem::size_of::<InstanceRaw>(), 128);
        assert_eq!(std::mem::size_of::<MaterialUniform>(), 64);
        assert_eq!(std::mem::size_of::<LineVertex>(), 28);
        assert_eq!(std::mem::size_of::<re_types::mesh::Vertex>(), 48);
    }

    #[test]
    fn the_shadowed_sun_wins_and_extra_lights_warn() {
        let mut lights = vec![
            light(LightKind::Directional, Vec3::ZERO, false),
            light(LightKind::Directional, Vec3::ZERO, true),
        ];
        for i in 0..10 {
            lights.push(light(
                LightKind::Point,
                Vec3::new(i as f32 * 10.0, 0.0, 0.0),
                false,
            ));
        }
        let setup = select_lights(&lights, Vec3::ZERO, 1.0);
        assert!(setup.sun.unwrap().shadows);
        assert_eq!(setup.points.len(), MAX_POINT_LIGHTS);
        // The two farthest point lights (x = 80 and 90) are dropped.
        assert!(setup.points.iter().all(|p| p.position_range[0] < 75.0));
        assert_eq!(setup.warnings.len(), 2);
        assert!(
            setup
                .warnings
                .iter()
                .all(|w| w.code == codes::TOO_MANY_LIGHTS)
        );
    }

    #[test]
    fn unusable_lights_are_skipped() {
        let mut dark = light(LightKind::Point, Vec3::ZERO, false);
        dark.intensity = 0.0;
        let mut nan = light(LightKind::Directional, Vec3::ZERO, true);
        nan.direction = Vec3::NAN;
        let setup = select_lights(&[dark, nan], Vec3::ZERO, 1.0);
        assert!(setup.sun.is_none() && setup.points.is_empty() && setup.warnings.is_empty());
    }

    #[test]
    fn bad_cameras_are_diagnostics() {
        let mut camera = Camera::default();
        assert!(projection(&camera, 1.5).is_ok());
        camera.near = 0.0;
        assert_eq!(
            projection(&camera, 1.5).unwrap_err().code,
            codes::INVALID_CAMERA
        );
    }

    fn camera_at(eye: Vec3, target: Vec3) -> Camera {
        Camera {
            world: glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y).inverse(),
            ..Camera::default()
        }
    }

    /// The eight corners of view depths `near..far` of `camera`.
    fn slice_corners(camera: &Camera, projection: &Mat4, near: f32, far: f32) -> Vec<Vec3> {
        let (tx, ty) = (projection.x_axis.x.recip(), projection.y_axis.y.recip());
        let mut corners = Vec::new();
        for d in [near, far] {
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                corners.push(camera.world.transform_point3(Vec3::new(
                    sx * tx * d,
                    sy * ty * d,
                    -d,
                )));
            }
        }
        corners
    }

    #[test]
    fn splits_blend_logarithmic_and_uniform() {
        let uniform = cascade_splits(1.0, 101.0, 4, 0.0);
        assert_eq!(uniform, [26.0, 51.0, 76.0, 101.0]);
        let log = cascade_splits(1.0, 16.0, 4, 1.0);
        for (got, want) in log.iter().zip([2.0, 4.0, 8.0, 16.0]) {
            assert!((got - want).abs() < 1e-4, "{log:?}");
        }
        let practical = cascade_splits(0.1, 150.0, 4, 0.75);
        assert!(practical.windows(2).all(|w| w[0] < w[1]), "{practical:?}");
        assert_eq!(practical[3], 150.0);
        // Fewer cascades: the rest repeat the far end.
        assert_eq!(cascade_splits(0.1, 50.0, 2, 0.5)[1..], [50.0; 3]);
    }

    #[test]
    fn cascades_cover_their_slices_and_casters_towards_the_sun() {
        let camera = camera_at(Vec3::new(4.0, 4.0, 6.0), Vec3::ZERO);
        let projection = projection(&camera, 1.5).unwrap();
        let direction = Vec3::new(-0.3, -1.0, -0.2).normalize();
        let view = light_view(direction);
        let splits = cascade_splits(camera.near, 60.0, 4, 0.75);
        let mut near = camera.near;
        for far in splits {
            let fit = fit_cascade(&view, &camera, &projection, near, far, 1024);
            // A caster 40 units up the sun's direction from the slice's centre.
            let above = camera
                .world
                .transform_point3(Vec3::new(0.0, 0.0, -0.5 * (near + far)))
                - direction * 40.0;
            let ls = transform_aabb(&Aabb::new(above, above), &view);
            assert!(fit.takes_caster(&ls));
            let m = fit.light_view_proj(&view, Some(-ls.max.z));
            for p in slice_corners(&camera, &projection, near, far)
                .into_iter()
                .chain([above])
            {
                let clip = m.project_point3(p);
                assert!(clip.x.abs() <= 1.0 && clip.y.abs() <= 1.0, "{p} -> {clip}");
                assert!((0.0..=1.0).contains(&clip.z), "{p} -> depth {}", clip.z);
            }
            // A caster beyond the far end of the slice cannot shadow it.
            let below = camera.world.transform_point3(Vec3::new(0.0, 0.0, -far))
                + direction * (3.0 * far + 10.0);
            assert!(!fit.takes_caster(&transform_aabb(&Aabb::new(below, below), &view)));
            near = far;
        }
    }

    #[test]
    fn cascades_keep_their_size_and_snap_to_texels() {
        let direction = Vec3::new(0.4, -1.0, 0.3).normalize();
        let view = light_view(direction);
        let mut sizes = Vec::new();
        for i in 0..24 {
            let t = i as f32 * 0.37;
            let eye = Vec3::new(3.0 * t, 2.0 + 0.1 * t, 5.0 - t);
            let target = eye + Vec3::new(t - 2.0, -0.3, 1.5 - 0.2 * t);
            let camera = camera_at(eye, target);
            let projection = projection(&camera, 16.0 / 9.0).unwrap();
            let fit = fit_cascade(&view, &camera, &projection, 0.1, 12.0, 2048);
            sizes.push(fit.radius);
            for v in [fit.center.x, fit.center.y] {
                let texels = v / fit.texel_world;
                assert_eq!(texels, texels.round(), "{v} is not on the texel grid");
            }
        }
        assert!(sizes.windows(2).all(|w| w[0] == w[1]), "{sizes:?}");
    }

    #[test]
    fn frustum_culls_boxes_outside_the_view() {
        let camera = camera_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let view_proj = projection(&camera, 1.0).unwrap() * camera.view();
        let frustum = Frustum::new(&view_proj);
        let unit = |c: Vec3| Aabb::new(c - Vec3::splat(0.5), c + Vec3::splat(0.5));
        assert!(frustum.intersects(&unit(Vec3::ZERO)));
        assert!(
            !frustum.intersects(&unit(Vec3::new(0.0, 0.0, 20.0))),
            "behind"
        );
        assert!(
            !frustum.intersects(&unit(Vec3::new(40.0, 0.0, 0.0))),
            "to the side"
        );
        assert!(
            !frustum.intersects(&unit(Vec3::new(0.0, 0.0, -2000.0))),
            "past far"
        );
        // A big box around the camera is in view.
        assert!(frustum.intersects(&Aabb::new(Vec3::splat(-100.0), Vec3::splat(100.0))));
    }

    #[test]
    fn bad_shadow_settings_fall_back() {
        let s = sanitize_shadows(&SunShadows {
            cascades: 9,
            distance: f32::NAN,
            split_lambda: 3.0,
            blend: -1.0,
        });
        assert_eq!(
            s,
            SunShadows {
                cascades: 4,
                distance: 150.0,
                split_lambda: 1.0,
                blend: 0.0
            }
        );
        assert_eq!(
            sanitize_shadows(&SunShadows { cascades: 0, ..s }).cascades,
            1
        );
    }
}
