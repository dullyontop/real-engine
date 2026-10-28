//! `BrickColor`: Roblox's numbered, named colours.
//!
//! A `BrickColor` is one of the 208 colours of [`COLORS`], each with a number
//! (what places store and `BrickColor.new(n)` takes), a name and 0-255
//! channels. 128 of them make the palette Studio's colour picker shows,
//! which `BrickColor.palette` indexes and `BrickColor.random` draws from.
//! An unknown number or name gives "Medium stone grey", as in Roblox.
//!
//! Four names belong to two colours each ("Deep orange", "Gold", "Lilac" and
//! "Rust"); by name, the one in the palette is meant. A colour that is not a
//! brick colour matches the brick colour with the smallest sum of channel
//! differences, the lowest number on a tie. The stored form is the
//! `BrickColor` record, the colour's number.

use mlua::{
    FromLua, Lua, MetaMethod, Result, Table, UserData, UserDataFields, UserDataMethods, Value,
};

use super::color::Color3;
use super::random;
use super::{bad_argument, num_arg, set_lib, strict, type_name, ud_arg};

/// The brick colours in number order: number, name and 0-255 channels.
const COLORS: [(u16, &str, [u8; 3]); 208] = [
    (1, "White", [242, 243, 243]),
    (2, "Grey", [161, 165, 162]),
    (3, "Light yellow", [249, 233, 153]),
    (5, "Brick yellow", [215, 197, 154]),
    (6, "Light green (Mint)", [194, 218, 184]),
    (9, "Light reddish violet", [232, 186, 200]),
    (11, "Pastel Blue", [128, 187, 219]),
    (12, "Light orange brown", [203, 132, 66]),
    (18, "Nougat", [204, 142, 105]),
    (21, "Bright red", [196, 40, 28]),
    (22, "Med. reddish violet", [196, 112, 160]),
    (23, "Bright blue", [13, 105, 172]),
    (24, "Bright yellow", [245, 205, 48]),
    (25, "Earth orange", [98, 71, 50]),
    (26, "Black", [27, 42, 53]),
    (27, "Dark grey", [109, 110, 108]),
    (28, "Dark green", [40, 127, 71]),
    (29, "Medium green", [161, 196, 140]),
    (36, "Lig. Yellowich orange", [243, 207, 155]),
    (37, "Bright green", [75, 151, 75]),
    (38, "Dark orange", [160, 95, 53]),
    (39, "Light bluish violet", [193, 202, 222]),
    (40, "Transparent", [236, 236, 236]),
    (41, "Tr. Red", [205, 84, 75]),
    (42, "Tr. Lg blue", [193, 223, 240]),
    (43, "Tr. Blue", [123, 182, 232]),
    (44, "Tr. Yellow", [247, 241, 141]),
    (45, "Light blue", [180, 210, 228]),
    (47, "Tr. Flu. Reddish orange", [217, 133, 108]),
    (48, "Tr. Green", [132, 182, 141]),
    (49, "Tr. Flu. Green", [248, 241, 132]),
    (50, "Phosph. White", [236, 232, 222]),
    (100, "Light red", [238, 196, 182]),
    (101, "Medium red", [218, 134, 122]),
    (102, "Medium blue", [110, 153, 202]),
    (103, "Light grey", [199, 193, 183]),
    (104, "Bright violet", [107, 50, 124]),
    (105, "Br. yellowish orange", [226, 155, 64]),
    (106, "Bright orange", [218, 133, 65]),
    (107, "Bright bluish green", [0, 143, 156]),
    (108, "Earth yellow", [104, 92, 67]),
    (110, "Bright bluish violet", [67, 84, 147]),
    (111, "Tr. Brown", [191, 183, 177]),
    (112, "Medium bluish violet", [104, 116, 172]),
    (113, "Tr. Medi. reddish violet", [229, 173, 200]),
    (115, "Med. yellowish green", [199, 210, 60]),
    (116, "Med. bluish green", [85, 165, 175]),
    (118, "Light bluish green", [183, 215, 213]),
    (119, "Br. yellowish green", [164, 189, 71]),
    (120, "Lig. yellowish green", [217, 228, 167]),
    (121, "Med. yellowish orange", [231, 172, 88]),
    (123, "Br. reddish orange", [211, 111, 76]),
    (124, "Bright reddish violet", [146, 57, 120]),
    (125, "Light orange", [234, 184, 146]),
    (126, "Tr. Bright bluish violet", [165, 165, 203]),
    (127, "Gold", [220, 188, 129]),
    (128, "Dark nougat", [174, 122, 89]),
    (131, "Silver", [156, 163, 168]),
    (133, "Neon orange", [213, 115, 61]),
    (134, "Neon green", [216, 221, 86]),
    (135, "Sand blue", [116, 134, 157]),
    (136, "Sand violet", [135, 124, 144]),
    (137, "Medium orange", [224, 152, 100]),
    (138, "Sand yellow", [149, 138, 115]),
    (140, "Earth blue", [32, 58, 86]),
    (141, "Earth green", [39, 70, 45]),
    (143, "Tr. Flu. Blue", [207, 226, 247]),
    (145, "Sand blue metallic", [121, 136, 161]),
    (146, "Sand violet metallic", [149, 142, 163]),
    (147, "Sand yellow metallic", [147, 135, 103]),
    (148, "Dark grey metallic", [87, 88, 87]),
    (149, "Black metallic", [22, 29, 50]),
    (150, "Light grey metallic", [171, 173, 172]),
    (151, "Sand green", [120, 144, 130]),
    (153, "Sand red", [149, 121, 119]),
    (154, "Dark red", [123, 46, 47]),
    (157, "Tr. Flu. Yellow", [255, 246, 123]),
    (158, "Tr. Flu. Red", [225, 164, 194]),
    (168, "Gun metallic", [117, 108, 98]),
    (176, "Red flip/flop", [151, 105, 91]),
    (178, "Yellow flip/flop", [180, 132, 85]),
    (179, "Silver flip/flop", [137, 135, 136]),
    (180, "Curry", [215, 169, 75]),
    (190, "Fire Yellow", [249, 214, 46]),
    (191, "Flame yellowish orange", [232, 171, 45]),
    (192, "Reddish brown", [105, 64, 40]),
    (193, "Flame reddish orange", [207, 96, 36]),
    (194, "Medium stone grey", [163, 162, 165]),
    (195, "Royal blue", [70, 103, 164]),
    (196, "Dark Royal blue", [35, 71, 139]),
    (198, "Bright reddish lilac", [142, 66, 133]),
    (199, "Dark stone grey", [99, 95, 98]),
    (200, "Lemon metalic", [130, 138, 93]),
    (208, "Light stone grey", [229, 228, 223]),
    (209, "Dark Curry", [176, 142, 68]),
    (210, "Faded green", [112, 149, 120]),
    (211, "Turquoise", [121, 181, 181]),
    (212, "Light Royal blue", [159, 195, 233]),
    (213, "Medium Royal blue", [108, 129, 183]),
    (216, "Rust", [144, 76, 42]),
    (217, "Brown", [124, 92, 70]),
    (218, "Reddish lilac", [150, 112, 159]),
    (219, "Lilac", [107, 98, 155]),
    (220, "Light lilac", [167, 169, 206]),
    (221, "Bright purple", [205, 98, 152]),
    (222, "Light purple", [228, 173, 200]),
    (223, "Light pink", [220, 144, 149]),
    (224, "Light brick yellow", [240, 213, 160]),
    (225, "Warm yellowish orange", [235, 184, 127]),
    (226, "Cool yellow", [253, 234, 141]),
    (232, "Dove blue", [125, 187, 221]),
    (268, "Medium lilac", [52, 43, 117]),
    (301, "Slime green", [80, 109, 84]),
    (302, "Smoky grey", [91, 93, 105]),
    (303, "Dark blue", [0, 16, 176]),
    (304, "Parsley green", [44, 101, 29]),
    (305, "Steel blue", [82, 124, 174]),
    (306, "Storm blue", [51, 88, 130]),
    (307, "Lapis", [16, 42, 220]),
    (308, "Dark indigo", [61, 21, 133]),
    (309, "Sea green", [52, 142, 64]),
    (310, "Shamrock", [91, 154, 76]),
    (311, "Fossil", [159, 161, 172]),
    (312, "Mulberry", [89, 34, 89]),
    (313, "Forest green", [31, 128, 29]),
    (314, "Cadet blue", [159, 173, 192]),
    (315, "Electric blue", [9, 137, 207]),
    (316, "Eggplant", [123, 0, 123]),
    (317, "Moss", [124, 156, 107]),
    (318, "Artichoke", [138, 171, 133]),
    (319, "Sage green", [185, 196, 177]),
    (320, "Ghost grey", [202, 203, 209]),
    (321, "Lilac", [167, 94, 155]),
    (322, "Plum", [123, 47, 123]),
    (323, "Olivine", [148, 190, 129]),
    (324, "Laurel green", [168, 189, 153]),
    (325, "Quill grey", [223, 223, 222]),
    (327, "Crimson", [151, 0, 0]),
    (328, "Mint", [177, 229, 166]),
    (329, "Baby blue", [152, 194, 219]),
    (330, "Carnation pink", [255, 152, 220]),
    (331, "Persimmon", [255, 89, 89]),
    (332, "Maroon", [117, 0, 0]),
    (333, "Gold", [239, 184, 56]),
    (334, "Daisy orange", [248, 217, 109]),
    (335, "Pearl", [231, 231, 236]),
    (336, "Fog", [199, 212, 228]),
    (337, "Salmon", [255, 148, 148]),
    (338, "Terra Cotta", [190, 104, 98]),
    (339, "Cocoa", [86, 36, 36]),
    (340, "Wheat", [241, 231, 199]),
    (341, "Buttermilk", [254, 243, 187]),
    (342, "Mauve", [224, 178, 208]),
    (343, "Sunrise", [212, 144, 189]),
    (344, "Tawny", [150, 85, 85]),
    (345, "Rust", [143, 76, 42]),
    (346, "Cashmere", [211, 190, 150]),
    (347, "Khaki", [226, 220, 188]),
    (348, "Lily white", [237, 234, 234]),
    (349, "Seashell", [233, 218, 218]),
    (350, "Burgundy", [136, 62, 62]),
    (351, "Cork", [188, 155, 93]),
    (352, "Burlap", [199, 172, 120]),
    (353, "Beige", [202, 191, 163]),
    (354, "Oyster", [187, 179, 178]),
    (355, "Pine Cone", [108, 88, 75]),
    (356, "Fawn brown", [160, 132, 79]),
    (357, "Hurricane grey", [149, 137, 136]),
    (358, "Cloudy grey", [171, 168, 158]),
    (359, "Linen", [175, 148, 131]),
    (360, "Copper", [150, 103, 102]),
    (361, "Medium brown", [86, 66, 54]),
    (362, "Bronze", [126, 104, 63]),
    (363, "Flint", [105, 102, 92]),
    (364, "Dark taupe", [90, 76, 66]),
    (365, "Burnt Sienna", [106, 57, 9]),
    (1001, "Institutional white", [248, 248, 248]),
    (1002, "Mid gray", [205, 205, 205]),
    (1003, "Really black", [17, 17, 17]),
    (1004, "Really red", [255, 0, 0]),
    (1005, "Deep orange", [255, 176, 0]),
    (1006, "Alder", [180, 128, 255]),
    (1007, "Dusty Rose", [163, 75, 75]),
    (1008, "Olive", [193, 190, 66]),
    (1009, "New Yeller", [255, 255, 0]),
    (1010, "Really blue", [0, 0, 255]),
    (1011, "Navy blue", [0, 32, 96]),
    (1012, "Deep blue", [33, 84, 185]),
    (1013, "Cyan", [4, 175, 236]),
    (1014, "CGA brown", [170, 85, 0]),
    (1015, "Magenta", [170, 0, 170]),
    (1016, "Pink", [255, 102, 204]),
    (1017, "Deep orange", [255, 175, 0]),
    (1018, "Teal", [18, 238, 212]),
    (1019, "Toothpaste", [0, 255, 255]),
    (1020, "Lime green", [0, 255, 0]),
    (1021, "Camo", [58, 125, 21]),
    (1022, "Grime", [127, 142, 100]),
    (1023, "Lavender", [140, 91, 159]),
    (1024, "Pastel light blue", [175, 221, 255]),
    (1025, "Pastel orange", [255, 201, 201]),
    (1026, "Pastel violet", [177, 167, 255]),
    (1027, "Pastel blue-green", [159, 243, 233]),
    (1028, "Pastel green", [204, 255, 204]),
    (1029, "Pastel yellow", [255, 255, 204]),
    (1030, "Pastel brown", [255, 204, 153]),
    (1031, "Royal purple", [98, 37, 209]),
    (1032, "Hot pink", [255, 0, 191]),
];

