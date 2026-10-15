//! `TweenInfo`, `PhysicalProperties`, `Font`, `RaycastParams` and
//! `OverlapParams`.

use mlua::{
    AnyUserData, FromLua, Lua, MetaMethod, Result, Table, UserData, UserDataFields,
    UserDataMethods, Value,
};

use super::{bad_argument, fmt_f32, fmt_f64, num_arg, set_lib, strict, type_name, ud_arg};
use crate::enums::EnumItem;

/// How a tween runs: its length, easing, repeats and delay.
#[derive(Clone, Debug, PartialEq)]
pub struct TweenInfo {
    /// Seconds per run.
    pub time: f64,
    /// `Enum.EasingStyle`.
    pub easing_style: EnumItem,
    /// `Enum.EasingDirection`.
    pub easing_direction: EnumItem,
    /// Extra runs after the first; negative repeats forever.
    pub repeat_count: i32,
    /// True if each run plays back in reverse after playing forward.
    pub reverses: bool,
    /// Seconds before each run starts.
    pub delay_time: f64,
}

impl UserData for TweenInfo {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "TweenInfo");
        fields.add_field_method_get("Time", |_, t| Ok(t.time));
        fields.add_field_method_get("EasingStyle", |lua, t| t.easing_style.to_lua(lua));
        fields.add_field_method_get("EasingDirection", |lua, t| t.easing_direction.to_lua(lua));
        fields.add_field_method_get("RepeatCount", |_, t| Ok(t.repeat_count));
        fields.add_field_method_get("Reverses", |_, t| Ok(t.reverses));
        fields.add_field_method_get("DelayTime", |_, t| Ok(t.delay_time));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, t, o: Value| {
            Ok(matches!(&o, Value::UserData(u) if u.borrow::<TweenInfo>().is_ok_and(|o| *t == *o)))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, t, ()| {
            Ok(format!(
                "Time:{} DelayTime:{} RepeatCount:{} Reverses:{} EasingDirection:{} EasingStyle:{}",
                fmt_f64(t.time),
                fmt_f64(t.delay_time),
                t.repeat_count,
                if t.reverses { "True" } else { "False" },
                t.easing_direction.name(),
                t.easing_style.name()
            ))
        });
        strict(methods, "TweenInfo");
    }
}

/// The physical material of a part, overriding its `Material`'s defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicalProperties {
    /// Mass per cubic stud.
    pub density: f32,
    /// Friction coefficient.
    pub friction: f32,
    /// Bounciness, 0 to 1.
    pub elasticity: f32,
    /// How much this part's friction counts against the other part's.
    pub friction_weight: f32,
    /// How much this part's elasticity counts against the other part's.
    pub elasticity_weight: f32,
    /// How much sound the material absorbs, 0 to 1.
    pub acoustic_absorption: f32,
}

impl PhysicalProperties {
    /// The defaults of a material, by item name (`"Plastic"`). Materials not
    /// in the table get Plastic's values.
    pub fn of_material(name: &str) -> Self {
        let (d, f, e) = MATERIALS
            .iter()
            .find(|(n, ..)| *n == name)
            .map(|&(_, d, f, e)| (d, f, e))
            .unwrap_or((0.7, 0.3, 0.5));
        Self {
            density: d,
            friction: f,
            elasticity: e,
            friction_weight: 1.0,
            elasticity_weight: 1.0,
            acoustic_absorption: 1.0,
        }
    }
}

/// Density, friction and elasticity of the common materials.
const MATERIALS: &[(&str, f32, f32, f32)] = &[
    ("Plastic", 0.7, 0.3, 0.5),
    ("SmoothPlastic", 0.7, 0.2, 0.5),
    ("Neon", 0.7, 0.3, 0.2),
    ("Wood", 0.35, 0.48, 0.2),
    ("WoodPlanks", 0.35, 0.48, 0.2),
    ("Marble", 2.563, 0.2, 0.17),
    ("Slate", 2.691, 0.4, 0.2),
    ("Concrete", 2.403, 0.7, 0.2),
    ("Granite", 2.691, 0.4, 0.2),
    ("Brick", 1.922, 0.8, 0.15),
    ("Pebble", 2.403, 0.4, 0.17),
    ("Cobblestone", 2.691, 0.5, 0.17),
    ("CorrodedMetal", 7.85, 0.7, 0.2),
    ("DiamondPlate", 7.85, 0.35, 0.25),
    ("Foil", 2.7, 0.4, 0.25),
    ("Metal", 7.85, 0.4, 0.25),
    ("Grass", 0.9, 0.4, 0.1),
    ("Sand", 1.602, 0.5, 0.05),
    ("Fabric", 0.7, 0.35, 0.05),
    ("Ice", 0.919, 0.02, 0.15),
    ("Glass", 2.4, 0.25, 0.2),
    ("ForceField", 2.4, 0.25, 0.2),
    ("Air", 0.01, 0.01, 0.01),
    ("Water", 1.0, 0.0, 0.01),
    ("Basalt", 2.691, 0.7, 0.15),
    ("CrackedLava", 2.691, 0.65, 0.15),
];

