# Architecture decisions

These are numbered architecture decision records (ADRs). Each one gives the context, the decision, the alternatives we considered, the consequences and a status.

- **Accepted**: decided. Changing it needs a new ADR that supersedes this one.
- **Provisional**: our current best choice. It still waits on evidence, usually a measurement that has not been made yet. Work may build on it, but only behind the interface named in the ADR, so that replacing it stays local.

The evidence comes from six spikes run in the design phase: render, physics, platform, AI, scripting and the entity core. Each has a report with working code and measurements. The reports live in the maintainers' private research bundle; the render report is published as `docs/research/rendering.md`, and M1 publishes the scripting and core reports as `docs/research/scripting.md` (WP3) and `docs/research/core.md` (WP0), with references to external projects removed.

Where a spike measured something, its result overrides earlier design assumptions.

---

## ADR-0001: One Rust workspace, `re_` crates, one `real` binary

**Status:** Accepted

**Context.** We need a layout that many people and agents can work on in parallel without editing the same files. It must also keep compile times reasonable on a 4-core machine.

**Decision.**
- Use one Cargo workspace. Library crates go in `crates/re_*`. Workspace members are `crates/*` and `tools/*` (`tools/xtask` and `tools/derive_alias_check`); example projects are data, not crates.
- The user-facing tool is a single binary called `real` (crate `re_cli`).
- The workspace uses edition 2024 and `rust-version = "1.95"`.
- All dependency versions live in `[workspace.dependencies]`.

**Alternatives.**
- Fewer, larger crates. This is simpler, but every change rebuilds more, and parallel work collides in the same files.
- Bevy-style plugin crates. These are too fine-grained for M1.

**Consequences.**
- Crate boundaries enforce module boundaries.
- The crate graph in [ARCHITECTURE.md](ARCHITECTURE.md) is normative. A new edge between crates needs review.

## ADR-0002: Design basis

**Status:** Accepted

**Decision.** The design is built around one command registry with `real verify` and stable diagnostic codes as the backbone for humans, CI and agents. Onto that it adds a procedural core (the ProcGraph IR, per-node content keys, seeds derived from node ids, StableKey and overrides, the regeneration diff, D1 determinism and `AssetId::Derived`, all free of GPU, physics and Luau dependencies) and performance seams (snapshot and command-buffer boundaries, one worker pool shared with Jolt, a sorted physics command buffer with a batched pose pull, fixed-step phases, a static API scan, PerfReport and static cells for M2). The documents in `docs/` are the design; the ADRs below give the reasons for each part.

## ADR-0003: One command registry for describing and changing a project

**Status:** Accepted

**Context.** Several things edit a project: the CLI, the MCP server, the editor UI, tests and (later) Luau tools. If each has its own code path, they drift apart and undo breaks. The AI spike built and tested the registry pattern (`CommandSpec`, `Transaction`, `CommandOutcome`, `Engine::dispatch`). The core spike built and tested the edit layer underneath it on bevy_ecs 0.20.0: scene edits as plain JSON operations addressed by stable ids, grouped into atomic transactions with exact inverses, undo/redo and an append-only log. Its tests showed that four transactions (set, rename plus remove component, despawn of a subtree, spawn with a generated id) undo and redo to byte-identical saved scenes, and that a transaction failing on its third operation leaves the scene byte-identical. A despawn of a 1,001-entity subtree took 6.9 ms and its undo 10.2 ms, in a world of 100,100 entities.

