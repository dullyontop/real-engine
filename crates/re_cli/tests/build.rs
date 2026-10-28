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

#[test]
fn an_audio_file_the_engine_cannot_play_stops_the_build() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    std::fs::create_dir_all(&project).unwrap();
    copy_project(&example("island"), &project);
    // `re_audio/tests/ogg.rs` says how these files were made.
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../re_audio/tests/data");
    let sfx = project.join("assets/sfx");
    std::fs::create_dir_all(&sfx).unwrap();
    // Damaged past its headers: only decoding it finds that.
    let mut broken = std::fs::read(data.join("sparse180.ogg")).unwrap();
    for b in &mut broken[25_000..25_100] {
        *b ^= 0x5a;
    }
    std::fs::write(sfx.join("broken.ogg"), broken).unwrap();
    std::fs::copy(data.join("three.ogg"), sfx.join("choir.ogg")).unwrap();
    std::fs::copy(data.join("stereo1.ogg"), sfx.join("door.ogg")).unwrap();
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
    assert!(
        t.contains("RE7402") && t.contains("assets/sfx/broken.ogg"),
        "{t}"
    );
    assert!(
        t.contains("RE7070") && t.contains("assets/sfx/choir.ogg"),
        "{t}"
    );
    assert!(!t.contains("door.ogg") && t.contains("RE0951"), "{t}");
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

/// A copy of the island whose `[package]` section is `package`.
fn island_with_package(dir: &Path, package: &str) -> PathBuf {
    let project = dir.join("island");
    std::fs::create_dir_all(&project).unwrap();
    copy_project(&example("island"), &project);
    let toml = project.join("real.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    let base = text.split("\n[package]").next().unwrap();
    std::fs::write(&toml, format!("{base}\n[package]\n{package}\n")).unwrap();
    project
}

/// Writes a square PNG icon of `width` × `height` pixels.
fn write_icon(path: &Path, width: u32, height: u32) {
    let icon = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 160, 255])
    });
    icon.save(path).unwrap();
}

/// A runtime folder for `target`, as `real build --runtime-only` writes one,
/// whose binary this machine cannot run: `real build` reads its
/// `runtime.json` instead.
fn foreign_runtime(dir: &Path, target: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let exe = dir.join(if target.contains("windows") {
        "real.exe"
    } else {
        "real"
    });
    std::fs::write(&exe, "not a program for this machine").unwrap();
    let mut info = serde_json::to_value(package::RuntimeVersions::current()).unwrap();
    info["target"] = target.into();
    std::fs::write(dir.join("runtime.json"), info.to_string()).unwrap();
    std::fs::write(
        dir.join("THIRD_PARTY_NOTICES.txt"),
        "THIRD-PARTY NOTICES\n\nthe runtime's crates\n",
    )
    .unwrap();
    exe
}

fn build_game(project: &Path, out: &Path, extra: &[&str]) -> (Output, serde_json::Value) {
    let mut args = vec![
        "build",
        "--project",
        project.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--json",
    ];
    args.extend(extra);
    let o = real(&args);
    let report = serde_json::from_slice(&o.stdout).unwrap_or_default();
    (o, report)
}

