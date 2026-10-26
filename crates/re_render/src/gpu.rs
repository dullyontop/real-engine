//! The GPU context: instance, adapter, device and queue, created headless or for a
//! window surface (ADR-0010).
//!
//! Both constructors install an uncaptured-error handler. wgpu's default handler
//! panics; ours logs each error as an `RE6001` diagnostic through `tracing` and
//! keeps the most recent ones in a [`GpuErrors`] log, so a validation error never
//! takes the process down.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use re_core::Diagnostic;

use crate::codes;

/// How many recent GPU errors [`GpuErrors`] keeps.
const ERROR_LOG_CAPACITY: usize = 64;

/// The environment variable that selects the backends for [`backends_from_env`].
pub const BACKENDS_ENV: &str = "RE_BACKENDS";

/// Parses a backend name: `vulkan` (or `vk`), `gl` (or `gles`, `opengl`), `metal`,
/// `dx12`, `primary` or `all`. Case does not matter.
pub fn parse_backends(name: &str) -> Option<wgpu::Backends> {
    match name.trim().to_ascii_lowercase().as_str() {
        "vulkan" | "vk" => Some(wgpu::Backends::VULKAN),
        "gl" | "gles" | "opengl" => Some(wgpu::Backends::GL),
        "metal" => Some(wgpu::Backends::METAL),
        "dx12" => Some(wgpu::Backends::DX12),
        "primary" => Some(wgpu::Backends::PRIMARY),
        "all" => Some(wgpu::Backends::all()),
        _ => None,
    }
}

/// The backends named by `RE_BACKENDS`, or [`wgpu::Backends::PRIMARY`] (Vulkan on
/// Linux, Metal on macOS, DX12 or Vulkan on Windows) when it is unset or empty.
pub fn backends_from_env() -> Result<wgpu::Backends, Diagnostic> {
    match std::env::var(BACKENDS_ENV) {
        Ok(value) if !value.trim().is_empty() => parse_backends(&value).ok_or_else(|| {
            Diagnostic::error(
                codes::UNKNOWN_BACKEND,
                format!("unknown backend `{value}` in {BACKENDS_ENV}"),
            )
            .with_hint("use vulkan, gl, metal, dx12, primary or all")
        }),
        _ => Ok(wgpu::Backends::PRIMARY),
    }
}

/// The limits to request: the WebGPU defaults when the adapter meets them,
/// otherwise the downlevel defaults (some GL drivers), or WebGL2's when even those do not
/// fit, always with the adapter's largest texture size.
pub fn pick_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let supported = adapter.limits();
    let wanted = wgpu::Limits::default();
    let base = if wanted.check_limits(&supported) {
        wanted
    } else if wgpu::Limits::downlevel_defaults().check_limits(&supported) {
        wgpu::Limits::downlevel_defaults()
    } else {
        // WebGL2 (the browser fallback, ADR-0049 §8) has no compute and fewer bindings.
        wgpu::Limits::downlevel_webgl2_defaults()
    };
    base.using_resolution(supported)
}

/// An instance that is not connected to a display server, for offscreen work.
pub fn headless_instance(backends: wgpu::Backends) -> wgpu::Instance {
    wgpu::Instance::new(instance_options(
        wgpu::InstanceDescriptor::new_without_display_handle(),
        backends,
    ))
}

/// `desc` with what every instance sets: `backends`, the debug flags the build
/// and the environment ask for, and FXC as the DX12 shader compiler. FXC is
/// part of Windows, so shaders compile the same way on every machine; wgpu's
/// default (`Auto`) would load a `dxcompiler.dll` from the DLL search path
/// when there is one (ADR-0038).
fn instance_options(
    mut desc: wgpu::InstanceDescriptor,
    backends: wgpu::Backends,
) -> wgpu::InstanceDescriptor {
    desc.backends = backends;
    desc.flags = wgpu::InstanceFlags::from_build_config().with_env();
    desc.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::Fxc;
    desc
}

/// The GPU errors reported through the uncaptured-error handler.
///
/// Cloning shares the same log. Every error is also logged with `tracing::error!`
/// when it happens.
#[derive(Clone, Default)]
pub struct GpuErrors {
    inner: Arc<ErrorLog>,
}

