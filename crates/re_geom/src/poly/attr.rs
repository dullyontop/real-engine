//! Attribute columns: named values per point, corner or face (ADR-0062 §3).

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::GeomError;

/// The texture coordinate in metres: corner, `vec2`.
pub const UV: &str = "uv";
/// The shading normal: corner, `vec3`.
pub const NORMAL: &str = "normal";
/// The linear vertex colour: point, corner or face; `vec3` or `vec4`.
pub const COLOR: &str = "color";
/// The index into [`PolyMesh::materials`](super::PolyMesh::materials): face, `int`.
pub const MATERIAL: &str = "material";

/// The longest attribute name, in bytes.
pub const MAX_NAME: usize = 32;

/// What a column has one value per.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrDomain {
    /// One value per point.
    Point,
    /// One value per corner (a point's use by a face).
    Corner,
    /// One value per face.
    Face,
}

impl AttrDomain {
    /// `point`, `corner` or `face`.
    pub fn name(self) -> &'static str {
        match self {
            AttrDomain::Point => "point",
            AttrDomain::Corner => "corner",
            AttrDomain::Face => "face",
        }
    }
}

/// The type of a column's values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    /// `f32`.
    Float,
    /// `i32`.
    Int,
    /// `bool`.
    Bool,
    /// Two `f32`s.
    Vec2,
    /// Three `f32`s.
    Vec3,
    /// Four `f32`s.
    Vec4,
    /// A unit quaternion.
    Quat,
}

impl ColumnType {
    /// `float`, `int`, `bool`, `vec2`, `vec3`, `vec4` or `quat`.
    pub fn name(self) -> &'static str {
        match self {
            ColumnType::Float => "float",
            ColumnType::Int => "int",
            ColumnType::Bool => "bool",
            ColumnType::Vec2 => "vec2",
            ColumnType::Vec3 => "vec3",
            ColumnType::Vec4 => "vec4",
            ColumnType::Quat => "quat",
        }
    }
}

/// One value of a column.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrValue {
    /// A `float`.
    Float(f32),
    /// An `int`.
    Int(i32),
    /// A `bool`.
    Bool(bool),
    /// A `vec2`.
    Vec2([f32; 2]),
    /// A `vec3`.
    Vec3(Vec3),
    /// A `vec4`.
    Vec4([f32; 4]),
    /// A `quat`.
    Quat(Quat),
}

impl AttrValue {
    /// The value's type.
    pub fn ty(&self) -> ColumnType {
        match self {
            AttrValue::Float(_) => ColumnType::Float,
            AttrValue::Int(_) => ColumnType::Int,
            AttrValue::Bool(_) => ColumnType::Bool,
            AttrValue::Vec2(_) => ColumnType::Vec2,
            AttrValue::Vec3(_) => ColumnType::Vec3,
            AttrValue::Vec4(_) => ColumnType::Vec4,
            AttrValue::Quat(_) => ColumnType::Quat,
        }
    }

    /// True if every component is finite (always for `int` and `bool`).
    pub fn is_finite(&self) -> bool {
        match self {
            AttrValue::Float(v) => v.is_finite(),
            AttrValue::Int(_) | AttrValue::Bool(_) => true,
            AttrValue::Vec2(v) => v.iter().all(|x| x.is_finite()),
            AttrValue::Vec3(v) => v.is_finite(),
            AttrValue::Vec4(v) => v.iter().all(|x| x.is_finite()),
            AttrValue::Quat(q) => q.is_finite(),
        }
    }

    /// The value a column named `name` of type `ty` gives an element it has
    /// no value for: white for `color`, else zero, `false` or the identity.
    pub fn default_for(name: &str, ty: ColumnType) -> AttrValue {
        match ty {
            ColumnType::Float => AttrValue::Float(0.0),
            ColumnType::Int => AttrValue::Int(0),
            ColumnType::Bool => AttrValue::Bool(false),
            ColumnType::Vec2 => AttrValue::Vec2([0.0; 2]),
            ColumnType::Vec3 if name == COLOR => AttrValue::Vec3(Vec3::ONE),
            ColumnType::Vec3 => AttrValue::Vec3(Vec3::ZERO),
            ColumnType::Vec4 if name == COLOR => AttrValue::Vec4([1.0; 4]),
            ColumnType::Vec4 => AttrValue::Vec4([0.0; 4]),
            ColumnType::Quat => AttrValue::Quat(Quat::IDENTITY),
        }
    }
}

