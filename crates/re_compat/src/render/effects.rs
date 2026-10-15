//! Particles (`ParticleEmitter`), beams (`Beam`), trails (`Trail`) and
//! highlights (`Highlight`).
//!
//! Particles are simulated here, on the CPU, from the emitters' properties;
//! beams are Bézier strips between their attachments; trails keep the path
//! their attachments swept. Particles plus beam and trail segments are held
//! to the project's effect budget: the excess is not drawn, counted in
//! [`EffectStats::dropped`](super::EffectStats) and warned about once per
//! overflow (`RE9351`).

use std::collections::HashSet;
use std::f32::consts::TAU;
use std::sync::Arc;

use glam::{Quat, Vec2, Vec3};
use mlua::MultiValue;
use re_core::Diagnostic;
use re_noise::Pcg32;
use re_reflect::Value;
use re_scene::{Entity, World};

use super::camera::View;
use super::lighting::{linear, normal_axis};
use super::{
    Highlight, Hub, RenderFrame, Ribbon, RibbonPoint, Sprite, SpriteShape, frame_of, instances_of,
};
use crate::gui::props::{FxMap, Props, Rgba, Scope};
use crate::instance::methods_api::{args, ret};
use crate::instance::tree;
use crate::runtime::{Bindings, lua_error};

/// The most particles one emitter keeps alive.
pub const MAX_PARTICLES_PER_EMITTER: usize = 4096;

#[derive(Clone, Copy, Debug)]
struct Particle {
    /// World position, or position in the parent's frame when locked.
    pos: Vec3,
    vel: Vec3,
    age: f32,
    life: f32,
    rot: f32,
    spin: f32,
}

#[derive(Clone, Debug, Default)]
struct Emitter {
    particles: Vec<Particle>,
    /// Particles owed by the rate, below one.
    owed: f32,
}

#[derive(Clone, Copy, Debug)]
struct TrailPoint {
    a: Vec3,
    b: Vec3,
    age: f32,
}

/// What the effects keep from frame to frame.
pub(crate) struct State {
    rng: Pcg32,
    emitters: FxMap<Entity, Emitter>,
    trails: FxMap<Entity, Vec<TrailPoint>>,
    /// `Emit(n)` calls since the last frame.
    emits: Vec<(Entity, u32)>,
    /// `Clear()` calls since the last frame.
    clears: Vec<Entity>,
    /// True while the budget overflows, so it is warned about once.
    over: bool,
}

impl Default for State {
    fn default() -> Self {
        State {
            rng: Pcg32::from_seed(0),
            emitters: FxMap::default(),
            trails: FxMap::default(),
            emits: Vec::new(),
            clears: Vec::new(),
            over: false,
        }
    }
}

impl State {
    pub(crate) fn reseed(&mut self, seed: u64) {
        self.rng = Pcg32::from_seed(seed ^ 0x5EED_E77E_C750_0000);
    }
}

/// The effect instances of a world, in a stable order.
pub(crate) struct Lists {
    emitters: Vec<Entity>,
    beams: Vec<Entity>,
    trails: Vec<Entity>,
    highlights: Vec<Entity>,
}

impl Lists {
    pub(crate) fn find(w: &mut World) -> Lists {
        let mut of = |class: &str| {
            let mut v = instances_of(w, class);
            v.sort_by_key(|e| e.to_bits());
            v
        };
        Lists {
            emitters: of("ParticleEmitter"),
            beams: of("Beam"),
            trails: of("Trail"),
            highlights: of("Highlight"),
        }
    }
}

pub(crate) fn register(b: &mut Bindings) {
    b.method(
        "ParticleEmitter",
        "Emit",
        Arc::new(|lua, e, a| {
            let (n,): (Option<f64>,) = args(lua, a, "ParticleEmitter:Emit")?;
            let n = n.unwrap_or(16.0);
            if !n.is_finite() {
                return Err(lua_error(
                    re_script::codes::BAD_ARGUMENT,
                    "ParticleEmitter:Emit needs a finite count",
                ));
            }
            let n = n.clamp(0.0, MAX_PARTICLES_PER_EMITTER as f64) as u32;
            super::hub_mut(lua, |h| h.effects.emits.push((e, n)));
            Ok(MultiValue::new())
        }),
    );
    for class in ["ParticleEmitter", "Trail"] {
        b.method(
            class,
            "Clear",
            Arc::new(|lua, e, _| {
                super::hub_mut(lua, |h| h.effects.clears.push(e));
                ret(lua, ())
            }),
        );
    }
}

