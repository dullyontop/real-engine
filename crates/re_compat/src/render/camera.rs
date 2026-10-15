//! `Workspace.CurrentCamera` as the frame's camera, the camera's projection
//! methods, and the `Ray` datatype they return.

use std::sync::Arc;

use glam::{Mat4, Quat, Vec2, Vec3};
use mlua::{Lua, MultiValue, UserData, UserDataFields, UserDataMethods, Value as LuaValue};
use re_scene::Entity;
use re_types::render::Camera;

use crate::datatypes::{bad_argument, num_arg, vec_arg, vec_value};
use crate::gui::GuiCamera;
use crate::gui::props::Scope;
use crate::instance::methods_api::ret;
use crate::runtime::{Bindings, state, with_world};

/// The near and far clip distances of the view, in studs.
pub const NEAR: f32 = 0.1;
pub const FAR: f32 = 10_000.0;

/// Where the camera is and how it sees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub position: Vec3,
    pub rotation: Quat,
    /// Vertical field of view in radians.
    pub fov_y: f32,
}

impl Default for View {
    fn default() -> Self {
        View {
            position: Vec3::new(0.0, 20.0, 20.0),
            rotation: Quat::IDENTITY,
            fov_y: 70f32.to_radians(),
        }
    }
}

impl View {
    pub fn camera(&self, _viewport: Vec2) -> Camera {
        Camera {
            world: Mat4::from_rotation_translation(self.rotation, self.position),
            fov_y_radians: self.fov_y,
            near: NEAR,
            far: FAR,
        }
    }

    /// The UI's view of the same camera, for billboards.
    pub fn gui(&self) -> GuiCamera {
        GuiCamera {
            position: self.position,
            rotation: self.rotation,
            fov_y: self.fov_y,
        }
    }

    /// Pixels per unit of tangent at the centre of a `viewport`-high view.
    fn focal(&self, viewport: Vec2) -> f32 {
        viewport.y * 0.5 / (self.fov_y * 0.5).tan()
    }

    /// The viewport point of `p` (x right, y down, in pixels) and its depth
    /// in front of the camera (negative behind it).
    pub fn project(&self, p: Vec3, viewport: Vec2) -> (Vec2, f32) {
        let v = self.rotation.inverse() * (p - self.position);
        let depth = -v.z;
        let d = if depth.abs() < 1e-6 {
            1e-6
        } else {
            depth.abs()
        };
        let f = self.focal(viewport);
        (
            Vec2::new(
                viewport.x * 0.5 + f * v.x / d,
                viewport.y * 0.5 - f * v.y / d,
            ),
            depth,
        )
    }

    /// The unit direction through viewport point `q`.
    pub fn ray(&self, q: Vec2, viewport: Vec2) -> Vec3 {
        let f = self.focal(viewport);
        let local = Vec3::new(
            (q.x - viewport.x * 0.5) / f,
            -(q.y - viewport.y * 0.5) / f,
            -1.0,
        );
        (self.rotation * local).normalize()
    }
}

/// The view of camera `e`, the default view without one.
pub(crate) fn read(s: &Scope, e: Option<Entity>) -> View {
    let Some(e) = e.filter(|&e| s.is_a(e, "Camera")) else {
        return View::default();
    };
    let p = s.props(e);
    let mut v = View::default();
    if let Some(pose) = p.get("CFrame").and_then(|c| crate::physics::pose_of(&c))
        && pose.pos.is_finite()
        && pose.rot.is_finite()
    {
        v.position = pose.pos;
        v.rotation = pose.rot.normalize();
    }
    v.fov_y = p.f32("FieldOfView", 70.0).clamp(1.0, 120.0).to_radians();
    v
}

/// A half-line: `Ray.new(origin, direction)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    /// Where it starts.
    pub origin: Vec3,
    /// Where it goes; its length matters to `Workspace:Raycast`.
    pub direction: Vec3,
}

impl Ray {
    /// The point of the ray nearest `p`.
    pub fn closest_point(&self, p: Vec3) -> Vec3 {
        let d = self.direction.normalize_or_zero();
        self.origin + d * (p - self.origin).dot(d).max(0.0)
    }
}

impl UserData for Ray {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "Ray");
        fields.add_field_method_get("Origin", |_, r| Ok(vec_value(r.origin.to_array())));
        fields.add_field_method_get("Direction", |_, r| Ok(vec_value(r.direction.to_array())));
        fields.add_field_method_get("Unit", |_, r| {
            Ok(Ray {
                origin: r.origin,
                direction: r.direction.normalize_or_zero(),
            })
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("ClosestPoint", |_, r, p: LuaValue| {
            let p = Vec3::from_array(vec_arg(&p, 1, "ClosestPoint")?);
            Ok(vec_value(r.closest_point(p).to_array()))
        });
        methods.add_method("Distance", |_, r, p: LuaValue| {
            let p = Vec3::from_array(vec_arg(&p, 1, "Distance")?);
            Ok(f64::from(r.closest_point(p).distance(p)))
        });
        methods.add_meta_method("__tostring", |_, r, ()| {
            let f = |v: Vec3| format!("{}, {}, {}", v.x, v.y, v.z);
            Ok(format!("{{{}}}, {{{}}}", f(r.origin), f(r.direction)))
        });
        methods.add_meta_method("__eq", |_, r, o: LuaValue| {
            Ok(matches!(o, LuaValue::UserData(ud) if ud.borrow::<Ray>().is_ok_and(|o| *o == *r)))
        });
    }
}