/// The values of one attribute, one per element of its domain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    /// `float` values.
    Float(Vec<f32>),
    /// `int` values.
    Int(Vec<i32>),
    /// `bool` values: a group, on faces.
    Bool(Vec<bool>),
    /// `vec2` values.
    Vec2(Vec<[f32; 2]>),
    /// `vec3` values.
    Vec3(Vec<Vec3>),
    /// `vec4` values.
    Vec4(Vec<[f32; 4]>),
    /// `quat` values.
    Quat(Vec<Quat>),
}

/// Applies `$body` to the vector inside a [`Column`], whatever its type.
macro_rules! each {
    ($col:expr, $v:ident => $body:expr) => {
        match $col {
            Column::Float($v) => $body,
            Column::Int($v) => $body,
            Column::Bool($v) => $body,
            Column::Vec2($v) => $body,
            Column::Vec3($v) => $body,
            Column::Vec4($v) => $body,
            Column::Quat($v) => $body,
        }
    };
}

impl Column {
    /// `len` copies of `value`.
    pub fn filled(value: AttrValue, len: usize) -> Column {
        match value {
            AttrValue::Float(v) => Column::Float(vec![v; len]),
            AttrValue::Int(v) => Column::Int(vec![v; len]),
            AttrValue::Bool(v) => Column::Bool(vec![v; len]),
            AttrValue::Vec2(v) => Column::Vec2(vec![v; len]),
            AttrValue::Vec3(v) => Column::Vec3(vec![v; len]),
            AttrValue::Vec4(v) => Column::Vec4(vec![v; len]),
            AttrValue::Quat(v) => Column::Quat(vec![v; len]),
        }
    }

    /// An empty column of type `ty` with room for `capacity` values.
    pub fn with_capacity(ty: ColumnType, capacity: usize) -> Column {
        match ty {
            ColumnType::Float => Column::Float(Vec::with_capacity(capacity)),
            ColumnType::Int => Column::Int(Vec::with_capacity(capacity)),
            ColumnType::Bool => Column::Bool(Vec::with_capacity(capacity)),
            ColumnType::Vec2 => Column::Vec2(Vec::with_capacity(capacity)),
            ColumnType::Vec3 => Column::Vec3(Vec::with_capacity(capacity)),
            ColumnType::Vec4 => Column::Vec4(Vec::with_capacity(capacity)),
            ColumnType::Quat => Column::Quat(Vec::with_capacity(capacity)),
        }
    }

    /// The type of the values.
    pub fn ty(&self) -> ColumnType {
        match self {
            Column::Float(_) => ColumnType::Float,
            Column::Int(_) => ColumnType::Int,
            Column::Bool(_) => ColumnType::Bool,
            Column::Vec2(_) => ColumnType::Vec2,
            Column::Vec3(_) => ColumnType::Vec3,
            Column::Vec4(_) => ColumnType::Vec4,
            Column::Quat(_) => ColumnType::Quat,
        }
    }

    /// The number of values.
    pub fn len(&self) -> usize {
        each!(self, v => v.len())
    }

    /// True with no values.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The value at `i`.
    ///
    /// # Panics
    /// If `i` is out of range.
    pub fn get(&self, i: usize) -> AttrValue {
        match self {
            Column::Float(v) => AttrValue::Float(v[i]),
            Column::Int(v) => AttrValue::Int(v[i]),
            Column::Bool(v) => AttrValue::Bool(v[i]),
            Column::Vec2(v) => AttrValue::Vec2(v[i]),
            Column::Vec3(v) => AttrValue::Vec3(v[i]),
            Column::Vec4(v) => AttrValue::Vec4(v[i]),
            Column::Quat(v) => AttrValue::Quat(v[i]),
        }
    }

    /// Sets the value at `i`; `false` (and no change) if `value` has another
    /// type.
    ///
    /// # Panics
    /// If `i` is out of range.
    pub fn set(&mut self, i: usize, value: AttrValue) -> bool {
        match (self, value) {
            (Column::Float(v), AttrValue::Float(x)) => v[i] = x,
            (Column::Int(v), AttrValue::Int(x)) => v[i] = x,
            (Column::Bool(v), AttrValue::Bool(x)) => v[i] = x,
            (Column::Vec2(v), AttrValue::Vec2(x)) => v[i] = x,
            (Column::Vec3(v), AttrValue::Vec3(x)) => v[i] = x,
            (Column::Vec4(v), AttrValue::Vec4(x)) => v[i] = x,
            (Column::Quat(v), AttrValue::Quat(x)) => v[i] = x,
            _ => return false,
        }
        true
    }

