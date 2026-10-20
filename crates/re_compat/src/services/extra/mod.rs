//! The services beyond the core ones: `HttpService`, `Debris`,
//! `ProximityPromptService` ([`prompts`]), `GuiService`, the `StarterGui`
//! core calls, and the services that need Roblox's online backend
//! (`TeleportService`, `TextService` filtering, `MarketplaceService`, `BadgeService`,
//! `MessagingService`), whose methods raise `RE9002` at once so a script's
//! `pcall` sees the failure.

pub mod prompts;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use mlua::{Lua, MultiValue, RegistryKey, Result, Table, Value as LuaValue};
use re_reflect::Value;
use re_scene::Entity;
use re_script::codes::BAD_ARGUMENT;

use crate::enums::EnumItem;
use crate::instance::methods_api::{args, ret};
use crate::instance::{self, arg_instance};
use crate::players::offline;
use crate::runtime::{Bindings, lua_error, state, with_world};

/// The seed of every VM's GUID generator, so a run is repeatable.
const GUID_SEED: u64 = 0x5245_4755_4944_0001;

/// Per-VM state of these services.
struct Extra {
    guid: u64,
    debris: VecDeque<Entity>,
    core: HashMap<String, RegistryKey>,
    core_gui: HashMap<String, bool>,
}

impl Default for Extra {
    fn default() -> Extra {
        Extra {
            guid: GUID_SEED,
            debris: VecDeque::new(),
            core: HashMap::new(),
            core_gui: HashMap::new(),
        }
    }
}

fn extra(lua: &Lua) -> mlua::AppDataRefMut<'_, Extra> {
    if lua.app_data_ref::<Extra>().is_none() {
        lua.set_app_data(Extra::default());
    }
    lua.app_data_mut::<Extra>().expect("just set")
}

/// Reseeds `HttpService:GenerateGUID` in this VM.
pub fn seed_guids(lua: &Lua, seed: u64) {
    extra(lua).guid = seed;
}

fn next_u64(state: &mut u64) -> u64 {
    // splitmix64
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn generate_guid(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let braces: Option<bool> = args(lua, a, "GenerateGUID")?;
    let (hi, lo) = {
        let mut x = extra(lua);
        (next_u64(&mut x.guid), next_u64(&mut x.guid))
    };
    // A version 4, variant 1 UUID.
    let hi = (hi & !0xf000) | 0x4000;
    let lo = (lo & !(0xc << 60)) | (0x8 << 60);
    let s = format!(
        "{:08X}-{:04X}-{:04X}-{:04X}-{:012X}",
        hi >> 32,
        (hi >> 16) & 0xffff,
        hi & 0xffff,
        lo >> 48,
        lo & 0xffff_ffff_ffff
    );
    ret(
        lua,
        if braces.unwrap_or(true) {
            format!("{{{s}}}")
        } else {
            s
        },
    )
}

fn json_error(what: &str, msg: impl std::fmt::Display) -> mlua::Error {
    lua_error(BAD_ARGUMENT, format!("{what}: {msg}"))
}

pub(crate) fn to_json(
    v: LuaValue,
    depth: usize,
    seen: &mut Vec<usize>,
) -> Result<serde_json::Value> {
    use serde_json::Value as J;
    const WHAT: &str = "JSONEncode";
    if depth > 64 {
        return Err(json_error(WHAT, "tables nest too deep"));
    }
    Ok(match v {
        LuaValue::Nil => J::Null,
        LuaValue::Boolean(b) => J::Bool(b),
        LuaValue::Integer(i) => J::from(i),
        LuaValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
                J::from(n as i64)
            } else {
                match serde_json::Number::from_f64(n) {
                    Some(x) => J::Number(x),
                    None => return Err(json_error(WHAT, format!("{n} cannot be written as JSON"))),
                }
            }
        }
        LuaValue::String(s) => J::String(s.to_string_lossy()),
        LuaValue::Table(t) => {
            let ptr = t.to_pointer() as usize;
            if seen.contains(&ptr) {
                return Err(json_error(WHAT, "a table contains itself"));
            }
            seen.push(ptr);
            let len = t.raw_len();
            let mut count = 0usize;
            for pair in t.clone().pairs::<LuaValue, LuaValue>() {
                pair?;
                count += 1;
            }
            let out = if count == 0 {
                // An empty table is an empty array.
                J::Array(Vec::new())
            } else if len > 0 && count == len {
                let mut arr = Vec::with_capacity(len);
                for i in 1..=len {
                    arr.push(to_json(t.raw_get(i)?, depth + 1, seen)?);
                }
                J::Array(arr)
            } else {
                let mut map = serde_json::Map::new();
                for pair in t.pairs::<LuaValue, LuaValue>() {
                    let (k, v) = pair?;
                    let key = match k {
                        LuaValue::String(s) => s.to_string_lossy(),
                        LuaValue::Integer(i) => i.to_string(),
                        LuaValue::Number(n) => n.to_string(),
                        other => {
                            return Err(json_error(
                                WHAT,
                                format!(
                                    "a key must be a string or a number, not a {}",
                                    other.type_name()
                                ),
                            ));
                        }
                    };
                    map.insert(key, to_json(v, depth + 1, seen)?);
                }
                J::Object(map)
            };
            seen.pop();
            out
        }
        // Instances, datatypes and enum items have no JSON form.
        LuaValue::UserData(_) | LuaValue::Vector(_) => J::Null,
        other => {
            return Err(json_error(
                WHAT,
                format!("a {} cannot be written as JSON", other.type_name()),
            ));
        }
    })
}

