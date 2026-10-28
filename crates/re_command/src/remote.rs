//! The engine request channel (ARCHITECTURE §6): a std channel that carries
//! command calls from other threads (the MCP server's tokio thread, the
//! editor socket) to the thread that owns the world, with a reply sender for
//! each [`CommandOutcome`].
//!
//! ```text
//! client thread                       engine thread
//! EngineHandle::call(name, args) ──▶  EngineReceiver::serve_pending(target, registry, policy)
//!             ◀── CommandOutcome ──
//! ```
//!
//! No tokio here: an async caller runs [`EngineHandle::call`] on a blocking
//! task.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

use re_core::Diagnostic;
use serde_json::Value as Json;

use crate::codes;
use crate::history::Origin;
use crate::registry::{CommandOutcome, CommandRegistry, CommandSpec, EditTarget, PolicyCheck};

/// One request to the engine thread.
#[derive(Debug)]
pub enum EngineRequest {
    /// Dispatch a command and send its outcome back.
    Command {
        /// The command's name.
        name: String,
        /// Its arguments (an object, or `null` for none).
        args: Json,
        /// Run it as a dry run.
        dry_run: bool,
        /// Who issued it (written to the log).
        origin: Origin,
        /// Where the outcome goes.
        reply: Sender<CommandOutcome>,
    },
    /// Send back every command's spec.
    ListSpecs {
        /// Where the specs go.
        reply: Sender<Vec<CommandSpec>>,
    },
}

impl EngineRequest {
    /// Answers this request against `target` through `registry` and
    /// `policy`. A caller that has gone away is ignored.
    pub fn handle<C: EditTarget>(
        self,
        target: &mut C,
        registry: &CommandRegistry<C>,
        policy: &dyn PolicyCheck,
    ) {
        match self {
            EngineRequest::Command {
                name,
                args,
                dry_run,
                origin,
                reply,
            } => {
                let outcome = registry.dispatch_as(target, &name, args, dry_run, policy, origin);
                let _ = reply.send(outcome);
            }
            EngineRequest::ListSpecs { reply } => {
                let _ = reply.send(registry.specs().to_vec());
            }
        }
    }
}

/// The client side of the channel. Cheap to clone and `Send`; every clone
/// talks to the same engine thread.
#[derive(Clone, Debug)]
pub struct EngineHandle {
    sender: Sender<EngineRequest>,
    origin: Origin,
}

/// The engine side of the channel, owned by the thread that owns the world.
#[derive(Debug)]
pub struct EngineReceiver {
    receiver: Receiver<EngineRequest>,
}

/// Creates a connected handle and receiver. Calls through the handle carry
/// [`Origin::User`] until [`EngineHandle::with_origin`] changes it.
pub fn channel() -> (EngineHandle, EngineReceiver) {
    let (sender, receiver) = mpsc::channel();
    (
        EngineHandle {
            sender,
            origin: Origin::User,
        },
        EngineReceiver { receiver },
    )
}

fn unavailable() -> Diagnostic {
    Diagnostic::error(
        codes::ENGINE_UNAVAILABLE,
        "the engine is not running (its request channel is closed)",
    )
    .with_hint(
        "start the engine again (`real do` opens the project, or `real edit` runs the editor)",
    )
}

impl EngineHandle {
    /// A handle whose calls carry `origin` (for example `Origin::Agent("mcp")`).
    pub fn with_origin(&self, origin: Origin) -> EngineHandle {
        EngineHandle {
            sender: self.sender.clone(),
            origin,
        }
    }

    /// The origin this handle's calls carry.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Sends a raw request. Fails with `RE2006` if the engine side is gone.
    pub fn send(&self, request: EngineRequest) -> Result<(), Diagnostic> {
        self.sender.send(request).map_err(|_| unavailable())
    }

    /// Dispatches a command on the engine thread and waits for its outcome.
    /// If the engine side is gone, the outcome is an `RE2006` error.
    pub fn call(&self, name: &str, args: Json, dry_run: bool) -> CommandOutcome {
        let (reply, rx) = mpsc::channel();
        let request = EngineRequest::Command {
            name: name.to_string(),
            args,
            dry_run,
            origin: self.origin.clone(),
            reply,
        };
        if let Err(d) = self.send(request) {
            return CommandOutcome::error(d);
        }
        rx.recv()
            .unwrap_or_else(|_| CommandOutcome::error(unavailable()))
    }

    /// Every command's spec, from the engine thread.
    pub fn list_specs(&self) -> Result<Vec<CommandSpec>, Diagnostic> {
        let (reply, rx) = mpsc::channel();
        self.send(EngineRequest::ListSpecs { reply })?;
        rx.recv().map_err(|_| unavailable())
    }
}

impl EngineReceiver {
    /// Waits for the next request; `None` once every handle is dropped.
    pub fn recv(&self) -> Option<EngineRequest> {
        self.receiver.recv().ok()
    }

    /// The next request if one is waiting. `Ok(None)` when none is waiting,
    /// `Err(())` once every handle is dropped.
    #[expect(
        clippy::result_unit_err,
        reason = "the only error is that every handle was dropped"
    )]
    pub fn try_recv(&self) -> Result<Option<EngineRequest>, ()> {
        match self.receiver.try_recv() {
            Ok(r) => Ok(Some(r)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(()),
        }
    }

    /// Waits up to `timeout` for a request. `Ok(None)` on timeout, `Err(())`
    /// once every handle is dropped.
    #[expect(
        clippy::result_unit_err,
        reason = "the only error is that every handle was dropped"
    )]
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<EngineRequest>, ()> {
        match self.receiver.recv_timeout(timeout) {
            Ok(r) => Ok(Some(r)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(()),
        }
    }

    /// Answers every request already waiting, without blocking (what a frame
    /// loop calls between frames). Returns how many were answered, or
    /// `Err(())` once every handle is dropped and nothing is left.
    #[expect(
        clippy::result_unit_err,
        reason = "the only error is that every handle was dropped"
    )]
    pub fn serve_pending<C: EditTarget>(
        &self,
        target: &mut C,
        registry: &CommandRegistry<C>,
        policy: &dyn PolicyCheck,
    ) -> Result<usize, ()> {
        let mut served = 0;
        loop {
            match self.try_recv() {
                Ok(Some(request)) => {
                    request.handle(target, registry, policy);
                    served += 1;
                }
                Ok(None) => return Ok(served),
                Err(()) if served > 0 => return Ok(served),
                Err(()) => return Err(()),
            }
        }
    }

    /// Answers requests until every handle is dropped (a headless engine).
    pub fn serve<C: EditTarget>(
        &self,
        target: &mut C,
        registry: &CommandRegistry<C>,
        policy: &dyn PolicyCheck,
    ) {
        while let Some(request) = self.recv() {
            request.handle(target, registry, policy);
        }
    }
}