    /// Appends `value`; `false` (and no change) if it has another type.
    pub fn push(&mut self, value: AttrValue) -> bool {
        match (self, value) {
            (Column::Float(v), AttrValue::Float(x)) => v.push(x),
            (Column::Int(v), AttrValue::Int(x)) => v.push(x),
            (Column::Bool(v), AttrValue::Bool(x)) => v.push(x),
            (Column::Vec2(v), AttrValue::Vec2(x)) => v.push(x),
            (Column::Vec3(v), AttrValue::Vec3(x)) => v.push(x),
            (Column::Vec4(v), AttrValue::Vec4(x)) => v.push(x),
            (Column::Quat(v), AttrValue::Quat(x)) => v.push(x),
            _ => return false,
        }
        true
    }

    /// True if every value is finite.
    pub fn is_finite(&self) -> bool {
        match self {
            Column::Float(v) => v.iter().all(|x| x.is_finite()),
            Column::Int(_) | Column::Bool(_) => true,
            Column::Vec2(v) => v.iter().flatten().all(|x| x.is_finite()),
            Column::Vec3(v) => v.iter().all(|x| x.is_finite()),
            Column::Vec4(v) => v.iter().flatten().all(|x| x.is_finite()),
            Column::Quat(v) => v.iter().all(|x| x.is_finite()),
        }
    }

    /// A column of the values at `indices`, in their order.
    pub fn gather(&self, indices: &[u32]) -> Column {
        match self {
            Column::Float(v) => Column::Float(indices.iter().map(|&i| v[i as usize]).collect()),
            Column::Int(v) => Column::Int(indices.iter().map(|&i| v[i as usize]).collect()),
            Column::Bool(v) => Column::Bool(indices.iter().map(|&i| v[i as usize]).collect()),
            Column::Vec2(v) => Column::Vec2(indices.iter().map(|&i| v[i as usize]).collect()),
            Column::Vec3(v) => Column::Vec3(indices.iter().map(|&i| v[i as usize]).collect()),
            Column::Vec4(v) => Column::Vec4(indices.iter().map(|&i| v[i as usize]).collect()),
            Column::Quat(v) => Column::Quat(indices.iter().map(|&i| v[i as usize]).collect()),
        }
    }

    /// Appends `src`'s value at `i` (same type).
    pub(crate) fn push_from(&mut self, src: &Column, i: usize) {
        match (self, src) {
            (Column::Float(v), Column::Float(s)) => v.push(s[i]),
            (Column::Int(v), Column::Int(s)) => v.push(s[i]),
            (Column::Bool(v), Column::Bool(s)) => v.push(s[i]),
            (Column::Vec2(v), Column::Vec2(s)) => v.push(s[i]),
            (Column::Vec3(v), Column::Vec3(s)) => v.push(s[i]),
            (Column::Vec4(v), Column::Vec4(s)) => v.push(s[i]),
            (Column::Quat(v), Column::Quat(s)) => v.push(s[i]),
            _ => unreachable!("columns of one layout share types"),
        }
    }

    /// Appends all of `src` (same type).
    pub(crate) fn extend_from(&mut self, src: &Column) {
        match (self, src) {
            (Column::Float(v), Column::Float(s)) => v.extend_from_slice(s),
            (Column::Int(v), Column::Int(s)) => v.extend_from_slice(s),
            (Column::Bool(v), Column::Bool(s)) => v.extend_from_slice(s),
            (Column::Vec2(v), Column::Vec2(s)) => v.extend_from_slice(s),
            (Column::Vec3(v), Column::Vec3(s)) => v.extend_from_slice(s),
            (Column::Vec4(v), Column::Vec4(s)) => v.extend_from_slice(s),
            (Column::Quat(v), Column::Quat(s)) => v.extend_from_slice(s),
            _ => unreachable!("columns of one layout share types"),
        }
    }

    /// Keeps the values whose `keep` entry is true.
    pub(crate) fn retain(&mut self, keep: &[bool]) {
        each!(self, v => {
            let mut i = 0;
            v.retain(|_| {
                i += 1;
                keep[i - 1]
            });
        });
    }

