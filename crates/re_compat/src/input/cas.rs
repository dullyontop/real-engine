//! `ContextActionService`: named actions bound to keys and buttons.
//!
//! Each raw input goes to the actions bound to its key (a `KeyCode` item) or
//! its device kind (a `UserInputType` item), from the highest priority down;
//! at equal priority the latest binding goes first. A handler that returns
//! `Enum.ContextActionResult.Pass` lets the next action see the input;
//! anything else (including nothing) sinks it. Handlers run as script
//! threads; one that yields counts as sinking.

use std::collections::BTreeSet;
use std::sync::Arc;

use mlua::{Function, Lua, MultiValue, Result, Table, Value as LuaValue};
use re_scene::Entity;
use re_types::input::InputPhase;

use super::{Event, Shared, input_state};
use crate::enums::EnumItem;
use crate::instance::methods_api::{args, ret};
use crate::instance::push;
use crate::runtime::{Bindings, lua_error};
use re_script::codes::BAD_ARGUMENT;

/// `Enum.ContextActionPriority.Medium`, the priority of `BindAction`.
const DEFAULT_PRIORITY: i64 = 2000;

/// What an action listens to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    /// A `KeyCode` item name.
    Key(String),
    /// A `UserInputType` item name.
    Type(String),
}

struct Action {
    name: String,
    f: Function,
    priority: i64,
    seq: u64,
    inputs: Vec<Input>,
    touch_button: bool,
    title: String,
    description: String,
    image: String,
    /// The (device kind, key) pairs this action got a Begin for and no End.
    held: BTreeSet<(String, String)>,
}

/// The bound actions of one VM.
#[derive(Default)]
pub(crate) struct Actions {
    list: Vec<Action>,
    seq: u64,
}

fn matches(inputs: &[Input], ev: &Event) -> bool {
    inputs.iter().any(|i| match i {
        Input::Key(k) => k == &ev.key,
        Input::Type(t) => t == &ev.input_type,
    })
}

/// Calls `f` as a script thread and returns its first result, or `None` when
/// it yielded or raised (the scheduler reports the error).
fn call(lua: &Lua, f: &Function, a: MultiValue) -> Result<Option<LuaValue>> {
    let wrapper: Function =
        match lua.named_registry_value::<Option<Function>>("re_compat.cas.call")? {
            Some(w) => w,
            None => {
                let w: Function = lua
                    .load("local f, out = ... ; out[1] = f(select(3, ...)) ; out.done = true")
                    .set_name("=ContextActionService")
                    .into_function()?;
                lua.set_named_registry_value("re_compat.cas.call", &w)?;
                w
            }
        };
    let out = lua.create_table()?;
    let mut full = vec![LuaValue::Function(f.clone()), LuaValue::Table(out.clone())];
    full.extend(a);
    re_script::sched::spawn(
        lua,
        wrapper,
        MultiValue::from_vec(full),
        "ContextActionService",
    )?;
    if out.raw_get::<bool>("done").unwrap_or(false) {
        Ok(Some(out.raw_get(1)?))
    } else {
        Ok(None)
    }
}

fn passes(v: &Option<LuaValue>) -> bool {
    match v {
        Some(LuaValue::UserData(ud)) => ud
            .borrow::<EnumItem>()
            .is_ok_and(|i| i.enum_name() == "ContextActionResult" && i.name() == "Pass"),
        _ => false,
    }
}

/// Offers one raw event to the bound actions.
pub(super) fn dispatch(lua: &Lua, st: &Shared, ev: &Event, obj: Entity) -> Result<()> {
    let mut order: Vec<(i64, u64)> = st
        .borrow()
        .actions
        .list
        .iter()
        .filter(|a| matches(&a.inputs, ev))
        .map(|a| (a.priority, a.seq))
        .collect();
    order.sort_by(|a, b| b.cmp(a));
    let key = (ev.input_type.clone(), ev.key.clone());
    for (_, seq) in order {
        // The handler of an earlier action may have unbound this one.
        let found = {
            let mut s = st.borrow_mut();
            s.actions.list.iter_mut().find(|a| a.seq == seq).map(|a| {
                match ev.phase {
                    InputPhase::Began => {
                        a.held.insert(key.clone());
                    }
                    InputPhase::Ended => {
                        a.held.remove(&key);
                    }
                    InputPhase::Changed => {}
                }
                (a.name.clone(), a.f.clone())
            })
        };
        let Some((name, f)) = found else { continue };
        let state = EnumItem::named("UserInputState", super::state_name(ev.phase)).to_lua(lua)?;
        let r = call(
            lua,
            &f,
            MultiValue::from_vec(vec![
                LuaValue::String(lua.create_string(&name)?),
                state,
                push(lua, obj)?,
            ]),
        )?;
        if !passes(&r) {
            break;
        }
    }
    Ok(())
}

