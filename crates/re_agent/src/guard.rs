//! A policy check on the client side of an [`EngineHandle`].
//!
//! The engine thread checks every call against the policy it was given. When
//! an agent reaches the engine through a handle whose engine applies a
//! different policy (the editor's, through the editor socket), [`guarded`]
//! puts the agent policy in front: refused calls never leave this process.

use std::sync::Arc;

use re_command::{
    CommandOutcome, CommandSpec, EngineHandle, EngineReceiver, EngineRequest, Origin, PolicyCheck,
    channel,
};
use re_core::{Diagnostic, Location};
use serde_json::Value as Json;

/// A handle that checks every call against `policy` before passing it to
/// `inner`, and gives every call `origin`. Calls are passed on one at a time.
pub fn guarded(
    inner: EngineHandle,
    policy: Arc<dyn PolicyCheck + Send + Sync>,
    origin: Origin,
) -> EngineHandle {
    let (handle, receiver) = channel();
    std::thread::Builder::new()
        .name("re-agent-guard".to_string())
        .spawn(move || run(receiver, inner, policy.as_ref()))
        .expect("the guard thread starts");
    handle.with_origin(origin)
}

fn run(receiver: EngineReceiver, inner: EngineHandle, policy: &dyn PolicyCheck) {
    let mut specs: Option<Vec<CommandSpec>> = None;
    while let Some(request) = receiver.recv() {
        match request {
            EngineRequest::ListSpecs { reply } => {
                if let Ok(list) = inner.list_specs() {
                    specs = Some(list.clone());
                    let _ = reply.send(list);
                }
            }
            EngineRequest::Command {
                name,
                args,
                dry_run,
                origin,
                reply,
            } => {
                let known = |specs: &Option<Vec<CommandSpec>>| {
                    specs
                        .as_deref()
                        .is_some_and(|all| all.iter().any(|s| s.name == name))
                };
                if !known(&specs) {
                    // A new or unlisted command: refresh once before refusing.
                    match inner.list_specs() {
                        Ok(list) => specs = Some(list),
                        Err(d) => {
                            let _ = reply.send(CommandOutcome::error(d));
                            continue;
                        }
                    }
                }
                let Some(spec) = specs
                    .as_deref()
                    .and_then(|all| all.iter().find(|s| s.name == name))
                else {
                    // Nothing the policy can check goes through.
                    let _ = reply.send(CommandOutcome::error(
                        Diagnostic::error(
                            re_command::codes::UNKNOWN_COMMAND,
                            format!("unknown command {name:?}"),
                        )
                        .at(Location::command(name.clone()))
                        .with_hint("`describe` lists the commands"),
                    ));
                    continue;
                };
                let dry = dry_run || args.get("dry_run").and_then(Json::as_bool) == Some(true);
                let mut checked = args.clone();
                if let Some(obj) = checked.as_object_mut() {
                    obj.remove("dry_run");
                }
                if let Err(d) = policy.check(spec, &checked, dry) {
                    let _ = reply.send(CommandOutcome::error(d));
                    continue;
                }
                let outcome = inner.with_origin(origin).call(&name, args, dry_run);
                let _ = reply.send(outcome);
            }
        }
    }
}