fn num(v: &Value) -> Option<f32> {
    match v {
        Value::Float(f) => Some(*f as f32),
        Value::Int(i) => Some(*i as f32),
        _ => None,
    }
}

/// A `NumberRange` as `(min, max)`.
fn range(p: &Props, name: &'static str, or: (f32, f32)) -> (f32, f32) {
    match p.get(name) {
        Some(Value::Record(_, fs)) if fs.len() == 2 => match (num(&fs[0]), num(&fs[1])) {
            (Some(a), Some(b)) if a.is_finite() && b.is_finite() => (a.min(b), a.max(b)),
            _ => or,
        },
        Some(v) => num(&v).filter(|f| f.is_finite()).map_or(or, |f| (f, f)),
        None => or,
    }
}

/// A sequence's value at `t` in 0..1, `or` when it has no keypoints.
fn sample(keys: &[(f32, [f32; 3])], t: f32, or: [f32; 3]) -> [f32; 3] {
    let Some(first) = keys.first() else {
        return or;
    };
    if t <= first.0 {
        return first.1;
    }
    for w in keys.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.0 {
            let k = if b.0 > a.0 {
                (t - a.0) / (b.0 - a.0)
            } else {
                1.0
            };
            return [
                a.1[0] + (b.1[0] - a.1[0]) * k,
                a.1[1] + (b.1[1] - a.1[1]) * k,
                a.1[2] + (b.1[2] - a.1[2]) * k,
            ];
        }
    }
    keys[keys.len() - 1].1
}

fn rgb(c: [f32; 3]) -> [f32; 3] {
    linear(Rgba([c[0], c[1], c[2], 1.0]))
}

/// The highlights, as the frame's [`Highlight`]s; returns the parts they
/// cover (drawn on their own, never merged).
pub(crate) fn highlights(
    s: &Scope,
    lists: &Lists,
    in_workspace: &dyn Fn(Entity) -> bool,
    out: &mut RenderFrame,
) -> HashSet<Entity> {
    let mut covered = HashSet::new();
    for &h in &lists.highlights {
        if !tree::alive(s.w, h) {
            continue;
        }
        let p = s.props(h);
        if !p.bool("Enabled", true) {
            continue;
        }
        let Some(target) = p.entity("Adornee").or_else(|| tree::parent(s.w, h)) else {
            continue;
        };
        if !in_workspace(target) {
            continue;
        }
        let mut parts: Vec<Entity> = Vec::new();
        if s.is_a(target, "BasePart") {
            parts.push(target);
        }
        parts.extend(
            tree::descendants(s.w, target)
                .into_iter()
                .filter(|&d| s.is_a(d, "BasePart") && !s.is_a(d, "Terrain")),
        );
        if parts.is_empty() {
            continue;
        }
        let fill = linear(p.color("FillColor", Rgba([1.0, 0.0, 0.0, 1.0])));
        let outline = linear(p.color("OutlineColor", Rgba::WHITE));
        let fill_a = 1.0 - p.f32("FillTransparency", 0.5).clamp(0.0, 1.0);
        let outline_a = 1.0 - p.f32("OutlineTransparency", 0.0).clamp(0.0, 1.0);
        covered.extend(parts.iter().copied());
        out.highlights.push(Highlight {
            entities: parts.into_iter().map(re_scene::entity_id).collect(),
            fill: [fill[0], fill[1], fill[2], fill_a],
            outline: [outline[0], outline[1], outline[2], outline_a],
            always_on_top: p.item("DepthMode", "AlwaysOnTop") == "AlwaysOnTop",
        });
    }
    covered
}

