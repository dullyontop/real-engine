//! The runtime commands `re_app` registers (ARCHITECTURE §6): `verify`.
//! `screenshot`, `play.run` and `asset.import` join as the renderer and the
//! asset pipeline are integrated.

use re_command::{CommandRegistry, Tier};
use re_core::Diagnostic;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::app::App;
use crate::verify::{self, VerifyOptions};

/// Arguments of `verify`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyArgs {
    /// Frames the boot stage runs (default 120).
    #[serde(default)]
    pub boot_frames: Option<u64>,
}

/// Registers the runtime commands on `registry`.
pub fn register(registry: &mut CommandRegistry<App>) {
    registry.register_typed::<VerifyArgs, _>(
        "verify",
        "Check the project as saved on disk: content files, scripts, a headless boot, replays (twice, comparing state hashes), golden screenshots and frame times. Returns the verify report.",
        Tier::Read,
        true,
        |app, args, _call| {
            let mut options = VerifyOptions::default();
            if let Some(n) = args.boot_frames {
                options.boot_frames = n.max(1);
                options.warmup_frames = options.warmup_frames.min(options.boot_frames - 1);
            }
            let stages = app.verify_stages();
            let refs: Vec<&dyn verify::VerifyStage> =
                stages.iter().map(|s| &**s as &dyn verify::VerifyStage).collect();
            let report = verify::run(app, &options, &refs);
            serde_json::to_value(report).map_err(|e| {
                Diagnostic::error(
                    crate::codes::BOOT_FAILED,
                    format!("the verify report could not be encoded: {e}"),
                )
            })
        },
    );
}
