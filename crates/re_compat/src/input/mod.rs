//! The input services: `UserInputService`, `InputObject` and
//! `ContextActionService`.
//!
//! Input reaches the layer as the host's raw device frame
//! ([`re_types::RawInputFrame`], through `HostServices::raw_input`), with keys
//! named as `real.toml` binds them (`KeyW`, `Space`, `South`); [`keymap`]
//! turns those names into `KeyCode` items. Once per frame, before the render-step
//! bindings, the client VM runs [`frame`]: for each raw event, in order, it
//! updates the `InputObject` of that key or device (one object per key or
//! device, reused, as scripts expect), offers the input to the bound actions
//! of `ContextActionService` from the highest priority down, then fires
//! `InputBegan`, `InputChanged` or `InputEnded`. The server VM sees no input.
//!
//! `gameProcessedEvent` is true only when the engine itself used the input; in
//! M2 nothing does (no text box takes focus), so it is false. An action that
//! sinks an input stops lower actions from seeing it, not the
//! `UserInputService` events.

mod cas;
mod keymap;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use mlua::{Lua, MultiValue, Result, Value as LuaValue};
use re_reflect::Value;
use re_scene::{Entity, World};
use re_script::ContextKind;
use re_types::input::{InputEvent, InputKind, InputPhase, MouseLock, RawInputFrame};

use crate::class::own_component_name;
use crate::datatypes::Vector2;
use crate::enums::EnumItem;
use crate::instance::methods_api::{args, ret};
use crate::instance::{push, tree};
use crate::runtime::{Bindings, compat_error, state, with_world};
use crate::scene::spawn_instance;
use crate::services::core::find_service;
use crate::signal;

/// The class of the input service.
pub const INPUT_SERVICE: &str = "UserInputService";

/// What the input services keep per VM.
#[derive(Default)]
pub(crate) struct InputState {
    /// The latest raw frame.
    raw: RawInputFrame,
    /// The `InputObject` of each (device kind, key) pair.
    objects: HashMap<(String, String), Entity>,
    /// The kind of the latest input (`UserInputType` item name).
    last_input_type: Option<String>,
    /// The cursor settings last sent to the host.
    mouse_sent: Option<(MouseLock, bool)>,
    /// Whether a gamepad was connected last frame.
    gamepad: bool,
    /// The bound actions.
    pub(crate) actions: cas::Actions,
}

type Shared = Rc<RefCell<InputState>>;

pub(crate) fn input_state(lua: &Lua) -> Shared {
    if let Some(s) = lua.app_data_ref::<Shared>() {
        return s.clone();
    }
    let s: Shared = Rc::default();
    lua.set_app_data(s.clone());
    s
}

/// Writes an own property of `class` on `e`, bypassing the read-only check
/// (for values the engine owns).
pub(crate) fn set_own(w: &mut World, class: &str, e: Entity, prop: &str, v: Value) {
    let Ok(key) = w.registry.key(&own_component_name(class)) else {
        return;
    };
    let Ok(index) = w.registry.schema(key).field_index(prop) else {
        return;
    };
    let _ = w.registry.set_field(&mut w.ecs, e, key, index, v);
}

/// Reads an own property of `class` on `e`.
pub(crate) fn get_own(w: &World, class: &str, e: Entity, prop: &str) -> Option<Value> {
    let key = w.registry.try_key(&own_component_name(class))?;
    let index = w.registry.schema(key).field_index(prop).ok()?;
    w.registry.get_field(&w.ecs, e, key, index)
}

/// `name` when it is a `KeyCode` item, else `None`.
fn key_code(name: &str) -> &str {
    if EnumItem::lookup("KeyCode", name).is_some() {
        name
    } else {
        "None"
    }
}

/// The `UserInputType` item name of a raw device kind.
fn input_type(name: &str) -> &str {
    if EnumItem::lookup("UserInputType", name).is_some() {
        name
    } else {
        "None"
    }
}

fn state_name(phase: InputPhase) -> &'static str {
    match phase {
        InputPhase::Began => "Begin",
        InputPhase::Changed => "Change",
        InputPhase::Ended => "End",
    }
}

/// A raw event in the layer's terms: a `UserInputType` and a `KeyCode` item.
pub(crate) struct Event {
    phase: InputPhase,
    /// The `UserInputType` item name.
    input_type: String,
    /// The `KeyCode` item name (`None` for devices without keys).
    key: String,
    position: glam::Vec3,
    delta: glam::Vec3,
}