/// A unit vector within `spread` degrees (x and y half-angles) of `dir`.
fn spread_dir(rng: &mut Pcg32, dir: Vec3, spread: Vec2) -> Vec3 {
    if spread.x == 0.0 && spread.y == 0.0 {
        return dir;
    }
    let (a, b) = dir.any_orthonormal_pair();
    let ax = rng.range_f32(-spread.x, spread.x).to_radians();
    let ay = rng.range_f32(-spread.y, spread.y).to_radians();
    let r = Quat::from_axis_angle(a, ax) * Quat::from_axis_angle(b, ay);
    (r * dir).normalize_or(dir)
}

/// Where and in which direction a new particle starts, in the emitter's
/// parent frame.
fn spawn_point(rng: &mut Pcg32, p: &Props, size: Vec3, emit_dir: Vec3) -> (Vec3, Vec3) {
    let shape = p.item("Shape", "Box");
    let surface = p.item("ShapeStyle", "Volume") == "Surface";
    let sign = match p.item("ShapeInOut", "Outward").as_str() {
        "Inward" => -1.0,
        "InAndOut" if rng.next_f32() < 0.5 => -1.0,
        _ => 1.0,
    };
    let half = size * 0.5;
    let mut unit = || rng.range_f32(-1.0, 1.0);
    match shape.as_str() {
        "Sphere" => {
            let r = half.min_element();
            let d = Vec3::new(unit(), unit(), unit()).normalize_or(Vec3::Y);
            let k = if surface { 1.0 } else { rng.next_f32().cbrt() };
            (d * r * k, d * sign)
        }
        "Disc" | "Cylinder" => {
            // Around the emission axis.
            let (a, b) = emit_dir.any_orthonormal_pair();
            let angle = rng.next_f32() * TAU;
            let k = if surface { 1.0 } else { rng.next_f32().sqrt() };
            let r = half.min_element() * k;
            let along = if shape == "Cylinder" {
                emit_dir * half.dot(emit_dir.abs()) * rng.range_f32(-1.0, 1.0)
            } else {
                Vec3::ZERO
            };
            (
                (a * angle.cos() + b * angle.sin()) * r + along,
                emit_dir * sign,
            )
        }
        _ => {
            let mut q = Vec3::new(unit(), unit(), unit()) * half;
            if surface {
                // On the face the particles leave from.
                q += emit_dir * (half.dot(emit_dir.abs()) - q.dot(emit_dir));
            }
            (q, emit_dir * sign)
        }
    }
}