fn json_encode(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let v: LuaValue = args(lua, a, "JSONEncode")?;
    let j = to_json(v, 0, &mut Vec::new())?;
    ret(lua, j.to_string())
}

pub(crate) fn from_json(lua: &Lua, j: serde_json::Value) -> Result<LuaValue> {
    use serde_json::Value as J;
    Ok(match j {
        J::Null => LuaValue::Nil,
        J::Bool(b) => LuaValue::Boolean(b),
        J::Number(n) => LuaValue::Number(n.as_f64().unwrap_or(0.0)),
        J::String(s) => LuaValue::String(lua.create_string(&s)?),
        J::Array(a) => {
            let t = lua.create_table()?;
            for (i, v) in a.into_iter().enumerate() {
                t.raw_set(i + 1, from_json(lua, v)?)?;
            }
            LuaValue::Table(t)
        }
        J::Object(m) => {
            let t = lua.create_table()?;
            for (k, v) in m {
                t.raw_set(k, from_json(lua, v)?)?;
            }
            LuaValue::Table(t)
        }
    })
}

fn json_decode(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let s: mlua::LuaString = args(lua, a, "JSONDecode")?;
    let j: serde_json::Value = serde_json::from_slice(&s.as_bytes())
        .map_err(|e| json_error("JSONDecode", format!("Can't parse JSON: {e}")))?;
    ret(lua, from_json(lua, j)?)
}

fn url_encode(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let s: mlua::LuaString = args(lua, a, "UrlEncode")?;
    let mut out = String::new();
    for b in s.as_bytes().iter() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    ret(lua, out)
}

fn add_item(lua: &Lua, debris: Entity, a: MultiValue) -> Result<MultiValue> {
    let (item, lifetime): (LuaValue, Option<f64>) = args(lua, a, "AddItem")?;
    let item = arg_instance(&item, "AddItem")?;
    let lifetime = lifetime.unwrap_or(10.0).max(0.0);
    let max = with_world(lua, |w| {
        state_classes(lua)
            .ok()
            .and_then(|c| match c.get_property(w, debris, "MaxItems") {
                Ok(Value::Int(n)) => Some(n.max(0) as usize),
                _ => None,
            })
    })?
    .unwrap_or(1000);
    let overflow: Vec<Entity> = {
        let mut x = extra(lua);
        x.debris.push_back(item);
        let mut out = Vec::new();
        while x.debris.len() > max {
            out.extend(x.debris.pop_front());
        }
        out
    };
    for e in overflow {
        instance::destroy(lua, e)?;
    }
    let f = lua.create_function(move |lua, ()| {
        extra(lua).debris.retain(|x| *x != item);
        instance::destroy(lua, item)
    })?;
    crate::net::after(lua, lifetime, "Debris", f)?;
    ret(lua, ())
}

