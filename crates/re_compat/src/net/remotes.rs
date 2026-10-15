//! Remotes and bindables: `RemoteEvent`, `UnreliableRemoteEvent`,
//! `RemoteFunction`, `BindableEvent` and `BindableFunction`.
//!
//! * Every argument and result is a deep copy ([`super::value`]).
//! * A remote message goes into the queue the two worlds share and is
//!   delivered by the receiver's [`pump`](super::pump), in send order: the
//!   server's messages reach the client in the same frame (after
//!   replication, so instances created with them already exist there), the
//!   client's reach the server at the start of the next frame.
//! * A remote event that arrives while nothing listens to it waits for the
//!   first connection, and an invocation waits for its handler, as in
//!   Roblox. At most [`MAX_WAITING`] messages wait; beyond that the oldest is
//!   dropped with a warning.
//! * `InvokeServer`, `InvokeClient` and `Invoke` park the caller until the
//!   handler, run in a new thread on the other side, returns; an error in the
//!   handler is raised in the caller (`RE9301`).
//! * In M2 there is one client: messages for other (simulated) players are
//!   counted as undelivered ([`NetLink::undelivered`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use mlua::{Function, Lua, MultiValue, Result, Value as LuaValue};
use re_scene::Entity;
use re_script::sched::{self, WakeMode, YieldToken};

use super::value::{NetValue, Target, decode_all, encode_all, instance_by_id};
use super::{Msg, NetLink, Side, link};
use crate::codes;
use crate::instance::{self, arg_instance, push, tree};
use crate::runtime::{Bindings, lua_error, state, with_world};
use crate::signal;

/// How many undelivered messages one VM keeps waiting for a listener.
pub const MAX_WAITING: usize = 4096;

/// Remote messages that arrived before anything listened, in arrival order.
#[derive(Default)]
struct Waiting(Vec<Msg>);

/// The callers parked in an invocation, by call id, with the player an
/// `InvokeClient` waits on.
#[derive(Default)]
struct Calls(HashMap<u64, (YieldToken, Option<String>)>);

fn call_error(what: &str, msg: impl std::fmt::Display) -> mlua::Error {
    lua_error(codes::REMOTE_CALL, format!("{what}: {msg}"))
}

/// The link, which a remote call needs; a VM outside two worlds has no other
/// side to talk to.
fn need_link(lua: &Lua, what: &str, side: Side) -> Result<NetLink> {
    match link(lua)? {
        Some(l) if l.side == side => Ok(l),
        Some(_) => Err(call_error(
            what,
            match side {
                Side::Server => "it can only be called from the server",
                Side::Client => "it can only be called from a client",
            },
        )),
        None => Err(call_error(
            what,
            "this script context has no other side to send to (it is not one of two worlds)",
        )),
    }
}

fn id_of(lua: &Lua, e: Entity) -> Result<String> {
    Ok(with_world(lua, |w| w.id_of(e).map(str::to_string))?.unwrap_or_default())
}

/// The `Player` argument of `FireClient` and `InvokeClient`.
fn player_arg(lua: &Lua, v: &LuaValue, what: &str) -> Result<Entity> {
    let p = arg_instance(v, what)?;
    let ok = with_world(lua, |w| tree::class(w, p) == Some("Player"))?;
    if !ok {
        return Err(call_error(what, "the first argument must be a Player"));
    }
    Ok(p)
}

fn split_first(a: MultiValue) -> (LuaValue, MultiValue) {
    let mut it = a.into_iter();
    let first = it.next().unwrap_or(LuaValue::Nil);
    (first, it.collect())
}

fn fire_server(lua: &Lua, e: Entity, a: MultiValue, class: &str) -> Result<MultiValue> {
    let what = format!("{class}:FireServer");
    let l = need_link(lua, &what, Side::Client)?;
    let args = encode_all(lua, a, &what)?;
    let remote = id_of(lua, e)?;
    let mut s = l.lock();
    let player = s.local_player.clone();
    s.to_server.push_back(Msg::Event {
        remote,
        player,
        args,
    });
    Ok(MultiValue::new())
}

/// Queues `msg` for `player`'s client, or counts it undelivered when that
/// player has none here.
fn to_client(l: &NetLink, player: &str, msg: Msg) {
    let mut s = l.lock();
    if s.local_player.as_deref() == Some(player) {
        s.to_client.push_back(msg);
    } else {
        s.undelivered += 1;
    }
}

