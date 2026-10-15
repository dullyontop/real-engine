//! The diagnostic codes `re_app` owns (`RE0500`–`RE0899`; `re_cli` owns
//! `RE0900`–`RE0999` of the shared runtime range).
//!
//! Each code has an explain page in `crates/re_app/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// The directory has no `real.toml`, so it is not a project.
pub const PROJECT_NOT_FOUND: Code = Code::new("RE0500");

/// `real.toml` cannot be read or has an invalid key or value.
pub const CONFIG_INVALID: Code = Code::new("RE0501");

/// The entry scene cannot be read or loaded.
pub const ENTRY_SCENE_FAILED: Code = Code::new("RE0502");

/// A script asked for something of an entity that does not have it (no body,
/// no character, no `HudText`), or of an entity that does not exist.
pub const HOST_NO_SUCH_THING: Code = Code::new("RE0503");
/// [`HOST_NO_SUCH_THING`] as text, for `re_types::HostError`.
pub const HOST_NO_SUCH_THING_STR: &str = "RE0503";

/// A script passed an invalid argument to a runtime service.
pub const HOST_INVALID_ARGUMENT: Code = Code::new("RE0504");
/// [`HOST_INVALID_ARGUMENT`] as text, for `re_types::HostError`.
pub const HOST_INVALID_ARGUMENT_STR: &str = "RE0504";

/// A `Collider` or `CharacterController` field is out of the range physics accepts
/// (`layer` outside 0..=65535, `groups` outside 0..=u32::MAX).
pub const PHYSICS_FIELD_OUT_OF_RANGE: Code = Code::new("RE0505");

/// A collider's `source` names a mesh or heightfield that is not available.
pub const SHAPE_SOURCE_MISSING: Code = Code::new("RE0506");

/// A replay file cannot be read or does not match the project's actions.
pub const REPLAY_FAILED: Code = Code::new("RE0507");

/// Two runs of the same replay ended with different state hashes.
pub const NONDETERMINISTIC: Code = Code::new("RE0508");

/// A screenshot differs from its golden image by more than the tolerance.
pub const GOLDEN_MISMATCH: Code = Code::new("RE0509");

/// A replay check names a golden image that does not exist.
pub const GOLDEN_MISSING: Code = Code::new("RE0510");

/// A measured frame time is over the budget in `real.toml [verify.perf]`.
pub const PERF_BUDGET: Code = Code::new("RE0511");

/// A verify stage did not run (the build or the project lacks what it needs).
pub const STAGE_SKIPPED: Code = Code::new("RE0512");

/// A content file (scene, prefab, material, graph, replay) is malformed.
pub const CONTENT_INVALID: Code = Code::new("RE0513");

/// The project did not boot headless.
pub const BOOT_FAILED: Code = Code::new("RE0514");

/// A command listed in a replay failed.
pub const REPLAY_COMMAND_FAILED: Code = Code::new("RE0515");

/// A screenshot was asked for but cannot be taken.
pub const SCREENSHOT_FAILED: Code = Code::new("RE0516");

/// The scene could not be written back to disk after an edit.
pub const SCENE_SAVE_FAILED: Code = Code::new("RE0518");

/// A run reported errors (subsystem or script errors during frames).
pub const RUN_ERRORS: Code = Code::new("RE0519");

/// `asset.import` could not copy a file into the project.
pub const ASSET_IMPORT_FAILED: Code = Code::new("RE0517");

/// A mesh, material or clip a component names cannot be loaded.
pub const ASSET_UNAVAILABLE: Code = Code::new("RE0520");

/// The window or its GPU surface could not be opened.
pub const WINDOW_FAILED: Code = Code::new("RE0521");

/// An animation clip animates a node that no entity under the player is named after.
pub const ANIM_TARGET_MISSING: Code = Code::new("RE0522");

/// A procedural source's graph could not be made: its graph file is missing or
/// invalid, or its builder script failed.
pub const GRAPH_SOURCE_FAILED: Code = Code::new("RE0523");

/// A character could not be put back on the ground after the terrain under it
/// was regenerated (no ground below it).
pub const REGROUND_FAILED: Code = Code::new("RE0524");

/// A regeneration during a verify replay broke a promise of the runtime: a
/// character lost its `Entity` or did not stand on the new ground, or a
/// collected (script-removed) item came back.
pub const REGENERATION_BROKE: Code = Code::new("RE0525");

/// A compat script was still waiting in `WaitForChild` when the boot run of
/// `real verify` ended.
pub const STUCK_YIELD: Code = Code::new("RE0526");