fn state_classes(lua: &Lua) -> Result<Arc<crate::ClassRegistry>> {
    Ok(state(lua)?.setup.classes.clone())
}

fn vector2(lua: &Lua, x: f64, y: f64) -> Result<LuaValue> {
    let v2: Table = lua.globals().get("Vector2")?;
    let new: mlua::Function = v2.get("new")?;
    new.call((x, y))
}

fn get_gui_inset(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let c = state_classes(lua)?;
    let inset = with_world(lua, |w| c.get_property(w, e, "TopbarInset").ok())?;
    let (x0, y0) = match inset {
        Some(Value::Record(_, f)) if f.len() >= 4 => {
            let n = |v: &Value| match v {
                Value::Float(x) => *x,
                Value::Int(i) => *i as f64,
                _ => 0.0,
            };
            (n(&f[0]), n(&f[3]))
        }
        _ => (0.0, 58.0),
    };
    let top_left = vector2(lua, x0, y0)?;
    let bottom_right = vector2(lua, 0.0, 0.0)?;
    ret(lua, (top_left, bottom_right))
}

/// The name of a `CoreGuiType` argument (an enum item, or its name).
fn core_gui_type(lua: &Lua, v: &LuaValue, what: &str) -> Result<String> {
    match v {
        LuaValue::UserData(ud) => {
            if let Ok(item) = ud.borrow::<EnumItem>()
                && item.enum_name() == "CoreGuiType"
            {
                return Ok(item.name().to_string());
            }
            Err(lua_error(
                BAD_ARGUMENT,
                format!("{what}: expected an Enum.CoreGuiType"),
            ))
        }
        LuaValue::String(s) if EnumItem::lookup("CoreGuiType", &s.to_string_lossy()).is_some() => {
            Ok(s.to_string_lossy())
        }
        _ => {
            let _ = lua;
            Err(lua_error(
                BAD_ARGUMENT,
                format!("{what}: expected an Enum.CoreGuiType"),
            ))
        }
    }
}

fn set_core_gui_enabled(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let (ty, on): (LuaValue, bool) = args(lua, a, "SetCoreGuiEnabled")?;
    let ty = core_gui_type(lua, &ty, "SetCoreGuiEnabled")?;
    let mut x = extra(lua);
    if ty == "All" {
        x.core_gui.clear();
    }
    x.core_gui.insert(ty, on);
    drop(x);
    ret(lua, ())
}

fn get_core_gui_enabled(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let ty: LuaValue = args(lua, a, "GetCoreGuiEnabled")?;
    let ty = core_gui_type(lua, &ty, "GetCoreGuiEnabled")?;
    ret(lua, core_gui_enabled(lua, &ty))
}

/// Whether a script left core GUI `ty` (`"Backpack"`, `"Chat"`, ...) on, by
/// `StarterGui:SetCoreGuiEnabled`, in this VM. Everything is on at first.
pub fn core_gui_enabled(lua: &Lua, ty: &str) -> bool {
    let x = extra(lua);
    x.core_gui
        .get(ty)
        .or_else(|| x.core_gui.get("All"))
        .copied()
        .unwrap_or(true)
}

/// The core settings `SetCore` accepts; others raise, as in Roblox.
const CORE_NAMES: &[&str] = &[
    "ChatActive",
    "ChatMakeSystemMessage",
    "ChatWindowPosition",
    "ChatWindowSize",
    "ChatBarDisabled",
    "SendNotification",
    "TopbarEnabled",
    "ResetButtonCallback",
    "PointsNotificationsActive",
    "BadgesNotificationsActive",
    "DevConsoleVisible",
    "AvatarContextMenuEnabled",
    "CoreGuiChatConnections",
    "PlayerBlockedEvent",
    "PlayerUnblockedEvent",
    "PromptSendFriendRequest",
    "PromptUnfriend",
    "PromptBlockPlayer",
    "PromptUnblockPlayer",
    "SetAvatarContextMenuTarget",
    "AddAvatarContextMenuOption",
    "RemoveAvatarContextMenuOption",
    "EmotesMenuOpen",
];

fn core_name(v: &str, what: &str) -> Result<()> {
    if CORE_NAMES.contains(&v) {
        Ok(())
    } else {
        Err(lua_error(
            BAD_ARGUMENT,
            format!("{what}: {v} has not been registered by the CoreScripts"),
        ))
    }
}

