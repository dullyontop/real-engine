//! `real build --archive-only` (ADR-0037): the archive is reproducible, and the
//! game in it plays as the project does.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use re_app::package::{self, ARCHIVE_NAME, Archive, StoredForm};
use re_app::{App, AppConfig};

fn real(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_real"))
        .args(args)
        .output()
        .expect("the real binary runs")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
}

/// Copies a project (without `.real/`) so a build may create sidecars in it.
fn copy_project(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == ".real" {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_project(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn build_archive(project: &Path, out: &Path) -> serde_json::Value {
    let o = real(&[
        "build",
        "--project",
        project.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--archive-only",
        "--json",
    ]);
    assert!(o.status.success(), "{}", text(&o));
    serde_json::from_slice(&o.stdout).unwrap()
}

/// Runs each of the project's replays packaged and in the project, headless,
/// and compares the state hashes.
fn packaged_matches_project(name: &str) {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join(name);
    std::fs::create_dir_all(&project).unwrap();
    copy_project(&example(name), &project);
    let out = dir.path().join("out");
    let report = build_archive(&project, &out);
    assert_eq!(report["ok"], true);

    let archive = Archive::open(&out.join(ARCHIVE_NAME)).unwrap();
    let paths: Vec<&str> = archive
        .index()
        .entries
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    assert!(paths.contains(&"real.toml"), "{paths:?}");
    assert!(!paths.iter().any(|p| p.starts_with("tests/")), "{paths:?}");
    for e in &archive.index().entries {
        if e.path.ends_with(".luau") {
            assert!(matches!(e.form, StoredForm::Bytecode { .. }), "{}", e.path);
        }
    }

    let config = AppConfig {
        headless: true,
        ..AppConfig::load(&project).unwrap()
    };
    let data = dir.path().join("data");
    for file in re_app::verify::content_files(&project, "tests/replays", ".replay.json") {
        let doc = re_input::load_replay(&project.join(&file)).unwrap();
        let mut dev = App::open(&project, config.clone()).unwrap();
        let expected = dev.run_headless(None, Some(doc.clone())).unwrap();

        let opened = package::open_package(&out.join(ARCHIVE_NAME), Some(&data)).unwrap();
        let packaged_config = AppConfig {
            headless: true,
            ..AppConfig::load_from(&opened.files).unwrap()
        };
        let mut game = App::open_files(opened.files.clone(), packaged_config).unwrap();
        let got = game.run_headless(None, Some(doc)).unwrap();
        let errors: Vec<_> = got.diagnostics.iter().filter(|d| d.is_error()).collect();
        assert!(errors.is_empty(), "{file}: {errors:?}");
        assert_eq!(got.state_hash, expected.state_hash, "{file}");
        assert!(opened.archive.take_case_matches().is_empty(), "{file}");
    }
    // Nothing was written next to the archive.
    let mut listed: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    listed.sort();
    assert_eq!(listed, [".real-build", ARCHIVE_NAME]);
}

#[test]
fn the_island_plays_the_same_packaged() {
    packaged_matches_project("island");
}

#[test]
fn the_plaza_plays_the_same_packaged() {
    packaged_matches_project("plaza");
}

#[test]
fn the_archive_is_reproducible() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    std::fs::create_dir_all(&project).unwrap();
    copy_project(&example("island"), &project);
    build_archive(&project, &dir.path().join("a"));
    build_archive(&project, &dir.path().join("b"));
    let a = std::fs::read(dir.path().join("a").join(ARCHIVE_NAME)).unwrap();
    let b = std::fs::read(dir.path().join("b").join(ARCHIVE_NAME)).unwrap();
    assert!(a == b, "two builds of the same project differ");
    // A second build into the same folder replaces the first.
    build_archive(&project, &dir.path().join("a"));
}

#[test]
fn a_script_that_does_not_compile_stops_the_build() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    std::fs::create_dir_all(&project).unwrap();
    copy_project(&example("island"), &project);
    std::fs::write(project.join("scripts/broken.luau"), "return = 1\n").unwrap();
    let out = dir.path().join("out");
    let o = real(&[
        "build",
        "--project",
        project.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--archive-only",
    ]);
    assert!(!o.status.success());
    let t = text(&o);
    assert!(t.contains("RE3001") && t.contains("RE0951"), "{t}");
    assert!(!out.join(ARCHIVE_NAME).exists());
}

/// A scene that names a project file the archive would leave out stops the
/// build, and including the file fixes it.
#[test]
fn a_scene_naming_an_unpacked_file_stops_the_build() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    std::fs::create_dir_all(&project).unwrap();
    copy_project(&example("island"), &project);
    std::fs::create_dir_all(project.join("art")).unwrap();
    std::fs::copy(
        project.join("materials/hero.material.json"),
        project.join("art/hero.material.json"),
    )
    .unwrap();
    let scene = project.join("scenes/main.scene.json");
    let text_before = std::fs::read_to_string(&scene).unwrap();
    assert!(text_before.contains("\"materials/hero.material.json\""));
    std::fs::write(
        &scene,
        text_before.replace(
            "\"materials/hero.material.json\"",
            "\"art/hero.material.json\"",
        ),
    )
    .unwrap();
    let out = dir.path().join("out");
    let build = || {
        real(&[
            "build",
            "--project",
            project.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--archive-only",
        ])
    };
    let o = build();
    assert!(!o.status.success());
    let t = text(&o);
    assert!(
        t.contains("RE0957") && t.contains("art/hero.material.json"),
        "{t}"
    );
    assert!(t.contains("[package] include"), "{t}");

    let toml = project.join("real.toml");
    let mut config = std::fs::read_to_string(&toml).unwrap();
    if config.contains("[package]") {
        config = config.replace("[package]", "[package]\ninclude = [\"art/**\"]");
    } else {
        config.push_str("\n[package]\ninclude = [\"art/**\"]\n");
    }
    std::fs::write(&toml, config).unwrap();
    let o = build();
    assert!(o.status.success(), "{}", text(&o));
}

/// Animation graphs in `anims/` are packed by default, so a scene's
/// `Animator` finds its graph in the packaged game.
#[test]
fn animation_graphs_are_packed_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    std::fs::create_dir_all(&project).unwrap();
    copy_project(&example("island"), &project);
    std::fs::create_dir_all(project.join("anims")).unwrap();
    std::fs::write(
        project.join("anims/hero.animgraph.json"),
        r#"{ "format": "real-engine/animgraph", "version": 1 }"#,
    )
    .unwrap();
    let scene = project.join("scenes/main.scene.json");
    let before = std::fs::read_to_string(&scene).unwrap();
    assert!(before.contains("\"entities\": ["));
    std::fs::write(
        &scene,
        before.replacen(
            "\"entities\": [",
            r#""entities": [ { "id": "animated", "components": {
              "Animator": { "graph": "anims/hero.animgraph.json" } } },"#,
            1,
        ),
    )
    .unwrap();
    let report = build_archive(&project, &dir.path().join("out"));
    let packed: Vec<&str> = report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert!(packed.contains(&"anims/hero.animgraph.json"), "{packed:?}");
}

#[test]
fn out_must_not_hold_other_files() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("keep.txt"), "mine").unwrap();
    let o = real(&[
        "build",
        "--project",
        example("island").to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--archive-only",
    ]);
    assert!(!o.status.success());
    assert!(text(&o).contains("RE0955"), "{}", text(&o));
    assert_eq!(
        std::fs::read_to_string(out.join("keep.txt")).unwrap(),
        "mine"
    );
}
