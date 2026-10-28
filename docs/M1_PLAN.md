# M1 plan

M1 is the first playable vertical slice. It is a procedural island with a third-person character, pushable crates, collectibles placed by a scatter node, a HUD, and a goal: collect every item, then press R to restart. The island can be regenerated with a new seed during play. The whole thing is checked headless by `real verify`, and it can be driven by an agent over MCP.

This plan splits M1 into nine work packages (WPs): WP0 to WP8.
- **WP0 comes first.** It lands in two PRs:
  - **PR "contracts"**: the workspace, GitHub Actions CI, `tools/xtask`, `re_core`, `re_noise` and the frozen plain-data contracts in `re_types`;
  - **PR "entity core"**: `re_reflect`, `re_scene` and `re_command`, ported from the core spike onto bevy_ecs 0.20.0, plus the published `docs/research/core.md`.
- After WP0's "contracts" PR merges, **WP1** (render), **WP2** (physics) and **WP5** (assets, input, audio, animation) start. They only need `re_core` and `re_types`.
- After WP0's "entity core" PR merges, **WP3** (scripting), **WP4** (procedural) and **WP7** (agent and MCP) start.
- **WP6** integrates the runtime, the CLI and the demo. **WP8** (editor-lite) builds on WP6.

**Every WP section below is a self-contained brief.** An engineer (or agent) who has only this repository and the research folder can do the WP from its section plus the documents it links. Each section gives the goal, the owned paths, the frozen interfaces it consumes and where they are defined, the spike code to reuse, what to build, the non-goals, the acceptance commands and the definition of done.

**Research folder.** The design-phase spikes live in a private research folder at **`/mnt/project-files/real-engine/research/`**. It is **not** part of this public repository, and nothing from it is committed except code that a WP ports on purpose (with the clean-port rule below). Paths written as `research/…` below are relative to that folder. Each WP ports the files listed in its Reuse table into its own crate, with their tests, so once a WP lands the repository no longer needs the research folder for that area. The render prototype was the exception: it lived in this repository at `prototypes/render/` until WP1 ported it into `crates/re_render/` and deleted it.

**Ownership rule.** Every path in the repository belongs to exactly one WP, and a WP only edits files it owns. Ownership can be finer than a crate: `crates/re_cli/src/tooling/` is split between WP6, WP7 and WP8, as listed below. One handover is planned: WP6 creates `crates/re_cli/src/tooling/{mcp,agent,editor}.rs` as stubs in its milestone "app" PR, so that `re_cli` compiles; from that merge on, `mcp.rs` and `agent.rs` belong to WP7 and `editor.rs` to WP8, and WP6 never edits them again. Each stub exposes exactly two names, which WP6's `tooling/mod.rs` and clap tree use and which are frozen at that merge: `pub struct McpArgs` / `AgentArgs` / `EditorArgs` (deriving `clap::Args`; the owner may add fields, or replace the struct with a `pub use` of a type with the same name) and `pub fn run(args: <X>Args, project: &std::path::Path) -> anyhow::Result<std::process::ExitCode>`. One shared file has a named exception: in `crates/re_cli/Cargo.toml` (owned by WP6), WP7 may add the `re_agent` and `re_mcp` dependency lines, as `optional = true`, plus their dev-dependency lines, and add `"dep:re_agent"` and `"dep:re_mcp"` to the `tooling` feature, and WP8 may do the same for `re_editor`; neither changes any other line. Both are tooling crates, so a plain (non-optional) line would link them into the `--no-default-features` build, which `cargo xtask check-graph` refuses. `re_agent` is needed first by WP6's `real do` PR (`tooling/do_cmd.rs`): whichever of that PR and WP7's first `re_cli` change lands first adds the optional `re_agent` line and `"dep:re_agent"`, and the other leaves them as they are. Two kinds of generated file are exceptions: `Cargo.lock` and the files in `schemas/`. They belong to WP0, but any WP may commit a change that `cargo` or `cargo xtask schemas` produced. Nobody edits them by hand. After WP0 merges, the maintainer (the lead) takes over every path WP0 owns: the root `Cargo.toml` (including `[workspace.dependencies]`), `Cargo.lock` and `schemas/` (both still under the generated-file exception above), `rust-toolchain.toml`, `deny.toml`, `.cargo/config.toml`, `.gitignore`, `.github/`, `tools/xtask/`, `tools/derive_alias_check/`, `README.md`, `docs/` (except `docs/research/scripting.md`, which WP3 owns) and the WP0 crates (`re_core`, `re_noise`, `re_types`, `re_reflect`, `re_scene`, `re_command`). The maintainer reviews every change to a frozen contract and acts as "the WP that defines it" for WP0's contracts. WP threads do not edit these files: each WP's PR description lists the doc, ADR and dependency changes it needs (a new crate in the workspace, a dependency from `docs/DEPENDENCIES.md`, an ADR amendment), and the maintainer makes them. A change to a frozen contract (see "Cross-WP contracts") needs a PR reviewed by the maintainer, by the WP that defines it and by every WP that consumes it.

## Common rules for every WP

- **Build settings.** `export CARGO_BUILD_JOBS=2`. Delete `target/` when disk use goes above 2 GB.
- **Toolchain.** `rust-toolchain.toml` says `channel = "stable"`. The workspace declares `rust-version = "1.95"`, and the `msrv` CI job builds with exactly 1.95.0. Use only dependencies listed in [DEPENDENCIES.md](DEPENDENCIES.md), always as `foo.workspace = true`. A new dependency needs an ADR.
- **One-time setup** in the reference container (CI does the same):
  ```sh
  sudo apt-get install -y build-essential g++ cmake pkg-config \
    libvulkan-dev mesa-vulkan-drivers vulkan-tools libegl1 libegl-mesa0 libgl1-mesa-dri libgles2 \
    libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev \
    libxcursor-dev libxrandr-dev libxi-dev libwayland-dev libasound2-dev libudev-dev xvfb
  rustup toolchain install 1.95.0 --profile minimal     # for the msrv check
  cargo install cargo-deny --version 0.20.2 --locked    # for `cargo deny check`
  ```
  Type-check tests (WP3, WP6) also need luau-lsp 1.70.1 on `PATH` (DEPENDENCIES.md, "External tools"; CI installs it).
- **Headless.** Tests must pass in the reference container:
  - 4 vCPU;
  - lavapipe for Vulkan and llvmpipe for GL;
  - no audio device;
  - no display, except for jobs explicitly run under `xvfb-run`.
- **GPU tests.**
  - The crates with GPU tests are `re_render`, `re_app`, `re_cli` and `re_editor`. Their tests run with `-- --test-threads=1`.
  - Choose the backend with `RE_BACKENDS=vulkan` (the default) or `RE_BACKENDS=gl`.
  - A winit `EventLoop` can only be created once per process and only on the main thread. Windowed tests are therefore separate binaries or examples run under `xvfb-run -a -s "-screen 0 1280x720x24"`. They are never `#[test]` functions.
- **Errors.** Every user-facing error is a `re_core::Diagnostic` with a code from the crate's range ([ARCHITECTURE.md §7](ARCHITECTURE.md#7-diagnostics-and-error-model)). Each code has an explain page `crates/<crate>/explain/RE####.md`, and the crate exports `pub const DIAGNOSTICS: &[re_core::DiagInfo]`.
- **Clean ports.** Code ported from spikes keeps its tests. Comments, test data and report sections that quote or name external projects are removed during the port; each WP's Reuse table names the known spots (for example `src/input.rs` in WP5, core.md §8 and the demo ids in WP0, and scripting.md's header and §3 in WP3).
- **Lints.** `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` and `cargo fmt --all --check` must pass. Ported spike code is made clippy-clean during the port.
- **Output files.** Acceptance commands write into `out/`, which is git-ignored. Create it first with `mkdir -p out`.
- **Definition of done (every WP).** The WP's PRs are green on every required CI job of ADR-0023 ([DECISIONS.md](DECISIONS.md#adr-0023-ci-linux-with-lavapipe-blocks-merges-windows-and-macos-are-reported-only)), reviewed and **merged to `main`**; the WP's acceptance commands pass in the reference container; no file outside the WP's owned paths changed (except tool-generated `Cargo.lock` and `schemas/`); every diagnostic code the WP uses has an explain page.

---

## Cross-WP contracts

A contract is **frozen** at the point named here. After that, changing it follows the ownership rule above.

| Contract | Defined in | Defined by | Frozen at | Consumed by |
|---|---|---|---|---|
| Plain-data types: `EntityId`, `RenderSnapshot`, `DrawItem`, `Camera`, `Light`, `MeshHandle`, `MaterialHandle`, `TextureHandle`, `MaterialDesc`, `DebugLine`, `MeshData`, `Aabb`, `ClipData`, `PhysicsCmd`, `BodyDesc`, `ShapeDesc`, `BodyPose`, `ContactEvent`, `RayHit`, `CharacterDesc`, `CharacterState`, `AudioCmd`, `ActionFrame`, `GraphDoc`, `ReplayDoc` | `crates/re_types/src/*.rs` (module per area, re-exported at the crate root) | WP0 | WP0 PR "contracts" | all |
| `HostServices` trait (what scripts may ask of the runtime) | `crates/re_types/src/host.rs` | WP0 | WP0 PR "contracts" | WP3 calls it, WP6 implements it |
| `Diagnostic`, `DiagInfo`, `Phase`, `FixedClock`, `TaskPool`, `Events<T>`, `profile_scope!`, `init_logging` | `crates/re_core/src/*.rs` | WP0 | WP0 PR "contracts" | all |
| Component registry: `Registry`, `ComponentKey`, `ComponentSchema`, `FieldSchema`, `FieldType`, `ComponentOrigin`, `Value`, `Color`, `Reflect`, `component!`, `field_enum!`, `Doc`, `ReflectError`, `suggest` | `crates/re_reflect/src/*.rs` | WP0 | WP0 PR "entity core" | WP3, WP4, WP6, WP7, WP8 |
| `World` facade (including `World::fork`), `Entity`, `StableId`, `IdIndex`, the built-in components and their fields, `SceneDoc` load/save, `PrefabFile`, procedural sources (`Procedural`, `GraphSource`, `Generated`, `Overrides`, `GenNode`, `SceneFragment`, `GenContext`, `Generators`, `RegenReport`), `suppress(id)` (the full generated id `<source id>/<key>`; a bare key is refused with RE1210), `state_hash` | `crates/re_scene/src/*.rs` | WP0 | WP0 PR "entity core" | WP3, WP4, WP6, WP7, WP8 |
| `Op`, `Transaction`, `Origin`, `History`, `CommandRegistry<C>`, `EditTarget`, `CommandSpec`, `Tier`, `CommandOutcome`, `PolicyCheck`, `remote::{channel, EngineHandle, EngineReceiver, EngineRequest}` | `crates/re_command/src/*.rs` | WP0 | WP0 PR "entity core" | WP3, WP4, WP6, WP7, WP8 |
| `compile_check(source, chunk_name) -> Vec<Diagnostic>` | `crates/re_script/src/lib.rs` | WP3 | WP3's first PR | WP7 (`script.write`) |
| `corpus::run`, `scan`, `types::run` | `crates/re_script_check/src/` | WP3 | WP3 exit | WP6 (`real check`, verify stage 3) |
| `Gpu`, `Renderer` (`render`, `paint_ui`, `upload_*`, `create_material`, `release_*`, `stats`), `RendererSettings`, `UiFrame`, `Capture` | `crates/re_render/src/` | WP1 | WP1 exit | WP6, WP8 |
| `Physics` (`new`, `config`, `apply`, `step`, `poses_into`, `velocity`, `pose`, `is_awake`, `character`, `events`, `take_diagnostics`, `body_count`, `character_count`, `jobs_queued`, `save_state` / `restore_state`, `motion_state` / `apply_motion_state`, queries `raycast` / `shape_cast` / `overlap_sphere`), `PhysicsConfig`, `LayerTable`, `QueryFilter`, `PhysicsSnapshot`, `MotionState`, `DIAGNOSTICS`, `codes` | `crates/re_physics/src/` | WP2 | WP2 exit | WP6 |
| `ProcGraph`, `NodeRegistry`, `Evaluator`, `EvalReport`, `GraphStore`, `register_generator`, `register_commands` | `crates/re_proc/src/` | WP4 | WP4 exit | WP6 |
| `AssetServer`, `load_material`, `AssetWatcher`, `RawInput`, `ActionMap`, `InputConfig`, `Replay`, `Audio`, `Sampler`, `Tween` | WP5 crates | WP5 | WP5 exit | WP6 |
| `App::open`, `App::serve(EngineReceiver, &dyn PolicyCheck)`, `AppConfig`, `verify::{run, VerifyStage}` | `crates/re_app/src/` | WP6 | WP6 milestone "app" | WP7, WP8 |
| `ScriptRuntime` (construction with a `HostServices`, the `game` and `tools` contexts, the per-phase entry point, `reload`), with signatures in rustdoc | `crates/re_script/src/` | WP3 | WP3 exit | WP6 |
| `ReplayRecorder` | `crates/re_input/src/` | WP5 | WP5 exit | WP6 |
| Tooling stubs: `McpArgs`, `AgentArgs`, `EditorArgs` and `run(args, project) -> anyhow::Result<ExitCode>` | `crates/re_cli/src/tooling/{mcp,agent,editor}.rs` | WP6 | WP6 milestone "app" | WP7, WP8 (fill them in) |
| Editor socket protocol: newline-delimited JSON, `{ "id", "command", "args", "dry_run" }` → `{ "id", "outcome" }`; `re_agent::remote::{connect, serve}` | `crates/re_agent/src/remote.rs` (client and the server loop), `re_editor` (hosts the server) | WP7 | WP7's first PR | WP6, WP8 |
| Single-writer lock: `re_agent::lock::{ProjectLock, acquire, probe, LockState}`, code `RE8003` | `crates/re_agent/src/lock.rs` | WP7 | WP7's first PR | WP6 (`real do`), WP7 (`real mcp`), WP8 (`real editor`) |

`re_mcp` never constructs the engine. `re_cli` opens the project with `re_app::App::open`, creates an `EngineHandle` / `EngineReceiver` pair, runs `App::serve` on the main thread, and hands the `EngineHandle` to `re_mcp::serve_stdio` on a background thread. So `re_mcp` depends only on `re_agent` and `re_command`, and `re_app` never links tokio.

---

## WP0: Workspace, CI, contracts and the entity core

**Goal.** Give every other WP a buildable workspace with green CI on Linux, Windows and macOS, the frozen contract crates, and the entity core (component registry, scene, commands with undo) ported from the core spike onto bevy_ecs 0.20.0. This is the foundation; keep it to porting and wiring, not new design.

**Owned paths**
- `Cargo.toml` (workspace root), `Cargo.lock`, `rust-toolchain.toml`, `deny.toml`, `.cargo/config.toml`, `.gitignore`
- `.github/` (the CI workflow, the composite setup action and the PR template)
- `README.md`, and `docs/` except `docs/research/scripting.md`
- `tools/xtask/`, `tools/derive_alias_check/`
- `schemas/` (generated; see the ownership rule)
- `crates/re_core/`, `crates/re_noise/`, `crates/re_types/`
- `crates/re_reflect/`, `crates/re_scene/`, `crates/re_command/`

**Depends on:** nothing. It consumes no frozen interface; it defines the first ones.

