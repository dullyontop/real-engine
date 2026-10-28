//! The scenes a game can load, by name (ADR-0059 §1): `[scenes]` in
//! `real.toml`, and the entry scene under its file stem unless `[scenes]`
//! names its path.

use std::collections::BTreeMap;

use crate::config::ProjectConfig;

/// The longest scene name.
pub const MAX_NAME: usize = 64;

/// The suffix of a scene file.
pub const SCENE_SUFFIX: &str = ".scene.json";

/// True if `name` can name a scene: 1 to 64 characters from `a`–`z`, `0`–`9`,
/// `_` and `-`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The name the entry scene gets when `[scenes]` does not name its path: its
/// file name without `.scene.json` (or its last extension), lowercased, with
/// every character a name cannot hold made `_`, and `main` when nothing is
/// left.
pub fn implicit_name(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file
        .strip_suffix(SCENE_SUFFIX)
        .or_else(|| file.rsplit_once('.').map(|(stem, _)| stem))
        .unwrap_or(file);
    let name: String = stem
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(MAX_NAME)
        .collect();
    if name.is_empty() { "main".into() } else { name }
}

/// The scenes of a game: each name with its project path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneList {
    paths: BTreeMap<String, String>,
    entry: String,
}

impl SceneList {
    /// The list `[project]` and `[scenes]` make.
    ///
    /// # Errors
    ///
    /// Why the list is wrong, as a sentence (`RE0572` at the config): a bad
    /// name, a path that is not a project path ending in `.scene.json`, two
    /// names for one path, or an implicit entry name that a listed scene
    /// already has.
    pub fn new(
        project: &ProjectConfig,
        scenes: &BTreeMap<String, String>,
    ) -> Result<SceneList, String> {
        let mut by_path: BTreeMap<&str, &str> = BTreeMap::new();
        for (name, path) in scenes {
            if !valid_name(name) {
                return Err(format!(
                    "[scenes] name {name:?} is not a scene name: use 1 to {MAX_NAME} characters from a-z, 0-9, _ and -"
                ));
            }
            if let Err(e) = re_core::files::ProjectPath::new(path) {
                return Err(format!("[scenes] {name} = {path:?}: {e}"));
            }
            if !path.ends_with(SCENE_SUFFIX) {
                return Err(format!(
                    "[scenes] {name} = {path:?} is not a scene file (a path ending in {SCENE_SUFFIX})"
                ));
            }
            if let Some(other) = by_path.insert(path, name) {
                return Err(format!(
                    "[scenes] names {other:?} and {name:?} are the same scene ({path:?}); keep one"
                ));
            }
        }
        let mut paths = scenes.clone();
        let entry_path = project.entry_scene.as_str();
        let entry = match by_path.get(entry_path) {
            Some(name) => (*name).to_string(),
            None => {
                let name = implicit_name(entry_path);
                if let Some(path) = paths.get(&name) {
                    return Err(format!(
                        "the entry scene {entry_path:?} is named {name:?} after its file, but [scenes] gives that name to {path:?}; name the entry scene in [scenes] too"
                    ));
                }
                paths.insert(name.clone(), entry_path.to_string());
                name
            }
        };
        Ok(SceneList { paths, entry })
    }

    /// The project path of scene `name`.
    pub fn path(&self, name: &str) -> Option<&str> {
        self.paths.get(name).map(String::as_str)
    }

    /// The name of the scene at project path `path`.
    pub fn name_of(&self, path: &str) -> Option<&str> {
        self.paths
            .iter()
            .find(|(_, p)| p.as_str() == path)
            .map(|(n, _)| n.as_str())
    }

    /// The entry scene's name.
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Every name, sorted.
    pub fn names(&self) -> Vec<String> {
        self.paths.keys().cloned().collect()
    }

    /// Every scene: names with their paths, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.paths.iter().map(|(n, p)| (n.as_str(), p.as_str()))
    }

    /// The name of the scene at `path`, listing it under its path when no
    /// name has it: a run that starts in a scene the list does not name (a
    /// play session the editor started from another scene file, or a replay's
    /// `start.scene`) can then read, switch to and reload it by that name. A
    /// path never collides with a name, which has no `/` or `.`.
    pub fn name_or_list(&mut self, path: &str) -> String {
        if let Some(name) = self.name_of(path) {
            return name.to_string();
        }
        self.paths.insert(path.to_string(), path.to_string());
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(entry: &str) -> ProjectConfig {
        ProjectConfig {
            entry_scene: entry.into(),
            ..ProjectConfig::default()
        }
    }

    fn scenes(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(n, p)| (n.to_string(), p.to_string()))
            .collect()
    }

    #[test]
    fn without_scenes_the_entry_scene_is_the_only_one() {
        let list = SceneList::new(&project("scenes/main.scene.json"), &BTreeMap::new()).unwrap();
        assert_eq!(list.names(), ["main"]);
        assert_eq!(list.entry(), "main");
        assert_eq!(list.path("main"), Some("scenes/main.scene.json"));
    }

    #[test]
    fn a_listed_entry_scene_keeps_its_listed_name() {
        let list = SceneList::new(
            &project("scenes/title.scene.json"),
            &scenes(&[
                ("title", "scenes/title.scene.json"),
                ("town", "scenes/town.scene.json"),
                ("harbour", "levels/harbour.scene.json"),
            ]),
        )
        .unwrap();
        assert_eq!(list.names(), ["harbour", "title", "town"]);
        assert_eq!(list.entry(), "title");
        assert_eq!(list.name_of("levels/harbour.scene.json"), Some("harbour"));
    }

    #[test]
    fn an_unlisted_entry_scene_is_named_after_its_file() {
        let list = SceneList::new(
            &project("scenes/Main Menu.scene.json"),
            &scenes(&[("town", "scenes/town.scene.json")]),
        )
        .unwrap();
        assert_eq!(list.entry(), "main_menu");
        assert_eq!(list.names(), ["main_menu", "town"]);
        assert_eq!(implicit_name("level.json"), "level");
        assert_eq!(implicit_name("scenes/.scene.json"), "main");
    }

    #[test]
    fn bad_lists_are_refused() {
        let entry = project("scenes/main.scene.json");
        for (pairs, why) in [
            (&[("Town", "scenes/town.scene.json")][..], "not a scene name"),
            (&[("town", "../town.scene.json")][..], ".."),
            (&[("town", "scenes/town.json")][..], "not a scene file"),
            (
                &[("a", "scenes/a.scene.json"), ("b", "scenes/a.scene.json")][..],
                "the same scene",
            ),
            (&[("main", "scenes/other.scene.json")][..], "name the entry scene"),
        ] {
            let e = SceneList::new(&entry, &scenes(pairs)).unwrap_err();
            assert!(e.contains(why), "{e}");
        }
        let long = "a".repeat(MAX_NAME + 1);
        assert!(SceneList::new(&entry, &scenes(&[(&long, "scenes/a.scene.json")])).is_err());
    }

    #[test]
    fn an_unlisted_start_scene_is_listed_under_its_path() {
        let mut list = SceneList::new(&project("scenes/main.scene.json"), &BTreeMap::new()).unwrap();
        assert_eq!(list.name_or_list("scenes/main.scene.json"), "main");
        assert_eq!(list.name_or_list("scenes/test.scene.json"), "scenes/test.scene.json");
        assert_eq!(
            list.path("scenes/test.scene.json"),
            Some("scenes/test.scene.json")
        );
    }
}
