//! `ProximityPrompt` and `ProximityPromptService`.
//!
//! * Each client frame ([`client_frame`]) the client shows the enabled
//!   prompts within `MaxActivationDistance` of the local character (the
//!   distance from the character's `PrimaryPart` or `HumanoidRootPart` to the
//!   prompt's parent part or attachment), nearest first, up to
//!   `MaxPromptsVisible`: `PromptShown` and `PromptHidden` fire on the
//!   prompt and the service. When those members are missing (no parts yet),
//!   nothing is shown.
//! * `InputHoldBegin` and `InputHoldEnd` (the harness's input, or a script
//!   on the client) hold the prompt: `PromptButtonHoldBegan`, then after
//!   `HoldDuration` `Triggered`, and on release `TriggerEnded` (or
//!   `PromptButtonHoldEnded` when released early). The client fires them
//!   with the local player and forwards them to the server, which fires them
//!   there.
//! * Line of sight, `Exclusivity` and the built-in prompt UI are not
//!   modelled (the class is `partial`).

use std::collections::HashMap;
use std::sync::Arc;

use mlua::{Function, Lua, MultiValue, Result, Value as LuaValue};
use re_reflect::Value;
use re_scene::Entity;
use re_script::sched;

use crate::class::ClassRegistry;
use crate::enums::EnumItem;
use crate::instance::methods_api::ret;
use crate::instance::{push, tree};
use crate::net::{self, Msg, NetLink, PromptAction, Side};
use crate::runtime::{Bindings, lua_error, state, with_world};
use crate::services::core::find_service;
use crate::signal;

/// The client's prompt state.
#[derive(Default)]
struct Prompts {
    shown: Vec<Entity>,
    /// Held prompts: when the hold began, and whether it triggered.
    held: HashMap<Entity, (f64, bool)>,
}

fn prompts(lua: &Lua) -> mlua::AppDataRefMut<'_, Prompts> {
    if lua.app_data_ref::<Prompts>().is_none() {
        lua.set_app_data(Prompts::default());
    }
    lua.app_data_mut::<Prompts>().expect("just set")
}

fn num(v: Option<Value>, default: f64) -> f64 {
    match v {
        Some(Value::Float(x)) => x,
        Some(Value::Int(i)) => i as f64,
        _ => default,
    }
}

fn boolean(v: Option<Value>, default: bool) -> bool {
    match v {
        Some(Value::Bool(b)) => b,
        _ => default,
    }
}

/// Registry key of the Luau distance helper.
const DIST_KEY: &str = "re_compat.prompts.distance";

fn distance_fn(lua: &Lua) -> Result<Function> {
    if let Some(f) = lua.named_registry_value::<Option<Function>>(DIST_KEY)? {
        return Ok(f);
    }
    let f: Function = lua
        .load(
            r#"return function(character, prompt)
                local ok, d = pcall(function()
                    local root = character.PrimaryPart or character:FindFirstChild("HumanoidRootPart")
                    local p = prompt.Parent
                    local pos = if p:IsA("Attachment") then p.WorldPosition else p.Position
                    return (pos - root.Position).Magnitude
                end)
                if ok and type(d) == "number" then return d end
                return nil
            end"#,
        )
        .set_name("=ProximityPrompt")
        .call(())?;
    lua.set_named_registry_value(DIST_KEY, &f)?;
    Ok(f)
}

fn local_player(lua: &Lua, link: &NetLink) -> Result<Option<Entity>> {
    let id = link.local_player();
    with_world(lua, |w| net::value::instance_by_id(w, id.as_deref()))
}

fn service(lua: &Lua) -> Result<Option<Entity>> {
    let g = state(lua)?.game;
    with_world(lua, |w| find_service(w, g, "ProximityPromptService"))
}

fn input_type(lua: &Lua) -> Result<LuaValue> {
    EnumItem::named("ProximityPromptInputType", "Keyboard").to_lua(lua)
}