/// The palette: the number of each of its 128 colours, by palette index.
const PALETTE: [u16; 128] = [
    141, 301, 107, 26, 1012, 303, 1011, 304, 28, 1018, 302, 305, 306, 307, 308, 1021, 309, 310,
    1019, 135, 102, 23, 1010, 312, 313, 37, 1022, 1020, 1027, 311, 315, 1023, 1031, 316, 151, 317,
    318, 319, 1024, 314, 1013, 1006, 321, 322, 104, 1008, 119, 323, 324, 325, 320, 11, 1026, 1016,
    1032, 1015, 327, 1005, 1009, 29, 328, 1028, 208, 45, 329, 330, 331, 1004, 21, 332, 333, 24,
    334, 226, 1029, 335, 336, 342, 343, 338, 1007, 339, 133, 106, 340, 341, 1001, 1, 9, 1025, 337,
    344, 345, 1014, 105, 346, 347, 348, 349, 1030, 125, 101, 350, 192, 351, 352, 353, 354, 1002, 5,
    18, 217, 355, 356, 153, 357, 358, 359, 360, 38, 361, 362, 199, 194, 363, 364, 365, 1003,
];

/// The engine stream `BrickColor.random` draws from.
const RANDOM_STREAM: u64 = 1;

/// One of Roblox's brick colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BrickColor {
    /// Index into [`COLORS`].
    index: u8,
}

