//! What both place-file readers produce: the instances, their decoded
//! property values and their parents, before any of it meets the class layer.

/// A `CFrame`: the position, then the rotation matrix row by row
/// (`R00 R01 R02 R10 … R22`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CFrame {
    pub position: [f32; 3],
    pub rotation: [f32; 9],
}

impl CFrame {
    /// The identity at the origin.
    pub const IDENTITY: CFrame = CFrame {
        position: [0.0; 3],
        rotation: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
    };

    /// Column `i` of the rotation (0 is the right vector, 1 up, 2 back).
    pub fn column(&self, i: usize) -> [f32; 3] {
        [self.rotation[i], self.rotation[3 + i], self.rotation[6 + i]]
    }
}

/// The axis-aligned rotations a binary `CFrame` names with one byte instead
/// of nine floats. The id is `6 * a + b + 1`, where `a` is the direction of
/// the matrix's first column and `b` of its second, each numbered `+X`,
/// `+Y`, `+Z`, `-X`, `-Y`, `-Z` from 0 to 5; the third column is their cross
/// product. The 24 right-handed combinations are the ids in use.
pub fn rotation_of_id(id: u8) -> Option<[f32; 9]> {
    if id < 1 {
        return None;
    }
    let n = id - 1;
    let (a, b) = ((n / 6) as usize, (n % 6) as usize);
    if a >= 6 || a % 3 == b % 3 {
        return None;
    }
    let axis = |k: usize| {
        let mut v = [0.0f32; 3];
        v[k % 3] = if k < 3 { 1.0 } else { -1.0 };
        v
    };
    let (x, y) = (axis(a), axis(b));
    let z = [
        x[1] * y[2] - x[2] * y[1],
        x[2] * y[0] - x[0] * y[2],
        x[0] * y[1] - x[1] * y[0],
    ];
    Some([x[0], y[0], z[0], x[1], y[1], z[1], x[2], y[2], z[2]])
}

/// The id [`rotation_of_id`] gives `rotation`, if it is one of the 24.
pub fn id_of_rotation(rotation: &[f32; 9]) -> Option<u8> {
    (2..=36u8).find(|&id| rotation_of_id(id).as_ref() == Some(rotation))
}

/// A `Font`: the family's content id, its weight (`100` to `900`), its
/// style (`Normal` or `Italic`) and a cached face id we do not use.
#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    pub family: String,
    pub weight: u16,
    pub style: String,
    pub cached_face: String,
}

/// A decoded property value, by the file's own type.
#[derive(Clone, Debug, PartialEq)]
pub enum PlaceValue {
    /// Text or bytes: names, script source, base64 or binary strings.
    String(Vec<u8>),
    Bool(bool),
    Int32(i32),
    Float32(f32),
    Float64(f64),
    UDim(f32, i32),
    /// `(x scale, x offset, y scale, y offset)`.
    UDim2(f32, i32, f32, i32),
    Ray {
        origin: [f32; 3],
        direction: [f32; 3],
    },
    /// One bit per face: `Right`, `Top`, `Back`, `Left`, `Bottom`, `Front`.
    Faces(u8),
    /// One bit per axis: `X`, `Y`, `Z`.
    Axes(u8),
    BrickColor(u32),
    Color3([f32; 3]),
    Vector2([f32; 2]),
    Vector3([f32; 3]),
    CFrame(CFrame),
    /// An enum item, by number.
    Enum(u32),
    /// Another instance of the file, by index into [`Dom::instances`].
    Ref(Option<usize>),
    Vector3int16([i16; 3]),
    /// Keypoints `(time, value, envelope)`.
    NumberSequence(Vec<[f32; 3]>),
    /// Keypoints `(time, colour)`; the envelope is not used.
    ColorSequence(Vec<(f32, [f32; 3])>),
    NumberRange(f32, f32),
    /// `(min x, min y, max x, max y)`.
    Rect([f32; 4]),
    /// `None` for the material's own; else density, friction, elasticity,
    /// friction weight, elasticity weight and acoustic absorption.
    PhysicalProperties(Option<[f32; 6]>),
    Color3uint8([u8; 3]),
    Int64(i64),
    SharedString(Vec<u8>),
    Bytecode(Vec<u8>),
    OptionalCFrame(Option<CFrame>),
    UniqueId([u8; 16]),
    Font(Font),
    /// A content value: a URI, or nothing.
    Content(Option<String>),
    SecurityCapabilities(u64),
}

