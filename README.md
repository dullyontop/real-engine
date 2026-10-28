# Real Engine

Real Engine is a proprietary 3D game engine written in Rust. It is built around three ideas:

- **Procedural first.** Worlds are described by node graphs (built from Luau or JSON), generated deterministically, and edited by hand on top. Hand edits survive regeneration.
- **Luau scripting.** An engine-native API, and a compatibility layer for Roblox-style projects (M2).
- **Agent-ready, AI optional.** Every edit goes through one command registry, shared by the CLI, the editor and an MCP server. `real verify --json` gives a pass/fail for the whole project, with a stable `RE####` code and a `real explain` page for every error. Everything an agent can do is also an ordinary engine feature, and no AI provider is required.

## Status

**M1 done.** All nine M1 work packages have merged, and `examples/island` is a playable demo: walk, push crates, collect items, win and restart, and regenerate the island with a new seed during play. CI runs `real verify` on it headless and a windowed smoke run under Xvfb. An MCP client drives it end to end in CI, and every item of the [definition of done](docs/M1_PLAN.md#definition-of-done-for-m1) is checked there.

**M2 done** (2026-10-20). Roblox-style projects run on Real Engine through `re_compat`, and every item of [M2's definition of done](docs/M2_PLAN.md#definition-of-done-for-m2) holds: the owner's private sample checks pass, `examples/plaza` is verified in CI, and a test reaches every implemented compat member. M2 includes container field types, attributes and tags, the `re_compat` class layer and data formats, the Roblox-style datatypes and enums, the instance data model with signals, the script runtime (`require`, `RunService`, `task`) with a headless test harness, server and client worlds with replication, players and remotes, physics for parts with static cells for large maps, characters, animation and input, tweens and sound, the 2D UI and the 3D view, `real import` for Rojo-style projects and Studio loader scripts, and running imported projects with `real run`, `real verify` and `play.run`.

**M3 done** (2026-10-26). Games ship as standalone builds for Linux, Windows, macOS and the browser, and every item of [M3's definition of done](docs/M3_PLAN.md#definition-of-done-for-m3) holds. M3 added:
- Replays end with the same `state_hash` on Linux, Windows and macOS, and a CI check compares them (D2, ADR-0036).
- `examples/valley` is a streamed world: you can walk it and drive a car along its road, and cells load ahead of you and unload behind (ADR-0042). Cells generate on worker threads, the terrain is drawn at levels of detail with regional erosion, and physics moves its origin with the player past 5 km.
- `real build` packs a game into `game.rpak` with a runtime-only player, laid out as a Linux folder, a Windows folder or a macOS app with the game's icon and the third-party notices, and `real verify --package` checks that it plays like the project (ADR-0037). The release workflow attaches prebuilt runtimes to each version tag's release, so a game can be built without the engine's source; no version has been tagged yet.
- `real build --target web` makes a folder any static server can host; the game runs in the browser with WebGPU or WebGL2, and saves last across page loads (ADR-0049).
- The editor has dockable panels, a node canvas over graph files (every edit an undoable `graph.*` command), and translate, rotate and scale gizmos (W, E, R) that move entities live, one undo step per drag (ADR-0044). It also has a script debugger, served over DAP with `--debug`, that stops in native and compat scripts (ADR-0045).
- The HDR post chain and cascaded shadows, configured in `[render.post]` and `[render.shadows]`; shaders that reload during a development run; textures block-compressed at import; glTF skins and animation graphs (`*.animgraph.json`: blend spaces, layers, state machines and root motion, driven by the `anim` Luau library and the `animgraph.*` commands); vehicles and ragdolls on Jolt; audio buses, effects and snapshots.
- Generator versions with `real migrate-graph`, `proc.bake`, structural overrides, node kinds written in Luau, and polygon booleans and roads with junctions as graph nodes.
- Project templates (`real new DIR --template native|roblox|terrain`, `--set KEY=VALUE`, `--list-templates`; the format is in [templates/README.md](templates/README.md)), generator recipes, `real agent eval`, and MCP over streamable HTTP on localhost (`real mcp --http`).
- For Roblox-style projects: saved data, script reload during play, SurfaceGui and ViewportFrame, skinned MeshParts, bloom, blur and sun rays, runs that repeat from `[compat] seed`, and `real import` of `.rbxl`, `.rbxlx`, `.rbxm` and `.rbxmx` files (with `--parent`).
- Online play for Roblox-style projects (ADR-0050): `real run --server` runs a dedicated server, `--host` plays while serving, and `--connect HOST[:PORT]` joins, over `re_net`, our reliable UDP transport. A browser build joins a server over a WebSocket (`?connect=ws://HOST:PORT/` on its page), and a built game serves and joins with the same flags (`real run --package DIR`, or the game's own executable). The traffic is not encrypted, so play on a LAN or with people you trust.

The workspace holds:

- The contract layer: `re_core`, `re_noise` and `re_types`.
- The entity model: `re_reflect` (the component registry, with its derive macro in `re_reflect_derive`), `re_scene` (the World facade over bevy_ecs, scenes, prefabs and procedural sources) and `re_command` (edit operations, undo and redo, and the command registry).
- The subsystems: `re_render` (wgpu), `re_physics` (Jolt), `re_assets`, `re_input`, `re_audio`, `re_anim` and `re_net` (UDP and WebSocket transports).
- Procedural generation: `re_geom` and `re_proc` (the ProcGraph).
- Scripting: `re_script` (Luau through mlua) and `re_script_check` (the validation suite).
- The runtime and CLI: `re_app` and `re_cli` (the `real` binary), and `re_web` (the browser runtime).
- Tooling: `re_agent`, `re_mcp` (the MCP server), `re_editor` (the editor) and `re_import` (`real import`: Rojo-style projects, Studio loader scripts, and place and model files).
- Compatibility (M2): `re_compat`, the Roblox-style layer.

Try it with `cargo run --release -p re_cli -- run --project examples/island` (or `examples/plaza`, or `examples/valley`), or `real verify --project examples/island --json` for the headless checks.

Real Engine is proprietary, and all rights are reserved: the repository has no open-source license, and the crates declare none. Compiled builds of the engine may later be distributed under a proprietary license ([DECISIONS.md, open question 1](docs/DECISIONS.md#open-questions-for-the-owner)). Third-party crates and the bundled fonts keep their own licenses, listed in [DEPENDENCIES.md](docs/DEPENDENCIES.md).

## Stack

All crate versions are pinned exactly and checked against crates.io. See [DEPENDENCIES.md](docs/DEPENDENCIES.md) for licenses (including the MPL-2.0 and font-license exceptions), features, external tools and bundled assets.

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
| Agents | rmcp 3.5.1 (MCP over stdio and streamable HTTP), tokio 1.53.3 |

## Documents

| Document | Contents |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | The crate graph, frame loop and threads, object model, procedural system, AI integration, diagnostics, and file formats |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Milestones M0 to M3, their exit criteria, and why they are in this order |
| [docs/M1_PLAN.md](docs/M1_PLAN.md) | The first milestone as nine self-contained work packages, with owned paths, cross-package contracts, acceptance commands, and the definition of done |
| [docs/M2_PLAN.md](docs/M2_PLAN.md) | The second milestone, the Roblox-style compatibility layer, as work packages WP9–WP17 in the same shape |
| [docs/M3_PLAN.md](docs/M3_PLAN.md) | The third milestone (ship games: platforms and D2, export, rendering, skinning, vehicles, streaming, the procedural workflow, the editor, the AI track, compat follow-ups, `examples/valley`, browser builds and online play), as work packages WP18–WP30 |
| [docs/DECISIONS.md](docs/DECISIONS.md) | Architecture decision records, plus open questions for the project owner |
| [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) | Every dependency with its version, license, MSRV and verification status; system packages |
| [docs/research/core.md](docs/research/core.md) | Findings from the entity-core spike (bevy_ecs, the component registry, scene files) |
| [docs/research/rendering.md](docs/research/rendering.md) | Findings from the rendering spike |
| [docs/research/scripting.md](docs/research/scripting.md) | Findings from the scripting spike (Luau through mlua, the scheduler, type checking) |

## Building the workspace

You need the current stable Rust toolchain (`rust-toolchain.toml` selects it; the MSRV is 1.95).

```sh
export CARGO_BUILD_JOBS=2
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked --exclude re_render --exclude re_app --exclude re_cli --exclude re_editor --exclude re_web
cargo test --locked -p re_render -p re_app -p re_cli -p re_editor -p re_web -- --test-threads=1   # GPU crates; needs lavapipe, see below
cargo xtask check        # crate-graph rules, duplicate versions, explain pages, schema drift, compat coverage, web clippy settings
cargo run --locked --release -p re_cli -- verify --project examples/island --json   # the demo's headless gate
cargo deny check         # needs cargo-deny 0.20.2: cargo install cargo-deny --version 0.20.2 --locked
                         # (or, on Linux x86_64, its release build: eval "$(tools/ci/cargo-deny.sh)")
```

CI (`.github/workflows/ci.yml`) runs these checks, `real verify` on the island, the plaza and the valley (with assertions on the JSON) and windowed smoke runs under Xvfb, plus a build with Rust 1.95.0, the browser build and the packaged Linux games. Its 12 checks are the merge rule (ADR-0023). The repository's CI minutes are billed, so a `changes` job runs only the checks a change needs: a draft PR runs nothing, a change to documentation alone runs `fmt`, and a push to main runs the release checks only when it changes a build input (ADR-0023's amendment of 2026-10-28). The whole suite on Windows and macOS, their packaged games and the cross-platform `state_hash` check are in `.github/workflows/platforms.yml`, which is started by hand before a release and for changes that touch one platform (ADR-0023's amendment of 2026-10-27). The type-check tests need luau-lsp 1.70.1 on `PATH` ([DEPENDENCIES.md](docs/DEPENDENCIES.md), "External tools").

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

- Read [ARCHITECTURE.md](docs/ARCHITECTURE.md). The work packages of M1, M2 and M3 ([M1_PLAN.md](docs/M1_PLAN.md), [M2_PLAN.md](docs/M2_PLAN.md) and [M3_PLAN.md](docs/M3_PLAN.md)) have all merged, and no later milestone is planned yet: new work starts with an ADR in [DECISIONS.md](docs/DECISIONS.md), and [ROADMAP.md](docs/ROADMAP.md) names what M3 left for later. Some packages ported code from the maintainers' private research spikes, which are not in this repository; ask the maintainer for access.
- Dependencies come from crates.io only, apart from the temporary patched copies of `mlua` and `oxijolt-sys` in `third_party/` that the owner approved (ADR-0049 §2). Adding one needs an entry in [DECISIONS.md](docs/DECISIONS.md) and a row in [DEPENDENCIES.md](docs/DEPENDENCIES.md).
- Tests must pass headless: software Vulkan, no audio device, and a virtual display only for the windowed smoke test.
- Do not commit third-party game projects or their scripts. The script validation suite reads them from paths you provide (`REAL_SCENE_SCRIPTS`, or one or more `--scripts-dir`), and skips when none is given.