/// The `UserInputType` item of a mouse button name (`Left`).
fn mouse_type(button: &str) -> &'static str {
    match button {
        "Left" => "MouseButton1",
        "Right" => "MouseButton2",
        "Middle" => "MouseButton3",
        _ => "None",
    }
}

/// Translates a raw event into the layer's terms.
fn translate(ev: &InputEvent) -> Event {
    let (ty, key) = match ev.kind {
        InputKind::Keyboard => ("Keyboard", keymap::key_code(&ev.button)),
        InputKind::MouseButton => (mouse_type(&ev.button), None),
        InputKind::MouseMovement => ("MouseMovement", None),
        InputKind::MouseWheel => ("MouseWheel", None),
        InputKind::Gamepad => ("Gamepad1", keymap::pad_code(&ev.button)),
        InputKind::Touch => ("Touch", None),
    };
    Event {
        phase: ev.phase,
        input_type: ty.to_string(),
        key: key.unwrap_or("None").to_string(),
        position: ev.position,
        delta: ev.delta,
    }
}

/// The `InputObject` for `(input_type, key)`, made on first use.
fn object(lua: &Lua, st: &Shared, ty: &str, key: &str) -> Result<Entity> {
    let k = (ty.to_string(), key.to_string());
    let existing = st.borrow().objects.get(&k).copied();
    if let Some(e) = existing
        && with_world(lua, |w| tree::alive(w, e))?
    {
        return Ok(e);
    }
    let classes = state(lua)?.setup.classes.clone();
    let e = with_world(lua, |w| {
        spawn_instance(w, &classes, "InputObject", None, "InputObject", None)
    })?
    .map_err(compat_error)?;
    with_world(lua, |w| {
        set_own(
            w,
            "InputObject",
            e,
            "KeyCode",
            Value::Enum(key_code(key).into()),
        );
        set_own(
            w,
            "InputObject",
            e,
            "UserInputType",
            Value::Enum(input_type(ty).into()),
        );
    })?;
    st.borrow_mut().objects.insert(k, e);
    Ok(e)
}

/// Updates the `InputObject` of a raw event and returns it.
fn object_for_event(lua: &Lua, st: &Shared, ev: &Event) -> Result<Entity> {
    let e = object(lua, st, &ev.input_type, &ev.key)?;
    with_world(lua, |w| {
        set_own(
            w,
            "InputObject",
            e,
            "UserInputState",
            Value::Enum(state_name(ev.phase).into()),
        );
        set_own(w, "InputObject", e, "Position", Value::Vec3(ev.position));
        set_own(w, "InputObject", e, "Delta", Value::Vec3(ev.delta));
    })?;
    Ok(e)
}

/// The input step of one frame, run in the client VM before the render-step
/// bindings with the host bound: reads the host's raw input, updates the
/// device flags, dispatches every event to the bound actions and fires the
/// `UserInputService` events, and passes the cursor settings to the host. In
/// the server VM it does nothing.
pub fn frame(lua: &Lua) -> Result<()> {
    let vm = state(lua)?;
    if vm.kind != ContextKind::Client {
        return Ok(());
    }
    let raw = re_script::host::with_host(lua, |h| Ok(h.services().raw_input().cloned()))?
        .unwrap_or_default();
    let st = input_state(lua);
    let uis = with_world(lua, |w| find_service(w, vm.game, INPUT_SERVICE))?;
    let gamepad_was = st.borrow().gamepad;
    st.borrow_mut().raw = raw.clone();
    st.borrow_mut().gamepad = raw.gamepad;
    if let Some(uis) = uis {
        with_world(lua, |w| {
            for (prop, v) in [
                ("KeyboardEnabled", raw.keyboard),
                ("MouseEnabled", raw.mouse),
                ("GamepadEnabled", raw.gamepad),
            ] {
                if get_own(w, INPUT_SERVICE, uis, prop) != Some(Value::Bool(v)) {
                    set_own(w, INPUT_SERVICE, uis, prop, Value::Bool(v));
                }
            }
        })?;
        signal::events::after_write(lua)?;
        if raw.gamepad != gamepad_was {
            let pad = EnumItem::named("UserInputType", "Gamepad1").to_lua(lua)?;
            let name = if raw.gamepad {
                "GamepadConnected"
            } else {
                "GamepadDisconnected"
            };
            signal::fire(lua, uis, name, MultiValue::from_vec(vec![pad]))?;
        }
    }
    for ev in raw.events.iter().map(translate) {
        let ev = &ev;
        let obj = object_for_event(lua, &st, ev)?;
        cas::dispatch(lua, &st, ev, obj)?;
        let Some(uis) = uis else { continue };
        let ty = input_type(&ev.input_type).to_string();
        let changed = st.borrow().last_input_type.as_deref() != Some(ty.as_str());
        if changed {
            st.borrow_mut().last_input_type = Some(ty.clone());
            let item = EnumItem::named("UserInputType", &ty).to_lua(lua)?;
            signal::fire(
                lua,
                uis,
                "LastInputTypeChanged",
                MultiValue::from_vec(vec![item]),
            )?;
        }
        let name = match ev.phase {
            InputPhase::Began => "InputBegan",
            InputPhase::Changed => "InputChanged",
            InputPhase::Ended => "InputEnded",
        };
        signal::fire(
            lua,
            uis,
            name,
            MultiValue::from_vec(vec![push(lua, obj)?, LuaValue::Boolean(false)]),
        )?;
        let jump = ev.phase == InputPhase::Began
            && ((ev.input_type == "Keyboard" && ev.key == "Space")
                || (ev.input_type == "Gamepad1" && ev.key == "ButtonA"));
        if jump {
            signal::fire(lua, uis, "JumpRequest", MultiValue::new())?;
        }
    }
    if let Some(uis) = uis {
        send_mouse_mode(lua, &st, uis)?;
    }
    Ok(())
}