#[derive(Default)]
struct ErrorLog {
    total: AtomicU64,
    recent: Mutex<VecDeque<Diagnostic>>,
}

impl GpuErrors {
    /// How many errors were reported since the device was created.
    pub fn count(&self) -> u64 {
        self.inner.total.load(Ordering::Relaxed)
    }

    /// Removes and returns the recent errors (at most the last 64), oldest first.
    pub fn take(&self) -> Vec<Diagnostic> {
        let mut recent = self
            .inner
            .recent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        recent.drain(..).collect()
    }

    /// Converts a wgpu error to an `RE6001` diagnostic, logs it and records it.
    pub fn report(&self, error: &wgpu::Error) {
        let kind = match error {
            wgpu::Error::OutOfMemory { .. } => "out of memory",
            wgpu::Error::Validation { .. } => "validation error",
            wgpu::Error::Internal { .. } => "internal error",
        };
        let diagnostic = Diagnostic::error(codes::GPU_ERROR, format!("GPU {kind}: {error}"))
            .with_hint("this is a bug in the caller or the renderer; the frame may be incomplete");
        tracing::error!(code = %codes::GPU_ERROR, "{}", diagnostic.message);
        self.inner.total.fetch_add(1, Ordering::Relaxed);
        let mut recent = self
            .inner
            .recent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if recent.len() == ERROR_LOG_CAPACITY {
            recent.pop_front();
        }
        recent.push_back(diagnostic);
    }
}

impl fmt::Debug for GpuErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuErrors")
            .field("count", &self.count())
            .finish()
    }
}

/// Installs the uncaptured-error handler on `device` and returns its log.
pub fn install_error_logger(device: &wgpu::Device) -> GpuErrors {
    let errors = GpuErrors::default();
    let sink = errors.clone();
    device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| sink.report(&error)));
    errors
}

/// An instance, adapter, device and queue.
///
/// The wgpu handles are reference-counted, so cloning them out of a `Gpu` is cheap.
#[derive(Debug)]
pub struct Gpu {
    /// The instance.
    pub instance: wgpu::Instance,
    /// The adapter the device was created on.
    pub adapter: wgpu::Adapter,
    /// The device.
    pub device: wgpu::Device,
    /// The device's queue.
    pub queue: wgpu::Queue,
    errors: GpuErrors,
}

impl Gpu {
    /// A device with no surface, for offscreen rendering, tests and CI.
    ///
    /// Fails with `RE6002` when no adapter matches `backends` and `RE6003` when the
    /// adapter refuses the device. It blocks until the device exists, which a browser
    /// never allows: use [`Gpu::headless_async`] there.
    pub fn headless(backends: wgpu::Backends) -> Result<Gpu, Diagnostic> {
        pollster::block_on(Self::headless_async(backends))
    }

    /// [`Gpu::headless`] without blocking, for the browser, where the adapter and the
    /// device arrive only after control returns to the page's event loop (ADR-0049 §3).
    pub async fn headless_async(backends: wgpu::Backends) -> Result<Gpu, Diagnostic> {
        Self::from_instance(headless_instance(backends), None, backends).await
    }