impl Default for BrickColor {
    fn default() -> Self {
        BrickColor::MEDIUM_STONE_GREY
    }
}

impl BrickColor {
    /// "Medium stone grey" (194): `BrickColor.Gray()`, and what an unknown
    /// number or name gives.
    pub const MEDIUM_STONE_GREY: BrickColor = BrickColor::known(194);
    /// "White" (1), the default `TeamColor` of a `Team` and a `Player`.
    pub const WHITE: BrickColor = BrickColor::known(1);

    /// The colour numbered `n`, which [`COLORS`] must hold.
    const fn known(n: u16) -> BrickColor {
        let mut i = 0;
        while i < COLORS.len() {
            if COLORS[i].0 == n {
                return BrickColor { index: i as u8 };
            }
            i += 1;
        }
        panic!("not a brick colour number")
    }

    /// The colour numbered `n`, or "Medium stone grey" when no colour has
    /// that number.
    pub fn from_number(n: i64) -> BrickColor {
        u16::try_from(n)
            .ok()
            .and_then(|n| COLORS.binary_search_by_key(&n, |c| c.0).ok())
            .map_or(BrickColor::MEDIUM_STONE_GREY, |i| BrickColor {
                index: i as u8,
            })
    }

    /// The colour named `name` (exactly), or "Medium stone grey". A name two
    /// colours share gives the palette's one.
    pub fn from_name(name: &str) -> BrickColor {
        let mut found = None;
        for (i, c) in COLORS.iter().enumerate() {
            if c.1 == name {
                let b = BrickColor { index: i as u8 };
                if found.is_none() || PALETTE.contains(&c.0) {
                    found = Some(b);
                }
            }
        }
        found.unwrap_or(BrickColor::MEDIUM_STONE_GREY)
    }