fn listed(report: &serde_json::Value, key: &str) -> Vec<String> {
    report[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A macOS app laid out on another OS from a runtime folder: the target comes
/// from its `runtime.json`, the archive, icon and notices go in the app's
/// resources, and the build says the app is not sealed. (On a Mac the build
/// seals the app, which needs a real binary: CI's macOS job checks that.)
#[cfg(not(target_os = "macos"))]
#[test]
fn a_mac_app_is_laid_out_from_a_runtime_folder() {
    let dir = tempfile::tempdir().unwrap();
    let project = island_with_package(
        dir.path(),
        "identifier = \"com.example.island\"\ndisplay_name = \"Island Demo\"\nversion = \"1.2.0-beta\"\nicon = \"icon.png\"\nnotices = [\"LICENSES/*.txt\"]",
    );
    write_icon(&project.join("icon.png"), 512, 512);
    std::fs::create_dir_all(project.join("LICENSES")).unwrap();
    std::fs::write(project.join("LICENSES/music.txt"), "music by someone\n").unwrap();
    let runtime = foreign_runtime(&dir.path().join("rt"), "aarch64-apple-darwin");
    let out = dir.path().join("out");
    let (o, report) = build_game(&project, &out, &["--runtime", runtime.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(report["target"], "aarch64-apple-darwin");
    assert_eq!(listed(&report, "files"), ["Island Demo.app"]);
    assert!(
        listed(&report, "notes")
            .iter()
            .any(|n| n.contains("not sealed")),
        "{report}"
    );
    assert_eq!(
        listed(&report["notices"], "game_files"),
        ["LICENSES/music.txt"]
    );

    let contents = out.join("Island Demo.app/Contents");
    let plist = std::fs::read_to_string(contents.join("Info.plist")).unwrap();
    for line in [
        "<key>CFBundleExecutable</key>\n\t<string>island</string>",
        "<key>CFBundleIdentifier</key>\n\t<string>com.example.island</string>",
        "<key>CFBundleShortVersionString</key>\n\t<string>1.2.0</string>",
        "<key>CFBundleIconFile</key>\n\t<string>island.icns</string>",
        "<key>LSMinimumSystemVersion</key>\n\t<string>11.0</string>",
    ] {
        assert!(plist.contains(line), "{line}\n{plist}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(contents.join("MacOS/island"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
    let resources = contents.join("Resources");
    let archive = Archive::open(&resources.join(ARCHIVE_NAME)).unwrap();
    let icon = archive
        .index()
        .entries
        .iter()
        .find(|e| e.path == "icon.png")
        .expect("the icon is packed");
    assert_eq!(icon.form, StoredForm::Source);
    let icns = std::fs::read(resources.join("island.icns")).unwrap();
    assert_eq!(&icns[..4], b"icns");
    let has = |kind: &[u8]| icns.windows(4).any(|w| w == kind);
    assert!(has(b"ic09") && has(b"icp4") && !has(b"ic10"));
    let notices = std::fs::read_to_string(resources.join("THIRD_PARTY_NOTICES.txt")).unwrap();
    assert!(
        notices.starts_with("THIRD-PARTY NOTICES\n\nthe runtime's crates\n"),
        "{notices}"
    );
    assert!(notices.contains("THE GAME'S OWN NOTICES"), "{notices}");
    assert!(notices.contains("LICENSES/music.txt\n"), "{notices}");
    assert!(notices.contains("music by someone"), "{notices}");

    // A second build replaces the app, and an earlier game's notices give
    // only their runtime's part to the next build.
    let (o, _) = build_game(&project, &out, &["--runtime", runtime.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let again = std::fs::read_to_string(resources.join("THIRD_PARTY_NOTICES.txt")).unwrap();
    assert_eq!(again, notices);
}

/// A Windows folder laid out from a runtime folder: the executable, the
/// archive and the notices, and nothing else.
#[test]
fn a_windows_folder_is_laid_out_from_a_runtime_folder() {
    let dir = tempfile::tempdir().unwrap();
    let project = island_with_package(dir.path(), "identifier = \"com.example.island\"");
    let target = "x86_64-pc-windows-msvc";
    let runtime = foreign_runtime(&dir.path().join("rt"), target);
    let out = dir.path().join("out");
    let (o, report) = build_game(
        &project,
        &out,
        &["--runtime", runtime.to_str().unwrap(), "--target", target],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        listed(&report, "files"),
        [ARCHIVE_NAME, "island.exe", "THIRD_PARTY_NOTICES.txt"]
    );
    let mut written: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    written.sort();
    assert_eq!(
        written,
        [
            ".real-build",
            "THIRD_PARTY_NOTICES.txt",
            ARCHIVE_NAME,
            "island.exe"
        ]
    );
    // The runtime is for Windows, so a build for Linux refuses it.
    let (o, _) = build_game(
        &project,
        &dir.path().join("linux"),
        &[
            "--runtime",
            runtime.to_str().unwrap(),
            "--target",
            "x86_64-unknown-linux-gnu",
        ],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("RE0953"), "{}", text(&o));
}

/// A runtime whose folder has no notices is refused before `--out` is
/// touched: a binary's crate list cannot be read back from it.
#[test]
fn a_runtime_without_notices_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let project = island_with_package(dir.path(), "identifier = \"com.example.island\"");
    let target = "x86_64-pc-windows-msvc";
    let runtime = foreign_runtime(&dir.path().join("rt"), target);
    std::fs::remove_file(dir.path().join("rt/THIRD_PARTY_NOTICES.txt")).unwrap();
    let out = dir.path().join("out");
    let (o, _) = build_game(
        &project,
        &out,
        &["--runtime", runtime.to_str().unwrap(), "--target", target],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("RE0954"), "{}", text(&o));
    assert!(!out.exists());
}

/// `[package] icon` must be a square PNG of at least 512 px.
#[test]
fn an_icon_must_be_a_large_square_png() {
    let dir = tempfile::tempdir().unwrap();
    let project = island_with_package(
        dir.path(),
        "identifier = \"com.example.island\"\nicon = \"icon.png\"",
    );
    write_icon(&project.join("icon.png"), 600, 400);
    let (o, _) = build_game(&project, &dir.path().join("out"), &["--archive-only"]);
    assert!(!o.status.success());
    let t = text(&o);
    assert!(t.contains("RE0950") && t.contains("600×400"), "{t}");
    write_icon(&project.join("icon.png"), 256, 256);
    let (o, _) = build_game(&project, &dir.path().join("out"), &["--archive-only"]);
    assert!(text(&o).contains("at least 512 px"), "{}", text(&o));
}

/// The Linux folder around a runtime this machine runs: the executable, the
/// archive, the notices, the icon and the desktop entry.
#[cfg(target_os = "linux")]
#[test]
fn a_linux_folder_has_its_icon_and_desktop_entry() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let project = island_with_package(
        dir.path(),
        "identifier = \"com.example.island\"\ndisplay_name = \"Island\"\nicon = \"icon.png\"",
    );
    write_icon(&project.join("icon.png"), 512, 512);
    // A runtime that answers `--runtime-info` as the game binary does.
    let rt = dir.path().join("rt");
    std::fs::create_dir_all(&rt).unwrap();
    let mut info = serde_json::to_value(package::RuntimeVersions::current()).unwrap();
    info["target"] = "x86_64-unknown-linux-gnu".into();
    let exe = rt.join("real");
    std::fs::write(&exe, format!("#!/bin/sh\nprintf '%s' '{info}'\n")).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(rt.join("THIRD_PARTY_NOTICES.txt"), "THIRD-PARTY NOTICES\n").unwrap();
    let out = dir.path().join("out");
    let (o, report) = build_game(
        &project,
        &out,
        &[
            "--runtime",
            exe.to_str().unwrap(),
            "--target",
            "x86_64-unknown-linux-gnu",
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        listed(&report, "files"),
        [
            ARCHIVE_NAME,
            "island",
            "THIRD_PARTY_NOTICES.txt",
            "island.png",
            "island.desktop"
        ]
    );
    let desktop = std::fs::read_to_string(out.join("island.desktop")).unwrap();
    assert!(desktop.contains("Name=Island\n") && desktop.contains("Icon=island\n"));
    assert_eq!(
        std::fs::read(out.join("island.png")).unwrap(),
        std::fs::read(project.join("icon.png")).unwrap()
    );
}