fn fire_client(lua: &Lua, e: Entity, a: MultiValue, class: &str) -> Result<MultiValue> {
    let what = format!("{class}:FireClient");
    let l = need_link(lua, &what, Side::Server)?;
    let (p, rest) = split_first(a);
    let p = player_arg(lua, &p, &what)?;
    let args = encode_all(lua, rest, &what)?;
    let remote = id_of(lua, e)?;
    let player = id_of(lua, p)?;
    to_client(
        &l,
        &player,
        Msg::Event {
            remote,
            player: None,
            args,
        },
    );
    Ok(MultiValue::new())
}

fn fire_all(lua: &Lua, e: Entity, a: MultiValue, class: &str) -> Result<MultiValue> {
    let what = format!("{class}:FireAllClients");
    let l = need_link(lua, &what, Side::Server)?;
    let args = encode_all(lua, a, &what)?;
    let remote = id_of(lua, e)?;
    let players = crate::players::players_of(lua)?;
    let local = l.local_player();
    let mut s = l.lock();
    for p in players {
        let id = with_world(lua, |w| w.id_of(p).map(str::to_string))?;
        if id.is_some() && id == local {
            s.to_client.push_back(Msg::Event {
                remote: remote.clone(),
                player: None,
                args: args.clone(),
            });
        } else {
            s.undelivered += 1;
        }
    }
    Ok(MultiValue::new())
}

/// Parks the caller of an invocation; the reply for `call` wakes it.
fn park_call(lua: &Lua, label: &str, call: u64, player: Option<String>) -> Result<MultiValue> {
    install(lua);
    let (values, token) = sched::park(lua, label, None)?;
    if let Some(mut c) = lua.app_data_mut::<Calls>() {
        c.0.insert(call, (token, player));
    }
    Ok(values)
}

fn invoke_server(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let what = "RemoteFunction:InvokeServer";
    let l = need_link(lua, what, Side::Client)?;
    let args = encode_all(lua, a, what)?;
    let remote = id_of(lua, e)?;
    let call = {
        let mut s = l.lock();
        s.next_call += 1;
        let call = s.next_call;
        let player = s.local_player.clone();
        s.to_server.push_back(Msg::Invoke {
            remote,
            player,
            call,
            args,
        });
        call
    };
    park_call(lua, what, call, None)
}

fn invoke_client(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let what = "RemoteFunction:InvokeClient";
    let l = need_link(lua, what, Side::Server)?;
    let (p, rest) = split_first(a);
    let p = player_arg(lua, &p, what)?;
    let args = encode_all(lua, rest, what)?;
    let remote = id_of(lua, e)?;
    let player = id_of(lua, p)?;
    let call = {
        let mut s = l.lock();
        if s.local_player.as_deref() != Some(player.as_str()) {
            return Err(call_error(
                what,
                "the player has no client in this process (only the local player has one)",
            ));
        }
        s.next_call += 1;
        let call = s.next_call;
        s.to_client.push_back(Msg::Invoke {
            remote,
            player: None,
            call,
            args,
        });
        call
    };
    park_call(lua, what, call, Some(player))
}

fn bindable_fire(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let args = encode_all(lua, a, "BindableEvent:Fire")?;
    let args = decode_all(lua, &args, Target::SameWorld)?;
    signal::fire(lua, e, "Event", args)?;
    Ok(MultiValue::new())
}

/// Registry key of the Luau function that runs a handler and reports.
const RUNNER_KEY: &str = "re_compat.net.runner";

/// Runs `cb` with `args` in a new script thread; `done` gets `pcall`'s
/// results (`ok` first). `queued` runs it where a deferred signal handler
/// would (at the end of the cycle, in order with them); otherwise it starts
/// at once.
fn run_handler(
    lua: &Lua,
    cb: Function,
    done: Function,
    args: MultiValue,
    label: &str,
    queued: bool,
) -> Result<()> {
    let runner: Function = match lua.named_registry_value::<Option<Function>>(RUNNER_KEY)? {
        Some(r) => r,
        None => {
            let r: Function = lua
                .load("return function(cb, done, ...) done(pcall(cb, ...)) end")
                .set_name("=handler")
                .call(())?;
            lua.set_named_registry_value(RUNNER_KEY, &r)?;
            r
        }
    };
    let mut all = MultiValue::from_iter([LuaValue::Function(cb), LuaValue::Function(done)]);
    all.extend(args);
    if queued {
        sched::defer(lua, runner, all, label, None)?;
    } else {
        sched::spawn(lua, runner, all, label)?;
    }
    Ok(())
}

