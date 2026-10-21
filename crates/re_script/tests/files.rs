//! Scripts and modules read through `ProjectFiles` (ADR-0037): bytecode loads
//! only when its form says so, and with this engine's compiler id.

mod common;

use common::Rig;
use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, ProjectPath};
use re_script::{COMPILER_ID, RuntimeConfig, compile_bytecode};

fn rig(files: Vec<(&str, Vec<u8>, Form)>) -> Rig {
    let mut layer = MemLayer::new(LayerKind::Memory);
    for (path, bytes, form) in files {
        layer.insert(ProjectPath::new(path).unwrap(), bytes, form);
    }
    Rig::with_config(RuntimeConfig::with_files(ProjectFiles::for_tests(layer)))
}

fn bytecode() -> Form {
    Form::Bytecode {
        compiler: COMPILER_ID.into(),
    }
}

#[test]
fn modules_load_from_memory_as_source_and_bytecode() {
    let util = compile_bytecode("return { n = 41 }", "scripts/util.luau").unwrap();
    let mut rig = rig(vec![
        ("scripts/util.luau", util, bytecode()),
        (
            "scripts/plain.luau",
            b"return { m = 1 }".to_vec(),
            Form::Source,
        ),
    ]);
    let v = rig
        .exec("return require('./util').n + require('./plain').m")
        .unwrap();
    assert_eq!(v, serde_json::json!(42));
}

#[test]
fn bytecode_stored_as_source_is_a_syntax_error() {
    let util = compile_bytecode("return 1", "scripts/util.luau").unwrap();
    let mut rig = rig(vec![("scripts/util.luau", util, Form::Source)]);
    let d = rig.exec_err("return require('./util')");
    assert_eq!(d.code, re_script::codes::SYNTAX);
    assert!(d.message.contains("binary chunk"), "{}", d.message);
}

#[test]
fn bytecode_from_another_compiler_is_refused() {
    let util = compile_bytecode("return 1", "scripts/util.luau").unwrap();
    let mut rig = rig(vec![(
        "scripts/util.luau",
        util,
        Form::Bytecode {
            compiler: "o2d0".into(),
        },
    )]);
    let d = rig.exec_err("return require('./util')");
    assert!(d.message.contains("o2d0"), "{}", d.message);
}

#[cfg(unix)]
#[test]
fn a_module_linked_from_outside_the_project_is_not_loaded() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.luau"), "return 'secret'").unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("scripts")).unwrap();
    std::fs::write(project.path().join("scripts/ok.luau"), "return 'ok'").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.luau"),
        project.path().join("scripts/secret.luau"),
    )
    .unwrap();
    let mut rig = Rig::at(project.path());
    assert_eq!(
        rig.exec("return require('./ok')").unwrap(),
        serde_json::json!("ok")
    );
    let d = rig.exec_err("return require('./secret')");
    assert!(!d.message.contains("'secret'"), "{}", d.message);
}