impl FromLua for PhysicalProperties {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        ud_arg::<PhysicalProperties>(&value, 1, "PhysicalProperties", "PhysicalProperties")
    }
}

impl UserData for PhysicalProperties {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "PhysicalProperties");
        fields.add_field_method_get("Density", |_, p| Ok(p.density));
        fields.add_field_method_get("Friction", |_, p| Ok(p.friction));
        fields.add_field_method_get("Elasticity", |_, p| Ok(p.elasticity));
        fields.add_field_method_get("FrictionWeight", |_, p| Ok(p.friction_weight));
        fields.add_field_method_get("ElasticityWeight", |_, p| Ok(p.elasticity_weight));
        fields.add_field_method_get("AcousticAbsorption", |_, p| Ok(p.acoustic_absorption));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, p, o: PhysicalProperties| Ok(*p == o));
        methods.add_meta_method(MetaMethod::ToString, |_, p, ()| {
            Ok(format!(
                "{}, {}, {}, {}, {}",
                fmt_f32(p.density),
                fmt_f32(p.friction),
                fmt_f32(p.elasticity),
                fmt_f32(p.friction_weight),
                fmt_f32(p.elasticity_weight)
            ))
        });
        strict(methods, "PhysicalProperties");
    }
}

/// A font face: a family asset with a weight and a style.
#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    /// The family asset id (`rbxasset://fonts/families/<Name>.json`).
    pub family: String,
    /// `Enum.FontWeight`.
    pub weight: EnumItem,
    /// `Enum.FontStyle`.
    pub style: EnumItem,
}

impl UserData for Font {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "Font");
        fields.add_field_method_get("Family", |_, f| Ok(f.family.clone()));
        fields.add_field_method_get("Weight", |lua, f| f.weight.to_lua(lua));
        fields.add_field_method_get("Style", |lua, f| f.style.to_lua(lua));
        fields.add_field_method_get("Bold", |_, f| Ok(f.weight.value() >= 600));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, f, o: Value| {
            Ok(matches!(&o, Value::UserData(u) if u.borrow::<Font>().is_ok_and(|o| *f == *o)))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, f, ()| {
            Ok(format!(
                "Font {{ Family = {}, Weight = {}, Style = {} }}",
                f.family,
                f.weight.name(),
                f.style.name()
            ))
        });
        strict(methods, "Font");
    }
}

/// Reads an instance list for a query filter: one instance or an array of
/// them. Instances are kept as the Luau values the script gave.
fn instance_list(v: &Value, n: usize, f: &str) -> Result<Vec<AnyUserData>> {
    match v {
        Value::Nil => Ok(Vec::new()),
        Value::UserData(ud) => Ok(vec![ud.clone()]),
        Value::Table(t) => {
            let mut out = Vec::new();
            for (i, x) in t.sequence_values::<Value>().enumerate() {
                match x? {
                    Value::UserData(ud) => out.push(ud),
                    other => {
                        return Err(bad_argument(
                            n,
                            f,
                            format!("item {} is a {}, not an Instance", i + 1, type_name(&other)),
                        ));
                    }
                }
            }
            Ok(out)
        }
        other => Err(bad_argument(
            n,
            f,
            format!(
                "Instance or array of Instances expected, got {}",
                type_name(other)
            ),
        )),
    }
}

fn list_table(lua: &Lua, list: &[AnyUserData]) -> Result<Table> {
    let t = lua.create_table_with_capacity(list.len(), 0)?;
    for ud in list {
        t.raw_push(ud.clone())?;
    }
    Ok(t)
}

/// The filter of a spatial query (`Workspace:Raycast`, `Blockcast`, ...).
/// The workspace queries read it.
#[derive(Clone, Debug)]
pub struct RaycastParams {
    /// The instances (with their descendants) to include or exclude.
    pub filter: Vec<AnyUserData>,
    /// `Enum.RaycastFilterType`: `Exclude` (the default) or `Include`.
    pub filter_type: EnumItem,
    /// True to pass through terrain water.
    pub ignore_water: bool,
    /// Only parts that collide with this collision group are hit.
    pub collision_group: String,
    /// True to skip parts whose `CanCollide` is false.
    pub respect_can_collide: bool,
    /// True to test every part, ignoring the broad phase.
    pub brute_force_all_slow: bool,
}