/// The message of a handler's error value.
fn error_text(v: Option<&LuaValue>) -> String {
    match v {
        Some(LuaValue::String(s)) => s.to_string_lossy(),
        Some(LuaValue::Error(e)) => e.to_string(),
        Some(other) => other
            .to_string()
            .unwrap_or_else(|_| other.type_name().to_string()),
        None => "nil".into(),
    }
}

/// Where a `BindableFunction:Invoke` stands: its outcome when the handler
/// finished without yielding, else the caller's park.
#[derive(Default)]
struct Pending {
    outcome: Option<std::result::Result<MultiValue, String>>,
    token: Option<YieldToken>,
}

fn bindable_invoke(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let what = "BindableFunction:Invoke";
    let Some(cb) = instance::callback(lua, e, "OnInvoke")? else {
        return Err(call_error(what, "OnInvoke is not set"));
    };
    let args = encode_all(lua, a, what)?;
    let args = decode_all(lua, &args, Target::SameWorld)?;
    let state: Rc<RefCell<Pending>> = Default::default();
    let st = state.clone();
    let done = lua.create_function(move |lua, results: MultiValue| {
        let mut it = results.into_iter();
        let ok = matches!(it.next(), Some(LuaValue::Boolean(true)));
        let rest: MultiValue = it.collect();
        let outcome = if ok {
            encode_all(lua, rest, "BindableFunction.OnInvoke result")
                .and_then(|r| decode_all(lua, &r, Target::SameWorld))
                .map_err(|e| e.to_string())
        } else {
            Err(format!(
                "[{}] {what}: the handler raised: {}",
                codes::REMOTE_CALL,
                error_text(rest.front())
            ))
        };
        let token = st.borrow().token;
        match (token, outcome) {
            (Some(t), Ok(r)) => {
                sched::wake(lua, t, r, WakeMode::Deferred)?;
            }
            (Some(t), Err(m)) => {
                sched::wake_error(lua, t, &m, WakeMode::Deferred)?;
            }
            (None, o) => st.borrow_mut().outcome = Some(o),
        }
        Ok(())
    })?;
    // The handler starts at once; a handler that does not yield returns its
    // results before Invoke returns, as in Roblox.
    run_handler(lua, cb, done, args, "BindableFunction.OnInvoke", false)?;
    let finished = state.borrow_mut().outcome.take();
    match finished {
        Some(Ok(values)) => Ok(values),
        Some(Err(m)) => Err(mlua::Error::runtime(m)),
        None => {
            let (values, token) = sched::park(lua, what, None)?;
            state.borrow_mut().token = Some(token);
            Ok(values)
        }
    }
}

/// Delivers the messages that arrived for this VM, and retries the ones that
/// were waiting for a listener, in order.
pub(crate) fn deliver(lua: &Lua, l: &NetLink) -> Result<()> {
    install(lua);
    let mut msgs: Vec<Msg> = lua
        .app_data_mut::<Waiting>()
        .map(|mut w| std::mem::take(&mut w.0))
        .unwrap_or_default();
    {
        let mut s = l.lock();
        let q = match l.side {
            Side::Server => &mut s.to_server,
            Side::Client => &mut s.to_client,
        };
        msgs.extend(q.drain(..));
    }
    let mut waiting = Vec::new();
    for m in msgs {
        if let Some(back) = deliver_one(lua, l, m)? {
            waiting.push(back);
        }
    }
    if waiting.len() > MAX_WAITING {
        let drop = waiting.len() - MAX_WAITING;
        re_script::vm::output(
            lua,
            true,
            format!("{drop} remote messages were dropped: nothing listened to them"),
        );
        waiting.drain(..drop);
    }
    if let Some(mut w) = lua.app_data_mut::<Waiting>() {
        w.0 = waiting;
    }
    Ok(())
}

