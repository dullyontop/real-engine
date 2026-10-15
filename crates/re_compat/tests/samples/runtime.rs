//! WP11's steps: which scripts start, the `require` graph a boot reaches, and
//! booting a sample to its first frames through `re_compat::testing`.
//!
//! * `runtime.start`: the scripts that start in each context, by full name,
//!   in start order. `args`: `{ "server": [...], "client": [...] }`, plus
//!   `"never_under": [service, ...]`, roots under which nothing may start.
//! * `runtime.boot`: imports the sample, starts its scripts and runs
//!   `frames` frames (default 1). Fails on any uncaught error, unless its
//!   message contains one of `allow` (each an allowed substring, for the
//!   parts of the API later work packages build). Until those parts exist,
//!   `stubs` names a class data file in the kit whose classes stand in for
//!   them (a new class the crate's data already declares is skipped; every
//!   method declared there returns an empty value of its
//!   declared type, and a builtin property of an instance type reads as what
//!   was written to it, else the instance's first child of that class), and `prelude` gives Luau run in `server` and `client`
//!   before the scripts start (to make stand-in instances such as the local
//!   player). `cpu_budget_ms` (default 10 000, Roblox's script timeout)
//!   replaces the per-resume budget. `two_worlds: true` runs the server and
//!   the client as two worlds with the `[compat] players` joined.
//! * `runtime.graph`: after `runtime.boot`, every `require` the boot ran
//!   resolved (no `RE3004`), and nothing it required sits under
//!   `never_under`.

use std::collections::BTreeSet;

use std::sync::Arc;

use mlua::{Lua, MultiValue, Value as LuaValue};
use re_compat::data::ClassFile;
use re_compat::runtime::scripts::{StartOptions, startable};
use re_compat::testing::{Harness, HarnessOptions, Side};
use re_compat::{Bindings, CompatSetup, GAME_ID};
use re_scene::Entity;
use serde_json::Value as J;

use super::Ctx;

pub fn step(name: &str) -> Option<super::Step> {
    Some(match name {
        "runtime.start" => start,
        "runtime.boot" => boot,
        "runtime.graph" => graph,
        _ => return None,
    })
}

fn strings(v: &J) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The values written to the stand-ins' instance properties.
type Refs = std::sync::Mutex<std::collections::HashMap<(Entity, String), Option<Entity>>>;

fn refs() -> &'static Refs {
    static REFS: std::sync::OnceLock<Refs> = std::sync::OnceLock::new();
    REFS.get_or_init(Default::default)
}

/// An empty value of a declared return type.
fn empty(lua: &Lua, ty: &str) -> mlua::Result<LuaValue> {
    Ok(match ty {
        "bool" | "boolean" => LuaValue::Boolean(false),
        "number" | "int" | "float" | "double" | "i32" | "i64" | "f32" | "f64" => {
            LuaValue::Number(0.0)
        }
        "string" => LuaValue::String(lua.create_string("")?),
        t if t == "table" || t.starts_with('{') => LuaValue::Table(lua.create_table()?),
        _ => LuaValue::Nil,
    })
}

/// The standard setup plus the kit's stand-in classes at `stubs`.
fn setup(ctx: &Ctx, stubs: Option<&str>) -> Result<Arc<CompatSetup>, String> {
    let mut b = Bindings::standard().map_err(|e| e.to_string())?;
    if let Some(file) = stubs {
        let path = ctx.kit_path(file);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{file}: {e}"))?;
        let mut data = ClassFile::parse(file, &text).map_err(|e| e.to_string())?;
        // A class the crate's own data declares by now is no longer stood in.
        let known = re_compat::ClassRegistry::builtin().map_err(|e| e.to_string())?;
        data.classes
            .retain(|c| c.extend || known.class(&c.name).is_err());
        // Nor is a member an extension adds that the class has by now.
        for c in data.classes.iter_mut().filter(|c| c.extend) {
            let has = |m: &str| known.member(&c.name, m).is_ok();
            c.properties.retain(|p| !has(&p.name));
            c.methods.retain(|m| !has(&m.name));
            c.events.retain(|e| !has(&e.name));
        }
        for c in &data.classes {
            for p in &c.properties {
                if !matches!(p.storage, re_compat::data::StorageDoc::Builtin) {
                    continue;
                }
                let ty = p.ty.clone();
                let key = format!("{}.{}", c.name, p.name);
                let k2 = key.clone();
                b.accessor(
                    &c.name,
                    &p.name,
                    re_compat::Accessor {
                        get: Arc::new(move |w, e| {
                            if let Some(v) = refs().lock().unwrap().get(&(e, key.clone())) {
                                return Ok(re_reflect::Value::Entity(*v));
                            }
                            let child = re_scene::hierarchy::children(&w.ecs, e)
                                .iter()
                                .copied()
                                .find(|c| {
                                    re_compat::instance::tree::class(w, *c) == Some(ty.as_str())
                                });
                            Ok(re_reflect::Value::Entity(child))
                        }),
                        set: (!p.read_only).then(|| -> Arc<re_compat::SetFn> {
                            Arc::new(move |_, e, v| {
                                if let re_reflect::Value::Entity(x) = v {
                                    refs().lock().unwrap().insert((e, k2.clone()), x);
                                }
                                Ok(())
                            })
                        }),
                    },
                );
            }
            for m in &c.methods {
                let returns = m.returns.clone();
                b.method(
                    &c.name,
                    &m.name,
                    Arc::new(move |lua: &Lua, _: Entity, _: MultiValue| {
                        returns
                            .iter()
                            .map(|t| empty(lua, t))
                            .collect::<mlua::Result<MultiValue>>()
                    }),
                );
            }
        }
        b.classes(|r| r.classes(file, data));
    }
    CompatSetup::new(b).map_err(|e| e.to_string())
}