    /// The colour at `index` (0 to 127) of the palette.
    pub fn from_palette(index: i64) -> Option<BrickColor> {
        let n = *PALETTE.get(usize::try_from(index).ok()?)?;
        Some(BrickColor::from_number(i64::from(n)))
    }

    /// The brick colour nearest to channels `r`, `g` and `b` (0 to 1): the
    /// smallest sum of channel differences, the lowest number on a tie.
    pub fn nearest(r: f32, g: f32, b: f32) -> BrickColor {
        let want = [
            f64::from(r) * 255.0,
            f64::from(g) * 255.0,
            f64::from(b) * 255.0,
        ];
        let mut best = (f64::INFINITY, 0);
        for (i, c) in COLORS.iter().enumerate() {
            let d: f64 = want
                .iter()
                .zip(c.2)
                .map(|(w, x)| (w - f64::from(x)).abs())
                .sum();
            // NaN channels compare false and keep the first colour.
            if d < best.0 {
                best = (d, i);
            }
        }
        BrickColor {
            index: best.1 as u8,
        }
    }

    /// The brick colour nearest to `c`.
    pub fn nearest_color(c: Color3) -> BrickColor {
        BrickColor::nearest(c.r, c.g, c.b)
    }

    /// The colour's number.
    pub fn number(self) -> u16 {
        COLORS[self.index as usize].0
    }

