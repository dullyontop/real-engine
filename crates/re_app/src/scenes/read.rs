//! The first two stages of a scene load (ADR-0059 §4): reading and parsing
//! the scene file, on a worker when the run applies loads late, and warming
//! the assets its components name into the caches, so that applying it does
//! not stall the frame.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use re_audio::Audio;
use re_core::files::ProjectFiles;
use re_core::time::Instant;
use re_core::{Diagnostic, Location, TaskPool};
use re_reflect::Doc;
use re_scene::SceneDoc;

use crate::codes;
use crate::media::{DERIVED, Media};

/// How long warming may take in one frame.
pub const WARM_BUDGET: Duration = Duration::from_millis(4);

/// `name` for messages: the name with its path, or the path alone for a
/// scene the list names by its path.
pub fn describe(name: &str, path: &str) -> String {
    if name == path {
        path.to_string()
    } else {
        format!("{name:?} ({path})")
    }
}

/// Reads and parses scene `name` at project path `path`.
///
/// # Errors
///
/// `RE0570` when the file cannot be read or does not parse (with the
/// scene's own diagnostic as `related`).
pub fn read(files: &ProjectFiles, name: &str, path: &str) -> Result<SceneDoc, Diagnostic> {
    let scene = describe(name, path);
    let text = files.read_str(path).map_err(|e| {
        Diagnostic::error(
            codes::SCENE_LOAD_FAILED,
            format!("cannot read scene {scene}: {e}"),
        )
        .at(Location::file(path))
    })?;
    re_scene::scene::from_json(&text).map_err(|e| {
        Diagnostic::error(
            codes::SCENE_LOAD_FAILED,
            format!("scene {scene} does not parse"),
        )
        .at(Location::file(path))
        .with_related(e.to_diagnostic())
    })
}

/// A read running on a worker.
#[derive(Debug)]
pub struct Job(Arc<Mutex<Option<Result<SceneDoc, Diagnostic>>>>);

impl Job {
    /// Starts reading scene `name` at `path` on the worker pool (at once, on
    /// the browser's single thread).
    pub fn start(files: &ProjectFiles, name: &str, path: &str) -> Job {
        let slot = Arc::new(Mutex::new(None));
        let (files, name, path) = (files.clone(), name.to_string(), path.to_string());
        let out = Arc::clone(&slot);
        TaskPool::global().spawn(move || {
            let result = read(&files, &name, &path);
            if let Ok(mut slot) = out.lock() {
                *slot = Some(result);
            }
        });
        Job(slot)
    }

    /// The result, once the worker is done.
    pub fn poll(&self) -> Option<Result<SceneDoc, Diagnostic>> {
        self.0.lock().ok()?.take()
    }
}

/// An asset a scene's components name, to warm before it applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asset {
    /// A `MeshRef.asset` file or built-in.
    Mesh(String),
    /// A `MaterialRef.asset`.
    Material(String),
    /// A `SkinnedMesh.asset`.
    Skin(String),
    /// An `AnimPlayer.clip`.
    Clip(String),
    /// An `Animator.graph`.
    Graph(String),
    /// An `AudioSource.clip`.
    Sound(String),
}

/// A component field that names an asset, and the kind of asset it names.
type AssetField = (&'static str, &'static str, fn(String) -> Asset);

/// The assets `scene`'s components name, in file order, each once. Derived
/// meshes are left out: they come from generation, at apply.
pub fn assets(scene: &SceneDoc) -> Vec<Asset> {
    const FIELDS: [AssetField; 6] = [
        ("MeshRef", "asset", Asset::Mesh),
        ("MaterialRef", "asset", Asset::Material),
        ("SkinnedMesh", "asset", Asset::Skin),
        ("AnimPlayer", "clip", Asset::Clip),
        ("Animator", "graph", Asset::Graph),
        ("AudioSource", "clip", Asset::Sound),
    ];
    let mut out: Vec<Asset> = Vec::new();
    for d in &scene.entities {
        for (component, field, asset) in FIELDS {
            let Some(Doc::Str(path)) = d.components.get(component).and_then(|f| f.get(field))
            else {
                continue;
            };
            if path.is_empty() || path.starts_with(DERIVED) {
                continue;
            }
            let asset = asset(path.clone());
            if !out.contains(&asset) {
                out.push(asset);
            }
        }
    }
    out
}