fn open(ctx: &mut Ctx) -> Result<Harness, String> {
    ctx.import()?;
    Harness::open(&ctx.project(), HarnessOptions::default()).map_err(|d| d.render_human())
}

fn start(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let h = open(ctx)?;
    let setup = CompatSetup::standard().map_err(|e| e.to_string())?;
    let opts = StartOptions {
        templates_as_player: true,
    };
    let never = strings(&args["never_under"]);
    let mut problems = Vec::new();
    for side in Side::BOTH {
        let w = h.world(side);
        let game = w.entity(GAME_ID).map_err(|e| e.to_string())?;
        let got: Vec<String> = startable(w, &setup.classes, game, side.kind(), opts)
            .into_iter()
            .map(|e| re_compat::instance::tree::full_name(w, e))
            .collect();
        for g in &got {
            if never
                .iter()
                .any(|n| g == n || g.starts_with(&format!("{n}.")))
            {
                problems.push(format!(
                    "{g} starts in {side:?} under a root where nothing may"
                ));
            }
        }
        let key = match side {
            Side::Server => "server",
            Side::Client => "client",
        };
        if args.get(key).is_some() {
            let want = strings(&args[key]);
            if got != want {
                problems.push(format!("{key}: got {got:?}, want {want:?}"));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// The harness after a boot, kept for `runtime.graph`.
struct Booted(Harness);

fn boot(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let frames = args["frames"].as_u64().unwrap_or(1) as u32;
    let allow = strings(&args["allow"]);
    ctx.import()?;
    let setup = setup(ctx, args["stubs"].as_str())?;
    // Roblox stops a script after 10 s without a yield, not after the
    // engine's per-resume budget; a boot's first resume runs whole module
    // trees.
    let opts = HarnessOptions {
        cpu_budget_ms: Some(args["cpu_budget_ms"].as_u64().unwrap_or(10_000)),
        two_worlds: args["two_worlds"].as_bool().unwrap_or(false),
        ..HarnessOptions::default()
    };
    let mut h = Harness::open_with(&ctx.project(), &setup, opts).map_err(|d| d.render_human())?;
    for side in Side::BOTH {
        let key = match side {
            Side::Server => "server",
            Side::Client => "client",
        };
        if let Some(src) = args["prelude"][key].as_str() {
            h.exec(side, "prelude", src);
        }
    }
    h.start();
    h.step(frames);
    let errors: Vec<String> = h
        .uncaught()
        .iter()
        .map(|(side, d)| format!("{side:?}: {}", d.render_human()))
        .filter(|m| !allow.iter().any(|a| m.contains(a.as_str())))
        .collect();
    ctx.state
        .insert("runtime.booted".into(), Box::new(Booted(h)));
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} uncaught error(s) in {frames} frame(s):\n{}",
            errors.len(),
            errors.join("\n")
        ))
    }
}

fn graph(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let never = strings(&args["never_under"]);
    let Some(b) = ctx
        .state
        .get("runtime.booted")
        .and_then(|b| b.downcast_ref::<Booted>())
    else {
        return Err("runtime.graph needs runtime.boot first".into());
    };
    let h = &b.0;
    let mut problems = Vec::new();
    for (side, d) in h.uncaught() {
        if d.code == re_script::codes::REQUIRE {
            problems.push(format!("{side:?}: {}", d.message));
        }
    }
    let mut required = BTreeSet::new();
    for side in Side::BOTH {
        for m in h.required(side) {
            if never.iter().any(|n| m.starts_with(&format!("{n}."))) {
                problems.push(format!("{side:?} required {m}"));
            }
            required.insert(m);
        }
    }
    if let Some(n) = args["min_required"].as_u64()
        && (required.len() as u64) < n
    {
        problems.push(format!(
            "the boot required {} distinct modules, want at least {n}",
            required.len()
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}
