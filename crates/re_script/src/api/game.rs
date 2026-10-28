//! What the `scene` library asks of the running game (ADR-0059 §3, §10):
//! [`GameAccess`], which `re_app` implements over its scenes module and hands
//! out through [`ScriptHost::game`](crate::ScriptHost::game). A host without a
//! game (builder runs, tools without a running game, tests) leaves it `None`,
//! and the library then raises `RE3015`, as the services do with no game
//! running.

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

/// The running game's scenes, as the `scene` library reaches them. Every call
/// answers at once; loads apply at the end of a frame (ADR-0059 §4).
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
}
