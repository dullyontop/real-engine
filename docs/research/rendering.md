# Rendering, windowing and editor UI: spike notes

These are the technical notes from the Real Engine rendering spike. Its code is in [`prototypes/render/`](../../prototypes/render/), a standalone Cargo project with its own Cargo.lock; the [README](../../prototypes/render/README.md) there says how to build it and run its tests.

- Date: 2026-10-14
- Code: `prototypes/render/`
  - `src/`, `tests/` and `scripts/` hold the code, and Cargo.lock pins every dependency.
  - The spike's run logs, PNG captures and copies of the upstream files it read are not in this repository. Measurements and image descriptions below come from those runs; the binaries regenerate the images (§11).
- Test machine:
  - Linux, no GPU, 4 CPUs, 15 GB RAM.
  - rustc and cargo 1.99.0, built with `CARGO_BUILD_JOBS=2`.
  - Mesa 25.2.8 provides lavapipe (Vulkan) and llvmpipe (GL over EGL).
  - SwiftShader (the copy bundled with a Chromium build) served as a second Vulkan ICD, selected through `VK_ICD_FILENAMES`.
  - Xvfb runs the windowed tests, and ImageMagick `import` captures the screen.
- Status: every goal of the spike was met by code that compiled and ran. The things that did not work are listed in §6 with their exact errors.

Legend used below:
- **[ran]**: this code ran here, and the output is cited.
- **[compiled]**: used by spike code that compiled.
- **[source]** or **[docs]**: read from the crate's source or upstream docs, but not run here.
- **NOT VERIFIED HERE**: taken from docs or CI configuration only.

---

## 1. Decision

| Crate | Exact version (Cargo.lock) | MSRV (`rust-version`) | License | Role |
|---|---|---|---|---|
| wgpu | **30.0.1** (released 2026-08-22) | 1.87.0 | MIT OR Apache-2.0 | GPU abstraction (Vulkan, Metal, DX12, GL) |
| winit | **0.30.13** (2026-03-02) | 1.70.0 | Apache-2.0 | Windows, event loop |
| egui | **0.36.2** (2026-09-08) | 1.95 | MIT OR Apache-2.0 | Editor UI |
| egui-wgpu | **0.36.2** | 1.95 | MIT OR Apache-2.0 | egui renderer on wgpu |
| egui-winit | **0.36.2** | 1.95 | MIT OR Apache-2.0 | Input and platform output for egui on winit |
| glam | **0.34.1** (2026-10-06) | 1.85.0 | MIT OR Apache-2.0 | Math (camera, matrices) |
| bytemuck | **1.25.2** | none declared | Zlib OR Apache-2.0 OR MIT | Pod casts for vertex and uniform data |
| pollster | **1.0.1** | 1.69 | Apache-2.0/MIT | `block_on` for wgpu futures |
| png | **0.18.1** | 1.73 | MIT OR Apache-2.0 | PNG output and golden-image input |
| log | **0.4.34** | 1.71.0 | MIT OR Apache-2.0 | Logging facade |
| env_logger | **0.11.11** | 1.71 | MIT OR Apache-2.0 | Logging in spike binaries only |

The **combined MSRV is 1.95**, set by the egui crates. Pin the toolchain to 1.95 or newer; this spike used 1.99.0. Licenses and MSRVs come from each crate's packaged `Cargo.toml`, and release dates and downloads from the crates.io API (`scripts/crate_stats.py`).

Notable transitive dependencies pinned in Cargo.lock:
- wgpu internals at 30.0.1, all with MSRV 1.87 and MIT OR Apache-2.0: wgpu-core, wgpu-hal, wgpu-types, naga.
- Platform glue: raw-window-handle 0.6.2, ash 0.38.0+1.3.281, glow 0.17.0, khronos-egl 6.0.0, gpu-allocator 0.28.0.
- egui family: epaint, emath and ecolor 0.36.2.
- **epaint_default_fonts 0.36.2** is licensed `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0`, because egui's bundled fonts use those font licences.
- Text and rendering: vello_cpu 0.1.0, skrifa 0.44, harfrust 0.12.
- Platform I/O: arboard 3.6.1, x11rb 0.13.2, wayland-client 0.31.15.

### 1.1 Why this exact set is the newest compatible one
All from crates.io API dependency data (`scripts/deps.py` and `scripts/crate_stats.py`), cross-checked against Cargo.lock:
- wgpu 30.0.1 is the newest wgpu, and no pre-release exists. `egui-wgpu 0.36.2` requires `wgpu ^30.0` and `winit ^0.30.13` (optional). `egui-winit 0.36.2` requires `winit ^0.30.13`. So the newest egui works with the newest wgpu, and egui fixes winit to 0.30.x.
- winit 0.30.13 is the newest stable release. The newest pre-release is `0.31.0-beta.3`, which egui does not support (§2).
- wgpu's own v30.0.1 examples use winit as well: the workspace has `winit = { version = "0.30.8", features = ["android-native-activity"] }`. Their framework implements `ApplicationHandler<AppAction>` and calls `event_loop.owned_display_handle()`, `InstanceDescriptor::new_with_display_handle_from_env` and `run_app` (wgpu's workspace `Cargo.toml` and its examples' `framework.rs`, at tag v30.0.1).
- MSRVs: wgpu's README says "If you're using `wgpu`, our MSRV is **1.87**. If you're running our tests or examples, our MSRV is **1.93**." egui's 1.95 is the binding constraint.
- glam 0.34.1 is the newest glam. wgpu takes no glam dependency, and egui uses its own emath, so glam is free to choose.

### 1.2 Dependency weight (measured)
- **214 unique packages** on the Linux host (`cargo tree -e normal,build`), giving **281 compile units** in a cold release build.
- Slowest units in the cold release build (from `cargo build --timings`, 2 jobs on 4 CPUs):

  | Crate | Time |
  |---|---|
  | naga | 44.9 s |
  | wgpu-core | 42.5 s |
  | x11rb-protocol | 35.0 s |
  | egui | 30.7 s |
  | vello_cpu | 29.0 s |
  | read-fonts | 22.8 s |
  | moxcms | 20.5 s |
  | image | 20.5 s |
  | wayland-protocols | 19.4 s |
  | winit | 19.1 s |
  | pxfm | 18.8 s |
  | skrifa | 17.8 s |
  | wgpu-hal | 17.7 s |
  | ash | 16.7 s |
  | harfrust | 14.5 s |

