//! `engine.d.luau` and `generator.d.luau`: the snapshots, and luau-lsp
//! loading them.

use std::path::Path;
use std::process::Command;

#[test]
fn engine_definitions_snapshot() {
    insta::assert_snapshot!("engine.d.luau", re_script::definitions());
}

#[test]
fn generator_definitions_snapshot() {
    insta::assert_snapshot!("generator.d.luau", re_script::generator::definitions());
}

#[test]
fn write_definitions_puts_them_in_the_project_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = re_script::write_definitions(dir.path()).unwrap();
    assert_eq!(path, dir.path().join(re_script::DEFINITIONS_PATH));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        re_script::definitions()
    );
    // Unchanged contents are not rewritten.
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    re_script::write_definitions(dir.path()).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before
    );
}

/// Runs `luau-lsp analyze` over `script` with the generated engine
/// definitions. `None` when luau-lsp is not installed (CI installs it, so
/// there it must be found).
fn analyze(dir: &Path, script: &str) -> Option<(bool, String)> {
    analyze_with(dir, script, "engine", &re_script::definitions())
}

/// Runs `luau-lsp analyze` over `script` with `definitions` loaded as `@name`.
fn analyze_with(dir: &Path, script: &str, name: &str, definitions: &str) -> Option<(bool, String)> {
    let defs = dir.join(format!("{name}.d.luau"));
    std::fs::write(&defs, definitions).unwrap();
    let file = dir.join("check.luau");
    std::fs::write(&file, script).unwrap();
    let out = match Command::new("luau-lsp")
        .arg("analyze")
        .arg("--platform=standard")
        .arg("--flag:LuauSolverV2=true")
        .arg(format!("--definitions=@{name}={}", defs.display()))
        .arg("--formatter=gnu")
        .arg(&file)
        .output()
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            assert!(
                std::env::var_os("CI").is_none(),
                "luau-lsp must be installed on CI"
            );
            println!("skipped: luau-lsp not found");
            return None;
        }
        Err(e) => panic!("luau-lsp: {e}"),
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    println!("{text}");
    Some((out.status.success(), text))
}

const GOOD: &str = r#"--!strict
local hero = world.get("hero")
if hero then
    local p: vector = hero:get("Transform", "translation")
    hero:set("Transform", "translation", p + vector.create(0, 1, 0))
    for _, child in hero:children() do
        print(child.name, child.alive)
    end
end
world.define_component("Collectible", { value = "i32" })
for _, coin in world.query("Collectible", "Transform") do
    physics.on_touch(coin, function(other: Entity, info)
        audio.play("assets/sfx/pickup.wav", info.point)
        coin:destroy()
    end)
end
local q = quat.from_euler(0, math.pi, 0) * quat.identity()
local v: vector = q * vector.create(1, 0, 0)
local c = time.on_step(function(dt: number)
    if input.pressed("jump") then
        local dt2: number = task.wait(0.5)
        print(dt + dt2, time.now())
    end
end)
c:disconnect()
task.spawn(function() task.wait() end)
local g = proc.graph("island", { seed = 1 })
local hills = g:param("hills", { type = "number", default = 16, min = 1, max = 64, doc = "Hill height." })
local h = g:node("terrain.heightfield", { size = 64, height = hills })
g:node("terrain.mesh", nil, { field = h:out("out") })
g:output("ground", h, { doc = "The heights." })
warn(script.path, script.context, v.x)
"#;

#[test]
fn luau_lsp_loads_the_definitions_and_checks_against_them() {
    let dir = tempfile::tempdir().unwrap();
    let Some((ok, text)) = analyze(dir.path(), GOOD) else {
        return;
    };
    assert!(
        ok,
        "the definitions or the sample do not type-check:\n{text}"
    );

    let Some((ok, text)) = analyze(
        dir.path(),
        "--!strict\nlocal e = world.get(5)\nworld.Spawn()\nlocal n: number = time.now() .. \"s\"\n",
    ) else {
        return;
    };
    assert!(!ok, "a type error must be reported");
    assert!(text.contains("check.luau:2"), "{text}");
    assert!(text.contains("check.luau:3"), "{text}");
}

const GOOD_GENERATOR: &str = r#"--!strict
return proc.node({
    kind = "project.rocks",
    version = 2,
    doc = "Scatters rocks.",
    inputs = { { name = "field", type = "field", required = true, doc = "The ground." } },
    output = "points",
    params = {
        count = proc.param.integer({ default = 10, min = 0, max = 1000 }),
        size = proc.param.number({ default = 1.5, doc = "Metres." }),
        shape = proc.param.enum({ values = { "round", "flat" }, default = "round" }),
        tint = proc.param.vec3({ default = vector.create(1, 1, 1) }),
        on = proc.param.bool(),
        label = proc.param.string({ default = "rock" }),
    },
    uses_seed = true,
    migrate = function(from, params)
        return params
    end,
    eval = function(ctx, inputs, params)
        local rng = ctx.rng:fork(1)
        local n: number = rng:int(1, params.count) + rng:next() * ctx.seed
        ctx:log(`node {ctx.node} v{ctx.version}: {n}`)
        local q = quat.from_euler(0, math.pi, 0)
        local up: vector = q * vector.create(0, 1, 0)
        print(color.new(1, 0.5, 0), up, inputs.field)
        return {}
    end,
})
"#;

#[test]
fn luau_lsp_loads_the_generator_definitions_and_checks_against_them() {
    let dir = tempfile::tempdir().unwrap();
    let defs = re_script::generator::definitions();
    let Some((ok, text)) = analyze_with(dir.path(), GOOD_GENERATOR, "generator", &defs) else {
        return;
    };
    assert!(
        ok,
        "the definitions or the sample do not type-check:\n{text}"
    );

    // The game's API is not there, and a parameter type must exist.
    let Some((ok, text)) = analyze_with(
        dir.path(),
        "--!strict\nlocal e = world.get(\"x\")\nlocal p = proc.param.numbr({})\nlocal n: number = proc.param.number({ min = \"0\" })\n",
        "generator",
        &defs,
    ) else {
        return;
    };
    assert!(!ok, "a type error must be reported");
    for line in ["check.luau:2", "check.luau:3", "check.luau:4"] {
        assert!(text.contains(line), "{line}: {text}");
    }
}
