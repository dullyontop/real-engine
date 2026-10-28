//! GPU pass times (ADR-0022's amendment for WP32), where the device has
//! `TIMESTAMP_QUERY`, which the renderer never asks of GL.
//!
//! The frame after [`Renderer::time_next_frame`](crate::Renderer::time_next_frame)
//! is timed: each of its passes, and of the `paint_ui` calls after it, writes
//! a timestamp at its start and its end. The next frame's commands resolve
//! them and read them back with `map_buffer_on_submit` into one of three
//! buffers in turn. Nothing waits for them: a frame whose buffer is still in
//! use is not timed, and the times arrive a frame or two later through
//! [`Renderer::take_gpu_times`](crate::Renderer::take_gpu_times). They are
//! measurements, not checks.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// What a timed pass belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Group {
    /// The shadow cascades.
    Shadow,
    /// The scene pass.
    Scene,
    /// The post chain.
    Post,
    /// The highlights and the on-top world quads.
    Overlay,
    /// The UI over the frame.
    Ui,
}

/// What one frame's passes took on the GPU, from
/// [`Renderer::take_gpu_times`](crate::Renderer::take_gpu_times). A group
/// with no pass in the frame took zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuTimes {
    /// The shadow cascades.
    pub shadow: Duration,
    /// The scene pass.
    pub scene: Duration,
    /// The post chain into the target.
    pub post: Duration,
    /// The highlights and the on-top world quads.
    pub overlay: Duration,
    /// The UI painted over the frame (`Renderer::paint_ui`).
    pub ui: Duration,
    /// All of the frame's timed passes together.
    pub total: Duration,
}

/// The most passes a frame times; later ones are not timed.
const PASSES: u32 = 32;
/// The frames whose times can be in flight at once.
const SLOTS: u32 = 3;
/// The bytes of one slot's timestamps (a multiple of
/// `wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT`).
const SLOT_BYTES: u64 = PASSES as u64 * 2 * wgpu::QUERY_SIZE as u64;
const _: () = assert!(SLOT_BYTES.is_multiple_of(wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT));

// A slot's states.
/// Neither written nor read.
const FREE: u8 = 0;
/// The timed frame's passes write into it.
const OPEN: u8 = 1;
/// Resolved, waiting for its readback to map.
const PENDING: u8 = 2;
/// Mapped: ready to read.
const READY: u8 = 3;
/// The map failed: nothing to read.
const FAILED: u8 = 4;

/// One frame's queries and the buffer they are read back through.
struct Slot {
    readback: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// The group of each pass written so far, in order: pass `n` writes
    /// queries `2n` and `2n + 1` of the slot.
    groups: Mutex<Vec<Group>>,
}

impl Slot {
    fn groups(&self) -> MutexGuard<'_, Vec<Group>> {
        self.groups.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The renderer's pass timer.
pub(crate) struct GpuTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    slots: Vec<Slot>,
    /// The slot the timed frame writes into.
    open: Option<u32>,
    /// The slot the next timed frame takes, when it is free.
    next: u32,
    /// The next frame is timed.
    armed: bool,
    /// Nanoseconds per timestamp tick.
    period: f64,
}

