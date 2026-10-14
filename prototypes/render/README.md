# Render prototype: wgpu 30, winit 0.30 and egui 0.36

This is the code from Real Engine's rendering spike. It checks that the chosen GPU, windowing and editor-UI crates work together, including on machines without a GPU. The full notes (measurements, API cheat-sheet, gotchas, risks and the recommendation) are in [`docs/research/rendering.md`](../../docs/research/rendering.md).

It is a prototype, not engine code. It is a standalone Cargo project with its own `Cargo.lock`, and the empty `[workspace]` table in `Cargo.toml` keeps it out of any workspace in a parent directory.

## What it proves

Everything below ran on Linux with CPU rasterizers only: Mesa 25.2.8 lavapipe (Vulkan) and llvmpipe (GL over EGL), plus SwiftShader as a second Vulkan driver. No real GPU, DX12, Metal or Wayland was available.

- **Headless rendering on CPU Vulkan and GL.**
  - With no window and no display, wgpu creates an instance, adapter and device on Vulkan (lavapipe or SwiftShader) and on GL (llvmpipe).
  - It renders a depth-tested, lit, indexed mesh into an offscreen texture: a cube on a checkered plane, with a WGSL shader, a glam camera and dynamic uniform offsets.
  - It reads the image back with 256-byte row padding and writes a PNG.
  - Checks in `src/checks.rs` confirm that the depth test works and that lighting varies by face. Depth readback is checked on Vulkan only, because GL cannot copy depth textures.
- **Windowed presentation under a virtual X server.**
  - A winit 0.30 `ApplicationHandler` opens a window under Xvfb.
  - It configures a wgpu 30 surface and presents frames of the scene with an egui overlay (egui-winit input, egui-wgpu rendering). Every `get_current_texture` call returned `Success`.
  - Lavapipe offers the Fifo, Immediate, Mailbox and FifoRelaxed present modes, and the spike presented 120 frames in each of the first three. GL offers only Fifo.
  - SwiftShader cannot present, because it has no `VK_KHR_xlib_surface`.
- **GUI rendering.**
  - egui-wgpu renders one egui 0.36 editor-style frame into an offscreen texture: a menu bar, an outliner, an inspector and a console.
  - The engine's 3D render appears in the central viewport through `register_native_texture`.
- **Also exercised:**
  - error scopes and uncaptured-error handling;
  - `request_adapter` and `request_device` failures and their messages;
  - `map_buffer_on_submit`;
  - timestamp queries, which give plausible values on Vulkan and meaningless ones on llvmpipe GL.

## Verified crate versions

These are the exact versions in `Cargo.lock`:

| Crate | Version | Role |
|---|---|---|
| wgpu | 30.0.1 | GPU abstraction (Vulkan, Metal, DX12, GL) |
| winit | 0.30.13 | Windows and the event loop |
| egui, egui-wgpu, egui-winit | 0.36.2 | Editor UI, its wgpu renderer and its winit integration |
| glam | 0.34.1 | Math (camera, matrices) |
| bytemuck | 1.25.2 | Pod casts for vertex and uniform data |
| pollster | 1.0.1 | `block_on` for wgpu futures |
| png | 0.18.1 | PNG output and input |
| log / env_logger | 0.4.34 / 0.11.11 | Logging |

- **Minimum Rust:** 1.95, set by the egui crates and declared as `rust-version`.
- **Verified with:** Rust 1.99.0.

## Layout

| Path | Contents |
|---|---|
| `src/lib.rs` | Library shared by the binaries and tests. Modules: `gpu` (instance, adapter, device), `scene` (meshes, camera, pipeline) with `scene.wgsl`, `readback`, `checks`, `gui` and `timing` |
| `src/bin/probe.rs` | Lists the adapters, with their features and limits |
| `src/bin/headless.rs` | Renders offscreen to a PNG, runs the image checks and prints timings |
| `src/bin/gui_offscreen.rs` | Renders one egui editor frame, with the 3D viewport, to a PNG |
| `src/bin/windowed.rs` | Window, surface, scene and egui overlay. It needs a display |
| `src/bin/imgdiff.rs` | Compares two PNGs: differing pixels and the largest channel delta |
| `tests/render.rs` | Integration tests |
| `scripts/run-windowed-capture.sh` | Runs `windowed` on a private Xvfb display and saves a screenshot |
| `scripts/deps.py`, `scripts/crate_stats.py` | crates.io queries used to choose the versions |

## Running the tests

**What you need:**

