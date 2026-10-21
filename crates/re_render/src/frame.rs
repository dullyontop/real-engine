//! Per-frame GPU data and the CPU-side light and shadow set-up.
//!
//! The `#[repr(C)]` structs here must match the WGSL declarations in
//! `shaders/common.wgsl` and `shaders/forward.wgsl`.

use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Mat4, Vec3, Vec4};
use re_core::Diagnostic;
use re_types::mesh::Aabb;
use re_types::render::{Camera, Light, LightKind};

use crate::codes;

/// The most point lights one frame draws.
pub const MAX_POINT_LIGHTS: usize = 8;

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
    pub light_view_proj: [[f32; 4]; 4],
    pub inv_view_proj: [[f32; 4]; 4],
    pub camera_pos: [f32; 4],
    pub sun_dir: [f32; 4],
    pub sun_color: [f32; 4],
    pub ambient_sky: [f32; 4],
    pub ambient_ground: [f32; 4],
    pub params: [f32; 4],
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
            let outer = half.cos();
            let inner = (half * 0.8).cos();
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

/// The sun's shadow projection for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShadowFit {
    /// World to shadow clip space.
    pub light_view_proj: Mat4,
    /// One shadow-map texel in world units.
    pub texel_world: f32,
}

/// Fits the shadow map of a sun travelling along `direction`.
///
/// The covered region is a sphere: the one around the camera frustum up to
/// `shadow_distance`, or the one around the drawn scene when that is smaller. Its
/// centre is snapped to whole texels in light space, so the shadow does not
/// shimmer when the camera moves. The depth range reaches back to every caster.
pub(crate) fn fit_shadow(
    direction: Vec3,
    camera: &Camera,
    aspect: f32,
    shadow_distance: f32,
    scene: &Aabb,
    casters: &Aabb,
    map_size: u32,
) -> ShadowFit {
    let far = camera.far.min(shadow_distance.max(camera.near * 2.0));
    let tan = (camera.fov_y_radians * 0.5).tan();
    let mut corners = Vec::with_capacity(8);
    for d in [camera.near, far] {
        let (hx, hy) = (d * tan * aspect, d * tan);
        for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            corners.push(
                camera
                    .world
                    .transform_point3(Vec3::new(sx * hx, sy * hy, -d)),
            );
        }
    }
    let frustum_center = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
    let frustum_radius = corners
        .iter()
        .map(|c| c.distance(frustum_center))
        .fold(0.0, f32::max);
    let scene_center = scene.center();
    let scene_radius = scene.half_extents().length();
    let (center, radius) = if scene_radius < frustum_radius {
        (scene_center, scene_radius)
    } else {
        (frustum_center, frustum_radius)
    };
    let radius = radius.max(1e-3);

    let up = if direction.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    let view = glam::camera::rh::view::look_to_mat4(Vec3::ZERO, direction, up);
    let texel_world = 2.0 * radius / map_size as f32;
    let mut c = view.transform_point3(center);
    c.x = (c.x / texel_world).floor() * texel_world;
    c.y = (c.y / texel_world).floor() * texel_world;

    // Light space looks down -Z: depth along the light is -z.
    let caster_corners = transform_aabb(casters, &view);
    let near = (-c.z - radius).min(-caster_corners.max.z) - texel_world;
    let far = -c.z + radius + texel_world;
    let proj = glam::camera::rh::proj::directx::orthographic(
        c.x - radius,
        c.x + radius,
        c.y - radius,
        c.y + radius,
        near,
        far,
    );
    ShadowFit {
        light_view_proj: proj * view,
        texel_world,
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
            192 + 13 * 16 + 8 * 32 + 8 * 48
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

    #[test]
    fn shadow_fit_covers_the_scene() {
        let camera = Camera {
            world: glam::camera::rh::view::look_at_mat4(
                Vec3::new(4.0, 4.0, 6.0),
                Vec3::ZERO,
                Vec3::Y,
            )
            .inverse(),
            ..Camera::default()
        };
        let scene = Aabb::new(Vec3::new(-3.0, 0.0, -3.0), Vec3::new(3.0, 2.0, 3.0));
        let fit = fit_shadow(
            Vec3::new(-0.3, -1.0, -0.2).normalize(),
            &camera,
            1.5,
            50.0,
            &scene,
            &scene,
            1024,
        );
        for i in 0..8 {
            let p = Vec3::new(
                if i & 1 == 0 { -3.0 } else { 3.0 },
                if i & 2 == 0 { 0.0 } else { 2.0 },
                if i & 4 == 0 { -3.0 } else { 3.0 },
            );
            let clip = fit.light_view_proj.project_point3(p);
            assert!(clip.x.abs() <= 1.0 && clip.y.abs() <= 1.0, "{p} -> {clip}");
            assert!((0.0..=1.0).contains(&clip.z), "{p} -> depth {}", clip.z);
        }
        // Higher points are nearer the sun: smaller depth.
        let low = fit.light_view_proj.project_point3(Vec3::ZERO).z;
        let high = fit.light_view_proj.project_point3(Vec3::Y * 2.0).z;
        assert!(high < low);
    }
}