    /// Reverses the values in `range`.
    pub(crate) fn reverse(&mut self, range: std::ops::Range<usize>) {
        each!(self, v => v[range].reverse());
    }
}

/// Checks an attribute name: lowercase letters, digits and `_`, starting
/// with a letter, at most [`MAX_NAME`] bytes, and not `position`.
pub fn check_name(name: &str) -> Result<(), GeomError> {
    let bad = |why: &str| {
        Err(GeomError::Argument(format!(
            "attribute name `{name}` {why}"
        )))
    };
    if name.is_empty() || name.len() > MAX_NAME {
        return bad(&format!("must be 1 to {MAX_NAME} bytes"));
    }
    if !name.as_bytes()[0].is_ascii_lowercase() {
        return bad("must start with a lowercase letter");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return bad("may hold only lowercase letters, digits and `_`");
    }
    if name == "position" {
        return bad("is reserved for the points' positions");
    }
    Ok(())
}

/// Checks that an attribute `name` of `domain` may hold values of type `ty`:
/// a valid name ([`check_name`]) and, for a standard attribute, its domain
/// and type (`Err`, `RE4510`).
pub fn check_attribute(name: &str, domain: AttrDomain, ty: ColumnType) -> Result<(), GeomError> {
    check_name(name)?;
    check_standard(name, domain, ty).map_err(GeomError::Argument)
}

/// Checks the type and domain the standard attributes must have.
pub(crate) fn check_standard(name: &str, domain: AttrDomain, ty: ColumnType) -> Result<(), String> {
    let want = match name {
        UV => Some((
            AttrDomain::Corner,
            "a corner `vec2`",
            ty == ColumnType::Vec2,
        )),
        NORMAL => Some((
            AttrDomain::Corner,
            "a corner `vec3`",
            ty == ColumnType::Vec3,
        )),
        MATERIAL => Some((AttrDomain::Face, "a face `int`", ty == ColumnType::Int)),
        COLOR => {
            let ok = matches!(ty, ColumnType::Vec3 | ColumnType::Vec4);
            if !ok {
                return Err(format!(
                    "`color` is a `vec3` or `vec4`, not a {} `{}`",
                    domain.name(),
                    ty.name()
                ));
            }
            None
        }
        _ => None,
    };
    match want {
        Some((d, what, ty_ok)) if d != domain || !ty_ok => Err(format!(
            "`{name}` is {what}, not a {} `{}`",
            domain.name(),
            ty.name()
        )),
        _ => Ok(()),
    }
}

/// Named columns of one domain, sorted by name.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Attributes {
    columns: Vec<(String, Column)>,
}

impl Attributes {
    /// No columns.
    pub fn new() -> Attributes {
        Attributes::default()
    }