/// Advances one emitter and appends its sprites.
#[allow(clippy::too_many_arguments)]
fn emitter(
    s: &Scope,
    st: &mut State,
    e: Entity,
    view: &View,
    dt: f32,
    emits: u32,
    sprites: &mut Vec<Sprite>,
) {
    let Some(parent) = tree::parent(s.w, e) else {
        st.emitters.remove(&e);
        return;
    };
    let Some((origin, rot, size)) = frame_of(s, parent) else {
        st.emitters.remove(&e);
        return;
    };
    let p = s.props(e);
    let em = st.emitters.entry(e).or_default();
    let rng = &mut st.rng;
    let dt = dt * p.f32("TimeScale", 1.0).clamp(0.0, 1.0);
    let locked = p.bool("LockedToPart", false);
    let accel = p.vec3("Acceleration");
    let accel = if accel.is_finite() { accel } else { Vec3::ZERO };
    let drag = p.f32("Drag", 0.0).max(0.0);

    // Age and move.
    let damp = 2f32.powf(-drag * dt);
    let local_accel = if locked { rot.inverse() * accel } else { accel };
    em.particles.retain_mut(|q| {
        q.age += dt;
        q.vel = (q.vel + local_accel * dt) * damp;
        q.pos += q.vel * dt;
        q.rot += q.spin * dt;
        q.age < q.life
    });

    // New particles.
    let mut n = emits;
    if p.bool("Enabled", true) {
        let rate = p.f32("Rate", 5.0).clamp(0.0, 1e5);
        em.owed += rate * dt;
        let whole = em.owed.floor();
        em.owed -= whole;
        n += whole as u32;
    }
    let room = MAX_PARTICLES_PER_EMITTER.saturating_sub(em.particles.len());
    let n = (n as usize).min(room);
    if n > 0 {
        let life = range(&p, "Lifetime", (5.0, 10.0));
        let speed = range(&p, "Speed", (5.0, 5.0));
        let rotation = range(&p, "Rotation", (0.0, 0.0));
        let spin = range(&p, "RotSpeed", (0.0, 0.0));
        let spread = p.vec2("SpreadAngle", Vec2::ZERO);
        let axis = normal_axis(&p.item("EmissionDirection", "Top"));
        let draw = |rng: &mut Pcg32, r: (f32, f32)| {
            if r.1 > r.0 {
                rng.range_f32(r.0, r.1)
            } else {
                r.0
            }
        };
        for _ in 0..n {
            let (at, dir) = spawn_point(rng, &p, size, axis);
            let dir = spread_dir(rng, dir, spread);
            let v = dir * draw(rng, speed);
            let (pos, vel) = if locked {
                (at, v)
            } else {
                (origin + rot * at, rot * v)
            };
            em.particles.push(Particle {
                pos,
                vel,
                age: 0.0,
                life: draw(rng, life).max(1e-3),
                rot: draw(rng, rotation).to_radians(),
                spin: draw(rng, spin).to_radians(),
            });
        }
    }

    let colors = p.sequence("Color");
    let alphas = p.sequence("Transparency");
    let sizes = p.sequence("Size");
    let brightness = p.f32("Brightness", 1.0).max(0.0);
    let emission = p.f32("LightEmission", 0.0).clamp(0.0, 1.0);
    let z_offset = p.f32("ZOffset", 0.0);
    for q in &em.particles {
        let t = (q.age / q.life).clamp(0.0, 1.0);
        let c = rgb(sample(&colors, t, [1.0; 3]));
        let a = 1.0 - sample(&alphas, t, [0.0; 3])[0].clamp(0.0, 1.0);
        let side = sample(&sizes, t, [1.0; 3])[0].max(0.0);
        if a <= 0.0 || side <= 0.0 {
            continue;
        }
        let mut pos = if locked { origin + rot * q.pos } else { q.pos };
        if z_offset != 0.0 {
            pos += (view.position - pos).normalize_or_zero() * z_offset;
        }
        sprites.push(Sprite {
            position: pos,
            size: side,
            rotation: q.rot,
            color: [c[0] * brightness, c[1] * brightness, c[2] * brightness, a],
            emission,
            shape: SpriteShape::Soft,
        });
    }
}

/// An attachment's world position and rotation.
fn attachment(s: &Scope, e: Option<Entity>) -> Option<(Vec3, Quat)> {
    let e = e.filter(|&e| tree::alive(s.w, e) && s.is_a(e, "Attachment"))?;
    frame_of(s, e).map(|(p, r, _)| (p, r))
}

fn cubic(p: [Vec3; 4], t: f32) -> Vec3 {
    let u = 1.0 - t;
    p[0] * (u * u * u) + p[1] * (3.0 * u * u * t) + p[2] * (3.0 * u * t * t) + p[3] * (t * t * t)
}

