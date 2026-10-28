//! `re_render`: Real Engine's renderer (ADR-0010).
//!
//! A forward PBR-lite renderer on wgpu 30 that draws a
//! [`RenderSnapshot`](re_types::render::RenderSnapshot), paints egui, and captures
//! images headless. The same code draws into a window surface and into an
//! offscreen [`Capture`]: every entry point takes a `&wgpu::TextureView`.
//!
//! * [`Gpu`]: instance, adapter, device and queue, headless or for a window, with
//!   GPU errors logged as `RE6001` instead of panicking.
//! * [`Renderer`]: uploads meshes and textures and creates materials behind
//!   generation-checked handles, draws snapshots (one shadowed sun, up to eight
//!   point lights, ambient, MSAA where the adapter allows it, debug lines, world
//!   quads) into an HDR target and runs the post chain of the snapshot's
//!   [`PostSettings`](re_types::render::PostSettings) (sun rays, bloom, blur,
//!   colour grading, tone mapping; ADR-0038), and paints egui ([`UiFrame`]).
//!   [`Renderer::render_with`] adds [`SceneExtras`]: fog, a sky, spot lights,
//!   sprites, ribbons and highlights. [`Renderer::set_shader_source`] and
//!   [`Renderer::reload_shaders`] swap shader files at run time ([`shader`]).
//! * [`Capture`] and [`Image`]: offscreen targets, readback and PNG files.
//! * [`imgdiff`]: golden-image comparison with a per-channel tolerance and an
//!   outlier budget.
//!
//! ```no_run
//! use re_render::{Capture, Gpu, Renderer};
//! use re_types::render::RenderSnapshot;
//!
//! let gpu = Gpu::headless(re_render::backends_from_env()?)?;
//! let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
//! let capture = Capture::new(&gpu.device, 320, 240)?;
//! renderer.render(&gpu.device, &gpu.queue, capture.view(), &RenderSnapshot::default())?;
//! capture.save_png(&gpu.device, &gpu.queue, std::path::Path::new("out/frame.png"))?;
//! # Ok::<(), re_core::Diagnostic>(())
//! ```
//!
//! Diagnostic codes owned by this crate lie in `RE6000`–`RE6999`; each has a page in
//! `explain/` and an entry in [`DIAGNOSTICS`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod capture;
pub mod codes;
pub mod extras;
mod frame;
pub mod gpu;
pub mod imgdiff;
mod post;
mod renderer;
pub mod shader;
mod slots;
mod timing;
mod ui;

pub use capture::{Capture, Image, ImageStats};
pub use extras::{
    Ambient, Fog, Highlight, MAX_HIGHLIGHTS, MAX_SPOT_LIGHTS, Ribbon, RibbonPoint, SceneExtras,
    Sky, SpotLight, Sprite, SpriteShape,
};
pub use frame::MAX_POINT_LIGHTS;
pub use gpu::{Gpu, GpuErrors, backends_from_env, parse_backends};
pub use renderer::{
    DrawTime, FrameReport, FrameStats, RenderStats, Renderer, RendererSettings, ResourceStats,
    ShaderReload, ShadowProbe,
};
pub use timing::GpuTimes;
pub use ui::UiFrame;

/// Re-exported so callers name the same wgpu and egui versions as the renderer.
pub use {egui, wgpu};

use re_core::DiagInfo;

macro_rules! diagnostics {
    ($($code:ident, $title:literal, $page:literal;)*) => {
        /// Every diagnostic code `re_render` owns, with its title and explain page.
        ///
        /// `re_cli` collects this list (with those of the other crates) for `real explain`.
        pub const DIAGNOSTICS: &[DiagInfo] = &[
            $(DiagInfo { code: codes::$code, title: $title, page: include_str!($page) },)*
        ];
    };
}

diagnostics! {
    GPU_ERROR, "Uncaptured GPU error", "../explain/RE6001.md";
    NO_ADAPTER, "No GPU adapter", "../explain/RE6002.md";
    DEVICE_FAILED, "The GPU device could not be created", "../explain/RE6003.md";
    SURFACE_FAILED, "The window surface could not be created", "../explain/RE6004.md";
    INVALID_TARGET, "Invalid render target", "../explain/RE6005.md";
    UNKNOWN_BACKEND, "Unknown backend in RE_BACKENDS", "../explain/RE6006.md";
    INVALID_CAMERA, "Invalid camera", "../explain/RE6007.md";
    STALE_MESH, "Stale mesh handle", "../explain/RE6010.md";
    STALE_TEXTURE, "Stale texture handle", "../explain/RE6011.md";
    STALE_MATERIAL, "Stale material handle", "../explain/RE6012.md";
    UNRESOLVED_TEXTURE, "Material texture not uploaded", "../explain/RE6013.md";
    BAD_MESH, "Malformed mesh data", "../explain/RE6014.md";
    BAD_TEXTURE, "Malformed texture data", "../explain/RE6015.md";
    BAD_MATERIAL, "Invalid material values", "../explain/RE6016.md";
    TOO_MANY_LIGHTS, "Too many lights", "../explain/RE6017.md";
    TOO_MANY_HIGHLIGHTS, "Too many highlights", "../explain/RE6018.md";
    READBACK_FAILED, "GPU readback failed", "../explain/RE6020.md";
    IMAGE_WRITE_FAILED, "Image file could not be written", "../explain/RE6021.md";
    IMAGE_READ_FAILED, "Image file could not be read", "../explain/RE6022.md";
    IMAGE_SIZE_MISMATCH, "Compared images differ in size", "../explain/RE6023.md";
    SHADER_INCLUDE, "Shader include error", "../explain/RE6030.md";
    SHADER_COMPILE, "Shader does not compile", "../explain/RE6031.md";
    SHADER_REJECTED, "Shader rejected by the GPU", "../explain/RE6032.md";
    SHADER_REPLACED, "Project replaces an engine shader", "../explain/RE6033.md";
    SKINNING_UNSUPPORTED, "GPU skinning unsupported", "../explain/RE6040.md";
    BAD_SKINNED_ITEM, "Undrawable skinned item", "../explain/RE6041.md";
}

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 6000..=6999;
