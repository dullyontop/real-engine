//! `CFrame`: a position and a rotation, in `f32`, as in Roblox.
//!
//! The rotation is a 3×3 matrix `r` in row-major order. Its columns are the
//! frame's axes: `RightVector` (X), `UpVector` (Y) and `-LookVector` (Z), so a
//! frame looks down its −Z axis. A point `v` in the frame's space is at
//! `r * v + p` in world space.

use mlua::{
    FromLua, Lua, MetaMethod, MultiValue, Result, Table, UserData, UserDataFields, UserDataMethods,
    Value,
};

use super::{bad_argument, fmt_f32, num_arg, set_lib, strict, type_name, vec_arg, vec_value};

type V = [f32; 3];
type M = [[f32; 3]; 3];

const IDENTITY: M = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: V, s: f32) -> V {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V, b: V) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn length(a: V) -> f32 {
    dot(a, a).sqrt()
}
fn unit(a: V) -> V {
    let l = length(a);
    [a[0] / l, a[1] / l, a[2] / l]
}
fn mat_vec(r: &M, v: V) -> V {
    [
        r[0][0] * v[0] + r[0][1] * v[1] + r[0][2] * v[2],
        r[1][0] * v[0] + r[1][1] * v[1] + r[1][2] * v[2],
        r[2][0] * v[0] + r[2][1] * v[1] + r[2][2] * v[2],
    ]
}
fn mat_t_vec(r: &M, v: V) -> V {
    [
        r[0][0] * v[0] + r[1][0] * v[1] + r[2][0] * v[2],
        r[0][1] * v[0] + r[1][1] * v[1] + r[2][1] * v[2],
        r[0][2] * v[0] + r[1][2] * v[1] + r[2][2] * v[2],
    ]
}
fn mat_mul(a: &M, b: &M) -> M {
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, x) in row.iter_mut().enumerate() {
            *x = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    o
}
fn transpose(r: &M) -> M {
    [
        [r[0][0], r[1][0], r[2][0]],
        [r[0][1], r[1][1], r[2][1]],
        [r[0][2], r[1][2], r[2][2]],
    ]
}
fn from_columns(x: V, y: V, z: V) -> M {
    [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]]
}

/// A coordinate frame: an `f32` position and rotation matrix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CFrame {
    /// The position.
    pub p: [f32; 3],
    /// The rotation matrix, row-major: `r[row][column]`.
    pub r: [[f32; 3]; 3],
}

impl Default for CFrame {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// How far from orthonormal a rotation given by a script may be before the
/// constructors orthonormalize it.
const ORTHONORMAL_TOLERANCE: f32 = 1e-4;

impl CFrame {
    /// No translation, no rotation.
    pub const IDENTITY: CFrame = CFrame {
        p: [0.0; 3],
        r: IDENTITY,
    };

    /// A frame at `p` with no rotation.
    pub fn from_position(p: [f32; 3]) -> Self {
        Self { p, r: IDENTITY }
    }

    /// A frame from its position and rotation matrix, orthonormalized when the
    /// matrix is not already orthonormal (see [`CFrame::orthonormalize`]).
    pub fn from_parts(p: [f32; 3], r: [[f32; 3]; 3]) -> Self {
        let cf = Self { p, r };
        if cf.is_orthonormal() {
            cf
        } else {
            cf.orthonormalize()
        }
    }

    /// True if the columns are unit length and perpendicular, within a small
    /// tolerance.
    pub fn is_orthonormal(&self) -> bool {
        let [x, y, z] = [self.x_vector(), self.y_vector(), self.z_vector()];
        let t = ORTHONORMAL_TOLERANCE;
        (dot(x, x) - 1.0).abs() < t
            && (dot(y, y) - 1.0).abs() < t
            && (dot(z, z) - 1.0).abs() < t
            && dot(x, y).abs() < t
            && dot(y, z).abs() < t
            && dot(z, x).abs() < t
            && (dot(cross(x, y), z) - 1.0).abs() < t
    }

    /// The same frame with an orthonormal, right-handed rotation: X keeps its
    /// direction, Y is made perpendicular to X in the plane of X and Y, and Z
    /// is X × Y.
    pub fn orthonormalize(&self) -> Self {
        let x = unit(self.x_vector());
        let y0 = self.y_vector();
        let y = unit(sub(y0, scale(x, dot(x, y0))));
        let z = cross(x, y);
        Self {
            p: self.p,
            r: from_columns(x, y, z),
        }
    }