fn parse_inputs(rest: MultiValue, what: &str) -> Result<Vec<Input>> {
    let mut out = Vec::new();
    for v in rest {
        let item = match &v {
            LuaValue::UserData(ud) => ud.borrow::<EnumItem>().ok().map(|i| *i),
            _ => None,
        };
        match item {
            Some(i) if i.enum_name() == "KeyCode" => out.push(Input::Key(i.name().into())),
            Some(i) if i.enum_name() == "UserInputType" => out.push(Input::Type(i.name().into())),
            // Touch buttons and player actions name no device here.
            Some(i) if i.enum_name() == "PlayerActions" => {}
            _ => {
                return Err(lua_error(
                    BAD_ARGUMENT,
                    format!(
                        "bad argument to {what}: inputs must be Enum.KeyCode or Enum.UserInputType items"
                    ),
                ));
            }
        }
    }
    Ok(out)
}

fn bind(
    lua: &Lua,
    name: String,
    f: Function,
    touch: bool,
    priority: i64,
    inputs: Vec<Input>,
) -> Result<()> {
    unbind_named(lua, &name)?;
    let st = input_state(lua);
    let mut s = st.borrow_mut();
    s.actions.seq += 1;
    let seq = s.actions.seq;
    s.actions.list.push(Action {
        name,
        f,
        priority,
        seq,
        inputs,
        touch_button: touch,
        title: String::new(),
        description: String::new(),
        image: String::new(),
        held: BTreeSet::new(),
    });
    Ok(())
}

fn bind_action(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let mut a = a.into_iter();
    let (name, f, touch): (String, Function, Option<bool>) =
        args(lua, MultiValue::from_iter(a.by_ref().take(3)), "BindAction")?;
    let inputs = parse_inputs(a.collect(), "BindAction")?;
    bind(
        lua,
        name,
        f,
        touch.unwrap_or(false),
        DEFAULT_PRIORITY,
        inputs,
    )?;
    ret(lua, ())
}

fn bind_action_at_priority(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let mut a = a.into_iter();
    let (name, f, touch, priority): (String, Function, Option<bool>, LuaValue) = args(
        lua,
        MultiValue::from_iter(a.by_ref().take(4)),
        "BindActionAtPriority",
    )?;
    let priority = match priority {
        LuaValue::Integer(i) => re_script::lua_int(i),
        LuaValue::Number(n) => n as i64,
        LuaValue::UserData(ud) => ud
            .borrow::<EnumItem>()
            .map(|i| i.value())
            .unwrap_or(DEFAULT_PRIORITY),
        _ => DEFAULT_PRIORITY,
    };
    let inputs = parse_inputs(a.collect(), "BindActionAtPriority")?;
    bind(lua, name, f, touch.unwrap_or(false), priority, inputs)?;
    ret(lua, ())
}

/// Removes action `name`; each input it holds gets a Cancel call first.
fn unbind_named(lua: &Lua, name: &str) -> Result<()> {
    let st = input_state(lua);
    let removed = {
        let mut s = st.borrow_mut();
        let i = s.actions.list.iter().position(|a| a.name == name);
        i.map(|i| s.actions.list.remove(i))
    };
    let Some(action) = removed else {
        return Ok(());
    };
    for (ty, key) in &action.held {
        let obj = super::object(lua, &st, ty, key)?;
        let cancel = EnumItem::named("UserInputState", "Cancel").to_lua(lua)?;
        call(
            lua,
            &action.f,
            MultiValue::from_vec(vec![
                LuaValue::String(lua.create_string(&action.name)?),
                cancel,
                push(lua, obj)?,
            ]),
        )?;
    }
    Ok(())
}