- Where the weight comes from (`cargo tree -i`):
  - The `clipboard` default feature of egui-winit pulls `arboard 3.6.1`. That brings in `image 0.25.10` (with moxcms and pxfm) and x11rb, and also `smithay-clipboard 0.7.3`. smithay-clipboard adds a **second smithay-client-toolkit (0.20.0 alongside winit's 0.19.2)** and a second calloop (0.13 and 0.14).
  - epaint 0.36.2 depends on `skrifa 0.44`, `harfrust 0.12` and `vello_cpu 0.1` unconditionally; none of them is optional in its Cargo.toml.
  - env_logger 0.11.11 pulls `jiff`.
- Duplicated crates (`cargo tree -d`): calloop, calloop-wayland-source, hashbrown (0.16 and 0.17), linux-raw-sys, miniz_oxide, rustc-hash (1 and 2), rustix (0.38 and 1.1), smithay-client-toolkit, syn (2 and 3), thiserror (1 and 2).
- **Release binaries link only libc, libm and libgcc_s** (`ldd target/release/windowed`). The Vulkan loader, EGL/GL, X11 and Wayland libraries are all loaded at run time, so the binaries carry no link-time graphics dependencies.

---

## 2. Rejected alternatives

| Candidate | Why rejected (data from the crates.io API) |
|---|---|
| **iced 0.14.0** / iced_wgpu 0.14.0 | `iced_wgpu 0.14.0` needs **wgpu ^27.0** and glam ^0.25, three wgpu majors behind. Using it would put two wgpu versions in the tree. Its last release was 2025-12-07 and the license is MIT. Its retained Elm-style architecture also fits worse in an editor embedded in an engine loop. |
| **imgui 0.12.0** + imgui-wgpu 0.28.0 | imgui-rs was last released on 2024-05-05. imgui-wgpu needs **wgpu ^29**, one major behind. It also needs a C++ build (Dear ImGui). |
| **dear-imgui-rs / dear-imgui-wgpu 0.20.0** | It does support wgpu 27 through 30 behind features, but it is young (25.8k total downloads) and also needs a C++ build. Worth watching. |
| **eframe 0.36.2** | It owns the event loop and the app lifecycle. The engine has to own winit's loop and the wgpu device, so eframe does not fit. It is fine for separate tools. |
| **winit 0.31.0-beta.3** | It is a pre-release, and egui 0.36.2 requires winit ^0.30.13. Its API differs: winit-core 0.31.0-beta.3 makes `can_create_surfaces` the required method, has `resumed` as a provided method, adds `proxy_wake_up` and `destroy_surfaces`, and passes `&dyn ActiveEventLoop` [source: the winit-core 0.31.0-beta.3 crate]. |
| **sdl3 0.20.0 / glfw 0.63.0** | Both are C libraries that must be built or shipped. wgpu's examples and egui integrate with winit instead. Downloads are 238k for sdl3 and 973k for glfw, against 58M for winit. |
| **ash 0.38 / glow 0.18 / vulkano 0.35.2** | Each covers a single API (Vulkan or GL), so we would lose Metal, DX12 and WebGPU-style validation. vulkano's last release was 2025-08-16. ash and glow are already used inside wgpu-hal. |
| **nalgebra 0.35.0** | Its generic API is heavier than needed, and its glam interop stops at glam 0.33. glam is smaller and has 161M downloads. |
| **image crate as a direct dependency** | Not needed, because png 0.18 reads and writes PNG directly. image still arrives transitively through arboard (§1.2). |

---

## 3. Verified facts (each tied to code that ran)

### 3.1 Headless adapters and devices: instance, adapter and device with no surface [ran]
The code is `src/gpu.rs` and `src/bin/probe.rs`.

| Option | Adapter (`AdapterInfo`) | Device type | Downlevel WebGPU-compliant | Features | max 2D texture | min_uniform_buffer_offset_alignment | Timestamps |
|---|---|---|---|---|---|---|---|
| Vulkan: lavapipe (default ICD) | `llvmpipe (LLVM 20.1.2, 256 bits)`, driver `llvmpipe`, `Mesa 25.2.8-0ubuntu0.24.04.4 (LLVM 20.1.2)` | Cpu | yes | 63 | 16384 | 32 | TIMESTAMP_QUERY, INSIDE_ENCODERS and INSIDE_PASSES; period 1 ns |
| Vulkan: SwiftShader (`VK_ICD_FILENAMES=<path to>/vk_swiftshader_icd.json`) | `SwiftShader Device (Subzero)`, vendor 0x1ae0, device 0xc0de | Cpu | yes | 47 | 8192 | 256 | Same three features; period 1 ns |
| GL over EGL (no display) | `llvmpipe (LLVM 20.1.2, 256 bits)`, `4.5 (Core Profile) Mesa 25.2.8` | Cpu | **no** | 28 | 16384 | 32 | Same three features (but readings are unreliable, §3.6) |

- All three adapters meet `Limits::default()`, the WebGPU defaults, and `request_device` succeeded on each in 17 to 41 ms.
- GL is missing these downlevel flags that lavapipe has: `READ_ONLY_DEPTH_STENCIL`, `DEPTH_TEXTURE_AND_BUFFER_COPIES`, `WEBGPU_TEXTURE_FORMAT_SUPPORT`, `FULL_DRAW_INDEX_UINT32`, `DEPTH_BIAS_CLAMP`, `VIEW_FORMATS` and `SURFACE_VIEW_FORMATS`.
- Fallback adapter: `request_adapter(force_fallback_adapter: true)` returns the same CPU adapter as `false` on every backend. Every adapter in this container is `DeviceType::Cpu`, so "the fallback adapter" here means lavapipe, llvmpipe or SwiftShader.
- With `Backends::VULKAN | GL` and both adapters present, `request_adapter` picked Vulkan (lavapipe).
- Compile-time backends on Linux with default features are `Backends(VULKAN | GL)`. `Instance::new(Backends::all())` took 96 ms in a debug build, including the EGL probe.
- Debug builds get `InstanceFlags(DEBUG | VALIDATION | VALIDATION_INDIRECT_CALL)` from `InstanceFlags::from_build_config().with_env()`.
- Device limits are what you ask for. Requesting `Limits::default().using_resolution(adapter.limits())` produced a device with `min_uniform_buffer_offset_alignment = 256`, even though lavapipe offers 32 (as printed by the `headless` binary). `Limits::using_alignment` exists [source: wgpu-types-30.0.1/src/limits.rs:766] but was not exercised.

### 3.2 Offscreen lit, depth-tested, indexed mesh: WGSL shader, glam camera, PNG [ran]
The code is `src/scene.rs`, `src/scene.wgsl`, `src/checks.rs` and `src/bin/headless.rs`.

The scene:
- A unit cube (24 vertices, 36 `u32` indices) sits on a ground plane with a procedural checker. The clear color is a dark blue.
- Lighting: Blinn-Phong from a directional light, a warm point light with inverse-square falloff, and hemisphere ambient.
- Uniforms: globals (128 B) at group 0, and per-object data (144 B) at group 1 with a **dynamic offset** at a stride of 256.
- Targets: `Rgba8UnormSrgb` color and `Depth32Float` depth.
- **The cube is drawn before the plane**, so the cube only stays visible if the depth test works.

The assertions are in `src/checks.rs`, and every one passes on all three adapters in both debug and release:
- the center pixel is not the clear color, and it is orange (r > g and r > b);
- more than 10% of pixels are not the clear color;
- there are at least 64 distinct colors;
- luma stddev over non-clear pixels is at least 8;
- every pair of camera-facing cube faces differs in luma by at least 4;
- depth at the center is in (0, 1), and the sky corner is exactly 1.0.

Measured on lavapipe at 640x480. GL gives the same values, and SwiftShader is within 1 level at these sample points.
- The center pixel is `[174,116,64,255]` (orange cube), against the sRGB clear color `[39,48,69,255]`.
- 295 distinct colors (287 to 297 across drivers and sizes); 62.3% of pixels differ from the clear color.
- Luma stddev over non-clear pixels is 25.5.
- The three camera-facing cube faces have luma 82.3, 177.9 and 124.4, which shows the lighting varies by normal.
- Depth at the center is 0.9764 and at the corner 1.0. Depth readback only runs on Vulkan; GL lacks `DEPTH_TEXTURE_AND_BUFFER_COPIES`, so depth is reported as NaN and the depth check is skipped there.

The spike captured nine PNGs (three adapters, each in a debug build and in release builds at 640x480 and 1280x720) and inspected them: an orange cube on a grey checker plane with a warm highlight at the lower left.

Cross-driver agreement, measured with the spike's `imgdiff` binary (`src/bin/imgdiff.rs`):
- lavapipe vs llvmpipe GL at 640x480: 4 pixels differ, by at most 1 level.
- lavapipe vs SwiftShader: 15.9% of pixels differ, 55 pixels (0.018%) by more than 2 levels, with a max delta of 146 on edge pixels.
- lavapipe debug vs release: identical.

So golden images need a tolerance of about 2 levels plus a small outlier budget.

### 3.3 Readback with 256-byte rows [ran]
This is `src/readback.rs` and the test `lit_scene_renders_offscreen_with_depth_and_padding`:
- A 150-px-wide target has 600 B rows, padded to 768 B for `copy_texture_to_buffer`. The rows are unpadded after `map_async` plus `device.poll(PollType::Wait { submission_index: Some(i), timeout: Some(60 s) })`.
- `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT == 256` is asserted by the test `row_padding_follows_copy_alignment`.

### 3.4 Windowed variant with a winit 0.30 `ApplicationHandler` and a surface, under Xvfb [ran]
This is `src/bin/windowed.rs` and `scripts/run-windowed-capture.sh`. Screenshots of the Xvfb root window, captured while the app held its last frame, show the scene with an egui overlay window.

| | Vulkan (lavapipe) | GL (llvmpipe, EGL on X11) |
|---|---|---|
| Surface formats | `[Bgra8UnormSrgb, Bgra8Unorm]` | `[Rgba8UnormSrgb, Bgra8UnormSrgb, Rgba8Unorm, Bgra8Unorm, Rgba16Float]` |
| Present modes | `[Immediate, Mailbox, Fifo, FifoRelaxed]` | `[Fifo]` |
| Alpha modes | `[Opaque, Inherit]` | `[Opaque]` |
| Color spaces (wgpu 30) | SRGB only; `display_hdr_info` all `None` | SRGB only |
| `get_default_config` present mode | Immediate | Fifo |
| 120 frames: `get_current_texture` outcomes | 120 × `Success` | 120 × `Success` |
| Frame time median (acquire, scene, egui, submit, present), release | 6.46 ms (Fifo), 6.53 ms (Immediate), 6.66 ms (Mailbox) | 7.99 ms |

- Under Xvfb, Fifo is not held to a 60 Hz cadence: 120 frames ran at 91.5, 86.8 and 88.1 frames/s for Fifo, Immediate and Mailbox. Xvfb has no real vblank, so these rates are presumably bound by the CPU rasterizer, not by present pacing.
- SwiftShader cannot present (see §6).
- Without a display, `EventLoop::new()` fails cleanly (see §6).
- Input events were not injected, so only the egui-winit wiring itself was exercised: `on_window_event`, `take_egui_input` and `handle_platform_output` ran without error.

### 3.5 One egui frame through egui-wgpu, offscreen [ran]
This is `src/gui.rs` and `src/bin/gui_offscreen.rs`. The PNGs it wrote were also inspected visually.

The frame is a 1024x640 `Rgba8Unorm` target holding an editor layout:
- a menu bar with File, Edit, Generate, Agents and Help;
- an Outliner, and an Inspector with a slider, checkbox, DragValue, button and progress bar;
- a console panel;
- a central **Viewport that shows the engine's 3D render** through `Renderer::register_native_texture`.

Checks:
- ≥50 distinct colors (739 measured), ≥50% coverage, and ≥2000 cube-orange pixels from the viewport (7882 measured). They pass on Vulkan in debug and release, at `--ppp 1.0` and `--ppp 1.5`, and on GL in debug.
- The first frame produced 11 primitives, about 1.5k vertices, about 4.5k indices and 1 texture delta (the font atlas).
- On frame 1 the bottom panel sits about 30 px too high and leaves a black strip. Frames 2 onward are correct (the capture of frame 61, for example).
- On GL the viewport looks darker and more saturated. GL has no `VIEW_FORMATS`, so the spike registers the sRGB view; see §6.

### 3.6 Timestamp queries [ran]
- Available on all three adapters: `TIMESTAMP_QUERY`, `TIMESTAMP_QUERY_INSIDE_ENCODERS` and `TIMESTAMP_QUERY_INSIDE_PASSES`, with `get_timestamp_period() = 1` ns.
- Render-pass timings (`src/timing.rs`, `RenderPassTimestampWrites`, `resolve_query_set`):
  - On lavapipe and SwiftShader they are plausible, close to the CPU wall time. For example, at 640x480 lavapipe gives a GPU median of 1.78 ms against a 2.10 ms CPU median.
  - **On llvmpipe GL the values are not meaningful**: medians of 0.031 to 0.039 ms against a 1.3 to 4.1 ms wall time.

### 3.7 Errors [ran] (`tests/render.rs`)
- `request_adapter` on a backend set with no adapter returns `Err(RequestAdapterError::NotFound{..})`. Its Display text is `No suitable graphics adapter found; noop not requested, vulkan not requested, metal support not compiled in, dx12 not requested, gl not requested, webgpu not requested`.
- `request_device` with too-large limits returns `Err`, displayed as `Limit 'max_texture_dimension_2d' value 65536 is better than allowed 16384`.
- An error scope (guard API) catches `create_buffer(MAP_READ | UNIFORM)` as `Some(Error::Validation{..})`, with the description: "In Device::create_buffer, label = 'invalid-usage' `MAP` usage can only be combined with the opposite `COPY`, requested BufferUsages(MAP_READ | UNIFORM)".
- `CommandEncoder::map_buffer_on_submit` followed by `poll(Wait{Some(index), ..})` reads back `[1,2,3,4]`.
- With no handler installed, wgpu panics on uncaptured errors. Its `default_error_handler` does `panic!("wgpu error: {err}\n")` [source: wgpu-30.0.1/src/backend/wgpu_core.rs:692]. The spike installs `on_uncaptured_error` so failures are reported instead.

### 3.8 Tests [ran]
Six tests pass on each of the three adapters: lavapipe (the default), llvmpipe GL (`SPIKE_BACKENDS=gl`) and SwiftShader (`VK_ICD_FILENAMES` set to its ICD file). They ran against the final code. Earlier runs, also passing, came before a small refactor.
- `cargo clippy --all-targets` reports 0 warnings.
- `cargo fmt --check` is clean.

---

## 4. Measurements

### 4.1 Build times (4 CPUs, `CARGO_BUILD_JOBS=2`, rustc 1.99.0, warm crates.io cache) [ran]

| Build | Wall time |
|---|---|
| Cold `cargo build --release --bins` (281 units) | **5 m 33 s** (user 10 m 25 s) |
| Cold `cargo build --bins` (dev, full debuginfo) | **2 m 42 s** |
| Cold dev with `CARGO_PROFILE_DEV_DEBUG=line-tables-only` | **2 m 20 s** |
| Incremental dev after touching `src/scene.rs` (lib) | 1.30 s |
| Incremental dev after touching a bin | 1.21 s |
| Incremental release after touching the lib | 3.78 s |
| `cargo test` build after a dev build | 2.2 to 4.7 s |
| `cargo clippy --all-targets`, cold (check build of all dependencies) | 1 m 20 s |

### 4.2 Artifact sizes [ran]

| Artifact | Release | Release, stripped | Dev, full debuginfo | Dev, line-tables-only |
|---|---|---|---|---|
| `windowed` (scene, egui and winit) | 22.0 MB | 16.5 MB | 358 MB | 144 MB |
| `gui_offscreen` | 15.9 MB | 12.5 MB | 236 MB | 106 MB |
| `headless` (wgpu only) | 9.7 MB | 7.3 MB | 196 MB | 72 MB |
| `probe` | 7.5 MB | n/a | 192 MB | 68 MB |
| Target directory | `target/release` 959 MB | n/a | `target/debug` **2.8 GB** | 1.7 GB (2.3 GB with test and clippy artifacts) |

### 4.3 Frame and setup times, release, CPU rasterizers [ran]

| Metric (median) | lavapipe (Vulkan) | llvmpipe (GL) | SwiftShader (Vulkan) |
|---|---|---|---|
| `Instance::new` | 31 ms | 34 ms | 4.4 ms |
| `request_adapter` / `request_device` | 0.6 ms / 16 ms | 1.1 ms / 9 ms | 0.2 ms / 23 ms |
| Pipeline creation (`SceneRenderer::new`) | 2.5 to 2.6 ms | 1.8 to 2.1 ms | 4.6 to 4.9 ms |
| First frame, including color and depth readback | 28 to 95 ms (most likely driver shader JIT) | 11 to 16 ms | 51 to 69 ms |
| Offscreen frame (update, encode, submit, wait) at 640x480 | **2.10 ms** | 1.29 ms | 5.41 ms |
| Same at 1280x720 | 5.47 ms | 4.14 ms | 15.5 ms |
| Frame plus color readback to CPU, 640x480 / 1280x720 | 2.34 / 6.80 ms | 2.51 / 8.78 ms | 5.34 / 18.7 ms |
| GPU pass time from timestamps, 640x480 | 1.78 ms | 0.039 ms (bogus) | 5.21 ms |

- Debug build, lavapipe at 640x480: 6.64 ms per frame, with validation on.
- egui offscreen frame at 1024x640 (run_ui, tessellate, upload, render, wait):
  - release, Vulkan: median **6.24 ms**; ppp 1.5: 5.95 ms
  - debug, Vulkan: 12.85 ms
  - debug, GL: 10.26 ms
  - `egui_wgpu::Renderer::new`: 15 to 27 ms
  - first `run_ui`, which builds the font atlas: 9.6 ms in release, about 95 ms in debug

---

## 5. API cheat-sheet: wgpu 30.0.1, winit 0.30.13, egui 0.36.2

Every snippet below is copied from the spike sources that compiled and ran. Elisions are marked with `// ...`.

### 5.1 Instance, headless or windowed (`src/gpu.rs`, `src/bin/windowed.rs`)
```rust
// Headless: wgpu 29+ removed InstanceDescriptor::default(); pick a constructor explicitly.
let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
desc.backends = backends;
desc.flags = flags.unwrap_or_else(|| wgpu::InstanceFlags::from_build_config().with_env());
wgpu::Instance::new(desc)

// Windowed: hand the instance the display handle up front.
let mut desc = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
    event_loop.owned_display_handle(),
));
desc.backends = self.backends;
let instance = wgpu::Instance::new(desc);
```
`new_without_display_handle_from_env` and `new_with_display_handle_from_env` also read `WGPU_BACKEND` and related variables [docs: CHANGELOG v29].

### 5.2 Enumerate and request adapters (`src/bin/probe.rs`, `src/gpu.rs`)
```rust
let adapters = pollster::block_on(instance.enumerate_adapters(backends)); // async since v28

let adapter = instance
    .request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: opts.power_preference,
        force_fallback_adapter: opts.force_fallback_adapter,
        compatible_surface: None,
        apply_limit_buckets: false, // new in v30
    })
    .await?; // Result<Adapter, RequestAdapterError> (since v25)
```

### 5.3 Device and limits (`src/gpu.rs`)
```rust
pub fn pick_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let supported = adapter.limits();
    let wanted = wgpu::Limits::default();
    let base = if wanted.check_limits(&supported) {
        wanted
    } else {
        wgpu::Limits::downlevel_defaults()
    };
    base.using_resolution(supported)
}

adapter
    .request_device(&wgpu::DeviceDescriptor {
        label: Some("spike-device"),
        required_features: adapter.features() & optional_features,
        required_limits: pick_limits(adapter),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    })
    .await // Result<(Device, Queue), RequestDeviceError>
```

### 5.4 Error handling (`src/gpu.rs`, `tests/render.rs`)
```rust
device.on_uncaptured_error(Arc::new(|err: wgpu::Error| {
    log::error!("uncaptured wgpu error: {err}");
    eprintln!("UNCAPTURED WGPU ERROR: {err}");
}));

// Error scopes are guards since v28:
let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
let _bad = gpu.device.create_buffer(&wgpu::BufferDescriptor { /* MAP_READ | UNIFORM */ ..});
let err = pollster::block_on(scope.pop());
assert!(matches!(err, Some(wgpu::Error::Validation { .. })));
```

### 5.5 Shader module, bind group layouts and pipeline (`src/scene.rs`)
```rust
let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
    label: Some("scene.wgsl"),
    source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("scene.wgsl"))),
});
// Dynamic uniform offsets must be multiples of this limit.
let align = u64::from(device.limits().min_uniform_buffer_offset_alignment);
let object_stride = object_size.div_ceil(align) * align;

let objects_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
    label: Some("objects-layout"),
    entries: &[wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: NonZeroU64::new(object_size),
        },
        count: None,
    }],
});
// The bind group binds a one-object window; the dynamic offset selects the slot:
resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
    buffer: &objects_buffer,
    offset: 0,
    size: NonZeroU64::new(object_size),
}),

// wgpu 29+: bind group layouts are Option<&BindGroupLayout> (gaps allowed).
let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
    label: Some("scene-pipeline-layout"),
    bind_group_layouts: &[Some(&globals_layout), Some(&objects_layout)],
    immediate_size: 0, // push constants are "immediates" since v28
});

let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
    label: Some("scene-pipeline"),
    layout: Some(&pipeline_layout),
    vertex: wgpu::VertexState {
        module: &shader,
        entry_point: Some("vs_main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        // wgpu 30: vertex buffer slots are Option<VertexBufferLayout>.
        buffers: &[Some(vertex_layout)],
    },
    primitive: wgpu::PrimitiveState {
        topology: wgpu::PrimitiveTopology::TriangleList,
        front_face: wgpu::FrontFace::Ccw,
        cull_mode: Some(wgpu::Face::Back),
        ..Default::default()
    },
    // wgpu 29+: depth_write_enabled / depth_compare are Option.
    depth_stencil: Some(wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }),
    multisample: wgpu::MultisampleState::default(),
    fragment: Some(wgpu::FragmentState { /* entry_point: Some("fs_main"), targets: [Some(ColorTargetState{..})] */ ..}),
    multiview_mask: None,
    cache: None,
});
```
The vertex layout is `attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3]`.

### 5.6 Render pass (`src/scene.rs`)
```rust
let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
    label: Some("scene-pass"),
    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
        view: color_view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(CLEAR_COLOR), store: wgpu::StoreOp::Store },
    })],
    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
        view: depth_view,
        depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
        stencil_ops: None,
    }),
    timestamp_writes,          // Option<RenderPassTimestampWrites>
    occlusion_query_set: None,
    multiview_mask: None,      // v28
});
pass.set_pipeline(&self.pipeline);
pass.set_bind_group(0, &self.globals_bind_group, &[]);
pass.set_bind_group(1, &self.objects_bind_group, &[offset]);
pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
pass.draw_indexed(0..mesh.index_count, 0, 0..1);
```

### 5.7 Texture readback with 256-byte rows (`src/readback.rs`)
```rust
pub fn padded_bytes_per_row(width: u32, bytes_per_pixel: u32) -> u32 {
    let unpadded = width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    unpadded.div_ceil(align) * align
}
let bpp = format.block_copy_size(Some(aspect)).ok_or_else(|| { /* ... no block copy size ... */ })?;
let padded = padded_bytes_per_row(size.width, bpp);
encoder.copy_texture_to_buffer(
    wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect },
    wgpu::TexelCopyBufferInfo {
        buffer: &buffer,
        layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(size.height) },
    },
    wgpu::Extent3d { width: size.width, height: size.height, depth_or_array_layers: 1 },
);
// ... submit ...
let (tx, rx) = mpsc::channel();
buffer.map_async(wgpu::MapMode::Read, .., move |result| { let _ = tx.send(result); });
// Native wgpu only runs map callbacks from poll()/submit().
device.poll(wgpu::PollType::Wait { submission_index: submission, timeout: Some(Duration::from_secs(60)) })?;
rx.recv().map_err(|e| SpikeError::Other(format!("map callback never ran: {e}")))??;
{
    let view = buffer.get_mapped_range(..)?; // wgpu 30: Result<BufferView, MapRangeError>
    for row in 0..rows as usize {
        let start = row * padded_bytes_per_row as usize;
        out.extend_from_slice(&view[start..start + unpadded_bytes_per_row as usize]);
    }
} // BufferView must be dropped before unmap()
buffer.unmap();
```
Other poll forms: `PollType::wait_indefinitely()` and `PollType::Poll`. `poll` returns `Result<PollStatus, PollError>`.

### 5.8 Map on submit (`tests/render.rs`, v27+)
```rust
encoder.copy_buffer_to_buffer(&src, 0, &dst, 0, None); // size: impl Into<Option<u64>> (v26)
encoder.map_buffer_on_submit(&dst, wgpu::MapMode::Read, .., move |r| { let _ = tx.send(r); });
let index = gpu.queue.submit([encoder.finish()]);
gpu.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(Duration::from_secs(10)) }).expect("poll");
```

### 5.9 Timestamp queries (`src/timing.rs`)
```rust
let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
    label: Some("pass-timestamps"), ty: wgpu::QueryType::Timestamp, count: 2,
});
// resolve buffer: QUERY_RESOLVE | COPY_SRC; readback buffer: COPY_DST | MAP_READ; size 2 * wgpu::QUERY_SIZE
wgpu::RenderPassTimestampWrites { query_set: &self.query_set,
    beginning_of_pass_write_index: Some(0), end_of_pass_write_index: Some(1) }
encoder.resolve_query_set(&self.query_set, 0..2, &self.resolve_buffer, 0);
encoder.copy_buffer_to_buffer(&self.resolve_buffer, 0, &self.readback_buffer, 0, None);
// after submit + map: ns = (t1 - t0) * queue.get_timestamp_period()
```

### 5.10 Surface and winit 0.30 loop (`src/bin/windowed.rs`)
```rust
impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) { /* ... calls self.init(event_loop) once ... */ }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) { /* ... */ }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) { /* ... exit after hold / 60 s safety net ... */ }
}
let event_loop = match EventLoop::new() {   // fails without DISPLAY/WAYLAND_DISPLAY (see §6)
    Ok(el) => el,
    Err(e) => {
        eprintln!("EventLoop::new failed: {e}");
        return ExitCode::from(2);
    }
};
event_loop.set_control_flow(ControlFlow::Poll);
// ...
if let Err(e) = event_loop.run_app(&mut app) { /* ... */ }

// In init(event_loop):
let attrs = Window::default_attributes()
    .with_title("Real Engine render spike")
    .with_inner_size(winit::dpi::PhysicalSize::new(self.size.0, self.size.1));
let window = Arc::new(
    event_loop
        .create_window(attrs)
        .map_err(|e| format!("create_window: {e}"))?,
);
// Arc<Window> is 'static, so the surface is Surface<'static> and can live in App.
let surface = instance
    .create_surface(window.clone())
    .map_err(|e| format!("create_surface: {e}"))?;
let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
    power_preference: wgpu::PowerPreference::HighPerformance,
    force_fallback_adapter: false,
    compatible_surface: Some(&surface),
    apply_limit_buckets: false,
}))
.map_err(|e| format!("request_adapter(compatible_surface): {e}"))?;
let caps = surface.get_capabilities(&adapter); // formats, present_modes, alpha_modes, usages, format_capabilities (v30)
let format = caps
    .formats
    .iter()
    .copied()
    .find(|f| f.is_srgb())
    .unwrap_or(caps.formats[0]);
let config = wgpu::SurfaceConfiguration {
    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
    format,
    color_space: wgpu::SurfaceColorSpace::Auto, // new in wgpu 30
    width: size.width.max(1),
    height: size.height.max(1),
    // Fifo is the only mode guaranteed everywhere; others are opt-in if supported.
    present_mode: if caps.present_modes.contains(&self.present_mode) {
        self.present_mode
    } else {
        // ... (log) ...
        wgpu::PresentMode::Fifo
    },
    desired_maximum_frame_latency: 2,
    alpha_mode: caps.alpha_modes[0],
    view_formats: vec![],
};
surface.configure(&device, &config);

// Per frame (wgpu 29+: enum instead of Result<_, SurfaceError>):
let frame = match gfx.surface.get_current_texture() {
    wgpu::CurrentSurfaceTexture::Success(frame) => frame,
    wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame, // still presentable; reconfigure after presenting
    wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
    wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => { /* reconfigure */ return; }
    wgpu::CurrentSurfaceTexture::Validation => { /* report */ return; }
};
// ... encode scene + egui into frame.texture.create_view(..) ...
gfx.queue.submit(user_cmds.into_iter().chain([encoder.finish()]));
window.pre_present_notify();
gfx.queue.present(frame); // wgpu 30: replaces SurfaceTexture::present
```
On teardown, wgpu's `Surface` keeps its `Arc<Window>` source alive in a `_handle_source` field [source: wgpu-30.0.1/src/api/surface.rs:48], so the window cannot be destroyed under it. `main` also drops `gfx`, which holds the surface, before `window`. A resize calls `surface.configure` again and recreates the depth texture.

### 5.11 egui 0.36 frame through egui-wgpu into any view (`src/gui.rs`)
```rust
let renderer = egui_wgpu::Renderer::new(device, format, egui_wgpu::RendererOptions::default());

// Headless input: physical size / ppp, plus the viewport's native ppp.
let mut raw = egui::RawInput {
    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO,
        egui::vec2(width as f32 / pixels_per_point, height as f32 / pixels_per_point))),
    max_texture_side: Some(max_texture_side as usize),
    time: Some(self.start.elapsed().as_secs_f64()),
    focused: true,
    ..Default::default()
};
raw.viewports.entry(raw.viewport_id).or_default().native_pixels_per_point = Some(pixels_per_point);

let mut full_output = self.ctx.run_ui(raw_input, run_ui); // egui 0.36: run_ui(|ui: &mut Ui| ..)
let ppp = full_output.pixels_per_point;
let primitives = self.ctx.tessellate(full_output.shapes, ppp);
// egui 0.36: each texture id maps to a SmallVec of deltas; TexturesDelta must be drained.
for (id, deltas) in full_output.textures_delta.set.drain() {
    for delta in &deltas {
        self.renderer.update_texture(device, queue, id, delta);
    }
}
let screen = egui_wgpu::ScreenDescriptor { size_in_pixels, pixels_per_point: ppp };
let user_cmds = self.renderer.update_buffers(device, queue, encoder, &primitives, &screen);
{
    // egui-wgpu wants RenderPass<'static>: call forget_lifetime().
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor { /* color: view, load */ ..})
        .forget_lifetime();
    self.renderer.render(&mut pass, &primitives, &screen);
}
for id in full_output.textures_delta.free.drain() {
    self.renderer.free_texture(&id);
}
// submit: queue.submit(user_cmds.into_iter().chain([encoder.finish()]))
```
egui 0.36 panels take `&mut Ui`:
```rust
egui::Panel::top("menu").show(ui, |ui| { /* ... */ });
egui::Panel::left("outliner").default_size(170.0).show(ui, |ui| { /* ... */ });
egui::Panel::bottom("console").default_size(70.0).show(ui, |ui| { /* ... */ });
egui::CentralPanel::default().show(ui, |ui| { ui.image(egui::load::SizedTexture::new(tex.id, tex.size * scale)); });
egui::Window::new("Real Engine").default_pos([12.0, 12.0]).show(ui.ctx(), |ui| { /* ... */ }); // Window still takes a Context
```
egui-winit wiring:
```rust
let egui_state = egui_winit::State::new(gui.ctx.clone(), egui::ViewportId::ROOT, &window,
    Some(window.scale_factor() as f32), None, Some(device.limits().max_texture_dimension_2d as usize));
let _response = gfx.egui_state.on_window_event(window, &event);
let raw_input = gfx.egui_state.take_egui_input(&window);
gfx.egui_state.handle_platform_output(&window, platform_output);
```

### 5.12 Engine viewport inside egui (`src/scene.rs`, `src/bin/gui_offscreen.rs`)
```rust
// Scene texture is Rgba8UnormSrgb with view_formats [Rgba8Unorm] (needs DownlevelFlags::VIEW_FORMATS).
let linear_alias = color_format.remove_srgb_suffix();
// ... TextureDescriptor { format: color_format, usage: RENDER_ATTACHMENT | COPY_SRC | TEXTURE_BINDING, view_formats, .. }
self.color.create_view(&wgpu::TextureViewDescriptor {
    label: Some("offscreen-color-unorm-view"),
    format: Some(self.color.format().remove_srgb_suffix()),
    ..Default::default()
})
let viewport_id = gui.renderer.register_native_texture(device, &egui_view, wgpu::FilterMode::Linear);
let viewport_tex = egui::load::SizedTexture::new(viewport_id, egui::vec2(480.0, 320.0));
```

### 5.13 glam 0.34 camera for wgpu (`src/scene.rs`)
```rust
// wgpu clip space on every backend: NDC depth 0..1, Y up = glam's `directx` projection.
let proj = glam::camera::rh::proj::directx::perspective(self.fov_y_radians, aspect, self.near, self.far);
let view = glam::camera::rh::view::look_at_mat4(self.eye, self.target, Vec3::Y);
proj * view
```
glam 0.34 removed `Mat4::perspective_rh` and `Mat4::look_at_rh` (§6). The `camera::rh::proj::vulkan` module is Y-down and would flip the image.

### 5.14 Recent breaking changes, wgpu v27 to v30 (from wgpu's CHANGELOG.md at v30.0.1)

| Version | Change | Here |
|---|---|---|
| v30 | `VertexState.buffers: &[Option<VertexBufferLayout>]` | compiled, ran |
| v30 | `Queue::present(surface_texture)` replaces `SurfaceTexture::present()` | compiled, ran |
| v30 | `Buffer::get_mapped_range*` returns `Result<_, MapRangeError>` | compiled, ran |
| v30 | `SurfaceConfiguration.color_space`, `SurfaceCapabilities.format_capabilities`, `Surface::display_hdr_info` | compiled, ran |
| v30 | `RequestAdapterOptions.apply_limit_buckets` (default false) | compiled, ran |
| v30 | `TextureUsages::TRANSIENT` renamed `TRANSIENT_ATTACHMENT` | seen in probe output |
| v30 | `dispatch` / `dispatch_indirect` renamed `dispatch_workgroups` / `dispatch_workgroups_indirect` | changelog only (no compute in the spike) |
| v30 | Integer shader I/O no longer defaults to `@interpolate(flat)` (must be written) | changelog only |
| v29 | `get_current_texture()` returns the `CurrentSurfaceTexture` enum | compiled, ran |
| v29 | `InstanceDescriptor::new_without_display_handle` / `new_with_display_handle` (+ `_from_env`) | compiled, ran |
| v29 | `PipelineLayoutDescriptor.bind_group_layouts: &[Option<&BindGroupLayout>]` | compiled, ran |
| v29 | `DepthStencilState.depth_write_enabled` / `depth_compare` are `Option` | compiled, ran |
| v29 | Write mappings go through `WriteOnly<[u8]>` instead of `&mut [u8]` | changelog only |
| v29 | wgpu MSRV policy is 1.87 | read |
| v28 | `enumerate_adapters` is async | compiled, ran |
| v28 | `push_error_scope` returns a guard; call `.pop()` on it | compiled, ran |
| v28 | `multiview_mask` on render passes and pipelines | compiled |
| v28 | `MipmapFilterMode` split from `FilterMode` | changelog only |
| v28 | Push constants became immediates (`immediate_size`, `set_immediates`) | `immediate_size: 0` compiled |
| v27 | `PollType::Wait { submission_index, timeout }` (WaitForSubmissionIndex removed) | compiled, ran |
| v27 | `CommandEncoder::map_buffer_on_submit` | compiled, ran |
| v27 | `DeviceDescriptor.experimental_features` | compiled |
| v26 | `copy_buffer_to_buffer` size is `impl Into<Option<BufferAddress>>` | compiled, ran |
| v25 | `request_adapter` returns `Result` | compiled, ran |

Release cadence: v27.0.0 on 2025-10-01, v28.0.0 on 2025-12-17, v29.0.0 on 2026-03-18 and v30.0.0 on 2026-07-01. Expect a breaking major roughly every quarter.

---

## 6. Gotchas and failures (exact messages)

1. **glam 0.34 removed `Mat4::perspective_rh` and `look_at_rh`.** The first build failed with:
   - "error[E0599]: no associated function or constant named `perspective_rh` found for struct `Mat4` in the current scope"
   - the same error for `look_at_rh`

   The fix is §5.13.
2. **egui 0.36 changed `TexturesDelta.set`** to a `HashMap<TextureId, SmallVec<[ImageDelta; 1]>>`. The compiler reported "expected `&ImageDelta`, found `&SmallVec<[ImageDelta; 1]>`".
3. **egui 0.36 debug-asserts if `TexturesDelta` is dropped without being drained.** The message is "Dropped TexturesDelta with 1 unapplied deltas. Deltas need to be handled. If you want to drop this intentionally call `clear` before dropping." (epaint-0.36.2/src/textures.rs:339). Drain `set` and `free` (§5.11).
4. **GL has no `VIEW_FORMATS`, and one invalid texture cascades.** With `view_formats: [Rgba8Unorm]` on GL, the errors were:
   - "In Device::create_texture, label = 'offscreen-color' Downlevel flags DownlevelFlags(VIEW_FORMATS) are required but not supported on the device."
   - then "Texture with 'offscreen-color' label is invalid"
   - then "TextureView with '' label is invalid" (in CommandEncoder 'scene-frame', and again in Queue::submit)
   - and finally the poll fails: "Tried to wait using a submission index (1) that has not been returned by a successful submission (last successful submission: 0"

   The fix: add the alias only when `adapter.get_downlevel_capabilities().flags.contains(DownlevelFlags::VIEW_FORMATS)`. On GL the spike registers the sRGB view with egui, and the viewport looks darker as a result.
5. **GL has no `DEPTH_TEXTURE_AND_BUFFER_COPIES`**, so depth cannot be read back on GL. The spike skips that check there.
6. **SwiftShader cannot present.** The error is "create_surface: Failed to create surface for any enabled backend: {Vulkan: InstanceError { message: "Vulkan driver does not support VK_KHR_xlib_surface", source: None }}". Use it only for headless work.
7. **No display means no event loop.** The error is "EventLoop::new failed: os error at …/winit-0.30.13/src/platform_impl/linux/mod.rs:765: neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set." Run windowed tests under Xvfb (`scripts/run-windowed-capture.sh`, or `xvfb-run -a`).
8. **winit's `EventLoop` can be created only once per process and only on the main thread** [source: winit-0.30.13/src/event_loop.rs]:
   - "Attempting to create the event loop off the main thread will panic"
   - a second creation returns `EventLoopError::RecreationAttempt`

   Windowed checks therefore belong in binaries, not in `#[test]` functions. libtest runs tests on worker threads.
9. **Uncaptured wgpu errors panic by default** (§3.7). Install `on_uncaptured_error` in tools and CI.
10. **Device limits default to the WebGPU values**, not the adapter's best. The dynamic-offset stride became 256 even though lavapipe allows 32 (§3.1).
11. **The first egui frame lays out wrongly.** The bottom panel sits about 30 px too high on frame 1; render a warm-up frame before capturing golden images (§3.5).
12. **egui-wgpu warns on sRGB targets:** "Detected a linear (sRGBA aware) framebuffer Bgra8UnormSrgb. egui prefers Rgba8Unorm or Bgra8Unorm". egui wants gamma-space Unorm targets and Unorm texture views. The windowed overlay was drawn straight into the sRGB swapchain and only produced this warning; the color error was not measured.
13. **llvmpipe GL timestamps are not meaningful** (§3.6).
14. **Harmless log noise:**
    - "EGL 'eglInitialize' code 0x3001: DRI2: failed to load driver", logged twice at ERROR level whenever a GL instance is created. The GL adapter still works.
    - "libEGL warning: DRI3 error: Could not get DRI3 device" on GL under Xvfb.
    - winit warnings under Xvfb: "error setting XSETTINGS; Xft options won't reload automatically" and "XRandR reported that the display's 0mm in size, which is certifiably insane".
15. **The first frame on lavapipe is slow:** 28 to 103 ms against a 2 ms steady state, most likely from shader JIT on first use. Warm up before timing.
16. **Tooling:** GNU `time` was not installed on the test machine, so build times were taken with bash's `time` keyword.

### 6.1 Disk and cleanup
- The debug target with full debuginfo reached 2.8 GB, and the `windowed` dev binary alone was 358 MB. Setting `CARGO_PROFILE_DEV_DEBUG=line-tables-only`, or `[profile.dev] debug = "line-tables-only"`, brought the target to 1.7 GB and that binary to 144 MB. The release target was 959 MB.
- The target directories (959 MB for release, and 2.3 GB for the line-tables-only dev build with its test and clippy artifacts) were deleted after the final runs. Rebuilding takes about 5.5 min for release or 2.5 min for dev (§4.1).

---

## 7. Windows and macOS: NOT VERIFIED HERE (from docs and CI config only)

- **Backend support** (wgpu v30.0.1 README):

  | API | Windows | Linux/Android | macOS/iOS |
  |---|---|---|---|
  | Vulkan | ✅ first class | ✅ first class | 🌋 needs MoltenVK |
  | Metal | | | ✅ first class |
  | DX12 | ✅ first class | | |
  | OpenGL | 🆗 GL 3.3+ | 🆗 GL ES 3.0+ | 📐 ANGLE only |

  `Backends::PRIMARY` is `VULKAN | METAL | DX12 | BROWSER_WEBGPU`, and `SECONDARY` is `GL` [source: wgpu-types backend.rs].
- **DX12 shader compiler.** `WGPU_DX12_COMPILER` accepts `dxc`, `static-dxc` or `fxc`. `dxc` needs `dxcompiler.dll` (v1.8.2502 or newer) in the working directory, and `static-dxc` needs the crate feature of the same name. Without either, wgpu falls back to `fxc` (README). This has to be decided for shipping on Windows.
- **wgpu's own GPU CI** (its `ci.yml` workflow at v30.0.1, job `gpu-test`), running `cargo xtask test --llvm-cov`:
  - `windows-2022`: installs DXC, **WARP** (the software D3D12 rasterizer), the Agility SDK, Mesa and the Vulkan SDK.
  - `macos-14` (aarch64): installs the Vulkan SDK (MoltenVK) and uses the runner's Metal device.
  - `ubuntu-24.04`: installs the Vulkan SDK and Mesa.

  So Real Engine can run headless GPU tests on hosted Windows runners with WARP and Mesa, and on hosted macOS runners with Metal. The compile and clippy matrix also covers Windows and macOS on x86_64 and aarch64. CI MSRV variables are `REPO_MSRV: "1.93"`, `WGPU_MSRV: "1.87"` and `CORE_MSRV: "1.87"`.
- **winit 0.30** (source docs):
  - The event loop must be created on the main thread, which matters most on macOS. `with_any_thread(true)` exists on the X11, Wayland and Windows `EventLoopBuilderExt*` traits.
  - On iOS `run_app` never returns.
  - On Wayland or X11, unset `WAYLAND_DISPLAY` or `DISPLAY` to force the other one.
- **egui-wgpu defaults:** its default features include `macos-window-resize-jitter-fix` (which turns on `wgpu/metal`), `wgpu/default` and `wgpu/webgl`.
- **HDR:** wgpu 30 says surfaces "can now be configured with an explicit color space, enabling HDR and wide-gamut output where the platform supports it" (CHANGELOG). Xvfb only reported SRGB, so HDR on Windows and macOS is untested.

---

## 8. Risks
- **Only CPU rasterizers were tested.** No real GPU, no DX12, no Metal, no Wayland, no HiDPI or scale factor other than 1 in a window, and no real input events. Performance numbers are relative only.
- **API churn.**
  - wgpu ships a breaking major roughly every quarter (§5.14).
  - egui 0.36 changed its core frame API: `run_ui`, `Panel::*` taking `&mut Ui`, and `TexturesDelta`.
  - winit 0.31, now in beta, changes the `ApplicationHandler` contract, and egui will move to it later.
  - Mitigation: keep wgpu, winit and egui behind thin engine-owned modules, pin exact versions, and upgrade deliberately.
- **GL is a downlevel backend.** It lacks VIEW_FORMATS, SURFACE_VIEW_FORMATS, depth copies and READ_ONLY_DEPTH_STENCIL, offers only Fifo, and its timestamps are unreliable here. Any GL fallback needs its own paths: a blit or manual gamma for the editor viewport, and no depth readback.
- **Golden images differ across drivers** (§3.2), so tests need a tolerance and an outlier budget.
- **Build weight.**
  - About 5.5 min for a cold release build at 2 jobs, and 2.8 GB for the debug target with full debuginfo.
  - egui-winit's defaults pull clipboard and image-decoding crates plus a second smithay-client-toolkit.
  - Use `line-tables-only` debuginfo in dev, and trim features once requirements are known.
- **Licensing notes.** winit is Apache-2.0 only, not dual-licensed. egui's default fonts add OFL-1.1 and the Ubuntu Font Licence 1.0. Both are permissive but carry notice obligations, so check with whoever owns licensing. Everything else in the chosen set is MIT, Apache-2.0 or Zlib.

## 9. Recommendation
- **Adopt the stack:** wgpu 30.0.1, winit 0.30.13, and egui, egui-wgpu and egui-winit 0.36.2 (not eframe), with glam 0.34.1, bytemuck and pollster. Pin exact versions in the workspace and pin the toolchain to Rust 1.95 or newer.
- **Renderer shape:** write the renderer against `(&Device, &Queue, &TextureView)`. That one path then serves both headless offscreen rendering and the swapchain, which is already shown working here.
- **Agent/MCP screenshots:** expose "render the viewport and read it back as a PNG" as a headless capability for AI agents and MCP tools. It costs about 2.3 ms at 640x480 on lavapipe.
- **CI, Linux:**
  - headless tests on lavapipe, plus a llvmpipe GL job for downlevel coverage, and optionally SwiftShader;
  - a windowed smoke binary under Xvfb;
  - golden PNGs compared with a tolerance.
- **CI, other platforms:** add Windows runners with WARP and macOS runners with Metal later, following wgpu's own CI.
- **Editor viewport:**
  - render the scene into an sRGB texture that also allows a Unorm `view_format`, and register the Unorm view with `register_native_texture`;
  - render the egui pass into a Unorm target or swapchain view;
  - on GL, add a blit fallback.
- **Error handling and tooling:**
  - install `on_uncaptured_error` early;
  - request limits with `using_resolution` (and consider `using_alignment`);
  - use `line-tables-only` debuginfo for dev builds.

## 10. Open questions
1. When egui moves to winit 0.31, how much of our windowing layer changes? (`can_create_surfaces`, `destroy_surfaces`, `&dyn ActiveEventLoop`.)
2. Should the GL fallback for the editor viewport use a blit pass or a non-sRGB scene target with manual gamma?
3. Should the swapchain be Unorm with an sRGB view for the scene? That needs SURFACE_VIEW_FORMATS, which GL lacks. The alternative is to compose the scene offscreen and blit.
4. Are timestamp queries reliable on real GPUs and on Metal or DX12? On llvmpipe GL they were not.
5. Wayland (and its CSD), HiDPI scale factors, input, IME, clipboard and AccessKit are all untested.
6. Does HDR or wide-gamut output through `SurfaceColorSpace` and `display_hdr_info` work on real displays?
7. Which DX12 shader compiler should we ship (FXC fallback, DXC DLL or `static-dxc`)?
8. Should we use `egui-winit` with `default-features = false` and `egui-wgpu` with `default-features = false` to cut build time? This is not tested; the needed features would be x11, wayland and maybe clipboard.
9. Could `opt-level = 1/2` for wgpu-core, naga and egui in dev builds make debug frame times usable? Not measured.

## 11. How to reproduce
```sh
cd prototypes/render
export CARGO_BUILD_JOBS=2
cargo build --release --bins
./target/release/probe                                  # all adapters; --backends vulkan|gl
./target/release/headless --backends vulkan --size 640x480 --frames 60 --out out/headless.png
./target/release/headless --backends gl --size 640x480 --frames 60 --out out/headless-gl.png
VK_ICD_FILENAMES=/path/to/vk_swiftshader_icd.json \
  ./target/release/headless --backends vulkan --out out/headless-ss.png
./target/release/gui_offscreen --backends vulkan --frames 60 --out out/gui.png   # --ppp 1.5
EXTRA_ARGS="--present-mode mailbox" scripts/run-windowed-capture.sh target/release/windowed vulkan out/win.png 120
./target/release/imgdiff out/a.png out/b.png
cargo test -- --nocapture --test-threads=1                         # lavapipe
SPIKE_BACKENDS=gl cargo test --test render -- --nocapture --test-threads=1
python3 -I scripts/crate_stats.py wgpu winit egui --deps wgpu,winit   # crates.io health data
```

## 12. Process notes
- **Sources used.** crates.io API: versions, dependency requirements, MSRV, licenses and downloads, via `scripts/deps.py` and `scripts/crate_stats.py`. Packaged crate sources as downloaded by Cargo. wgpu v30.0.1 README, CHANGELOG, ci.yml and examples from the gfx-rs/wgpu repository at tag v30.0.1. The winit-core 0.31.0-beta.3 crate from crates.io (source read only). All code came from crates.io.
- **No OS packages were installed** for this spike; Mesa, Xvfb and ImageMagick were already present.
