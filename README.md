# Real Engine

Real Engine is an open-source 3D game engine written in Rust, at the design stage. It is being designed around three ideas:

- **Procedural first.** Worlds are to be described by node graphs (built from Luau or JSON), generated deterministically, and edited by hand on top, with hand edits designed to survive regeneration.
- **Luau scripting.** The plan starts with an engine-native API, followed by a compatibility layer for Roblox-style projects.
- **Agent-ready, AI optional.** Every edit is designed to go through one command registry, shared by the CLI, the editor and an MCP server. `real verify` is planned to give a JSON pass/fail for the whole project, with a stable `RE####` code and a `real explain` page for every error. Everything an agent can do is also an ordinary engine feature, and no AI provider is required.

## Status

**Design stage.** The architecture and the first milestone plan are written (see below). The only code in this repository today is the **rendering prototype** in `prototypes/render/`. It is a standalone spike that verified the rendering stack headless and windowed; it will be removed once M1 WP1 has ported it into `re_render`. None of the engine crates described in the docs exist yet.

The license is not final: see [DECISIONS.md, open questions](docs/DECISIONS.md#open-questions-for-the-owner). The prototype's `Cargo.toml` declares `MIT OR Apache-2.0`.

## Stack

All crate versions are pinned exactly and checked against crates.io. See [DEPENDENCIES.md](docs/DEPENDENCIES.md) for licenses (including the MPL-2.0 and font-license exceptions), features, external tools, and which entries are still Provisional.

| Area | Crates |
|---|---|
| Toolchain | Rust stable; MSRV 1.95 (set by egui and bevy_ecs); edition 2024 |
| Rendering | wgpu 30.0.1, winit 0.30.13, egui / egui-wgpu / egui-winit 0.36.2, glam 0.34.1 |
| Entity core | bevy_ecs 0.20.0 (standalone, no bevy_reflect) with our own component registry |
| Physics | oxijolt 1.1.0+jolt-5.6.0 (Jolt Physics 5.6.0), built from source |
| Scripting | mlua 0.12.2 (Luau 0.740) |
| Script tooling | full_moon 3.0.0 (static scan); luau-lsp 1.70.1 as the external type checker |
| Assets, audio, input | gltf 1.4.1, image 0.25.10, kira 0.12.5, gilrs 0.11.2, notify 8.2.0 |
| Data | serde 1.0.229, serde_json 1.0.154, toml 1.1.8, postcard 1.1.3, blake3 1.8.7 |
| Agents | rmcp 3.5.1 (MCP over stdio), tokio 1.53.3 |

## Documents

| Document | Contents |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | The crate graph, frame loop and threads, object model, procedural system, AI integration, diagnostics, and file formats |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Milestones M0 to M3+, their exit criteria, and why they are in this order |
| [docs/M1_PLAN.md](docs/M1_PLAN.md) | The first milestone as nine self-contained work packages, with owned paths, cross-package contracts, acceptance commands, and the definition of done |
| [docs/DECISIONS.md](docs/DECISIONS.md) | Architecture decision records, plus open questions for the project owner |
| [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) | Every dependency with its version, license, MSRV and verification status; system packages |
| [docs/research/rendering.md](docs/research/rendering.md) | Findings from the rendering spike |

## Building and running the rendering prototype

You need Rust 1.95 or newer. On Debian or Ubuntu, install the system packages:

```sh
sudo apt-get install -y build-essential pkg-config libvulkan-dev mesa-vulkan-drivers \
  libegl1 libegl-mesa0 libgl1-mesa-dri libgles2 \
  libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libxcursor-dev libxrandr-dev \
  libxi-dev libwayland-dev xvfb
```

`mesa-vulkan-drivers` provides lavapipe, a software Vulkan driver, so no GPU is needed. The EGL and Mesa GL packages are for the GL backend (llvmpipe). This is the prototype's subset; the full engine list is in [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md#system-packages).

```sh
cd prototypes/render

# Headless tests on Vulkan (lavapipe), then on GL (llvmpipe)
cargo test --locked -- --test-threads=1 --nocapture
SPIKE_BACKENDS=gl cargo test --locked --test render -- --test-threads=1 --nocapture

# Render one frame to a PNG
cargo build --release --bins
target/release/headless --backends vulkan --size 640x480 --out out/headless.png

# Windowed smoke test under a virtual display (prints WINDOWED OK)
xvfb-run -a -s "-screen 0 1280x720x24" target/release/windowed --backends vulkan --frames 120
```

The other binaries are `probe` (adapter report), `gui_offscreen` (egui to a PNG) and `imgdiff` (compare two images). A cold release build takes a few minutes.

## Contributing

- Read [ARCHITECTURE.md](docs/ARCHITECTURE.md), then pick a work package in [M1_PLAN.md](docs/M1_PLAN.md). Each package lists the paths it owns, so parallel work does not collide. Some packages port code from the maintainers' private research spikes, which are not in this repository; ask the maintainer for access.
- Dependencies come from crates.io only. Adding one needs an entry in [DECISIONS.md](docs/DECISIONS.md) and a row in [DEPENDENCIES.md](docs/DEPENDENCIES.md).
- Tests must pass headless: software Vulkan, no audio device, and a virtual display only for the windowed smoke test.
- Do not commit third-party game projects or their scripts. The script validation suite reads them from paths you provide (`REAL_SCENE_SCRIPTS`, or one or more `--scripts-dir`), and skips when none is given.