    /// The number of columns.
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    /// True with no columns.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// The columns, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Column)> {
        self.columns.iter().map(|(n, c)| (n.as_str(), c))
    }

    /// The column names, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.columns.iter().map(|(n, _)| n.as_str())
    }

    fn find(&self, name: &str) -> Result<usize, usize> {
        self.columns.binary_search_by(|(n, _)| n.as_str().cmp(name))
    }

    /// The column named `name`.
    pub fn get(&self, name: &str) -> Option<&Column> {
        self.find(name).ok().map(|i| &self.columns[i].1)
    }

    /// The column named `name`, to change its values. Keep its length.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Column> {
        self.find(name).ok().map(|i| &mut self.columns[i].1)
    }

    /// True if there is a column named `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.find(name).is_ok()
    }

    /// Adds or replaces the column `name`, returning the one it replaced.
    /// `Err` (`RE4510`) for an invalid name ([`check_name`]); the column's
    /// length is checked with the mesh ([`PolyMesh::validate`](super::PolyMesh::validate)).
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        column: Column,
    ) -> Result<Option<Column>, GeomError> {
        let name = name.into();
        check_name(&name)?;
        Ok(match self.find(&name) {
            Ok(i) => Some(std::mem::replace(&mut self.columns[i].1, column)),
            Err(i) => {
                self.columns.insert(i, (name, column));
                None
            }
        })
    }

    /// Every column, to change or append values (names and order kept).
    pub(crate) fn columns_mut(&mut self) -> impl Iterator<Item = (&str, &mut Column)> {
        self.columns.iter_mut().map(|(n, c)| (n.as_str(), c))
    }

    /// Removes the column `name`.
    pub fn remove(&mut self, name: &str) -> Option<Column> {
        self.find(name).ok().map(|i| self.columns.remove(i).1)
    }

    /// The `float` column `name`.
    pub fn floats(&self, name: &str) -> Option<&[f32]> {
        match self.get(name) {
            Some(Column::Float(v)) => Some(v),
            _ => None,
        }
    }

    /// The `int` column `name`.
    pub fn ints(&self, name: &str) -> Option<&[i32]> {
        match self.get(name) {
            Some(Column::Int(v)) => Some(v),
            _ => None,
        }
    }

    /// The `bool` column `name`.
    pub fn bools(&self, name: &str) -> Option<&[bool]> {
        match self.get(name) {
            Some(Column::Bool(v)) => Some(v),
            _ => None,
        }
    }

    /// The `vec2` column `name`.
    pub fn vec2s(&self, name: &str) -> Option<&[[f32; 2]]> {
        match self.get(name) {
            Some(Column::Vec2(v)) => Some(v),
            _ => None,
        }
    }

    /// The `vec3` column `name`.
    pub fn vec3s(&self, name: &str) -> Option<&[Vec3]> {
        match self.get(name) {
            Some(Column::Vec3(v)) => Some(v),
            _ => None,
        }
    }

    /// The `vec4` column `name`.
    pub fn vec4s(&self, name: &str) -> Option<&[[f32; 4]]> {
        match self.get(name) {
            Some(Column::Vec4(v)) => Some(v),
            _ => None,
        }
    }

    /// The `quat` column `name`.
    pub fn quats(&self, name: &str) -> Option<&[Quat]> {
        match self.get(name) {
            Some(Column::Quat(v)) => Some(v),
            _ => None,
        }
    }

    /// The column `name` with its default for every new element, made if
    /// absent with `len` defaults; `Err` (`RE4510`) for an invalid name or a
    /// column of another type.
    pub fn ensure(
        &mut self,
        name: &str,
        ty: ColumnType,
        len: usize,
    ) -> Result<&mut Column, GeomError> {
        let i = match self.find(name) {
            Ok(i) => {
                let have = self.columns[i].1.ty();
                if have != ty {
                    return Err(GeomError::Argument(format!(
                        "attribute `{name}` is a `{}`, not a `{}`",
                        have.name(),
                        ty.name()
                    )));
                }
                i
            }
            Err(i) => {
                check_name(name)?;
                let column = Column::filled(AttrValue::default_for(name, ty), len);
                self.columns.insert(i, (name.to_string(), column));
                i
            }
        };
        Ok(&mut self.columns[i].1)
    }

    /// Checks every column: names, sorted order, length `len`, finite values
    /// and the standard attributes' types (`Err` is the message of `RE4511`).
    pub(crate) fn check(&self, domain: AttrDomain, len: usize) -> Result<(), String> {
        for (i, (name, col)) in self.columns.iter().enumerate() {
            check_name(name).map_err(|e| e.to_string())?;
            if i > 0 && self.columns[i - 1].0.as_str() >= name.as_str() {
                return Err(format!(
                    "{} attributes are not sorted by name (`{}` before `{name}`)",
                    domain.name(),
                    self.columns[i - 1].0
                ));
            }
            if col.len() != len {
                return Err(format!(
                    "{} attribute `{name}` has {} values for {len} {}s",
                    domain.name(),
                    col.len(),
                    domain.name()
                ));
            }
            if !col.is_finite() {
                return Err(format!(
                    "{} attribute `{name}` has a non-finite value",
                    domain.name()
                ));
            }
            check_standard(name, domain, col.ty())?;
        }
        Ok(())
    }

    /// The same columns with no values, room for `capacity` each.
    pub(crate) fn empty_like(&self, capacity: usize) -> Attributes {
        Attributes {
            columns: self
                .columns
                .iter()
                .map(|(n, c)| (n.clone(), Column::with_capacity(c.ty(), capacity)))
                .collect(),
        }
    }

    /// Every column's values at `indices`.
    pub(crate) fn gather(&self, indices: &[u32]) -> Attributes {
        Attributes {
            columns: self
                .columns
                .iter()
                .map(|(n, c)| (n.clone(), c.gather(indices)))
                .collect(),
        }
    }

    /// Appends `src`'s values at `i` to every column (same layout).
    pub(crate) fn push_from(&mut self, src: &Attributes, i: usize) {
        for ((_, c), (_, s)) in self.columns.iter_mut().zip(&src.columns) {
            c.push_from(s, i);
        }
    }

    /// Keeps the elements whose `keep` entry is true, in every column.
    pub(crate) fn retain(&mut self, keep: &[bool]) {
        for (_, c) in &mut self.columns {
            c.retain(keep);
        }
    }

    /// Reverses the elements in `range`, in every column.
    pub(crate) fn reverse(&mut self, range: std::ops::Range<usize>) {
        for (_, c) in &mut self.columns {
            c.reverse(range.clone());
        }
    }

    /// Columns of `layout`, empty, with room for `capacity` values each.
    pub(crate) fn from_layout(layout: &[(String, ColumnType)], capacity: usize) -> Attributes {
        Attributes {
            columns: layout
                .iter()
                .map(|(n, t)| (n.clone(), Column::with_capacity(*t, capacity)))
                .collect(),
        }
    }

    /// Appends `n` elements from `src`, whose columns may be any subset of
    /// this layout: a column `src` lacks takes its default.
    pub(crate) fn extend_partial(&mut self, src: &Attributes, n: usize) {
        for (name, c) in &mut self.columns {
            match src.get(name) {
                Some(s) => c.extend_from(s),
                None => {
                    let d = AttrValue::default_for(name, c.ty());
                    for _ in 0..n {
                        c.push(d);
                    }
                }
            }
        }
    }
}