    /// A frame from a position and a quaternion (normalized first).
    pub fn from_quaternion(p: [f32; 3], q: [f32; 4]) -> Self {
        let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
        let [x, y, z, w] = [q[0] / n, q[1] / n, q[2] / n, q[3] / n];
        let r = [
            [
                1.0 - 2.0 * (y * y + z * z),
                2.0 * (x * y - z * w),
                2.0 * (x * z + y * w),
            ],
            [
                2.0 * (x * y + z * w),
                1.0 - 2.0 * (x * x + z * z),
                2.0 * (y * z - x * w),
            ],
            [
                2.0 * (x * z - y * w),
                2.0 * (y * z + x * w),
                1.0 - 2.0 * (x * x + y * y),
            ],
        ];
        Self { p, r }
    }

    /// The rotation as a unit quaternion `[x, y, z, w]` with `w >= 0`.
    pub fn quaternion(&self) -> [f32; 4] {
        let r = &self.r;
        let trace = r[0][0] + r[1][1] + r[2][2];
        let q = if trace > 0.0 {
            let s = (trace + 1.0).sqrt() * 2.0;
            [
                (r[2][1] - r[1][2]) / s,
                (r[0][2] - r[2][0]) / s,
                (r[1][0] - r[0][1]) / s,
                0.25 * s,
            ]
        } else if r[0][0] > r[1][1] && r[0][0] > r[2][2] {
            let s = (1.0 + r[0][0] - r[1][1] - r[2][2]).sqrt() * 2.0;
            [
                0.25 * s,
                (r[0][1] + r[1][0]) / s,
                (r[0][2] + r[2][0]) / s,
                (r[2][1] - r[1][2]) / s,
            ]
        } else if r[1][1] > r[2][2] {
            let s = (1.0 + r[1][1] - r[0][0] - r[2][2]).sqrt() * 2.0;
            [
                (r[0][1] + r[1][0]) / s,
                0.25 * s,
                (r[1][2] + r[2][1]) / s,
                (r[0][2] - r[2][0]) / s,
            ]
        } else {
            let s = (1.0 + r[2][2] - r[0][0] - r[1][1]).sqrt() * 2.0;
            [
                (r[0][2] + r[2][0]) / s,
                (r[1][2] + r[2][1]) / s,
                0.25 * s,
                (r[1][0] - r[0][1]) / s,
            ]
        };
        let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
        let sgn = if q[3] < 0.0 { -1.0 } else { 1.0 };
        [
            q[0] * sgn / n,
            q[1] * sgn / n,
            q[2] * sgn / n,
            q[3] * sgn / n,
        ]
    }