    /// The colour's name.
    pub fn name(self) -> &'static str {
        COLORS[self.index as usize].1
    }

    /// The colour's 0-255 channels.
    pub fn rgb(self) -> [u8; 3] {
        COLORS[self.index as usize].2
    }

    /// The colour as a `Color3` (channels over 255).
    pub fn color(self) -> Color3 {
        let [r, g, b] = self.rgb();
        Color3::from_rgb(f64::from(r), f64::from(g), f64::from(b))
    }
}

impl FromLua for BrickColor {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        ud_arg::<BrickColor>(&value, 1, "BrickColor", "BrickColor")
    }
}

impl UserData for BrickColor {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "BrickColor");
        fields.add_field_method_get("Number", |_, b| Ok(b.number()));
        fields.add_field_method_get("Name", |_, b| Ok(b.name()));
        fields.add_field_method_get("Color", |_, b| Ok(b.color()));
        fields.add_field_method_get("r", |_, b| Ok(b.color().r));
        fields.add_field_method_get("g", |_, b| Ok(b.color().g));
        fields.add_field_method_get("b", |_, b| Ok(b.color().b));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, b, o: BrickColor| Ok(*b == o));
        methods.add_meta_method(MetaMethod::ToString, |_, b, ()| Ok(b.name()));
        strict(methods, "BrickColor");
    }
}