impl GpuTimer {
    /// A timer, when `device` has `TIMESTAMP_QUERY`.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<GpuTimer> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("re_render pass times"),
            ty: wgpu::QueryType::Timestamp,
            count: SLOTS * PASSES * 2,
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("re_render pass times"),
            size: u64::from(SLOTS) * SLOT_BYTES,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let slots = (0..SLOTS)
            .map(|_| Slot {
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("re_render pass times readback"),
                    size: SLOT_BYTES,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(FREE)),
                groups: Mutex::new(Vec::with_capacity(PASSES as usize)),
            })
            .collect();
        Some(GpuTimer {
            queries,
            resolve,
            slots,
            open: None,
            next: 0,
            armed: false,
            period: f64::from(queue.get_timestamp_period()),
        })
    }

    /// Times the next frame.
    pub(crate) fn arm(&mut self) {
        self.armed = true;
    }

    /// Starts a frame's commands in `encoder`: the last timed frame is over,
    /// so its queries are resolved and read back once `encoder` is submitted;
    /// then, when armed and the next slot is free, this frame is timed.
    pub(crate) fn begin_frame(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(n) = self.open.take() {
            let slot = &self.slots[n as usize];
            let passes = slot.groups().len() as u32;
            if passes == 0 {
                slot.state.store(FREE, Ordering::Release);
            } else {
                let first = n * PASSES * 2;
                let offset = u64::from(n) * SLOT_BYTES;
                let bytes = u64::from(passes) * 2 * u64::from(wgpu::QUERY_SIZE);
                encoder.resolve_query_set(
                    &self.queries,
                    first..first + passes * 2,
                    &self.resolve,
                    offset,
                );
                encoder.copy_buffer_to_buffer(&self.resolve, offset, &slot.readback, 0, bytes);
                slot.state.store(PENDING, Ordering::Release);
                let state = slot.state.clone();
                encoder.map_buffer_on_submit(
                    &slot.readback,
                    wgpu::MapMode::Read,
                    ..bytes,
                    move |result| {
                        state.store(
                            if result.is_ok() { READY } else { FAILED },
                            Ordering::Release,
                        );
                    },
                );
            }
        }
        if std::mem::take(&mut self.armed) {
            let slot = &self.slots[self.next as usize];
            if slot.state.load(Ordering::Acquire) == FREE {
                slot.groups().clear();
                slot.state.store(OPEN, Ordering::Release);
                self.open = Some(self.next);
                self.next = (self.next + 1) % SLOTS;
            }
        }
    }

    /// The timestamp writes of a pass of `group`, when the frame is timed and
    /// has a query pair left.
    pub(crate) fn writes(&self, group: Group) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let n = self.open?;
        let mut groups = self.slots[n as usize].groups();
        let pass = groups.len() as u32;
        if pass == PASSES {
            return None;
        }
        groups.push(group);
        let first = n * PASSES * 2 + pass * 2;
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.queries,
            beginning_of_pass_write_index: Some(first),
            end_of_pass_write_index: Some(first + 1),
        })
    }

    /// The times read back since the last call, oldest first.
    pub(crate) fn take(&mut self) -> Vec<GpuTimes> {
        let mut out = Vec::new();
        // From the oldest slot in turn: the one after the last taken.
        for k in 0..SLOTS {
            let n = (self.next + k) % SLOTS;
            let slot = &self.slots[n as usize];
            match slot.state.load(Ordering::Acquire) {
                READY => {
                    let groups = slot.groups();
                    let bytes = groups.len() as u64 * 2 * u64::from(wgpu::QUERY_SIZE);
                    if let Ok(view) = slot.readback.get_mapped_range(..bytes) {
                        // Read as bytes: a mapped range need not be aligned for u64.
                        let ticks: Vec<u64> = view
                            .as_chunks::<8>()
                            .0
                            .iter()
                            .map(|b| u64::from_le_bytes(*b))
                            .collect();
                        out.push(times(self.period, &groups, &ticks));
                    }
                    drop(groups);
                    slot.readback.unmap();
                    slot.state.store(FREE, Ordering::Release);
                }
                FAILED => slot.state.store(FREE, Ordering::Release),
                _ => {}
            }
        }
        out
    }
}

/// The times of passes of `groups`, with start and end `ticks` of `period`
/// nanoseconds.
fn times(period: f64, groups: &[Group], ticks: &[u64]) -> GpuTimes {
    let mut times = GpuTimes::default();
    for (group, [start, end]) in groups.iter().zip(ticks.as_chunks::<2>().0) {
        // A clock that steps back between two queries gives zero.
        let ns = end.saturating_sub(*start) as f64 * period;
        let took = Duration::from_nanos(ns as u64);
        times.total += took;
        *match group {
            Group::Shadow => &mut times.shadow,
            Group::Scene => &mut times.scene,
            Group::Post => &mut times.post,
            Group::Overlay => &mut times.overlay,
            Group::Ui => &mut times.ui,
        } += took;
    }
    times
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_times_add_up_by_group() {
        let groups = [Group::Shadow, Group::Shadow, Group::Scene, Group::Ui];
        let ticks = [10, 20, 30, 35, 40, 100, 200, 190];
        let t = times(2.0, &groups, &ticks);
        assert_eq!(t.shadow, Duration::from_nanos(30));
        assert_eq!(t.scene, Duration::from_nanos(120));
        assert_eq!(t.ui, Duration::ZERO, "a clock that steps back gives zero");
        assert_eq!((t.post, t.overlay), (Duration::ZERO, Duration::ZERO));
        assert_eq!(t.total, Duration::from_nanos(150));
    }
}
