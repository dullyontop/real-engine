//! The pure-function steps (WP10): a sample's own modules run through
//! `require` in a bare compat VM with the datatypes and enums installed.
//!
//! * `pure.eval` (`{"code": luau, "expect": [value, ...], "rel": r?}`): runs
//!   `code` and compares what it returns with `expect`, in order. A number
//!   matches within relative tolerance `r` (default 0: exactly equal as
//!   doubles); `[x, y]` or `[x, y, z]` matches a Vector2, a Vector3 or a
//!   list of numbers the same way; strings, booleans and `null` match
//!   exactly.
//!
//! The VM has no data model yet, only what pure modules reach: instances are
//! path stand-ins (`script`, `script.Parent.X`, `game:GetService(name).X`,
//! and `inst(path)` for a `/`-separated path from the service down), and
//! `require` of a stand-in runs the ModuleScript at that path in the imported
//! project, once. Raycasts on a stand-in hit nothing: an empty world.

use std::path::Path;

use mlua::{Lua, Table, Value};
use serde_json::Value as J;

use super::Ctx;

pub fn step(name: &str) -> Option<super::Step> {
    Some(match name {
        "pure.eval" => eval,
        _ => return None,
    })
}

/// The stand-in instances, written in Luau.
const STAND_INS: &str = r#"
local load = ...
local nodes = {}
local node
local function parent(path)
    return string.match(path, "^(.*)/[^/]*$")
end
local function raycast()
    return nil
end
local mt = {
    __type = "Instance",
    __index = function(n, k)
        local path = rawget(n, "__path")
        if k == "Parent" then
            local p = parent(path)
            return if p then node(p) else node("")
        elseif k == "Name" then
            return string.match(path, "[^/]*$")
        elseif k == "GetService" or k == "FindFirstChild" or k == "WaitForChild" then
            return function(self, name)
                local base = rawget(self, "__path")
                return node(if base == "" then name else base .. "/" .. name)
            end
        elseif k == "Raycast" or k == "Blockcast" or k == "Spherecast" or k == "Shapecast" then
            return raycast
        end
        return node(if path == "" then k else path .. "/" .. k)
    end,
    __tostring = function(n)
        return rawget(n, "__path")
    end,
}
node = function(path)
    local n = nodes[path]
    if n == nil then
        n = setmetatable({ __path = path }, mt)
        nodes[path] = n
    end
    return n
end
local cache = {}
local function require(n)
    local path = if type(n) == "table" then rawget(n, "__path") else nil
    if type(path) ~= "string" then
        error(`require expects an instance, got {typeof(n)}`, 2)
    end
    local hit = cache[path]
    if hit == nil then
        hit = { load(path, node(path)) }
        cache[path] = hit
    end
    return hit[1]
end
return node, require
"#;

