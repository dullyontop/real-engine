//! The importer end to end: both input forms into new and existing projects,
//! re-import idempotence, stale removal, safety (nothing runs, an error
//! writes nothing) and the report.

mod common;

use std::path::Path;

use common::{fixture, registry, tree_text};
use re_compat::scene::validate;
use re_import::{ImportOptions, ImportReport, InputFormat, codes, import, new_project};
use re_reflect::Value;
use re_scene::World;

fn opts() -> ImportOptions {
    ImportOptions::default()
}

fn paths(items: &[re_import::Item]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|i| (re_import::display_path(&i.path), i.class.clone()))
        .collect()
}

/// Loads the project's scene into a world with the class layer.
fn load(project: &Path) -> World {
    let r = registry();
    let mut w = World::new();
    r.register_components(&mut w).unwrap();
    let text = std::fs::read_to_string(project.join("scenes/main.scene.json")).unwrap();
    w.load(&re_scene::scene::from_json(&text).unwrap()).unwrap();
    let problems = validate(&mut w, &r);
    assert!(problems.is_empty(), "{problems:#?}");
    w
}

fn script_text(project: &Path, report: &ImportReport, path: &str) -> String {
    let w = load(project);
    let item = report
        .created
        .iter()
        .chain(&report.adopted)
        .find(|i| re_import::display_path(&i.path) == path)
        .unwrap_or_else(|| panic!("{path} not imported"));
    let e = w.entity(&item.id).unwrap();
    let sr = w.ecs.get::<re_scene::ScriptRef>(e).unwrap();
    std::fs::read_to_string(project.join(&sr.path)).unwrap()
}