/// The client's per-frame prompt work: which prompts show, and holds that
/// completed.
pub(crate) fn client_frame(lua: &Lua, link: &NetLink) -> Result<()> {
    if link.side != Side::Client {
        return Ok(());
    }
    let c: Arc<ClassRegistry> = state(lua)?.setup.classes.clone();
    let player = local_player(lua, link)?;
    let svc = service(lua)?;
    let character = match player {
        Some(p) => with_world(lua, |w| crate::players::character_of(w, p))?,
        None => None,
    };
    let (enabled, max_visible) = match svc {
        Some(s) => with_world(lua, |w| {
            (
                boolean(c.get_property(w, s, "Enabled").ok(), true),
                num(c.get_property(w, s, "MaxPromptsVisible").ok(), 16.0) as usize,
            )
        })?,
        None => (true, 16),
    };
    let mut visible: Vec<(f64, Entity)> = Vec::new();
    if let (Some(ch), true) = (character, enabled) {
        let all = with_world(lua, |w| {
            w.ecs
                .query::<(Entity, &crate::class::ClassMarker)>()
                .iter(&w.ecs)
                .filter(|(_, m)| m.class == "ProximityPrompt")
                .map(|(e, _)| e)
                .collect::<Vec<_>>()
                .into_iter()
                .filter(|e| {
                    tree::alive(w, *e) && boolean(c.get_property(w, *e, "Enabled").ok(), true)
                })
                .map(|e| {
                    (
                        e,
                        num(c.get_property(w, e, "MaxActivationDistance").ok(), 10.0),
                    )
                })
                .collect::<Vec<_>>()
        })?;
        let dist = distance_fn(lua)?;
        let chv = push(lua, ch)?;
        for (p, max) in all {
            let d: Option<f64> = dist.call((chv.clone(), push(lua, p)?))?;
            if let Some(d) = d
                && d <= max
            {
                visible.push((d, p));
            }
        }
        visible.sort_by(|a, b| a.0.total_cmp(&b.0));
        visible.truncate(max_visible);
    }
    let now: Vec<Entity> = visible.into_iter().map(|(_, p)| p).collect();
    let (shown, hidden) = {
        let mut st = prompts(lua);
        let shown: Vec<Entity> = now
            .iter()
            .copied()
            .filter(|p| !st.shown.contains(p))
            .collect();
        let hidden: Vec<Entity> = st
            .shown
            .iter()
            .copied()
            .filter(|p| !now.contains(p))
            .collect();
        st.shown = now;
        (shown, hidden)
    };
    for p in hidden {
        let alive = with_world(lua, |w| tree::alive(w, p))?;
        if alive {
            signal::fire(lua, p, "PromptHidden", MultiValue::new())?;
            if let Some(s) = svc {
                signal::fire(
                    lua,
                    s,
                    "PromptHidden",
                    MultiValue::from_iter([push(lua, p)?]),
                )?;
            }
        }
    }
    for p in shown {
        signal::fire(
            lua,
            p,
            "PromptShown",
            MultiValue::from_iter([input_type(lua)?]),
        )?;
        if let Some(s) = svc {
            signal::fire(
                lua,
                s,
                "PromptShown",
                MultiValue::from_iter([push(lua, p)?, input_type(lua)?]),
            )?;
        }
    }
    // A held prompt that is gone or disabled is let go.
    let held_now: Vec<Entity> = prompts(lua).held.keys().copied().collect();
    for p in held_now {
        let keep = with_world(lua, |w| {
            tree::alive(w, p) && boolean(c.get_property(w, p, "Enabled").ok(), true)
        })?;
        if !keep {
            prompts(lua).held.remove(&p);
        }
    }
    // Holds that lasted HoldDuration trigger.
    let t = sched::now(lua)?;
    let held: Vec<(Entity, f64)> = prompts(lua)
        .held
        .iter()
        .filter(|(_, (_, done))| !done)
        .map(|(e, (start, _))| (*e, *start))
        .collect();
    for (p, start) in held {
        let dur = with_world(lua, |w| num(c.get_property(w, p, "HoldDuration").ok(), 0.0))?;
        if t - start >= dur {
            trigger(lua, link, p)?;
        }
    }
    Ok(())
}