/// The union of several layouts, sorted by name; `Err` (`RE4510`) when one
/// name has two types.
pub(crate) fn union_layout<'a>(
    all: impl IntoIterator<Item = &'a Attributes>,
    domain: AttrDomain,
) -> Result<Vec<(String, ColumnType)>, GeomError> {
    let mut out: Vec<(String, ColumnType)> = Vec::new();
    for attrs in all {
        for (name, col) in attrs.iter() {
            match out.binary_search_by(|(n, _)| n.as_str().cmp(name)) {
                Ok(i) if out[i].1 != col.ty() => {
                    return Err(GeomError::Argument(format!(
                        "{} attribute `{name}` is a `{}` in one mesh and a `{}` in another",
                        domain.name(),
                        out[i].1.name(),
                        col.ty().name()
                    )));
                }
                Ok(_) => {}
                Err(i) => out.insert(i, (name.to_string(), col.ty())),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["uv", "a", "floor_2", "x"] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "2a", "_a", "Ab", "a-b", "position", &"a".repeat(33)] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn columns_stay_sorted() {
        let mut a = Attributes::new();
        a.insert("b", Column::Float(vec![1.0])).unwrap();
        a.insert("a", Column::Int(vec![2])).unwrap();
        a.insert("c", Column::Bool(vec![true])).unwrap();
        assert_eq!(a.names().collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(a.ints("a"), Some(&[2][..]));
        assert!(a.check(AttrDomain::Face, 1).is_ok());
        assert!(a.check(AttrDomain::Face, 2).is_err());
        assert_eq!(a.remove("b"), Some(Column::Float(vec![1.0])));
    }

    #[test]
    fn standard_attributes_have_fixed_types() {
        let mut a = Attributes::new();
        a.insert(UV, Column::Vec3(vec![Vec3::ZERO])).unwrap();
        assert!(a.check(AttrDomain::Corner, 1).is_err());
        let mut a = Attributes::new();
        a.insert(UV, Column::Vec2(vec![[0.0; 2]])).unwrap();
        assert!(a.check(AttrDomain::Corner, 1).is_ok());
        assert!(a.check(AttrDomain::Point, 1).is_err());
        let mut a = Attributes::new();
        a.insert(COLOR, Column::Vec4(vec![[1.0; 4]])).unwrap();
        assert!(a.check(AttrDomain::Point, 1).is_ok());
        assert!(a.check(AttrDomain::Face, 1).is_ok());
    }

    #[test]
    fn color_defaults_to_white() {
        assert_eq!(
            AttrValue::default_for(COLOR, ColumnType::Vec3),
            AttrValue::Vec3(Vec3::ONE)
        );
        assert_eq!(
            AttrValue::default_for("tint", ColumnType::Vec3),
            AttrValue::Vec3(Vec3::ZERO)
        );
    }
}