fn beam(s: &Scope, e: Entity) -> Option<Ribbon> {
    let p = s.props(e);
    if !p.bool("Enabled", true) {
        return None;
    }
    let (a0, r0) = attachment(s, p.entity("Attachment0"))?;
    let (a1, r1) = attachment(s, p.entity("Attachment1"))?;
    let c0 = p.f32("CurveSize0", 0.0);
    let c1 = p.f32("CurveSize1", 0.0);
    let ctrl = [a0, a0 + r0 * Vec3::X * c0, a1 - r1 * Vec3::X * c1, a1];
    let segments = p.int("Segments", 10).clamp(1, 1000) as usize;
    let segments = if c0 == 0.0 && c1 == 0.0 { 1 } else { segments };
    let (w0, w1) = (p.f32("Width0", 1.0), p.f32("Width1", 1.0));
    let colors = p.sequence("Color");
    let alphas = p.sequence("Transparency");
    let brightness = p.f32("Brightness", 1.0).max(0.0);
    let points = (0..=segments)
        .map(|i| {
            let t = i as f32 / segments as f32;
            let c = rgb(sample(&colors, t, [1.0; 3]));
            let a = 1.0 - sample(&alphas, t, [0.5; 3])[0].clamp(0.0, 1.0);
            RibbonPoint {
                position: cubic(ctrl, t),
                width: (w0 + (w1 - w0) * t).max(0.0),
                color: [c[0] * brightness, c[1] * brightness, c[2] * brightness, a],
            }
        })
        .collect();
    Some(Ribbon {
        points,
        normal: (!p.bool("FaceCamera", false)).then(|| (r0 * Vec3::Y).normalize_or(Vec3::Y)),
        emission: p.f32("LightEmission", 0.0).clamp(0.0, 1.0),
    })
}

fn trail(s: &Scope, st: &mut State, e: Entity, dt: f32) -> Option<Ribbon> {
    let p = s.props(e);
    let life = p.f32("Lifetime", 2.0).max(1e-3);
    let points = st.trails.entry(e).or_default();
    for q in points.iter_mut() {
        q.age += dt;
    }
    points.retain(|q| q.age < life);
    let ends = (p.bool("Enabled", true))
        .then(|| {
            Some((
                attachment(s, p.entity("Attachment0"))?.0,
                attachment(s, p.entity("Attachment1"))?.0,
            ))
        })
        .flatten();
    let mut drawn = points.clone();
    if let Some((a, b)) = ends {
        let min_len = p.f32("MinLength", 0.1).max(0.0);
        let far_enough = points
            .last()
            .is_none_or(|q| ((q.a + q.b) * 0.5).distance((a + b) * 0.5) >= min_len);
        let head = TrailPoint { a, b, age: 0.0 };
        if far_enough {
            points.push(head);
            drawn = points.clone();
        } else if points
            .last()
            .is_some_and(|q| q.a.distance(a) + q.b.distance(b) > 1e-4)
        {
            // Between kept points the strip still reaches the attachments.
            drawn.push(head);
        }
    }
    let max_len = p.f32("MaxLength", 0.0);
    if max_len > 0.0 {
        // Keep the newest `max_len` studs.
        let mut total = 0.0;
        let mut keep = drawn.len();
        for i in (1..drawn.len()).rev() {
            total +=
                ((drawn[i].a + drawn[i].b) * 0.5).distance((drawn[i - 1].a + drawn[i - 1].b) * 0.5);
            if total > max_len {
                break;
            }
            keep = i - 1;
        }
        let cut = keep.min(drawn.len().saturating_sub(2));
        drawn.drain(..cut);
    }
    if drawn.len() < 2 {
        return None;
    }
    let colors = p.sequence("Color");
    let alphas = p.sequence("Transparency");
    let widths = p.sequence("WidthScale");
    let brightness = p.f32("Brightness", 1.0).max(0.0);
    // The points run oldest to newest; a point's sequence time is its age
    // over the lifetime, so 0 is the attachments' end.
    let head = drawn[drawn.len() - 1];
    let prev = drawn[drawn.len() - 2];
    let tangent = (head.a + head.b - prev.a - prev.b).normalize_or_zero();
    let normal = (head.b - head.a).cross(tangent).normalize_or_zero();
    let points = drawn
        .iter()
        .map(|q| {
            let t = (q.age / life).clamp(0.0, 1.0);
            let c = rgb(sample(&colors, t, [1.0; 3]));
            let a = 1.0 - sample(&alphas, t, [0.5; 3])[0].clamp(0.0, 1.0);
            RibbonPoint {
                position: (q.a + q.b) * 0.5,
                width: q.a.distance(q.b) * sample(&widths, t, [1.0; 3])[0].max(0.0),
                color: [c[0] * brightness, c[1] * brightness, c[2] * brightness, a],
            }
        })
        .collect();
    Some(Ribbon {
        points,
        normal: (!p.bool("FaceCamera", false) && normal != Vec3::ZERO).then_some(normal),
        emission: p.f32("LightEmission", 0.0).clamp(0.0, 1.0),
    })
}

