//! The steps of `play.run` (docs/M2_PLAN.md, WP16): after its first frames,
//! a play can play more frames, join players, chat as a player, call a
//! `BindableFunction` test hook and dump the instance tree. All but `frames`
//! and `dump_tree` need a `[compat]` project. Each step returns a JSON result
//! ([`App::play_steps`](crate::App::play_steps)).

use re_compat::dump::DumpArgs;
use re_core::Diagnostic;
use re_scene::{Entity, World};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value as Json;

use crate::codes;

/// One step of `play.run`, run in order after the play's first `frames`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlayStep {
    /// Play this many more frames: `{"frames": 30}`.
    Frames(u64),
    /// Join the next player (`Player2` after the `[compat] players`):
    /// `"add_player"`. Returns the player's name and id.
    AddPlayer,
    /// A player says `text` in chat: `{"say": {"text": "/shop"}}`.
    Say(SayStep),
    /// Call a `BindableFunction` with arguments and return its results:
    /// `{"invoke": {"path": "ServerStorage.Hooks.Score", "args": [1]}}`.
    Invoke(InvokeStep),
    /// The instance tree, with attributes and tags, as `scene.dump_tree`
    /// returns it: `{"dump_tree": {"path": "Workspace", "depth": 2}}`.
    DumpTree(DumpStep),
}

/// Arguments of the `say` step.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SayStep {
    /// Which player speaks, by number (default 1, the local player).
    #[serde(default = "first")]
    pub player: u32,
    /// The chat message.
    pub text: String,
}

fn first() -> u32 {
    1
}

/// Arguments of the `invoke` step.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvokeStep {
    /// The `BindableFunction`, as a dotted path of names from `game`
    /// (`ServerStorage.Hooks.Score`).
    pub path: String,
    /// The arguments, as JSON values.
    #[serde(default)]
    pub args: Vec<Json>,
    /// Call it in the client context, on the client's copy (default: the
    /// server).
    #[serde(default)]
    pub client: bool,
}

/// Arguments of the `dump_tree` step.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DumpStep {
    /// Where to start: a dotted path of names from `game` (`Workspace.Map`),
    /// or `game` (the default).
    #[serde(default)]
    pub path: Option<String>,
    /// How many levels below the start to include (default: all).
    #[serde(default)]
    pub depth: Option<u32>,
    /// Include every component's fields (default false).
    #[serde(default)]
    pub components: bool,
    /// The most nodes to return (default 2000, at most 10000).
    #[serde(default)]
    pub max_nodes: Option<usize>,
    /// Dump the client's world (default: the server's).
    #[serde(default)]
    pub client: bool,
}

impl PlayStep {
    /// The frames this step plays.
    pub fn frames(&self) -> u64 {
        match self {
            PlayStep::Frames(n) => *n,
            _ => 0,
        }
    }
}

/// A failed step (`RE0527`).
pub(crate) fn step_failed(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::PLAY_STEP_FAILED, msg)
}

/// The instance at a dotted `path` of names from `game` (`game` itself for
/// `game` or an empty path; a leading `game.` is allowed).
pub(crate) fn instance_at(world: &World, path: &str) -> Result<Entity, Diagnostic> {
    let mut cur = world
        .entity(re_compat::GAME_ID)
        .map_err(|_| step_failed("the world has no `game` instance"))?;
    let rest = if path == "game" {
        ""
    } else {
        path.strip_prefix("game.").unwrap_or(path)
    };
    if rest.is_empty() {
        return Ok(cur);
    }
    for (i, name) in rest.split('.').enumerate() {
        cur = *re_scene::hierarchy::children(&world.ecs, cur)
            .iter()
            .find(|c| world.name_of(**c) == name)
            .ok_or_else(|| {
                let at: Vec<&str> = rest.split('.').take(i).collect();
                step_failed(format!(
                    "no instance named {name:?} under game{}{}",
                    if at.is_empty() { "" } else { "." },
                    at.join(".")
                ))
            })?;
    }
    Ok(cur)
}

/// The `dump_tree` arguments of a step, starting at `root` (an id).
pub(crate) fn dump_args(step: &DumpStep, root: String) -> DumpArgs {
    DumpArgs {
        root: Some(root),
        depth: step.depth,
        components: step.components,
        max_nodes: step.max_nodes,
    }
}
