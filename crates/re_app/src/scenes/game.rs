//! What scripts reach of the running game ([`GameAccess`], ADR-0059 §10): the
//! scenes and the save slots, lent by the App to the `scene` and `save`
//! libraries for the length of a script call.

use re_script::{GameAccess, SaveInfo, SaveSlot, SceneLoadState, SceneRefusal, SceneRequestKind};

use super::{Saves, Scenes};

/// The game a script call reaches: [`GameAccess`] over the App's scenes and
/// saves.
pub struct Game<'a> {
    /// The scenes loaded, asked for and kept.
    pub scenes: &'a mut Scenes,
    /// The save slots.
    pub saves: &'a mut Saves,
}

impl GameAccess for Game<'_> {
    fn scene_request(&mut self, kind: SceneRequestKind, name: &str) -> Result<u64, SceneRefusal> {
        self.scenes.scene_request(kind, name)
    }

    fn scene_unload(&mut self, name: &str) -> Result<(), SceneRefusal> {
        self.scenes.scene_unload(name)
    }

    fn scene_load(&self, id: u64) -> Option<SceneLoadState> {
        self.scenes.scene_load(id)
    }

    fn scene_current(&self) -> String {
        self.scenes.main().to_string()
    }

    fn scenes_loaded(&self) -> Vec<String> {
        self.scenes.loaded()
    }

    fn scenes_listed(&self) -> Vec<String> {
        self.scenes.list().names()
    }

    fn keep(&mut self, id: &str, keep: bool) {
        self.scenes.keep(id, keep);
    }

    fn is_kept(&self, id: &str) -> bool {
        self.scenes.is_kept(id)
    }

    fn save_version(&self) -> u32 {
        self.saves.version()
    }

    fn save_write(&mut self, slot: &str, saved_at: i64, data: serde_json::Value) {
        self.saves.write(slot, saved_at, data);
    }

    fn save_read(&mut self, slot: &str) -> Option<SaveSlot> {
        self.saves.read(slot)
    }

    fn save_exists(&mut self, slot: &str) -> bool {
        self.saves.exists(slot)
    }

    fn save_list(&mut self) -> Vec<SaveInfo> {
        self.saves.list()
    }

    fn save_delete(&mut self, slot: &str) {
        self.saves.delete(slot);
    }
}