fn eval(ctx: &mut Ctx, args: &J) -> Result<(), String> {
    let code = args["code"].as_str().ok_or("args.code")?;
    let expect = args["expect"].as_array().ok_or("args.expect")?;
    let rel = args.get("rel").and_then(J::as_f64).unwrap_or(0.0);
    ctx.import()?;
    let modules = modules(&ctx.project())?;
    let lua = Lua::new();
    let got = run(&lua, modules, code).map_err(|e| format!("the code failed: {e}"))?;
    if got.len() != expect.len() {
        return Err(format!(
            "it returned {} value(s), want {}",
            got.len(),
            expect.len()
        ));
    }
    let mut problems = Vec::new();
    for (i, (g, w)) in got.iter().zip(expect).enumerate() {
        if let Err(m) = matches(g, w, rel) {
            problems.push(format!("value {}: {m}", i + 1));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// The ModuleScripts of the imported project: instance path to source.
fn modules(project: &Path) -> Result<Vec<(String, String)>, String> {
    let text = std::fs::read_to_string(project.join("scenes/main.scene.json"))
        .map_err(|e| e.to_string())?;
    let doc = re_scene::scene::from_json(&text).map_err(|e| e.to_string())?;
    let field = |e: &re_scene::scene::EntityDoc, c: &str, f: &str| match e
        .components
        .get(c)
        .and_then(|x| x.get(f))
    {
        Some(re_reflect::Doc::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let by_id: std::collections::HashMap<&str, &re_scene::scene::EntityDoc> =
        doc.entities.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut out = Vec::new();
    for e in &doc.entities {
        if field(e, "ClassMarker", "class").as_deref() != Some("ModuleScript") {
            continue;
        }
        let Some(file) = field(e, "ScriptRef", "path") else {
            continue;
        };
        let mut names = vec![];
        let mut at = Some(e);
        while let Some(x) = at.filter(|x| x.id != "game") {
            names.push(x.name.as_str());
            at = x.parent.as_deref().and_then(|p| by_id.get(p).copied());
        }
        names.reverse();
        let source =
            std::fs::read_to_string(project.join(&file)).map_err(|e| format!("{file}: {e}"))?;
        out.push((names.join("/"), source));
    }
    Ok(out)
}

fn run(lua: &Lua, modules: Vec<(String, String)>, code: &str) -> mlua::Result<Vec<J>> {
    let globals = lua.globals();
    re_compat::datatypes::install(lua, &globals, 0)?;
    let sources: std::collections::HashMap<String, String> = modules.into_iter().collect();
    let load = lua.create_function(move |lua, (path, node): (String, Value)| {
        let Some(source) = sources.get(&path) else {
            return Err(mlua::Error::runtime(format!(
                "no ModuleScript at {path} in the imported project"
            )));
        };
        let env = lua.create_table()?;
        env.set("script", node)?;
        let mt = lua.create_table()?;
        mt.set("__index", lua.globals())?;
        env.set_metatable(Some(mt))?;
        lua.load(source.as_str())
            .set_name(format!("={path}"))
            .set_environment(env)
            .eval::<Value>()
    })?;
    let (node, require): (mlua::Function, mlua::Function) =
        lua.load(STAND_INS).set_name("=stand-ins").call(load)?;
    let game: Value = node.call("")?;
    globals.set("game", game)?;
    globals.set("workspace", node.call::<Value>("Workspace")?)?;
    globals.set("require", require)?;
    globals.set("inst", node)?;
    lua.sandbox(true)?;
    let values: mlua::MultiValue = lua.load(code).set_name("=check").eval()?;
    values.into_iter().map(|v| to_json(lua, v)).collect()
}

fn to_json(lua: &Lua, v: Value) -> mlua::Result<J> {
    Ok(match v {
        Value::Nil => J::Null,
        Value::Boolean(b) => J::Bool(b),
        Value::Integer(i) => J::from(i),
        Value::Number(n) => J::from(n),
        Value::Vector(v) => J::from(vec![v.x() as f64, v.y() as f64, v.z() as f64]),
        Value::String(s) => J::String(s.to_string_lossy()),
        Value::UserData(ud) => match re_compat::datatypes::userdata_to_value(lua, &ud)? {
            Some(re_reflect::Value::Record(_, fields)) if fields.len() == 2 => {
                let f = |v: &re_reflect::Value| match v {
                    re_reflect::Value::Float(x) => J::from(*x),
                    other => J::String(format!("{other:?}")),
                };
                J::from(vec![f(&fields[0]), f(&fields[1])])
            }
            _ => J::String(
                lua.coerce_string(Value::UserData(ud))?
                    .map_or_else(|| "userdata".to_string(), |s| s.to_string_lossy()),
            ),
        },
        Value::Table(t) => list(lua, t)?,
        other => J::String(other.type_name().to_string()),
    })
}

fn list(lua: &Lua, t: Table) -> mlua::Result<J> {
    let mut out = Vec::new();
    for v in t.sequence_values::<Value>() {
        out.push(to_json(lua, v?)?);
    }
    Ok(J::Array(out))
}

fn matches(got: &J, want: &J, rel: f64) -> Result<(), String> {
    let bad = || Err(format!("got {got}, want {want}"));
    match (got, want) {
        (J::Number(g), J::Number(w)) => {
            let (g, w) = (
                g.as_f64().unwrap_or(f64::NAN),
                w.as_f64().unwrap_or(f64::NAN),
            );
            if g == w || (g - w).abs() <= rel * w.abs().max(g.abs()) {
                Ok(())
            } else {
                bad()
            }
        }
        (J::Array(g), J::Array(w)) if g.len() == w.len() => {
            for (a, b) in g.iter().zip(w) {
                if matches(a, b, rel).is_err() {
                    return bad();
                }
            }
            Ok(())
        }
        _ if got == want => Ok(()),
        _ => bad(),
    }
}