/// Loads `asset` into the caches. A failure is left for the asset's first
/// use to report, as it would be without warming.
pub fn warm(asset: &Asset, media: &mut Media, audio: &mut Audio) {
    match asset {
        Asset::Mesh(p) => drop(media.mesh(p)),
        Asset::Material(p) => drop(media.material(p)),
        Asset::Skin(p) => drop(media.skin(p)),
        Asset::Clip(p) => drop(media.clip(p)),
        Asset::Graph(p) => drop(media.anim_graph(p)),
        Asset::Sound(p) => drop(audio.load_clip(p)),
    }
}

/// Warms `assets` from `next` on until the budget that started at `start`
/// is spent; returns the index of the next asset to warm.
pub fn warm_some(
    assets: &[Asset],
    next: usize,
    start: Instant,
    media: &mut Media,
    audio: &mut Audio,
) -> usize {
    let mut next = next;
    while next < assets.len() && start.elapsed() < WARM_BUDGET {
        warm(&assets[next], media, audio);
        next += 1;
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use re_core::files::{Form, LayerKind, MemLayer, ProjectPath};

    fn files(pairs: &[(&str, &str)]) -> ProjectFiles {
        let mut layer = MemLayer::new(LayerKind::Memory);
        for (path, text) in pairs {
            layer.insert(
                ProjectPath::new(path).unwrap(),
                text.as_bytes().to_vec(),
                Form::Source,
            );
        }
        ProjectFiles::for_tests(layer)
    }

    const TOWN: &str = r#"{ "format": "real-engine/scene", "version": 1, "entities": [
        { "id": "a", "components": { "MeshRef": { "asset": "assets/house.glb" }, "MaterialRef": { "asset": "materials/brick.material.json" } } },
        { "id": "b", "components": { "MeshRef": { "asset": "assets/house.glb" } } },
        { "id": "c", "components": { "MeshRef": { "asset": "derived:abc" }, "AudioSource": { "clip": "assets/sfx/bell.wav" } } }
    ] }"#;

    #[test]
    fn reads_parse_or_fail_with_the_scene_named() {
        let files = files(&[
            ("scenes/town.scene.json", TOWN),
            ("scenes/bad.scene.json", "{"),
        ]);
        assert_eq!(
            read(&files, "town", "scenes/town.scene.json")
                .unwrap()
                .entities
                .len(),
            3
        );
        let d = read(&files, "bad", "scenes/bad.scene.json").unwrap_err();
        assert_eq!(d.code, codes::SCENE_LOAD_FAILED);
        assert!(
            d.message
                .contains(r#""bad" (scenes/bad.scene.json) does not parse"#),
            "{}",
            d.message
        );
        let d = read(&files, "scenes/gone.scene.json", "scenes/gone.scene.json").unwrap_err();
        assert!(
            d.message
                .starts_with("cannot read scene scenes/gone.scene.json: "),
            "{}",
            d.message
        );

        let job = Job::start(&files, "town", "scenes/town.scene.json");
        let doc = loop {
            if let Some(r) = job.poll() {
                break r.unwrap();
            }
            std::thread::yield_now();
        };
        assert_eq!(doc.entities.len(), 3);
    }

    #[test]
    fn assets_are_listed_once_in_file_order() {
        let doc = re_scene::scene::from_json(TOWN).unwrap();
        assert_eq!(
            assets(&doc),
            [
                Asset::Mesh("assets/house.glb".into()),
                Asset::Material("materials/brick.material.json".into()),
                Asset::Sound("assets/sfx/bell.wav".into()),
            ]
        );
    }
}