fn set_core(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let (name, value): (String, LuaValue) = args(lua, a, "SetCore")?;
    client_only(lua, "SetCore")?;
    core_name(&name, "SetCore")?;
    let key = lua.create_registry_value(value)?;
    let old = extra(lua).core.insert(name, key);
    if let Some(k) = old {
        lua.remove_registry_value(k)?;
    }
    ret(lua, ())
}

fn get_core(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "GetCore")?;
    client_only(lua, "GetCore")?;
    core_name(&name, "GetCore")?;
    if let Some(v) = core_value(lua, &name)? {
        return ret(lua, v);
    }
    let default = match name.as_str() {
        "ChatActive"
        | "TopbarEnabled"
        | "PointsNotificationsActive"
        | "BadgesNotificationsActive" => true,
        "ChatBarDisabled" | "DevConsoleVisible" | "AvatarContextMenuEnabled" | "EmotesMenuOpen" => {
            false
        }
        _ => {
            return Err(lua_error(
                BAD_ARGUMENT,
                format!("GetCore: {name} has no value to read until SetCore sets it"),
            ));
        }
    };
    ret(lua, default)
}

/// `SetCore` and `GetCore` belong to a client.
fn client_only(lua: &Lua, what: &str) -> Result<()> {
    if crate::net::is_server(lua)? {
        return Err(lua_error(
            BAD_ARGUMENT,
            format!("{what} can only be called from a client"),
        ));
    }
    Ok(())
}

/// The value a script gave core setting `name` with `StarterGui:SetCore` in
/// this VM, if any.
pub fn core_value(lua: &Lua, name: &str) -> Result<Option<LuaValue>> {
    let x = extra(lua);
    x.core.get(name).map(|k| lua.registry_value(k)).transpose()
}

type Imp = fn(&Lua, Entity, MultiValue) -> Result<MultiValue>;

/// Registers these services' methods.
pub fn register(b: &mut Bindings) {
    let plain: &[(&str, &str, Imp)] = &[
        ("HttpService", "GenerateGUID", generate_guid),
        ("HttpService", "JSONEncode", json_encode),
        ("HttpService", "JSONDecode", json_decode),
        ("HttpService", "UrlEncode", url_encode),
        ("Debris", "AddItem", add_item),
        ("GuiService", "GetGuiInset", get_gui_inset),
        ("StarterGui", "SetCore", set_core),
        ("StarterGui", "GetCore", get_core),
        ("StarterGui", "SetCoreGuiEnabled", set_core_gui_enabled),
        ("StarterGui", "GetCoreGuiEnabled", get_core_gui_enabled),
    ];
    for (class, name, f) in plain {
        b.method(class, name, Arc::new(*f));
    }
    for (class, name) in OFFLINE {
        b.method(class, name, offline(class, name));
    }
    prompts::register(b);
}

/// The methods that need an online service.
const OFFLINE: &[(&str, &str)] = &[
    ("HttpService", "GetAsync"),
    ("HttpService", "PostAsync"),
    ("HttpService", "RequestAsync"),
    ("TeleportService", "Teleport"),
    ("TeleportService", "TeleportAsync"),
    ("TeleportService", "TeleportToPlaceInstance"),
    ("TeleportService", "ReserveServer"),
    ("TeleportService", "GetLocalPlayerTeleportData"),
    ("TeleportOptions", "SetTeleportData"),
    ("TeleportOptions", "GetTeleportData"),
    ("TextService", "FilterStringAsync"),
    ("MarketplaceService", "PromptProductPurchase"),
    ("MarketplaceService", "PromptGamePassPurchase"),
    ("MarketplaceService", "UserOwnsGamePassAsync"),
    ("MarketplaceService", "PlayerOwnsAsset"),
    ("MarketplaceService", "GetProductInfo"),
    ("BadgeService", "AwardBadge"),
    ("BadgeService", "UserHasBadgeAsync"),
    ("BadgeService", "GetBadgeInfoAsync"),
    ("MessagingService", "PublishAsync"),
    ("MessagingService", "SubscribeAsync"),
];