#[test]
fn loader_builds_the_tree_without_running_anything() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("garden");
    let r = registry();
    let report = import(&fixture("loader_basic.luau"), &project, &r, &opts()).unwrap();
    assert!(report.ok(), "{}", report.render_human());
    assert_eq!(report.format, InputFormat::Loader);
    assert!(report.new_project);
    assert_eq!(
        paths(&report.created),
        [
            ("ReplicatedStorage.Garden", "Folder"),
            ("ReplicatedStorage.Garden.Plants", "Folder"),
            ("ReplicatedStorage.Garden.Plants.Rose", "ModuleScript"),
            ("ReplicatedStorage.Garden.Plants.Rose", "ModuleScript"),
            ("ReplicatedStorage.Garden.\"Seeds.v2\"", "ModuleScript"),
            ("ServerScriptService.Gardener", "Script"),
            ("StarterPlayer.StarterPlayerScripts.Watering", "LocalScript"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    // The built-in StarterPlayerScripts is found, not made as a Folder.
    assert_eq!(
        paths(&report.adopted),
        [(
            "StarterPlayer.StarterPlayerScripts".to_string(),
            "StarterPlayerScripts".to_string()
        )]
    );
    // The dotted name is one element.
    assert!(
        report
            .created
            .iter()
            .any(|i| i.path == ["ReplicatedStorage", "Garden", "Seeds.v2"])
    );
    assert_eq!(report.classes.get("ModuleScript"), Some(&3));
    assert_eq!(report.classes.get("Folder"), Some(&2));
    // The prints, the `if` and the final `error(...)` are reported, not run.
    let skipped: Vec<&str> = report
        .unrecognised
        .iter()
        .map(|u| u.statement.as_str())
        .collect();
    assert_eq!(skipped.len(), 3, "{skipped:?}");
    assert!(skipped[0].starts_with("print(\"loading"));
    assert!(skipped[1].starts_with("if done then"));
    assert!(skipped[2].starts_with("error("));
    assert_eq!(report.unrecognised[0].line, 5);

    // Sources: escapes decoded, long brackets without their first newline.
    assert_eq!(
        script_text(&project, &report, "ServerScriptService.Gardener"),
        "print(\"tending\\tthe garden\")\n"
    );
    assert_eq!(
        script_text(&project, &report, "ReplicatedStorage.Garden.\"Seeds.v2\""),
        "local s = \"]]\"\nreturn s\n"
    );
    // Duplicate names are two instances with two files.
    let files: Vec<String> = tree_text(&project).into_iter().map(|(p, _)| p).collect();
    assert!(
        files.contains(&"scripts/ReplicatedStorage/Garden/Plants/Rose.luau".to_string()),
        "{files:?}"
    );
    assert!(files.contains(&"scripts/ReplicatedStorage/Garden/Plants/Rose~2.luau".to_string()));
    assert!(files.contains(&"scripts/ReplicatedStorage/Garden/Seeds.v2.luau".to_string()));
    assert!(files.contains(&"scripts/ServerScriptService/Gardener.server.luau".to_string()));
    assert!(
        files.contains(
            &"scripts/StarterPlayer/StarterPlayerScripts/Watering.client.luau".to_string()
        )
    );

    // The project is a compat project.
    let toml = std::fs::read_to_string(project.join("real.toml")).unwrap();
    assert!(
        re_compat::CompatConfig::from_project_toml(&toml)
            .unwrap()
            .is_some()
    );
    let w = load(&project);
    let sr = w
        .ecs
        .get::<re_scene::ScriptRef>(w.entity(&report.created[5].id).unwrap())
        .unwrap();
    assert_eq!(sr.context, re_scene::ScriptContext::Server);
}

#[test]
fn reimport_is_idempotent_and_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry();
    for input in ["loader_basic.luau", "loader_table.luau", "rojo_basic"] {
        let a = dir.path().join(format!("{input}-a"));
        let b = dir.path().join(format!("{input}-b"));
        let first = import(&fixture(input), &a, &r, &opts()).unwrap();
        import(&fixture(input), &b, &r, &opts()).unwrap();
        let before = tree_text(&a);
        // Two new projects get the same files, ids included (the project
        // name differs only in real.toml).
        let strip = |t: Vec<(String, String)>| -> Vec<(String, String)> {
            t.into_iter().filter(|(p, _)| p != "real.toml").collect()
        };
        assert_eq!(strip(before.clone()), strip(tree_text(&b)), "{input}");

        let again = import(&fixture(input), &a, &r, &opts()).unwrap();
        assert!(again.created.is_empty(), "{input}: {:?}", again.created);
        assert!(again.removed.is_empty(), "{input}");
        assert_eq!(
            again.adopted.len(),
            first.created.len() + first.adopted.len(),
            "{input}"
        );
        assert!(again.adopted.iter().all(|i| !i.updated), "{input}");
        assert!(
            again.files.written.is_empty(),
            "{input}: {:?}",
            again.files.written
        );
        assert_eq!(tree_text(&a), before, "{input}");
    }
}

#[test]
fn script_table_applies_like_the_loader_and_removes_only_its_own_stale_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("kit");
    let r = registry();
    let report = import(&fixture("loader_table.luau"), &project, &r, &opts()).unwrap();
    assert!(report.ok(), "{}", report.render_human());
    assert_eq!(report.classes.values().sum::<u32>(), 6);
    assert_eq!(report.classes.get("LocalScript"), Some(&2));
    // The loop that applies the table in Studio is skipped.
    assert_eq!(report.unrecognised.len(), 1);
    assert!(
        report.unrecognised[0]
            .statement
            .starts_with("for _, entry in ENTRIES do")
    );

    let w = load(&project);
    let class_of = |p: &str| -> String {
        let i = report
            .created
            .iter()
            .chain(&report.adopted)
            .find(|i| re_import::display_path(&i.path) == p)
            .unwrap_or_else(|| panic!("{p}"));
        w.ecs
            .get::<re_compat::ClassMarker>(w.entity(&i.id).unwrap())
            .unwrap()
            .class
            .clone()
    };
    assert_eq!(class_of("ReplicatedStorage.Kit"), "Folder");
    assert_eq!(class_of("ReplicatedStorage.Kit.Util"), "Folder");
    // A character container is a model, not a folder.
    assert_eq!(class_of("StarterPlayer.StarterCharacter"), "Model");
    assert_eq!(
        class_of("StarterPlayer.StarterPlayerScripts"),
        "StarterPlayerScripts"
    );
    assert_eq!(
        class_of("ServerStorage.Old.\"ReplicatedStorage.Kit.Util\""),
        "ModuleScript"
    );
    // What the table creates carries the loader's tag; what existed does not.
    let tags = |p: &str| -> Vec<String> {
        let i = report
            .created
            .iter()
            .chain(&report.adopted)
            .find(|i| re_import::display_path(&i.path) == p)
            .unwrap();
        w.ecs
            .get::<re_scene::Tags>(w.entity(&i.id).unwrap())
            .map(|t| t.tags.clone())
            .unwrap_or_default()
    };
    assert_eq!(tags("ReplicatedStorage.Kit"), ["KitManaged"]);
    assert_eq!(tags("ReplicatedStorage.Kit.Util.Maths"), ["KitManaged"]);
    assert!(tags("StarterPlayer.StarterPlayerScripts").is_empty());

    // Someone adds their own script next to the imported ones.
    let mut scene: re_scene::SceneDoc = re_scene::scene::from_json(
        &std::fs::read_to_string(project.join("scenes/main.scene.json")).unwrap(),
    )
    .unwrap();
    let util = report
        .created
        .iter()
        .find(|i| re_import::display_path(&i.path) == "ReplicatedStorage.Kit.Util")
        .unwrap()
        .id
        .clone();
    let mut own =
        re_compat::scene::instance_doc(&r, "ModuleScript", "mine", "Mine", Some(&util)).unwrap();
    let mut sr = indexmap::IndexMap::new();
    sr.insert(
        "path".to_string(),
        re_reflect::Doc::Str("scripts/mine.luau".into()),
    );
    sr.insert("context".to_string(), re_reflect::Doc::Str("Module".into()));
    own.components.insert("ScriptRef".into(), sr);
    scene.entities.push(own);
    std::fs::write(
        project.join("scenes/main.scene.json"),
        re_scene::scene::to_json(&scene),
    )
    .unwrap();
    std::fs::write(project.join("scripts/mine.luau"), "return 1\n").unwrap();

    // A version of the loader without the Text entry.
    let text = std::fs::read_to_string(fixture("loader_table.luau")).unwrap();
    let start = text
        .find("\t{ { \"ReplicatedStorage\", \"Kit\", \"Util\", \"Text\" }")
        .unwrap();
    let end = start + text[start..].find("]=] },\n").unwrap() + "]=] },\n".len();
    let dropped = format!("{}{}", &text[..start], &text[end..]);
    let variant = dir.path().join("variant.luau");
    std::fs::write(&variant, &dropped).unwrap();

    // Kept on request.
    let kept = import(
        &variant,
        &project,
        &r,
        &ImportOptions {
            keep_stale: true,
            ..opts()
        },
    )
    .unwrap();
    assert!(kept.removed.is_empty());

    let again = import(&variant, &project, &r, &opts()).unwrap();
    assert!(again.created.is_empty());
    assert_eq!(
        paths(&again.removed),
        [(
            "ReplicatedStorage.Kit.Util.Text".to_string(),
            "ModuleScript".to_string()
        )]
    );
    assert_eq!(
        again.files.deleted,
        ["scripts/ReplicatedStorage/Kit/Util/Text.luau"]
    );
    assert!(
        !project
            .join("scripts/ReplicatedStorage/Kit/Util/Text.luau")
            .exists()
    );
    // The script nobody imported stays, with its file.
    let w = load(&project);
    assert!(w.entity("mine").is_ok());
    assert!(project.join("scripts/mine.luau").exists());

    // `local REMOVE_OLD = false` keeps stale scripts too.
    let original = import(&fixture("loader_table.luau"), &project, &r, &opts()).unwrap();
    assert_eq!(original.created.len(), 1);
    let no_remove = dir.path().join("no_remove.luau");
    std::fs::write(
        &no_remove,
        dropped.replace("local REMOVE_OLD = true", "local REMOVE_OLD = false"),
    )
    .unwrap();
    assert!(
        import(&no_remove, &project, &r, &opts())
            .unwrap()
            .removed
            .is_empty()
    );
}

