//! The phases of a frame, in the order they run (ADR-0009).
//!
//! ```text
//! Input → PreRender → (PreAnimation → PreSimulation → Physics → PostSimulation) × 0..=5
//!       → LateFrame → Sync → Render → EndFrame
//! ```
//!
//! Systems register against phases, not against each other. The order matches the
//! one Roblox-style scripts expect: render-step work before simulation, "stepped"
//! before physics and "heartbeat" after it.

use std::fmt;

use serde::{Deserialize, Serialize};

/// One phase of a frame. The derived ordering is the execution order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Phase {
    /// Poll window and gamepad events (or the replay), update the action map.
    Input,
    /// Variable-step script callbacks, once per frame.
    PreRender,
    /// Fixed step: animation clips and tweens write transforms.
    PreAnimation,
    /// Fixed step: scripts' fixed update; commands queued by scripts.
    PreSimulation,
    /// Fixed step: apply the sorted physics commands, step physics, pull poses and events.
    Physics,
    /// Fixed step: contact callbacks, then scripts' post-physics update.
    PostSimulation,
    /// Once per frame after the last fixed step: the poses drawn this frame
    /// (interpolated by the frame's alpha), then scripts' late-frame work, such
    /// as cameras that follow what is drawn (ADR-0057 §7).
    LateFrame,
    /// Write poses back, propagate transforms, build the render snapshot and audio commands.
    Sync,
    /// Draw the snapshot and the UI overlay.
    Render,
    /// Flush the command journal, release unreferenced GPU resources, record performance.
    EndFrame,
}

impl Phase {
    /// Every phase, in execution order.
    pub const ALL: [Phase; 10] = [
        Phase::Input,
        Phase::PreRender,
        Phase::PreAnimation,
        Phase::PreSimulation,
        Phase::Physics,
        Phase::PostSimulation,
        Phase::LateFrame,
        Phase::Sync,
        Phase::Render,
        Phase::EndFrame,
    ];

    /// The phases that run once per fixed step, in order.
    pub const FIXED: [Phase; 4] = [
        Phase::PreAnimation,
        Phase::PreSimulation,
        Phase::Physics,
        Phase::PostSimulation,
    ];

    /// True for the phases that run once per fixed step (zero to five times a frame).
    pub const fn is_fixed_step(self) -> bool {
        matches!(
            self,
            Phase::PreAnimation | Phase::PreSimulation | Phase::Physics | Phase::PostSimulation
        )
    }

    /// The phase's name as written in logs and performance reports (`PreSimulation`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Phase::Input => "Input",
            Phase::PreRender => "PreRender",
            Phase::PreAnimation => "PreAnimation",
            Phase::PreSimulation => "PreSimulation",
            Phase::Physics => "Physics",
            Phase::PostSimulation => "PostSimulation",
            Phase::LateFrame => "LateFrame",
            Phase::Sync => "Sync",
            Phase::Render => "Render",
            Phase::EndFrame => "EndFrame",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
