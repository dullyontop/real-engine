//! `roblox.d.luau`: generated from the compat tables, loaded by luau-lsp, and
//! written into a project once.

use std::path::Path;
use std::process::Command;

use re_compat::ClassRegistry;
use re_script_check::roblox;

fn registry() -> ClassRegistry {
    ClassRegistry::builtin().unwrap()
}

/// Runs `luau-lsp analyze` over `script` with both definitions files. `None`
/// when luau-lsp is not installed (CI installs it, so there it must be found).
fn analyze(dir: &Path, script: &str) -> Option<(bool, String)> {
    let engine = dir.join("engine.d.luau");
    std::fs::write(&engine, re_script::definitions()).unwrap();
    let defs = dir.join("roblox.d.luau");
    std::fs::write(&defs, roblox::definitions(&registry())).unwrap();
    let file = dir.join("check.luau");
    std::fs::write(&file, script).unwrap();
    let out = match Command::new("luau-lsp")
        .arg("analyze")
        .arg("--platform=standard")
        .arg("--flag:LuauSolverV2=true")
        .arg(format!("--definitions=@engine={}", engine.display()))
        .arg(format!("--definitions=@roblox={}", defs.display()))
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
    assert!(
        !text.contains("Failed to read definitions") && !text.contains("SyntaxError"),
        "the definitions do not load:\n{text}"
    );
    Some((out.status.success(), text))
}

#[test]
fn the_definitions_load_and_type_the_compat_api() {
    let dir = tempfile::tempdir().unwrap();
    let ok = r#"--!strict
local storage = game:GetService("ReplicatedStorage")
local folder = Instance.new("Folder")
folder.Name = "Kit"
folder.Parent = storage
local found = storage:FindFirstChild("Kit")
local isFolder: boolean = found ~= nil and found:IsA("Folder")
local conn = storage.ChildAdded:Connect(function(child)
    print(child)
end)
conn:Disconnect()
local v = Vector3.new(1, 2, 3) * 2 + Vector3.zero
local cf = CFrame.new() * CFrame.Angles(0, math.pi, 0)
local moved: Vector3 = cf * v
local items = Enum.EasingStyle:GetEnumItems()
local style = Enum.EasingStyle.Linear
print(isFolder, moved.Magnitude, #items, style.Name, workspace.Name)
"#;
    let Some((success, text)) = analyze(dir.path(), ok) else {
        return;
    };
    assert!(success, "{text}");

    let wrong =
        "--!strict\nlocal n: number = game:GetService(\"ReplicatedStorage\").Name\nprint(n)\n";
    let (success, text) = analyze(dir.path(), wrong).unwrap();
    assert!(!success && text.contains("TypeError"), "{text}");
}

#[test]
fn write_definitions_puts_them_in_the_project_once() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let path = roblox::write_definitions(dir.path(), &reg).unwrap();
    assert_eq!(path, dir.path().join(roblox::DEFINITIONS_PATH));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        roblox::definitions(&reg)
    );
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    roblox::write_definitions(dir.path(), &reg).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before
    );
}