/// The filter of a region query (`GetPartBoundsInBox`, `GetPartBoundsInRadius`, ...).
#[derive(Clone, Debug)]
pub struct OverlapParams {
    /// The instances (with their descendants) to include or exclude.
    pub filter: Vec<AnyUserData>,
    /// `Enum.RaycastFilterType`.
    pub filter_type: EnumItem,
    /// The most parts to return; 0 for no limit.
    pub max_parts: i32,
    /// Only parts that collide with this collision group are found.
    pub collision_group: String,
    /// True to skip parts whose `CanCollide` is false.
    pub respect_can_collide: bool,
    /// True to test every part, ignoring the broad phase.
    pub brute_force_all_slow: bool,
}

macro_rules! filter_fields {
    ($fields:ident, $t:ty) => {
        $fields.add_field_method_get("FilterDescendantsInstances", |lua, p| {
            list_table(lua, &p.filter)
        });
        $fields.add_field_method_set("FilterDescendantsInstances", |_, p, v: Value| {
            p.filter = instance_list(&v, 3, "FilterDescendantsInstances")?;
            Ok(())
        });
        $fields.add_field_method_get("FilterType", |lua, p| p.filter_type.to_lua(lua));
        $fields.add_field_method_set("FilterType", |_, p, v: Value| {
            p.filter_type = EnumItem::coerce(&v, "RaycastFilterType")?;
            Ok(())
        });
        $fields.add_field_method_get("CollisionGroup", |_, p| Ok(p.collision_group.clone()));
        $fields.add_field_method_set("CollisionGroup", |_, p, v: String| {
            p.collision_group = v;
            Ok(())
        });
        $fields.add_field_method_get("RespectCanCollide", |_, p| Ok(p.respect_can_collide));
        $fields.add_field_method_set("RespectCanCollide", |_, p, v: bool| {
            p.respect_can_collide = v;
            Ok(())
        });
        $fields.add_field_method_get("BruteForceAllSlow", |_, p| Ok(p.brute_force_all_slow));
        $fields.add_field_method_set("BruteForceAllSlow", |_, p, v: bool| {
            p.brute_force_all_slow = v;
            Ok(())
        });
    };
}

impl UserData for RaycastParams {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "RaycastParams");
        filter_fields!(fields, RaycastParams);
        fields.add_field_method_get("IgnoreWater", |_, p| Ok(p.ignore_water));
        fields.add_field_method_set("IgnoreWater", |_, p, v: bool| {
            p.ignore_water = v;
            Ok(())
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("AddToFilter", |_, p, v: Value| {
            p.filter.extend(instance_list(&v, 1, "AddToFilter")?);
            Ok(())
        });
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("RaycastParams"));
        params_strict(methods, "RaycastParams");
    }
}

impl UserData for OverlapParams {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "OverlapParams");
        filter_fields!(fields, OverlapParams);
        fields.add_field_method_get("MaxParts", |_, p| Ok(p.max_parts));
        fields.add_field_method_set("MaxParts", |_, p, v: Value| {
            let n = num_arg(&v, 3, "MaxParts", None)?;
            if n < 0.0 {
                return Err(bad_argument(3, "MaxParts", "MaxParts cannot be negative"));
            }
            p.max_parts = n as i32;
            Ok(())
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("AddToFilter", |_, p, v: Value| {
            p.filter.extend(instance_list(&v, 1, "AddToFilter")?);
            Ok(())
        });
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("OverlapParams"));
        params_strict(methods, "OverlapParams");
    }
}

/// As [`strict`], for the mutable params types: unknown members raise
/// `RE9001` on read and write.
fn params_strict<T, M: UserDataMethods<T>>(methods: &mut M, type_name: &'static str) {
    methods.add_meta_function(
        MetaMethod::Index,
        move |_, (_ud, key): (AnyUserData, Value)| -> Result<Value> {
            let key = key.to_string().unwrap_or_else(|_| "?".into());
            Err(super::unsupported(type_name, &key))
        },
    );
    methods.add_meta_function(
        MetaMethod::NewIndex,
        move |_, (_ud, key, _v): (AnyUserData, Value, Value)| -> Result<()> {
            let key = key.to_string().unwrap_or_else(|_| "?".into());
            Err(super::unsupported(type_name, &key))
        },
    );
}