/// Delivers one message; gives it back when it must wait for a listener.
fn deliver_one(lua: &Lua, l: &NetLink, m: Msg) -> Result<Option<Msg>> {
    let server = l.side == Side::Server;
    match m {
        Msg::Event {
            ref remote,
            ref player,
            ref args,
        } => {
            let Some(e) = with_world(lua, |w| instance_by_id(w, Some(remote)))? else {
                return Ok(None); // the remote is gone on this side
            };
            let event = if server {
                "OnServerEvent"
            } else {
                "OnClientEvent"
            };
            if !signal::listens(lua, e, event) {
                return Ok(Some(m));
            }
            let mut values = MultiValue::new();
            if server {
                let Some(p) = with_world(lua, |w| instance_by_id(w, player.as_deref()))? else {
                    return Ok(None); // the sender left
                };
                values.push_back(push(lua, p)?);
            }
            values.extend(decode_all(lua, args, Target::OtherWorld)?);
            signal::fire(lua, e, event, values)?;
            Ok(None)
        }
        Msg::Invoke {
            ref remote,
            ref player,
            call,
            ref args,
        } => {
            let reply_to = |result: std::result::Result<Vec<NetValue>, String>| {
                let mut s = l.lock();
                let msg = Msg::Reply { call, result };
                if server {
                    s.to_client.push_back(msg);
                } else {
                    s.to_server.push_back(msg);
                }
            };
            let Some(e) = with_world(lua, |w| instance_by_id(w, Some(remote)))? else {
                reply_to(Err(format!(
                    "[{}] the RemoteFunction does not exist on the {}",
                    codes::REMOTE_CALL,
                    if server { "server" } else { "client" }
                )));
                return Ok(None);
            };
            let name = if server {
                "OnServerInvoke"
            } else {
                "OnClientInvoke"
            };
            let Some(cb) = instance::callback(lua, e, name)? else {
                return Ok(Some(m)); // waits for its handler
            };
            let mut values = MultiValue::new();
            if server {
                let Some(p) = with_world(lua, |w| instance_by_id(w, player.as_deref()))? else {
                    return Ok(None);
                };
                values.push_back(push(lua, p)?);
            }
            values.extend(decode_all(lua, args, Target::OtherWorld)?);
            let link = l.clone();
            let label = format!("RemoteFunction.{name}");
            let done = lua.create_function(move |lua, results: MultiValue| {
                let mut it = results.into_iter();
                let ok = matches!(it.next(), Some(LuaValue::Boolean(true)));
                let rest: MultiValue = it.collect();
                let result = if ok {
                    encode_all(lua, rest, &format!("RemoteFunction.{name} result"))
                        .map_err(|e| e.to_string())
                } else {
                    Err(format!(
                        "[{}] the {}'s handler raised: {}",
                        codes::REMOTE_CALL,
                        if server { "server" } else { "client" },
                        error_text(rest.front())
                    ))
                };
                let mut s = link.lock();
                let msg = Msg::Reply { call, result };
                if server {
                    s.to_client.push_back(msg);
                } else {
                    s.to_server.push_back(msg);
                }
                Ok(())
            })?;
            let queued = state(lua)?.behavior == crate::signal::SignalBehavior::Deferred;
            run_handler(lua, cb, done, values, &label, queued)?;
            Ok(None)
        }
        Msg::Reply { call, result } => {
            let token = lua
                .app_data_mut::<Calls>()
                .and_then(|mut c| c.0.remove(&call))
                .map(|(t, _)| t);
            let Some(token) = token else {
                return Ok(None);
            };
            match result {
                Ok(values) => {
                    let values = decode_all(lua, &values, Target::OtherWorld)?;
                    sched::wake(lua, token, values, WakeMode::Deferred)?;
                }
                Err(msg) => {
                    let msg = msg.trim_start_matches("runtime error: ").to_string();
                    sched::wake_error(lua, token, &msg, WakeMode::Deferred)?;
                }
            }
            Ok(None)
        }
        Msg::Prompt {
            prompt,
            player,
            action,
        } => {
            crate::services::extra::prompts::on_server(lua, &prompt, &player, action)?;
            Ok(None)
        }
        Msg::Kick { player } => {
            crate::players::kicked(lua, &player)?;
            Ok(None)
        }
        Msg::Chat { player, text } => {
            crate::players::chat_on_client(lua, l, &player, &text)?;
            Ok(None)
        }
    }
}

