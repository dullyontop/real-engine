//! World streaming (ADR-0042) in the frame loop: the streaming step, the
//! floating origin physics follows, and the terrain view a screenshot waits
//! for.

use super::App;

impl App {
    /// True if the world streams: it has streamed sources, or its manager
    /// still holds something.
    fn streams(&mut self) -> bool {
        self.world
            .ecs
            .get_resource::<re_proc::stream::Streamer>()
            .is_some_and(|s| !s.is_idle())
            || self
                .world
                .ecs
                .query_filtered::<(), re_scene::ecs::query::With<re_scene::Streamed>>()
                .iter(&self.world.ecs)
                .next()
                .is_some()
    }

    /// Before a screenshot under [`Lateness::Wait`]: completes the streamed
    /// terrain view ([`re_proc::stream::complete_view`]), so the image shows
    /// the terrain whole, however fast its chunks were made. A live run
    /// ([`Lateness::Late`]) never waits, and a world that has not streamed
    /// yet (a scene being edited) has no view to complete. Nor does the
    /// browser, which has no worker threads (every chunk would be made on
    /// the page's thread) and reads no screenshot back.
    ///
    /// [`Lateness::Wait`]: re_proc::stream::Lateness::Wait
    /// [`Lateness::Late`]: re_proc::stream::Lateness::Late
    pub(super) fn complete_terrain_view(&mut self) {
        if !cfg!(target_arch = "wasm32")
            && self.stream_lateness == re_proc::stream::Lateness::Wait
            && self.stream_tick > 0
            && self.streams()
        {
            let problems = re_proc::stream::complete_view(&mut self.world);
            self.diagnostics.extend(problems);
        }
    }

    /// World streaming (ADR-0042): loads and unloads the cells of `Streamed`
    /// procedural sources around the focuses, before scripts' fixed update
    /// sees this tick's stream events. A world with no streamed source pays a
    /// query.
    pub(super) fn stream_step(&mut self) {
        if self.streams() {
            if let Some(mut s) = self
                .world
                .ecs
                .get_resource_mut::<re_proc::stream::Streamer>()
                && s.lateness() != self.stream_lateness
            {
                s.set_lateness(self.stream_lateness);
            }
            // The terrain view is only made for an app that draws.
            if let Some(mut s) = self
                .world
                .ecs
                .get_resource_mut::<re_proc::stream::Streamer>()
                && s.view() != self.graphics.is_some()
            {
                s.set_view(self.graphics.is_some());
            }
            let report =
                re_proc::stream::step(&mut self.world, self.stream_tick, &self.config.stream);
            self.diagnostics.extend(report.diagnostics);
            // A streamer that could not be made has no origin to follow.
            if self
                .world
                .ecs
                .get_resource::<re_proc::stream::Streamer>()
                .is_some()
            {
                self.follow_origin(report.origin);
            }
        }
        self.stream_tick += 1;
    }

    /// Moves physics' simulation origin to world streaming's floating origin
    /// (ADR-0042), between steps and before the tick's physics commands, so
    /// bodies and characters kilometres out stay within Jolt's reach. A move
    /// physics refuses (`RE5016`: something would end up out of reach) is
    /// reported once per origin and tried again each tick.
    fn follow_origin(&mut self, origin: glam::Vec3) {
        if self.physics.origin() == origin {
            return;
        }
        match self.physics.set_origin(origin) {
            Ok(()) => self.origin_refused = None,
            Err(d) => {
                if self.origin_refused != Some(origin) {
                    self.origin_refused = Some(origin);
                    self.diagnostics.push(d);
                }
            }
        }
    }
}