/// `BrickColor.new`: from a number, a name, a `Color3`, or three 0-1
/// channels.
fn new(a: &Value, b: &Value, c: &Value) -> Result<BrickColor> {
    match a {
        Value::Nil => Ok(BrickColor::MEDIUM_STONE_GREY),
        Value::String(s) => Ok(BrickColor::from_name(&s.to_str()?)),
        Value::UserData(_) => Ok(BrickColor::nearest_color(ud_arg::<Color3>(
            a, 1, "new", "Color3",
        )?)),
        Value::Integer(_) | Value::Number(_) if !b.is_nil() || !c.is_nil() => {
            Ok(BrickColor::nearest(
                num_arg(a, 1, "new", None)? as f32,
                num_arg(b, 2, "new", Some(0.0))? as f32,
                num_arg(c, 3, "new", Some(0.0))? as f32,
            ))
        }
        Value::Integer(_) | Value::Number(_) => {
            let n = num_arg(a, 1, "new", None)?;
            Ok(if n.is_finite() {
                BrickColor::from_number(n.trunc() as i64)
            } else {
                BrickColor::MEDIUM_STONE_GREY
            })
        }
        other => Err(bad_argument(
            1,
            "new",
            format!(
                "number, string or Color3 expected, got {}",
                type_name(other)
            ),
        )),
    }
}

/// Installs `BrickColor`.
pub(crate) fn install(lua: &Lua, globals: &Table) -> Result<()> {
    let mut members: Vec<(&str, Value)> = vec![
        (
            "new",
            Value::Function(
                lua.create_function(|_, (a, b, c): (Value, Value, Value)| new(&a, &b, &c))?,
            ),
        ),
        (
            "palette",
            Value::Function(lua.create_function(|_, i: Value| {
                let i = num_arg(&i, 1, "palette", None)?;
                (i.fract() == 0.0)
                    .then(|| BrickColor::from_palette(i as i64))
                    .flatten()
                    .ok_or_else(|| bad_argument(1, "palette", "the palette index must be 0 to 127"))
            })?),
        ),
        (
            "random",
            Value::Function(lua.create_function(|lua, ()| {
                let i = random::engine_integer(lua, RANDOM_STREAM, 0, PALETTE.len() as i64 - 1);
                Ok(BrickColor::from_palette(i).unwrap_or_default())
            })?),
        ),
    ];
    for (name, n) in [
        ("White", 1),
        ("Gray", 194),
        ("DarkGray", 199),
        ("Black", 26),
        ("Red", 21),
        ("Yellow", 24),
        ("Green", 28),
        ("Blue", 23),
    ] {
        let b = BrickColor::from_number(n);
        members.push((
            name,
            Value::Function(lua.create_function(move |_, ()| Ok(b))?),
        ));
    }
    set_lib(lua, globals, "BrickColor", members)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_palette_is_known() {
        assert!(COLORS.windows(2).all(|w| w[0].0 < w[1].0));
        for n in PALETTE {
            assert_eq!(BrickColor::from_number(i64::from(n)).number(), n);
        }
        let mut sorted = PALETTE;
        sorted.sort_unstable();
        assert!(
            sorted.windows(2).all(|w| w[0] != w[1]),
            "a palette colour twice"
        );
    }

    #[test]
    fn lookups() {
        assert_eq!(BrickColor::from_number(11).name(), "Pastel Blue");
        assert_eq!(BrickColor::from_number(4), BrickColor::MEDIUM_STONE_GREY);
        assert_eq!(BrickColor::from_number(-1).number(), 194);
        assert_eq!(BrickColor::from_name("Bright red").number(), 21);
        assert_eq!(BrickColor::from_name("bright red").number(), 194);
        assert_eq!(BrickColor::from_name("Gold").number(), 333);
        assert_eq!(BrickColor::from_name("Deep orange").number(), 1005);
        assert_eq!(
            BrickColor::from_palette(70).map(BrickColor::name),
            Some("Gold")
        );
        assert_eq!(BrickColor::from_palette(128), None);
        assert_eq!(
            BrickColor::nearest(0.502, 0.734, 0.859).name(),
            "Pastel Blue"
        );
        for (i, c) in COLORS.iter().enumerate() {
            let b = BrickColor { index: i as u8 };
            assert_eq!(BrickColor::nearest_color(b.color()), b, "{}", c.1);
        }
    }
}
