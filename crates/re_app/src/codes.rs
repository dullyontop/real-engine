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
