//! `Lighting` and the lights: the sun from `ClockTime`, the ambient light,
//! fog, `Atmosphere`, `Sky`, the post effects (`ColorCorrectionEffect`,
//! `BloomEffect`, `BlurEffect`, `SunRaysEffect`), and `PointLight`,
//! `SpotLight` and `SurfaceLight` under parts and attachments.

use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use glam::{Quat, Vec3};
use mlua::{Lua, MultiValue, Value as LuaValue};
use re_reflect::Value;
use re_scene::{Entity, World};
use re_types::render::{Bloom, Light, LightKind, SunRays};

use super::{ColorGrading, Fog, Sky, SpotLight};
use crate::class::Accessor;
use crate::gui::props::{Props, Rgba, Scope};
use crate::instance::methods_api::{args, ret};
use crate::instance::tree;
use crate::runtime::{Bindings, lua_error, state, with_world};

/// Linear RGB of an sRGB colour.
pub(crate) fn linear(c: Rgba) -> [f32; 3] {
    let l = super::looks::srgb_to_linear;
    [l(c.0[0].max(0.0)), l(c.0[1].max(0.0)), l(c.0[2].max(0.0))]
}

fn add(a: [f32; 3], b: [f32; 3], k: f32) -> [f32; 3] {
    [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k]
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// The unit vector towards the sun at `clock` hours and `latitude`
/// degrees: it rises in +X (east) at 6, is highest at 12 (towards +Z, the
/// south, north of the equator) and sets in -X at 18. The sun stays on the
/// equator (no seasons).
pub fn sun_direction(clock: f32, latitude: f32) -> Vec3 {
    let clock = if clock.is_finite() { clock } else { 14.0 };
    let lat = if latitude.is_finite() {
        latitude.clamp(-90.0, 90.0).to_radians()
    } else {
        0.0
    };
    let h = (clock / 24.0 - 0.5) * TAU;
    Vec3::new(
        -libm::sinf(h),
        libm::cosf(lat) * libm::cosf(h),
        libm::sinf(lat) * libm::cosf(h),
    )
    .normalize_or(Vec3::Y)
}

/// `ClockTime` from a `TimeOfDay` string (`"HH:MM:SS"`, minutes and seconds
/// optional), wrapped into 0..24.
fn parse_time(s: &str) -> Option<f32> {
    let mut parts = s.trim().split(':');
    let mut total = 0.0f64;
    for scale in [1.0, 1.0 / 60.0, 1.0 / 3600.0] {
        match parts.next() {
            Some(p) => total += p.trim().parse::<f64>().ok()? * scale,
            None => break,
        }
    }
    if parts.next().is_some() || !total.is_finite() {
        return None;
    }
    Some(total.rem_euclid(24.0) as f32)
}

fn format_time(clock: f64) -> String {
    let secs = (clock.rem_euclid(24.0) * 3600.0).round() as u64 % 86_400;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

fn clock_of(w: &World, lighting: Entity) -> f64 {
    match crate::input::get_own(w, "Lighting", lighting, "ClockTime") {
        Some(Value::Float(f)) => f,
        Some(Value::Int(i)) => i as f64,
        _ => 14.0,
    }
}

fn set_clock(w: &mut World, lighting: Entity, clock: f64) -> Result<(), crate::CompatError> {
    crate::tween::set_own(
        w,
        lighting,
        "Lighting",
        "ClockTime",
        Value::Float(clock.rem_euclid(24.0)),
    )
}

/// `Lighting.TimeOfDay`, a view of `ClockTime`.
pub(crate) fn accessors() -> Vec<(&'static str, &'static str, Accessor)> {
    vec![(
        "Lighting",
        "TimeOfDay",
        Accessor {
            get: Arc::new(|w, e| Ok(Value::Str(format_time(clock_of(w, e))))),
            set: Some(Arc::new(|w, e, v| {
                let Value::Str(s) = v else {
                    return Err(crate::CompatError::Runtime {
                        code: re_script::codes::BAD_ARGUMENT,
                        message: "TimeOfDay must be a string".into(),
                    });
                };
                let clock = parse_time(&s).ok_or_else(|| crate::CompatError::Runtime {
                    code: re_script::codes::BAD_ARGUMENT,
                    message: format!("TimeOfDay must look like \"14:00:00\", got {s:?}"),
                })?;
                set_clock(w, e, f64::from(clock))
            })),
        },
    )]
}

pub(crate) fn register(b: &mut Bindings) {
    let sun = |lua: &Lua, e: Entity, sign: f32| -> mlua::Result<MultiValue> {
        let classes = state(lua)?.setup.classes.clone();
        let d = with_world(lua, |w| {
            let s = Scope::new(w, &classes);
            let p = s.props(e);
            sun_direction(
                p.f32("ClockTime", 14.0),
                p.f32("GeographicLatitude", 41.733),
            )
        })? * sign;
        ret(lua, LuaValue::Vector(mlua::Vector::new(d.x, d.y, d.z)))
    };
    b.method(
        "Lighting",
        "GetSunDirection",
        Arc::new(move |lua, e, _| sun(lua, e, 1.0)),
    );
    b.method(
        "Lighting",
        "GetMoonDirection",
        Arc::new(move |lua, e, _| sun(lua, e, -1.0)),
    );
    b.method(
        "Lighting",
        "GetMinutesAfterMidnight",
        Arc::new(|lua, e, _| {
            let clock = with_world(lua, |w| clock_of(w, e))?;
            ret(lua, clock * 60.0)
        }),
    );
    b.method(
        "Lighting",
        "SetMinutesAfterMidnight",
        Arc::new(|lua, e, a| {
            let (m,): (f64,) = args(lua, a, "Lighting:SetMinutesAfterMidnight")?;
            if !m.is_finite() {
                return Err(lua_error(
                    re_script::codes::BAD_ARGUMENT,
                    "Lighting:SetMinutesAfterMidnight needs a finite number",
                ));
            }
            with_world(lua, |w| set_clock(w, e, m / 60.0))?
                .map_err(crate::runtime::compat_error)?;
            crate::signal::events::after_write(lua)?;
            ret(lua, ())
        }),
    );
}

/// What `Lighting` and its children give a frame.
pub(crate) struct Environment {
    pub sun: Option<Light>,
    pub ambient: ([f32; 3], [f32; 3]),
    pub exposure: f32,
    pub fog: Option<Fog>,
    pub sky: Sky,
    pub effects: PostEffects,
}

/// The post effects of a frame, in the renderer's terms.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PostEffects {
    /// Colour correction (`ColorCorrectionEffect`).
    pub grading: Option<ColorGrading>,
    /// Glow (`BloomEffect`).
    pub bloom: Option<Bloom>,
    /// The full-screen blur's radius in fractions of the view's height
    /// (`BlurEffect`); 0 is off.
    pub blur_radius: f32,
    /// Light shafts from the sun (`SunRaysEffect`).
    pub sun_rays: Option<SunRays>,
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The environment of a frame from `Lighting` (`None`: the defaults) and
/// the camera's children (for colour correction under the camera).
pub(crate) fn environment(
    s: &Scope,
    lighting: Option<Entity>,
    camera: Option<Entity>,
) -> Environment {
    let p = lighting.map(|l| s.props(l));
    let f = |name: &'static str, or: f32| p.as_ref().map_or(or, |p| p.f32(name, or));
    let c = |name: &'static str, or: [f32; 3]| {
        let or = Rgba([or[0], or[1], or[2], 1.0]);
        linear(p.as_ref().map_or(or, |p| p.color(name, or)))
    };
    let clock = f("ClockTime", 14.0);
    let to_sun = sun_direction(clock, f("GeographicLatitude", 41.733));
    // 1 by day, 0 at night, fading as the sun crosses the horizon.
    let day = smoothstep(-0.1, 0.15, to_sun.y);
    let brightness = f("Brightness", 2.0).max(0.0);
    let shadows = p.as_ref().is_none_or(|p| p.bool("GlobalShadows", true));
    let warm = smoothstep(0.0, 0.35, to_sun.y);
    let sun = if day > 0.0 && brightness > 0.0 {
        Some(Light {
            kind: LightKind::Directional,
            direction: -to_sun,
            color: [1.0, 0.75 + 0.2 * warm, 0.55 + 0.4 * warm],
            intensity: brightness * 0.5 * day,
            shadows,
            ..Light::default()
        })
    } else if brightness > 0.0 {
        // Moonlight.
        Some(Light {
            kind: LightKind::Directional,
            direction: to_sun,
            color: [0.6, 0.7, 1.0],
            intensity: brightness * 0.04,
            shadows,
            ..Light::default()
        })
    } else {
        None
    };
    let ambient = c("Ambient", [0.0; 3]);
    let outdoor = c("OutdoorAmbient", [0.5; 3]);
    let outdoor_scale = 0.25 + 0.75 * day;
    let ambient = (
        add(ambient, outdoor, outdoor_scale * 0.6),
        add(ambient, outdoor, outdoor_scale * 0.25),
    );
    let exposure = libm::powf(2f32, f("ExposureCompensation", 0.0).clamp(-5.0, 5.0));

    // Sky by time of day.
    let dusk = 1.0 - smoothstep(0.0, 0.3, to_sun.y.abs());
    let day_zenith = [0.18, 0.36, 0.78];
    let day_horizon = [0.62, 0.74, 0.9];
    let night = [0.01, 0.015, 0.04];
    let mut zenith = mix(night, day_zenith, day);
    let mut horizon = mix(mix(night, day_horizon, day), [0.95, 0.5, 0.25], dusk * 0.6);
    let mut fog = None;
    let fog_end = f("FogEnd", 100_000.0);
    if fog_end < 100_000.0 {
        let start = f("FogStart", 0.0).max(0.0);
        fog = Some(Fog {
            start,
            end: fog_end.max(start + 0.01),
            color: c("FogColor", [0.753; 3]),
        });
    }
    let mut celestial = true;
    if let Some(l) = lighting {
        for k in tree::children(s.w, l) {
            if s.is_a(k, "Atmosphere") {
                let a = s.props(k);
                let density = a.f32("Density", 0.395).clamp(0.0, 1.0);
                let haze = a.f32("Haze", 0.0).clamp(0.0, 10.0);
                let color = linear(a.color("Color", Rgba([0.78, 0.78, 0.78, 1.0])));
                let tint = mix(color, horizon, 0.4);
                horizon = mix(horizon, tint, density);
                zenith = mix(zenith, tint, density * 0.3);
                let end = (5000.0 * (1.0 - density).powi(3) + 120.0) / (1.0 + haze * 0.3);
                if density > 0.0 && fog.is_none_or(|f: Fog| f.end > end) {
                    fog = Some(Fog {
                        start: 0.0,
                        end,
                        color: mix(horizon, color, 0.5),
                    });
                }
            } else if s.is_a(k, "Sky") {
                celestial = s.props(k).bool("CelestialBodiesShown", true);
            }
        }
    }
    let ground = mix(horizon, [0.18, 0.17, 0.16], 0.6);
    let sky = Sky {
        zenith,
        horizon,
        ground,
        sun: (celestial && to_sun.y > -0.05).then_some((to_sun, [4.0, 3.6, 3.0])),
    };

    let effects = effects(s, lighting, camera);
    Environment {
        sun,
        ambient,
        exposure,
        fog,
        sky,
        effects,
    }
}

/// How many studs of `Size` make one target height of blur or glow radius:
/// the default `Size` of 24 is a radius of 5% of the view's height, the
/// renderer's default bloom radius.
const SIZE_PER_HEIGHT: f32 = 480.0;

/// The enabled post effects under `Lighting` and the camera, combined: colour
/// corrections add (tints multiply); blooms add their intensities and take
/// the widest size and lowest threshold; blurs combine as Gaussians do (the
/// radii add in squares); sun rays add their intensities and take the
/// widest spread.
pub(crate) fn effects(s: &Scope, lighting: Option<Entity>, camera: Option<Entity>) -> PostEffects {
    let mut out = PostEffects::default();
    let mut blur2 = 0.0f32;
    for parent in [lighting, camera].into_iter().flatten() {
        for k in tree::children(s.w, parent) {
            let kind = if s.is_a(k, "ColorCorrectionEffect") {
                0
            } else if s.is_a(k, "BloomEffect") {
                1
            } else if s.is_a(k, "BlurEffect") {
                2
            } else if s.is_a(k, "SunRaysEffect") {
                3
            } else {
                continue;
            };
            let e = s.props(k);
            if !e.bool("Enabled", true) {
                continue;
            }
            match kind {
                0 => {
                    let g = out.grading.get_or_insert_with(ColorGrading::default);
                    g.brightness += e.f32("Brightness", 0.0);
                    g.contrast += e.f32("Contrast", 0.0);
                    g.saturation += e.f32("Saturation", 0.0);
                    let t = linear(e.color("TintColor", Rgba::WHITE));
                    g.tint = [g.tint[0] * t[0], g.tint[1] * t[1], g.tint[2] * t[2]];
                }
                1 => {
                    let intensity = finite(e.f32("Intensity", 1.0)).max(0.0);
                    let radius = finite(e.f32("Size", 24.0)).clamp(0.0, 56.0) / SIZE_PER_HEIGHT;
                    let threshold = finite(e.f32("Threshold", 2.0)).max(0.0);
                    match &mut out.bloom {
                        Some(b) => {
                            b.intensity += intensity;
                            b.radius = b.radius.max(radius);
                            b.threshold = b.threshold.min(threshold);
                        }
                        None => {
                            out.bloom = Some(Bloom {
                                intensity,
                                threshold,
                                radius,
                                ..Bloom::default()
                            })
                        }
                    }
                }
                2 => {
                    let r = finite(e.f32("Size", 24.0)).clamp(0.0, 56.0) / SIZE_PER_HEIGHT;
                    blur2 += r * r;
                }
                _ => {
                    let intensity = finite(e.f32("Intensity", 0.25)).max(0.0);
                    let spread = finite(e.f32("Spread", 1.0)).clamp(0.0, 1.0);
                    match &mut out.sun_rays {
                        Some(r) => {
                            r.intensity += intensity;
                            r.spread = r.spread.max(spread);
                        }
                        None => out.sun_rays = Some(SunRays { intensity, spread }),
                    }
                }
            }
        }
    }
    out.blur_radius = blur2.sqrt();
    out
}

fn finite(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

/// The unit axis of a `NormalId` item in a part's frame.
pub(crate) fn normal_axis(item: &str) -> Vec3 {
    match item {
        "Right" => Vec3::X,
        "Left" => -Vec3::X,
        "Top" => Vec3::Y,
        "Bottom" => -Vec3::Y,
        "Back" => Vec3::Z,
        _ => -Vec3::Z,
    }
}

/// A light's source: where it is, how it is turned, and the parent part's
/// size (zero for an attachment).
pub(crate) fn source(s: &Scope, e: Entity) -> Option<(Vec3, Quat, Vec3)> {
    let parent = tree::parent(s.w, e)?;
    super::frame_of(s, parent)
}

/// The lights of the world: point lights into `points`, spot and surface
/// lights into `spots`.
pub(crate) fn lights(
    s: &Scope,
    lights: &[Entity],
    points: &mut Vec<Light>,
    spots: &mut Vec<SpotLight>,
) {
    for &e in lights {
        let p: Props = s.props(e);
        if !p.bool("Enabled", true) {
            continue;
        }
        let Some((pos, rot, size)) = source(s, e) else {
            continue;
        };
        let brightness = p.f32("Brightness", 1.0).max(0.0);
        let color = linear(p.color("Color", Rgba::WHITE));
        let is_point = s.is_a(e, "PointLight");
        let range = p
            .f32("Range", if is_point { 8.0 } else { 16.0 })
            .clamp(0.0, 60.0);
        if brightness <= 0.0 || range <= 0.0 {
            continue;
        }
        // About half the brightness lands at half the range.
        let lumens = PI / 2.0 * brightness * range * range;
        if is_point {
            points.push(Light {
                kind: LightKind::Point,
                position: pos,
                color,
                intensity: lumens,
                range,
                ..Light::default()
            });
            continue;
        }
        let axis = normal_axis(&p.item("Face", "Front"));
        let direction = (rot * axis).normalize_or_zero();
        let mut position = pos;
        if s.is_a(e, "SurfaceLight") {
            position += rot * (axis * size * 0.5);
        }
        let angle = p.f32("Angle", 90.0).clamp(0.0, 180.0);
        spots.push(SpotLight {
            position,
            direction,
            color,
            intensity: lumens,
            range,
            half_angle: (angle * 0.5).to_radians(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sun_rises_east_peaks_at_noon_and_sets_west() {
        let rise = sun_direction(6.0, 0.0);
        assert!(rise.x > 0.99 && rise.y.abs() < 1e-5);
        let noon = sun_direction(12.0, 0.0);
        assert!((noon - Vec3::Y).length() < 1e-5);
        let set = sun_direction(18.0, 0.0);
        assert!(set.x < -0.99);
        assert!(sun_direction(0.0, 0.0).y < -0.99);
        // North of the equator the noon sun is towards +Z (south).
        let tilted = sun_direction(12.0, 45.0);
        assert!(tilted.z > 0.7 && tilted.y > 0.7);
    }

    #[test]
    fn time_of_day_strings_round_trip() {
        assert_eq!(parse_time("14:30:00"), Some(14.5));
        assert_eq!(parse_time("6"), Some(6.0));
        assert_eq!(parse_time("25:00:00"), Some(1.0));
        assert_eq!(parse_time("noon"), None);
        assert_eq!(parse_time("1:2:3:4"), None);
        assert_eq!(format_time(14.5), "14:30:00");
        assert_eq!(format_time(-1.0), "23:00:00");
    }
}
