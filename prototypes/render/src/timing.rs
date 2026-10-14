//! GPU timestamp queries around a render pass (when `Features::TIMESTAMP_QUERY` is
//! available) and simple wall-clock statistics.

use std::time::Duration;

use crate::Result;

pub struct GpuPassTimer {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    period_ns: f32,
}

impl GpuPassTimer {
    /// `None` when the device was created without `Features::TIMESTAMP_QUERY`.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("pass-timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let size = 2 * u64::from(wgpu::QUERY_SIZE);
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("timestamp-resolve"),
            size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("timestamp-readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Some(Self {
            query_set,
            resolve_buffer,
            readback_buffer,
            // Nanoseconds per timestamp tick.
            period_ns: queue.get_timestamp_period(),
        })
    }

    pub fn period_ns(&self) -> f32 {
        self.period_ns
    }

    pub fn pass_writes(&self) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: Some(1),
        }
    }

    /// Call after the timed pass has ended, on the same encoder.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.resolve_query_set(&self.query_set, 0..2, &self.resolve_buffer, 0);
        encoder.copy_buffer_to_buffer(&self.resolve_buffer, 0, &self.readback_buffer, 0, None);
    }

    /// Map the readback buffer and return the pass duration. Call after submitting.
    pub fn read(&self, device: &wgpu::Device) -> Result<Duration> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.readback_buffer
            .map_async(wgpu::MapMode::Read, .., move |r| {
                let _ = tx.send(r);
            });
        device.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()
            .map_err(|e| crate::SpikeError::Other(format!("timestamp map callback lost: {e}")))??;
        let ticks: [u64; 2] = {
            let view = self.readback_buffer.get_mapped_range(..)?;
            let t: &[u64] = bytemuck::cast_slice(&view);
            [t[0], t[1]]
        };
        self.readback_buffer.unmap();
        let ns = (ticks[1].wrapping_sub(ticks[0])) as f64 * f64::from(self.period_ns);
        Ok(Duration::from_nanos(ns as u64))
    }
}

/// min / median / mean / max of a set of durations, in milliseconds.
pub fn summarize_ms(samples: &[Duration]) -> String {
    if samples.is_empty() {
        return "no samples".into();
    }
    let mut v: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e3).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let median = v[v.len() / 2];
    format!(
        "n={} min={:.3}ms median={:.3}ms mean={:.3}ms max={:.3}ms",
        v.len(),
        v[0],
        median,
        mean,
        v[v.len() - 1]
    )
}