/// Advances the effects by `dt` and adds what fits the budget.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    s: &Scope,
    hub: &mut Hub,
    lists: &Lists,
    in_workspace: &dyn Fn(Entity) -> bool,
    view: &View,
    dt: f32,
    out: &mut RenderFrame,
) {
    let dt = if dt.is_finite() {
        dt.clamp(0.0, 0.25)
    } else {
        0.0
    };
    let budget = hub.settings.effect_budget as usize;
    let st = &mut hub.effects;
    for e in std::mem::take(&mut st.clears) {
        if let Some(em) = st.emitters.get_mut(&e) {
            em.particles.clear();
        }
        st.trails.remove(&e);
    }
    let emits = std::mem::take(&mut st.emits);
    let alive: HashSet<Entity> = lists
        .emitters
        .iter()
        .chain(&lists.trails)
        .copied()
        .filter(|&e| in_workspace(e))
        .collect();
    st.emitters.retain(|e, _| alive.contains(e));
    st.trails.retain(|e, _| alive.contains(e));

    let mut ribbons = Vec::new();
    for &e in &lists.beams {
        if in_workspace(e) {
            ribbons.extend(beam(s, e));
        }
    }
    for &e in &lists.trails {
        if in_workspace(e) {
            ribbons.extend(trail(s, st, e, dt));
        }
    }
    let mut sprites = Vec::new();
    for &e in &lists.emitters {
        if !in_workspace(e) {
            continue;
        }
        let n = emits.iter().filter(|(x, _)| *x == e).map(|(_, n)| *n).sum();
        emitter(s, st, e, view, dt, n, &mut sprites);
    }

    // The budget: ribbons first (whole), then particles.
    let mut used = 0usize;
    let mut dropped = 0usize;
    for r in ribbons {
        let pieces = r.points.len() - 1;
        if used + pieces <= budget {
            used += pieces;
            out.ribbons.push(r);
        } else {
            dropped += pieces;
        }
    }
    let room = budget - used;
    if sprites.len() > room {
        dropped += sprites.len() - room;
        sprites.truncate(room);
    }
    used += sprites.len();
    out.sprites = sprites;
    out.effects.drawn = used as u32;
    out.effects.dropped = dropped as u32;
    if dropped > 0 && !st.over {
        out.diagnostics.push(
            Diagnostic::warning(
                crate::codes::EFFECT_BUDGET,
                format!(
                    "{} particles and beam or trail segments were not drawn: the frame has {} over the effect budget of {budget}",
                    dropped, dropped
                ),
            )
            .with_hint("lower emitter rates or lifetimes, or raise [compat] effect_budget"),
        );
    }
    st.over = dropped > 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_interpolate_between_keypoints() {
        let k = [(0.0, [0.0; 3]), (0.5, [1.0; 3]), (1.0, [0.0; 3])];
        assert_eq!(sample(&k, 0.25, [9.0; 3]), [0.5; 3]);
        assert_eq!(sample(&k, 0.5, [9.0; 3]), [1.0; 3]);
        assert_eq!(sample(&k, 2.0, [9.0; 3]), [0.0; 3]);
        assert_eq!(sample(&[], 0.3, [9.0; 3]), [9.0; 3]);
    }

    #[test]
    fn spread_stays_within_its_angle() {
        let mut rng = Pcg32::from_seed(1);
        for _ in 0..200 {
            let d = spread_dir(&mut rng, Vec3::Y, Vec2::new(10.0, 10.0));
            assert!(d.angle_between(Vec3::Y).to_degrees() <= 14.2 + 1e-3);
        }
    }

    #[test]
    fn a_curve_ends_at_its_attachments() {
        let p = [
            Vec3::ZERO,
            Vec3::X,
            Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(3.0, 0.0, 0.0),
        ];
        assert_eq!(cubic(p, 0.0), p[0]);
        assert!((cubic(p, 1.0) - p[3]).length() < 1e-6);
    }
}