**Do not add a `LICENSE` file.** Real Engine is proprietary, as the owner settled on 2026-10-27 ([DECISIONS.md, open question 1](DECISIONS.md#open-questions-for-the-owner)): crates declare no license and set `publish = false`. Until that decision they declared `license = "MIT OR Apache-2.0"`, the render prototype's value.

### Reuse

All paths are in the private research folder `/mnt/project-files/real-engine/research/` (not in this repository).

**Core spike** (report `research/spikes/core/core.md`; code in `research/spikes/core/code/re_core/`, a crate that the spike also called `re_core` — it is **not** our `re_core`, which holds diagnostics and time). Read the whole report first: its sections 4 (what the prototype does), 5 (API cheat-sheet), 6 (measurements) and 7 (gotchas) are the specification for this part.

| From (`research/spikes/core/code/…`) | Into |
|---|---|
| `re_core/Cargo.toml` | The bevy_ecs feature set (`default-features = false`, `std`, `multi_threaded`) and `serde_json`'s `float_roundtrip` |
| `re_core/src/value.rs` (`FieldType`, `Value`, `Color`, coercion, `FieldValue`, `field_enum!`) | `re_reflect::value` |
| `re_core/src/registry.rs` (`Registry`, `register`, `register_script` with `ComponentDescriptor::new_with_layout`, `references_to`, `set_field` with the equality check, `component!`) | `re_reflect::registry`. Rename the spike's registry `Origin` to `ComponentOrigin` (the command `Origin` keeps its name). |
| `re_core/src/doc.rs` (`Doc`, `to_value`, `from_value`, `f32_from_parsed_f64`) | `re_reflect::doc` |
| `re_core/src/schema.rs` (`describe`, `scene_json_schema`) | `re_reflect::schema` for the registry part; `re_scene::World::describe` adds generators |
| `re_core/src/error.rs` (`CoreError`, `suggest`) | Error enums in each crate, converted to `re_core::Diagnostic` with a code. **Keep every message text verbatim** (they are listed in the report's §5); only the code is new. |
| `re_core/src/ids.rs` (`StableId` with `on_insert` / `on_discard` hooks, `IdIndex`, `IdGen`) | `re_scene::ids` |
| `re_core/src/components.rs` (`Transform requires(GlobalTransform)`, `GlobalTransform`, `Name`) | `re_scene::components`, extended to the built-in table below. The spike's `Primitive`, `PointLight`, `Weld` and `PillarGrid` move to `crates/re_scene/tests/support/` as test-only components. |
| `re_core/src/hierarchy.rs` (`parent`, `children`, `subtree`, `is_self_or_ancestor`, `propagate_all`, `propagate_changed` with `RemovedComponents<ChildOf>`) | `re_scene::hierarchy` |
| `re_core/src/scene.rs` (`SceneDoc`, `EntityDoc`, `SaveOptions { omit_defaults }`, `save`, `load`, `to_json`, `from_json`, `collapse_scalar_lists`) | `re_scene::scene`. Drop `to_ron` / `from_ron` and the `ron` dependency. |
| `re_core/src/procedural.rs` (`Procedural`, `Generated`, `Overrides`, `OverrideDoc`, `GenNode`, `GenContext::random`, `Generators`, `regenerate_changed`, `regenerate`) | `re_scene::procedural`, changed as described in "Build" (keep `Entity` for surviving keys, deletion overrides, suppression) |
| `re_core/src/core.rs` (`Core { world, registry, generators }`, `update`, `get`, `get_doc`, `entity`, `id_of`, `new_id`) | `re_scene::world::World` (fields `ecs`, `registry`, `generators`) |
| `re_core/src/commands.rs` (`Command`, `Origin`, `Transaction`, `History::{execute, undo, redo, log_since_jsonl}`, `apply`, inverse building, inbound-reference repair on despawn) | `re_command::op` (the spike's `Command` enum becomes `Op`) and `re_command::history` |
| `re_core/tests/core.rs` (15 tests) | Split by crate into `crates/re_reflect/tests/`, `crates/re_scene/tests/`, `crates/re_command/tests/` (list under "Acceptance") |
| `re_core/examples/bench.rs` | `crates/re_scene/examples/core_bench.rs`, with a `--check` flag (see "Acceptance") |
| `re_core/examples/demo.rs` | `crates/re_command/examples/agent_demo.rs`, writing `scene.json`, `registry.json`, `scene.schema.json` and `log.jsonl`. Clean port: rename the source id `arena` to `yard` (and `Arena` to `Yard`) here, in its snapshot references and in the tests that use it. |
| `re_core/examples/f32_exhaustive.rs` | `crates/re_reflect/examples/f32_exhaustive.rs` (manual only: 2,170 s on 4 threads in the spike) |
| `re_core/examples/ron_scaling.rs`, `ron_probe.rs` | Not ported |
| `out/scene.json`, `out/scene.compact.json`, `out/registry.json`, `out/scene.schema.json`, `out/log.jsonl` | Format references for the snapshot tests |
| `comparisons/ecs/src/bevy/src/features.rs`, `comparisons/ecs/logs/` | Reference only: bevy_ecs 0.20 hooks, observers, relationships and dynamic components as compiled by the spike |
| `research/spikes/core/core.md` | `docs/research/core.md`, published with these changes: in §8, delete the whole introductory paragraph (the sentence about the two sample scenes and the class and API lists under it) and the use count in the attributes row; keep only the mapping table and the host comparison. In §8's `ChildAdded` row, replace "observers or hooks on `ChildOf`" with "observers on `ChildOf` insert/discard" (bevy's relationship already owns `ChildOf`'s hooks; ARCHITECTURE §4). In §5's examples, rename the id `arena` to `yard` and the name `Arena` to `Yard`, matching `agent_demo`. |

**Other spikes**

| From | Into |
|---|---|
| `research/spikes/platform/code/re_noise/` (src, tests, examples) | `crates/re_noise/`, unchanged apart from the crate metadata. Its 14 tests (`tests/golden.rs` with the cross-architecture goldens, `tests/properties.rs`) come along. |
| `research/spikes/platform/code/re_platform_spike/src/timestep.rs` and `tests/timestep.rs` | `re_core::time` (`FixedClock`) |
| `research/spikes/platform/code/re_platform_spike/src/log.rs` and `tests/log.rs` | `re_core::log` |
| `research/spikes/ai/code/src/engine.rs` (`Tier`, `CommandSpec`, `CommandOutcome`, `CommandError`, `ErrorCode`, `Engine::dispatch`, dry runs) | The pattern for `re_command::registry` |
| `research/spikes/physics/code/oxijolt-spike/src/bridge.rs` (`EntityId`) | The shape of `re_types::EntityId` |

### Build

**PR "contracts"**

1. **Workspace.**
   - `members = ["crates/*", "tools/*"]` (a glob, because `tools/derive_alias_check` only appears in PR "entity core"), `resolver = "3"`. `examples/island` is a project directory (data and Luau only), not a crate, so it is **not** a member.
   - `[workspace.package]` sets edition 2024, `rust-version = "1.95"` and `publish = false` (and, until the owner's license decision of 2026-10-27, `license = "MIT OR Apache-2.0"`).
   - `[workspace.dependencies]` holds every crate in DEPENDENCIES.md with `=` versions and the listed features, including `bevy_ecs = { version = "=0.20.0", default-features = false, features = ["std", "multi_threaded"] }` and `serde_json = { version = "=1.0.154", features = ["preserve_order", "float_roundtrip"] }`.
   - The dev profile uses `debug = "line-tables-only"`.
   - `.cargo/config.toml` defines `[alias] xtask = "run --locked -p xtask --"`.
   - `deny.toml`: the license allow-list and per-crate exceptions of DEPENDENCIES.md (policy 4), `[licenses] private = { ignore = true }` for our own unpublished crates, bans on a second copy of `wgpu`, `winit`, `egui`, `mlua`, `bevy_ecs` or `glam` 0.34 inside our crates, `[graph] targets` limited to the three desktop triples, `[bans] multiple-versions = "warn"`, and the duplicate skip-list of DEPENDENCIES.md ("Known duplicate versions"). Skip entries for crates not yet in the tree only produce warnings.
   - `.gitignore` adds `out/`, `**/.real/*` and `!**/.real/agent-policy.toml` (the `**/` matters: a pattern with an inner `/` is otherwise anchored to the repository root; so a project's committed agent policy, such as `examples/island/.real/agent-policy.toml`, is tracked while the rest of `.real/` is ignored), keeping `Cargo.lock` tracked.
2. **`tools/xtask`** (a plain binary crate using `cargo metadata` output through `serde_json`; no engine crates):
   - `cargo xtask check-graph`: the crate dependency rules of ARCHITECTURE §2 ("must not depend on" column), including "subsystem crates reach only the contract layer", "`re_proc` and `re_geom` reach no wgpu, oxijolt or mlua", "only `re_reflect`, `re_scene` and `re_command` name `bevy_ecs`", and "`re_cli --no-default-features` reaches none of the tooling crates (`re_script_check`, `re_agent`, `re_mcp`, `re_editor`), tokio, rmcp and full_moon". Rules about crates that do not exist yet pass vacuously.
   - `cargo xtask dupes` (the duplicate gate): `cargo tree -d -e normal --workspace --all-features --target <triple>` (all features, as `deny.toml` `[graph]`) for each of `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc` and `aarch64-apple-darwin`, compared with the skip-list in `deny.toml` as **(name, version) pairs**. The same version listed twice (a host and a target build with different features) is not a duplicate.
   - `cargo xtask explain`: scans `crates/*/src/**/*.rs` for string literals matching `"RE[0-9]{4}"`; each code must have `crates/<crate>/explain/RE####.md`, lie inside the crate's range (the table in ARCHITECTURE §7, copied into xtask), and be used by one crate only; each page must be referenced by a string literal ending in `explain/RE####.md` (the `include_str!` of its `DIAGNOSTICS` entry) in the crate's sources. Paths in its messages use `/` on every OS.
   - `cargo xtask schemas [--check]`: if `crates/re_cli` exists, runs `cargo run --locked -p re_cli -- schema --out schemas/`; with `--check` it fails when the committed files differ. Until `re_cli` exists it prints `skipped: re_cli not built yet`.
   - `cargo xtask check` runs `check-graph`, `dupes`, `explain` and `schemas --check`.
3. **CI**: `.github/workflows/ci.yml` plus a composite action `.github/actions/linux-setup/action.yml`, and a PR template `.github/pull_request_template.md` (WP, owned paths touched, acceptance output, doc changes requested). The workflow, in full:
   - Triggers: `pull_request`, and `push` to `main`. `concurrency: { group: ci-${{ github.ref }}, cancel-in-progress: true }`. Env: `CARGO_TERM_COLOR=always`, `CARGO_BUILD_JOBS=2`.
   - `linux-setup` (composite): `sudo apt-get update`, then `sudo apt-get install -y` with exactly the package list in DEPENDENCIES.md "System packages" (it includes `mesa-vulkan-drivers`, which provides lavapipe for the GPU tests, the EGL and Mesa GL packages that the GL backend loads on headless Linux, and `xvfb`); then `rustup toolchain install stable --profile minimal --component rustfmt --component clippy`; then `Swatinem/rust-cache@v2`.
   - Jobs (all `runs-on: ubuntu-latest` unless stated; each starts with `actions/checkout@v7`):

     | Job | Steps after checkout | Required for merge |
     |---|---|---|
     | `fmt` | `rustup toolchain install stable --profile minimal --component rustfmt`; `cargo fmt --all --check` | yes |
     | `clippy` | `linux-setup`; `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` (all features, so feature-gated code such as `re_core`'s `profile-puffin` and `profile-tracy` is compiled and linted; the System packages include the C++ toolchain the Tracy client needs) | yes |
     | `test` | `linux-setup`; install luau-lsp (below); `cargo test --workspace --locked --exclude re_render --exclude re_app --exclude re_cli --exclude re_editor` (cargo only warns about excluded packages that do not exist yet); then the GPU step below | yes |
     | `msrv` | `linux-setup`; `rustup toolchain install 1.95.0 --profile minimal`; `cargo +1.95.0 build --workspace --all-targets --locked` (all targets, so tests, examples and dev-dependencies also build with the MSRV) | yes |
     | `deny` | `rustup toolchain install stable --profile minimal`; `cargo install cargo-deny --version 0.20.2 --locked`; `cargo deny check` | yes |
     | `xtask` | `linux-setup`; `cargo xtask check` | yes |
     | `verify-demo` | a guard step `echo "present=$([ -f examples/island/real.toml ] && echo true || echo false)" >> "$GITHUB_OUTPUT"`; every later step has `if: steps.guard.outputs.present == 'true'`: `linux-setup`, luau-lsp, `cargo run --locked --release -p re_cli -- verify --project examples/island --json` | yes (passes as skipped until WP6) |
     | `xvfb-smoke` | the same guard; `linux-setup`; `cargo build --locked --release -p re_cli`; `xvfb-run -a -s "-screen 0 1280x720x24" target/release/real run --project examples/island --frames 300 --exit-after` | yes (skipped until WP6) |
     | `build-windows` | `runs-on: windows-latest`, `continue-on-error: true`; `rustup toolchain install stable --profile minimal`; `Swatinem/rust-cache@v2`; `cargo build --workspace --locked`; the pure-Rust test step below (`shell: bash`) | no (reported) |
     | `build-macos` | the same on `macos-latest` | no (reported) |

   - The GPU step of `test`:
     ```sh
     pkgs=""
     for c in re_render re_app re_cli re_editor; do
       if [ -d "crates/$c" ]; then pkgs="$pkgs -p $c"; fi
     done
     if [ -z "$pkgs" ]; then echo "no GPU crates yet"; exit 0; fi
     vulkaninfo --summary || true          # records the runner's Mesa/lavapipe version in the log
     cargo test --locked $pkgs -- --test-threads=1
     if [ -d crates/re_render ]; then
       RE_BACKENDS=gl cargo test --locked -p re_render -- --test-threads=1
     fi
     ```
     (`vulkaninfo` comes from `vulkan-tools`, which is in the System packages list.) WP1 records the Mesa version it recorded the goldens with in `crates/re_render/tests/goldens/MESA_VERSION`; when CI's Mesa differs and a golden fails by more than the tolerance, WP1 (or the maintainer) re-records it from CI's output rather than widening the tolerance.
   - The pure-Rust test step of `build-windows` and `build-macos` (`shell: bash` on both, so it runs the same on Windows). Like the GPU step, it names only crates that exist, because `cargo test -p` fails on a package that does not exist yet (unlike `--exclude`, which only warns). In PR "contracts" it tests `re_core`, `re_noise` and `re_types`; from PR "entity core" on it also tests the model crates:
     ```sh
     pkgs=""
     for c in re_core re_noise re_types re_reflect re_scene re_command; do
       if [ -d "crates/$c" ]; then pkgs="$pkgs -p $c"; fi
     done
     cargo test --locked $pkgs
     ```
   - The luau-lsp step (verified 2026-10-14: the zip holds a single `luau-lsp` binary):
     ```sh
     curl -fsSL -o /tmp/luau-lsp.zip https://github.com/JohnnyMorganz/luau-lsp/releases/download/1.70.1/luau-lsp-linux-x86_64.zip
     echo "1a2ea1ae4f98f8946cefd970a4b54853e11ab4775e6932faa7f60cf920346567  /tmp/luau-lsp.zip" | sha256sum -c -
     sudo unzip -o /tmp/luau-lsp.zip -d /usr/local/bin && sudo chmod +x /usr/local/bin/luau-lsp
     ```
   - The action versions: `actions/checkout@v7` and `Swatinem/rust-cache@v2`, the newest majors, verified 2026-10-14 with `git ls-remote --tags` (`refs/tags/v7` of actions/checkout is `3d3c42e5aac5ba805825da76410c181273ba90b1`; `refs/tags/v2` of Swatinem/rust-cache is `49a0bdc70d2e1b713ca9e2869b211fcce03d3c1c`).
   - After the first green run on `main`, the maintainer marks the eight Linux jobs as required status checks in the repository's branch protection. (Not done: `main` has no branch protection, and ADR-0023's merge rule says which checks must be green.)
4. **`re_core`** (modules `diag`, `time`, `phase`, `task`, `events`, `profile`, `log`):
   - `Diagnostic { code, severity, message, location, hint, related }`, `Severity`, `Code`, `Location` (file/line/column, entity id, or command name), `DiagInfo { code, title, page }`, and `render_human` / `render_json`;
   - `FixedClock` (integer nanoseconds, a 5-step cap) and the `Phase` enum, in the order of ADR-0009;
   - `TaskPool`, a wrapper around one `rayon::ThreadPool`, with `global()`, `spawn`, `scope` and `install`. Its threads are named `re-worker-N` (N from 0, through `ThreadPoolBuilder::thread_name`), so tests can tell them apart from Jolt's or bevy's threads;
   - `Events<T>` (typed, double-buffered);
   - `profile_scope!`, with features `profile-puffin` and `profile-tracy`;
   - `init_logging(LogConfig)`.
5. **`re_noise`**, ported unchanged.
6. **`re_types`** (**frozen** when this PR merges). It depends only on `glam`, `serde` and `serde_json`, plus `schemars` behind an optional `schemars` feature that derives `JsonSchema` for the file formats it defines (`GraphDoc`, `ReplayDoc` and their parts), so that `real schema` (WP6) generates `graph.schema.json` and `replay.schema.json` from the frozen types; it does **not** depend on `re_core` (errors cross `HostServices` as the plain `HostError` below, and callers turn them into `Diagnostic`s). Every data type is `Send + Clone + Serialize + Deserialize`. One module per area, all re-exported at the crate root:
   - `ids.rs`: `EntityId(pub u64)` (it carries `bevy_ecs::entity::Entity::to_bits()`; only `re_scene` converts);
   - `render.rs`: `RenderSnapshot { camera, lights, items, debug_lines, clear }`, `Camera`, `Light`, `DrawItem { entity, mesh, material, world: Mat4, flags }`, `DrawFlags`, `DebugLine`, `MeshHandle`, `MaterialHandle`, `TextureHandle`, `TextureRef` (`Asset(asset id string)` or `Handle(TextureHandle)`: `re_assets` cannot hold renderer handles, so it fills material textures with `Asset` and `re_app` resolves them to `Handle` before `Renderer::create_material`), `MaterialDesc`;
   - `mesh.rs`: `MeshData` (vertex layout `{pos, normal, uv, color}`, `u32` indices) and `Aabb`. They live here, not in `re_geom`, so that `re_render` and `re_assets` depend only on the contract layer;
   - `anim.rs`: `ClipData` (translation, rotation and scale channels per target), produced by `re_assets` and sampled by `re_anim`;
   - `physics.rs`: `PhysicsCmd`, `BodyDesc`, `BodyKind`, `ShapeDesc` (box, sphere, capsule, convex hull, triangle mesh, heightfield with `samples[z*n + x]`), `Layer`, `GroupMask`, `BodyPose { id, pos, rot }` (no velocities, see ADR-0008), `ContactEvent`, `RayHit`, `CharacterDesc` (height, radius, max slope, step height, mass, and `inner_body: bool` so that the character takes part in contact events), `CharacterState`, `ConstraintDesc { a, b, kind }` with `ConstraintKind::{Fixed, Hinge, Distance}` (the M1 set), carried by `PhysicsCmd::AddConstraint { id, desc }` / `RemoveConstraint { id }`, where `id` is the constraint's own entity; removals of constraints sort before removals of bodies, additions after additions of bodies;
   - `audio.rs`: `AudioCmd`;
   - `input.rs`: `ActionFrame` (the state of every action this frame);
   - `graph.rs`: `GraphDoc`, `NodeDoc`, `EdgeDoc`, the `*.graph.json` form, which `re_proc` turns into the `ProcGraph` of ARCHITECTURE §5:
     ```rust
     pub struct GraphDoc { pub format: String /* "real-engine/graph" */, pub version: u32 /* 1 */,
                           pub name: String, pub seed: i64 /* default; a source's Procedural.seed wins */,
                           pub nodes: Vec<NodeDoc>, pub edges: Vec<EdgeDoc> }
     pub struct NodeDoc { pub id: String, pub kind: String /* "terrain.heightfield" */, pub version: u32,
                          pub params: serde_json::Map<String, serde_json::Value> }
     pub struct EdgeDoc { pub from: (String, String) /* (node id, port) */, pub to: (String, String) }
     ```
     On disk: `{ "format": "real-engine/graph", "version": 1, "name": "island", "seed": 1234, "nodes": [{ "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 256 } }], "edges": [{ "from": ["h", "field"], "to": ["t", "a"] }] }`. Node ids are unique; `nodes` keep file order; unknown keys are errors;
   - `replay.rs`: `ReplayDoc` (ARCHITECTURE §8, "Replay file");
   - `host.rs`: the `HostServices` trait and its error type, with these exact signatures (`Vec3` is `glam::Vec3`):
     ```rust
     pub struct HostError { pub code: &'static str /* an RE#### code of the implementing crate */, pub message: String }
     pub type HostResult<T> = Result<T, HostError>;
     pub trait HostServices {
         fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32, mask: GroupMask) -> Option<RayHit>;
         fn apply_impulse(&mut self, entity: EntityId, impulse: Vec3) -> HostResult<()>;
         fn character_move(&mut self, entity: EntityId, desired_velocity: Vec3, jump: bool) -> HostResult<()>;
         fn character_state(&self, entity: EntityId) -> HostResult<CharacterState>;
         fn body_velocity(&self, entity: EntityId) -> HostResult<(Vec3, Vec3)>; // (linear, angular)
         fn play_sound(&mut self, clip: &str, at: Option<Vec3>, volume: f32) -> HostResult<()>;
         fn actions(&self) -> &ActionFrame;
         fn set_hud_text(&mut self, entity: EntityId, text: &str) -> HostResult<()>;
         fn request_screenshot(&mut self, path: &str) -> HostResult<()>;
         fn regenerate(&mut self, source_id: &str, seed: i64) -> HostResult<()>;
         fn request_restart(&mut self);
     }
     ```
     `re_app` implements it and owns the codes (RE05xx); `re_script` turns a `HostError` into a Luau error carrying the code.

**PR "entity core"** (ADR-0003, ADR-0005, ADR-0016, ADR-0017, ADR-0032)

7. **`re_reflect`** (depends on `re_core`, `bevy_ecs`, `glam`, `serde`, `serde_json`, `indexmap`, `thiserror`):
   - The closed field-type set `bool`, `i32`, `i64`, `f32`, `f64`, `vec3`, `quat`, `color`, `string`, `entity` (nullable) and unit enums; the dynamic `Value`; the coercion rules (integer → float; integral float → integer; f64 → f32 with a range check; non-finite rejected; quaternions normalised; enums checked against their variants).
   - `Doc`, the untyped tree used at every text boundary: accepts `[x, y, z]` or `{x, y, z}` for vectors and 3 or 4 numbers for colours; numbers parsed through `f32_from_parsed_f64` for f32 fields.
   - `Registry::{register::<T>, register_script, key, schema, component_id, schemas, components_of, references_to, has, get_field, get_all, set_field, insert, remove}`; script components stored as a `Vec<Value>` behind a real `ComponentId` (`ComponentDescriptor::new_with_layout` takes 8 arguments in 0.20; `summary_tick` must be false unless storage is `Table`).
   - `component!` and `field_enum!` macros (keep the spike's `requires(...)` clause; it avoids a macro_rules ambiguity, gotcha 7). No derive crate.
   - `Registry::describe() -> serde_json::Value` and `Registry::scene_json_schema() -> serde_json::Value` (JSON Schema 2020-12).
   - Codes RE1000–1199 (unknown component or field, type mismatch, out-of-range f32, non-finite float, unknown enum variant, duplicate component name).
8. **`re_scene`** (depends on `re_reflect`, `re_types`, `re_core`, `bevy_ecs`, `glam`, `serde`, `serde_json`, `indexmap`, `blake3`, `thiserror`). It re-exports `bevy_ecs::entity::Entity` as `re_scene::Entity`. Its public API:
   - `pub use bevy_ecs as ecs;` so that other crates write typed queries without depending on bevy_ecs themselves (`cargo xtask check-graph` enforces that). **Bevy derives in other crates:** `#[derive(Component)]` and `#[derive(Resource)]` expand to a bare `bevy_ecs::` path when the crate does not depend on `bevy_ecs` (bevy_macro_utils' fallback), so a crate that uses them (WP4's `GraphStore`, WP6's sync components and resources) puts `use re_scene::ecs as bevy_ecs;` in every module that uses the derives (a `use` is visible only in its own module, so the crate root alone covers only derives written there). The same holds for `re_reflect::component!`, which derives `Component`. `re_scene`'s crate docs say so, and a small workspace member `tools/derive_alias_check` (WP0; `publish = false`; it depends only on `re_scene` and `re_reflect`, never on `bevy_ecs`, and derives a `Component` and a `Resource` and declares `re_reflect::component!` types with a `requires(...)` clause through the alias, at the crate root and in a submodule, with one test that inserts and queries them) proves it compiles on every `cargo test --workspace`.
   - `World { pub ecs: bevy_ecs::world::World, pub registry: Registry, pub generators: Generators }` with `new()`, `entity(id)`, `id_of(entity)`, `new_id()`, `find(&self, path_of_names)`, `get(id, component, field) -> Value`, `get_doc`, `set(id, component, field, value)`, `regenerate_changed() -> RegenReport`, `propagate()` (incremental), `update()` (both), `describe()`, `scene_json_schema()`, `suppress(id)` (the full generated id `<source id>/<key>`, not a bare key, which is refused with RE1210), `despawn_runtime(entity)` (for game scripts: suppresses generated entities instead of recording an override), `state_hash() -> blake3::Hash`, and `fork(&self) -> World`. **`fork`** is the world copy that `play.run` and `graph.generate` run on: a fresh `World` with the same registry (built-in, Rust and script-defined components, script components registered first), the same generators, and every entity copied by value through the registry, **including generated entities** (with their `Generated` keys), overrides and the session's suppressions. Ids, `ChildOf` order and `state_hash` are identical. ECS resources are not copied, except `IdIndex`, which hooks rebuild; a caller that needs one of its own resources in the copy (for example WP4's `GraphStore`) clones it in. History is not copied.
   - Schedules use `Schedule::set_executor(SingleThreadedExecutor::new())` and no `par_iter`, so bevy_tasks' compute pool (default name `TaskPool (n)`, one thread per core, created lazily) never starts (ADR-0005, ADR-0007).
   - `ids`: `StableId` (immutable component; `#[component(immutable, on_insert = …, on_discard = …)]`; 0.20 calls the old `on_replace` hook `on_discard`), `IdIndex`, `IdGen` (ids like `e7kq2m9x4tb`; `/` reserved for generated ids; uniqueness checked).
   - `hierarchy`: helpers over bevy's `ChildOf` / `Children` (`add_child`, `insert_child(index, child)`, recursive despawn), cycle checks, `propagate_all`, `propagate_changed`.
   - `scene`: `SceneDoc { format: "real-engine/scene", version, script_components, entities }`, `EntityDoc { id, name, parent, components, overrides }`, `SaveOptions { omit_defaults }`, `World::save`, `World::load` (atomic: a failed load leaves the world untouched), `to_json`, `from_json`, `collapse_scalar_lists`, `migrate`. Order: roots sorted by id, then depth-first in `Children` order; components sorted by name; fields in declaration order. An optional `$schema` key is accepted and written back unchanged. **New compared with the spike:** a top-level `script_components` list holds the `ComponentSchema` of every script-origin component the saving world had registered (since #35; before, only those authored entities used); `load` registers them, or checks they match what is already registered (a mismatch is a diagnostic).
   - `prefab`: `PrefabFile` (the same document with `"format": "real-engine/prefab"`) and `PrefabFile::instantiate(&Registry, key_prefix) -> Result<SceneFragment, SceneError>`, whose keys are `<key_prefix>/<prefab entity id>` (the registry converts the field values; entity-typed fields must be null).
   - `procedural`: `Procedural { generator: string, seed: i64 }`, `GraphSource { graph: string, builder: string, params: string }`, `Generated { source, key }` (runtime only), `Overrides` with entries `{ target, component, field, value }` or `{ target, deleted: true }`, `GenNode { key, name, parent_key, components }`, `type SceneFragment = Vec<GenNode>`, `GenContext` (`param`, `random(key)`, and **new**: `resource::<R>()` read access to ECS resources), `Generators::register(GeneratorInfo { name, doc, params_component, run, fingerprint_extra })` (**new**: `fingerprint_extra: Option<fn(&GenContext) -> u64>`, so a generator can fold external input such as a graph document into the fingerprint), and `RegenReport { sources, spawned, updated, despawned, orphans, released_assets, diagnostics }` (`diagnostics` holds generator failures, `RE1207`, and overrides that could not be applied, `RE1209`; orphans are data, not diagnostics).
   - **Changes to the spike's regeneration** (ADR-0017): apply the new fragment as a diff, so an entity whose key survives keeps its `Entity` and is updated in place; spawn new keys and despawn vanished ones; list `MeshRef` / `MaterialRef` values that are no longer referenced in `released_assets`; re-apply overrides by key; skip keys with a deletion override or a suppression; keep orphans and report them; a failing generator leaves the old output in place.
   - Built-in components, registered by `register_builtins` (field names, types and starting defaults below; **frozen at the "entity core" PR**, after review by the consuming WP named in the last column):

     | Component | Fields (type = default) | Reviewed by |
     |---|---|---|
     | `Transform` (requires `GlobalTransform`) | `translation: vec3 = 0`, `rotation: quat = identity`, `scale: vec3 = 1` | all |
     | `MeshRef` | `asset: string = ""` (an asset path, `builtin:cube`, `builtin:sphere`, `builtin:plane`, `builtin:capsule`, or `derived:<hash>`) | WP6 |
     | `MaterialRef` | `asset: string = ""` (a `materials/*.material.json` path) | WP6 |
     | `Visible` | `visible: bool = true` | WP6 |
     | `Camera` | `fov_y_deg: f32 = 60`, `near: f32 = 0.1`, `far: f32 = 1000`, `active: bool = false` | WP6 |
     | `Light` | `kind: enum {Directional, Point} = Point`, `color: color = white`, `intensity: f32 = 800`, `range: f32 = 20`, `shadows: bool = false` | WP6 |
     | `RigidBody` | `kind: enum {Static, Dynamic, Kinematic} = Static`, `mass: f32 = 1`, `friction: f32 = 0.5`, `restitution: f32 = 0`, `linear_damping: f32 = 0.05`, `angular_damping: f32 = 0.05` | WP2 |
     | `Collider` | `shape: enum {Box, Sphere, Capsule, Convex, Mesh, Heightfield} = Box`, `half_extents: vec3 = 0.5`, `radius: f32 = 0.5`, `half_height: f32 = 0.5`, `source: string = ""` (mesh or heightfield asset for Convex, Mesh, Heightfield), `layer: i32 = 0` (valid 0..=65535), `groups: i64 = 1` (bit mask, valid 0..=u32::MAX), `sensor: bool = false`, `enabled: bool = true` | WP2 |
     | `CharacterController` | `height: f32 = 1.8`, `radius: f32 = 0.4`, `max_slope_deg: f32 = 45`, `step_height: f32 = 0.3`, `mass: f32 = 70`, `inner_body: bool = true`, `layer: i32 = 0` (valid 0..=65535), `groups: i64 = 1` (bit mask, valid 0..=u32::MAX) | WP2 |
     | `ScriptRef` | `path: string = ""`, `context: enum {Game, Tools} = Game` | WP3 |
     | `AudioSource` | `clip: string = ""` (a project asset path such as `assets/sfx/pickup.wav`, as in `AudioCmd`), `volume: f32 = 1`, `looping: bool = false`, `spatial: bool = true`, `autoplay: bool = false` | WP5 |
     | `AnimPlayer` | `clip: string = ""` (`<model asset path>#<clip name>`, e.g. `assets/models/door.glb#open`; the clip name is `ClipData.name`, the glTF animation name or `animation_<index>`), `speed: f32 = 1`, `looping: bool = true`, `playing: bool = true` | WP5 |
     | `HudText` | `text: string = ""`, `anchor: enum {TopLeft, Top, TopRight, Center, BottomLeft, Bottom, BottomRight} = TopLeft`, `size: f32 = 18` | WP6 |
     | `Procedural` | `generator: string = ""`, `seed: i64 = 0` | WP4 |
     | `GraphSource` | `graph: string = ""`, `builder: string = ""`, `params: string = ""` (per-node parameter overrides as canonical JSON, `{"<node id>": {"<param>": <value>}}`; a string because M1 has no map field type, ADR-0032) | WP4 |

     `Name` (the file's `name`), `StableId` (the file's `id`), `GlobalTransform`, `Generated` and the hierarchy are not registry components.

     Notes for WP6, which maps these components to `re_types` (ADR-0008):
     - The reflect field types have no `u16` or `u32`, so `layer` and `groups` are `i32` and `i64`. WP6 converts them to `Layer(u16)` and `GroupMask(u32)` (bit `g` is group `g`; the default `1` is group 0) and reports a value out of range as a diagnostic instead of truncating it. A layer that is in range but outside the world's `LayerTable` is refused by `re_physics` with `RE5015` (the standard table has 8 layers, `LayerTable::MAX_LAYERS` is 32).
     - A `Collider` without a `RigidBody` is a `Static` body.
     - `Collider.enabled = false` removes the body (`PhysicsCmd::RemoveBody`); it is not a flag on the body. Setting it back to `true` adds the body again.
     - `Mesh` and `Heightfield` shapes are static only: `re_physics` refuses them on `Dynamic` and `Kinematic` bodies with `RE5003`.
     - A heightfield `source` asset carries everything `ShapeDesc::Heightfield` needs: `n` (samples per side), `spacing`, `offset` and the `n × n` samples, not only the samples.
     - Playback time of an `AnimPlayer` is runtime state in `re_app`, not a component field.
   - Codes RE1200–1999 (unknown id, duplicate id, reserved `/`, cycle, parse errors naming the entity index, id, component and field, forward parent reference, generator failure, script-component schema mismatch).
9. **`re_command`** (depends on `re_scene`, `re_reflect`, `re_core`, `serde`, `serde_json`, `schemars`, `indexmap`, `thiserror`):
   - `Op` (the spike's `Command`): `spawn`, `despawn`, `set_field`, `add_component`, `remove_component`, `reparent`, `rename`, `set_override`, `restore`. Serde `tag = "op"`, `rename_all = "snake_case"`, `deny_unknown_fields`.
   - `Origin` (`User`, `Script(name)`, `Agent(name)`), `Transaction { label, origin, ops }`, `History::{execute, undo, redo, can_undo, can_redo, log, log_since_jsonl}`, `LogEntry` with kinds `do`, `undo`, `redo`, `rejected`.
   - **Generated entities** (ADR-0017): `set_field` on a generated entity also records an override in the same transaction; `despawn` of a generated entity records a deletion override (the spike refused it); `reparent`, `add_component` and `remove_component` on one are refused with the spike's hint ("change its source's generator parameters instead"), and so is `rename` (RE2003, "generated names come from the generator"), because no override can carry a name.
   - `CommandRegistry<C: EditTarget>`, where `trait EditTarget { fn world(&mut self) -> &mut re_scene::World; fn history(&mut self) -> &mut History; }`. `CommandSpec { name, summary, tier, input_schema, annotations }` (input schemas from schemars, root a plain object), `Tier { Read, Edit, Destructive }`, `CommandOutcome { ok, changes, diagnostics, data }`, `dispatch(&mut C, name, args, dry_run, &dyn PolicyCheck)`.
   - `dry_run`: every mutating command accepts `"dry_run": true`; the registry executes the transaction, records its `changes`, undoes it without touching the redo stack or the log, and returns the changes. A dry run needs only the `read` tier.
   - `PolicyCheck`, a trait (`fn check(&self, spec: &CommandSpec, args: &serde_json::Value, dry_run: bool) -> Result<(), Diagnostic>`) that `re_agent` implements; `AllowAll` for tests and the CLI.
   - `remote::{channel, EngineHandle, EngineReceiver, EngineRequest}`: a std channel carrying `(command, args, dry_run, origin)` requests with a reply sender for the `CommandOutcome`, plus `ListSpecs`. `EngineHandle::with_origin` sets the `Origin` the log records.
   - Built-in commands (`register_builtin_commands`): read tier `describe`, `query`, `entity.get`, `command.describe`; edit tier `entity.spawn`, `entity.set`, `entity.reparent`, `entity.delete`, `component.add`, `component.remove`, `scene.apply` (`{ "label"?: string, "ops": [Op] }`, one atomic transaction), `undo`, `redo`.
   - Codes RE2000–2999.
10. **`docs/research/core.md`**: the core spike report, published with the §8 change in the Reuse table.

### Interfaces this WP freezes

Everything in the first six rows of "Cross-WP contracts". Consumers find them at `crates/re_core/src/`, `crates/re_types/src/`, `crates/re_reflect/src/`, `crates/re_scene/src/` and `crates/re_command/src/`, with rustdoc on every public item.

### Non-goals

- Any subsystem (render, physics, scripting, procedural nodes, assets).
- A `#[derive]` macro, container field types (lists, maps), undo coalescing, persisting the log, a typed bulk-spawn path, RON support, scene comments.
- A `LICENSE` file.

### Acceptance (headless)

```sh
export CARGO_BUILD_JOBS=2
mkdir -p out
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo test --locked -p re_noise --test golden          # cross-architecture noise goldens from the spike
cargo xtask check
cargo deny check
cargo +1.95.0 build --workspace --all-targets --locked
cargo run --locked -p re_scene --release --example core_bench -- --check
cargo run --locked -p re_command --example agent_demo -- out/demo   # writes scene.json, registry.json, scene.schema.json, log.jsonl
```

Required tests, PR "contracts":
- `FixedClock` gives at most 5 steps per frame and never drifts over 10,000 frames;
- every `re_types` data type round-trips through serde_json;
- `RenderSnapshot` is `Send` (a compile-time assertion);
- the ported `re_noise` tests (14) and `re_core` log and timestep tests pass;
- `cargo xtask explain` fails on a fixture with a missing page (unit test inside xtask).

Required tests, PR "entity core" (the ported spike tests keep their names):
- `re_reflect`:
  - `by_name_access_and_coercion`: an f64 `0.1` from a script reads back as f32 `0.1`, and the error texts of the report's §5 cheat-sheet are reproduced **verbatim** (unknown field with "did you mean", unknown component with "did you mean", unknown enum variant, wrong vector arity);
  - `double_rounding_f32_is_recovered`: the f32 values with bits `0x15ae43fd` and `0x95ae43fd` (`7.038531e-26` and its negative) survive JSON text → f64 → f32 exactly (**the spike's first fixed bug**);
  - `every_f32_survives_text_via_f64`, over a deterministic sample of at least 1,000,000 bit patterns plus the two values above (the exhaustive run is the manual `f32_exhaustive` example);
  - `schema_lists_components_fields_and_docs`;
  - `equal_writes_do_not_trigger_change_detection`.
- `re_scene`:
  - `script_components_are_real_ecs_components` (visible through `contains_id`, saved and reloaded byte-identically, survives despawn and undo), extended: the saved file carries `script_components`, and loading it into a fresh `World` with no script run first succeeds;
  - `id_index_follows_hooks`;
  - `hierarchy_reparent_order_and_cycles`;
  - `transform_propagation_incremental_matches_full` (including removed `ChildOf`);
  - `scene_round_trip_is_byte_identical` (JSON);
  - `load_errors_name_the_place` (a failed load leaves zero entities);
  - a deletion override and a suppressed key both stop regeneration from spawning that key, and suppression is not saved;
  - a prefab instantiates into a fragment whose keys are prefixed by the given key;
  - `state_hash` is identical across two runs and changes when one float bit changes;
  - `fork` of a world with generated entities, overrides, a suppression and a script component has the same `state_hash`; editing the fork leaves the original's `state_hash` unchanged;
  - `tools/derive_alias_check` builds and its test passes;
  - after `update()` and a save/load, no thread named `TaskPool (n)` exists in the process (read `/proc/self/task/*/comm`; Linux only, `#[cfg(target_os = "linux")]`).
- `re_command`:
  - `undo_redo_restores_identical_scenes`: four transactions undone and redone give byte-identical saves at every step, including a redone generated id, **and an entity reference from outside a despawned subtree is restored by undo** (**the spike's second fixed bug**: before the fix the reference dangled and saved as `null`);
  - `failed_transaction_rolls_back`: a 4-operation transaction failing on operation 3 leaves the scene byte-identical and logs `rejected`;
  - `commands_are_plain_json_for_agents`: operations parse from the JSON of the report's §5; the spawn result carries the generated id; the log is JSON lines with the origin;
  - `despawn` of a generated entity records a deletion override, and undo removes it;
  - `procedural_sources_overrides_and_regeneration` (listed here because it needs `History` for undo; its source is the spike's procedural test), adapted: generated entities are not saved; an edit records an override and undo removes it; changing a parameter regenerates and keeps the override; same key and seed give the same values whatever the grid size; an orphaned override is kept and reported; **an entity whose key survives keeps its `Entity`**;
  - a dry run returns the same `changes` as the real call and leaves `state_hash`, the undo stack and the log unchanged;
  - `scene.apply` with an invalid third operation changes nothing and returns an `RE2xxx` diagnostic naming operation 2 (zero-based);
  - a `remote::channel` round trip from another thread returns the `CommandOutcome`.

`core_bench -- --check` builds the spike's 100,100-entity world (100 roots × 10 blocks × 99 leaves) and fails if any measurement exceeds **3×** the spike's figure, or if the JSON round trip is not byte-identical:

| Measurement | Spike (release, reference container) | Fail above |
|---|---|---|
| Direct spawn with ids and hooks | 173 ms | 519 ms |
| 100,100 `spawn` operations in one transaction | 1.03 s | 3.09 s |
| `propagate_all` | 5.5 ms | 16.5 ms |
| Incremental update, nothing changed | 0.42 ms | 1.26 ms |
| Incremental update after moving 1,000 leaves | 0.99 ms | 2.97 ms |
| Incremental update after moving 10 roots | 1.36 ms | 4.08 ms |
| 1M by-name reads (`World::get`) | 107 ms | 321 ms |
| 1M `set_field` with a resolved key | 36 ms | 108 ms |
| Save to `SceneDoc` / `SceneDoc` to JSON | 299 ms / 632 ms | 897 ms / 1.90 s |
| JSON to `SceneDoc` / load into the World | 477 ms / 486 ms | 1.43 s / 1.46 s |
| Despawn of a 1,001-entity subtree / its undo | 6.9 ms / 10.2 ms | 20.7 ms / 30.6 ms |

The bench is run by hand in the reference container on a near-idle machine and its output is pasted into the PR; it is not a CI job.

### Definition of done

Both PRs are green on every CI job, **including `build-windows` and `build-macos` for this WP's scope** (their steps must succeed even though they do not block later PRs), reviewed and merged to `main`; the acceptance commands pass; `docs/research/core.md` is published; the CI run on `main` after the second merge is green.

---

## WP1: Renderer

**Goal.** A headless-testable forward renderer in `re_render` that draws a `RenderSnapshot`, paints egui, uploads and releases GPU resources with generation-checked handles, and captures images, ported from the render prototype.

**Owned paths:** `crates/re_render/` (and `prototypes/render/`, which WP1 deleted)

**Starts after:** WP0 PR "contracts" is merged.

**Frozen interfaces consumed**
- `re_types::render::{RenderSnapshot, Camera, Light, DrawItem, DrawFlags, DebugLine, MeshHandle, MaterialHandle, TextureHandle, TextureRef, MaterialDesc}` (`crates/re_types/src/render.rs`);
- `re_types::mesh::{MeshData, Aabb}` (`crates/re_types/src/mesh.rs`);
- `re_core::{Diagnostic, DiagInfo}` (`crates/re_core/src/diag.rs`).

**Reuse.** The prototype was in this repository at `prototypes/render/` until WP1 ported it into `crates/re_render/` and deleted it (the table below is the record of what went where). The same code and its logs and images are also in the private research folder at `/mnt/project-files/real-engine/research/spikes/render/` (`render.md`, `code/`, `images/`), which is not in this repository. The published findings are `docs/research/rendering.md`.

| File (`prototypes/render/…`) | Into |
|---|---|
| `src/gpu.rs` (`headless_instance`, `pick_limits`, `install_error_logger`) | `re_render::gpu` |
| `src/readback.rs` (`padded_bytes_per_row`, `read_texture`, `write_png_rgba8`, `image_stats`) | `re_render::capture` |
| `src/scene.rs` and `src/scene.wgsl` | The starting point for the forward pass |
| `src/gui.rs` | egui-wgpu painting, including the warm-up frame |
| `src/checks.rs` and `src/timing.rs` | Test helpers |
| `src/bin/imgdiff.rs` | `re_render::imgdiff` (library) plus the `re-imgdiff` example |
| `src/bin/windowed.rs` | `examples/windowed_smoke.rs` |
| `tests/render.rs` | The pattern for the headless tests |

Done: WP1's PR deleted `prototypes/render/` once `re_render`'s tests covered what its tests covered, and the README section about it was replaced by "Running the renderer tests".

**Build**
- `Gpu::headless(backends) -> Result<Gpu, Diagnostic>` and `Gpu::for_surface(Arc<W>, backends) -> Result<(Gpu, wgpu::Surface<'static>), Diagnostic>` (the window is usually an `Arc<winit::window::Window>`; configure the surface with `Gpu::surface_config`). Both install the uncaptured-error logger, which reports `RE6001` and never panics.
- `Renderer::new(&Gpu, format)`.
- `Renderer::new(&Gpu, format)` uses the default `RendererSettings` (exposure, the sky and ground ambient colours, shadow map size and distance, MSAA); `Renderer::with_settings` and `settings_mut` change them.
- Resources: `upload_mesh(&MeshData) -> Result<MeshHandle, Diagnostic>`, `upload_texture(&TextureData) -> Result<TextureHandle, Diagnostic>`, `create_material(&MaterialDesc) -> Result<MaterialHandle, Diagnostic>` (handles only: a material whose `TextureRef` is still an `Asset` is rejected with a diagnostic, see `MaterialDesc::is_resolved`), and `release_mesh`, `release_texture`, `release_material`, each returning `Result<(), Diagnostic>`. Handles carry a generation, so a released handle is detected, not reused by mistake. `stats()` reports live counts and bytes per resource kind.
- `Renderer::render(&Device, &Queue, &TextureView, &RenderSnapshot) -> Result<FrameReport, Diagnostic>` (stale handles are skipped and listed in the `FrameReport`). It is a forward PBR-lite pass:
  - base colour, a texture, metallic and roughness;
  - one directional light with a single shadow map;
  - ambient light;
  - up to 8 point lights;
  - depth;
  - MSAA where the downlevel caps allow it.
- A debug-line pass.
- `Renderer::paint_ui(&Device, &Queue, &TextureView, &UiFrame) -> Result<(), Diagnostic>`, where `UiFrame { primitives: Vec<egui::ClippedPrimitive>, textures_delta: egui::TexturesDelta, size_px: [u32; 2], pixels_per_point: f32 }`. The caller tessellates (`UiFrame::from_output(&ctx, &mut full_output, size_px)` calls `egui::Context::tessellate`), because tessellation needs the context's font atlas.
- egui is painted into an `Rgba8Unorm` overlay and composited with a manual-gamma blit on every backend (one code path; GL has no `VIEW_FORMATS`), so the panel looks the same on GL and Vulkan (measured difference 0.000 levels).
- `Capture`: render into an offscreen texture, read it back (rows padded to 256 bytes) and write a PNG.
- A WGSL `#include` preprocessor (about 50 lines; no naga_oil).
- The projection uses glam 0.34's right-handed, DirectX-depth API, as the prototype did.
- Codes RE6000–6999 with explain pages.

**Interfaces this WP freezes:** `Gpu`, `Renderer`, `RendererSettings`, `UiFrame`, `Capture` (at WP1 exit; consumed by WP6 and WP8).

**Non-goals**
- A render thread.
- Cascaded shadows.
- Post-processing.
- Skinning.
- A GPU-driven pipeline.
- Texture compression.
- Shader hot reload.
- Static cells.

**Acceptance**

```sh
export CARGO_BUILD_JOBS=2
cargo test --locked -p re_render -- --test-threads=1
RE_BACKENDS=gl cargo test --locked -p re_render -- --test-threads=1
cargo build --locked -p re_render --example windowed_smoke
xvfb-run -a -s "-screen 0 1280x720x24" target/debug/examples/windowed_smoke --frames 120   # prints WINDOWED OK
cargo clippy -p re_render --all-targets --locked -- -D warnings
```

Required tests:
- golden images for: a lit cube grid, a shadowed plane, debug lines, an egui panel after the warm-up frame, and one textured mesh. Each backend has **its own golden set** (`tests/goldens/vulkan/` and `tests/goldens/gl/`), because GL lacks view formats and depth copies. Tolerance: at most 2 levels per channel, with an outlier budget of 0.1% of pixels;
- the GL egui panel's mean luminance is within a tolerance of the Vulkan panel's (the gamma fix works). WP1 records the tolerance it measured;
- readback is correct for a width that is not a multiple of 64;
- an injected validation error is logged as `RE6001`, and the process does not panic;
- one `MeshHandle` uploaded once and drawn 1000 times;
- uploading and releasing 100 meshes returns `stats()` to its starting counts, and a released handle is rejected with a diagnostic.

**Definition of done:** the common definition (PRs green on CI on lavapipe, merged to `main`), plus `prototypes/render/` deleted by the last PR (done).

---

## WP2: Physics

**Goal.** `re_physics`: Jolt through oxijolt behind a command-buffer API, deterministic for the same inputs on one machine, sharing the engine's single rayon pool, ported from the physics spike.

**Owned paths:** `crates/re_physics/`

**Starts after:** WP0 PR "contracts" is merged.

**Frozen interfaces consumed**
- `re_types::physics::{PhysicsCmd, BodyDesc, BodyKind, ShapeDesc, Layer, GroupMask, BodyPose, ContactEvent, RayHit, CharacterDesc, CharacterState}` (`crates/re_types/src/physics.rs`);
- `re_types::ids::EntityId` (`crates/re_types/src/ids.rs`);
- `re_core::{TaskPool, Diagnostic, DiagInfo}` (`crates/re_core/src/task.rs`, `diag.rs`).
- For review only: the `RigidBody`, `Collider` and `CharacterController` field table in WP0 (`crates/re_scene/src/components.rs`). WP6, not WP2, maps them to `BodyDesc`.

**Reuse.** Paths are in the private research folder `/mnt/project-files/real-engine/research/` (not in this repository). Read `research/spikes/physics/physics.md` first; ADR-0008 summarises its gotchas.

| From | Into |
|---|---|
| `research/spikes/physics/code/oxijolt-spike/src/bridge.rs` (`EntityId` map, `layers::table`, `Physics::{new, add, remove, forget_removed, step}`) | `re_physics::world` |
| `research/spikes/physics/code/oxijolt-spike/src/scenarios.rs` (12 scenarios and the bench) and `tests/scenarios.rs` | `re_physics/tests/` and `examples/pile_bench.rs` |
| `research/spikes/physics/code/oxijolt-spike/Cargo.toml` | The oxijolt feature set (`default-features = false`) |
| `research/spikes/physics/partial/src/lib.rs` (`StdThreadPool`, the only spike code implementing a caller-supplied `oxijolt::JobSystem`, around line 255) | The pattern for `JoltRayonJobs` in `re_physics::jobs` |

**Build**
- `Physics::new(PhysicsConfig, &'static TaskPool) -> Result<Physics, Diagnostic>`. Jolt's job system holds the pool for the world's lifetime, so `re_app` keeps its `TaskPool` in a static or leaks it once at startup (`TaskPool::global()` qualifies).
  - It uses `JoltRayonJobs: oxijolt::JobSystem`: `max_concurrency()` returns threads + 1, and `queue_job` calls `pool.spawn(move || job.run())`.
  - It is passed through `WorldSettings::job_system(Arc<dyn JobSystem>)`.
- `apply(&mut Vec<PhysicsCmd>)` sorts by `(kind, EntityId)`, applies the commands and leaves the vector empty (its capacity is kept).
- `step(dt)`.
- `poses_into(&mut self, &mut Vec<BodyPose>)`, one batched pull through oxijolt's `active_body_poses_into`, sorted by entity. It returns the **active** bodies, plus every body that fell asleep during the last step (with its final pose) and every character (its capsule's centre); character inner bodies are not returned. A sleeping body is not returned again until it wakes, and its `Transform` keeps the exact value it had when it fell asleep.
- `velocity(id) -> Option<(Vec3, Vec3)>`, a per-body read for the few entities that need it (characters, scripts); `pose(id)`, `is_awake(id)` and `character(id) -> Option<CharacterState>` likewise.
- `events() -> &[ContactEvent]`. Events are filtered, so that `Removed` events for bodies that are already gone are dropped. The sleep-triggered `Removed` is documented.
- Queries: `raycast`, `shape_cast`, `overlap_sphere`, each with a `QueryFilter { groups, exclude, sensors }`.
- Failed commands are skipped and reported through `take_diagnostics()`.
- `Character` wraps `CharacterVirtual`. When `CharacterDesc::inner_body` is set (the default for the player), it creates the inner body, so the character triggers sensors and appears in contact events. A `CharacterVirtual` without one reports nothing in contact events.
- Sensors.
- Constraints: only fixed, hinge and distance are exposed in M1.
- Collision layers (`LayerTable`: each engine layer is a static and a moving Jolt object layer; the standard table has 8 layers, layer 1 being debris) and group masks (which filter queries and characters only, ADR-0008).
- Heightfield and triangle-mesh colliders, static only. Using them on a dynamic or kinematic body gives `RE5003`.
- `save_state` and `restore_state`, for `real verify` replays (exact, and only within the world that saved the state). `motion_state() -> MotionState` and `apply_motion_state(&MotionState)` carry the pose, velocity and sleep state of each dynamic and kinematic body, plus each character's full state (ground and contacts), from one world to another by `EntityId`. This is for the world copies `play.run` uses. Entities the target lacks are skipped with an RE5004 warning.
- Kinematic bodies have their velocity zeroed when no target is set.
- Fixes for the gotchas in ADR-0008. Codes RE5000–5999 with explain pages.

**Interfaces this WP freezes** (at WP2 exit; consumed by WP6): `Physics::{new, config, apply, step, poses_into, velocity, pose, is_awake, character, events, take_diagnostics, body_count, character_count, jobs_queued, save_state, restore_state, motion_state, apply_motion_state, raycast, shape_cast, overlap_sphere}`, `PhysicsConfig`, `LayerTable`, `QueryFilter`, `PhysicsSnapshot`, `MotionState`, `DIAGNOSTICS` and `codes`.

**Non-goals**
- D2 cross-platform determinism.
- Double precision.
- Vehicles.
- Ragdolls.
- Exposing all 12 constraint kinds.
- `PhysicsSettings` tuning.

**Acceptance**

```sh
export CARGO_BUILD_JOBS=2
cargo test --locked -p re_physics
cargo run --locked -p re_physics --release --example pile_bench -- --bodies 1000 --steps 600
#   prints "pile_bench mean_ms=<x> p95_ms=<y>"; advisory target mean < 8 ms in CI (spike: 2.85–3.78 ms)
cargo clippy -p re_physics --all-targets --locked -- -D warnings
```

Required tests:
- all 12 spike scenarios ported;
- an unsorted command stream and a sorted one give bit-identical poses;
- the same command stream run twice gives identical `BodyPose` bits;
- the pool is shared: no thread other than the rayon pool (`re-worker-N`) and the caller runs Jolt jobs (checked by thread names);
- the character walks up a 30° slope and stops at a wall;
- **the character walks into a sensor and a `ContactEvent` names the sensor and the character** (inner body);
- **the character walking into a 20 kg dynamic crate moves the crate** by at least 0.5 m over 2 s;
- a heightfield matches its samples within the spike's tolerance of 0.02 m.

**Definition of done:** the common definition (PRs green on CI, merged to `main`). The first oxijolt build in CI takes about a minute longer (cmake and g++ are in the CI package list; no libclang is needed).

---

## WP3: Scripting and the validation suite

**Goal.** `re_script`: sandboxed Luau contexts with the engine-native API, a CPU budget that `pcall` cannot swallow, project-confined `require`, a scheduler, hot reload, generated `engine.d.luau`; and `re_script_check`: the static scan, the external type-checker runner and the corpus validation suite that takes its scripts from outside the repository.

**Owned paths:** `crates/re_script/`, `crates/re_script_check/`, `docs/research/scripting.md`

**Starts after:** WP0 PR "entity core" is merged.

**Frozen interfaces consumed**
- `re_scene::World` (`crates/re_scene/src/world.rs`): `get`, `set`, `entity`, `id_of`, `find`, `despawn_runtime`; `re_scene::Entity` (bevy's `Entity`; handles are its `to_bits()`);
- `re_reflect::{Registry::register_script, ComponentSchema, FieldSchema, FieldType, ComponentOrigin, Value}` (`crates/re_reflect/src/registry.rs`, `value.rs`), for `world.define_component` and by-name field access;
- `re_command::{CommandRegistry, EditTarget, Origin}` (`crates/re_command/src/`), for commands issued from the `tools` context (with `Origin::Script`);
- `re_types::{GraphDoc, ActionFrame, EntityId, RayHit, host::HostServices}` (`crates/re_types/src/`);
- `re_core::{Diagnostic, Phase, FixedClock}`.

**Reuse.** Paths are in the private research folder `/mnt/project-files/real-engine/research/` (not in this repository). Base: `research/spikes/scripting/code/`. Read `research/spikes/scripting/scripting.md` first; ADR-0011 and ADR-0013 summarise it.

| From | Into |
|---|---|
| `src/vm.rs` (VM setup, sandbox, per-script environments with `safeenv`, `GLOBALS_KEY`) | `re_script::vm` |
| `src/watchdog.rs` (raw `lua_Callbacks.interrupt`, sticky once tripped) | `re_script::budget` |
| `src/modules.rs` (`ProjectRequirer`, `.luaurc` aliases, hot-reload convention) | `re_script::modules` |
| `src/scheduler.rs` (`task.wait` stub, resume loop) | `re_script::scheduler`, with a binary heap keyed by wake time instead of the spike's `partition` pass |
| `src/entity.rs` (`Copy` handle userdata, interning in a weak-valued table) | `re_script::api::entity` |
| `src/math.rs` (`V3` newtype for vector ↔ glam) | `re_script::api::math` |
| `src/diag.rs` (`ScriptError` from mlua errors and thread tracebacks) | `re_script::diag`, mapped to `RE3xxx` |
| `tests/features.rs` (14 feature tests), `tests/send_sync.rs` and `tests/project/` | `re_script/tests/` |
| `src/bin/bench.rs` | `re_script/examples/script_bench.rs` |
| `src/bin/corpus.rs` | `re_script_check::corpus` (tier 1) |
| `types/engine.d.luau`, `analyzer/defs/`, `analyzer/proj*/`, `analyzer/tests/` | The starting point for the generated definitions and the type-check fixtures |
| `research/spikes/ai/code/src/engine.rs` | The pattern for `CommandError` mapping |
| `research/spikes/scripting/scripting.md` | `docs/research/scripting.md`, with these parts removed, because they describe an external corpus: the header bullet "Sample scenes: …" under "Where things are", the §3 "Corpus" subsection, and §11 open question 2. Keep only the sentence "an external Roblox-style corpus compiled with no failures". |

**Build**

1. **`docs/research/scripting.md`.** Publish the spike report as described in the Reuse table.
2. **`re_script`.**
   - `ScriptRuntime` holds one VM per named context (`game` and `tools`). Each VM:
     - is created with `StdLib::ALL_SAFE`, with `loadstring`, `getfenv` and `setfenv` removed **before** `sandbox(true)`;
     - installs our `require` (`ProjectRequirer` over `create_require_function`), limited to `scripts/`, with aliases from the project `.luaurc`;
     - has `set_memory_limit` from `real.toml [scripts] memory_limit_mb`;
     - has a CPU budget through the **raw** `lua_Callbacks.interrupt` (through `mlua::ffi`). Once tripped it keeps raising at every safepoint until it is re-armed, so `pcall` cannot swallow the timeout.
   - Each script and module runs in its own environment table whose metatable `__index`es the frozen globals, with `safeenv` set, so builtin fastcalls stay on.
   - The `send` and `jit` features of mlua are off. `re_script` offers `jit` as an opt-in cargo feature.
   - It compiles with `mlua::chunk::Compiler`. **It does not use `set_hook`**, because that does not exist in Luau builds.
   - Vectors are native Luau vectors, built with the builtin `vector.create(x, y, z)`. M1 does not call `Compiler::set_vector_ctor` (it is `#[doc(hidden)]` in mlua 0.12.2).
   - The scheduler provides `task.spawn`, `task.defer`, `task.delay`, `task.wait` and `task.cancel`, resumed in the phases of ADR-0009. `task.wait` inside a user coroutine (`coroutine.wrap`) is a documented M1 limitation: it raises a clear `RE3xxx` error instead of yielding to the wrong coroutine.
   - The native API (ARCHITECTURE §4, "Luau view"): `world` (`get(id)`, `find(path)`, `query`, `spawn`, `define_component(name, fields)`, `restart()`), entity handles (`:get(component, field)`, `:set(component, field, value)`, `:children()`, `:destroy()`, which calls `World::despawn_runtime`), `vector` and `quat` helpers, `input`, `physics`, `audio`, `ui`, `time`, `proc` (a builder that returns a `GraphDoc`, plus `proc.regenerate(source_id, seed)`), `print` and `warn`. Hot paths are batch-shaped (`world.query`), because a userdata method call costs about 250 ns. Field writes go through the registry, so Luau numbers are coerced exactly as ADR-0032 says (f64 → f32 with a range check).
   - The runtime calls subsystems only through `re_types::host::HostServices`.
   - Global tables are strict. Reading an unknown member raises `RE9001`.
   - `compile_check(source, chunk_name) -> Vec<Diagnostic>` (tier 1, frozen when WP3's first PR merges).
   - Errors are unwrapped (the duplicated "runtime error" prefix is removed) and reported as `RE3xxx` with the file, the line of the first Luau frame (taken from the traceback when mlua gives none) and a traceback.
   - Hot reload: `ScriptRuntime::reload(path)`, called by `re_app` when `re_assets`' watcher reports a change under `scripts/`. A module written as `local M = ... or {}` is patched in place; other modules are replaced for later `require` calls. A reload with a syntax error keeps the running version.
   - **Engine type definitions.** Every binding is registered with its Luau type signature. `re_script::api::definitions() -> String` writes `engine.d.luau` from that registry (`declare extern type … with … end` for userdata, `declare <lib>: {…}` for libraries). A snapshot test keeps it stable, and a CI test loads it with luau-lsp.
3. **`re_script_check`** (a tooling crate, behind `re_cli`'s `tooling` feature, because it links full_moon).
   - `scan(path) -> ApiUsage` using full_moon `LuaVersion::luau()` (tier 2).
   - `GapReport` compares the usage against a coverage table. M1 ships the native API table. M2 adds the compatibility table.
   - `types::run(project, files) -> Vec<Diagnostic>` (tier 3). It runs the external checker out of process (ADR-0013): `luau-lsp analyze --platform=standard --flag:LuauSolverV2=true --definitions=@engine=<engine.d.luau> --formatter=gnu`, or a patched `luau-analyze` given with `--checker`. It parses the gnu output into `Diagnostic`s. If no checker is found it returns `RE3090 type checker not found`.
   - `corpus::run(dirs, options) -> CorpusReport`, written as JSON. Tier 1 uses `re_script::compile_check`.

**Validation-suite contract** (ADR-0024)
- `crates/re_script_check/tests/corpus.rs` reads the corpus directories from the `REAL_SCENE_SCRIPTS` environment variable. Several directories are separated by `:` on Unix and `;` on Windows.
- `real check --corpus --scripts-dir <DIR> [--scripts-dir <DIR> …] [--report out/corpus.json]` is the same runner (wired by WP6). The flag may be repeated, and when given it replaces the variable.
- **When neither is set, the corpus test prints `skipped: REAL_SCENE_SCRIPTS not set` and passes.**
- `crates/re_script_check/tests/fixtures/` holds original synthetic Luau files written for this repo. The suite always runs over them. They cover:
  - modules and `require`;
  - vector literals;
  - digit separators;
  - `--!strict` and no directive;
  - a deliberate syntax error, which must be reported, not crash;
  - a deliberate type error against `engine.d.luau`, which tier 3 must report;
  - a file with the Roblox-style API surface, to exercise the gap report.
- Tiers:
  1. compile, which is the gate (100% of non-fixture-error files compile);
  2. the static gap report, which never fails;
  3. types, which are advisory in `real verify` and in the corpus run. `real check --types` asked for explicitly fails with `RE3090` when no checker is found, so it can never pass vacuously.
- The report schema is `{ dirs, files, compile: { ok, failed: [diag] }, scan: { by_member: { name: count }, unknown_receivers, dynamic_requires }, types?: {...} }`.
- Files that are not `.lua` or `.luau` are ignored. A missing optional file never fails the run.
- Never commit corpus content or anything derived from it (names, counts, paths) to the repository.

**Interfaces this WP freezes:** `compile_check` (WP3's first PR, for WP7); `corpus::run`, `scan`, `types::run` (exit, for WP6); `ScriptRuntime` (exit, for WP6).

**Non-goals**
- The Roblox-style compatibility layer (M2).
- A debugger.
- Luau-defined commands.
- Luau inside ProcGraph nodes.
- The `server` and `client` contexts.

**Acceptance**

```sh
export CARGO_BUILD_JOBS=2
cargo test --locked -p re_script
cargo test --locked -p re_script_check                       # runs fixtures; corpus test skips when unset
REAL_SCENE_SCRIPTS=<dir>[:<dir>…] cargo test --locked -p re_script_check --test corpus -- --nocapture
#   <dir> is any local directory of .lua/.luau files; nothing from it is committed
cargo run --locked -p re_script --release --example script_bench   # advisory; compare with the spike's numbers
cargo clippy -p re_script -p re_script_check --all-targets --locked -- -D warnings
```

Required tests:
- the 14 ported spike feature tests;
- a runaway loop is stopped by the CPU budget with `RE3010`, and the context remains usable;
- `for i = 1, 1e9 do pcall(function() while true do end end) end` is stopped too (the budget is sticky);
- the memory limit gives `RE3011`, and the VM runs new code after a collection;
- `require` outside `scripts/` is refused;
- `loadstring` is nil, and a global written by one script is nil in another;
- the same entity gives the same userdata (`==`), and handles are released after despawn;
- `world.define_component("Collectible", { value = "i32" })` creates a component that `world.query` finds, and a Luau `0.1` written to an f32 field reads back as f32 `0.1`;
- `task.wait` resumes in the correct phase;
- an unknown global member gives `RE9001`;
- the error message contains "runtime error" at most once;
- the `engine.d.luau` snapshot matches, and luau-lsp loads it with no errors;
- the corpus runner over the fixtures gives a snapshot-tested report, including the expected type error.

**Definition of done:** the common definition (PRs green on CI, where the `test` job has luau-lsp installed; merged to `main`), plus `docs/research/scripting.md` published.

---

## WP4: Geometry and procedural core

**Goal.** `re_geom` (mesh operations) and `re_proc` (the ProcGraph IR, the M1 node set, a deterministic cached parallel evaluator, and the `graph` generator that plugs ProcGraphs into WP0's procedural sources), with no GPU, physics or Luau dependency.

**Owned paths:** `crates/re_geom/`, `crates/re_proc/`

**Starts after:** WP0 PR "entity core" is merged.

**Frozen interfaces consumed**
- `re_types::graph::{GraphDoc, NodeDoc, EdgeDoc}`, `re_types::mesh::{MeshData, Aabb}` (`crates/re_types/src/`);
- `re_scene::procedural::{Generators, GeneratorInfo, GenContext, GenNode, SceneFragment, Procedural, GraphSource, Overrides, RegenReport}` and `re_scene::prefab::PrefabFile` (`crates/re_scene/src/procedural.rs`, `prefab.rs`); `re_scene::World` (`regenerate_changed`);
- `re_reflect::Value` (components in `GenNode` are given by name with `Value`s);
- `re_command::{CommandRegistry, EditTarget, Op, Transaction, CommandSpec, Tier}` (to register the `graph.*` commands);
- `re_noise::{Pcg32, fbm*, ridged*, warped_fbm*, …}` (`crates/re_noise/`);
- `re_core::{TaskPool, Diagnostic}`.

**Reuse.** `re_noise` (already ported by WP0). For the generator model, read the private core spike: `/mnt/project-files/real-engine/research/spikes/core/code/re_core/src/procedural.rs` (the spike's `pillar_grid` generator is the smallest complete example of a generator; it is now a test fixture in `crates/re_scene/tests/support/`) and §9 of `research/spikes/core/core.md`. The research folder is private and not in this repository.

**Build**
1. **`re_geom`** (operations over `re_types::MeshData`).
   - Primitives: box, sphere, cylinder, capsule and plane.
   - `Heightfield` (`samples[z*n + x]`, matching Jolt), `Heightfield::to_mesh` and `Heightfield::height_at(x, z)`.
   - `compute_normals`, `transform` and `merge`.
2. **`re_proc`.**
   - `ProcGraph` from and to `GraphDoc`, with validation (cycles, port types, unknown node kinds) giving `RE40xx`.
   - `NodeRegistry` with the 16 M1 node kinds of ARCHITECTURE §5 (15 table rows; `mesh.transform` and `mesh.merge` share one). Each node's parameters have a schemars schema, which `real schema` includes in `graph.schema.json`.
   - `points.instance` instantiates prefab files through `re_scene::PrefabFile::instantiate`. `material.assign` sets `MaterialRef.asset` to a material file path; `re_app` loads it through `re_assets`. Mesh nodes set `MeshRef.asset` to `derived:<blake3 key>`; `terrain.mesh` with `collider = true` also adds a `Collider` with `shape = Heightfield` and `source = derived:<key>`.
   - An `Evaluator` that evaluates in topological order, in parallel on `TaskPool`.
   - Per-node blake3 keys; seeds derived per node: `Pcg32::from_seed(seed).fork(hash64(node_id))`.
   - A memory cache and a postcard disk cache in `.real/cache/proc/`.
   - `EvalReport`.
   - **The `graph` generator** (`register_generator(&mut Generators)`): its parameter component is `GraphSource`; it reads the graph document from the `GraphStore` resource (a map from source id to `GraphDoc`, filled by `re_app` from the `graph` file or the builder script; `GraphStore` derives `Clone` so a world copy can carry it) through `GenContext::resource`, applies the source's `GraphSource.params` overrides on top of the document's node parameters (an override naming an unknown node or parameter is `RE4011`, and the node keeps its document value), folds the document's canonical hash into the fingerprint through `fingerprint_extra` (the overrides are already in the fingerprint, because `GraphSource` is the parameter component), evaluates it with the source's `Procedural.seed`, and returns a `SceneFragment` whose StableKeys are `<node_id>/<local_key>`.
   - Commands (`register_commands`): `graph.set_param` (edit: `{ source, node, param, value }`; validates the value against the node kind's parameter schema, then issues one ordinary `set_field` `Op` on the source's `GraphSource.params`, so it is undoable with the frozen `Op` set, is saved in the scene file, and works the same for builder graphs and graph files; it never rewrites the builder script or the `graphs/*.graph.json` file, and the regeneration follows in EndFrame as for any parameter change), `graph.regenerate` (edit: `{ source, seed? }`; sets `Procedural.seed` and calls `World::regenerate_changed`, returning the `RegenReport` and the orphan warning `RE4010`), `graph.generate` (read: runs on `World::fork()`, with the `GraphStore` cloned into the copy, and returns a report; budgeted) and `graph.report` (read).
   - Codes RE4000–4499 (`re_proc`) and RE4500–4599 (`re_geom`).
3. **`crates/re_proc/examples/island.graph.json`** (WP4 owns it): a committed graph file for `eval_island`, built from the M1 node set and shaped like the demo island. `re_proc` cannot run Luau, so the example never reads WP6's builder script; WP6 may later check that the builder's output and this file evaluate alike, but nothing depends on it.

**Interfaces this WP freezes:** `ProcGraph`, `NodeRegistry`, `Evaluator`, `EvalReport`, `GraphStore`, `register_generator`, `register_commands` (at WP4 exit; consumed by WP6).

**Non-goals**
- A node canvas UI.
- Polygon booleans (i_overlay is deferred).
- Erosion.
- Luau nodes.
- GPU evaluation.
- Streaming.

**Acceptance**

```sh
export CARGO_BUILD_JOBS=2
mkdir -p out
cargo test --locked -p re_geom -p re_proc
cargo xtask check-graph        # proves re_proc/re_geom reach no wgpu, oxijolt or mlua
cargo run --locked -p re_proc --release --example eval_island -- --seed 1234 --report out/eval.json
cargo clippy -p re_geom -p re_proc --all-targets --locked -- -D warnings
```

Required tests:
- **D1**: the same graph and seed give the same fragment hash in two runs and on 1 vs 4 threads;
- adding an unrelated node changes no other node's output (seed stability);
- changing one parameter only re-evaluates downstream nodes (`EvalReport` cache hits);
- through `re_scene`: regenerating a graph source with a new seed keeps the `Entity` of entities whose key survived, re-applies overrides and respects deletion overrides;
- an orphaned override gives `RE4010` and stays in the file;
- `graph.set_param` on a builder-sourced source changes the evaluated output, records one `set_field` on `GraphSource.params`, and `undo` restores the previous output and `state_hash`;
- `points.instance` with a fixture prefab gives the expected keys and components;
- `RegenReport::released_assets` lists the `derived:` meshes no longer referenced after a regeneration;
- golden hashes for each node with fixed parameters.

**Definition of done:** the common definition (PRs green on CI, merged to `main`).

---

## WP5: Assets, input, audio and animation-lite

**Goal.** `re_assets` (glTF import, material files, a content-addressed cache, hot reload), `re_input` (action map and replays), `re_audio` (kira with a headless capture backend) and `re_anim` (clip sampling and tweens), ported from the platform spike.

**Owned paths:** `crates/re_assets/`, `crates/re_input/`, `crates/re_audio/`, `crates/re_anim/`

**Starts after:** WP0 PR "contracts" is merged.

**Frozen interfaces consumed**
- `re_types::{input::ActionFrame, replay::ReplayDoc, audio::AudioCmd, ids::EntityId, mesh::MeshData, render::{MaterialDesc, TextureRef}, anim::ClipData}` (`crates/re_types/src/`);
- `re_core::{Diagnostic, DiagInfo, init_logging}`.
- For review only: the `AudioSource` and `AnimPlayer` field table in WP0 (`crates/re_scene/src/components.rs`).

**Reuse.** Paths are in the private research folder `/mnt/project-files/real-engine/research/` (not in this repository). Base: `research/spikes/platform/code/re_platform_spike/`. Read `research/spikes/platform/platform.md` first.

| From | Into |
|---|---|
| `src/assets.rs` (`import_gltf`, `ProcessedCache`, `cache_key`, `encode_mesh` / `decode_mesh`) and `tests/gltf_import.rs` | `re_assets`. Replace the hand-written mesh encoding with postcard. |
| `src/watch.rs` and `tests/watch.rs` | `re_assets::watch` |
| `src/input.rs` and `tests/input.rs` | `re_input`. Clean port: delete the module comment at `src/input.rs` lines 5–6 and the comment at `tests/input.rs` line 15, which refer to an external project. |
| `src/audio.rs` (`CaptureBackend`, `Audio::new_or_headless`, `sine`, `rms`) and `tests/audio.rs` | `re_audio` |
| `examples/` | Reference only |

**Build**
1. **`re_assets`.**
   - `AssetId::{Source(Uuid), Derived(Hash)}`.
   - `.meta.json` sidecars.
   - `AssetServer` with handles and generations.
   - glTF import: meshes (`MeshData`), materials (`MaterialDesc`) and node animations (`ClipData`). Images are decoded with `image`, because gltf's `Image::source` unwraps.
   - `load_material(path) -> Result<MaterialDesc, Diagnostic>` for `*.material.json` files, with texture references resolved to asset ids (`TextureRef::Asset` holding the `AssetId`'s text form; `re_app` uploads the textures and swaps in `TextureRef::Handle`).
   - The postcard cache, written atomically (temp file, then rename).
   - Hot reload through `AssetWatcher`. Atomic saves arrive as `Created`. Changes under `scripts/` are reported too, for script hot reload.
2. **`re_input`.**
   - `RawInput` from winit and gilrs (gilrs keeps its default features), including raw mouse motion for camera look.
   - `ActionMap<String>`, built from `InputConfig`, which is parsed from `real.toml [input]` (format in ARCHITECTURE §8).
   - Injection: `set_button`, `set_pad_axis`.
   - `Replay` playback and `ReplayRecorder` (`ReplayDoc`).
   - `ActionFrame` output.
3. **`re_audio`.**
   - `Audio::new_or_headless()`: a kira manager with cpal, or the capture backend.
   - `apply(&[AudioCmd])`: play, stop, volume and spatial pan.
   - The capture backend is used for tests (kira's mock output is private).
   - glam crosses into kira only as plain arrays, because kira uses glam 0.33.
4. **`re_anim`.**
   - A `Sampler` over `ClipData`, with linear and step interpolation, and slerp for rotation.
   - `Tween` (from, to, duration, easing) for scripts.
5. Codes: RE7000–7199 (`re_assets`), RE7200–7299 (`re_input`), RE7400–7499 (`re_audio`), RE7600–7699 (`re_anim`).

**Interfaces this WP freezes:** `AssetServer`, `load_material`, `AssetWatcher`, `RawInput`, `ActionMap`, `InputConfig`, `Replay`, `ReplayRecorder`, `Audio`, `Sampler`, `Tween` (at WP5 exit; consumed by WP6).

**Non-goals**
- Skinning.
- Blend trees.
- Texture compression.
- Streaming.
- An audio effects graph.
- Network input.

**Acceptance**

```sh
export CARGO_BUILD_JOBS=2
cargo test --locked -p re_assets -p re_input -p re_anim
cargo test --locked -p re_audio          # no audio device: uses CaptureBackend; must not touch ALSA default device
cargo clippy -p re_assets -p re_input -p re_audio -p re_anim --all-targets --locked -- -D warnings
```

Required tests:
- glTF import of a fixture with a mesh, a material, a texture and an animation;
- a fixture `*.material.json` loads into the expected `MaterialDesc`, and a missing texture gives a diagnostic;
- a cache hit on the second import, and a cache miss when the importer version changes;
- a watcher event after an atomic save;
- the `[input]` fixture parses, and an unknown key name gives a diagnostic;
- an injected button press gives `just_pressed` for exactly one frame;
- a replay round-trips (record, save, load and play) with identical `ActionFrame`s;
- a 440 Hz sine through the capture backend has the expected RMS;
- the clip sampler is checked at keyframes and at midpoints.

**Definition of done:** the common definition (PRs green on CI, merged to `main`).

---

## WP6: Runtime integration, CLI, verify and the demo

**Status:** done. The demo merged in #31; the `verify-demo` and `xvfb-smoke` CI jobs run it.

**Goal.** `re_app` (the frame loop of ADR-0009, the sync glue between the World and the subsystems, the headless runner, `real verify`, the runtime commands), `re_cli` (the `real` binary) and the `examples/island` demo that meets the M1 definition of done.

**Owned paths:** `crates/re_app/`, `crates/re_cli/` except `crates/re_cli/src/tooling/{mcp,agent,editor}.rs` (which WP6 only creates as stubs, then hands over), `crates/re_cli/tests/mcp_e2e.rs` (WP7) and `crates/re_cli/tests/editor_proxy.rs` (WP8), and `examples/island/` (WP7 and WP8 may also add their crates' lines to `crates/re_cli/Cargo.toml`; see the ownership rule)

**Starts after:** WP0 is merged. Milestone "app" is **one PR** and needs no WP1–WP5 interface frozen (see "Milestone app" below); later WP6 PRs integrate each of WP1–WP5 as its interface freezes. `real do` needs WP7's first PR (`re_agent::lock` and `re_agent::remote::connect`). The demo PR needs WP1–WP5 merged and WP7's `real agent init` (it commits that command's `.real/agent-policy.toml`).

**Frozen interfaces consumed** (each defined in the crate named, at the path in "Cross-WP contracts"):
- `re_render::{Gpu, Renderer, UiFrame, Capture}` (WP1);
- `re_physics::Physics` (WP2);
- `re_script::{ScriptRuntime, compile_check}`, `re_script_check::{corpus::run, scan, types::run}` (WP3);
- `re_proc::{register_generator, register_commands, GraphStore, Evaluator, EvalReport}` (WP4);
- `re_assets::{AssetServer, load_material, AssetWatcher}`, `re_input::{RawInput, ActionMap, InputConfig, Replay}`, `re_audio::Audio`, `re_anim::{Sampler, Tween}` (WP5);
- `re_scene::{World, Entity, SceneDoc, PrefabFile, RegenReport}` and the built-in components, `re_reflect::Registry`, `re_command::{CommandRegistry, EditTarget, History, PolicyCheck, remote::*}` (WP0);
- `re_types::*` including `host::HostServices`, which `re_app` implements; `re_core::*` (WP0);
- `re_agent::lock::{acquire, probe}` and `re_agent::remote::connect` (`crates/re_agent/src/`, WP7's first PR), for `real do`; `re_scene::World::fork` (WP0), for `play.run`.

**Reuse**
- `crates/re_render/examples/windowed_smoke.rs` (the event-loop shape) and the headless tests in `crates/re_render/tests/` (the headless driver shape); the prototype's `windowed.rs` and `headless.rs` are in the private research folder at `/mnt/project-files/real-engine/research/spikes/render/code/src/bin/` (not in this repository).

**Build**

**Milestone "app"** (WP7 and WP8 wait on this). It is one PR. It builds only on WP0's frozen contracts (`re_scene`, `re_command`, `re_reflect`, `re_types`, `re_core`); no WP1–WP5 interface has to be frozen first. It freezes `App::open`, `App::serve`, `AppConfig` and `verify::{run, VerifyStage}`, and creates the tooling stubs. The sync modules start out producing and consuming `re_types` values only (`RenderSnapshot`, `PhysicsCmd`, `BodyPose`, `AudioCmd`). Later WP6 PRs wire in each subsystem once its interface is frozen, without changing the frozen signatures (`AppConfig` gains fields only with defaults):
- `re_physics::Physics` (WP2 exit): `sync::physics`, `play.run`;
- `re_render::{Gpu, Renderer, UiFrame, Capture}` (WP1 exit): `run_windowed`, `screenshot`, GPU resource release;
- `re_script::ScriptRuntime` (WP3 exit) and `re_script_check` (WP3 exit): `HostServices`, hot reload, verify stage 3;
- `re_proc::{register_generator, register_commands, GraphStore}` (WP4 exit): procedural sources at startup and re-grounding;
- the WP5 interfaces (WP5 exit): assets, materials, input, audio and animation.

Everything WP6 builds before the CLI and the demo (the milestone PR plus those integration PRs):
- `re_app::App::open(project, AppConfig)`, `AppConfig` from `real.toml`, and the phase loop of ADR-0009. `App` implements `re_command::EditTarget`.
- `App::serve(EngineReceiver, &dyn PolicyCheck)`: a headless loop on the current thread that answers `EngineRequest`s, checking each against the policy it is given. It is edit mode: nothing simulates, so the scene being edited is the scene on disk (running the game is `play.run` or `real run`).
- `sync::physics::{push, pull}` (`RigidBody`, `Collider`, `CharacterController` → `PhysicsCmd`; poses → `Transform`), `sync::render::build` (`Transform`/`GlobalTransform`, `MeshRef`, `MaterialRef`, `Visible`, `Camera`, `Light` → `RenderSnapshot`), `sync::audio::build` and `GlobalTransform` interpolation. Sync uses typed bevy queries through `re_scene::World::ecs`, not by-name access. The component-to-`BodyDesc` / `CharacterDesc` mapping rules are the notes under WP0's built-in component table. Queries apply a character's `groups` to its inner body, so `HostServices::raycast` with a mask that excludes the player's groups skips the player.
- `World::propagate()` runs in the Sync phase; `World::regenerate_changed()` runs in EndFrame after commands.
- `run_windowed` and `run_headless(frames, replay)`.
- `HostServices` implementation. A game script's `destroy()` of a generated entity calls `World::despawn_runtime`, which suppresses its key.
- Commands registered by `re_app`:
  - `play.run`: step a copy of the World for N frames (at most 3600), then return a report and a screenshot (`read` tier, budgeted). The copy is `World::fork()` with `GraphStore` cloned in; physics is a fresh `Physics` built from the copy by `sync::physics::push` (same body order), then `apply_motion_state` with the live world's `motion_state()`, so poses, velocities, sleep and characters' ground state carry over. Contacts start fresh, so the copy's first step reports `Begin` for touching bodies, and the copy continues close to the live world but not bit-identically; scripts run in fresh VMs on the copy, starting from their entry points (in-VM Luau state is not copied in M1, and the report says so); audio uses the capture backend. Nothing from the copy reaches the live world;
  - `screenshot`: capture the current view (`read`);
  - `verify`: run `verify::run` and return its report (`read`);
  - `asset.import`: import a file into `assets/` with a `.meta.json` (`edit`).
- Procedural sources at startup: for every `GraphSource`, run its builder script in the `tools` context (or read its graph file), store the `GraphDoc` in `re_proc::GraphStore`, then let `World::regenerate_changed` evaluate, instantiate prefabs and apply the fragment; load materials through `re_assets::load_material`.
- **Re-grounding.** After `graph.regenerate` applies a diff, each character is snapped onto the new ground: a downward raycast from above the terrain's maximum height at the character's x and z, then the character is placed at the hit point plus half its height plus the skin width.
- **GPU resource release.** For every `derived:` asset in `RegenReport::released_assets`, `re_app` calls `Renderer::release_mesh` at the end of the frame.
- Script hot reload: `AssetWatcher` changes under `scripts/` go to `ScriptRuntime::reload`.
- PerfReport, giving per-phase mean, p95 and max, plus counts and the renderer's `stats()`.
- `verify::run(&App, &VerifyOptions, extra: &[&dyn VerifyStage])`, with the stages of ADR-0022. Stage 3 (the static scan) is passed in by `re_cli` when it is built with `tooling`; without it the stage reports "skipped".
- Codes RE0500–0899 (`re_app`) and RE0900–0999 (`re_cli`).

**CLI and demo:**
- `re_cli` builds the `real` binary. Global option: `--project DIR` (default: the current directory). Subcommands:
  - `new`;
  - `run [--headless] [--frames N] [--replay F] [--screenshot F] [--exit-after]`. `--exit-after` exits after the given frames and prints `RUN OK` when no error was reported;
  - `describe [--json] [--luau-defs]` (`--luau-defs` prints `engine.d.luau`);
  - `do <command> --args JSON [--dry-run]`, behind the `tooling` feature because it uses `re_agent` (ADR-0027); WP6 writes it in `crates/re_cli/src/tooling/do_cmd.rs`, which WP6 owns. It calls `re_agent::lock::probe`: when the editor holds the lock it sends the command through `re_agent::remote::connect` (the editor applies it); otherwise it takes the lock with `re_agent::lock::acquire` for the duration of the command, so a second headless writer gets `RE8003`. Read-only subcommands (`run`, `verify`, `describe`, `check`) take no lock;
  - `verify [--json]`;
  - `explain RE####`. `re_cli` registers the `explain` command, built from every linked crate's `DIAGNOSTICS`;
  - `graph eval`;
  - `schema [--out DIR]`, which writes every JSON Schema (the scene schema from `World::scene_json_schema`, the other content files, the verify report, the agent policy and the node parameters); `cargo xtask schemas` calls it;
  - behind the `tooling` feature: `check [--types] [--corpus --scripts-dir D …]`, `do` (above), `mcp`, `agent …` and `editor`. A runtime-only `real` runs, verifies, describes and explains a project but cannot edit it. `crates/re_cli/src/tooling/mod.rs` (WP6) declares the modules; `mcp.rs` and `agent.rs` belong to WP7 and `editor.rs` to WP8. WP6 creates those three files as stubs that print "not built yet" and exit 2 in its milestone "app" PR, then hands them over (see the ownership rule).
- `examples/island` (a project directory, not a crate):
  - a procedural island built from the M1 node set by the builder `scripts/tools/island_graph.luau`, placed by a procedural source entity in `scenes/main.scene.json`;
  - a third-person character on `CharacterController` with an inner body;
  - a camera rig in `scripts/game/camera.luau` that orbits and follows the character, driven by a `look` action bound to mouse motion and the right stick;
  - pushable dynamic crates;
  - collectibles placed by `scatter.poisson` and `points.instance` from `prefabs/collectible.prefab.json`, using sensors and a script-defined `Collectible` component;
  - a HUD showing "collected / total", through egui;
  - a goal: when every collectible is collected, the HUD shows a win message and `R` restarts (`world.restart()` reloads the entry scene);
  - a key (`G`) that regenerates the island with a new seed during play, through `proc.regenerate`, while the character stands on it. Collected items stay collected (runtime suppression by key);
  - `real.toml` with an `[input]` section and a committed `.real/agent-policy.toml` from `real agent init`;
  - three replays (`walk_collect`, `regenerate`, and `full_game`, which walks, pushes a crate, collects every gem, wins and presses `R`) and four goldens per backend (`walk_collect_f120`, `walk_collect_f599`, `regenerate_f479`, `full_game_f2776`).

**Interfaces this WP freezes:** `App::open`, `App::serve`, `AppConfig`, `verify::{run, VerifyStage}` (milestone "app"; consumed by WP7 and WP8).

**Non-goals**
- A render thread.
- An export or packaging pipeline.
- Settings UI.

**Acceptance**

```sh
export CARGO_BUILD_JOBS=2
mkdir -p out
cargo build --locked -p re_cli --release
target/release/real verify --project examples/island --json > out/verify.json; echo $?    # 0
target/release/real run --project examples/island --headless --frames 600 \
    --replay examples/island/tests/replays/walk_collect.replay.json --screenshot out/f600.png
target/release/real check --project examples/island --types        # needs luau-lsp 1.70.1; RE3090 and exit 1 without it
xvfb-run -a -s "-screen 0 1280x720x24" target/release/real run --project examples/island --frames 300 --exit-after   # prints RUN OK
cargo build --locked -p re_cli --no-default-features    # runtime-only build
cargo xtask check                                       # check-graph proves it links no tooling crate, tokio, rmcp or full_moon; schemas are current
cargo test --locked -p re_app -p re_cli -- --test-threads=1
```

Required `verify.json` results:
- every stage `ok: true`;
- `state_hash` matches between the two replay runs;
- goldens are within tolerance;
- the regeneration replay keeps the character's `Entity`, keeps it on the ground (re-grounded, no fall through), keeps collected items collected, and keeps script handles valid.

Required tests:
- regenerating 10 times with different seeds keeps the renderer's live mesh count bounded (it returns to the same count after each regeneration);
- the type-check run over the island reports no errors, and the fixture with a deliberate type error is reported;
- the `verify-demo` and `xvfb-smoke` CI jobs, which skip until `examples/island/real.toml` exists, now run and pass.

**Definition of done:** the common definition (PRs green on CI, with `verify-demo` and `xvfb-smoke` now running for real; merged to `main`).

---

## WP7: Agent and MCP

**Goal.** `re_agent` (policy, sessions, checkpoints, agent commands, `AGENTS.md` and client configs, the editor-socket client) and `re_mcp` (an rmcp stdio server exposing one MCP tool per registered command), plus the `real mcp` and `real agent` wiring.

**Owned paths:** `crates/re_agent/`, `crates/re_mcp/`, `crates/re_cli/src/tooling/mcp.rs`, `crates/re_cli/src/tooling/agent.rs`, `crates/re_cli/tests/mcp_e2e.rs`

**Starts after:** WP0 PR "entity core" is merged (agent and MCP crates). `script.write` needs WP3's `compile_check` (frozen in WP3's first PR); the `real mcp` wiring needs WP6 milestone "app"; `real agent init` must land before WP6's demo PR, which commits its `.real/agent-policy.toml`; `mcp_e2e.rs` needs the finished island demo (`graph.regenerate`, `screenshot` and `verify` on `examples/island`), so it lands after WP6's demo PR (the one that adds `examples/island/real.toml`). WP7's first PR holds only the socket protocol, `re_agent::remote` and `re_agent::lock`, which need nothing beyond WP0.

**Frozen interfaces consumed**
- `re_command::{CommandRegistry, EditTarget, CommandSpec, Tier, CommandOutcome, PolicyCheck, History, remote::{channel, EngineHandle, EngineReceiver, EngineRequest}}` (`crates/re_command/src/`, WP0);
- `re_scene::World::{save, state_hash}` (`crates/re_scene/src/`, WP0), for checkpoints;
- `re_script::compile_check` (`crates/re_script/src/lib.rs`, WP3);
- `re_app::{App::open, App::serve, AppConfig}` (`crates/re_app/src/`, WP6 milestone "app");
- `re_core::Diagnostic`.

**Reuse.** Paths are in the private research folder `/mnt/project-files/real-engine/research/` (not in this repository). Base: `research/spikes/ai/code/`. Read `research/spikes/ai/ai.md` and `research/spikes/ai/agents-research.md` first; ADR-0020, ADR-0025 and ADR-0026 summarise them.

| From | Into |
|---|---|
| `src/mcp.rs` (`RealEngineServer`) | `re_mcp::server` |
| `src/main.rs` (`call` subcommand) | The pattern for `real do` |
| `tests/stdio.rs` (12 tests, two protocol revisions) | `re_mcp/tests/stdio.rs` |
| `examples/claude-code.mcp.json` | `re_agent/templates/` |
| `docs/overview.md` | Reference |

**Build**
1. **`re_agent`.**
   - `Policy`, loaded from `.real/agent-policy.toml`: the maximum tier, path globs and budgets. It implements `PolicyCheck`. With no policy file the maximum tier is `read`.
   - `Session` with a checkpoint at start (a saved scene plus the touched files).
   - Commands it registers (`re_agent::register_commands`):
     - `script.write` (`edit`): compile-gated through `re_script::compile_check`. With errors it writes nothing and returns the diagnostics;
     - `checkpoint.restore` (`edit`): saves the current state as a new checkpoint, then restores the session's start checkpoint, so a restore can itself be restored;
     - `file.delete` (`destructive`).
   - Budgets for `play.run` and `graph.generate` are enforced through `PolicyCheck`. Both run on `World::fork()` (WP0).
   - `generate_agents_md(&CommandRegistry)`, plus `CLAUDE.md` (`@AGENTS.md`). `AGENTS.md` explains `scene.apply` and the operation JSON (ADR-0003) with examples.
   - `real agent init` writes `.real/agent-policy.toml` (`edit` tier and the default scopes of ADR-0020; this is the explicit opt-in) and client configs in each client's own format (ADR-0025): `.mcp.json` (`mcpServers`, Claude Code), `.vscode/mcp.json` (`servers` with `type`), `.cursor/mcp.json` and `.gemini/settings.json` (`mcpServers`). For Codex (`[mcp_servers.real-engine]` TOML in a user or trusted-project `config.toml`) and Zed (the `context_servers` settings key) it prints a snippet, because those files live outside the project. The server name is `real-engine`.
   - `cli::AgentArgs`, giving `real agent init|status|rollback`.
   - `remote::connect(socket) -> Result<EngineHandle, Diagnostic>`: the client side of the editor socket protocol; `remote::serve(listener, EngineHandle)`: the server loop, which `re_editor` hosts. Both are `#[cfg(unix)]` (a Unix socket at `.real/editor.sock`). On Windows M1 compiles a stub that returns `RE8004` ("the editor socket is not available on Windows in M1"); named pipes are later work. `windows-latest` must still build.
   - `lock`: `ProjectLock`, `acquire(project, holder) -> Result<ProjectLock, Diagnostic>` and `probe(project) -> LockState { Free, Editor(socket path), Headless(pid) }`. `acquire` takes an operating-system file lock (`File::try_lock`) on `.real/editor.lock` and writes the holder's pid and kind to `.real/editor.owner`; if another process holds the lock, it gives `RE8003 project is locked by <pid>`. The OS lock alone decides who holds the project, so a leftover owner file from a dead holder is ignored and the OS releases the lock however the holder exits. The lock is released on drop. (This replaces the plan's first scheme, `create_new` plus a pid liveness check, under which two racing writers could both take over a stale lock.) Every writer uses this module: `real do` (WP6), `real mcp` (WP7) and `real editor` (WP8); no other crate emits `RE8003`. `re_agent` is a tooling crate: `re_cli` links it only with the `tooling` feature, and all three writers are tooling subcommands (ADR-0027).
   - Codes RE8000–8099 (ARCHITECTURE §7).
2. **`re_mcp`.**
   - `serve_stdio(EngineHandle, McpOptions) -> anyhow::Result<()>`, using rmcp with `server`, `transport-io` and `macros` in its own tokio runtime.
   - One MCP tool per registered command (25 in M1, see ARCHITECTURE §6), with the schema hygiene of ADR-0025 and annotations derived from the tier.
   - Errors are `isError` results carrying JSON text.
   - `screenshot` and `play.run` return image content and a PNG path.
   - Revision 2025-11-25 is the baseline, and 2026-07-28 is accepted.
   - Codes RE8500–8599.
3. **`re_cli/src/tooling/mcp.rs` and `agent.rs`.** `real mcp` calls `re_agent::lock::probe`: when the editor holds the lock it connects to `.real/editor.sock` through `remote::connect` and serves MCP over that handle; otherwise it takes the lock with `acquire`, opens the project with `App::open`, and serves MCP on a background thread while the main thread runs `App::serve`. A second headless writer gets `RE8003`.

**Interfaces this WP freezes:** the editor socket protocol, `re_agent::remote::{connect, serve}` and `re_agent::lock` (WP7's first PR; consumed by WP6 and WP8).

**Non-goals**
- The HTTP MCP transport.
- Luau-defined commands.
- A generic `command.run` tool.
- A shell tier.
- Multi-user editing.

**Acceptance**

```sh
export CARGO_BUILD_JOBS=2
cargo test --locked -p re_agent -p re_mcp        # includes ported stdio tests for both protocol revisions
cargo test --locked -p re_cli --test mcp_e2e -- --test-threads=1
cargo clippy -p re_agent -p re_mcp --all-targets --locked -- -D warnings
```

Required tests:
- `tools/list` has 25 tools, no root `anyOf` or `oneOf`, no `format` keywords, all four annotation hints on every tool, and every description under 4096 characters;
- a tool call over the edit tier with a `read` policy gives an `isError` result with `RE8001`; a dry run of the same call succeeds;
- `lock`: a second `acquire` on the same project gives `RE8003`; racing writers get exactly one lock; a leftover owner file from a dead holder is ignored; `probe` reports an editor holding the lock; on Unix a `remote::serve` / `remote::connect` pair round-trips a command;
- `scene.apply` with three operations applies atomically, and one `undo` call reverts all three;
- `script.write` with a syntax error writes nothing and returns `RE3xxx`;
- `real agent init` output is snapshot-tested for every client;
- `crates/re_cli/tests/mcp_e2e.rs` drives `real mcp --project examples/island` over stdio and checks each DoD item 5 action: list tools, `entity.spawn`, `graph.set_param`, `graph.regenerate`, `screenshot`, `verify` and `checkpoint.restore` (which restores `state_hash`), and that a `script.write` outside the policy scopes is refused.

**Definition of done:** the common definition (PRs green on CI, merged to `main`).

---

## WP8: Editor-lite

**Goal.** A minimal editor (`real editor`): the game view plus egui panels, every action a command so undo works, holding the project's single-writer lock and serving the editor socket.

**Owned paths:** `crates/re_editor/`, `crates/re_cli/src/tooling/editor.rs`, `crates/re_cli/tests/editor_proxy.rs`

**Starts after:** WP6 milestone "app", WP1, and WP7's first PR (socket protocol and lock). `crates/re_cli/tests/editor_proxy.rs` runs `real do`, so it lands after WP6's `real do` PR.

**Frozen interfaces consumed**
- `re_app::{App, AppConfig}` (`crates/re_app/src/`, WP6);
- `re_render::{Renderer, UiFrame, Capture}` (`crates/re_render/src/`, WP1), for headless UI tests;
- `re_agent::remote::{connect, serve}`, the editor socket protocol and `re_agent::lock` (`crates/re_agent/src/remote.rs`, `lock.rs`, WP7);
- `re_command::{CommandRegistry, History, CommandSpec}` (WP0), for the command palette and undo;
- `re_reflect::{Registry, ComponentSchema, FieldType, Value}` and `re_scene::World` (WP0), for the hierarchy and the inspector.

**Reuse.** None beyond the crates above. The egui painting pattern is `re_render::UiFrame` and `Renderer::paint_ui` (see `crates/re_render/examples/windowed_smoke.rs`); the prototype's version is in the private research folder at `/mnt/project-files/real-engine/research/spikes/render/code/src/gui.rs` (not in this repository).

**Build**
- `run_editor(App, EditorOptions) -> anyhow::Result<()>`: the game view plus egui panels:
  - hierarchy (ids and names, from `ChildOf` / `Children`);
  - inspector, generated from the registry's `ComponentSchema`s (one widget per `FieldType`), editing through `entity.set`;
  - console, showing diagnostics;
  - graph parameters with a regenerate button;
  - command palette, over the registry.
- It takes the project lock through `re_agent::lock::acquire` (as the editor holder) and serves `EditorSocket` (`.real/editor.sock`) by hosting `re_agent::remote::serve` with the protocol in "Cross-WP contracts". Other writers proxy to it; a refused writer gets `RE8003` from `re_agent`, and `re_editor` emits no RE80xx code itself.
- Every editor action is a command, so undo works. Each inspector edit is one undo step (no drag coalescing in M1).
- `real editor` (`re_cli/src/tooling/editor.rs`).
- Codes RE8800–8899.

**Non-goals**
- Gizmos.
- A node canvas.
- Docking.
- Undo coalescing.

**Acceptance**

```sh
export CARGO_BUILD_JOBS=2
cargo test --locked -p re_editor -- --test-threads=1   # headless: panels rendered via re_render::Capture, socket round-trip
cargo test --locked -p re_cli --test editor_proxy -- --test-threads=1   # `real do` proxied to an editor socket
cargo clippy -p re_editor --all-targets --locked -- -D warnings
```

Required tests:
- each panel renders headless after the warm-up frame;
- in `re_editor`'s tests: with the editor socket served, a command sent through `re_agent::remote::connect` is applied to the editor's world, and `re_agent::lock::acquire` from a second writer gets `RE8003`;
- in `crates/re_cli/tests/editor_proxy.rs` (WP8; `re_editor` as a dev-dependency of `re_cli`): the test serves the editor socket in-process for a temporary project, runs the `real` binary (`CARGO_BIN_EXE_real`) with `do entity.spawn`, and checks that the in-process world has the entity;
- an inspector edit is undone by `undo`, and the saved scene is byte-identical to the one before the edit.

**Definition of done:** the common definition (PRs green on CI, merged to `main`).

---

## Dependency graph

```mermaid
graph LR
  WP0c[WP0 PR contracts] --> WP0e[WP0 PR entity core]
  WP0c --> WP1[WP1 render]
  WP0c --> WP2[WP2 physics]
  WP0c --> WP5[WP5 assets/input/audio/anim]
  WP0e --> WP3[WP3 scripting + validation]
  WP0e --> WP4[WP4 geom + proc]
  WP0e --> WP7[WP7 agent + MCP]
  WP0e --> WP6[WP6 runtime + CLI + demo]
  WP3 -. compile_check, for script.write .-> WP7
  WP1 -. integration + demo PRs .-> WP6
  WP2 -. integration + demo PRs .-> WP6
  WP3 -. integration + demo PRs .-> WP6
  WP4 -. integration + demo PRs .-> WP6
  WP5 -. integration + demo PRs .-> WP6
  WP6 -. app milestone .-> WP7
  WP6 -. app milestone .-> WP8[WP8 editor-lite]
  WP7 -. socket protocol .-> WP8
  WP7 -. lock + remote::connect, for real do .-> WP6
  WP7 -. real agent init, for the demo's agent policy .-> WP6
  WP6 -. real do, for editor_proxy.rs .-> WP8
```

Solid edges gate the whole WP; dotted edges are partial: they gate one PR (or the PRs named), not the whole WP. WP6 starts after WP0 (milestone "app" builds only on WP0's contracts); each of WP1–WP5 gates only WP6's PR that integrates it and the demo PR. WP3 gates only WP7's `script.write`, which needs `compile_check` from WP3's first PR. WP6's milestone "app" PR comes first; WP7's first PR (socket protocol and lock) then unblocks WP6's `real do` PR and WP8; WP7's `real agent init` PR comes before WP6's demo PR; WP7's `mcp_e2e.rs` comes after the demo PR; WP8's `editor_proxy.rs` comes after `real do`.

Critical path: WP0 → WP3 → WP6 → WP8. Every area now starts from a working spike, so no WP is a research task. WP2 and WP4 have the most code, but their risks are already understood from the spikes.

## Definition of done for M1

**Status:** done. Every item below holds on `main` and is checked by the CI-required jobs; the `verify-demo` job fails if the report has fewer replays or goldens than item 2 names, or any skipped stage.

M1 is done when **all** of the following are true on a clean checkout in the reference container (headless, lavapipe, no audio device), after the one-time setup in "Common rules":

1. The CI-required jobs of ADR-0023 pass on Linux. Windows and macOS build jobs have run and their status is reported. They are not required to pass, except for WP0's scope.
2. `real verify --project examples/island --json` exits 0 with every stage ok, including three replays with matching `state_hash`, four goldens within tolerance, and a PerfReport.
3. `xvfb-run … real run --project examples/island --frames 300 --exit-after` prints `RUN OK`.
4. The demo shows, and replays check:
   - walking, with the camera following;
   - pushing crates;
   - collecting scattered items, with the HUD count updating;
   - the win message when all items are collected, and restarting with `R`;
   - regenerating the island with a new seed during play, without losing the character or script handles, with the character re-grounded and collected items still collected.
5. An MCP client can do all of these, as checked by `crates/re_cli/tests/mcp_e2e.rs`, without writing outside the policy scopes:
   - list tools;
   - spawn an entity;
   - change a graph parameter;
   - regenerate;
   - take a screenshot;
   - run `verify`;
   - roll back (`checkpoint.restore`).
6. `cargo test -p re_script_check` passes with `REAL_SCENE_SCRIPTS` unset, which skips the corpus and runs the fixtures. With it set to a directory of Roblox-style scripts, it produces a JSON report in which tier-1 compile succeeds for every file.
7. `docs/research/scripting.md` and `docs/research/core.md` (the published spike reports) exist. Every Provisional ADR is either Accepted or superseded, except those that explicitly depend on owner decisions or later milestones.
8. Every diagnostic code used has an explain page. `real explain` works for each one.
9. No document describes an unimplemented feature as done.