    /// A device that can present to `window`, and the window's surface.
    ///
    /// `window` is usually an `Arc<winit::window::Window>`. The instance keeps a
    /// clone of it as its display handle (needed by GL on Wayland), and the surface
    /// keeps another, so both outlive any other reference to the window.
    ///
    /// Configure the surface with [`Gpu::surface_config`]. It blocks like
    /// [`Gpu::headless`]; use [`Gpu::for_surface_async`] in a browser.
    pub fn for_surface<W>(
        window: Arc<W>,
        backends: wgpu::Backends,
    ) -> Result<(Gpu, wgpu::Surface<'static>), Diagnostic>
    where
        W: wgpu::rwh::HasWindowHandle
            + wgpu::rwh::HasDisplayHandle
            + fmt::Debug
            + Send
            + Sync
            + 'static,
    {
        pollster::block_on(Self::for_surface_async(window, backends))
    }

    /// [`Gpu::for_surface`] without blocking, for the browser.
    pub async fn for_surface_async<W>(
        window: Arc<W>,
        backends: wgpu::Backends,
    ) -> Result<(Gpu, wgpu::Surface<'static>), Diagnostic>
    where
        W: wgpu::rwh::HasWindowHandle
            + wgpu::rwh::HasDisplayHandle
            + fmt::Debug
            + Send
            + Sync
            + 'static,
    {
        let instance = wgpu::Instance::new(instance_options(
            wgpu::InstanceDescriptor::new_with_display_handle(Box::new(window.clone())),
            backends,
        ));
        let surface = instance.create_surface(window).map_err(|e| {
            Diagnostic::error(
                codes::SURFACE_FAILED,
                format!("could not create a window surface: {e}"),
            )
        })?;
        let gpu = Self::from_instance(instance, Some(&surface), backends).await?;
        Ok((gpu, surface))
    }

    async fn from_instance(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        backends: wgpu::Backends,
    ) -> Result<Gpu, Diagnostic> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: surface,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| {
                Diagnostic::error(
                    codes::NO_ADAPTER,
                    format!("no GPU adapter for backends {backends:?}: {e}"),
                )
                .with_hint(
                    "install a Vulkan or GL driver (Mesa's lavapipe or llvmpipe work without a GPU), \
                     or choose other backends with RE_BACKENDS",
                )
            })?;
        let info = adapter.get_info();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("re_render"),
                // Block-compressed textures upload as they are where the
                // adapter has them, and are decoded otherwise (ADR-0047).
                required_features: adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC,
                required_limits: pick_limits(&adapter),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| {
                Diagnostic::error(
                    codes::DEVICE_FAILED,
                    format!(
                        "the adapter `{}` refused to create a device: {e}",
                        info.name
                    ),
                )
            })?;
        let errors = install_error_logger(&device);
        tracing::info!(
            adapter = %info.name,
            backend = ?info.backend,
            driver = %info.driver_info,
            "GPU device created"
        );
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            errors,
        })
    }

    /// The errors reported by the uncaptured-error handler.
    pub fn errors(&self) -> &GpuErrors {
        &self.errors
    }

    /// The adapter's description.
    pub fn info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }

    /// The backend the device runs on.
    pub fn backend(&self) -> wgpu::Backend {
        self.adapter.get_info().backend
    }

    /// True when textures may be viewed with another format (absent on GL).
    pub fn supports_view_formats(&self) -> bool {
        self.downlevel_flags()
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS)
    }

    /// True when depth textures can be copied to buffers (absent on GL).
    pub fn supports_depth_copies(&self) -> bool {
        self.downlevel_flags()
            .contains(wgpu::DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES)
    }

    fn downlevel_flags(&self) -> wgpu::DownlevelFlags {
        self.adapter.get_downlevel_capabilities().flags
    }

    /// One line describing the adapter: name, backend, type and driver.
    pub fn adapter_line(&self) -> String {
        let info = self.info();
        format!(
            "{} ({:?}, {:?}, {})",
            info.name, info.backend, info.device_type, info.driver_info
        )
    }

    /// A configuration for `surface` at `width` x `height`: the first sRGB format
    /// it offers (the renderer writes linear colour), Fifo presentation, and
    /// `RENDER_ATTACHMENT` usage. Fails with `RE6004` when the surface offers no
    /// format for this adapter.
    pub fn surface_config(
        &self,
        surface: &wgpu::Surface<'_>,
        width: u32,
        height: u32,
    ) -> Result<wgpu::SurfaceConfiguration, Diagnostic> {
        let caps = surface.get_capabilities(&self.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| {
                Diagnostic::error(
                    codes::SURFACE_FAILED,
                    "the window surface supports no format on this adapter",
                )
            })?;
        let alpha_mode = caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        Ok(wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_instance_compiles_dx12_shaders_with_fxc() {
        let desc = instance_options(
            wgpu::InstanceDescriptor::new_without_display_handle(),
            wgpu::Backends::all(),
        );
        assert_eq!(desc.backends, wgpu::Backends::all());
        assert!(matches!(
            desc.backend_options.dx12.shader_compiler,
            wgpu::Dx12Compiler::Fxc
        ));
    }
}
