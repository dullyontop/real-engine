//! Real Engine rendering spike: wgpu 30 + winit 0.30 + egui 0.36.
//!
//! Library part shared by the binaries (`probe`, `headless`, `windowed`, `gui_offscreen`)
//! and the integration tests. Everything here runs without a window or display.

pub mod checks;
pub mod gpu;
pub mod gui;
pub mod readback;
pub mod scene;
pub mod timing;

use std::fmt;

/// Error type for the spike. Wraps the wgpu request errors so that their exact
/// `Display` text can be reported.
#[derive(Debug)]
pub enum SpikeError {
    Adapter(wgpu::RequestAdapterError),
    Device(wgpu::RequestDeviceError),
    Poll(wgpu::PollError),
    Map(wgpu::BufferAsyncError),
    MapRange(wgpu::MapRangeError),
    Png(png::EncodingError),
    Io(std::io::Error),
    Validation(String),
    Other(String),
}

impl fmt::Display for SpikeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpikeError::Adapter(e) => write!(f, "request_adapter failed: {e}"),
            SpikeError::Device(e) => write!(f, "request_device failed: {e}"),
            SpikeError::Poll(e) => write!(f, "device.poll failed: {e}"),
            SpikeError::Map(e) => write!(f, "map_async failed: {e}"),
            SpikeError::MapRange(e) => write!(f, "get_mapped_range failed: {e}"),
            SpikeError::Png(e) => write!(f, "png encoding failed: {e}"),
            SpikeError::Io(e) => write!(f, "io error: {e}"),
            SpikeError::Validation(e) => write!(f, "wgpu validation error: {e}"),
            SpikeError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SpikeError {}

impl From<wgpu::RequestAdapterError> for SpikeError {
    fn from(e: wgpu::RequestAdapterError) -> Self {
        SpikeError::Adapter(e)
    }
}
impl From<wgpu::RequestDeviceError> for SpikeError {
    fn from(e: wgpu::RequestDeviceError) -> Self {
        SpikeError::Device(e)
    }
}
impl From<wgpu::PollError> for SpikeError {
    fn from(e: wgpu::PollError) -> Self {
        SpikeError::Poll(e)
    }
}
impl From<wgpu::BufferAsyncError> for SpikeError {
    fn from(e: wgpu::BufferAsyncError) -> Self {
        SpikeError::Map(e)
    }
}
impl From<wgpu::MapRangeError> for SpikeError {
    fn from(e: wgpu::MapRangeError) -> Self {
        SpikeError::MapRange(e)
    }
}
impl From<png::EncodingError> for SpikeError {
    fn from(e: png::EncodingError) -> Self {
        SpikeError::Png(e)
    }
}
impl From<std::io::Error> for SpikeError {
    fn from(e: std::io::Error) -> Self {
        SpikeError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, SpikeError>;

/// Parse a backend name used on the command line of the spike binaries.
pub fn parse_backends(s: &str) -> Option<wgpu::Backends> {
    match s.to_ascii_lowercase().as_str() {
        "vulkan" | "vk" => Some(wgpu::Backends::VULKAN),
        "gl" | "gles" | "opengl" => Some(wgpu::Backends::GL),
        "primary" => Some(wgpu::Backends::PRIMARY),
        "all" => Some(wgpu::Backends::all()),
        "noop" => Some(wgpu::Backends::NOOP),
        _ => None,
    }
}