/// Installs `TweenInfo`, `PhysicalProperties`, `Font`, `RaycastParams` and
/// `OverlapParams`.
pub(crate) fn install(lua: &Lua, globals: &Table) -> Result<()> {
    let tween_new = lua.create_function(
        |_,
         (time, style, dir, repeats, reverses, delay): (
            Value,
            Value,
            Value,
            Value,
            Value,
            Value,
        )| {
            let style = match style {
                Value::Nil => EnumItem::named("EasingStyle", "Quad"),
                v => EnumItem::coerce(&v, "EasingStyle")?,
            };
            let dir = match dir {
                Value::Nil => EnumItem::named("EasingDirection", "Out"),
                v => EnumItem::coerce(&v, "EasingDirection")?,
            };
            let reverses = match reverses {
                Value::Nil => false,
                Value::Boolean(b) => b,
                other => {
                    return Err(bad_argument(
                        5,
                        "new",
                        format!("boolean expected, got {}", type_name(&other)),
                    ));
                }
            };
            Ok(TweenInfo {
                time: num_arg(&time, 1, "new", Some(1.0))?,
                easing_style: style,
                easing_direction: dir,
                repeat_count: num_arg(&repeats, 4, "new", Some(0.0))? as i32,
                reverses,
                delay_time: num_arg(&delay, 6, "new", Some(0.0))?,
            })
        },
    )?;
    set_lib(
        lua,
        globals,
        "TweenInfo",
        vec![("new", Value::Function(tween_new))],
    )?;

    let pp_new = lua.create_function(|_, args: mlua::Variadic<Value>| {
        let a = |i: usize| args.get(i).cloned().unwrap_or(Value::Nil);
        if let Value::UserData(_) | Value::String(_) = a(0)
            && !matches!(a(0), Value::String(ref s) if s.to_str().is_ok_and(|s| s.trim().parse::<f64>().is_ok()))
        {
            let m = EnumItem::coerce(&a(0), "Material")?;
            return Ok(PhysicalProperties::of_material(m.name()));
        }
        Ok(PhysicalProperties {
            density: num_arg(&a(0), 1, "new", None)? as f32,
            friction: num_arg(&a(1), 2, "new", None)? as f32,
            elasticity: num_arg(&a(2), 3, "new", None)? as f32,
            friction_weight: num_arg(&a(3), 4, "new", Some(1.0))? as f32,
            elasticity_weight: num_arg(&a(4), 5, "new", Some(1.0))? as f32,
            acoustic_absorption: num_arg(&a(5), 6, "new", Some(1.0))? as f32,
        })
    })?;
    set_lib(
        lua,
        globals,
        "PhysicalProperties",
        vec![("new", Value::Function(pp_new))],
    )?;

    let font_new = lua.create_function(|_, (family, weight, style): (Value, Value, Value)| {
        let family = match family {
            Value::String(s) => s.to_str()?.to_string(),
            other => {
                return Err(bad_argument(
                    1,
                    "new",
                    format!("string expected, got {}", type_name(&other)),
                ));
            }
        };
        Ok(Font {
            family,
            weight: match weight {
                Value::Nil => EnumItem::named("FontWeight", "Regular"),
                v => EnumItem::coerce(&v, "FontWeight")?,
            },
            style: match style {
                Value::Nil => EnumItem::named("FontStyle", "Normal"),
                v => EnumItem::coerce(&v, "FontStyle")?,
            },
        })
    })?;
    let font_from_name =
        lua.create_function(|_, (name, weight, style): (String, Value, Value)| {
            Ok(Font {
                family: format!("rbxasset://fonts/families/{name}.json"),
                weight: match weight {
                    Value::Nil => EnumItem::named("FontWeight", "Regular"),
                    v => EnumItem::coerce(&v, "FontWeight")?,
                },
                style: match style {
                    Value::Nil => EnumItem::named("FontStyle", "Normal"),
                    v => EnumItem::coerce(&v, "FontStyle")?,
                },
            })
        })?;
    set_lib(
        lua,
        globals,
        "Font",
        vec![
            ("new", Value::Function(font_new)),
            ("fromName", Value::Function(font_from_name)),
        ],
    )?;

    let rp_new = lua.create_function(|_, ()| {
        Ok(RaycastParams {
            filter: Vec::new(),
            filter_type: EnumItem::named("RaycastFilterType", "Exclude"),
            ignore_water: false,
            collision_group: "Default".into(),
            respect_can_collide: false,
            brute_force_all_slow: false,
        })
    })?;
    set_lib(
        lua,
        globals,
        "RaycastParams",
        vec![("new", Value::Function(rp_new))],
    )?;

    let op_new = lua.create_function(|_, ()| {
        Ok(OverlapParams {
            filter: Vec::new(),
            filter_type: EnumItem::named("RaycastFilterType", "Exclude"),
            max_parts: 0,
            collision_group: "Default".into(),
            respect_can_collide: false,
            brute_force_all_slow: false,
        })
    })?;
    set_lib(
        lua,
        globals,
        "OverlapParams",
        vec![("new", Value::Function(op_new))],
    )
}