/// Fails the `InvokeClient` calls waiting on `player` (who left).
pub(crate) fn fail_calls_for(lua: &Lua, player: &str) -> Result<()> {
    let tokens: Vec<YieldToken> = match lua.app_data_mut::<Calls>() {
        Some(mut c) => {
            let ids: Vec<u64> =
                c.0.iter()
                    .filter(|(_, (_, p))| p.as_deref() == Some(player))
                    .map(|(k, _)| *k)
                    .collect();
            ids.into_iter()
                .filter_map(|k| c.0.remove(&k).map(|(t, _)| t))
                .collect()
        }
        None => Vec::new(),
    };
    for t in tokens {
        sched::wake_error(
            lua,
            t,
            &format!(
                "[{}] RemoteFunction:InvokeClient: the player left before answering",
                codes::REMOTE_CALL
            ),
            WakeMode::Deferred,
        )?;
    }
    Ok(())
}

/// Fails every invocation waiting in this VM (the client's player left).
pub(crate) fn fail_all_calls(lua: &Lua, why: &str) -> Result<()> {
    let tokens: Vec<YieldToken> = match lua.app_data_mut::<Calls>() {
        Some(mut c) => c.0.drain().map(|(_, (t, _))| t).collect(),
        None => Vec::new(),
    };
    for t in tokens {
        sched::wake_error(
            lua,
            t,
            &format!("[{}] RemoteFunction: {why}", codes::REMOTE_CALL),
            WakeMode::Deferred,
        )?;
    }
    Ok(())
}

/// Calls a `BindableFunction`'s `OnInvoke` from the host (a harness's
/// `invoke` step) with already-built arguments, and waits for its results
/// inside the same run. Returns them, or the handler's error message.
pub(crate) fn invoke_bindable_now(
    lua: &Lua,
    e: Entity,
    args: MultiValue,
) -> Result<std::result::Result<MultiValue, String>> {
    let Some(cb) = instance::callback(lua, e, "OnInvoke")? else {
        return Ok(Err(format!(
            "[{}] BindableFunction:Invoke: OnInvoke is not set",
            codes::REMOTE_CALL
        )));
    };
    let slot: std::rc::Rc<std::cell::RefCell<Option<std::result::Result<MultiValue, String>>>> =
        Default::default();
    let out = slot.clone();
    let done = lua.create_function(move |_, results: MultiValue| {
        let mut it = results.into_iter();
        let ok = matches!(it.next(), Some(LuaValue::Boolean(true)));
        let rest: MultiValue = it.collect();
        *out.borrow_mut() = Some(if ok {
            Ok(rest)
        } else {
            Err(error_text(rest.front()))
        });
        Ok(())
    })?;
    run_handler(lua, cb, done, args, "BindableFunction.OnInvoke", false)?;
    sched::settle(lua)?;
    let r = slot.borrow_mut().take();
    Ok(r.unwrap_or_else(|| Err("the handler is still running (it yielded)".into())))
}

type Imp = fn(&Lua, Entity, MultiValue) -> Result<MultiValue>;

/// Registers the remotes' and bindables' methods. Their callbacks need no
/// accessor: the instance layer keeps them per VM.
pub fn register(b: &mut Bindings) {
    for class in ["RemoteEvent", "UnreliableRemoteEvent"] {
        let c: &'static str = class;
        b.method(
            class,
            "FireServer",
            Arc::new(move |l, e, a| fire_server(l, e, a, c)),
        );
        b.method(
            class,
            "FireClient",
            Arc::new(move |l, e, a| fire_client(l, e, a, c)),
        );
        b.method(
            class,
            "FireAllClients",
            Arc::new(move |l, e, a| fire_all(l, e, a, c)),
        );
    }
    let yielding: &[(&str, &str, Imp)] = &[
        ("RemoteFunction", "InvokeServer", invoke_server),
        ("RemoteFunction", "InvokeClient", invoke_client),
        ("BindableFunction", "Invoke", bindable_invoke),
    ];
    for (class, name, f) in yielding {
        b.yielding_method(class, name, Arc::new(*f));
    }
    b.method("BindableEvent", "Fire", Arc::new(bindable_fire));
}

/// Installs the per-VM state of the remotes.
fn install(lua: &Lua) {
    if lua.app_data_ref::<Waiting>().is_none() {
        lua.set_app_data(Waiting::default());
    }
    if lua.app_data_ref::<Calls>().is_none() {
        lua.set_app_data(Calls::default());
    }
}
