//! The steps a `play.run` takes after its first frames (ADR-0021).

use re_core::Diagnostic;

use re_script::ContextKind;

use super::{App, RunReport};

impl App {
    /// Runs the steps of a `play.run` (after its first frames), in order, and
    /// returns one JSON result per step (`{ "step", "ok", ... }`). Frames,
    /// diagnostics, output and the final state hash are added to `report`. A
    /// step that fails reports its diagnostic and the play goes on.
    pub fn play_steps(
        &mut self,
        steps: &[crate::play::PlayStep],
        report: &mut RunReport,
    ) -> Vec<serde_json::Value> {
        use crate::play::PlayStep;
        use serde_json::json;
        let mut results = Vec::new();
        for step in steps {
            let result = match step {
                PlayStep::Frames(n) => match self.run_headless(Some(*n), None) {
                    Ok(r) => {
                        report.frames += r.frames;
                        report.diagnostics.extend(r.diagnostics);
                        report.screenshots.extend(r.screenshots);
                        report.output.extend(r.output);
                        Ok(json!({ "step": "frames", "frames": r.frames }))
                    }
                    Err(d) => Err(("frames", d)),
                },
                PlayStep::AddPlayer => match self.join_player() {
                    Ok(p) => {
                        let world = &self.world;
                        Ok(json!({
                            "step": "add_player",
                            "player": world.name_of(p),
                            "id": world.id_of(p),
                        }))
                    }
                    Err(d) => Err(("add_player", d)),
                },
                PlayStep::Say(say) => self.play_say(say).map_err(|d| ("say", d)),
                PlayStep::Invoke(inv) => self.play_invoke(inv).map_err(|d| ("invoke", d)),
                PlayStep::DumpTree(dump) => self.play_dump(dump).map_err(|d| ("dump_tree", d)),
            };
            results.push(match result {
                Ok(mut v) => {
                    v["ok"] = json!(true);
                    v
                }
                Err((name, d)) => {
                    let v = json!({ "step": name, "ok": false, "error": d });
                    self.diagnostics.push(d);
                    v
                }
            });
        }
        let mut diagnostics = self.take_diagnostics();
        diagnostics.retain(|d| d.severity != re_core::Severity::Note || d.is_error());
        report.diagnostics.extend(diagnostics);
        report
            .output
            .extend(std::mem::take(&mut self.script_output));
        report.state_hash = self.state_hash().to_hex().to_string();
        report.compat = self.compat.as_ref().map(|c| c.info());
        results
    }

    fn play_say(&mut self, say: &crate::play::SayStep) -> Result<serde_json::Value, Diagnostic> {
        if self.compat.is_none() {
            return Err(crate::play::step_failed("`say` needs a [compat] project"));
        }
        let name = format!("Player{}", say.player);
        let player = crate::play::instance_at(&self.world, &format!("Players.{name}"))?;
        let mut said = Ok(());
        self.compat_call(ContextKind::Server, |vm, h| {
            let (r, d) = re_compat::players::say(vm, h, player, &say.text);
            said = r;
            d
        });
        said?;
        Ok(serde_json::json!({ "step": "say", "player": name }))
    }

    fn play_invoke(
        &mut self,
        inv: &crate::play::InvokeStep,
    ) -> Result<serde_json::Value, Diagnostic> {
        let Some(c) = &self.compat else {
            return Err(crate::play::step_failed(
                "`invoke` needs a [compat] project",
            ));
        };
        let (kind, world) = if inv.client {
            (ContextKind::Client, &c.client)
        } else {
            (ContextKind::Server, &self.world)
        };
        let entity = crate::play::instance_at(world, &inv.path)?;
        let args = serde_json::Value::Array(inv.args.clone()).to_string();
        let mut out = Err(crate::play::step_failed("the call did not run"));
        self.compat_call(kind, |vm, h| {
            let (r, d) = re_compat::net::invoke_bindable(vm, h, entity, &args);
            out = r;
            d
        });
        let text = out?;
        let results: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
        Ok(serde_json::json!({ "step": "invoke", "path": inv.path, "results": results }))
    }

    fn play_dump(&self, dump: &crate::play::DumpStep) -> Result<serde_json::Value, Diagnostic> {
        let world = match (&self.compat, dump.client) {
            (Some(c), true) => &c.client,
            (None, true) => {
                return Err(crate::play::step_failed(
                    "a client dump needs a [compat] project",
                ));
            }
            _ => &self.world,
        };
        let start = crate::play::instance_at(world, dump.path.as_deref().unwrap_or(""))?;
        let root = world.id_of(start).unwrap_or_default().to_string();
        let tree = re_compat::dump::dump_tree(world, &crate::play::dump_args(dump, root))?;
        let mut v = serde_json::json!(tree);
        v["step"] = serde_json::json!("dump_tree");
        Ok(v)
    }
}