/// Passes `MouseBehavior` and `MouseIconEnabled` to the host when they
/// changed. A host without a window ignores them; the properties stay
/// recorded.
fn send_mouse_mode(lua: &Lua, st: &Shared, uis: Entity) -> Result<()> {
    let (behavior, icon) = with_world(lua, |w| {
        (
            get_own(w, INPUT_SERVICE, uis, "MouseBehavior"),
            get_own(w, INPUT_SERVICE, uis, "MouseIconEnabled"),
        )
    })?;
    let lock = match behavior {
        Some(Value::Enum(n)) if n == "LockCenter" => MouseLock::LockCenter,
        Some(Value::Enum(n)) if n == "LockCurrentPosition" => MouseLock::LockCurrentPosition,
        _ => MouseLock::Free,
    };
    let icon = !matches!(icon, Some(Value::Bool(false)));
    if st.borrow().mouse_sent == Some((lock, icon)) {
        return Ok(());
    }
    st.borrow_mut().mouse_sent = Some((lock, icon));
    re_script::host::with_host(lua, |h| {
        let _ = h.services().set_mouse_behavior(lock, icon);
        Ok(())
    })
}

/// An enum argument of type `ty`.
fn enum_arg(v: &LuaValue, ty: &str) -> Result<EnumItem> {
    EnumItem::coerce(v, ty)
}

fn raw(lua: &Lua) -> RawInputFrame {
    input_state(lua).borrow().raw.clone()
}

fn is_key_down(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let v: LuaValue = args(lua, a, "IsKeyDown")?;
    let key = enum_arg(&v, "KeyCode")?;
    ret(lua, held(&raw(lua), key.name()))
}

/// True while the key or gamepad button of `KeyCode` item `code` is held.
fn held(raw: &RawInputFrame, code: &str) -> bool {
    match keymap::raw_of(code) {
        Some((true, k)) => raw.key_down(k),
        Some((false, b)) => raw.pad_button_down(b),
        None => false,
    }
}

/// The mouse button name of a `UserInputType` item.
fn mouse_button_name(item: EnumItem) -> Option<&'static str> {
    match item.name() {
        "MouseButton1" => Some("Left"),
        "MouseButton2" => Some("Right"),
        "MouseButton3" => Some("Middle"),
        _ => None,
    }
}

fn is_mouse_button_pressed(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let v: LuaValue = args(lua, a, "IsMouseButtonPressed")?;
    let item = enum_arg(&v, "UserInputType")?;
    let down = mouse_button_name(item).is_some_and(|n| raw(lua).mouse_button_down(n));
    ret(lua, down)
}

fn is_gamepad_button_down(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let (pad, key): (LuaValue, LuaValue) = args(lua, a, "IsGamepadButtonDown")?;
    let pad = enum_arg(&pad, "UserInputType")?;
    let key = enum_arg(&key, "KeyCode")?;
    let raw = raw(lua);
    ret(
        lua,
        pad.name() == "Gamepad1"
            && raw.gamepad
            && keymap::raw_of(key.name()).is_some_and(|(kb, b)| !kb && raw.pad_button_down(b)),
    )
}

