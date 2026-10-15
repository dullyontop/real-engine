//! `engine.d.luau`: the snapshot, and luau-lsp loading it.

use std::path::Path;
use std::process::Command;

#[test]
fn engine_definitions_snapshot() {
    insta::assert_snapshot!("engine.d.luau", re_script::definitions());
}

/// Runs `luau-lsp analyze` over `script` with the generated definitions. `None`
/// when luau-lsp is not installed (CI installs it, so there it must be found).
fn analyze(dir: &Path, script: &str) -> Option<(bool, String)> {
    let defs = dir.join("engine.d.luau");
    std::fs::write(&defs, re_script::definitions()).unwrap();
    let file = dir.join("check.luau");
    std::fs::write(&file, script).unwrap();
    let out = match Command::new("luau-lsp")
        .arg("analyze")
        .arg("--platform=standard")
        .arg("--flag:LuauSolverV2=true")
        .arg(format!("--definitions=@engine={}", defs.display()))
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
local h = g:node("terrain.heightfield", { size = 64 })
g:node("terrain.mesh", nil, { field = h:out("out") })
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
