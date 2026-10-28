//! Fresh worlds (ADR-0059 §5, §9): a scene loaded into a new World, as a
//! project opens, `world.restart()` reloads the scene the run began in and
//! edit mode opens a scene; and the copy of a World that a play session plays
//! (`play.run`, the editor's Play) and restarts from.

use re_core::files::ProjectFiles;
use re_core::{Diagnostic, Location};
use re_proc::stream::{StreamConfig, Streamer};
use re_scene::World;

use crate::codes;
use crate::config::AppConfig;
use crate::procedural;

/// An empty World with the components the project's content may use: in a
/// compat project, the class layer's components too.
///
/// # Errors
///
/// `RE0502` when the compat classes do not register.
pub fn content_world(config: &AppConfig) -> Result<World, Diagnostic> {
    let mut world = World::new();
    if config.compat.is_some() {
        // A compat scene's instances carry the class layer's components.
        let setup = crate::compat::setup()?;
        setup.classes.register_components(&mut world).map_err(|e| {
            Diagnostic::error(
                codes::ENTRY_SCENE_FAILED,
                format!("the compat classes do not register: {e}"),
            )
        })?;
    }
    Ok(world)
}

/// Gives `world` its streaming manager with `[stream]`, so `stream.load` works
/// before the first step.
///
/// # Errors
///
/// `RE4300` when `config` is not a valid `[stream]`.
pub fn ensure_streamer(world: &mut World, config: &StreamConfig) -> Result<(), Diagnostic> {
    if !world.ecs.contains_resource::<Streamer>() {
        world.ecs.insert_resource(Streamer::new(config.clone())?);
    }
    Ok(())
}

/// A fresh World with the scene at project path `path` loaded, and the graphs
/// of its procedural sources in its store (they generate on the first
/// `regenerate_changed`), with the problems making those graphs.
///
/// # Errors
///
/// When the scene cannot be read, parsed or loaded (with the scene's own
/// diagnostic as `related`): `RE0502` for the entry scene, as when the
/// project opens, and `RE0570` for any other.
pub fn fresh_world(
    files: &ProjectFiles,
    config: &AppConfig,
    path: &str,
) -> Result<(World, Vec<Diagnostic>), Diagnostic> {
    let (code, what) = if path == config.project.entry_scene {
        (codes::ENTRY_SCENE_FAILED, "the entry scene")
    } else {
        (codes::SCENE_LOAD_FAILED, "scene")
    };
    let fail =
        |msg: String| Diagnostic::error(code, msg).at(Location::file(path.replace('\\', "/")));
    let text = files
        .read_str(path)
        .map_err(|e| fail(format!("cannot read {what} {path}: {e}")))?;
    let doc = re_scene::scene::from_json(&text).map_err(|e| {
        fail(format!("{what} {path} does not parse")).with_related(e.to_diagnostic())
    })?;
    let mut world = content_world(config).map_err(|e| fail(e.message.clone()))?;
    procedural::install(&mut world, files);
    let mut problems = procedural::declare_generators(&mut world, files, config.proc.limits());
    world.load(&doc).map_err(|e| {
        fail(format!("{what} {path} does not load")).with_related(e.to_diagnostic())
    })?;
    world.propagate_all();
    problems.extend(procedural::load_graphs(&mut world, files, None));
    Ok((world, problems))
}

/// A copy of `from` to play from, or to restart a play session from:
/// [`World::fork`], with the procedural sources' graphs and world streaming's
/// state (ADR-0042) carried over, so the copy streams on from where `from`
/// is.
pub fn fork_world(from: &World) -> World {
    let mut world = from.fork();
    if let Some(store) = from.ecs.get_resource::<re_proc::GraphStore>() {
        world.ecs.insert_resource(store.clone());
    }
    if let Some(streamer) = from.ecs.get_resource::<Streamer>() {
        let forked = streamer.forked(from, &world);
        world.ecs.insert_resource(forked);
    }
    world
}