**Decision.**
- Every **durable** change to a project goes through a named command registered in `re_command`. Durable means a change that is saved to disk or recorded in undo history.
- A command has:
  - a stable dotted name, such as `entity.spawn`, `entity.set` or `graph.regenerate` (the M1 list is in [ARCHITECTURE.md §6](ARCHITECTURE.md#mcp-tool-set-m1-25-tools));
  - a JSON Schema for its input, generated with schemars;
  - a tier (ADR-0020);
  - a handler that edits the world inside a `Transaction`.
- **Edit operations.** Underneath the registry, every scene change is one of a closed set of operations (`re_command::Op`): `spawn`, `despawn`, `set_field`, `add_component`, `remove_component`, `reparent`, `rename`, `set_override`, plus `restore` (the inverse of `despawn`). Operations are plain JSON (`{"op": "set_field", "id": "crate", "component": "RigidBody", "field": "mass", "value": 12.5}`), address entities by stable id (ADR-0005) and components and fields by name (ADR-0032), are validated before anything changes, and each returns its exact inverse.
- **Transactions.** A `Transaction { label, origin, ops }` is atomic: if any operation fails, the ones already applied are undone, and a `rejected` entry is logged. `re_command::History` keeps the undo and redo stacks and an append-only log of JSON lines that records the origin (the user, a script, or a named agent). Registry commands such as `entity.set` build one-operation transactions; `scene.apply` takes a list of operations and applies them as one transaction.
- **Despawn keeps undo exact.** A despawn snapshots the subtree, clears every inbound entity reference from outside it (found through the registry's entity-typed fields) and records them, so its inverse restores the subtree and those references by stable id. The core spike found this bug (a restored entity is a new `Entity`, so references dangled and saved as `null`); the fix comes with a required test.
- Every mutating command accepts `dry_run`. The registry runs the transaction, records its changes, rolls it back, and returns the changes. This is implemented once in `re_command`, not per command.
- The registry is also the description surface. `real describe`, MCP `tools/list`, the editor's command palette and the generated `AGENTS.md` all read it.
- **Per-frame gameplay does not use commands.** Scripts and systems that move things every frame call direct APIs. Commands are for edits.

**Alternatives.**
- Separate APIs for the CLI, MCP and the editor.
- Making every state change a command, including per-frame gameplay. That is too slow and makes the undo history useless. The core spike measured 100,100 spawns through commands at 1.03 s, against 173 ms for direct typed spawning, so bulk runtime spawning uses direct APIs.
- Undo by whole-scene snapshots. Simple, but each edit would cost a full save at 100k entities (299 ms to the document tree in the spike).

**Consequences.**
- Undo and redo, change journals, dry runs, the agent permission checks and the regeneration diff all hook in at one place.
- Adding an editor button means adding a command.
- Merging consecutive edits of one field during a drag into one undo step (a gesture id on the transaction) and persisting the log across sessions are not in M1.
- The edit layer is backed by the core spike (research bundle `spikes/core/`, published as `docs/research/core.md` in M1 WP0).

## ADR-0004: Diagnostics with stable `RE####` codes and `real explain`

**Status:** Accepted

**Context.** Errors are read by humans, by CI and by agents. Agents need stable identifiers they can match on. Humans need a fix hint.

**Decision.** All user-facing problems are a `re_core::Diagnostic`:
- `code` (`RE0001` to `RE9999`)
- `severity` (error, warning or note)
- `message`
- an optional `location` (file, line and column, or entity id, or command name)
- an optional `hint`
- `related` (a list of further diagnostics)

Codes are grouped in blocks by crate. The single table of ranges is in [ARCHITECTURE.md §7](ARCHITECTURE.md#7-diagnostics-and-error-model). `RE9001` is "unsupported API member" (ADR-0012).

Each code has a Markdown page compiled into the binary, so `real explain RE3004` works offline. CLI output is human text by default, or JSON with `--json`.

**Alternatives.** Free-form error strings, or plain `anyhow` chains. Neither gives agents anything stable to key on.

**Consequences.**
- A test checks that every code used in the source has an explain page, that no code is used twice, and that each code is in its crate's range.
- A code is never reused for a different meaning.

## ADR-0005: Entity core: bevy_ecs 0.20.0 standalone, behind our own API, with stable string ids

**Status:** Accepted. Evidence: the core spike (research bundle `spikes/core/core.md` and `spikes/core/code/`; published as `docs/research/core.md` in M1 WP0). It replaces the earlier provisional choice of hecs.

**Context.** We need entities with components, an ordered parent/child hierarchy with recursive despawn, ids that survive saving, undo and regeneration, per-component change tracking for commands, undo and transform propagation, lifecycle hooks to keep indexes exact, and components that scripts and agents define at run time. In M2 a Roblox-style instance tree must map onto this (ARCHITECTURE §4).

The core spike compared five storage crates on one shared workload (release builds, 100,000 entities, medians of 5 runs; differences under about 20% are noise) and built a full prototype on the winner:

| Crate | Spawn 100k | Iterate per pass | 10k random `get` | Hierarchy | Hooks | Per-component change ticks | Runtime-defined components with by-id access |
|---|---|---|---|---|---|---|---|
| bevy_ecs 0.20.0 (`std`) | 29 ms | 261–277 µs | 1.2–1.3 ms | yes | yes | yes | yes |
| hecs 0.11.2 | 22 ms | 282 µs | 1.4–1.7 ms | no | no | clone-and-compare only | partial |
| sparsey 0.13.4 | 27 ms | 303–308 µs | 0.17–0.19 ms | no | no | no | no |
| shipyard 0.11.5 | 27–29 ms | 449–485 µs | 0.36–0.42 ms | no | yes | opt-in | not found |
| flecs_ecs 0.2.2 | 101–103 ms | 217–226 µs | 2.8–3.7 ms | yes | yes | per table | yes |

The prototype (`re_core` in the spike, about 2,600 lines plus 15 passing tests) measured at 100,100 entities (100 roots × 10 blocks × 99 leaves):
- direct spawning with string ids and hook-maintained id index: 173 ms (1.7 µs per entity);
- full transform propagation 5.5 ms; incremental propagation 0.42 ms with nothing changed, 0.99 ms after moving 1,000 leaves, 1.36 ms after moving 10 roots (10,010 entities);
- 1 million field reads: typed 8.5 ms, through the registry with a resolved key 15.4 ms, fully by name 107 ms; 1 million writes: typed 15.2 ms, through the registry with coercion and equality check 36 ms;
- clean debug build of the prototype 37.9 s wall (100.7 s CPU), 89 packages; incremental rebuild after touching one file 0.9 s.

**Decision.**
- **Storage** is `bevy_ecs = "=0.20.0"` used standalone, with `default-features = false` and features `std` and `multi_threaded`. Not `bevy_reflect` (ADR-0032), not `async_executor` or `backtrace`: the default features add crates and build time for nothing we use (against `std` alone, +12 crates and about +45% build CPU in the spike; against our `std` + `multi_threaded` set, about +8 crates).
- **Our API.** `re_scene::World` is the engine-facing facade (`ecs`, the bevy `World`; `registry`, the component registry of ADR-0032; `generators`, the procedural generator registry of ADR-0017). Only the model crates (`re_reflect`, `re_scene`, `re_command`) depend on bevy_ecs; `cargo xtask check-graph` enforces it. Outside them, `Entity` is re-exported as `re_scene::Entity`, and the few systems that need typed queries (`re_app`'s sync) reach the bevy `World` through `re_scene::World::ecs` and the `re_scene::ecs` re-export. Every other use goes through names, stable ids and `re_reflect::Value`. A bevy upgrade therefore touches the model crates and the sync code only, and is done deliberately: the pin is exact.
- **Identity.** `StableId` (a string) is the persistent identity used by files, commands, logs, undo history, agents and scripts. It is an immutable component; its `on_insert` and `on_discard` hooks keep the `IdIndex` resource exact, including when a whole subtree is despawned. Engine-made ids look like `e7kq2m9x4tb` (50 random bits); humans and agents may choose readable ids such as `player_spawn`, and uniqueness is checked. `/` is reserved for generated ids (`<source id>/<key>`, ADR-0017). `Entity` (index plus generation) is a runtime handle that is never written down; `re_types::EntityId(u64)` carries `Entity::to_bits()` across the contract layer.
- **Hierarchy** is bevy's built-in `ChildOf` / `Children` relationship: ordered children, `insert_child` to place or reorder, recursive despawn. Cycles are refused by our commands. Files write a `parent` id per entity; there is no `Parent` or `Children` component of our own.
- **Transforms.** `Transform` (local: `translation`, `rotation`, `scale`) requires `GlobalTransform`, which is derived and never saved. Two propagation systems are ported from the spike: a full one and a change-driven one that recomputes only subtrees whose `Transform` or `ChildOf` changed, and also reads `RemovedComponents<ChildOf>`, because `Changed<ChildOf>` does not fire on removal. A test proves the incremental result equals the full one.
- **Threads.** Our schedules use `SingleThreadedExecutor` and no `par_iter`, so bevy_tasks' compute pool, which bevy creates lazily with one thread per core, is never started (verified in the bevy_ecs and bevy_tasks 0.20.0 sources). Parallel work stays on the single rayon pool of ADR-0007. The `multi_threaded` feature stays on, as measured by the spike, so that a later ADR can switch executors without a dependency change.

**Alternatives.**
- **hecs 0.11.2** (the earlier provisional choice). Smallest and quickest to build (6 crates, 5.7 s CPU debug), but no hierarchy, no hooks or observers, no per-component change ticks, no scheduler, no by-id byte access. We would rebuild most of bevy_ecs.
- **sparsey 0.13.4.** The fastest structural churn, but no dynamic components, change detection, hooks or hierarchy.
- **shipyard 0.11.5.** The runner-up: hooks, opt-in change tracking and a parallel workload, but no hierarchy, no by-id byte access found, and slower iteration.
- **flecs_ecs 0.2.2.** Feature-rich, but a C library through bindgen; a cached query's `is_changed()` without `detect_changes()` segfaults from safe Rust, and spawning is about 4× slower.
- **Our own storage.** Nothing in the requirements needs it, and bevy_ecs's table and sparse-set split, hooks and relationships are what we would write.
- **An instance tree with dynamic properties as the primary store** (Roblox-like). Easy for M2, slow for Rust systems. M2 maps the instance tree onto entities instead (ARCHITECTURE §4).

**Consequences.**
- bevy_ecs 0.20 renamed the `on_replace` hook to `on_discard`, and `ComponentDescriptor::new_with_layout` takes 8 arguments. Most material online is older; the spike's code is the reference.
- Component names in bevy panics need bevy's `debug` feature; we do not enable it, and our own diagnostics name components.
- bevy_ecs adds no glam and keeps the combined MSRV at 1.95 (DEPENDENCIES.md).
- Bulk runtime spawning goes through direct typed APIs; commands are for edits (ADR-0003).
- Windows and macOS builds of this layer are unverified until CI runs them (M1 WP0); nothing in it is platform-specific.

## ADR-0006: Subsystems take snapshots and command buffers, not the scene

**Status:** Accepted

**Context.** If render, physics and audio depend on `re_scene`, every scene change rebuilds them and couples their threading to scene locks.

**Decision.** `re_render`, `re_physics`, `re_audio`, `re_input`, `re_assets` and `re_anim` do **not** depend on `re_scene`. They depend only on the contract layer (`re_core`, `re_types`). The main inputs and outputs are:

| Crate | Input | Output |
|---|---|---|
| `re_render` | `RenderSnapshot`: a list of `DrawItem { entity, mesh: MeshHandle, material: MaterialHandle, world: Mat4, flags }` plus a camera, lights and debug lines. egui is painted separately through `paint_ui`. | Images |
| `re_physics` | Sorted `PhysicsCmd` buffers | Batched `BodyPose` arrays (active bodies, position and rotation) and contact events |
| `re_audio` | `AudioCmd` buffers | Sound |

`re_app` owns all the sync glue that builds snapshots from the World and applies results back (`re_app::sync::{render, physics, audio}`).

**Alternatives.** Systems that query the World directly, as in Bevy. That is fast to write, but it couples everything together.

**Consequences.**
- Each subsystem can be tested alone with hand-built snapshots. The render and physics spikes already work this way.
- A render thread can be added later by sending the snapshot to that thread (ADR-0007).

## ADR-0007: Threading: main thread plus one rayon pool. No render thread in M1.

**Status:** Accepted

**Context.** winit's event loop must run on the main thread and can be created only once per process. Jolt wants a job system. Two thread pools would fight over 4 cores in CI.

**Decision.**
- The main thread runs the event loop and the frame.
- There is one global `rayon::ThreadPool`, created by `re_core::TaskPool::new(threads)`.
- bevy_ecs (ADR-0005) does not get a pool of its own: `re_scene` runs its schedules with `SingleThreadedExecutor` and uses no `par_iter`, so bevy_tasks' lazily created compute pool is never started. A test checks that no thread named `TaskPool (n)` exists after a frame.
- Jolt uses the same pool through an `oxijolt::JobSystem` implementation:
  - `max_concurrency()` returns `threads + 1`;
  - `queue_job` calls `pool.spawn(move || job.run())`.
- Luau runs on the main thread, one VM per script context. mlua's `Lua` is `!Send` without the `send` feature, and we do not enable that feature: the scripting spike measured it at 25–40% extra cost per Rust↔Luau call. Parallel scripting, if ever needed, means more VMs, each pinned to one thread.
- tokio is used only inside `real mcp`, in its own runtime on a background thread. It reaches the engine through a std channel (`re_command::remote`).
- **M1 has no render thread.** The seam for one is `RenderSnapshot: Send`. A later milestone may move `Renderer::render(&snapshot)` to a dedicated thread without changing its API.

**Alternatives.**
- A render thread from day one. It adds latency and complexity, and nothing in M1 needs it.
- Jolt's own thread pool. That would mean two pools.

**Consequences.**
- Frame time is the sum of simulate, sync and render, until a later milestone.
- PerfReport (ADR-0022) records each phase separately, so we will know when a render thread is worth doing.

## ADR-0008: Physics: oxijolt with default features off, hidden behind `re_physics`

**Status:** Accepted

**Context.** The physics spike tested oxijolt 1.1.0 (Jolt 5.6.0) across 12 scenarios. A 1000-body pile stepped in 2.85 to 3.78 ms (mean), and the crate built from source in about 55 s.

The crate's default `prebuilt` feature downloads binaries during the build, which our crates.io-only rule does not allow. The crate is young: 464 downloads when we checked.

**Decision.**
- Use `oxijolt = "=1.1.0"` with `default-features = false`.
- `re_physics` is the only crate that names oxijolt.
- `PhysicsWorld` is `Send + Sync` (`unsafe impl` at `world.rs:500` and `:533` in the oxijolt source), so the stepping call may run on a pool thread.
- Input is a `Vec<PhysicsCmd>` sorted by `(kind, entity)` before it is applied, so the result does not depend on the order in which scripts issued commands.
- Output is one batched pose pull per step through `active_body_poses_into(&mut Vec<BodyPose>)`, plus `take_events()`. oxijolt's `BodyPose` carries only the body id, position and rotation, and covers active bodies only. So `re_types::BodyPose` has no velocities, sleeping bodies keep their last `Transform`, and velocities are read per body (`Physics::velocity`) only for the entities that need them.
- Characters use `CharacterVirtual`. It is not a body and reports nothing in contact events unless it has an inner body, so `CharacterDesc` has an `inner_body` flag, on by default for the player, which lets the character trigger sensors.
- `re_physics` makes up for the other gotchas the spike found:
  - A kinematic body keeps its last velocity. We zero it when no target is set.
  - `Removed` contact events can name bodies that are already gone. We filter those against our id map.
  - `Removed` also fires when a pair falls asleep. We document this and expose sleeping separately.
  - `position()` is the shape origin, not the centre of mass.
  - Mesh and heightfield shapes are refused on dynamic bodies. We report this as an `RE5xxx` diagnostic, not a panic.
  - World positions are limited to ±5 km, because `Real` is f32. We warn beyond that.

**Alternatives.**
- rapier3d. It was not chosen in the physics spike.
- Raw joltc bindings. The spike tried this as `rawjoltc-spike`: more work, and no gain.
- oxijolt with `prebuilt`. That breaks the crates.io-only rule.

**Consequences.**
- CI needs cmake and a C++ compiler (no libclang: oxijolt-sys ships committed bindings and its `bindgen` feature stays off). The first build takes about a minute longer.
- Swapping the physics library is possible, because no oxijolt type leaks.
- `PhysicsSettings` and a centre-of-mass getter are not exposed by oxijolt 1.1.0. Features that need them wait for an upstream release.
- oxijolt never calls `JPH_Shutdown`. This is harmless at process exit, and we document it.

## ADR-0009: Fixed-step simulation phases in Roblox order

**Status:** Accepted

**Context.** Gameplay and physics need a fixed step. The M2 compatibility layer must fire its events in the order Roblox-style scripts expect.

**Decision.**
- `re_core::FixedClock` uses integer nanoseconds and a 60 Hz step by default, with at most 5 catch-up steps per frame. The code comes from the platform spike's `timestep.rs`.
- Each frame runs these phases in this order:
  1. `Input`: poll events, update the action map, apply replayed input.
  2. `PreRender`: variable-step script callbacks. In M2 this is where render-step bindings and the "render stepped" events run.
  3. For each fixed step:
     1. `PreAnimation`
     2. `PreSimulation`: scripts' fixed update. M2's "stepped" events map here.
     3. `Physics`: apply the sorted `PhysicsCmd` buffer, step Jolt, pull poses and events.
     4. `PostSimulation`: contact callbacks, then scripts' post-physics update. M2's "heartbeat" events map here.
  4. `Sync`: write poses back into the World, propagate `GlobalTransform`, build the `RenderSnapshot` and audio commands.
  5. `Render`
  6. `EndFrame`: flush the command journal, release unreferenced GPU resources and update PerfReport.

**Alternatives.** Bevy-style free scheduling. It is flexible, but the order of events is then not predictable for scripts.

**Consequences.**
- The phase list is an enum in `re_core`. Systems register against phases, not against each other.
- Interpolating between fixed steps for rendering is in M1, and is cheap.

## ADR-0010: Rendering: wgpu 30, our own device, one path for headless and windowed

**Status:** Accepted

**Context.** The render spike verified wgpu 30.0.1, winit 0.30.13 and egui 0.36.2 under lavapipe and llvmpipe, headless and in Xvfb.

**Decision.**
- `re_render::Renderer::render(&Device, &Queue, &TextureView, &RenderSnapshot)` is the only drawing entry point for the scene.
- egui is drawn by `egui-wgpu` into the same view through `Renderer::paint_ui(&Device, &Queue, &TextureView, &UiFrame)`. `UiFrame` holds already-tessellated `egui::ClippedPrimitive`s, the `TexturesDelta`, the target size and pixels per point. The caller tessellates with `egui::Context::tessellate`, because `FullOutput` carries untessellated shapes and tessellation needs the context's font atlas. We do not use eframe.
- GPU resources have explicit release calls (`release_mesh`, `release_texture`, `release_material`) and generation-checked handles, so regenerated content does not leak GPU memory.
- Headless mode renders into an offscreen texture and reads it back. Rows are padded to 256 bytes (`padded_bytes_per_row`).
- Windowed mode renders into the surface texture.
- We install `Device::on_uncaptured_error` with a logger, because the default handler panics.
- The projection is `glam` 0.34's `Mat4` built through the right-handed, DirectX-depth path. `perspective_rh` was removed in 0.34; the prototype shows the replacement.
- The GL backend lacks view formats and depth copies, so features that need them check `downlevel_capabilities`. On GL, egui is painted through a manual-gamma blit, because without `VIEW_FORMATS` the spike's panel came out darker.
- Golden image tests compare with a tolerance of about 2 levels per channel plus a small outlier budget, using the prototype's `imgdiff` approach. **Each backend has its own golden set**, and `verify` uses the set for the backend it ran on.
- The first egui frame lays out wrongly, so captures render one warm-up frame first.

**Alternatives.**
- eframe. It owns the loop and device, which blocks headless tests.
- Bevy's renderer.
- Raw Vulkan through ash.

**Consequences.**
- Every render test runs headless in CI on lavapipe, and the GL set on llvmpipe.
- Window-only behaviour (resize, surface loss) is covered by one Xvfb smoke binary, because a winit `EventLoop` cannot be created twice in one test process.

## ADR-0011: Scripting: Luau through mlua 0.12.2

**Status:** Accepted. The scripting spike built and tested a prototype (14 feature tests in the default, `send` and `jit` builds, plus benchmarks).

**Context.**
- The owner wants Luau, so that Roblox-style projects can be brought over (M2).
- mlua 0.12.2 vendors Luau 0.740 (through mlua-sys 0.13.0 and luau0-src 0.22.0+luau740). The vendored sources include the compiler and VM, but **not** Luau's Analysis (type checker).
- What the spike measured (ns per operation, default build):
  - loop iteration with the raw `lua_Callbacks.interrupt`: 13.2–13.5; with `Lua::set_interrupt`: 28.4–28.5; with no interrupt: 9.7–10.5;
  - a `math.sqrt` loop in a per-script environment with `safeenv`: 22–23; without `safeenv`: 89–92;
  - Rust calling a Luau function: 122–128; Luau calling a Rust closure: 62–65; a userdata method with a World lookup: 246–272;
  - native vector arithmetic: 19; the same with `{x, y, z}` tables: 139–271;
  - scheduler resume plus `task.wait`: about 1.1 µs.
- What it verified: the sandbox (globals and libraries read-only after `sandbox(true)`), per-script isolation, entity handle interning, `require` confined to the project (path escapes are refused by Luau's own navigator), hot reload by in-place module patching, memory limits with recovery, and that `pcall` cannot swallow a timeout once the interrupt keeps raising.
- `Lua::set_hook` is compiled out in Luau builds (`#[cfg(not(feature = "luau"))]`).

**Decision.**
- `re_script` embeds Luau with `mlua = { version = "=0.12.2", features = ["luau"] }`. The `send` feature is off (ADR-0007). `jit` is an opt-in cargo feature of `re_script`, off by default in M1.
- **One VM per script context** (`game` and `tools` in M1; `server` and `client` in M2), each created with `StdLib::ALL_SAFE`. `loadstring`, `getfenv` and `setfenv` are removed, our `require` is installed, and then `sandbox(true)` freezes the globals. Contexts are separate VMs because they have different capabilities; within a context, scripts share one VM.
- **Per-script environments.** Each script and module gets its own environment table whose read-only metatable `__index`es the frozen globals, with `safeenv` set so that builtin fastcalls stay on (the 4× difference above).
- **CPU budget** through a raw `lua_Callbacks.interrupt` installed with `mlua::ffi`, not `Lua::set_interrupt`. It checks a deadline every 1024 safepoints. Once tripped it raises at every safepoint until it is re-armed, so `pcall` cannot swallow the timeout. It is about 2× cheaper per loop iteration than `set_interrupt`.
- **Memory** is limited with `set_memory_limit`.
- **`require`** goes through `create_require_function` with a resolver (`ProjectRequirer`) limited to the project's `scripts/` tree. It supports `./` and `../` paths, `@self`, and aliases from the project's `.luaurc`, which the type checker reads too.
- Source is compiled with `mlua::chunk::Compiler`.
- **Vectors** use Luau's builtin `vector.create(x, y, z)`. We do not call `Compiler::set_vector_ctor`: it is `#[doc(hidden)]` in mlua 0.12.2, it stores a single alternative constructor, and `vector` is already the name of Luau's vector library. If M2 needs `Vector3.new` as a fast constructor, it sets that one name in the compatibility contexts only, with a pinned-version test.
- We do **not** use `set_hook`, because it does not exist in Luau builds. A future debugger would build on the interrupt callback plus `inspect_stack`; that is unverified.
- **Entity handles** are `Copy` userdata (index plus generation) interned in a weak-valued table, so the same entity compares equal across calls and handles work as table keys.
- **Waiting.** `task.wait` is a small Luau stub that yields to a Rust scheduler; the scheduler owns suspended threads and resumes them in the phases. M1 uses a binary heap keyed by wake time. Yielding inside a callback that Rust invoked is impossible, and gives a clear error.
- **Hot reload.** A changed module is re-run with its current table as `...`, so modules written as `local M = ... or {}` are patched in place. Change notifications come from `re_assets`' file watcher, not an mtime poll, because atomic-save editors change file identity.
- **Errors** from Luau are unwrapped. mlua prints "runtime error" twice, so the host strips the duplicate prefix and maps the error to `RE3xxx` with file and line. When mlua gives no line, the first Luau frame of the traceback is used.
- Hot engine APIs are batch-shaped (for example `world.query`), because a userdata method call costs about 250 ns.

**Alternatives.**
- `Lua::set_interrupt` for the budget. About 2× more expensive per safepoint, and a naive implementation lets `pcall` swallow the timeout.
- Separate VMs per script. Isolation is already given by per-script environments, at much lower memory cost.
- The `send` feature. 25–40% slower calls, and nothing in the design moves a VM between threads.
- Plain Lua 5.4 or LuaJIT through mlua. They lack Luau syntax, types, the sandbox and the native vector type.
- Rhai or another Rust-native language. That throws away Roblox compatibility.
- A hand-written binding to the C API. More work for no gain; we drop to `mlua::ffi` in exactly one place, the interrupt.

**Consequences.**
- The raw interrupt relies on Luau errors unwinding through `extern "C-unwind"` Rust frames. This works on Linux. It must be checked on MSVC, and under `panic = "abort"`, before Windows blocks merges (M3).
- `task.wait` inside a user coroutine (`coroutine.wrap`) would yield to the wrong coroutine. M1 raises a clear error there; M2 revisits it for compatibility.
- Whether the JIT should be on by default (2–6× on numeric loops, 4× slower loading) is decided from M1's PerfReport.

## ADR-0012: Engine-native Luau API in M1; Roblox-style compatibility layer in M2

**Status:** Accepted for M1. The size of M2 depends on the open owner question "Must scene scripts run unchanged?" (see Open questions).

**Context.** The owner's target is a set of external Roblox-style projects made of Luau scripts. They use a large Roblox API surface: services, instance classes, events, enums, datatypes and yielding calls. Building all of that before the engine core exists would delay everything.

**Decision.**
- M1 ships a small **engine-native** Luau API in `re_script::api`:
  - `world` (get by stable id, find by name path, query, spawn, `define_component` for script-defined components, restart);
  - entity handles (get and set component fields by name through the component registry of ADR-0032, children, destroy);
  - `vector` (the builtin library) and `quat` helpers;
  - `input` actions;
  - `physics` (raycast, impulse);
  - `audio.play`;
  - `proc` (the graph builder, ADR-0015, and `proc.regenerate`);
  - `ui` HUD text;
  - `time`;
  - `task.spawn`, `task.defer`, `task.delay`, `task.wait` and `task.cancel` on our scheduler;
  - `print` and `warn`.
- The whole native API is described by a generated `engine.d.luau` (ADR-0013).
- M2 adds `re_compat`. It is a Luau-and-Rust layer that provides Roblox-style globals, services, instances, datatypes, enums and the event model, mapped onto the native API. Each member is listed in a machine-readable coverage table.
- Calling a member that is not implemented raises `RE9001 unsupported API member: <Class>.<Member>`. It never fails silently.
- Members that have to wait on a service that does not exist locally, such as data storage, exist and fail with a clear `RE9002` error. They do not hang.

**Alternatives.**
- Roblox API first. It is huge, and it would shape the core around one compatibility target.
- Never providing compatibility. That fails the owner's goal.

**Consequences.**
- M2 is measured with the validation suite (ADR-0024): compile rate, static gap report, runtime boot.
- The static scan (ADR-0014) produces the M2 work list from real scripts before M2 starts.

## ADR-0013: Type checking with the official Luau tools, out of process

**Status:** Accepted. Which checker binary is shipped to users is open owner question 2.

**Context.**
- The Luau sources that mlua vendors have no Analysis module, so the engine cannot type-check in process.
- The scripting spike verified two official checkers against a generated definitions file:
  - **luau-lsp 1.70.1** (MIT), unpatched: `luau-lsp analyze --platform=standard --flag:LuauSolverV2=true --definitions=@engine=engine.d.luau`. Without `LuauSolverV2` it misses writes to read-only fields. Its Luau submodule is the same commit as Luau 0.740, which mlua vendors. As an LSP over stdio it initialised in 0.05 s.
  - **luau-analyze 0.740** (MIT) with a 27-line patch that adds `--definitions=PATH`. It built from source in 220 s. Stock 0.740 has no way to load definitions.
- Both report through `--formatter=gnu` (`file:l.c-l.c: Kind: msg`), with exit code 0 for clean, 1 for errors and 2 for an internal error. There is no JSON output.
- The definitions syntax for 0.740 is `declare extern type Entity with … end` for userdata and `declare <name>: { … }` for libraries; `declare class` is rejected.
- The spike rejected the `luau-analyze` 0.0.1 and `ruau` 0.4.0 crates: few downloads, unproven, and they either re-implement the checker or link Analysis next to mlua.

**Decision.**
- `re_script` generates `engine.d.luau` from its binding registry (`re_script::api::definitions()`). Every binding is registered with its Luau type signature. A snapshot test keeps the file stable, and CI loads it with luau-lsp.
- `re_script_check::types` runs an external checker as a child process and parses its gnu output into `RE3xxx` diagnostics. The default is luau-lsp 1.70.1 found on `PATH`; `real.toml [scripts] checker` or `--checker` can name a patched `luau-analyze` instead.
- The project's `.luaurc` is shared by the checker and the runtime resolver, so both agree on aliases (`"./"`, not `"."`, as an alias value).
- Type results are **advisory** inside `real verify` and the corpus run. `real check --types`, asked for explicitly, fails with `RE3090 type checker not found` when no checker is found, so it never passes without checking anything.
- The checker is an external tool, like cmake for oxijolt. The crates.io-only rule covers code dependencies; the checker version is pinned in CI and in DEPENDENCIES.md.

**Alternatives.**
- The `luau-analyze` 0.0.1 crate in a separate binary. Rejected by the spike (above); it is also strict-only, single-file and behind mlua's Luau version.
- Vendor Luau's Analysis sources and build them with `cc`. Possible, and it would allow in-process checking, but it is a C++ build to maintain. It is an owner question.
- Skip type checking. Agents and editors would then have no machine-readable description of the API.

**Consequences.**
- Editors and agents get the same checker through luau-lsp, with `engine.d.luau`.
- The patched `luau-analyze`, if used, needs a rebase on each Luau upgrade.
- The release binaries for Windows and macOS exist but have not been tried.

## ADR-0014: Static API scan with full_moon

**Status:** Accepted. The license exception it needs (ADR-0019) waits on the owner's license decision.

**Context.** Before M2 we need to know which API members real scripts use, without running them. Compiling only tells us that the syntax is valid. Design-phase probes compiled and parsed an external Roblox-style corpus with no failures, using `mlua::chunk::Compiler` and `full_moon` 3.0.0 (`LuaVersion::luau()`).

**Decision.** `re_script_check` uses full_moon to walk each file and record:
- global reads;
- `game:GetService("X")` style service lookups;
- method calls and property reads on known receivers, including members read as values (`obj.Method` passed to a function);
- `Instance.new("Class")`;
- `Enum.X.Y`;
- datatype constructors;
- `require` targets, with computed targets reported as dynamic.

It compares these against the coverage table (ADR-0012) and reports a gap list with counts. This is validation tier 2.

**Alternatives.**
- Regex scanning. It is fragile.
- Using Luau's own AST through FFI. That is not exposed by mlua.

**Consequences.**
- full_moon is MPL-2.0 (ADR-0019). `re_script_check` is a tooling crate, so the runtime build does not link it.
- The scan is approximate. It cannot resolve dynamic receivers, so it reports "unknown receiver" counts separately.

## ADR-0015: Procedural generation: ProcGraph IR built from Luau, run by Rust nodes

**Status:** Accepted

**Context.** Procedural generation is a primary goal. Graphs must be:
- deterministic;
- cacheable per node;
- editable by agents and (later) a node canvas;
- runnable with no GPU, physics or Luau, so that they can be tested headless and fast.

**Decision.**
- `re_proc` defines `ProcGraph`: nodes with typed ports, edges, and per-node parameters as `serde_json::Value`. It is stored as JSON (`*.graph.json`).
- A Luau **builder** API (`proc.graph()`, `g:node("terrain.heightfield", {...})`, `g:connect(...)`) records a ProcGraph. Builders are `tools`-context scripts under `scripts/tools/`. They do not run anything. Luau never runs inside node evaluation in M1.
- Nodes are Rust functions registered in a `NodeRegistry`. M1 ships 16 node kinds in the 15 rows of the table in ARCHITECTURE §5 (`mesh.transform` and `mesh.merge` share a row). Each node's parameters have a JSON Schema.
- Each node's cache key is `blake3(node type, node version, canonical params, input keys, seed)`.
- Each evaluation produces an `EvalReport`: per node, whether it hit the cache, how long it took, and its output sizes and diagnostics.
- The seed for a node is `Pcg32::from_seed(graph_seed).fork(hash(node_id))`, using `re_noise`. Adding a node does not change the random numbers of the other nodes.
- `re_proc` and `re_geom` depend on neither wgpu, oxijolt nor mlua.
- Outputs are `SceneFragment`s: entities to create, keyed by `StableKey`, with components given by name as `re_reflect` values (ADR-0017). `re_proc` registers a generator named `graph` with `re_scene`, so a ProcGraph is one kind of procedural source. Prefabs are instantiated through `re_scene::PrefabFile`. Meshes produced by nodes are registered as `AssetId::Derived(node_key)`.

**Alternatives.**
- Luau scripts that generate directly. This has no caching, no determinism guarantee and no diff.
- Houdini-style graph only, with no code builder. That is bad for agents and text diffs.

**Consequences.**
- Regenerating with a new seed only re-evaluates nodes whose key changed.
- Node evaluation can run on the rayon pool.
- Luau-defined nodes (M3+) would plug into the `NodeRegistry`; no trait for them is shipped before it has an implementation.

## ADR-0016: Data formats: JSON for content, TOML for config, postcard for caches

**Status:** Accepted

**Context.** Content files are diffed in git and edited by agents. Caches need speed, not readability.

**Decision.**
- **Scenes** (`*.scene.json`), **prefabs** (`*.prefab.json`), **materials** (`*.material.json`) and **graphs** (`*.graph.json`) are JSON. Each may carry a `$schema` field pointing to its schema in `schemas/`. Object keys are written in a stable order, and floats are written in shortest round-trip form.
- **Scene and prefab files** follow the core spike's format (ARCHITECTURE §8): `"format": "real-engine/scene"`, a `version`, and a flat `entities` list in a deterministic order (roots sorted by id, then depth-first in `Children` order; components sorted by name; fields in declaration order). Each entity has a stable `id`, an optional `name` and `parent` id, its `components`, and, on procedural sources, `overrides`. Lists of scalars (vectors, colours) are written on one line, so a changed vector is a one-line diff. Unknown keys are errors. Generated entities are not written. Saving then loading then saving is byte-identical.
- Scene and prefab schemas come from the component registry (ADR-0032), not from schemars, because components can be defined at run time: `World::describe()` emits the registry as JSON, and `World::scene_json_schema()` emits JSON Schema 2020-12. Other file schemas (materials, graphs, the verify report, the agent policy, command inputs) come from schemars.
- `serde_json` is built with `float_roundtrip`. Without it the core spike found 7.9% of ordinary f64 values parsed one ulp off. NaN and infinities never reach a file, because the registry rejects non-finite floats (serde_json would write them as `null`).
- **Configuration** is TOML: `real.toml` for the project and `.real/agent-policy.toml` for agent permissions.
- **Caches** under `.real/cache/` are postcard, with a header carrying the format version. Caches can always be deleted.
- All schemas are generated by one command (`real schema`, run in this repository as `cargo xtask schemas`) and checked for drift in CI.

**Alternatives.**
- RON 0.12.2. The most compact text, with comments and the smallest diffs, but the core spike found its `deserialize_any` path quadratic (`Parser::next_bytes_is_float` scans the rest of the input for every number): 2.2 s for 1,000 entities and 32.5 s for 4,000, against 8 ms and 14 ms for JSON. Even on the linear typed path it parsed 8.6× slower than JSON (797 ms against 93 ms for 100,000 entities × 3 components). Agents also write it less reliably. Revisit if ron fixes the lookahead and comments become important.
- TOML for scenes. Deep table headers, no `None` inside arrays, reordered fields, a key appended at the end of a block silently moves into the last sub-table, and the slowest parser in the test (1.6 s per 100,000 typed entities).
- A binary scene format. It cannot be diffed.
- bincode. Its 2.x release changed the API substantially; postcard is smaller, stable at 1.x and has a documented wire format.

**Consequences.**
- `real check` validates every content file against its schema.
- The JSON schemas are published in `schemas/`.
- JSON has no comments. A per-entity note field or accepting JSONC on load are the candidates; this is decided before the editor writes scenes that people edit by hand.
- At 100,100 entities the spike saved a scene to JSON text in about 0.9 s (40.5 MB; 21.3 MB with defaults omitted) and loaded it in about 1 s, dominated by the generic document tree. A typed fast path is a later optimisation.

**Evidence.** The core spike's format comparison and round-trip tests (research bundle `spikes/core/`; published as `docs/research/core.md` in M1 WP0).

## ADR-0017: Procedural sources, generated ids and overrides that survive regeneration

**Status:** Accepted. Evidence: the core spike's procedural prototype and its test `procedural_sources_overrides_and_regeneration` (research bundle `spikes/core/`).

**Context.** If a user or agent recolours a pillar that a generator made, and the generator then runs again, the edit must not be lost. It must also not pile up stale copies. The same holds for a generated entity that was deleted, or collected during play. Generated content must be addressable like any other entity by the editor, commands and agents.

**Decision.**
- **A procedural source is an ordinary saved entity** carrying `Procedural { generator, seed }` plus a parameter component for its generator (for a ProcGraph, `GraphSource`; ARCHITECTURE §5). The scene file stores the source, its own transform, and its `overrides`. It never stores generated entities.
- **Generators return plain data**: a `SceneFragment`, a list of `GenNode { key, name, parent_key, components }` with components given by name. A Rust generator (the ProcGraph evaluator in `re_proc`, the test generators) and, later, a Luau generator produce the same thing.
- **Generated ids.** Each output node has a generator-chosen **StableKey** (for a ProcGraph, `<node_id>/<local_key>`, where `local_key` is the scatter point index, grid cell and so on). The entity's id is `<source id>/<StableKey>`, so selection, commands, agents and overrides address it like any other entity. Randomness is per key (`GenContext::random(key)` hashes the seed and the key), so growing a grid or adding points does not reshuffle existing ones.
- **Overrides** are stored on the source as `(target key, component, field, value)` entries, or `(target key, deleted: true)`. Editing a generated entity through a command records an override in the same transaction, so undo removes both the value and the override. Despawning a generated entity records a deletion override. Structural edits (reparent, add or remove component) and renames on a generated entity are refused with a hint to change the source's parameters (a rename because no override can carry a name).
- **Regeneration.** After edits, the engine fingerprints each source (generator name, seed, parameter values, and for a graph the canonical hash of its document). When the fingerprint changed it runs the generator and applies the new fragment as a diff:
  - entities whose key survives keep their `Entity`, so script handles stay valid; their components are updated in place;
  - new keys are spawned, vanished keys are despawned;
  - `Derived` meshes no longer referenced are listed for release;
  - overrides are re-applied by key; a deletion override stops that key being spawned; overrides whose key disappeared are kept and reported as orphans (`RE4010`), never silently dropped;
  - a failing generator leaves the previous output in place.
- During play, a generated entity destroyed by a game script is **suppressed** by key for the rest of the session (`World::suppress`), so it is not spawned again by a regeneration. Suppression is play state and is never saved.

**Alternatives.**
- Delete everything and regenerate (the spike's first version). It loses `Entity` identity and invalidates script handles; M1 keeps surviving keys instead.
- Bake on first edit. The entity then stops following the generator. Baking as an explicit command is later work.

**Consequences.**
- The diff is the regeneration contract. M1 tests include regenerating with a new seed during play while a script holds a handle and some items have been collected.
- Not in M1: baking, structural overrides other than deletion (adding under a generated node), generator versioning, and asynchronous generation. Prefabs are instantiated through the same fragment model (`PrefabFile::instantiate`).

## ADR-0018: Determinism: D1 harness in M1, D2 deferred

**Status:** Accepted

**Context.** We want tests that replay input and compare state, and procedural outputs that are reproducible. Determinism across machines (D2) needs Jolt's `cross-platform-deterministic` feature and a careful audit of floating-point maths. That is expensive.

**Decision.**
- **D1**: the same binary, on the same machine, with the same inputs, gives the same `state_hash` every run. Its parts are:
  - fixed step;
  - sorted physics commands;
  - seeded RNG through `re_noise`;
  - script time from `FixedClock`;
  - no iteration over `HashMap` where the order matters (we use `IndexMap` or sort).
- `state_hash` is blake3 over a canonical dump of the World:
  - entities sorted by stable id, generated ones included;
  - components sorted by name;
  - floats as bits.
- `real verify` runs a recorded input replay twice and compares the per-frame hashes.
- `re_noise` is already bit-identical across x86_64, i686 and aarch64 (platform spike goldens).
- **D2** (identical across machines and operating systems) is deferred to M3+. Its seam is the oxijolt feature flag plus the harness itself.

**Alternatives.** Full lockstep determinism in M1. It is too costly for what M1 needs.

**Consequences.** When a run is not deterministic, `real verify` reports the first frame that differs and the components that differ.

## ADR-0019: License exceptions: MPL-2.0 for named crates, font licenses for egui's fonts

**Status:** Provisional. It needs the owner's agreement as part of the license question (Open question 1).

**Context.** Our allow-list is permissive licenses. A scan of the full M1 lockfile (`cargo metadata` over every crate and feature in DEPENDENCIES.md) found these crates outside it:

| Crate(s) | License | Pulled by | Linked into |
|---|---|---|---|
| `full_moon`, `full_moon_derive` | MPL-2.0 | `re_script_check` (static scan) | tooling builds only |
| `triple_buffer` | MPL-2.0 | `kira` 0.12.5 (unconditional dependency) | the runtime |
| `symphonia`, `symphonia-core`, `symphonia-format-riff`, `symphonia-metadata` | MPL-2.0 | `kira`'s `wav` feature | the runtime |
| `epaint_default_fonts` | `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` | egui's `default_fonts` feature | the runtime |
| `clipboard-win`, `error-code` | BSL-1.0 | `arboard`, through egui-winit's `clipboard` feature (Windows only) | the runtime on Windows |

**Decision.**
- Add BSL-1.0 (Boost) to the allow-list. It is permissive and needs no attribution in binaries.
- Add `cargo-deny` exceptions, crate by crate, for the MPL-2.0 crates above and for `epaint_default_fonts` (OFL-1.1 and Ubuntu-font-1.0, for the bundled fonts). No other crate may use these licenses.
- All of these crates are used unmodified.
- MPL-2.0 code **does** ship in the runtime, through kira. `full_moon` is the only MPL crate confined to tooling: `re_script_check` is reachable only through `re_cli`'s `tooling` feature, and `cargo xtask check-graph` proves that `re_cli --no-default-features` does not reach it.

**Alternatives.**
- Write our own Luau parser instead of full_moon.
- Drop kira's `wav` feature and decode WAV ourselves. This removes the symphonia crates but not `triple_buffer`.
- Disable egui's `default_fonts` and ship fonts under a permissive license. The UI then needs its own font setup.
- A different audio crate. The platform spike verified kira, including its headless fallback.

**Consequences.** MPL-2.0 is file-level copyleft: it requires us to publish any modifications to those crates' files, and we make none. OFL-1.1 requires the font license to travel with the fonts; the generated third-party notice includes it.

## ADR-0020: Agent safety model

**Status:** Accepted

**Context.** Agents (through MCP or the CLI) will edit projects. The AI spike recommended tiers with `edit` as an opt-in, dry runs on every mutating command, and no shell tier.

**Decision.**
- Every command has a tier:
  - `read`: describe, query, screenshot, verify, explain, dry runs, and the **budgeted operations** below;
  - `edit`: reversible changes inside the project, including `checkpoint.restore`;
  - `destructive`: deleting files, overwriting outside undo, publishing.
- **There is no shell tier.** No command runs arbitrary processes.
- **Budgeted operations.** `graph.generate` and `play.run` (step the game N frames) run on a **copy of the World** with a time and memory budget. They return a report and a screenshot, and write only agent scratch files under `.real/agent/`. They are `read` tier, because they cannot change the project. Committing a result is a separate `edit` command.
- **Dry runs.** Every mutating command accepts `dry_run` and returns the changes it would make, at `read` tier (ADR-0003).
- **MCP annotations** come from the tier: `read` → `readOnlyHint: true, destructiveHint: false`; `edit` → `readOnlyHint: false, destructiveHint: false`; `destructive` → `destructiveHint: true`. `openWorldHint` is always `false`, and `idempotentHint` is set per command.
- `.real/agent-policy.toml` sets:
  - the maximum allowed tier;
  - path scopes (glob allow and deny lists for file-writing commands);
  - budgets (frames, milliseconds, memory, number of entities touched per transaction).
- **Defaults.** With no policy file, an agent gets `read` only. `real agent init` writes a policy with `edit` and the scopes `scripts/**`, `scenes/**`, `graphs/**`, `prefabs/**`, `materials/**` and `assets/**`, and moderate budgets. Committing that file is the opt-in, and it is visible in review. `destructive` is never in a generated policy; a human adds it.
- Each agent session gets a checkpoint when it starts. `checkpoint.restore` (and `real agent rollback <session>`) returns to it after saving the current state as another checkpoint, so it can be undone.
- `script.write` only commits if the script compiles (tier-1 check). Otherwise it returns the diagnostics and writes nothing.
- **One writer per project.** When the editor is running, it holds `.real/editor.lock` and listens on a local socket (`.real/editor.sock`, a Unix socket; on Windows a named pipe is later work, and M1 builds a stub that reports `RE8004`). Other writers, such as `real mcp` or the CLI, detect it and proxy their commands to the editor. They do not write files directly.

**Alternatives.**
- Free access with a review afterwards.
- `edit` on by default with no policy file. It is convenient, but an agent pointed at a project would then change it without anyone having opted in.
- A shell tier with confirmation prompts. That is too easy to misuse; agents have their own shells.

**Consequences.** The policy is checked through `re_command::PolicyCheck`. Every caller gets it, and the check is not specific to MCP.

## ADR-0021: Profiling: our own `profile_scope!` over puffin or Tracy

**Status:** Accepted

**Context.** The platform spike measured `tracing` spans at about 50 times the cost of puffin or Tracy scopes. The `profiling` crate 1.0.18 pins tracy-client ^0.18, but we use 0.19.

**Decision.**
- `re_core::profile_scope!("name")` expands to:
  - a puffin scope with feature `profile-puffin`;
  - a Tracy zone with feature `profile-tracy`;
  - nothing otherwise.
- `tracing` is for logs and diagnostics, not hot-path timing.

**Alternatives.** The `profiling` crate. Plain `tracing` spans.

**Consequences.** With the default features the profiler costs nothing.

## ADR-0022: `real verify` is the JSON gate, and includes PerfReport

**Status:** Accepted

**Context.** Humans, CI and agents need one command that says whether a project is healthy.

**Decision.**
- `real verify [--project DIR] [--json]` runs these stages in order:
  1. schema check of all content files;
  2. script compile (tier 1);
  3. static API scan (tier 2, report only; present when `real` is built with `tooling`, otherwise reported as skipped);
  4. headless boot;
  5. input replays under `tests/replays/*.replay.json`, with `state_hash` compared across two runs;
  6. golden screenshots under `tests/goldens/<backend>/`;
  7. PerfReport.
- `re_app::verify::run` owns the stages. Stage 3 is passed in by `re_cli` as a `VerifyStage`, so `re_app` never links full_moon.
- PerfReport gives per-phase frame times (mean, p95, max), entity and draw counts, GPU resource counts, and physics step time.
- In M1, budgets in `real.toml [verify.perf]` produce warnings, not failures. The reason is that CI runs on software rendering.
- The exit code is 0 when there are no errors, 1 when there are errors, and 2 on usage errors.
- The JSON output follows a schema (`schemas/verify-report.schema.json`): `{ ok, stages: [{ name, ok, diagnostics, duration_ms, data }], perf }`.

**Alternatives.** Separate commands with no shared report.

**Consequences.** CI and agents both run `real verify --json` and parse the same structure.

## ADR-0023: CI: Linux with lavapipe blocks merges; Windows and macOS are reported only

**Status:** Accepted

**Context.** Development containers are Linux with lavapipe, llvmpipe and Xvfb, and no audio device. Nothing has been verified on Windows or macOS.

**Decision.**
- The required jobs, all on Ubuntu, are exactly these (M1_PLAN refers to this list):
  - `fmt` (`cargo fmt --all --check`);
  - `clippy` (`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`);
  - `test` (`CARGO_BUILD_JOBS=2`, lavapipe). It has two steps: the non-GPU crates in parallel, then the GPU crates (`re_render`, `re_app`, `re_cli`, `re_editor`) with `--test-threads=1`;
  - `msrv` (toolchain 1.95.0, `cargo build --workspace --all-targets --locked`);
  - `deny` (`cargo deny check`, cargo-deny 0.20.2);
  - `xtask` (`cargo xtask check`: crate graph, duplicate versions, explain pages, schema drift);
  - `verify-demo` (`real verify --project examples/island --json`);
  - `xvfb-smoke` (`real run --frames 300 --exit-after` under Xvfb).
- Windows (`windows-latest`) and macOS (`macos-latest`) run `cargo build --workspace --locked`, then the tests of the pure-Rust crates that need no GPU, no audio device and no C++ toolchain (`re_core`, `re_noise`, `re_types`, `re_reflect`, `re_scene`, `re_command`). Their status is reported on the PR, and they do not block a merge (`continue-on-error: true`).
- The workflow lives in `.github/workflows/ci.yml` and is written by M1 WP0 (M1_PLAN). Jobs for crates that do not exist yet skip themselves, so CI is green from the first PR.

**Alternatives.** All three operating systems blocking merges from day one. That is unverified, and it would stop every PR on platform issues we cannot reproduce.

**Consequences.** Making Windows and macOS block merges is an M3 exit item.

## ADR-0024: Validation suite contract for external script corpora

**Status:** Accepted

**Context.** The owner has external Roblox-style projects that the engine should eventually run. They must not be committed to this public repository, and the suite must still run in CI without them.

**Decision.**
- The suite lives in `crates/re_script_check/tests/corpus.rs`, with the runner in `re_script_check::corpus`.
- It finds the scripts directories in this order:
  1. the `--scripts-dir <DIR>` flag of `real check --corpus`, which may be repeated;
  2. the `REAL_SCENE_SCRIPTS` environment variable, which may hold several directories separated by the OS path separator.
- If neither is set, the corpus tests print `skipped: REAL_SCENE_SCRIPTS not set` and pass.
- The suite always runs over **synthetic fixtures** in `crates/re_script_check/tests/fixtures/`. These are small, original Luau files written for this repo that exercise the same tiers.
- The tiers are:
  1. **Compile.** Every `.lua` and `.luau` file compiles with `mlua::chunk::Compiler`. This is the gate: in M1 its target is 100% of files.
  2. **Static scan.** The full_moon gap report. It is a report and never fails.
  3. **Types.** The external checker of ADR-0013. Advisory in the corpus run.
  4. **Boot** (M2). The project loads in the compatibility layer and runs N frames headless without uncaught errors.
- Output is a JSON report, with an optional `--report <FILE>`. Files that are not Luau are ignored, and a missing optional file never fails the suite.

**Alternatives.** Committing the corpora. That is not allowed. Making the corpus tests fail when it is unset would break CI for every contributor.

**Consequences.** Anyone with a Roblox-style project can point the suite at it.

## ADR-0025: MCP server with rmcp over stdio

**Status:** Accepted

**Context.** The AI spike ran rmcp 3.5.1 over stdio with 12 tests across protocol revisions 2025-11-25 and 2026-07-28, and drove it end to end with Claude Code. Its research table lists how each coding agent registers MCP servers; only the Claude Code rows were executed.

**Decision.**
- `real mcp` serves MCP over stdio using `rmcp` (`server`, `transport-io`, `macros`) in its own tokio runtime. The crate is `re_mcp`. It receives an `EngineHandle` from `re_cli` and never opens the project itself.
- Tools come from the command registry (ADR-0003). Their schema hygiene follows what the spike learned:
  - no `anyOf` or `oneOf` at the root of an input schema;
  - schemars `format` keywords are stripped;
  - all four annotation hints (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`) are set explicitly, since `destructiveHint` and `openWorldHint` default to true;
  - descriptions put keywords first and stay under 4096 characters.
- `real agent init` writes:
  - `AGENTS.md`, generated from the registry;
  - `CLAUDE.md` containing `@AGENTS.md`;
  - `.real/agent-policy.toml` (ADR-0020);
  - client configs in each client's own format: `.mcp.json` with `mcpServers` (Claude Code), `.vscode/mcp.json` with `servers` and an explicit `type` (VS Code), `.cursor/mcp.json` with `mcpServers` (Cursor) and `.gemini/settings.json` with `mcpServers` (Gemini CLI). Codex reads TOML `[mcp_servers.NAME]` from a user or trusted-project `config.toml`, and Zed uses the `context_servers` settings key; for these two it prints a snippet, because their files live outside the project.
  - The server name is `real-engine`, with no underscore, because Gemini CLI builds rule names from it.
- Only the Claude Code configuration has been verified end to end. The others follow vendor documentation, and the snapshot tests pin their exact text.

**Alternatives.** HTTP transport. Not needed for local tools in M1.

**Consequences.** Any MCP client can drive the engine. Engine logic stays in `re_command`, and `re_mcp` is a thin adapter.

## ADR-0026: Answers to the AI spike's open questions

**Status:** Accepted

1. **Live editor or headless process?** Both, with the same registry.
   - `real mcp` opens the project headless by default.
   - If `.real/editor.sock` is live, it proxies every command to the running editor instead, so the human sees changes immediately and there is one writer (ADR-0020).
2. **Multiple writers?** Not supported. There is one writer per project.
   - The editor, or else the first headless writer (`real do`, `real mcp`), holds `.real/editor.lock`. One module, `re_agent::lock`, implements the lock for every writer.
   - A second headless writer gets `RE8003 project is locked by <pid>`, with a hint to start the editor or stop the other process.
   - Readers are always allowed.
3. **Error shape.**
   - Tool failures are `isError: true` results whose text is JSON: `{ "code": "RE2001", "message": "...", "location": ..., "hint": "...", "explain": "real explain RE2001" }`.
   - JSON-RPC protocol errors are only for protocol problems.
   - Resource-not-found uses the error number the negotiated revision requires: -32002 or -32602, as the spike found.
4. **Which protocol revision?** Implement **2025-11-25** as the baseline and accept **2026-07-28** when the client offers it. Both are covered by the stdio tests carried over from the spike. Newer revisions are added only when rmcp supports them and a test covers them.
5. **How many tools in `tools/list`?** One tool per registered command: 25 in M1 (listed in ARCHITECTURE §6).
   - Command input schemas are also reachable through `command.describe`.
   - There is no generic `command.run` tool in M1. It would hide the target command's tier from the client's annotations. If M2's Luau-defined commands make the list too long, `command.run` is added with a rule that the policy checks the **target** command's tier and scopes, and it is annotated as `destructiveHint: true`.
6. **Images.** `screenshot` and `play.run` return MCP image content **and** write the PNG under `.real/agent/` with its path in the text. This is because some clients do not show image content.
7. **Luau-defined commands?** Not in M1. In M2, scripts in the `tools` context may register commands with a schema. They are capped at `edit` tier and are subject to the same policy. They never get `destructive`.

## ADR-0027: Split runtime and tooling crates

**Status:** Accepted

**Context.** A shipped game should not link an async runtime, the MCP SDK, a parser it does not need at run time, or editor code. Separate crates also give separate owners.

**Decision.**
- Runtime: `re_app` (the frame loop, sync, verify and the runtime commands) and `re_cli` (the `real` binary). `examples/island` is a project the runtime loads; it is not a crate.
- Tooling: `re_script_check` (static scan, external type checker, corpus runner), `re_agent` (policy, sessions, checkpoints, agent commands, `AGENTS.md` generation), `re_mcp` (the rmcp adapter) and `re_editor` (editor-lite UI and the editor socket).
- `re_cli` depends on the tooling crates behind the default-on cargo feature `tooling`.
- A game export build uses `--no-default-features` and links none of them, so it links no tokio, rmcp or full_moon. `cargo xtask check-graph` checks this (no tooling crate and none of those three dependencies in `cargo tree -p re_cli --no-default-features`).
- A subcommand that needs a tooling crate is itself behind `tooling`: `check`, `mcp`, `agent`, `editor` and `do`. `real do` needs `re_agent::lock` and `re_agent::remote::connect` (one writer, ADR-0026), so the runtime-only `real` has no `do`; it runs, verifies, describes and explains a project but does not edit it. Nothing is split out of `re_agent` for the runtime build.

**Consequences.** WP6, WP7 and WP8 own disjoint crates and files (see [M1_PLAN.md](M1_PLAN.md)).

## ADR-0028: One glam version in our crates, and duplicates checked in CI

**Status:** Accepted

**Context.**
- kira 0.12.5 pulls glam 0.33.
- oxijolt has an optional `glam032` feature.
- transform-gizmo-egui pulls glam 0.32.

**Decision.**
- Our crates use glam 0.34.1 only. bevy_ecs 0.20.0 without `bevy_reflect` pulls no glam; reflecting glam types needs bevy_reflect's optional `glam` feature, which ties it to glam 0.33.x (0.33.12 resolved), and that is one reason it is not used (ADR-0032).
- Libraries that bring another glam are converted at the boundary with arrays or mint, and their glam types never appear in our APIs.
- CI runs `cargo xtask dupes`, which compares `cargo tree -d` for each of the three desktop target triples (as name and version pairs) against the allow-list in DEPENDENCIES.md. cargo-deny's `multiple-versions` is `"warn"`; `xtask dupes` is the gate.

**Consequences.** A dependency that adds a new duplicate needs a DEPENDENCIES row with a reason.

## ADR-0029: Assets: content-addressed cache with derived ids

**Status:** Accepted

**Context.** The platform spike verified:
- glTF import (`Gltf::from_slice` plus `import_buffers`, with images decoded by us, because gltf's `Image::source` unwraps);
- a blake3 cache key over length-prefixed fields;
- atomic writes (a temp file, then rename);
- debounced watching, where an atomic save arrives as `Created`.

**Decision.**
- `AssetId` is an enum:
  - `Source(Uuid)`: a file with a `.meta.json` sidecar holding the uuid;
  - `Derived(blake3::Hash)`: produced by a ProcGraph node or importer.
- Processed data is cached at `.real/cache/<hash>.bin`, in postcard.
- `re_assets` also loads `*.material.json` files into `re_types::MaterialDesc`. Textures are referenced as `TextureRef::Asset(id)`, because `re_assets` cannot depend on `re_render`; `re_app` uploads them and replaces each with `TextureRef::Handle` before `Renderer::create_material`.
- Hot reload is driven by `AssetWatcher`. A reload re-imports the asset, bumps the handle generation, and the next snapshot picks it up. The same watcher reports script changes for script hot reload.

**Consequences.** Derived assets need no files in the project. Deleting `.real/cache` is always safe.

## ADR-0030: Input, audio and animation-lite come from the platform spike

**Status:** Accepted

**Context.** The platform spike built:
- an action map with injectable raw input, including raw mouse motion for camera look;
- kira audio with a headless capture backend;
- logging;
- a fixed timestep;
- file watching.

**Decision.**
- Port these into `re_input`, `re_audio`, `re_assets` and `re_core`.
- Action bindings are data, in `real.toml [input]` (format in ARCHITECTURE §8).
- Input replays use the injection API (`set_button`, `set_pad_axis`).
- `re_audio` uses `Audio::new_or_headless`. Tests use the capture backend, because kira's mock backend output is private.
- `re_anim` (animation-lite) in M1 is limited to:
  - sampling glTF node animations (translation, rotation, scale), imported by `re_assets` as `re_types::ClipData`, onto transforms;
  - simple tweens from scripts.
- There is no skinning in M1.

**Consequences.** gilrs keeps its default features: without them gilrs-core fails to compile.

## ADR-0031: Static cells for large static worlds (M2)

**Status:** Provisional. This is a design note for M2.

**Context.** Roblox-style projects can contain very large static maps made of many individual parts. One entity, draw item and physics body per part would not scale.

**Decision (planned).**
- In M2, static geometry that is not scripted is grouped into spatial cells. Each cell has:
  - one merged mesh per material;
  - one static compound physics body;
  - a sparse index back to the source instances, so that scripts touching one part split it out of the cell.
- M1 keeps the seam: `DrawItem` and `PhysicsCmd::AddBody` both accept merged geometry.
- The target map size is set by the owner at the start of M2, together with the performance budgets.

**Consequences.** M2 must measure this before it is accepted.

## ADR-0032: Component reflection: our own registry with a closed set of field types; no bevy_reflect

**Status:** Accepted. Evidence: the core spike (research bundle `spikes/core/`; published as `docs/research/core.md` in M1 WP0), including its bevy_reflect 0.20.0 comparison.

**Context.** Commands, the inspector, Luau, agents and scene files all read and write component fields by name. Scripts and agents must be able to define new components at run time, and those must be real ECS components: queried, change-tracked, saved and undone like native ones. Luau numbers are f64, while most engine fields are f32.

**Decision.**
- `re_reflect` holds a **component registry** (`Registry`) over bevy_ecs components. Each registered component has a `ComponentSchema`: name, doc, origin (native or script), and fields with a name, a `FieldType`, a doc and a default.
- **A closed set of field types**: `bool`, `i32`, `i64`, `f32`, `f64`, `vec3`, `quat`, `color`, `string`, `entity`, and unit enums. They map one-to-one onto Luau values, JSON and the typed properties the M2 layer needs. One dynamic `Value` type carries them.
- **Central coercion rules**: integer to float; integral float to integer; f64 to f32 with a range check; non-finite floats rejected; quaternions normalised; enums checked against their variants. A Luau `0.1` written to an f32 field reads back as f32 `0.1`.
- **Exact f32 text round trip.** Parsing text as f64 and casting to f32 is wrong for exactly 2 of the 4,278,190,080 finite f32 values (`7.038531e-26` and its negative, bits `0x15ae43fd` and `0x95ae43fd`). `f32_from_parsed_f64` re-parses the shortest decimal form when the cast is not exact; the spike's exhaustive check then found 0 mismatches over every finite f32. This fix comes with a required test.
- **Native components** are declared with the `component!` macro (fields, docs, defaults, and a `requires(...)` clause), which implements the `Reflect` trait. A derive macro is later work.
- **Script-defined components** are registered at run time with `Registry::register_script`. Each gets a real bevy_ecs `ComponentId` (through `ComponentDescriptor::new_with_layout`) whose storage is a `Vec<Value>`, so it is visible to queries, change detection, saving, despawn and undo exactly like a native component.
- **Access by name** (`World::get(id, "RigidBody", "mass")`), or by a key resolved once for hot paths. Writing a value equal to the current one does not trip change detection.
- **Errors name the fix**: unknown names come with a "did you mean" suggestion and the list of valid fields, for example `component RigidBody has no field "mas" (did you mean "mass"?); fields are: kind, mass, friction, restitution`.
- **The registry is the description surface** for components: `World::describe()` emits every component, field, type, default and doc as JSON, and `World::scene_json_schema()` emits JSON Schema 2020-12 for scene files (ADR-0016).

**Alternatives.**
- **bevy_reflect 0.20.0.** Rejected after the spike tested it in detail:
  - reflecting glam types needs its optional `glam` feature, which ties it to glam 0.33.x (0.33.12 resolved), while our stack uses glam 0.34.1; a glam 0.34 field fails to register (`Vec3` does not implement `GetTypeRegistration`, with two glam versions in the graph), and bevy moves glam every release;
  - it does no numeric coercion: `try_apply(&50.0f64)` on an f32 field fails with "`f64` is not `f32`", and `ReflectComponent::apply` with an f64 panics;
  - it writes full Rust type paths into files and silently ignores enum shape mismatches;
  - its APIs were renamed or moved in every release;
  - bevy_remote's JSON schema export has no docs or defaults and pulls 278 to 322 crates.

  Its field access speed is comparable to ours (169 to 209 ns per fully dynamic write plus read, against about 107 ns for our by-name read).
- **A derive macro now** (`re_reflect_derive` with syn). Better errors and nested structs, but not needed for M1's built-ins; the `component!` macro works today.

**Consequences.**
- The registry is about 400 lines, with our own error messages and coercion rules, and one model for native and script components.
- By-name access costs about 2× typed access with a pre-resolved key, and about 12× when every name is looked up; that is fine for scripts, commands and agents, while hot engine systems stay typed.
- Container field types (lists and string-keyed maps) are not in M1. M2 adds them for attributes and tags (ARCHITECTURE §4); they affect the schema, the Luau bindings and diffing, so they get their own design pass.

---

## Open questions for the owner

1. **License.**
   - The repository has no `LICENSE` file. The render prototype's `Cargo.toml` declares `MIT OR Apache-2.0`.
   - Recommendation: dual MIT / Apache-2.0, the usual choice for Rust, which matches most crates' defaults and the prototype. Add `LICENSE-MIT` and `LICENSE-APACHE` at the root.
   - This also needs a decision on the exceptions in ADR-0019. MPL-2.0 code ships **in the runtime** through kira (`triple_buffer`, and the symphonia crates through the `wav` feature), not only in tooling (full_moon). egui's default fonts bring OFL-1.1 and Ubuntu-font-1.0. Do you accept these, or should we drop kira's `wav` feature, replace kira, or replace the default fonts?
   - Until the owner decides, the workspace `license` field is left as the prototype's value, and the README says the license is not final.
2. **Which Luau type checker do we ship?** The engine cannot type-check in process today, because mlua's vendored Luau has no Analysis module (ADR-0013). The options:
   - **(a) An external official checker** (the current default). luau-lsp 1.70.1 works unpatched and also serves editors; a patched luau-analyze 0.740 (27-line `--definitions` patch, a 220 s source build) is the alternative. Both are MIT tools, not code dependencies, so the crates.io-only rule does not apply, but users must install them (or we bundle the release binaries), and the patched analyzer needs a rebase on each Luau upgrade.
   - **(b) Vendor Luau's Analysis sources** (MIT) into a crate of our own and build them with `cc`. This gives in-process checking at mlua's exact Luau version, on every platform, with no external binary. It is a C++ build we would maintain.
   - **(c) The `luau-analyze` 0.0.1 crate.** The scripting spike rejected it: few downloads, strict-only, single-file, behind mlua's Luau version, no Windows support, and it cannot link into the same binary as mlua.
   - Recommendation: (a) with luau-lsp for M1 and M2, and revisit (b) if bundling external binaries proves painful.
3. **Must the external sample scenes' scripts run unchanged?**
   - The current plan is an engine-native Luau API in M1 and a Roblox-style compatibility layer in M2 (ADR-0012).
   - "Unchanged" means every global, service, class, event, enum and datatype those scripts touch must exist with matching behaviour, including yielding calls and deprecated members.
   - The alternative is "runs after a mechanical migration done by a tool". It is much cheaper, and it could be offered as a `real migrate` command.
   - The answer sets the size of M2.
4. **Windows and macOS priority.** When should these platforms start blocking merges? M3 is the current plan.
