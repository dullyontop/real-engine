//! What the `scene` and `save` libraries ask of the running game (ADR-0059
//! §3, §7, §10): [`GameAccess`], which `re_app` implements over its scenes
//! module and hands out through [`ScriptHost::game`](crate::ScriptHost::game).
//! A host without a game (builder runs, tools without a running game, tests)
//! leaves it `None`, and the libraries then raise `RE3015`, as the services
//! do with no game running.

/// How a scene request asks for its scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneRequestKind {
    /// `scene.switch`: unload every scene and load this one in their place.
    Switch,
    /// `scene.load`: load this one beside the loaded scenes.
    Load,
}

/// Why the game refused a scene request; the library raises it in the calling
/// script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneRefusal {
    /// No scene has that name (`RE3100`).
    Unknown,
    /// The request does not fit the scenes loaded or asked for (`RE3101`):
    /// why, as a sentence.
    Refused(String),
}

/// A load that `scene.switch` or `scene.load` started, as `SceneLoad` reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneLoadState {
    /// The scene's name.
    pub name: String,
    /// How far the load got, 0 to 1.
    pub progress: f64,
    /// True once the load is over, applied or failed.
    pub done: bool,
    /// Why the load failed (then nothing changed), or `None`.
    pub error: Option<String>,
}

/// A save slot's data, as `save.read` gets it (ADR-0059 §7).
#[derive(Clone, Debug, PartialEq)]
pub struct SaveSlot {
    /// The game's data version the slot was written with (`[saves] version`
    /// of the build that wrote it).
    pub data_version: u32,
    /// The run's `os.time()` when the slot was written.
    pub saved_at: i64,
    /// The data, as JSON.
    pub data: serde_json::Value,
}

/// A save slot as `save.list` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveInfo {
    /// The slot's name.
    pub slot: String,
    /// The game's data version it was written with.
    pub data_version: u32,
    /// The run's `os.time()` when it was written.
    pub saved_at: i64,
}

/// The running game's scenes and save slots, as the `scene` and `save`
/// libraries reach them. Every call answers at once; loads apply at the end
/// of a frame (ADR-0059 §4), and save files are written there (§7).
pub trait GameAccess {
    /// Starts a switch or an additive load of scene `name` and returns the
    /// load's id, for [`GameAccess::scene_load`].
    ///
    /// # Errors
    ///
    /// [`SceneRefusal::Unknown`] for a name the game does not have, and
    /// [`SceneRefusal::Refused`] for a load of a scene that is already loaded
    /// or asked for.
    fn scene_request(&mut self, kind: SceneRequestKind, name: &str) -> Result<u64, SceneRefusal>;

    /// Unloads `name`, a scene `scene.load` added, at the end of the frame.
    ///
    /// # Errors
    ///
    /// [`SceneRefusal::Unknown`] for a name the game does not have, and
    /// [`SceneRefusal::Refused`] for the main scene or a scene that is not
    /// loaded.
    fn scene_unload(&mut self, name: &str) -> Result<(), SceneRefusal>;

    /// The state of load `id`, or `None` for an id this game never gave.
    fn scene_load(&self, id: u64) -> Option<SceneLoadState>;

    /// The main scene's name.
    fn scene_current(&self) -> String;

    /// The loaded scenes: the main scene, then the added ones in load order.
    fn scenes_loaded(&self) -> Vec<String>;

    /// Every scene the game can load, sorted.
    fn scenes_listed(&self) -> Vec<String>;

    /// Keeps the root entity with stable id `id` across switches, or stops
    /// keeping it (`keep` false). The library has checked that it is a root
    /// and not streamed.
    fn keep(&mut self, id: &str, keep: bool);

    /// True if the entity with stable id `id` is kept across switches.
    fn is_kept(&self, id: &str) -> bool;

    /// `[saves] version`: the data version this build writes, and reads to.
    fn save_version(&self) -> u32;

    /// Keeps `data` in `slot` (a name the library checked), stamped with
    /// [`GameAccess::save_version`] and `saved_at`. Reads see it at once; its
    /// file is written at the end of the frame.
    fn save_write(&mut self, slot: &str, saved_at: i64, data: serde_json::Value);

    /// The data in `slot`, or `None` for an empty slot.
    fn save_read(&mut self, slot: &str) -> Option<SaveSlot>;

    /// True if `slot` holds data.
    fn save_exists(&mut self, slot: &str) -> bool;

    /// Every slot that holds data, sorted by name.
    fn save_list(&mut self) -> Vec<SaveInfo>;

    /// Empties `slot`; its file is removed at the end of the frame.
    fn save_delete(&mut self, slot: &str);
}