fn vector2(lua: &Lua, x: f32, y: f32) -> Result<LuaValue> {
    Ok(LuaValue::UserData(lua.create_userdata(Vector2 { x, y })?))
}

fn get_mouse_location(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    let p = raw(lua).mouse_position;
    ret(lua, vector2(lua, p.x, p.y)?)
}

fn get_mouse_delta(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let d = raw(lua).mouse_delta;
    let k = match with_world(lua, |w| {
        get_own(w, INPUT_SERVICE, e, "MouseDeltaSensitivity")
    })? {
        Some(Value::Float(k)) => k as f32,
        _ => 1.0,
    };
    ret(lua, vector2(lua, d.x * k, d.y * k)?)
}

fn get_keys_pressed(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    let st = input_state(lua);
    let raw = st.borrow().raw.clone();
    let mut out = Vec::new();
    for k in &raw.keys_down {
        let code = keymap::key_code(k).unwrap_or("None");
        out.push(object(lua, &st, "Keyboard", code)?);
    }
    ret(lua, crate::instance::push_list(lua, &out)?)
}

fn get_mouse_buttons_pressed(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    let st = input_state(lua);
    let raw = st.borrow().raw.clone();
    let mut out = Vec::new();
    for b in &raw.mouse_buttons_down {
        out.push(object(lua, &st, mouse_type(b), "None")?);
    }
    ret(lua, crate::instance::push_list(lua, &out)?)
}

fn get_last_input_type(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    let last = input_state(lua).borrow().last_input_type.clone();
    // Before any input the player is assumed to use the keyboard.
    let name = last.unwrap_or_else(|| "Keyboard".to_string());
    ret(lua, EnumItem::named("UserInputType", &name).to_lua(lua)?)
}

fn get_focused_text_box(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    ret(lua, LuaValue::Nil)
}

fn get_connected_gamepads(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    let t = lua.create_table()?;
    if raw(lua).gamepad {
        t.raw_push(EnumItem::named("UserInputType", "Gamepad1").to_lua(lua)?)?;
    }
    ret(lua, t)
}

fn get_gamepad_connected(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let v: LuaValue = args(lua, a, "GetGamepadConnected")?;
    let pad = enum_arg(&v, "UserInputType")?;
    ret(lua, pad.name() == "Gamepad1" && raw(lua).gamepad)
}

fn is_modifier_key_down(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let v: LuaValue = args(lua, a, "IsModifierKeyDown")?;
    let m = enum_arg(&v, "ModifierKey")?;
    let keys: &[&str] = match m.name() {
        "Shift" => &["ShiftLeft", "ShiftRight"],
        "Ctrl" => &["ControlLeft", "ControlRight"],
        "Alt" => &["AltLeft", "AltRight"],
        "Meta" => &["SuperLeft", "SuperRight", "Meta"],
        _ => &[],
    };
    let raw = raw(lua);
    ret(lua, keys.iter().any(|k| raw.key_down(k)))
}

type Imp = fn(&Lua, Entity, MultiValue) -> Result<MultiValue>;

/// Registers the methods of `UserInputService`, `InputObject` and
/// `ContextActionService`.
pub fn register(b: &mut Bindings) {
    let uis: &[(&str, Imp)] = &[
        ("IsKeyDown", is_key_down),
        ("IsMouseButtonPressed", is_mouse_button_pressed),
        ("IsGamepadButtonDown", is_gamepad_button_down),
        ("GetMouseLocation", get_mouse_location),
        ("GetMouseDelta", get_mouse_delta),
        ("GetKeysPressed", get_keys_pressed),
        ("GetMouseButtonsPressed", get_mouse_buttons_pressed),
        ("GetLastInputType", get_last_input_type),
        ("GetFocusedTextBox", get_focused_text_box),
        ("GetConnectedGamepads", get_connected_gamepads),
        ("GetGamepadConnected", get_gamepad_connected),
    ];
    for (name, f) in uis {
        b.method(INPUT_SERVICE, name, Arc::new(*f));
    }
    b.method(
        "InputObject",
        "IsModifierKeyDown",
        Arc::new(is_modifier_key_down),
    );
    cas::register(b);
}