    /// A rotation of `angle` radians about the X axis.
    pub fn rotation_x(angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        Self {
            p: [0.0; 3],
            r: [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
        }
    }

    /// A rotation of `angle` radians about the Y axis.
    pub fn rotation_y(angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        Self {
            p: [0.0; 3],
            r: [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
        }
    }

    /// A rotation of `angle` radians about the Z axis.
    pub fn rotation_z(angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        Self {
            p: [0.0; 3],
            r: [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    /// `CFrame.Angles` / `fromEulerAnglesXYZ`: rotate about Z, then Y, then X
    /// (in the frame's terms, `Rx * Ry * Rz`).
    pub fn from_euler_xyz(rx: f32, ry: f32, rz: f32) -> Self {
        Self::rotation_x(rx) * Self::rotation_y(ry) * Self::rotation_z(rz)
    }

    /// `fromEulerAnglesYXZ` / `fromOrientation`: `Ry * Rx * Rz`.
    pub fn from_euler_yxz(rx: f32, ry: f32, rz: f32) -> Self {
        Self::rotation_y(ry) * Self::rotation_x(rx) * Self::rotation_z(rz)
    }

    /// A rotation of `angle` radians about `axis` (normalized first).
    pub fn from_axis_angle(axis: [f32; 3], angle: f32) -> Self {
        let [x, y, z] = unit(axis);
        let (s, c) = angle.sin_cos();
        let t = 1.0 - c;
        Self {
            p: [0.0; 3],
            r: [
                [t * x * x + c, t * x * y - s * z, t * x * z + s * y],
                [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
                [t * x * z - s * y, t * y * z + s * x, t * z * z + c],
            ],
        }
    }

    /// A frame at `at` looking at `target`, with `up` choosing the roll.
    ///
    /// When the look direction is parallel to `up`, the right vector is
    /// `look × Z` instead (`look × X` if that is degenerate too), so the
    /// result is still a valid rotation. When `at == target` the rotation is
    /// the identity.
    pub fn look_at(at: [f32; 3], target: [f32; 3], up: [f32; 3]) -> Self {
        let d = sub(target, at);
        if length(d) == 0.0 {
            return Self::from_position(at);
        }
        let look = unit(d);
        let mut right = cross(look, up);
        if length(right) < 1e-6 {
            right = cross(look, [0.0, 0.0, 1.0]);
            if length(right) < 1e-6 {
                right = cross(look, [1.0, 0.0, 0.0]);
            }
        }
        let right = unit(right);
        let up = unit(cross(right, look));
        Self {
            p: at,
            r: from_columns(right, up, scale(look, -1.0)),
        }
    }

    /// The inverse: `self * self.inverse()` is the identity (the rotation is
    /// transposed, so it assumes an orthonormal rotation, as in Roblox).
    pub fn inverse(&self) -> Self {
        let rt = transpose(&self.r);
        let p = mat_vec(&rt, self.p);
        Self {
            p: [-p[0], -p[1], -p[2]],
            r: rt,
        }
    }

    /// `self * v`: a point from this frame's space to world space.
    pub fn point_to_world(&self, v: [f32; 3]) -> [f32; 3] {
        add(mat_vec(&self.r, v), self.p)
    }

    /// A point from world space to this frame's space.
    pub fn point_to_object(&self, v: [f32; 3]) -> [f32; 3] {
        mat_t_vec(&self.r, sub(v, self.p))
    }

    /// A direction from this frame's space to world space (no translation).
    pub fn vector_to_world(&self, v: [f32; 3]) -> [f32; 3] {
        mat_vec(&self.r, v)
    }

    /// A direction from world space to this frame's space.
    pub fn vector_to_object(&self, v: [f32; 3]) -> [f32; 3] {
        mat_t_vec(&self.r, v)
    }

    /// The X column (`RightVector`, `XVector`).
    pub fn x_vector(&self) -> [f32; 3] {
        [self.r[0][0], self.r[1][0], self.r[2][0]]
    }

    /// The Y column (`UpVector`, `YVector`).
    pub fn y_vector(&self) -> [f32; 3] {
        [self.r[0][1], self.r[1][1], self.r[2][1]]
    }

    /// The Z column (`ZVector`; `LookVector` is its negation).
    pub fn z_vector(&self) -> [f32; 3] {
        [self.r[0][2], self.r[1][2], self.r[2][2]]
    }

    /// The look direction, −Z.
    pub fn look_vector(&self) -> [f32; 3] {
        scale(self.z_vector(), -1.0)
    }

    /// The angles `(rx, ry, rz)` with `from_euler_xyz(rx, ry, rz) == self`
    /// (up to rounding).
    pub fn to_euler_xyz(&self) -> (f32, f32, f32) {
        let r = &self.r;
        let sy = r[0][2].clamp(-1.0, 1.0);
        let ry = sy.asin();
        if sy.abs() < 0.999_999_9 {
            ((-r[1][2]).atan2(r[2][2]), ry, (-r[0][1]).atan2(r[0][0]))
        } else {
            // Gimbal lock: X and Z turn about the same axis; put it all in X.
            (r[2][1].atan2(r[1][1]), ry, 0.0)
        }
    }

    /// The angles `(rx, ry, rz)` with `from_euler_yxz(rx, ry, rz) == self`.
    pub fn to_euler_yxz(&self) -> (f32, f32, f32) {
        let r = &self.r;
        let sx = (-r[1][2]).clamp(-1.0, 1.0);
        let rx = sx.asin();
        if sx.abs() < 0.999_999_9 {
            (rx, r[0][2].atan2(r[2][2]), r[1][0].atan2(r[1][1]))
        } else {
            (rx, (-r[2][0]).atan2(r[0][0]), 0.0)
        }
    }

    /// The rotation as an axis and an angle in radians in `[0, π]`. The
    /// identity gives axis X and angle 0.
    pub fn to_axis_angle(&self) -> ([f32; 3], f32) {
        let [x, y, z, w] = self.quaternion();
        let s = (x * x + y * y + z * z).sqrt();
        if s < 1e-7 {
            return ([1.0, 0.0, 0.0], 0.0);
        }
        ([x / s, y / s, z / s], 2.0 * s.atan2(w))
    }

    /// Interpolates position linearly and rotation along the shortest arc;
    /// `alpha = 0` gives `self` exactly.
    pub fn lerp(&self, goal: &CFrame, alpha: f32) -> Self {
        let rel = transpose(&self.r);
        let rel = CFrame {
            p: [0.0; 3],
            r: mat_mul(&rel, &goal.r),
        };
        let (axis, angle) = rel.to_axis_angle();
        let r = if angle == 0.0 || alpha == 0.0 {
            self.r
        } else {
            mat_mul(&self.r, &CFrame::from_axis_angle(axis, angle * alpha).r)
        };
        let p = add(self.p, scale(sub(goal.p, self.p), alpha));
        Self { p, r }
    }

    /// The twelve components: `x, y, z, R00, R01, R02, R10, ..., R22`.
    pub fn components(&self) -> [f32; 12] {
        let r = &self.r;
        [
            self.p[0], self.p[1], self.p[2], r[0][0], r[0][1], r[0][2], r[1][0], r[1][1], r[1][2],
            r[2][0], r[2][1], r[2][2],
        ]
    }

    /// True if positions and rotations differ by at most `eps` per component.
    pub fn fuzzy_eq(&self, o: &CFrame, eps: f32) -> bool {
        self.components()
            .iter()
            .zip(o.components())
            .all(|(a, b)| (a - b).abs() <= eps)
    }
}

impl std::ops::Mul for CFrame {
    type Output = CFrame;
    fn mul(self, o: CFrame) -> CFrame {
        CFrame {
            p: add(mat_vec(&self.r, o.p), self.p),
            r: mat_mul(&self.r, &o.r),
        }
    }
}

impl FromLua for CFrame {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        match &value {
            Value::UserData(ud) if ud.is::<CFrame>() => Ok(*ud.borrow::<CFrame>()?),
            _ => Err(bad_argument(
                1,
                "CFrame",
                format!("CFrame expected, got {}", type_name(&value)),
            )),
        }
    }
}

fn cf_arg(v: &Value, n: usize, f: &str) -> Result<CFrame> {
    super::ud_arg::<CFrame>(v, n, f, "CFrame")
}

fn args(m: &MultiValue) -> Vec<Value> {
    m.iter().cloned().collect()
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).cloned().unwrap_or(Value::Nil)
}

fn nums<const N: usize>(a: &[Value], f: &str) -> Result<[f32; N]> {
    let mut o = [0.0; N];
    for (i, x) in o.iter_mut().enumerate() {
        *x = num_arg(&arg(a, i), i + 1, f, None)? as f32;
    }
    Ok(o)
}

/// `CFrame.new(...)` in all its forms.
fn new(a: &[Value]) -> Result<CFrame> {
    let n = a.iter().rposition(|v| !v.is_nil()).map_or(0, |i| i + 1);
    match n {
        0 => Ok(CFrame::IDENTITY),
        1 => Ok(CFrame::from_position(vec_arg(&a[0], 1, "new")?)),
        2 => {
            // Deprecated form: a position and a point to look at.
            let at = vec_arg(&a[0], 1, "new")?;
            let target = vec_arg(&a[1], 2, "new")?;
            Ok(CFrame::look_at(at, target, [0.0, 1.0, 0.0]))
        }
        3 => Ok(CFrame::from_position(nums::<3>(a, "new")?)),
        7 => {
            let v = nums::<7>(a, "new")?;
            Ok(CFrame::from_quaternion(
                [v[0], v[1], v[2]],
                [v[3], v[4], v[5], v[6]],
            ))
        }
        12 => {
            let v = nums::<12>(a, "new")?;
            Ok(CFrame::from_parts(
                [v[0], v[1], v[2]],
                [[v[3], v[4], v[5]], [v[6], v[7], v[8]], [v[9], v[10], v[11]]],
            ))
        }
        _ => Err(bad_argument(
            n,
            "new",
            format!("CFrame.new takes 0, 1, 2, 3, 7 or 12 arguments, got {n}"),
        )),
    }
}

fn angles(a: &[Value], f: &str) -> Result<[f32; 3]> {
    let mut o = [0.0; 3];
    for (i, x) in o.iter_mut().enumerate() {
        *x = num_arg(&arg(a, i), i + 1, f, Some(0.0))? as f32;
    }
    Ok(o)
}

/// Installs the `CFrame` library.
pub(crate) fn install(lua: &Lua, globals: &Table) -> Result<()> {
    let f = |func: fn(&[Value]) -> Result<CFrame>| {
        lua.create_function(move |_, m: MultiValue| func(&args(&m)))
            .map(Value::Function)
    };
    let identity = lua.create_userdata(CFrame::IDENTITY)?;
    set_lib(
        lua,
        globals,
        "CFrame",
        vec![
            ("new", f(new)?),
            ("identity", Value::UserData(identity)),
            (
                "Angles",
                f(|a| {
                    let [x, y, z] = angles(a, "Angles")?;
                    Ok(CFrame::from_euler_xyz(x, y, z))
                })?,
            ),
            (
                "fromEulerAnglesXYZ",
                f(|a| {
                    let [x, y, z] = angles(a, "fromEulerAnglesXYZ")?;
                    Ok(CFrame::from_euler_xyz(x, y, z))
                })?,
            ),
            (
                "fromEulerAnglesYXZ",
                f(|a| {
                    let [x, y, z] = angles(a, "fromEulerAnglesYXZ")?;
                    Ok(CFrame::from_euler_yxz(x, y, z))
                })?,
            ),
            (
                "fromOrientation",
                f(|a| {
                    let [x, y, z] = angles(a, "fromOrientation")?;
                    Ok(CFrame::from_euler_yxz(x, y, z))
                })?,
            ),
            (
                "fromEulerAngles",
                f(|a| {
                    // Roblox's default order for this form is XYZ.
                    let [x, y, z] = angles(a, "fromEulerAngles")?;
                    Ok(CFrame::from_euler_xyz(x, y, z))
                })?,
            ),
            (
                "lookAt",
                f(|a| {
                    let at = vec_arg(&arg(a, 0), 1, "lookAt")?;
                    let target = vec_arg(&arg(a, 1), 2, "lookAt")?;
                    let up = match arg(a, 2) {
                        Value::Nil => [0.0, 1.0, 0.0],
                        v => vec_arg(&v, 3, "lookAt")?,
                    };
                    Ok(CFrame::look_at(at, target, up))
                })?,
            ),
            (
                "lookAlong",
                f(|a| {
                    let at = vec_arg(&arg(a, 0), 1, "lookAlong")?;
                    let dir = vec_arg(&arg(a, 1), 2, "lookAlong")?;
                    let up = match arg(a, 2) {
                        Value::Nil => [0.0, 1.0, 0.0],
                        v => vec_arg(&v, 3, "lookAlong")?,
                    };
                    Ok(CFrame::look_at(at, add(at, dir), up))
                })?,
            ),
            (
                "fromAxisAngle",
                f(|a| {
                    let axis = vec_arg(&arg(a, 0), 1, "fromAxisAngle")?;
                    let angle = num_arg(&arg(a, 1), 2, "fromAxisAngle", None)? as f32;
                    Ok(CFrame::from_axis_angle(axis, angle))
                })?,
            ),
            (
                "fromMatrix",
                f(|a| {
                    let p = vec_arg(&arg(a, 0), 1, "fromMatrix")?;
                    let x = vec_arg(&arg(a, 1), 2, "fromMatrix")?;
                    let y = vec_arg(&arg(a, 2), 3, "fromMatrix")?;
                    let z = match arg(a, 3) {
                        Value::Nil => unit(cross(x, y)),
                        v => vec_arg(&v, 4, "fromMatrix")?,
                    };
                    Ok(CFrame::from_parts(p, from_columns(x, y, z)))
                })?,
            ),
        ],
    )
}

fn vecs(
    lua: &Lua,
    m: MultiValue,
    f: &str,
    op: impl Fn([f32; 3]) -> [f32; 3],
) -> Result<MultiValue> {
    let mut out = MultiValue::with_capacity(m.len());
    for (i, v) in m.iter().enumerate() {
        out.push_back(vec_value(op(vec_arg(v, i + 1, f)?)));
    }
    let _ = lua;
    Ok(out)
}

fn cfs(lua: &Lua, m: MultiValue, f: &str, op: impl Fn(CFrame) -> CFrame) -> Result<MultiValue> {
    let mut out = MultiValue::with_capacity(m.len());
    for (i, v) in m.iter().enumerate() {
        out.push_back(Value::UserData(lua.create_userdata(op(cf_arg(
            v,
            i + 1,
            f,
        )?))?));
    }
    Ok(out)
}

impl UserData for CFrame {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "CFrame");
        fields.add_field_method_get("Position", |_, c| Ok(vec_value(c.p)));
        fields.add_field_method_get("p", |_, c| Ok(vec_value(c.p)));
        fields.add_field_method_get("X", |_, c| Ok(c.p[0]));
        fields.add_field_method_get("Y", |_, c| Ok(c.p[1]));
        fields.add_field_method_get("Z", |_, c| Ok(c.p[2]));
        fields.add_field_method_get("x", |_, c| Ok(c.p[0]));
        fields.add_field_method_get("y", |_, c| Ok(c.p[1]));
        fields.add_field_method_get("z", |_, c| Ok(c.p[2]));
        fields.add_field_method_get("LookVector", |_, c| Ok(vec_value(c.look_vector())));
        fields.add_field_method_get("lookVector", |_, c| Ok(vec_value(c.look_vector())));
        fields.add_field_method_get("RightVector", |_, c| Ok(vec_value(c.x_vector())));
        fields.add_field_method_get("rightVector", |_, c| Ok(vec_value(c.x_vector())));
        fields.add_field_method_get("UpVector", |_, c| Ok(vec_value(c.y_vector())));
        fields.add_field_method_get("upVector", |_, c| Ok(vec_value(c.y_vector())));
        fields.add_field_method_get("XVector", |_, c| Ok(vec_value(c.x_vector())));
        fields.add_field_method_get("YVector", |_, c| Ok(vec_value(c.y_vector())));
        fields.add_field_method_get("ZVector", |_, c| Ok(vec_value(c.z_vector())));
        fields.add_field_method_get("Rotation", |_, c| {
            Ok(CFrame {
                p: [0.0; 3],
                r: c.r,
            })
        });
        for (i, name) in [
            "R00", "R01", "R02", "R10", "R11", "R12", "R20", "R21", "R22",
        ]
        .into_iter()
        .enumerate()
        {
            fields.add_field_method_get(name, move |_, c| Ok(c.r[i / 3][i % 3]));
        }
    }

    fn add_methods<Me: UserDataMethods<Self>>(methods: &mut Me) {
        for name in ["Inverse", "inverse"] {
            methods.add_method(name, |_, c, ()| Ok(c.inverse()));
        }
        for name in ["Lerp", "lerp"] {
            methods.add_method(name, |_, c, (goal, alpha): (Value, Value)| {
                let goal = cf_arg(&goal, 1, "Lerp")?;
                let alpha = num_arg(&alpha, 2, "Lerp", None)? as f32;
                Ok(c.lerp(&goal, alpha))
            });
        }
        for name in ["ToWorldSpace", "toWorldSpace"] {
            methods.add_method(name, |lua, c, m: MultiValue| {
                cfs(lua, m, "ToWorldSpace", |o| *c * o)
            });
        }
        for name in ["ToObjectSpace", "toObjectSpace"] {
            methods.add_method(name, |lua, c, m: MultiValue| {
                let inv = c.inverse();
                cfs(lua, m, "ToObjectSpace", |o| inv * o)
            });
        }
        for name in ["PointToWorldSpace", "pointToWorldSpace"] {
            methods.add_method(name, |lua, c, m: MultiValue| {
                vecs(lua, m, "PointToWorldSpace", |v| c.point_to_world(v))
            });
        }
        for name in ["PointToObjectSpace", "pointToObjectSpace"] {
            methods.add_method(name, |lua, c, m: MultiValue| {
                vecs(lua, m, "PointToObjectSpace", |v| c.point_to_object(v))
            });
        }
        for name in ["VectorToWorldSpace", "vectorToWorldSpace"] {
            methods.add_method(name, |lua, c, m: MultiValue| {
                vecs(lua, m, "VectorToWorldSpace", |v| c.vector_to_world(v))
            });
        }
        for name in ["VectorToObjectSpace", "vectorToObjectSpace"] {
            methods.add_method(name, |lua, c, m: MultiValue| {
                vecs(lua, m, "VectorToObjectSpace", |v| c.vector_to_object(v))
            });
        }
        for name in ["ToEulerAnglesXYZ", "toEulerAnglesXYZ", "ToEulerAngles"] {
            methods.add_method(name, |_, c, ()| Ok(c.to_euler_xyz()));
        }
        for name in ["ToEulerAnglesYXZ", "toEulerAnglesYXZ", "ToOrientation"] {
            methods.add_method(name, |_, c, ()| Ok(c.to_euler_yxz()));
        }
        for name in ["ToAxisAngle", "toAxisAngle"] {
            methods.add_method(name, |_, c, ()| {
                let (axis, angle) = c.to_axis_angle();
                Ok((vec_value(axis), angle))
            });
        }
        for name in ["GetComponents", "components"] {
            methods.add_method(name, |_, c, ()| {
                Ok(MultiValue::from_iter(
                    c.components().into_iter().map(|x| Value::Number(x as f64)),
                ))
            });
        }
        methods.add_method("Orthonormalize", |_, c, ()| Ok(c.orthonormalize()));
        methods.add_method("FuzzyEq", |_, c, (o, eps): (Value, Value)| {
            let o = cf_arg(&o, 1, "FuzzyEq")?;
            let eps = num_arg(&eps, 2, "FuzzyEq", Some(1e-5))? as f32;
            Ok(c.fuzzy_eq(&o, eps))
        });
        methods.add_meta_function(MetaMethod::Mul, |lua, (a, b): (Value, Value)| {
            let a = cf_arg(&a, 1, "mul")?;
            match &b {
                Value::Vector(_) => Ok(vec_value(a.point_to_world(vec_arg(&b, 2, "mul")?))),
                Value::UserData(ud) if ud.is::<CFrame>() => {
                    let b = *ud.borrow::<CFrame>()?;
                    Ok(Value::UserData(lua.create_userdata(a * b)?))
                }
                other => Err(super::error(
                    re_script::codes::BAD_ARGUMENT,
                    format!("attempt to multiply a CFrame with a {}", type_name(other)),
                )),
            }
        });
        methods.add_meta_function(MetaMethod::Add, |_, (a, b): (Value, Value)| {
            let (c, v) = match (&a, &b) {
                (Value::UserData(_), Value::Vector(_)) => {
                    (cf_arg(&a, 1, "add")?, vec_arg(&b, 2, "add")?)
                }
                (Value::Vector(_), Value::UserData(_)) => {
                    (cf_arg(&b, 2, "add")?, vec_arg(&a, 1, "add")?)
                }
                _ => {
                    return Err(super::error(
                        re_script::codes::BAD_ARGUMENT,
                        format!("attempt to add {} and {}", type_name(&a), type_name(&b)),
                    ));
                }
            };
            Ok(CFrame {
                p: add(c.p, v),
                r: c.r,
            })
        });
        methods.add_meta_function(MetaMethod::Sub, |_, (a, b): (Value, Value)| {
            let c = cf_arg(&a, 1, "sub")?;
            let v = match &b {
                Value::Vector(_) => vec_arg(&b, 2, "sub")?,
                other => {
                    return Err(super::error(
                        re_script::codes::BAD_ARGUMENT,
                        format!("attempt to subtract a {} from a CFrame", type_name(other)),
                    ));
                }
            };
            Ok(CFrame {
                p: sub(c.p, v),
                r: c.r,
            })
        });
        methods.add_meta_method(MetaMethod::Eq, |_, c, o: CFrame| Ok(*c == o));
        methods.add_meta_method(MetaMethod::ToString, |_, c, ()| {
            Ok(c.components().map(fmt_f32).join(", "))
        });
        strict(methods, "CFrame");
    }
}