fn camera_view(lua: &Lua, e: Entity) -> mlua::Result<(View, Vec2, f32)> {
    let classes = state(lua)?.setup.classes.clone();
    let view = with_world(lua, |w| read(&Scope::new(w, &classes), Some(e)))?;
    let (viewport, inset) = super::hub_mut(lua, |h| (h.viewport, h.inset));
    Ok((view, viewport, inset))
}

fn point_arg(a: &MultiValue, what: &str) -> mlua::Result<Vec3> {
    let v = a.front().cloned().unwrap_or(LuaValue::Nil);
    Ok(Vec3::from_array(vec_arg(&v, 1, what)?))
}

fn xy_depth(a: &MultiValue, what: &str) -> mlua::Result<(Vec2, f32)> {
    let at = |i: usize| a.get(i).cloned().unwrap_or(LuaValue::Nil);
    let x = num_arg(&at(0), 1, what, None)?;
    let y = num_arg(&at(1), 2, what, None)?;
    let depth = num_arg(&at(2), 3, what, Some(0.0))?;
    if !(x.is_finite() && y.is_finite() && depth.is_finite()) {
        return Err(bad_argument(1, what, "the point is not finite"));
    }
    Ok((Vec2::new(x as f32, y as f32), depth as f32))
}

/// `(Vector3(x, y, depth), on screen)` for a point, `inset` pixels taken off
/// y (the GUI inset for screen points, 0 for viewport points).
fn to_point(
    lua: &Lua,
    e: Entity,
    a: &MultiValue,
    what: &str,
    screen: bool,
) -> mlua::Result<MultiValue> {
    let p = point_arg(a, what)?;
    let (view, viewport, inset) = camera_view(lua, e)?;
    let (q, depth) = view.project(p, viewport);
    let on = depth > 0.0 && q.x >= 0.0 && q.y >= 0.0 && q.x <= viewport.x && q.y <= viewport.y;
    let y = if screen { q.y - inset } else { q.y };
    ret(lua, (vec_value([q.x, y, depth]), on))
}

fn to_ray(
    lua: &Lua,
    e: Entity,
    a: &MultiValue,
    what: &str,
    screen: bool,
) -> mlua::Result<MultiValue> {
    let (mut q, depth) = xy_depth(a, what)?;
    let (view, viewport, inset) = camera_view(lua, e)?;
    if screen {
        q.y += inset;
    }
    let d = view.ray(q, viewport);
    ret(
        lua,
        Ray {
            origin: view.position + d * depth,
            direction: d,
        },
    )
}

pub(crate) fn register(b: &mut Bindings) {
    b.method(
        "Camera",
        "WorldToScreenPoint",
        Arc::new(|lua, e, a| to_point(lua, e, &a, "WorldToScreenPoint", true)),
    );
    b.method(
        "Camera",
        "WorldToViewportPoint",
        Arc::new(|lua, e, a| to_point(lua, e, &a, "WorldToViewportPoint", false)),
    );
    b.method(
        "Camera",
        "ScreenPointToRay",
        Arc::new(|lua, e, a| to_ray(lua, e, &a, "ScreenPointToRay", true)),
    );
    b.method(
        "Camera",
        "ViewportPointToRay",
        Arc::new(|lua, e, a| to_ray(lua, e, &a, "ViewportPointToRay", false)),
    );
    b.method(
        "Camera",
        "GetRenderCFrame",
        Arc::new(|lua, e, _| {
            let (view, _, _) = camera_view(lua, e)?;
            let pose = re_physics::Pose::new(view.position, view.rotation);
            ret(
                lua,
                crate::datatypes::to_lua(lua, "CFrame", crate::physics::cframe_value(&pose))?,
            )
        }),
    );
    b.global(
        "Ray",
        Arc::new(|lua, _| {
            let new = lua.create_function(|_, (o, d): (LuaValue, LuaValue)| {
                Ok(Ray {
                    origin: Vec3::from_array(vec_arg(&o, 1, "new")?),
                    direction: Vec3::from_array(vec_arg(&d, 2, "new")?),
                })
            })?;
            let t = lua.create_table()?;
            t.raw_set("new", new)?;
            t.set_readonly(true);
            Ok(LuaValue::Table(t))
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_and_rays_agree() {
        let view = View {
            position: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_rotation_y(0.4),
            fov_y: 70f32.to_radians(),
        };
        let vp = Vec2::new(800.0, 600.0);
        let p = Vec3::new(-2.0, 4.0, -12.0);
        let (q, depth) = view.project(p, vp);
        assert!(depth > 0.0);
        let d = view.ray(q, vp);
        let along = view.position + d * (p - view.position).length();
        assert!((along - p).length() < 1e-3, "{along} vs {p}");
        // The centre looks straight ahead.
        let ahead = view.ray(vp * 0.5, vp);
        assert!((ahead - view.rotation * -Vec3::Z).length() < 1e-5);
        // Behind the camera, the depth is negative.
        assert!(view.project(view.position + view.rotation * Vec3::Z, vp).1 < 0.0);
    }

    #[test]
    fn closest_point_stays_on_the_ray() {
        let r = Ray {
            origin: Vec3::ZERO,
            direction: Vec3::X * 5.0,
        };
        assert_eq!(
            r.closest_point(Vec3::new(3.0, 4.0, 0.0)),
            Vec3::new(3.0, 0.0, 0.0)
        );
        assert_eq!(r.closest_point(Vec3::new(-3.0, 4.0, 0.0)), Vec3::ZERO);
    }
}
