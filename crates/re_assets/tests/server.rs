//! The asset server: handles, ids, clips, the processed cache and hot reload.

mod common;

use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use re_assets::{
    AssetChange, AssetServer, AssetWatcher, GLTF_IMPORTER_VERSION, LoadedAsset, ProcessedCache,
    cache_key,
};
use re_core::time::Instant;
use re_types::render::{TextureFormat, TextureRef};

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("real.toml"), "[project]\nname = \"t\"\n").unwrap();
    for d in [
        "assets/models",
        "assets/textures",
        "materials",
        "scripts/game",
    ] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(root.join("assets/models/lid.glb"), common::fixture_glb()).unwrap();
    std::fs::write(root.join("assets/textures/rock.png"), common::png_2x2()).unwrap();
    std::fs::write(
        root.join("materials/rock.material.json"),
        r#"{ "base_color_texture": "assets/textures/rock.png", "roughness": 0.8 }"#,
    )
    .unwrap();
    dir
}

/// Writes through a temporary file and a rename, as editors do.
fn atomic_save(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension("save.tmp");
    std::fs::write(&tmp, bytes).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

#[test]
fn second_import_is_a_cache_hit_and_a_new_importer_version_misses() {
    let dir = project();
    let root = dir.path();

    let mut first = AssetServer::new(root);
    let h = first.load_gltf("assets/models/lid.glb").unwrap();
    assert_eq!((first.cache().hits(), first.cache().misses()), (0, 1));
    let imported = first.gltf(h).unwrap().clone();

    // A fresh server (a new run) reads the processed result from the cache.
    let mut second = AssetServer::new(root);
    let h2 = second.load_gltf("assets/models/lid.glb").unwrap();
    assert_eq!((second.cache().hits(), second.cache().misses()), (1, 0));
    assert_eq!(second.gltf(h2).unwrap(), &imported);

    // The same source under the next importer version has a different key, with
    // no entry: a miss.
    let source = std::fs::read(root.join("assets/models/lid.glb")).unwrap();
    let cache = ProcessedCache::new(root.join(".real/cache/assets"));
    let current = cache_key("gltf", GLTF_IMPORTER_VERSION, b"{}", &[&source]);
    let next = cache_key("gltf", GLTF_IMPORTER_VERSION + 1, b"{}", &[&source]);
    assert!(cache.contains(&current));
    assert!(!cache.contains(&next));
    assert!(cache.get::<Vec<u8>>(&next).is_none());

    // Changing the import settings in the sidecar also misses.
    let meta_path = root.join("assets/models/lid.glb.meta.json");
    let meta = std::fs::read_to_string(&meta_path).unwrap();
    std::fs::write(
        &meta_path,
        meta.replace("\"settings\": {}", "\"settings\": { \"scale\": 2 }"),
    )
    .unwrap();
    let mut third = AssetServer::new(root);
    third.load_gltf("assets/models/lid.glb").unwrap();
    assert_eq!((third.cache().hits(), third.cache().misses()), (0, 1));
}

#[test]
fn cache_keys_are_length_prefixed_and_corrupt_entries_miss() {
    assert_ne!(cache_key("ab", 1, b"c", &[]), cache_key("a", 1, b"bc", &[]));
    assert_ne!(
        cache_key("x", 1, b"", &[b"ab", b"c"]),
        cache_key("x", 1, b"", &[b"a", b"bc"])
    );

    let dir = tempfile::tempdir().unwrap();
    let mut cache = ProcessedCache::new(dir.path());
    let key = cache_key("test", 1, b"", &[b"source"]);
    let (v, outcome) = cache
        .get_or_insert_with(&key, || Ok::<_, ()>(vec![1u32, 2, 3]))
        .unwrap();
    assert_eq!((v, outcome), (vec![1, 2, 3], re_assets::CacheOutcome::Miss));
    let (v, outcome) = cache
        .get_or_insert_with(&key, || -> Result<Vec<u32>, ()> { panic!("must hit") })
        .unwrap();
    assert_eq!((v, outcome), (vec![1, 2, 3], re_assets::CacheOutcome::Hit));

    // Overwrite the entry with garbage: it reads as a miss and is rebuilt.
    let hex = key.to_hex();
    std::fs::write(
        dir.path().join(&hex[..2]).join(hex.as_str()),
        b"REAC\x01\0\0\0\xff\xff",
    )
    .unwrap();
    let (_, outcome) = cache
        .get_or_insert_with(&key, || Ok::<_, ()>(vec![9u32]))
        .unwrap();
    assert_eq!(outcome, re_assets::CacheOutcome::Miss);
}

#[test]
fn handles_ids_and_textures() {
    let dir = project();
    let mut s = AssetServer::new(dir.path());
    let lid = s.load_gltf("assets/models/lid.glb").unwrap();
    assert_eq!(
        s.load_gltf("./assets/models/../models/lid.glb").unwrap(),
        lid,
        "same path, same handle"
    );
    let info = s.info(lid.into()).unwrap().clone();
    assert_eq!(info.path, "assets/models/lid.glb");
    assert_eq!(info.version, 0);
    let id = info.id.unwrap();
    assert_eq!(s.path_of(id), Some("assets/models/lid.glb"));

    // A texture inside the glTF, by the id its material refers to.
    let tex_ref = s.gltf(lid).unwrap().materials[0]
        .desc
        .base_color_texture
        .clone()
        .unwrap();
    let TextureRef::Asset(tex_id) = tex_ref else {
        panic!()
    };
    assert_eq!(s.texture_by_id(&tex_id).unwrap().width, 2);

    // A material's texture file, loaded on first use.
    let mat = s.load_material("materials/rock.material.json").unwrap();
    let textures = s.material_textures(mat).unwrap();
    assert_eq!(textures.len(), 1);
    assert_eq!(textures[0].0, 0, "base colour slot");
    assert_eq!(textures[0].2.format, TextureFormat::Rgba8Srgb);
    assert!(matches!(
        s.loaded("assets/textures/rock.png"),
        Some(LoadedAsset::Texture(_))
    ));

    // Releasing makes every copy of the handle stale, even after the slot is reused.
    s.release(lid);
    assert!(s.gltf(lid).is_none());
    let again = s.load_gltf("assets/models/lid.glb").unwrap();
    assert_eq!(again.index(), lid.index());
    assert_ne!(again, lid);
    assert!(s.gltf(lid).is_none());
    assert!(s.gltf(again).is_some());
    assert_eq!(
        s.info(again.into()).unwrap().id,
        Some(id),
        "the sidecar keeps the id"
    );

    assert_eq!(
        s.texture_by_id("not-an-id").unwrap_err().code.to_string(),
        "RE7041"
    );
    assert_eq!(
        s.load("assets/models/tree.fbx")
            .unwrap_err()
            .code
            .to_string(),
        "RE7042"
    );
    assert_eq!(
        s.load("/etc/passwd").unwrap_err().code.to_string(),
        "RE7040"
    );
}

#[test]
fn clips_by_reference() {
    let dir = project();
    let mut s = AssetServer::new(dir.path());
    let clip = s.clip("assets/models/lid.glb#open").unwrap();
    assert_eq!(clip.duration, 2.0);
    let err = s.clip("assets/models/lid.glb#close").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7041");
    assert!(err.hint.unwrap().contains("open"));
    assert_eq!(
        s.clip("assets/models/lid.glb")
            .unwrap_err()
            .code
            .to_string(),
        "RE7041"
    );
}

#[test]
fn reload_after_changes_keeps_last_good_on_failure() {
    let dir = project();
    let root = dir.path();
    let mut s = AssetServer::new(root);
    let mat = s.load_material("materials/rock.material.json").unwrap();
    let tex = s.load_texture("assets/textures/rock.png").unwrap();
    let tex_id = s.info(tex.into()).unwrap().id.unwrap();

    // An atomic save arrives as `Created` for a loaded path: re-imported.
    atomic_save(
        &root.join("materials/rock.material.json"),
        br#"{ "roughness": 0.3 }"#,
    );
    let r = s.apply_changes(&[AssetChange::Created("materials/rock.material.json".into())]);
    assert_eq!(r.reloaded, [LoadedAsset::Material(mat)]);
    assert_eq!(s.material(mat).unwrap().roughness, 0.3);
    assert_eq!(s.info(mat.into()).unwrap().version, 1);

    // A broken save keeps the last good version and reports why.
    std::fs::write(
        root.join("materials/rock.material.json"),
        "{ \"roughness\": ",
    )
    .unwrap();
    let r = s.apply_changes(&[AssetChange::Modified("materials/rock.material.json".into())]);
    assert!(r.reloaded.is_empty());
    assert_eq!(r.problems[0].code.to_string(), "RE7050");
    assert_eq!(r.problems[0].related[0].code.to_string(), "RE7010");
    assert_eq!(s.material(mat).unwrap().roughness, 0.3);

    // A texture change reports its id for re-upload.
    std::fs::write(
        root.join("assets/textures/rock.png"),
        common::png(1, 1, &[1, 2, 3, 4]),
    )
    .unwrap();
    let r = s.apply_changes(&[AssetChange::Modified("assets/textures/rock.png".into())]);
    assert_eq!(r.changed_ids, [tex_id]);
    assert_eq!(s.texture(tex).unwrap().width, 1);

    // Changing the sidecar's settings re-imports the file.
    let meta = root.join("assets/textures/rock.png.meta.json");
    let text = std::fs::read_to_string(&meta).unwrap().replace(
        "\"settings\": {}",
        "\"settings\": { \"color_space\": \"linear\" }",
    );
    std::fs::write(&meta, text).unwrap();
    let r = s.apply_changes(&[AssetChange::Modified(
        "assets/textures/rock.png.meta.json".into(),
    )]);
    assert_eq!(r.reloaded.len(), 1);
    assert_eq!(s.texture(tex).unwrap().format, TextureFormat::Rgba8Unorm);

    // Saving unchanged content is not a reload.
    let r = s.apply_changes(&[AssetChange::Modified("assets/textures/rock.png".into())]);
    assert!(r.reloaded.is_empty());

    // Scripts are reported; unloaded files are ignored.
    let r = s.apply_changes(&[
        AssetChange::Modified("scripts/game/player.luau".into()),
        AssetChange::Created("assets/models/other.glb".into()),
    ]);
    assert_eq!(r.scripts, ["scripts/game/player.luau"]);
    assert!(r.reloaded.is_empty() && r.problems.is_empty());
}

#[test]
fn renames_keep_the_id() {
    let dir = project();
    let root = dir.path();
    let mut s = AssetServer::new(root);
    let tex = s.load_texture("assets/textures/rock.png").unwrap();
    let id = s.info(tex.into()).unwrap().id.unwrap();
    std::fs::rename(
        root.join("assets/textures/rock.png"),
        root.join("assets/textures/stone.png"),
    )
    .unwrap();
    s.apply_changes(&[AssetChange::Renamed {
        from: "assets/textures/rock.png".into(),
        to: "assets/textures/stone.png".into(),
    }]);
    assert_eq!(
        s.info(tex.into()).unwrap().path,
        "assets/textures/stone.png"
    );
    assert_eq!(s.path_of(id), Some("assets/textures/stone.png"));
    assert!(
        root.join("assets/textures/stone.png.meta.json").is_file(),
        "the sidecar moved along"
    );
    assert_eq!(
        s.loaded("assets/textures/stone.png"),
        Some(LoadedAsset::Texture(tex))
    );
    assert_eq!(s.texture_by_id(&id.to_string()).unwrap().width, 2);
}

fn collect(
    w: &AssetWatcher,
    wait: Duration,
    until: impl Fn(&[AssetChange]) -> bool,
) -> Vec<AssetChange> {
    let deadline = Instant::now() + wait;
    let mut all = Vec::new();
    while Instant::now() < deadline {
        all.extend(w.poll());
        if until(&all) {
            break;
        }
        sleep(Duration::from_millis(20));
    }
    all
}

#[test]
fn watcher_reports_an_atomic_save_and_the_server_reloads() {
    let dir = project();
    let root = dir.path().canonicalize().unwrap();
    let mut s = AssetServer::new(&root);
    let mat = s.load_material("materials/rock.material.json").unwrap();
    let w = AssetWatcher::new(&root, Duration::from_millis(100)).unwrap();
    sleep(Duration::from_millis(50));

    atomic_save(
        &root.join("materials/rock.material.json"),
        br#"{ "roughness": 0.25 }"#,
    );
    let target = "materials/rock.material.json";
    let changes = collect(&w, Duration::from_secs(5), |c| {
        c.iter().any(|c| c.path() == target)
    });
    assert!(
        changes.iter().any(|c| c.path() == target),
        "no event for the atomic save: {changes:?}"
    );
    assert!(
        changes.iter().all(|c| !c.path().ends_with(".tmp")),
        "temporary files are filtered: {changes:?}"
    );
    let r = s.apply_changes(&changes);
    assert_eq!(r.reloaded, [LoadedAsset::Material(mat)]);
    assert_eq!(s.material(mat).unwrap().roughness, 0.25);

    // Script edits come through too, for script hot reload.
    std::fs::write(root.join("scripts/game/player.luau"), "print('hi')\n").unwrap();
    let changes = collect(&w, Duration::from_secs(5), |c| {
        c.iter().any(AssetChange::is_script)
    });
    assert!(
        changes
            .iter()
            .any(|c| c.is_script() && c.path() == "scripts/game/player.luau"),
        "{changes:?}"
    );

    // The engine's own cache writes under .real/ are not reported.
    std::fs::create_dir_all(root.join(".real/cache")).unwrap();
    std::fs::write(root.join(".real/cache/x"), "x").unwrap();
    let changes = collect(&w, Duration::from_millis(400), |_| false);
    assert!(
        changes.iter().all(|c| !c.path().starts_with(".real/")),
        "{changes:?}"
    );
}

#[test]
fn watching_a_missing_directory_is_a_diagnostic() {
    let err = AssetWatcher::new(Path::new("/definitely/not/here"), Duration::from_millis(50))
        .err()
        .unwrap();
    assert_eq!(err.code.to_string(), "RE7030");
}