/// Fires `action` on the client for the local player and forwards it.
fn act(lua: &Lua, link: &NetLink, prompt: Entity, action: PromptAction) -> Result<()> {
    let Some(player) = local_player(lua, link)? else {
        return Ok(());
    };
    fire_action(lua, prompt, player, action)?;
    let (pid, plid) = with_world(lua, |w| {
        (
            w.id_of(prompt).map(str::to_string),
            w.id_of(player).map(str::to_string),
        )
    })?;
    if let (Some(prompt), Some(player)) = (pid, plid) {
        link.lock().to_server.push_back(Msg::Prompt {
            prompt,
            player,
            action,
        });
    }
    Ok(())
}

fn fire_action(lua: &Lua, prompt: Entity, player: Entity, action: PromptAction) -> Result<()> {
    let (own, svc) = match action {
        PromptAction::HoldBegan => ("PromptButtonHoldBegan", "PromptButtonHoldBegan"),
        PromptAction::HoldEnded => ("PromptButtonHoldEnded", "PromptButtonHoldEnded"),
        PromptAction::Triggered => ("Triggered", "PromptTriggered"),
        PromptAction::TriggerEnded => ("TriggerEnded", "PromptTriggerEnded"),
    };
    let pl = push(lua, player)?;
    signal::fire(lua, prompt, own, MultiValue::from_iter([pl.clone()]))?;
    if let Some(s) = service(lua)? {
        signal::fire(lua, s, svc, MultiValue::from_iter([push(lua, prompt)?, pl]))?;
    }
    Ok(())
}

fn trigger(lua: &Lua, link: &NetLink, prompt: Entity) -> Result<()> {
    if let Some(h) = prompts(lua).held.get_mut(&prompt) {
        h.1 = true;
    }
    act(lua, link, prompt, PromptAction::Triggered)
}

/// A prompt action that arrived on the server.
pub(crate) fn on_server(lua: &Lua, prompt: &str, player: &str, action: PromptAction) -> Result<()> {
    let (p, pl) = with_world(lua, |w| {
        (
            net::value::instance_by_id(w, Some(prompt)),
            net::value::instance_by_id(w, Some(player)),
        )
    })?;
    if let (Some(p), Some(pl)) = (p, pl) {
        fire_action(lua, p, pl, action)?;
    }
    Ok(())
}

fn client_link(lua: &Lua, what: &str) -> Result<NetLink> {
    match with_world(lua, |w| w.ecs.get_resource::<NetLink>().cloned())? {
        Some(l) if l.side == Side::Client => Ok(l),
        _ => Err(lua_error(
            crate::codes::REMOTE_CALL,
            format!("ProximityPrompt:{what} can only be called from a client"),
        )),
    }
}

fn input_hold_begin(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let link = client_link(lua, "InputHoldBegin")?;
    let c = state(lua)?.setup.classes.clone();
    let (enabled, dur) = with_world(lua, |w| {
        (
            tree::alive(w, e) && boolean(c.get_property(w, e, "Enabled").ok(), true),
            num(c.get_property(w, e, "HoldDuration").ok(), 0.0),
        )
    })?;
    if !enabled || prompts(lua).held.contains_key(&e) {
        return ret(lua, ());
    }
    let t = sched::now(lua)?;
    prompts(lua).held.insert(e, (t, false));
    if dur > 0.0 {
        act(lua, &link, e, PromptAction::HoldBegan)?;
    } else {
        trigger(lua, &link, e)?;
    }
    ret(lua, ())
}

fn input_hold_end(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let link = client_link(lua, "InputHoldEnd")?;
    let Some((_, triggered)) = prompts(lua).held.remove(&e) else {
        return ret(lua, ());
    };
    let action = if triggered {
        PromptAction::TriggerEnded
    } else {
        PromptAction::HoldEnded
    };
    act(lua, &link, e, action)?;
    ret(lua, ())
}

/// Registers the prompt methods.
pub fn register(b: &mut Bindings) {
    b.method(
        "ProximityPrompt",
        "InputHoldBegin",
        Arc::new(input_hold_begin),
    );
    b.method("ProximityPrompt", "InputHoldEnd", Arc::new(input_hold_end));
}