fn unbind_action(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "UnbindAction")?;
    unbind_named(lua, &name)?;
    ret(lua, ())
}

fn unbind_all(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    let names: Vec<String> = input_state(lua)
        .borrow()
        .actions
        .list
        .iter()
        .map(|a| a.name.clone())
        .collect();
    for n in names {
        unbind_named(lua, &n)?;
    }
    ret(lua, ())
}

fn info(lua: &Lua, a: &Action, stack_order: usize) -> Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("title", a.title.as_str())?;
    t.raw_set("description", a.description.as_str())?;
    t.raw_set("image", a.image.as_str())?;
    t.raw_set("createTouchButton", a.touch_button)?;
    t.raw_set("priorityLevel", a.priority)?;
    t.raw_set("stackOrder", stack_order as i64)?;
    let inputs = lua.create_table()?;
    for i in &a.inputs {
        let item = match i {
            Input::Key(k) => EnumItem::named("KeyCode", k),
            Input::Type(t) => EnumItem::named("UserInputType", t),
        };
        inputs.raw_push(item.to_lua(lua)?)?;
    }
    t.raw_set("inputTypes", inputs)?;
    Ok(t)
}

fn get_bound_action_info(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "GetBoundActionInfo")?;
    let st = input_state(lua);
    let s = st.borrow();
    match s
        .actions
        .list
        .iter()
        .enumerate()
        .find(|(_, a)| a.name == name)
    {
        Some((i, a)) => ret(lua, info(lua, a, i + 1)?),
        None => ret(lua, lua.create_table()?),
    }
}

fn get_all_bound_action_info(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    let st = input_state(lua);
    let s = st.borrow();
    let t = lua.create_table()?;
    for (i, a) in s.actions.list.iter().enumerate() {
        t.raw_set(a.name.as_str(), info(lua, a, i + 1)?)?;
    }
    ret(lua, t)
}

fn setter(
    field: fn(&mut Action) -> &mut String,
    what: &'static str,
) -> impl Fn(&Lua, Entity, MultiValue) -> Result<MultiValue> {
    move |lua, _, a| {
        let (name, v): (String, LuaValue) = args(lua, a, what)?;
        let text = match &v {
            LuaValue::String(s) => s.to_string_lossy(),
            other => format!("{other:?}"),
        };
        let st = input_state(lua);
        if let Some(a) = st
            .borrow_mut()
            .actions
            .list
            .iter_mut()
            .find(|a| a.name == name)
        {
            *field(a) = text;
        }
        ret(lua, ())
    }
}

fn get_button(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    ret(lua, LuaValue::Nil)
}

/// Registers the `ContextActionService` methods.
pub(super) fn register(b: &mut Bindings) {
    const C: &str = "ContextActionService";
    b.method(C, "BindAction", Arc::new(bind_action));
    b.method(C, "BindActionAtPriority", Arc::new(bind_action_at_priority));
    b.method(C, "UnbindAction", Arc::new(unbind_action));
    b.method(C, "UnbindAllActions", Arc::new(unbind_all));
    b.method(C, "GetBoundActionInfo", Arc::new(get_bound_action_info));
    b.method(
        C,
        "GetAllBoundActionInfo",
        Arc::new(get_all_bound_action_info),
    );
    b.method(
        C,
        "SetTitle",
        Arc::new(setter(|a| &mut a.title, "SetTitle")),
    );
    b.method(
        C,
        "SetDescription",
        Arc::new(setter(|a| &mut a.description, "SetDescription")),
    );
    b.method(
        C,
        "SetImage",
        Arc::new(setter(|a| &mut a.image, "SetImage")),
    );
    // There are no touch buttons, so a position has nowhere to go.
    b.method(C, "SetPosition", Arc::new(|lua: &Lua, _, _| ret(lua, ())));
    b.method(C, "GetButton", Arc::new(get_button));
}