impl PlaceValue {
    /// The file type's name, for reports.
    pub fn type_name(&self) -> &'static str {
        match self {
            PlaceValue::String(_) => "string",
            PlaceValue::Bool(_) => "bool",
            PlaceValue::Int32(_) => "int",
            PlaceValue::Float32(_) => "float",
            PlaceValue::Float64(_) => "double",
            PlaceValue::UDim(..) => "UDim",
            PlaceValue::UDim2(..) => "UDim2",
            PlaceValue::Ray { .. } => "Ray",
            PlaceValue::Faces(_) => "Faces",
            PlaceValue::Axes(_) => "Axes",
            PlaceValue::BrickColor(_) => "BrickColor",
            PlaceValue::Color3(_) => "Color3",
            PlaceValue::Vector2(_) => "Vector2",
            PlaceValue::Vector3(_) => "Vector3",
            PlaceValue::CFrame(_) => "CFrame",
            PlaceValue::Enum(_) => "token",
            PlaceValue::Ref(_) => "Ref",
            PlaceValue::Vector3int16(_) => "Vector3int16",
            PlaceValue::NumberSequence(_) => "NumberSequence",
            PlaceValue::ColorSequence(_) => "ColorSequence",
            PlaceValue::NumberRange(..) => "NumberRange",
            PlaceValue::Rect(_) => "Rect",
            PlaceValue::PhysicalProperties(_) => "PhysicalProperties",
            PlaceValue::Color3uint8(_) => "Color3uint8",
            PlaceValue::Int64(_) => "int64",
            PlaceValue::SharedString(_) => "SharedString",
            PlaceValue::Bytecode(_) => "bytecode",
            PlaceValue::OptionalCFrame(_) => "OptionalCFrame",
            PlaceValue::UniqueId(_) => "UniqueId",
            PlaceValue::Font(_) => "Font",
            PlaceValue::Content(_) => "Content",
            PlaceValue::SecurityCapabilities(_) => "SecurityCapabilities",
        }
    }
}

/// One instance of the file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Instance {
    pub class: String,
    /// Its properties under their serialised names, in file order.
    pub properties: Vec<(String, PlaceValue)>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

impl Instance {
    /// The value of property `name`, if the file has one.
    pub fn get(&self, name: &str) -> Option<&PlaceValue> {
        self.properties
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }
}

/// A whole file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dom {
    pub instances: Vec<Instance>,
    /// The instances with no parent, in file order.
    pub roots: Vec<usize>,
    /// The file's metadata pairs.
    pub meta: Vec<(String, String)>,
    /// What the reader skipped without failing: unknown value types
    /// (`RE9810`), unknown chunks.
    pub warnings: Vec<re_core::Diagnostic>,
}

impl Dom {
    /// Links every instance to its parent and fills [`Dom::roots`].
    pub fn link(&mut self, parents: &[Option<usize>]) {
        for (i, p) in parents.iter().enumerate() {
            self.instances[i].parent = *p;
            match p {
                Some(p) => self.instances[*p].children.push(i),
                None => self.roots.push(i),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_24_rotation_ids_are_right_handed_and_round_trip() {
        let ids: Vec<u8> = (0..=40).filter(|&i| rotation_of_id(i).is_some()).collect();
        assert_eq!(ids.len(), 24);
        assert_eq!(rotation_of_id(2), Some(CFrame::IDENTITY.rotation));
        // 0x03: the up vector points along +Z, the back vector along -Y.
        assert_eq!(
            rotation_of_id(3),
            Some([1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0])
        );
        for id in ids {
            let r = rotation_of_id(id).unwrap();
            assert_eq!(id_of_rotation(&r), Some(id));
        }
    }
}
