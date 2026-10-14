# Real Engine

Real Engine is an open-source 3D game engine written in Rust, at the design stage. It is being designed around three ideas:

- **Procedural first.** Worlds are to be described by node graphs (built from Luau or JSON), generated deterministically, and edited by hand on top, with hand edits designed to survive regeneration.
- **Luau scripting.** The plan starts with an engine-native API, followed by a compatibility layer for Roblox-style projects.
- **Agent-ready, AI optional.** Every edit is designed to go through one command registry, shared by the CLI, the editor and an MCP server. `real verify` is planned to give a JSON pass/fail for the whole project, with a stable `RE####` code and a `real explain` page for every error. Everything an agent can do is also an ordinary engine feature, and no AI provider is required.

## Status

**Early M1.** The architecture and the first milestone plan are written (see below). The Cargo workspace holds these crates:

- The contract layer: `re_core` (diagnostics, fixed-step clock, frame phases, task pool, events, profiling and logging), `re_noise` (deterministic noise) and `re_types` (the plain-data contracts between the app and its subsystems).
- The entity model: `re_reflect` (the component registry: typed, documented fields read and written by name), `re_scene` (the World facade over bevy_ecs, with stable ids, the hierarchy, the built-in components, scene and prefab files and procedural sources) and `re_command` (edit operations, transactions with undo and redo, and the command registry).
- The subsystems: `re_render` (the wgpu forward renderer with egui painting and headless capture), `re_physics` (Jolt through oxijolt behind a command-buffer API), `re_assets` (glTF import, material files, the processed-asset cache and hot reload), `re_input` (raw device state, the action map and replays), `re_audio` (kira playback driven by `AudioCmd`, with a headless capture backend) and `re_anim` (glTF node clips and tweens).

The repository checks are in `tools/xtask`, and `tools/derive_alias_check` proves that bevy's derives work through the `re_scene::ecs` re-export. Nothing runs a game yet: the runtime that ties these crates together (`re_app`), scripting, the procedural graph, the CLI, the agent layer and the editor are later M1 work packages.

The license is not final: see [DECISIONS.md, open questions](docs/DECISIONS.md#open-questions-for-the-owner). The crates declare `MIT OR Apache-2.0` until the owner decides.

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

## Building the workspace

You need the current stable Rust toolchain (`rust-toolchain.toml` selects it; the MSRV is 1.95).

```sh
export CARGO_BUILD_JOBS=2
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked --exclude re_render
cargo test --locked -p re_render -- --test-threads=1   # needs lavapipe, see below
cargo xtask check        # crate-graph rules, duplicate versions, explain pages, schema drift
cargo deny check         # needs cargo-deny 0.20.2: cargo install cargo-deny --version 0.20.2 --locked
```

CI (`.github/workflows/ci.yml`) runs the same checks, plus a build with Rust 1.95.0 and builds on Windows and macOS (ADR-0023).

## Running the renderer tests

`re_render`'s tests draw real frames, headless, and compare them with golden images in `crates/re_render/tests/goldens/` (one set per backend). No GPU is needed. On Debian or Ubuntu, install the system packages:

```sh
sudo apt-get install -y build-essential pkg-config libvulkan-dev mesa-vulkan-drivers \
  libegl1 libegl-mesa0 libgl1-mesa-dri libgles2 \
  libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libxcursor-dev libxrandr-dev \
  libxi-dev libwayland-dev xvfb
```

`mesa-vulkan-drivers` provides lavapipe, a software Vulkan driver. The EGL and Mesa GL packages are for the GL backend (llvmpipe). This is the renderer's subset; the full engine list is in [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md#system-packages).

```sh
export CARGO_BUILD_JOBS=2

# Headless tests on Vulkan (lavapipe), then on GL (llvmpipe)
cargo test --locked -p re_render -- --test-threads=1
RE_BACKENDS=gl cargo test --locked -p re_render -- --test-threads=1

# Windowed smoke test under a virtual display (prints WINDOWED OK)
cargo build --locked -p re_render --example windowed_smoke
xvfb-run -a -s "-screen 0 1280x720x24" target/debug/examples/windowed_smoke --frames 120

# Compare two images with the golden-test tolerance
cargo run --locked -p re_render --example re-imgdiff -- <a.png> <b.png>
```

CI runs the GPU crates' tests separately from the rest of the workspace, with `--test-threads=1`.

## Contributing

- Read [ARCHITECTURE.md](docs/ARCHITECTURE.md), then pick a work package in [M1_PLAN.md](docs/M1_PLAN.md). Each package lists the paths it owns, so parallel work does not collide. Some packages port code from the maintainers' private research spikes, which are not in this repository; ask the maintainer for access.
- Dependencies come from crates.io only. Adding one needs an entry in [DECISIONS.md](docs/DECISIONS.md) and a row in [DEPENDENCIES.md](docs/DEPENDENCIES.md).
- Tests must pass headless: software Vulkan, no audio device, and a virtual display only for the windowed smoke test.
- Do not commit third-party game projects or their scripts. The script validation suite reads them from paths you provide (`REAL_SCENE_SCRIPTS`, or one or more `--scripts-dir`), and skips when none is given.