#[test]
fn replaced_roots_drop_what_the_input_no_longer_declares() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("garden");
    let r = registry();
    import(&fixture("loader_basic.luau"), &project, &r, &opts()).unwrap();
    let text = std::fs::read_to_string(fixture("loader_basic.luau")).unwrap();
    let start = text.find("\t\tdo\n\t\t\tlocal plants").unwrap();
    let end = text.find("\t\tlocal odd").unwrap();
    let variant = dir.path().join("smaller.luau");
    std::fs::write(&variant, format!("{}{}", &text[..start], &text[end..])).unwrap();
    let report = import(&variant, &project, &r, &opts()).unwrap();
    assert!(report.ok(), "{}", report.render_human());
    let mut removed = paths(&report.removed);
    removed.sort();
    assert_eq!(
        removed,
        [
            ("ReplicatedStorage.Garden.Plants", "Folder"),
            ("ReplicatedStorage.Garden.Plants.Rose", "ModuleScript"),
            ("ReplicatedStorage.Garden.Plants.Rose", "ModuleScript"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    assert_eq!(report.files.deleted.len(), 2);
    load(&project);
}

#[test]
fn a_statement_inside_a_building_block_stops_the_import() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("p");
    let err = import(
        &fixture("loader_bad_block.luau"),
        &project,
        &registry(),
        &opts(),
    )
    .unwrap_err();
    assert_eq!(err.code, codes::UNRECOGNISED_STATEMENT);
    assert!(err.is_error());
    assert!(
        err.message.contains("f:SetAttribute(\"Count\", 3)"),
        "{}",
        err.message
    );
    match err.location {
        Some(re_core::Location::File { line, .. }) => assert_eq!(line, Some(12)),
        other => panic!("{other:?}"),
    }
    assert!(!project.exists(), "nothing is written");
}

#[test]
fn a_loader_that_does_not_parse_names_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let err = import(
        &fixture("loader_syntax.luau"),
        &dir.path().join("p"),
        &registry(),
        &opts(),
    )
    .unwrap_err();
    assert_eq!(err.code, codes::LOADER_SYNTAX);
    match err.location {
        Some(re_core::Location::File { line, .. }) => assert_eq!(line, Some(5)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_dry_run_writes_nothing_and_reports_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("p");
    let r = registry();
    let dry = import(
        &fixture("loader_basic.luau"),
        &project,
        &r,
        &ImportOptions {
            dry_run: true,
            ..opts()
        },
    )
    .unwrap();
    assert!(dry.dry_run);
    assert!(!project.exists());
    let real = import(&fixture("loader_basic.luau"), &project, &r, &opts()).unwrap();
    assert_eq!(dry.created, real.created);
    assert_eq!(dry.files, real.files);
}

#[test]
fn unknown_classes_and_services_fail_alone() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("loader.luau");
    std::fs::write(
        &input,
        r#"local a = Instance.new("Folder")
a.Name = "Box"
a.Parent = game:GetService("ReplicatedStorage")
local b = Instance.new("NoSuchClass")
b.Name = "Ghost"
b.Parent = a
local inner = Instance.new("ModuleScript")
inner.Name = "Inside"
inner.Parent = b
local c = Instance.new("Folder")
c.Parent = game:GetService("NoSuchService")
local d = Instance.new("Folder")
d.Name = "NotAScript"
d.Source = "return 1"
d.Parent = a
"#,
    )
    .unwrap();
    let report = import(&input, &dir.path().join("p"), &registry(), &opts()).unwrap();
    assert!(!report.ok());
    let failed: Vec<(String, String)> = report
        .failed
        .iter()
        .map(|f| (re_import::display_path(&f.path), f.message.clone()))
        .collect();
    assert_eq!(failed.len(), 4, "{failed:#?}");
    assert!(failed[0].1.contains("unknown class \"NoSuchClass\""));
    assert_eq!(failed[1].0, "ReplicatedStorage.Box.Ghost.Inside");
    assert!(
        failed
            .iter()
            .any(|(p, m)| p == "NoSuchService.Folder" && m.contains("unknown service"))
    );
    assert!(
        failed
            .iter()
            .any(|(p, m)| p == "ReplicatedStorage.Box.NotAScript" && m.contains("not a script"))
    );
    assert_eq!(paths(&report.created).len(), 2);
}

#[test]
fn scripts_that_do_not_compile_are_imported_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("loader.luau");
    std::fs::write(
        &input,
        "local m = Instance.new(\"ModuleScript\")\nm.Name = \"Broken\"\nm.Source = \"local x = \\nif then\\n\"\nm.Parent = game:GetService(\"ReplicatedStorage\")\n",
    )
    .unwrap();
    let report = import(&input, &dir.path().join("p"), &registry(), &opts()).unwrap();
    assert!(report.ok(), "warnings do not fail an import");
    assert_eq!(report.created.len(), 1);
    let d = &report.diagnostics[0];
    assert_eq!(d.code, codes::SCRIPT_SYNTAX);
    match &d.location {
        Some(re_core::Location::File { file, line, .. }) => {
            assert_eq!(file, "scripts/ReplicatedStorage/Broken.luau");
            assert_eq!(*line, Some(2));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn rojo_projects_follow_the_naming_rules() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("p");
    let r = registry();
    let report = import(&fixture("rojo_basic"), &project, &r, &opts()).unwrap();
    assert_eq!(report.format, InputFormat::Rojo);
    let mut got = paths(&report.created);
    got.sort();
    assert_eq!(
        got,
        [
            ("ReplicatedStorage.Shared", "ModuleScript"),
            ("ReplicatedStorage.Shared.Tools", "Folder"),
            ("ReplicatedStorage.Shared.Tools.\"Saw.v1\"", "ModuleScript"),
            ("ReplicatedStorage.Shared.Tools.Hammer", "ModuleScript"),
            ("ServerScriptService.Jobs", "Script"),
            ("ServerScriptService.Jobs.Schedule", "ModuleScript"),
            ("ServerScriptService.Lights", "Folder"),
            ("ServerScriptService.Lights.Desk", "TestLamp"),
            ("ServerScriptService.Lights.Hall", "TestLamp"),
            ("ServerScriptService.Main", "Script"),
            ("StarterPlayer.StarterPlayerScripts.Main", "LocalScript"),
            ("Workspace.Lamp", "TestLamp"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    // The picture is skipped with a warning; a bad property value fails alone.
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == codes::ROJO_INVALID && d.message.contains("notes.png"))
    );
    assert_eq!(report.failed.len(), 1, "{:#?}", report.failed);
    assert!(report.failed[0].message.starts_with("property Brightness"));

    let w = load(&project);
    let id = |p: &str| {
        report
            .created
            .iter()
            .find(|i| re_import::display_path(&i.path) == p)
            .unwrap()
            .id
            .clone()
    };
    let lamp = w.entity(&id("Workspace.Lamp")).unwrap();
    assert_eq!(
        r.get_property(&w, lamp, "Brightness").unwrap(),
        Value::Float(2.5)
    );
    assert_eq!(
        r.get_property(&w, lamp, "Label").unwrap(),
        Value::Str("porch".into())
    );
    let attrs = w.ecs.get::<re_scene::Attributes>(lamp).unwrap();
    assert_eq!(attrs.get("Kind"), Some(&Value::Str("light".into())));
    assert_eq!(attrs.get("Watts"), Some(&Value::Float(40.0)));
    let desk = w.entity(&id("ServerScriptService.Lights.Desk")).unwrap();
    assert_eq!(
        r.get_property(&w, desk, "Enabled").unwrap(),
        Value::Bool(false)
    );
    let jobs = w.entity(&id("ServerScriptService.Jobs")).unwrap();
    assert_eq!(
        w.ecs
            .get::<re_scene::Attributes>(jobs)
            .unwrap()
            .get("Owner"),
        Some(&Value::Str("ops".into()))
    );
    assert_eq!(
        std::fs::read_to_string(project.join("scripts/ServerScriptService/Jobs.server.luau"))
            .unwrap(),
        "print(\"jobs runner\")\n"
    );

    // A file removed from the source is removed from the project.
    let src = dir.path().join("src-copy");
    copy_dir(&fixture("rojo_basic"), &src);
    std::fs::remove_file(src.join("src/shared/Tools/Hammer.luau")).unwrap();
    let again = import(&src, &project, &r, &opts()).unwrap();
    assert_eq!(
        paths(&again.removed),
        [(
            "ReplicatedStorage.Shared.Tools.Hammer".to_string(),
            "ModuleScript".to_string()
        )]
    );
    assert!(again.created.is_empty());
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

#[test]
fn a_native_project_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("real.toml"),
        "[project]\nname = \"native\"\n",
    )
    .unwrap();
    let err = import(
        &fixture("loader_basic.luau"),
        dir.path(),
        &registry(),
        &opts(),
    )
    .unwrap_err();
    assert_eq!(err.code, codes::PROJECT_UNSUITABLE);
    assert!(err.message.contains("[compat]"));
}

#[test]
fn a_new_project_is_an_empty_place_that_takes_an_import() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("fresh");
    let r = registry();
    let files = new_project(&project, None, &r).unwrap();
    assert_eq!(files, ["real.toml", "scenes/main.scene.json", ".gitignore"]);
    let w = load(&project);
    for s in re_import::PLACE_SERVICES {
        assert!(w.entity(s).is_ok(), "{s}");
    }
    assert!(w.entity("StarterPlayerScripts").is_ok());
    let report = import(&fixture("loader_basic.luau"), &project, &r, &opts()).unwrap();
    assert!(!report.new_project);
    assert!(report.ok());
    load(&project);
}

fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

fn removed_paths(report: &ImportReport) -> Vec<String> {
    let mut out: Vec<String> = report
        .removed
        .iter()
        .map(|i| re_import::display_path(&i.path))
        .collect();
    out.sort();
    out
}

#[test]
fn an_emptied_or_dropped_service_loses_what_an_import_put_there() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in");
    let project = dir.path().join("game");
    let r = registry();
    let tree = |services: &str| {
        format!(r#"{{"name":"t","tree":{{"$className":"DataModel",{services}}}}}"#)
    };
    write_files(
        &input,
        &[
            (
                "default.project.json",
                &tree(
                    r#""ServerScriptService":{"$path":"server"},"ReplicatedStorage":{"$path":"shared"}"#,
                ),
            ),
            ("server/Main.server.luau", "print(1)\n"),
            ("shared/Util.luau", "return 1\n"),
        ],
    );
    import(&input, &project, &r, &opts()).unwrap();

    // The service's directory is now empty.
    std::fs::remove_file(input.join("server/Main.server.luau")).unwrap();
    let report = import(&input, &project, &r, &opts()).unwrap();
    assert_eq!(removed_paths(&report), ["ServerScriptService.Main"]);
    assert_eq!(report.files.deleted.len(), 1);

    // The project no longer names the service at all.
    write_files(
        &input,
        &[("default.project.json", &tree(r#""Workspace":{}"#))],
    );
    let report = import(&input, &project, &r, &opts()).unwrap();
    assert_eq!(removed_paths(&report), ["ReplicatedStorage.Util"]);
    load(&project);
}

#[test]
fn a_loader_that_empties_a_service_removes_its_old_contents() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("game");
    let r = registry();
    let loader = |body: &str| {
        format!(
            "local function wipe(where, label)\n\tfor _, kid in ipairs(where:GetChildren()) do\n\t\tif kid.Name == label then\n\t\t\tkid:Destroy()\n\t\tend\n\tend\nend\n\
             local function new(kind, label, where)\n\tlocal made = Instance.new(kind)\n\tmade.Name = label\n\tmade.Parent = where\n\treturn made\nend\n\
             do\n\twipe(game:GetService(\"ServerScriptService\"), \"Old\")\n{body}end\n"
        )
    };
    let input = dir.path().join("in.luau");
    std::fs::write(
        &input,
        loader("\tlocal s = new(\"Script\", \"Old\", game:GetService(\"ServerScriptService\"))\n\ts.Source = \"print(1)\"\n"),
    )
    .unwrap();
    import(&input, &project, &r, &opts()).unwrap();
    std::fs::write(&input, loader("")).unwrap();
    let report = import(&input, &project, &r, &opts()).unwrap();
    assert_eq!(removed_paths(&report), ["ServerScriptService.Old"]);
}

#[test]
fn names_that_differ_only_in_case_get_their_own_files() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in");
    let project = dir.path().join("game");
    write_files(
        &input,
        &[
            (
                "default.project.json",
                r#"{"name":"t","tree":{"$className":"DataModel","ServerScriptService":{"$path":"server"}}}"#,
            ),
            ("server/Main.server.luau", "print(1)\n"),
            ("server/main.server.luau", "print(2)\n"),
        ],
    );
    // On a case-insensitive file system (macOS and Windows by default) the two
    // inputs are one file, so there is nothing to tell apart.
    if std::fs::read_dir(input.join("server")).unwrap().count() < 2 {
        println!("skipped: this file system folds case");
        return;
    }
    let report = import(&input, &project, &registry(), &opts()).unwrap();
    let mut files: Vec<String> = report
        .files
        .written
        .iter()
        .filter(|p| p.starts_with("scripts/"))
        .map(|p| p.to_lowercase())
        .collect();
    let n = files.len();
    files.sort();
    files.dedup();
    assert_eq!((n, files.len()), (2, 2), "{:?}", report.files.written);
}

#[test]
fn a_helper_that_does_more_than_make_is_not_read_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.luau");
    std::fs::write(
        &input,
        "local function new(kind, label, where)\n\tlocal made = Instance.new(kind)\n\tmade.Name = label .. \"_suffix\"\n\tmade.Parent = where.Parent\n\treturn made\nend\n\
         do\n\tnew(\"Folder\", \"X\", game:GetService(\"ReplicatedStorage\"))\nend\n",
    )
    .unwrap();
    let err = import(&input, &dir.path().join("game"), &registry(), &opts()).unwrap_err();
    assert_eq!(err.code, codes::UNRECOGNISED_STATEMENT);
}

#[test]
fn a_script_table_with_a_bad_entry_is_reported_and_keeps_stale_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("game");
    let r = registry();
    let input = dir.path().join("in.luau");
    let table = |extra: &str| {
        format!(
            "local ENTRIES = {{\n\t{{ {{ \"ServerScriptService\", \"A\" }}, \"Script\", \"print(1)\" }},\n{extra}}}\n"
        )
    };
    std::fs::write(
        &input,
        table("\t{ { \"ServerScriptService\", \"B\" }, \"Script\", \"print(2)\" },\n"),
    )
    .unwrap();
    import(&input, &project, &r, &opts()).unwrap();
    std::fs::write(
        &input,
        table("\t{ { \"ReplicatedStorage\", \"Ping\" }, \"RemoteEvent\", \"\" },\n"),
    )
    .unwrap();
    let report = import(&input, &project, &r, &opts()).unwrap();
    assert_eq!(report.unrecognised.len(), 1, "{}", report.render_human());
    assert!(report.removed.is_empty(), "{}", report.render_human());
}

#[cfg(unix)]
#[test]
fn a_symlink_loop_in_a_rojo_project_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in");
    write_files(
        &input,
        &[(
            "default.project.json",
            r#"{"name":"t","tree":{"$className":"DataModel","ServerScriptService":{"$path":"server"}}}"#,
        )],
    );
    std::fs::create_dir_all(input.join("server")).unwrap();
    std::os::unix::fs::symlink(input.join("server"), input.join("server/again")).unwrap();
    let err = import(&input, &dir.path().join("game"), &registry(), &opts()).unwrap_err();
    assert_eq!(err.code, codes::ROJO_INVALID);
}