- Rust 1.95 or newer.
- A wgpu adapter. Any GPU driver works. Without a GPU, use Mesa's software drivers: lavapipe for Vulkan, or llvmpipe for GL through EGL.
- No display: the tests run headless.

```sh
cd prototypes/render

# Default backends (Vulkan, then GL): runs on lavapipe when there is no GPU.
cargo test --locked -- --test-threads=1 --nocapture

# Force the GL backend.
SPIKE_BACKENDS=gl cargo test --locked --test render -- --test-threads=1 --nocapture

# Use a specific Vulkan driver, for example SwiftShader.
VK_ICD_FILENAMES=/path/to/vk_swiftshader_icd.json cargo test --locked -- --test-threads=1
```

**Options:**

- `SPIKE_BACKENDS` takes `vulkan`, `gl`, `primary` or `all`.
- `--nocapture` prints the measured values.
- `--test-threads=1` keeps each test's output together, which is how the header of `tests/render.rs` says to run them.

**The six tests in `tests/render.rs`:**

| Test | What it checks |
|---|---|
| `row_padding_follows_copy_alignment` | The row-padding arithmetic for texture readback. No GPU needed |
| `lit_scene_renders_offscreen_with_depth_and_padding` | Renders the scene at 150x100 (an odd row size) and runs the image checks |
| `gui_frame_renders_offscreen` | One egui frame: geometry is produced, the font atlas is uploaded, and the image is not empty |
| `error_scope_catches_validation_error` | An error scope catches an invalid buffer usage |
| `request_errors_are_reported` | `request_adapter` with no matching backend fails, and so does `request_device` with excessive limits |
| `map_buffer_on_submit_reads_back` | A buffer copy is read back through `map_buffer_on_submit` |

### Windowed run under Xvfb

Windowed presentation is not a `cargo test`. winit creates its event loop only once per process and only on the main thread, but libtest runs tests on worker threads. Run the binary under a virtual X server instead:

```sh
cargo build --release --bins
xvfb-run -a -s "-screen 0 1280x720x24" target/release/windowed --backends vulkan --frames 120
```

It prints `WINDOWED OK` and exits with 0 when every frame was presented.

**Options:**

- `--backends` takes `vulkan` or `gl`.
- `--present-mode` takes `fifo` (the default), `immediate`, `mailbox` or `auto-no-vsync`.
- `--size` takes `WxH`.

To save a screenshot too, use `scripts/run-windowed-capture.sh target/release/windowed vulkan out/windowed.png 120`. It needs Xvfb and ImageMagick's `import`.

### Other binaries

```sh
target/release/probe                                   # all adapters; --backends vulkan|gl
target/release/headless --backends vulkan --size 640x480 --out out/headless.png
target/release/gui_offscreen --backends vulkan --frames 60 --out out/gui.png
target/release/imgdiff out/a.png out/b.png
```

- `headless` exits with 0 when every image check passes, 1 when a check fails and 2 when setup fails.
- The PNGs go to `out/`, which `.gitignore` excludes.

### Build cost

These figures are from 4 CPUs with `CARGO_BUILD_JOBS=2`.

| Build | Time | Target directory |
|---|---|---|
| Cold debug | about 2.5 min | 2.8 GB with full debuginfo |
| Cold debug with `CARGO_PROFILE_DEV_DEBUG=line-tables-only` | about 2.3 min | 1.7 GB |
| Cold release | about 5.5 min | 959 MB |

Release binaries link only libc, libm and libgcc_s. Vulkan, EGL/GL, X11 and Wayland libraries are loaded at run time.

## Status

**Last verified on 2026-10-14, when the prototype was added to this repository.** All runs used Rust 1.99.0 and debug builds.

- **`cargo check --locked --all-targets`:** clean, with no warnings.
- **`cargo test --locked`:** 6 of 6 tests pass on lavapipe (Vulkan) and on llvmpipe (GL).
- **Binaries:**
  - `headless` passes its checks on both backends;
  - `gui_offscreen` passes on Vulkan;
  - `windowed` under `xvfb-run` presents every frame on both backends.

**Not verified:**

- real GPUs;
- Windows (DX12) and macOS (Metal);
- Wayland;
- HiDPI windows;
- real input events.

**Known issues** (details in the notes, §6):

- **First egui frame:** its layout is about 30 px off. Render a warm-up frame before capturing.
- **GL viewport:** it looks darker, because GL lacks `VIEW_FORMATS` and the sRGB view is shown as is.
- **sRGB swapchain:** egui-wgpu warns when drawing into one.
