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
- Output is one batched pose pull per step through `active_body_poses_into(&mut Vec<BodyPose>)`, plus `take_events()`. oxijolt's `BodyPose` carries only the body id, position and rotation, and covers active bodies only. So `re_types::BodyPose` has no velocities, sleeping bodies keep their last `Transform` (`Physics::poses_into` also reports the final pose of bodies that fell asleep during the step, so that value is exact), and velocities are read per body (`Physics::velocity`) only for the entities that need them.
- **Layers decide collisions; group masks filter queries and characters.** Which bodies collide is decided by `re_physics::LayerTable` alone: each engine `Layer` becomes two Jolt object layers, one for static bodies (in the static broad-phase tree) and one for moving bodies (dynamic and kinematic, in the moving tree), so a pair of layers has a moving-vs-moving and a moving-vs-static switch. The standard table has 8 layers, layer 1 being debris (moving debris hits static bodies only). `GroupMask` does not affect body-to-body collision: it filters queries (`QueryFilter::groups`) and characters (a character only collides with bodies whose groups intersect its own, through a contact-validate listener).
- Characters use `CharacterVirtual`. It is not a body and reports nothing in contact events unless it has an inner body, so `CharacterDesc` has an `inner_body` flag, on by default for the player, which lets the character trigger sensors.
- `re_physics` makes up for the other gotchas the spike found:
  - A kinematic body keeps its last velocity. We zero it when no target is set.
  - `Removed` contact events can name bodies that are already gone. We filter those against our id map.
  - `Removed` also fires when a pair falls asleep. We document this and expose sleeping separately.
  - `position()` is the shape origin, not the centre of mass.
  - Mesh and heightfield shapes are refused on dynamic and kinematic bodies. We report this as `RE5003`, not a panic.
  - World positions are limited to ±5 km, because `Real` is f32. re_physics refuses bodies, characters and transforms beyond that with RE5006, and the command is dropped.

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
- egui is drawn by `egui-wgpu` into the same view through `Renderer::paint_ui(&Device, &Queue, &TextureView, &UiFrame)`. `UiFrame` holds already-tessellated `egui::ClippedPrimitive`s, the `TexturesDelta`, the target size and pixels per point. The caller tessellates with `egui::Context::tessellate` (`UiFrame::from_output` wraps it), because `FullOutput` carries untessellated shapes and tessellation needs the context's font atlas. We do not use eframe.
- GPU resources have explicit release calls (`release_mesh`, `release_texture`, `release_material`) and generation-checked handles, so regenerated content does not leak GPU memory. Uploads, `create_material` and releases return `Result<_, Diagnostic>`; `Gpu::for_surface(Arc<W>, backends)` returns the `Gpu` and a `wgpu::Surface<'static>`.
- Headless mode renders into an offscreen texture and reads it back. Rows are padded to 256 bytes (`padded_bytes_per_row`).
- Windowed mode renders into the surface texture.
- We install `Device::on_uncaptured_error` with a logger, because the default handler panics.
- The projection is `glam` 0.34's `Mat4` built through the right-handed, DirectX-depth path. `perspective_rh` was removed in 0.34; the prototype showed the replacement, and `re_render` uses it.
- The GL backend lacks view formats and depth copies, so features that need them check `downlevel_capabilities`. Without `VIEW_FORMATS` the spike's egui panel came out darker on GL, so egui is painted into an `Rgba8Unorm` overlay and composited with a manual-gamma blit on **every** backend: one code path, and WP1 measured a 0.000-level difference in the egui panel's mean luminance between Vulkan and GL (their egui goldens are identical).
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

**Status:** Accepted. The owner chose "Demo first" (open question 3, resolved): M1 ships the playable demo on the engine-native API, and M2's goal is the sample scenes running through the Roblox-style layer. Whether a mechanical migration step on the way is acceptable is still open.

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
- M1's demo is written against the native API only; no compatibility work blocks M1.
- M2's exit is the sample scenes running through `re_compat`. If a mechanical migration step is accepted, `re_compat` may cover less of the API surface and a `real migrate` tool covers the rest. [M2_PLAN.md](M2_PLAN.md) assumes scripts run unchanged until the owner decides.
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

**Context.** Our allow-list is permissive licenses. A scan of the full M1 lockfile (`cargo metadata` over every crate and feature in DEPENDENCIES.md) found these crates outside it. (The first scan also listed `symphonia`, `symphonia-core`, `symphonia-format-riff` and `symphonia-metadata`, MPL-2.0, pulled by kira's `wav` feature. That feature has been dropped: it enables symphonia's RIFF container without the PCM codec, so it could not decode WAV anyway. `re_audio` reads WAV itself, and the symphonia crates are no longer in the build.)

| Crate(s) | License | Pulled by | Linked into |
|---|---|---|---|
| `full_moon`, `full_moon_derive` | MPL-2.0 | `re_script_check` (static scan) | tooling builds only |
| `triple_buffer` | MPL-2.0 | `kira` 0.12.5 (unconditional dependency) | the runtime |
| `epaint_default_fonts` | `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` | egui's `default_fonts` feature | the runtime |
| `clipboard-win`, `error-code` | BSL-1.0 | `arboard`, through egui-winit's `clipboard` feature (Windows only) | the runtime on Windows |

**Decision.**
- Add BSL-1.0 (Boost) to the allow-list. It is permissive and needs no attribution in binaries.
- Add `cargo-deny` exceptions, crate by crate, for the MPL-2.0 crates above and for `epaint_default_fonts` (OFL-1.1 and Ubuntu-font-1.0, for the bundled fonts). No other crate may use these licenses.
- All of these crates are used unmodified.
- MPL-2.0 code **does** ship in the runtime, but only `triple_buffer`, an unconditional dependency of kira. `full_moon` is the only MPL crate confined to tooling: `re_script_check` is reachable only through `re_cli`'s `tooling` feature, and `cargo xtask check-graph` proves that `re_cli --no-default-features` does not reach it.

**Alternatives.**
- Write our own Luau parser instead of full_moon.
- Keep kira's `wav` feature. Rejected: it brought four more MPL-2.0 crates into the runtime and could not decode PCM WAV, so `re_audio` decodes WAV itself. Dropping it does not remove `triple_buffer`.
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
  2. script compile (tier 1; the stage's `data.scripts` counts the scripts compiled);
  3. static API scan (tier 2; present when `real` is built with `tooling`, otherwise reported as skipped). Its `data` is `{ scripts, covered, missing }`. API gaps are warnings (`RE0903`, "scripts use an API the engine does not provide"), and a script the scan cannot parse is an error;
  4. headless boot;
  5. input replays under `tests/replays/*.replay.json`, with `state_hash` compared across two runs. Each run also reports `regenerations[]` (`frame`, `characters_kept`, `grounded`, `collected_kept`, `script_errors`): a terrain regeneration during a replay (a change in the set of derived heightfield colliders) must keep each character's `Entity`, get each character on the ground within 60 frames, and keep collected (suppressed) ids absent, or the stage fails with `RE0525`. A restart drops the pending checks;
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
   - The editor, or else the first headless writer (`real do`, `real mcp`), holds an operating-system file lock (`std::fs::File::try_lock`) on `.real/editor.lock` and writes its pid and kind to `.real/editor.owner`. The OS lock alone decides who holds the project; the owner file is information for `RE8003` and `probe`, and a leftover one from a dead holder is ignored. The OS releases the lock however the holder exits, so there is no stale-lock recovery. One module, `re_agent::lock`, implements the lock for every writer.
   - Amended in M1 (WP7): the first design created the lock file with `create_new` and replaced it when its pid was dead. Review showed that two writers racing to replace a stale lock could both win, so WP7 froze the OS-lock scheme instead.
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

**Status:** Accepted. Evidence: WP13's `bigmap` benchmark (`crates/re_compat/examples/bigmap.rs`), merged in #65.

**Context.** Roblox-style projects can contain very large static maps made of many individual parts. One entity, draw item and physics body per part would not scale.

**Decision.**
- Anchored geometry that no script has touched is grouped into spatial cells (`re_compat::physics::StaticCells`). Each cell has:
  - one merged mesh per material (drawn by WP15's 3D rendering);
  - one static compound physics body (an assembly of `re_physics::parts`);
  - a sparse index back to the source instances, so that a script touching one part splits it out of the cell.
- `DrawItem` and `PhysicsCmd::AddBody` both accept merged geometry (the seam M1 kept).
- Jolt limits positions to ±5 km per axis (`re_physics::WORLD_LIMIT`). A map that is off-centre but no wider than 10 km is simulated around a **floating origin**: the compat layer moves the simulation origin to the middle of the parts on a 64-stud grid (`Physics::set_origin`, which rebases every assembly through oxijolt and keeps world poses and velocities). A part that still cannot fit is reported with `RE5006` instead of being dropped without a word.
- Budgets for M2, for a 250,000-part map: load within 30 s, a mean step within 8 ms, and a peak resident set within 4096 MB.

**Measured** (`bigmap --parts 250000 --frames 600`, in the reference container):

| Run | Cells | Load | Step mean | Step p95 | Step max | Peak RSS |
|---|---|---|---|---|---|---|
| static map | 2,416 | 3.877 s | 4.576 ms | 6.717 ms | 11.258 ms | 1282 MB |
| a part touched every 10 frames (60 split out) | 2,416 | 3.834 s | 4.868 ms | 7.000 ms | 18.968 ms | 1282 MB |

**Consequences.** Static maps cost a few thousand bodies instead of one per part, and splitting a touched part out costs one cell rebuild. A map wider than 10 km needs a different scheme (several simulation regions), which is not planned for M2.

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
- **Amendment (M2, WP9 PR "containers").** The closed set grows by three container types. `list` holds items of one scalar or record type. `map` holds string keys and dynamically typed values: scalars, records, or lists of those; integers are stored as floats. `record` is a named, fixed tuple of scalar fields registered once per process (`RecordSchema::register`); registration refuses a changed shape, so the set stays closed. No container holds an entity reference. In files, map values that are not a bool, number or string carry a `$type` tag. `Value::diff` reports changed list indices, map keys or record fields, and an equal write trips no change detection. In Luau, a list is an array, a map a string-keyed table, and a record a frozen table until WP10 replaces it with userdata. Immutable components can be reflected; the registry writes them by replacement so their hooks see every change, which keeps `re_scene`'s `TagIndex` exact.
- **Amendment (M2, WP9 PR "skeleton").** A third component origin, `ComponentOrigin::Host`, joins `Native` and `Script`. The host registers such a component from its own data with `Registry::register_host`, with the same checks as a script component, before any scene loads. `World::save` leaves host components out of `script_components`, and a file stores their fields by name like a native component's, so a field missing from an older file takes its default. A scene that names a host component nobody registered fails to load with an unknown-component error. The compatibility layer's per-class `Compat_<Class>` components use this origin, so adding a property or an enum item never breaks a saved compat scene.

## ADR-0033: Private sample projects are checked by a public runner and a private kit

**Status:** Accepted

**Context.** M2's goal is the owner's two sample projects running through `re_compat` (ADR-0012). They must never be committed to this public repository (ADR-0024), but their acceptance must be repeatable, not a manual check.

**Decision.**
- The runner is public and generic: `crates/re_compat/tests/samples.rs`. It reads expectation files from the directory in `REAL_SAMPLE_KIT`, and prints `skipped: REAL_SAMPLE_KIT not set` and passes when the variable is unset.
- An expectation file names its input (path and sha256 prefix), the `real import` options, any stand-in content to add (a ground plane, generated content for missing parts of the sample), the boot expectations (frames, exact script output, allowed warnings), and a list of `play.run` drivers, each with assertions on `dump_tree` output, attributes and logged values.
- The kit (expectation files, drivers and stand-ins) lives in the private research folder with the samples. It is never committed.
- A PR description says only whether its private checks pass, with no counts, sample names, output or paths; counts and details go in the private research folder. (Amended during M2: counts were allowed at first, but even counts describe the samples.)

**Alternatives.** Manual runs, which are not repeatable. Committing redacted samples, which is still sample content.

**Consequences.** Anyone with their own Roblox-style project can write a kit for it. Public CI covers the layer through synthetic fixtures and `examples/plaza`.

## ADR-0034: Compatibility class and enum data are written as facts, not copied prose

**Status:** Accepted

**Context.** `re_compat` needs names, types, defaults and inheritance for many classes, members and enums. Vendor reference documentation and API dumps carry these facts alongside copyrighted prose.

**Decision.**
- `crates/re_compat/classes/*.json` and `enums/*.json` hold names, signatures, types, defaults, inheritance and our coverage status, written by hand or by a script, for the members real scripts use.
- Doc strings in these files are written for this repository. No description, example or other prose is copied from any vendor's documentation or tool output.
- Rig data for the standard R6 and R15 characters (part names, sizes, joint offsets) is written the same way.

**Consequences.** The data covers what real scripts use rather than the whole vendor API. The gap report (ADR-0014) shows what to add next.

## ADR-0035: Compat fonts are open-licensed substitutes, bundled as files

**Status:** Accepted (it adds a file-level font exception alongside ADR-0019, which stays Provisional with the license question)

**Context.** Compat GUI text (`TextLabel`, `TextButton`, `TextBox`, WP15) names font families from the vendor's catalogue. Those font files are not ours to ship, and egui's default fonts (ADR-0019) cover only one sans and one monospace family, so headings, body text and code would all look alike.

**Decision.**
- Ship three OFL-1.1 families as files under `crates/re_compat/assets/fonts/`: Montserrat (weights 400, 500, 700, 900), Source Sans 3 (400, 700) and Roboto Mono (400, 700). That is 8 files, about 307 KB, latin subset only.
- The source is the `@fontsource` packages on npm: montserrat 5.3.0, source-sans-3 5.3.1, roboto-mono 5.3.0. Their WOFF 1.0 files are converted to TTF by a small script under `crates/re_compat/assets/fonts/tools/`, which only unpacks the zlib-compressed tables and changes no glyph data. `crates/re_compat/assets/fonts/README.md` records the package versions and the conversion step.
- Each family's `OFL.txt` sits next to its files and goes into the generated third-party notice.
- `crates/re_compat/assets/fonts/substitutes.json` maps compat font families to a bundled family:
  - geometric display families (Gotham and its relatives) go to Montserrat;
  - the default sans families go to Source Sans 3;
  - code and monospace families go to Roboto Mono;
  - any unknown family falls back to Source Sans 3.
- No vendor font file, and no font derived from one, is in the repository or downloaded at run time.

**Alternatives.**
- egui's default fonts only. Rejected: two families cannot tell headings, body text and code apart.
- Fetch fonts at run time. Rejected: the engine works offline and runs are deterministic.
- Full (non-latin) subsets. Deferred: about ten times the size. A project can add its own fonts.

**Consequences.**
- Text metrics differ from the vendor's fonts, so text that relies on exact widths (`TextFits`, `TextBounds`) can wrap differently.
- OFL-1.1 allows bundling and modification but forbids selling the fonts alone, and a modified font may not keep its reserved name. We ship them unmodified apart from the container format, which the OFL allows.
- The license question (Open question 1) now covers these files as well as egui's fonts.

---

## ADR-0039: Skins from glTF, GPU skinning, and animation graphs as data

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP21). The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). Evidence: the WP21 probe (`research/m3/wp21/`): the skinned vertex layout and the vertex-stage palette run on wgpu 30.0.1 on Vulkan (lavapipe) and GL (llvmpipe through EGL) and match the CPU to 4.8e-7; pose evaluation costs 20.8 µs for a 65-joint character with three clips; f32 evaluation stays within 8.5e-6 of an f64 reference even on a 63-deep joint chain.

**Context.** ADR-0030 limited `re_anim` in M1 to sampling glTF node clips onto entity transforms, plus tweens; M2 WP14 added keyframe tracks, a priority mixer and a two-bone solver for jointed rigs. There is no skinning: `re_assets` ignores glTF skins, `JOINTS_n` and `WEIGHTS_n`, and `re_render` has one vertex layout (`Vertex`, 48 bytes, locations 0–3) plus the instance stream (locations 4–11). M3 needs skinned characters driven by an animation graph that scripts, the editor and agents can change (M3 plan, WP21). The graph must be plain data, like the ProcGraph (`*.graph.json`, ADR-0016), so commands can edit it. Facts checked for this ADR:
- **gltf 1.4.1** (the pinned version, MIT OR Apache-2.0) exposes everything needed: `Document::skins()`, `Skin::joints()`, `Skin::skeleton()`, `Skin::reader(..).read_inverse_bind_matrices()`, `Node::skin()`, and `Reader::read_joints(set)` / `read_weights(set)` with `into_u16()` and `into_f32()` casts over the U8, U16 and F32 encodings glTF allows. No new dependency is needed.
- **wgpu 30.0.1.** WebGPU default limits allow 16 vertex attributes and 8 storage buffers per stage; the downlevel defaults allow 4. Storage buffers in the vertex stage need `DownlevelFlags::VERTEX_STORAGE`. Measured: lavapipe reports it with 48 storage buffers per stage and 32 attributes; GL (llvmpipe, OpenGL 4.5 / ES 3.2) reports it with 16 and 16. DX12 (WARP) and Metal support vertex storage buffers; WebGL2 does not, and the web is not an M3 target.
- **Cost** (probe `eval`, one core of a 2.1 GHz Xeon): sampling one clip's translation, rotation and scale for 65 joints is 4.5 µs; the hierarchy walk and palette for 65 joints is 1.0 µs; a full evaluation (three clips, a 1D blend, an additive layer, palette) is 20.8 µs, 32.2 µs with 301 keys per channel, and 84.4 µs for 256 joints.

**Decision.**

*Skins in the contract layer (`re_types`, frozen at PR "skins").*
- `re_types::anim` gains `SkinData`: the skin's name, its joints in an order where every parent comes before its children (`JointDesc { name, parent: Option<u16>, rest: Trs }`), one inverse bind matrix per joint, and per-joint bind-space bounds of the vertices that joint influences (for skinned bounds and picking). At most 65,535 joints per skin, the range of a `u16` joint index; glTF sets no lower limit, so neither do we.
- `re_types::mesh` gains `SkinWeights { joints: Vec<[u16; 4]>, weights: Vec<[u16; 4]> }`, one entry per vertex of the `MeshData` it goes with. Weights are unorm16 and sum to 65,535. `MeshData` and `Vertex` do not change, so static meshes pay nothing and the 14 existing `MeshData { .. }` literals keep compiling.
- `re_types::render` gains `SkinnedDrawItem { entity, mesh, material, world, flags, palette: u32, joints: u32, bounds: Aabb }` and two `RenderSnapshot` fields with serde defaults: `skinned: Vec<SkinnedDrawItem>` and `joint_palette: Vec<Mat4>`. `palette` is the item's first matrix in `joint_palette`, so one buffer holds every character's palette and `DrawItem` stays `Copy` and unchanged. A palette matrix is `joint_model * inverse_bind` in the mesh's model space; `world` places the mesh as for a static item. The snapshot literals in `re_render`, `re_app`, `re_compat` and the `re_types` tests that do not end in `..Default::default()` get it in the "skins" PR; WP20's "post" PR adds fields to the same struct, so whichever lands first converts them.

*glTF import (`re_assets`, maintainer-reviewed).*
- The importer reads every skin into `SkinData`, and `JOINTS_0`/`WEIGHTS_0` (plus `JOINTS_1`/`WEIGHTS_1` when present) into `SkinWeights`. More than four influences keep the four largest. Weights are renormalised to sum to one before quantising, and the rounding remainder goes to the largest weight. A vertex with all-zero weights binds to the skin's first joint.
- Joint names are the node names the clip importer already uses for targets, so the existing `ClipData` animates joints with no format change. Within one skin, a duplicate joint name is renamed `<name>#<node index>` with a warning; clips from the same file follow the rename.
- Clips from another file (an animation library) bind to a skin by joint name. Unmatched clip targets are ignored, with one warning per clip and skin.
- A skinned primitive is imported as an `ImportedMesh` with `skin: Option<(usize, SkinWeights)>`. `GLTF_IMPORTER_VERSION` goes to 2, so cached imports rebuild.
- Diagnostics are a new `re_assets` block, `RE7060`–`RE7069`, reserved by the maintainer: malformed skin (joint count against inverse bind matrices, a joint outside the skin's tree, joints and weights of different lengths), more than four influences (warning), and duplicate joint names (warning).
- The plan names the skin parts as `re_assets/src/gltf/`. Today the importer is one file, `gltf_import.rs`. The "skins" PR first splits it into `gltf/{mod,skin}.rs` as a mechanical move the maintainer reviews; WP21 owns only `gltf/skin.rs`.

*GPU skinning (`re_render`, reviewed by WP20).*
- A vertex-shader path with four weights per vertex (linear blend skinning). Skinned meshes upload with `Renderer::upload_skinned_mesh(&MeshData, &SkinWeights) -> Result<MeshHandle, Diagnostic>`, which adds a second vertex buffer of 16 bytes per vertex: joints `Uint16x4` at location 12 and weights `Unorm16x4` at location 13. A skinned instance stream adds the palette base as `Uint32` at location 14. That is 15 attributes, within the 16 GL allows; the probe ran this exact layout on Vulkan and GL.
- The frame's `joint_palette` is uploaded once per frame into one read-only storage buffer, bound in the vertex stage of the skinned pipelines. The shader skins the position with the weighted sum of four palette matrices, and the normal with its upper 3×3. Joints with non-uniform scale therefore shade slightly wrong, which is the usual trade-off of linear blend skinning.
- Every pipeline that draws meshes gets a skinned variant: forward (opaque and blended), shadow, and the selection mask. They share the existing shaders through `#include` and a skinned entry point, so lighting and materials stay one code path.
- When the adapter lacks `VERTEX_STORAGE`, the renderer warns once with a new `re_render` code. It then draws skinned items in their bind pose (palette set to identity). It does not fake animation. No M3 target lacks the flag.
- Skinned bounds are the union of each joint's bind-space bounds moved by its palette matrix, computed in `re_anim`. They are conservative, and nothing culls on them yet.

*Pose evaluation and the animation graph (`re_anim`, `re_types::anim`, frozen at PR "graphs").*
- `re_anim` gains `Skeleton` (from `SkinData`), `Pose` (local translation, rotation and scale per joint, indexed, not named), and `BoundClip`: a `ClipData` resolved once against a skeleton into per-joint channel indices, with a cached key cursor per channel. The existing name-based `Sampler` and the keyframe `Mixer` stay for entity rigs and the compat layer.
- `AnimGraph` is plain data in `re_types::anim`, saved as `*.animgraph.json` with `"format": "real-engine/animgraph"`, `"version": 1` and unknown keys refused, and a JSON Schema exported like `graph.schema.json`. Every element has an `id` unique within its kind, which commands address:
  - `params`: `float`, `bool` or `trigger`, each with a default.
  - `nodes`, the blend tree:
    - `clip`: a clip reference `<model path>#<clip name>`, with speed, looping and `root_motion`.
    - `blend1d`: a float parameter, and children at sorted thresholds, blended piecewise linearly and clamped at the ends.
    - `blend2d`: two float parameters, and children at points, weighted with gradient-band (freeform cartesian) interpolation, which needs no triangulation and is easy to check against a reference.
    - `additive`: a base node plus an additive child with a weight or a weight parameter.
    Blend spaces keep their children in phase by normalised time (`sync`, on by default), so walk and run cycles line up.
  - `layers`: each has a state machine, a blend mode (`override` or `additive`), a weight, and an optional mask. An additive layer applies its delta from the clip's first frame, or a named reference clip.
  - A state machine has `states` (each with one node), an `entry` state, and `transitions`. A transition has `from` (a state or `any`), `to`, a crossfade `duration`, an optional normalised `exit_time`, `conditions`, and `interruptible`. A condition compares a float parameter (`<`, `<=`, `>`, `>=`), tests a bool, or consumes a trigger. Transitions are checked in file order and the first that passes wins. An interrupted crossfade freezes the blended pose and fades from it.
  - `masks`: per-joint weights given by joint name, with `subtree: true` to cover a joint's descendants. A name the skeleton lacks is a warning when the graph binds, not an error, so one mask fits several skeletons.
- Blending is per joint: translation and scale interpolate linearly, and rotation uses normalised lerp with a hemisphere fix. glTF's own linear rotation keys stay spherical. With nlerp, a blend needs no `acos` or `sin`, so blending uses no platform maths function. Sampling follows the maths rules of ADR-0036 (D2) for simulated paths once it is accepted.
- **Root motion** is an option per clip node. The root joint is the skin's `skeleton` node, or else its first joint. The root's horizontal translation and yaw since the previous step are taken out of the pose, blended with the same weights as the pose, and wrapped correctly when a clip loops. The delta moves the entity: through `move_character` when it has a `CharacterController`, otherwise by writing its `Transform`. Vertical motion stays in the pose.
- **When evaluation runs.** The graph advances (parameters, transitions, times, root motion) and the pose is evaluated in the PreAnimation phase of every fixed step, so poses are deterministic and replayable like every simulated path. Joints are not entities: a 65-joint character is one pose array, not 65 transforms in the World. The state hash covers the entity transform and the graph state (active states and normalised times), not the joint array. Characters evaluate in parallel on the shared worker pool. At the measured 20.8 µs, 100 three-clip characters cost about 2.1 ms on one core. The render snapshot uses the latest step's pose, not an interpolation between steps; that may look stepped above 60 Hz, and interpolating local poses is the follow-up if the valley shows it.
- **Seams for other WPs.**
  - WP22's ragdolls read and write the model-space pose through `re_anim::ModelPose`, with per-joint blend weights, after graph evaluation and before the palette is built.
  - A `BoneAttachment { joint }` component on a child entity puts that child at the joint each step, for weapons, props and cameras.
  - WP27's skinned `MeshPart`s use `Skeleton`, `Pose` and the palette builder; they do not use the graph.

*Scene, scripts and commands (cross-owner, small PRs).*
- `re_scene` (maintainer) gains two components:
  - `SkinnedMesh { asset }`, where `asset` is `<model path>#<skin name>`. It draws every glTF primitive bound to that skin, with the materials from the file.
  - `Animator { graph, playing, speed }`, where `graph` is an `*.animgraph.json` path. Without a graph, `AnimPlayer`'s single clip also drives a `SkinnedMesh`.
  `re_app/src/sync/` gets an `anim_skin` module next to `anim.rs`.
- The Luau library `anim` lives in `re_script` (WP24 reviews), through new `HostServices` methods in `re_types::host` (maintainer). It has `anim.set(entity, name, value)`, `anim.get(entity, name)`, `anim.trigger(entity, name)`, `anim.state(entity, layer?) -> string` and `anim.joint(entity, joint) -> (vector, quat)` in world space. An unknown parameter, or a value of the wrong type, raises a coded error that lists the graph's parameters.
- Commands (registered by `re_app`, maintainer-reviewed):
  - `animgraph.create`.
  - `animgraph.edit { path, ops }`, where each op is `upsert` or `remove` of one `param`, `node`, `state`, `transition`, `layer` or `mask` by id. The whole batch is validated and journalled as one undo step, like `graph.set_param`.
  - `animgraph.validate`.
  - `anim.set_param`, for a running play session.
  The CLI, the editor (WP25's canvas) and MCP agents all reach these through the one registry (ADR-0003).
- `re_anim` diagnostics continue in `RE7606`–`RE7699`: an invalid graph (with the path of the bad element), a missing clip, an unknown or mistyped parameter, a mask joint the skeleton lacks (warning), and a cycle in the blend tree.

*Tests (WP21 acceptance).*
- An f64 reference evaluator in `re_anim/tests`. Sampling, 1D and 2D blending, additive layers, masks, crossfades and root motion are checked against it on synthetic skeletons, to 1e-5 absolute in model-space matrices with positions within 10 m. The probe measured at most 8.5e-6 for a 63-deep chain and 2.3e-6 for a humanoid-sized tree.
- The skinned golden fixture is a small `.glb` that a test helper generates (a two-bone bar and a 20-joint figure). No third-party model is committed, so no asset licence is involved. The golden shows the figure in three poses on lavapipe and GL, with the existing goldens' tolerance.
- Import tests cover each `RE7060`–`RE7069` case, U8, U16 and F32 weights, more than four influences, and clip binding by name across files.

**Alternatives.**
- **Joints as entities** (one `Transform` per bone, as with M1 node clips). Attaching things, and scripting individual bones, falls out naturally. But a 65-joint character becomes 65 entities with change tracking, hierarchy propagation and scene-file noise, and joint transforms enter the state hash. Rejected; `BoneAttachment` and `anim.joint` cover attaching and reading a bone.
- **CPU skinning**, or skinning in a compute pre-pass. CPU skinning re-uploads every vertex every frame. A compute pre-pass adds a storage-buffer write and a second vertex buffer per character, and pays off only with many passes per mesh or with morph targets. Neither is in M3. The vertex path can move to compute later behind `upload_skinned_mesh`.
- **Palette in a uniform buffer, or in a float texture.** A uniform array fits only 256 joints in the 16 KiB downlevel binding, and needs a binding per character or dynamic offsets aligned to 256 bytes. A texture works without `VERTEX_STORAGE`, but costs four texel fetches per matrix and texture-size juggling. Every M3 target has vertex storage, so one storage buffer is the simplest choice. The texture path is the fallback if the web returns.
- **Dual-quaternion skinning.** It avoids the volume loss of linear blending at twisted joints, but handles scale badly and is not what glTF content is authored for. It can become a per-material option later.
- **2D blend by Delaunay triangulation.** It is standard and exact on the hull, but it needs triangulation code, and it changes discontinuously when points move. Gradient band is smooth, simple, and fast enough for the 4 to 16 points a blend space has.
- **A graph format in Luau or TOML.** Luau graphs are code, which commands and agents cannot edit as data. TOML handles deep nesting poorly. JSON with a schema matches `*.graph.json` and the MCP tools.

**Consequences.**
- One new vertex buffer and three pipeline variants in `re_render`, owned by WP20 and reviewed there. Static meshes are unchanged.
- No new dependency and no `DEPENDENCIES.md` row.
- The contract grows in four places: `SkinData`, `SkinWeights`, `SkinnedDrawItem` with the snapshot fields, and `AnimGraph`. `RenderSnapshot` literals across four crates gain `..Default::default()` once.
- Cross-owner work is small but spread out: `re_scene` components and `HostServices` methods (maintainer), the `anim` Luau library (WP24), the commands and sync module in `re_app` (maintainer), and the skinned pipelines (WP20). WP21 writes each change and its owner reviews it.
- Not in M3: morph targets, inverse kinematics beyond the two-bone solver, facial animation, retargeting between differently named skeletons, more than four influences per vertex, GPU-driven culling of skinned items, and interpolating poses between fixed steps.

## ADR-0040: Vehicles and ragdolls: oxijolt's `vehicle` and `ragdoll` modules behind `PhysicsCmd`

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP22). The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). Evidence: WP22's phase-0 benches. They are summarised under "Measured"; the full tables are in the maintainers' research bundle.

**Context.** M3 needs drivable vehicles and ragdolls for skinned characters (M3_PLAN, WP22). The valley example (WP28) drives a car, and WP27 maps Roblox-style characters onto ragdolls. The pinned `oxijolt =1.1.0+jolt-5.6.0` (MIT OR Apache-2.0, ADR-0008) already wraps both Jolt features, so neither needs a new dependency.

Its vehicle and ragdoll APIs, as read from the crate source:

- **Wheeled vehicles.**
  - `PhysicsWorld::create_wheeled_vehicle(chassis: BodyId, &WheeledVehicleSettings)` attaches Jolt's `VehicleConstraint` with a `WheeledVehicleController` to a dynamic body the caller made.
  - Settings: wheels (`WheelSettings`: attachment point, radius, width, suspension min/max/preload and spring, steering axis, max steer angle, longitudinal and lateral friction curves, brake and hand-brake torque), differentials (`VehicleDifferentialSettings`: wheel pair, ratio, left/right split, limited slip, share of engine torque), anti-roll bars, the engine (torque curve, min/max rpm, inertia), the transmission (automatic or manual gears, shift points, clutch), and the pitch/roll limit.
  - Every value Jolt asserts on or divides by is validated, and a bad value returns `VehicleError::InvalidValue` instead of reaching Jolt.
  - `WheeledVehicleSettings::car(front_left, radius, tester)` is a four-wheel front-drive preset.
- **Collision testers.**
  - `VehicleCollisionTester::{ray, cast_sphere, cast_cylinder}`, each querying as one object layer.
  - Wheels never hit their own chassis, sensors or soft bodies.
  - There is no per-sub-shape filter, so a wheel cannot skip one child of a compound.
- **Driving.**
  - `vehicle_mut(id).set_driver_input(DriverInput { forward, right, brake, hand_brake })`, with `forward` and `right` in [−1, 1] and `brake` in [0, 1].
  - `set_gravity` overrides gravity per vehicle; `set_max_pitch_roll_angle` and `set_collision_tester` change those settings.
- **Readout.** `vehicle(id)` gives per wheel the contact (body, point, normal, slip directions), suspension length, angular velocity, rotation and steer angle, plus `wheel_world_transform(i)`, `engine_rpm()` and `current_gear()`.
- **Two more kinds.**
  - `create_motorcycle(MotorcycleSettings)`: two wheels, a lean controller, and the same `DriverInput`.
  - `create_tracked_vehicle(TrackedVehicleSettings)`: two tracks, with its own `TrackedDriverInput { forward, left_ratio, right_ratio, brake }`.
  - The id is typed by kind (`VehicleId<K>`), so the wrong input does not compile.
- **Ragdolls.**
  - `Skeleton::new(&[SkeletonJoint { name, parent }])` defines the skeleton.
  - `RagdollSettings::new` or `new_stabilized(skeleton, parts)` builds the settings. Each part is one body per joint plus a `RagdollJoint` to its parent: `SwingTwist`, `Hinge` or `SixDof`, with limits, friction and motors.
  - `create_ragdoll(settings, pose, activation)` creates the ragdoll. A ragdoll's own parts never collide with each other, but they do collide with other ragdolls.
- **Driving and reading a ragdoll.**
  - Driving: `RagdollMut::{set_pose, drive_to_pose_using_kinematics, drive_to_pose_using_motors, stop_motors, set_motion_type, set_linear_and_angular_velocity, activate}`.
  - Reading: `pose()`, `root_transform()`, `is_calm()`, and `SettleDetector`.
- **Skeleton mapping.** `SkeletonMapper` maps poses between a coarse ragdoll skeleton and a detailed animation skeleton. Joints are matched by name. `map` goes from ragdoll to animation and `map_reverse` from animation to ragdoll, and both are pure functions. The mapper offers translation locks.
- **Structure.**
  - Vehicles and ragdolls take part in `WorldState` save and restore (driver input, drivetrain, track speeds), in `rebase` (the floating origin of ADR-0031), and in body-count and id checks.
  - A chassis cannot be removed while its vehicle exists, and a ragdoll part cannot carry a vehicle.
- **Determinism.** oxijolt's own tests run fleets of cars, tanks and motorcycles, and ragdolls, bit for bit across worker counts and processes. Our benches confirm this (below).
- **Gotchas.**
  - A sleeping chassis gets no drive force, and a new driver input does not wake it.
  - `WorldState` saves driver input and drivetrain state, but not what the setters change (gravity override, pitch and roll limit, tester).

The engine side has its own constraints:
- `re_physics` hides oxijolt (ADR-0008).
- Subsystem crates do not depend on each other (ARCHITECTURE §2), so `re_physics` cannot see `re_anim`'s skeletons.
- Components live in `re_scene`, the sync in `re_app`, and the Luau API in `re_script`. All three have other owners in M3.

**Decision.**

*Vehicles (PR "vehicles").*
- `re_types::physics` gains plain-data vehicle types and three commands, added in a maintainer-reviewed frozen-contract change.
  - `PhysicsCmd::AddVehicle { id, desc: VehicleDesc }`: `id` is the chassis entity, which must already have a dynamic body (an `AddBody` in the same buffer sorts before it).
  - `PhysicsCmd::RemoveVehicle { id }`: it leaves the chassis body in place.
  - `PhysicsCmd::SetVehicleInput { id, input: VehicleInput }`: `throttle` in [−1, 1], `steer` in [−1, 1] (positive is right), `brake` and `handbrake` in [0, 1].
- **Apply order.** The new commands take ranks after every existing kind, so the ranks of today's commands, which `re_types`' tests pin, do not change: `RemoveVehicle` 12, `AddVehicle` 13, `SetVehicleInput` 14.
  - A removal and a re-creation of the same vehicle in one buffer still apply in that order.
  - Because `RemoveVehicle` sorts after `RemoveBody`, removing a chassis body removes its vehicle first. Jolt would refuse the body otherwise. This is the same rule `RE5014` applies to constraints. The later `RemoveVehicle`, if any, is then a no-op.
  - `PhysicsCmd` is matched exhaustively only inside `re_types` and `re_physics` today, so the new variants break no other crate.
- **`VehicleDesc`.** It holds `kind: VehicleKind` (`Wheeled` default, `Motorcycle`), the wheels, the engine, the transmission, anti-roll bars, the pitch and roll limit, and the tester.
  - Every field has a default taken from Jolt's defaults, through oxijolt's presets.
  - Units are metres, kilograms, N·m, rpm and degrees.
  - Wheels are `WheelDesc` values: chassis-local attachment point, radius, width, suspension travel and spring (frequency and damping), max steer angle, brake and hand-brake torque, and `driven`.
  - Differentials are derived from the driven wheels: wheels with the same local z (within 1 cm) make an axle, with left being +X as in Jolt. An explicit `differentials` list overrides this.
- **Testers.**
  - The default tester is `CastCylinder`, which matches the tire. It cost the same as a ray within noise (below).
  - `Ray` and `CastSphere` stay available.
  - The tester queries as the chassis layer's moving object layer, so a wheel touches exactly what the chassis's `Layer` collides with in the `LayerTable`. No new object layers are needed.
- **Tracked vehicles are declined for M3.** They need their own input (`left_ratio`, `right_ratio`) and the valley does not use one. They can be added later as a `VehicleKind` variant and a `SetTrackedInput` command, an additive change.
- **Waking.** `re_physics` wakes the chassis when a `SetVehicleInput` differs from the last one. This makes up for the sleeping-chassis gotcha.
- **Readout.**
  - The chassis pose arrives through `poses_into` like any body.
  - `Physics::vehicle(id) -> Option<VehicleState>` gives speed along forward, engine rpm, gear, and per wheel its contact entity, contact point, suspension length, angular velocity and steer angle.
  - `Physics::vehicle_poses_into(&mut Vec<WheelPose>)` gives each wheel's world pose (`{ id, wheel, pos, rot }`) for rendering, once per step, in entity then wheel order.
- **Errors.** Invalid vehicles get diagnostic `RE5018` ("Invalid vehicle"). This covers a missing or non-dynamic chassis, a body that already has a vehicle, a value oxijolt refuses (its message is kept), and a motorcycle without exactly two wheels. The command is dropped and nothing is created.
- **Engine side** (cross-owner PRs):
  - `re_scene` gets a `Vehicle` component on the chassis (kind, engine and transmission fields, tester) and a `VehicleWheel` component on child entities. The child's `Transform` is the attachment point, and wheels are ordered by `Children`. The maintainer reviews this.
  - `re_app`'s physics sync turns them into `AddVehicle`, and writes wheel poses back to the wheel entities' rendered transforms each frame. The maintainer reviews this.
  - The Luau API in `re_script` is `physics.drive(e, { throttle, steer, brake, handbrake })` and `physics.vehicle(e) -> { speed, rpm, gear, wheels = { … } }`, with `NoServices` refusing both with `RE3015`. WP24 reviews this.
  - Procedural graphs can then emit vehicles as plain entities.

*Ragdolls (PR "ragdolls", after WP21's PR "skins").*
- **Skeleton data.** `re_physics` takes skeleton data as plain values in `re_types`, not WP21's types:
  - `RagdollDesc`: the parts, plus the **animation skeleton**, which is joint names, parent indices and a neutral pose in model space.
  - `re_app` fills the animation skeleton from WP21's `SkinData` (joint names, parents, inverse bind matrices), so `re_physics` never depends on `re_anim`.
  - Each part is a `RagdollPartDesc`:
    - the animation joint it follows, by name;
    - a shape in that joint's space (capsule, box or sphere);
    - mass;
    - a joint to the parent part (`SwingTwist { cone_deg, twist_min_deg, twist_max_deg }`, `Hinge { min_deg, max_deg }` or `SixDof`);
    - friction.
- **`RagdollDesc::from_skeleton(skeleton, joints, total_mass)`.** This procedural default in `re_types` puts capsules along the bones between the chosen joints, gives them swing-twist joints with humanoid limits by default, and splits the mass by capsule volume. A project overrides it part by part. It is a pure function, tested on its own, and it follows the engine's rule that generated data stays editable.
- **Mapping and stabilizing.**
  - Inside `re_physics`, the parts become an oxijolt `Skeleton` and `RagdollSettings::new_stabilized`. A `SkeletonMapper` maps from the ragdoll skeleton to the animation skeleton, with translation locks on every descendant of the root.
  - `new_stabilized` is the default because it bounds parent and child mass ratios, which keeps chains stiff. `stabilize: false` keeps the authored masses.
- **Commands:**
  - `AddRagdoll { id, desc, pose: Option<Vec<JointPose>> }` (in the animation skeleton's local space; `None` is the neutral pose);
  - `RemoveRagdoll { id }`;
  - `SetRagdollMode { id, mode }`;
  - ranks 15 to 19, in the order `RemoveRagdoll`, `AddRagdoll`, `SetRagdollMode`, `SetRagdollTarget`, `ApplyRagdollImpulse`, after the vehicle commands; `RemoveBody` is never needed for parts, which the ragdoll owns;
  - `SetRagdollTarget { id, pose }`: the animation pose the drive aims at, mapped with `map_reverse`;
  - `ApplyRagdollImpulse { id, joint, impulse }`: hit reactions, applied to the part the joint maps to.
- **Modes:**
  - `Animated`: kinematic parts driven to the target with `drive_to_pose_using_kinematics` each step. The ragdoll pushes the world but does not fall.
  - `Simulated`: dynamic parts, motors off, a limp ragdoll.
  - `Powered { strength }`: dynamic parts with `drive_to_pose_using_motors`, an active ragdoll that holds the animation against hits. `strength` scales the motor spring frequency between 0 and the joint's authored value.
  - Switching from `Animated` keeps the parts' current velocities, so a falling character carries its momentum.
- **Ragdoll readout.** `Physics::ragdoll(id) -> Option<RagdollState>` gives the mode, the root pose, `calm` (from `SettleDetector`), and the **simulated pose in the animation skeleton's local space** (`SkeletonMapper::map`). The renderer and `re_anim` therefore read one skeleton.
- **The animated/simulated blend** is plain data:
  - It is in `re_types` (`RagdollBlend { weight, seconds }`). `blend_pose(animated, simulated, weight)` is a pure per-joint lerp of translation and slerp of rotation.
  - `re_app` applies the blend after `PostSimulation`. It blends the animation graph's pose (WP21) with the ragdoll's simulated pose, ramping the weight over `seconds`.
  - This gives "fall into a ragdoll" (weight 0 → 1 when the mode turns `Simulated`) and "get up" (1 → 0 while a get-up clip plays, after `calm`).
  - Physics itself never blends, so a replay's physics state does not depend on render-rate blending.
- **Ragdoll errors.** Diagnostic `RE5019` ("Invalid ragdoll") covers a part naming a missing joint, a broken hierarchy (oxijolt's `UnmappedJoint` and `HierarchyMismatch`), invalid limits and a bad pose. The command is dropped.
- **Engine side, ragdolls** (cross-owner PRs as for vehicles):
  - a `Ragdoll` component in `re_scene` (the part list or "generate", total mass, mode);
  - Luau: `physics.ragdoll(e, mode)`, `physics.ragdoll_state(e)` and `physics.ragdoll_impulse(e, joint, impulse)`.

*Both.*
- **Determinism.**
  - Vehicles and ragdolls run inside the Jolt step, and their only inputs are commands. The command sort and oxijolt's bit-for-bit behaviour across worker counts keep D1.
  - Once WP18's PR "d2" turns on `cross-platform-deterministic`, the reference poses of the acceptance tests are recorded with it.
  - D2 then compares exact bits across platforms. Turning the feature on moved our reference pose by 4.6 cm, so references recorded without it cannot be reused.
- **Save and restore.** `save_state` and `restore_state` cover vehicles and ragdolls through `WorldState`. `motion_state` and `apply_motion_state` (carrying motion into a fresh world) carry the chassis and part bodies' velocities, but not drivetrain state (rpm, wheel spin). This is documented on `MotionState`.
- **Limits.** Vehicles and ragdolls are bounded by the world's body limit and the ±5 km limit (`RE5006`).

**Measured** (4 vCPU Xeon @ 2.80 GHz, release, 4 Jolt worker threads, 1/60 s steps).

| What | Result |
|---|---|
| Open-loop path, flat and 10° ramp, 720 ticks: same run twice; 1 vs 4 threads | bit-identical; bit-identical |
| Same path, gentle inputs: start moved 1 µm / 1 mm; start yaw 0.001° | end moves 0.2 mm / 1.0 mm; 0.11 m and 0.08° |
| Same path, aggressive inputs (full throttle, two steering phases): start yaw 0.001° | end moves 0.47 m and 0.70° |
| Same path with `cross-platform-deterministic` | end moves 4.6 cm; still bit-identical across threads |
| 1 / 8 / 32 / 128 cars, ray tester, mean step | 0.128 / 0.177 / 0.340 / 0.803 ms (6.3 µs per car at 128) |
| same, cylinder tester | 0.131 / 0.193 / 0.320 / 0.854 ms |
| same, ray, with `cross-platform-deterministic` | 0.143 / 0.190 / 0.339 / 0.768 ms (within noise) |
| 1 / 8 / 32 tumbling 12-part ragdolls, mean step (p95) | 0.291 (0.382) / 0.312 (0.533) / 0.594 (1.018) ms |
| 8 ragdolls, 300 ticks, 1 vs 4 threads | bit-identical |

Budgets for M3, checked by benches in `re_physics`:
- 32 driving cars within 1 ms mean step;
- 32 active ragdolls within 2 ms mean step.

**Acceptance tests.**
- **Vehicle path test.**
  - A car drives the gentle open-loop path: flat ground, then a 10° slope, 720 ticks.
  - It must end within **0.15 m and 1.0°** of the committed reference pose on any platform before D2 is on. This is three times the 4.6 cm a change of floating-point mode caused, and above the 0.11 m that a 0.001° start-yaw change caused; a last-bit difference is about 300 times smaller than that change.
  - On the same machine it must be bit-identical with 1 and 4 threads.
  - From WP18's PR "d2" on, the test also compares an exact digest of the chassis and wheel state with the committed one on all three CI platforms.
- **Aggressive path.** It is not a tolerance test, because 0.001° at the start moves its end by 0.47 m. It only checks determinism on one machine.
- **Ragdoll tests.**
  - A ragdoll from a generated humanoid skeleton falls and becomes calm on flat ground.
  - Animated mode follows a target pose within 1 cm per joint.
  - Powered mode recovers a pose after an impulse.
  - The mapped pose round-trips through `map_reverse`.
  - Determinism holds across worker counts.

**Alternatives.**
- **Our own raycast vehicle** on plain bodies and `ShapeCast`. That duplicates Jolt's tested suspension, tire and drivetrain model for no gain, and adds Rust work in the step, which hurts determinism.
- **Exposing oxijolt's settings structs directly.** This breaks ADR-0008, and pins game data to oxijolt's API.
- **Ragdolls driven by WP21's skeleton type in `re_physics`.** This breaks the subsystem rule (§2). The plain-data `RagdollDesc` filled by `re_app` keeps the crates apart.
- **Blending inside `re_physics`.** It would tie render-rate blending to the physics state, so we blend in the sync instead.
- **Tracked vehicles now.** They have a different input path and no M3 consumer, so they are deferred.

**Consequences.**
- The vehicle and ragdoll contracts freeze at PRs "vehicles" and "ragdolls"; WP27 and WP28 consume them.
- No new dependency and no DEPENDENCIES.md row.
- `re_physics` gains `RE5018` and `RE5019` with explain pages.
- Cross-owner PRs are needed for `re_scene` components, the `re_app` sync and the `re_script` API.
- A wheel cannot ignore one child of a compound (oxijolt has no per-sub-shape filter). A vehicle driving over static cells (ADR-0031) sees the whole cell's compound, which is the wanted behaviour.
- Soft bodies, destruction and tracked vehicles stay out of M3.

## ADR-0041: Audio effects graph: named buses on kira tracks, kira's built-in effects, snapshots in `real.toml`

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP22). The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). Evidence: WP22's phase-0 kira bench, summarised under "Measured".

**Context.** M3 needs audio buses with per-bus effects (reverb, low-pass and high-pass filters, delay, compressor), a mixer snapshot in `real.toml`, a Luau API, and a headless path that applies the same graph so tests can check levels without a device (M3_PLAN, WP22). `re_audio` today plays every sound on kira's main track. Spatial voices are spatial sub-tracks under it, and `CaptureBackend` (ADR-0030) drives kira's real `Renderer` at a fixed 48 kHz. The pinned `kira =0.12.5` (MIT OR Apache-2.0) provides the following, read from its source:

- **Tracks.**
  - The main track.
  - Sub-tracks, which nest (`TrackHandle::add_sub_track`, `add_spatial_sub_track`); a child sums into its parent.
  - Send tracks, which take volume and effects only, cannot play sounds, and output only to the main track.
  - A sub-track routes to a send with `TrackBuilder::with_send(&send, volume)`. The send is post-effect and post-fader, on top of the normal output.
- **Routes are fixed after build.** `set_send` changes an existing route's volume, and a new route is refused (`NonexistentRoute`).
- **Effects** are added on the builder (`add_effect`, `with_effect`, `add_built_effect` for a custom `Box<dyn Effect>`) and run in insertion order. **No handle can add or remove an effect after the track is built.**
- **Built-in effects** (none behind a feature):
  - `reverb`: Freeverb, with feedback, damping, stereo width and mix.
  - `filter`: a 12 dB/oct state-variable filter, with low-pass, band-pass, high-pass and notch modes, cutoff, resonance and mix.
  - `delay`: delay time, which is fixed after init, plus feedback and mix.
  - `compressor`: threshold, ratio, attack, release, makeup gain and mix. It has no sidechain, knee or lookahead.
  - `eq_filter`: bell, low shelf and high shelf.
  - `distortion`: hard and soft clip.
  - `panning_control` and `volume_control`.
- **Parameter changes.** Every parameter except the delay time has a handle setter taking `Value` (fixed, from a modulator, or from listener distance) and a `Tween`.
  - Commands cross to the audio thread through `triple_buffer`, so only the last write per parameter between two render batches applies.
- **Capacities** (`AudioManagerSettings`): 128 sub-tracks, 16 send tracks and 8 listeners. Each track also holds 128 sounds and 128 child sub-tracks. The number of effects per track is unbounded.
- **Determinism and sample rate.**
  - The DSP has no randomness, clock, SIMD or threads.
  - Results depend on the sample rate: reverb and delay size their buffers in `init`, and the filter, EQ and compressor use `dt`.
  - The filter, EQ and compressor call `tan`, `exp`, `powf` and `log10` from the platform maths library, so bits can differ between glibc, macOS and MSVC.
- **Known panic.** `delay_time(Duration::ZERO)` panics (`chunks_mut(0)`, `delay.rs:76`).

**Decision.**
- **Buses.**
  - A project declares named **buses** in `real.toml`. Each bus is a kira sub-track whose parent is another bus or the main track. The audio graph is a tree of buses plus up to 15 **sends** (kira's limit of 16, with one kept spare).
  - The graph is built when `Audio` starts (`Audio::set_graph`), and rebuilt when `re_app` hands it a changed `[audio]` section after a project reload. The rebuild stops the sounds on the buses it replaces and reports this at info level.
  - Without an `[audio]` section, `re_audio` builds the default graph: buses `music`, `sfx` and `ui` under the main track, and no effects. Projects that do not configure audio sound as they do today.
- **Effects** are kira's built-ins, wrapped in a plain-data `re_types::audio::EffectDesc`:
  - `Reverb { feedback, damping, stereo_width, mix }`;
  - `Filter { mode: LowPass | HighPass | BandPass | Notch, cutoff_hz, resonance, mix }`;
  - `Delay { seconds, feedback_db, mix }`;
  - `Compressor { threshold_db, ratio, attack_ms, release_ms, makeup_db, mix }`;
  - `Eq { kind: Bell | LowShelf | HighShelf, hz, gain_db, q }`;
  - `Distortion { kind, drive_db, mix }`.
  - Defaults are kira's, and the units are named.
  - A bus's effects run in the listed order, followed by a meter (below).
  - Steeper filters are written as several stages.
- **Validation.**
  - `re_audio` validates every value before kira sees it: a delay of at least 1 ms, a cutoff between 10 Hz and the Nyquist frequency, a ratio of at least 1, and finite values throughout.
  - An invalid effect is dropped with the new `RE7406` ("Invalid audio effect") and the rest of the bus still builds. A cycle, an unknown parent or send, or more than 15 sends is `RE7407` ("Invalid audio bus graph"). A graph with that error falls back to the default graph, so a typo never silences a game.
- **`real.toml` format** (added to ARCHITECTURE §8 by the maintainer):

```toml
[audio]
default_bus = "sfx"              # where sources without a bus play

[audio.buses.music]
volume_db = -6.0

[audio.buses.sfx]
sends = { world_reverb = -9.0 }  # send name = level in dB
effects = [
  { kind = "compressor", threshold_db = -18.0, ratio = 3.0 },
]

[audio.buses.footsteps]
parent = "sfx"
effects = [{ kind = "filter", mode = "high_pass", cutoff_hz = 120.0 }]

[audio.sends.world_reverb]
effects = [{ kind = "reverb", feedback = 0.85, damping = 0.3, mix = 1.0 }]

[audio.snapshots.underwater]     # a mixer snapshot: bus volumes and effect parameters
buses = { music = -12.0, sfx = -3.0 }
effects = [{ bus = "sfx", effect = 0, cutoff_hz = 600.0 }]   # sets only the fields given
```

- **Effects are addressed by bus and index.** A snapshot or command sets only the fields it names, through the effect's kira handle. A filter's `mode`, an EQ's `kind` and a distortion's `kind` can change through their handles, but turning one effect into another (a filter into a delay) cannot, and is refused with `RE7406`.
- **Snapshots.**
  - A snapshot is a named set of bus volumes and effect parameters.
  - Applying one tweens to it over `seconds`, with kira's linear tween. Fields it does not name keep their current values.
  - There is no stack of snapshots: the last one applied wins, and applying the empty snapshot `default` returns to the values in `real.toml`.
- **Commands.** New `AudioCmd` variants, added in a maintainer-reviewed frozen-contract change, follow the existing `SetSourceAttenuation` pattern. Existing variants are not changed, so no constructor elsewhere breaks.
  - `SetSourceBus { entity, bus }`: sent before `StartSource`, it applies until the next `StopSource`.
  - `PlayOneShotOnBus { clip, position, volume, bus }`.
  - `SetBusVolume { bus, volume_db, seconds }`.
  - `SetEffectParam { bus, effect, param, value, seconds }`. `param` is a field name from `EffectDesc`.
  - `ApplySnapshot { name, seconds }`.
- **Unknown bus or snapshot.** A command naming an unknown bus or snapshot warns once per name with `RE7408` and falls back to `default_bus` or does nothing.
- **Spatial voices** become spatial sub-tracks under their bus, through `TrackHandle::add_spatial_sub_track`, so a spatial footstep goes through the `footsteps` bus's filter and the reverb send.
  - Spatial voices and buses share kira's sub-track capacity. `re_audio` raises `sub_track_capacity` to 256 on the manager and on each bus, and keeps `RE7403` when the capacity is reached.
- **Levels.**
  - A custom kira `Effect` written in `re_audio` (`Meter`, about 40 lines) is appended to every bus and send. It records peak and RMS per channel for each render batch into an atomic, lock-free slot.
  - `Audio::bus_levels() -> Vec<BusLevel { bus, peak_db, rms_db }>` reads them. Tests use it headless, the editor's mixer panel (WP25) can draw it, and `real verify` can report it.
  - kira has no metering API. This is our code, not a dependency.
- **Headless fallback.**
  - `CaptureBackend` already runs kira's real renderer at a fixed 48 kHz, so the same graph, effects and meters apply with and without a device.
  - The sample rate is pinned in tests, because reverb, delay and filters depend on it.
  - With a device, the graph runs at the device's rate. Kira re-initialises the effects on a rate change.
- **Luau API** (in `re_script`, reviewed by WP24; `NoServices` refuses it with `RE3015`):
  - `audio.play(clip, position?, volume?, bus?)`, which gains `bus`;
  - `audio.set_bus_volume(bus, db, seconds?)`;
  - `audio.set_effect(bus, index, { param = value, … }, seconds?)`;
  - `audio.snapshot(name, seconds?)`;
  - `audio.levels(bus) -> { peak_db, rms_db }`.
  - The `AudioSource` component in `re_scene` gains `bus: String`, empty by default, which means `default_bus`. The maintainer reviews this.
- **Compat.** Roblox-style `SoundGroup` and `SoundEffect` (WP27) map onto these buses and effects later, through the same commands.

**Measured** (4 vCPU Xeon @ 2.80 GHz, release, 48 kHz, 512-frame render calls, CPU time per second of audio, best of 5).

| Graph | CPU ms per audio s |
|---|---|
| one sound on a sub-track, no effects | 1.42 |
| reverb (on the track / on a send) | 4.66 / 4.70 |
| filter, low-pass / high-pass | 2.65 / 2.84 |
| delay, 250 ms | 2.93 |
| compressor | 6.50 |
| EQ bell | 4.54 |
| two buses and a reverb send, no effects | 2.58 |
| two buses with filter, delay and compressor, and a reverb send | 13.49 (74× realtime) |

**Determinism of the output.**
- With fixed parameters, the output was bit-identical between two runs, between two processes, and across render-call sizes of 128, 480, 512, 800 and 480,000 frames.
- Budget: the default graph plus a typical project graph of 4 buses and 1 reverb send stays within 2% of one core (20 ms per audio second). A bench in `re_audio` reports it.

**Acceptance tests.**
- **Reference buffers.**
  - Effect graph tests render 0.2 s of a fixed stimulus headless (a sine plus a click, at 48 kHz, stereo). There is one case per effect kind, one with a send, one with nested buses and one with a snapshot tween.
  - Each output is compared with a reference buffer committed under `crates/re_audio/tests/reference/`: raw little-endian f32, about 77 KB each, under 1 MB in all. `RE_UPDATE_AUDIO_REFERENCE=1` rewrites them, and a reviewer checks the rewrite.
  - The comparison allows a **maximum absolute difference of 1e-4 per sample**. This absorbs last-bit differences from the platform maths library on Windows and macOS. Bit equality is checked only against a second render on the same machine.
- **Level tests.**
  - A bus at −6 dB measures −6 ± 0.1 dB RMS against the same sound on the main track.
  - A low-pass at 500 Hz attenuates a 4 kHz sine by at least 18 dB.
  - A send at −9 dB adds the reverb's tail.
  - A snapshot's tween reaches its target within one render batch after `seconds`.
  - An invalid graph falls back to the default graph with `RE7407`.

**Alternatives.**
- **Our own DSP** (biquads, Schroeder reverb) as `re_audio` code. It is more work and duplicates kira, and kira's effects already fit the list. It is still the route for any effect kira lacks (sidechain, chorus, convolution), as a custom `Effect`.
- **fundsp or another DSP crate.** It would be a new dependency for effects kira already has.
- **Rebuilding a bus to change its effect list at runtime.** It is allowed only on a `real.toml` reload. Scripts change parameters and snapshots, not structure, so a playing sound is never cut.
- **HRTF or convolution spatial audio.** It is a non-goal of the plan.

**Consequences.**
- No new dependency and no DEPENDENCIES.md row. Kira's effects are modules of the pinned crate.
- The MPL-2.0 `triple_buffer`, already listed in ADR-0019 and in open question 1, is unchanged.
- `re_audio` gains `RE7406`, `RE7407` and `RE7408` with explain pages.
- The bus graph is fixed per load: a script can change volumes and parameters but not add a bus.
- There is no sidechain ducking. A snapshot or a volume tween does ducking instead.
- Delay time cannot be tweened.
- Audio output is not part of `state_hash` or D2. Cross-platform checks use the tolerance above.

## ADR-0043: Procedural workflow: generator versions, bake, structural overrides, Luau generators, polygon booleans and roads

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP24). The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). Evidence: two probes in `research/m3/wp24/` (sources and output): `luau-node-bench` (mlua 0.12.2, Luau 0.740, the workspace's pins) and `overlay-probe` (i_overlay 9.1.0 and earcut 0.4.11). Measured on one core of the reference container (Intel Xeon @ 2.80 GHz).

**Context.** The M3 plan (WP24) asks for five things on top of ADR-0015 and ADR-0017: a version per generator and node kind in the cache key with an explicit `real migrate-graph`; bake to authored entities as one undoable transaction with a record of where they came from; structural overrides (move, replace, insert) kept across re-runs; generators and ProcGraph nodes written in Luau in a `generator` context with a CPU budget and no world access; and polygon booleans with a road node, re-checking `i_overlay`. Facts found in the code at `69ed909`:

- **Node versions are recorded but not honoured.** `NodeDoc.version` is in every graph file, and `ProcGraph::validate` accepts any recorded version from 1 up to the kind's current one (`graph.rs`). But the cache key hashes `def.version`, the engine's current version, not the recorded one (`eval.rs`, `node_key`), and evaluation always runs the current code. The day a kind goes to version 2, every old graph silently gets version 2's output. All 16 kinds are at version 1, so nothing has diverged yet.
- **Builder scripts always record version 1** (`re_script/src/api/proc.rs`), and a builder has no way to ask for another.
- **Generators have no version.** `Procedural` is `{ generator, seed }`; `GeneratorInfo` has no version; the fingerprint covers the generator name, seed, parameter component and the graph document's hash.
- **Overrides** are field values and deletions only. `re_command` refuses `reparent`, `rename`, `add_component` and `remove_component` on a generated entity (`op.rs`), and `SetOverride` is the one op that edits the list, with an `index` so its inverse restores the list exactly.
- **A source that loses `Procedural` keeps its output.** `World::regenerate_changed` visits only entities that carry `Procedural` (`world.rs`), so the generated entities of a source whose `Procedural` was removed stay in the world but are never saved.
- **Derived assets exist only in memory.** Meshes and heightfields made by nodes are named `derived:<hex>` and live in the `GraphStore` and the disk cache. The asset server loads only `.gltf`, `.glb`, `.png`, `.jpg`/`.jpeg` and `.material.json`, and heightfield colliders resolve only from the store (`re_app/src/procedural.rs`). An entity that names a `derived:` asset cannot outlive its generator.
- **Luau facts** (mlua 0.12.2, Luau 0.740):
  - VMs are not `Send` (the `send` feature is off, ADR-0011), and the evaluator runs nodes on the `TaskPool`.
  - The CPU budget is a wall-clock deadline checked every 1,024 safepoints (`budget.rs`). Wall-clock limits are not deterministic: the same node could pass on a fast machine and fail on a slow one.
  - Luau numbers are f64; graph values are f32. `vector` components are f32.
  - Under `sandbox(true)` the environment is "safe", and `FASTCALL` then runs the built-in `math.sin` and friends directly, even if the `math` table entry was replaced (`lvmexecute.cpp`). Replacing a maths function therefore only works if the compiler is told not to treat it as a builtin; mlua's `Compiler::set_disabled_builtins` passes Luau's `disabledBuiltins` option for that.
  - `^` calls the C library's `pow`, except for the constant exponents 2, 0.5 and 3, which the VM computes exactly (`LOP_POWK`).
- **i_overlay re-check.** M1 found it too new. On 2026-10-15:
  - 9.1.0 (2026-10-14) is the newest release, MIT OR Apache-2.0, MSRV 1.88, about 9.8 million downloads; geo 0.33.1 uses it (pinned to 4.5) for its booleans.
  - Without features it pulls `i_float` 5.0.0, `i_shape` 5.0.0, `i_key_sort` 0.12.0 and `i_tree` 0.20.0 (all MIT, same author) and `libm` 0.2, which is already in our lockfile at 0.2.16. The `allow_multithreading` feature would add rayon; `glam` is optional.
  - The core is integer arithmetic. The float API maps coordinates onto a grid (adaptive, or fixed with `*_fixed_scale`), runs the integer solver and maps back. Strokes and outlines also use integer construction by default (`MathMode::Integer`), which its README recommends "for cross-platform deterministic construction". The only trigonometry outside tests is its own integer `Angle`.
  - It has been churning: majors 5.0.0 (April 2026), 6 (May), 7 (June), 8 (August) and 9 (September).
  - It contains `unsafe` code (84 occurrences across `i_overlay` and `i_float`).
- **No triangulator in the workspace.** `re_geom` makes meshes from primitives and heightfields only. earcut 0.4.11 (2026-07-26, MIT OR Apache-2.0, about 1.1 million recent downloads) depends only on `num-traits`, already in the lockfile at 0.2.19. Its arithmetic is f64 with `sqrt` only in its 3D helper, and it uses `unsafe` for node access.

**Measured** (`research/m3/wp24/`, best of 7 runs, one core):

| Work | Luau | Rust | Notes |
|---|---|---|---|
| New VM: `ALL_SAFE` libraries, sandbox, memory limit, raw interrupt | 112 µs | | per evaluation, if each gets a fresh VM |
| Element-wise map of a 256×256 field, through `buffer` | 4.2 ms (64 ns/sample) | 0.23 ms | 19× slower; output bit-identical to a Rust version computing in f64 |
| 5-tap blur of a 256×256 field, through `buffer` | 10.3 ms (157 ns/sample) | 0.18 ms | 58× slower; bit-identical as above |
| The same map, through Luau tables (marshalling included) | 15.2 ms (232 ns/sample) | | 3.6× the `buffer` path |
| Rotate 10,000 points, through `buffer` | 1.4 ms (140 ns/point) | | |
| Build 2,000 entity tables in Luau and read them back in Rust | 3.0 ms (1.5 µs/entity) | | a generator's fragment |
| Raw counting interrupt on vs off | +1 to +5% | | |
| Safepoints of the blur, three runs | 65,794 each time | | the count is deterministic |
| Safepoint rate | 6.1–6.4 million/s | | calibrates the budget below |
| i_overlay: stroke two 41-point roads 8 m wide, then union and intersect | | 0.31–0.35 ms | 1 mm grid, integer stroke |
| i_overlay: union of 5,000 random squares over 2 km | | 9.5–10.9 ms | 1 mm grid; 4,346 shapes |
| earcut: triangulate the probe's road union (240 points) | | 238 triangles | area matches the polygon |

A Rust version computing in f32 is not bit-identical to the Luau one (Luau rounds only once, at the end); the Rust f32 times were 0.19 ms (map) and 0.11 ms (blur). Two runs of the overlay probe gave identical output hashes. A 16 km square at a 1 mm grid works; a 4,000 km triangle returns `ScaleTooLarge` rather than wrong output.

**Decision.**

*1. Node and generator versions (PR "versioning"; frozen contract).*
- **A node evaluates at its recorded version.** `NodeCtx::version()` gives the recorded version to the node's function, and the cache key hashes the recorded version instead of `def.version`. Rust kinds keep old behaviour behind `ctx.version()` (or by keeping the old function); there is still one `NodeDef` per kind.
- **`NodeSpec`** gains `oldest: u32`, the oldest version the engine still evaluates (1 for every M3 kind). A recorded version of 0 or above `version` stays `RE4012`. A recorded version below `oldest` is a new error, `RE4018` ("retired node version"), whose hint names the last engine release that evaluates it. A version is retired only in a release whose notes say so; none is retired in M3.
- **Parameter shapes migrate in memory.** A `NodeDef` lists `Migration { from, note, changes_output, params: fn(&mut Map) -> Result<(), String> }` steps, one per version step. Before checking and evaluating, the evaluator runs the steps from the recorded version up to the current shape, so `check` and `eval` see one shape. The key hashes the migrated parameters and the recorded version. A failed step is `RE4020`.
- **The rule that makes this real.** Any change that alters a kind's output for some valid input bumps its version and keeps the old output for the old version. `re_proc/tests/versions.rs` keeps one golden output hash per (kind, version) on fixed inputs. A changed golden fails CI, and goldens are only ever added. A shared change (in `re_noise`, `re_geom` or a dependency upgrade such as i_overlay) that moves a golden therefore needs version bumps for the kinds it moves.
- **Keys stay the same today.** For a graph whose nodes are all at version 1, the key bytes are unchanged (`def.version` and the recorded version are both 1), so existing disk caches stay warm and "a versioned graph from before the PR loads unchanged" holds by construction. A test pins a key computed on `main` before the PR.
- **Builders.** `g:node(kind, ...)` keeps recording version 1. `g:node("terrain.heightfield@2", ...)` asks for a version. Absent means 1, never "the current one", because a builder runs on every load and must keep meaning the same thing. Graph files and the `graph.*` commands write the current version for new nodes. `real check` warns with `RE4019` when a graph or builder uses a version older than the current one, naming `real migrate-graph`.
- **Generator versions** (maintainer-reviewed change in `re_scene`):
  - `GeneratorInfo` gains `version: u32` (current) and `oldest: u32`.
  - `Procedural` gains `version: i64 = 1`. Files written before M3 have no field and mean 1, which is what they were generated with.
  - `GenContext::version()` returns the recorded version, and the fingerprint includes it.
  - The `graph` generator is version 1. That version covers how it assembles the fragment: keys `<node id>/<local key>`, the sink order, and `<node>/mesh` entities for mesh sinks.
- **Version stamp.** `re_proc::version_stamp(graph, registry) -> blake3::Hash` hashes the generator name and version and, for each node in document order, its id, kind, recorded version and implementation hash. The implementation hash is all zeros for Rust kinds (their goldens guard them) and the hash of the generator scripts for Luau kinds (section 4). `EvalReport` carries it as `stamp`, and the `graph` generator's `fingerprint_extra` folds it in, so editing a Luau node's script regenerates the sources that use it. WP23's `StreamCell` keys cells by it.
- **Frozen at "versioning":** `NodeSpec.version`/`oldest`, `Migration`, `NodeCtx::version()`, the key's inputs, `version_stamp`, `Procedural.version`/`GeneratorInfo.version`, and the bake format (section 2). WP23 registers its erosion node against this `NodeSpec`.
- **Room for streaming (WP23, ADR-0042).** "versioning" also carries, so `NodeSpec` freezes once: `NodeSpec.streaming` (pointwise, region-tiled with an apron, cell-local, or not streamable; default not streamable) and an optional `Domain { origin: [i64; 2], n, spacing }` on `NodeCtx` that, when set, is part of the node key next to the version. WP23 fills in the semantics and the per-kind values for the existing kinds in its own PRs.

*`real migrate-graph`.*
- `real migrate-graph [--project DIR] [--dry-run] [--json]` upgrades every node of every graph file the project uses (and every `*.graph.json` under `graphs/`) to its kind's current version, through the migration steps. It writes files atomically in ADR-0016's stable form, and raises sources' `Procedural.version` when their generator has migrations (none in M3).
- It evaluates each source before and after and reports, per node, `{ id, kind, from, to, notes, params_before, params_after, output_changed }`. Per source it gives `{ fragment_hash_before, fragment_hash_after, keys_added, keys_removed, overrides_orphaned }`, so the report says what changes in the world, including overrides that would lose their target.
- Builder scripts are not rewritten. The report lists `{ script, node, kind, from, to }` and the `@N` to write.
- It is all or nothing: any failed step (`RE4020`) writes nothing and exits non-zero.
- The logic is `re_proc::migrate` (WP24). The subcommand is a small `re_cli` PR the maintainer reviews. A read-only command `graph.migration_report` returns the same report to agents and the editor.

*2. Bake (PR "bake"; its format is frozen at "versioning").*
- **`proc.bake { source, keys?, keep_source? }`** is an edit command in `re_proc`. It makes one transaction of existing ops, so undo is the ops' exact inverses:
  1. A `Spawn` per output entity, in output order (parents first). The entity's current state is copied, overrides included. Its id is `<source id>.<key with "/" replaced by ".">` (authored ids may not contain `/`), with `~2`, `~3` and so on after a clash. It keeps its name and its parent: a mapped baked parent, or the source itself. Entity-reference fields that point at other generated entities of the source are remapped to their baked ids.
  2. Each spawned entity gets a **`Baked`** component (defined in `re_proc`, registered by `re_app`). Its fields are `source` (the source's id), `key`, `generator`, `generator_version`, `stamp` (the version stamp, hex), `seed` and `inputs` (hex of the fingerprint inputs: parameters and graph document). That is the provenance record. It is saved like any component, so `describe`, queries and agents can find everything baked from a source.
  3. **Full bake** (the default): `SetOverride` removals for every override (their inverses restore the list by index), then `RemoveComponent` of `GraphSource` and `Procedural`. The source stays in place as an ordinary authored entity, keeping its transform and authored children, with the baked entities under it.
  4. **Partial bake** (`keys` given, or `keep_source: true`): deletion overrides for the baked keys instead, so the source keeps generating everything else.
- **One `re_scene` rule (maintainer-reviewed):** when a source loses `Procedural`, the next regeneration despawns its generated output and drops its `GeneratedFingerprint`. Undoing the bake re-adds `Procedural`, the missing fingerprint forces a regeneration, and D1 gives the same output with the same ids. This rule also fixes the stray-output case in the context.
- **Derived assets become files.** They are written before the ops are applied, content-addressed under `assets/baked/<source id>/`:
  - Meshes become `<16 hex>.glb`: glTF binary with POSITION, NORMAL, TEXCOORD_0, COLOR_0 and u32 indices, so they load through the existing importer and open in other tools. The writer is about 150 lines over serde_json in `re_proc`, with no new dependency. A test round-trips every vertex field bit for bit through `re_assets::import_gltf`.
  - Heightfields become `<16 hex>.heightfield`: the magic `REHF`, a u32 format version 1, a u32 `n`, an f32 `cell`, then `n²` f32 little-endian samples in Jolt's layout. `re_app` resolves a `Collider.source` path ending in `.heightfield` (a small shared PR).
  - Undo does not delete these files. They are content-addressed and harmless, and `real check` reports unreferenced ones under `assets/baked/` as a warning.
- Bake is refused during play (`RE4021`). Acceptance: bake, then undo, gives a byte-identical saved scene and the same `state_hash` (generated entities included) as before the bake.

*3. Structural overrides (PR "overrides").*
- **Moving in space is already a field override** (`Transform`), and stays one. The new overrides change structure. They are stored with the others, in the source's `overrides` list:
  - `{"target": "trees/12", "moved": {"parent": "rocks/3", "index": 0}}`: re-parents a generated entity under another entity of the same source's output (`null` means the source itself).
  - `{"target": "trees/12", "replaced": {"prefab": "prefabs/rock.prefab.json"}}` or `{"target": ..., "replaced": {"components": {...}}}`: keeps the key and id (so references and later field overrides still find it) and the generated `Transform` unless the replacement sets one. The rest of its components come from the replacement. Its generated descendants are not spawned unless `"keep_children": true`. Prefab children get keys `<target>/~<prefab entity id>`.
  - `{"insert": "lamp1", "parent": "road/7", "index": 2, "name": "Lamp", "components": {...}}` (or `"prefab": ...`): adds an authored entity to the output with key `+lamp1` and id `<source>/+lamp1`.
- **Where they apply.** A pure function `apply_structure(fragment, overrides)` runs in `re_scene`'s `regenerate` after the generator returns and before deletions and field overrides. Inserts are added, replacements applied, moves re-parented, and the result put back into parents-before-children order. Deletion and field overrides then apply by key as today, to inserted and replaced entities too.
- **Failures.** A move that would make a cycle, or whose new parent is missing, leaves the entity at its generated parent with warning `RE1209`. Overrides whose target or parent key is gone are orphans, kept and reported (`RE4010`), never dropped. Generators may not emit keys starting with `+` or `~` (`RE1207`).
- **Ops.** `re_command`'s `SetOverride` gains `moved`, `replaced` and `insert` (one structural kind per op), with the same index-exact inverse. `re_proc` adds `proc.move`, `proc.replace`, `proc.insert` and `proc.clear_structure`, which emit these ops and regenerate in the same transaction. `reparent` and `spawn` under a generated entity stay refused, with a hint naming `proc.move` and `proc.insert`. WP25's canvas and outliner map a hierarchy drag of a generated entity to `proc.move`.
- This amends ADR-0017's "structural edits ... are refused". `apply_structure`, the `OverrideDoc` variants and the `SetOverride` fields live in maintainer crates. WP24 writes them in maintainer-reviewed PRs, as WP23 does for its `re_scene` hooks.

*4. Luau generators and Luau nodes (PR "luau").*
- **One mechanism.** A Luau node is a ProcGraph node kind whose `eval` is Luau. A "Luau generator" is a Luau node whose output is a fragment, run by a source through a one-node graph, so caching, versions, parameters (`graph.set_param`), overrides and bake work unchanged. There is no second generator kind. `proc.create_source { kind, params }` writes such a source.
- **Declaration.** Each file under `scripts/generators/` returns `proc.node{ kind, version, oldest?, doc, inputs, output, params, uses_seed, migrate?, eval }`.
  - `kind` must start with `project.`, so a future engine kind can never collide with a project's. A collision or a malformed declaration is `RE4100`.
  - `params` uses a small schema DSL (`proc.param.number{default, min, max}`, `integer`, `bool`, `string`, `enum`, `vec3`) that becomes the kind's JSON Schema for `describe`.
  - `migrate(from, params)` is the Luau form of the migration steps, and `ctx.version` is the recorded version.
  - `re_app` declares the scripts at project load and on hot reload, and registers them in the `GraphStore`'s `NodeRegistry` (a small shared PR).
- **The `generator` context** (new module `re_script::generator`):
  - It has the safe standard libraries without `os`, and `math.random` is removed.
  - `ctx` gives the node's seed and a deterministic generator over it (re_noise's `Pcg32`), the recorded version, the field size and cell, and `ctx:log(text)`, which goes into the `EvalReport` (capped).
  - `require` reaches only modules under `scripts/generators/`.
  - There is no `world`, entity, command, task, time or file access. Output depends only on parameters, inputs, seed and the scripts.
- **A fresh VM for every evaluation**, created on the worker thread that runs the node from bytecode compiled once (112 µs measured). No state survives between evaluations, so a script cannot make output depend on evaluation order or thread. Nothing but bytecode (`Arc<[u8]>`) crosses threads.
- **A deterministic budget.** The interrupt counts safepoints. The default is 20,000,000 per evaluation, about 3 s at the measured rate, set in `real.toml` as `[proc] luau_budget`. Going over is `RE4101`, the same on every machine. The memory limit is `[proc] luau_memory_mb` (default 256). A wall-clock backstop of 60 s reports `RE4102`, marked as machine-dependent, for a VM stuck in a long builtin. A `pcall` cannot swallow either; the probe confirms this for the counting interrupt.
- **Data at the boundary.**
  - Fields cross as `buffer`s of f32 (`samples[z*n+x]`); points as a `buffer` of records plus a tags array; meshes as `buffer`s; fragments as tables.
  - Buffers are 3.6× faster than tables on the measured map.
  - Outputs are checked: non-finite numbers, wrong lengths, and bad keys or component names are `RE4103`, naming the node and script.
  - Luau computes in f64. A Rust twin of a Luau node therefore computes in f64 and rounds at the output; the plan's acceptance test ("a Luau node and the same node in Rust give identical output") uses twins written that way, which the probe shows are bit-identical.
- **Maths across machines (D2).**
  - Generator scripts compile with `disabled_builtins` for the transcendental `math` functions (`sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `exp`, `log`, `log10`, `pow`, `sinh`, `cosh`, `tanh`). The context's `math` table supplies portable versions of them: the ones ADR-0036 picks for simulated paths, or `re_geom::math` until it lands.
  - `^` with an exponent other than the constants 2, 0.5 and 3 still calls the C `pow`. That is the one known D2 gap. The static scan should warn on it in generator scripts; see the open questions.
- **Cost guidance** (documented, from the measurements): Luau nodes suit points, fragments and fields up to about 512² (17 to 40 ms for a map or a stencil). Large terrain stays in Rust nodes. The "luau" PR measures again through the real context before merging.
- **Dependency edge.** `re_script` gains a dependency on `re_proc` (for `NodeDef`, `NodeCtx` and the value types). `re_proc` still reaches no mlua. ARCHITECTURE §2 and `check-graph` get the new edge (maintainer).

*5. Polygon booleans and roads (PR "booleans and roads").*
- **Dependencies:** `i_overlay = "=9.1.0"` with default features off, and `earcut = "=0.4.11"`, both used by `re_geom` only, and nowhere in its public types. A new `re_geom::polygon` module wraps them:
  - `Shapes` (polygons with holes, f64 metres in the source's XZ plane) with `boolean(a, b, op)`, `union_all`, `offset`, `stroke(path, style)` and `triangulate(&Shapes, height) -> MeshData`.
  - Every call uses i_overlay's fixed-scale API at a 1 mm grid (scale 1000) on its i32 engine, with integer stroke and outline construction. Results are therefore exact integers mapped back to floats, and the same on every platform.
  - Coordinates beyond about ±1,000 km are refused (`ScaleTooLarge`), which becomes `RE4503`. Invalid input is `RE4504`, and a triangulation failure `RE4505`.
- **Churn is contained.** The version is pinned exactly and wrapped in one module. `re_geom`'s goldens hash the outputs, so an upgrade that changes any output is seen, and the node kinds it changes get a version bump under section 1.
- **Roads** (`re_geom::road`, all in f64 with polynomial trigonometry and no fused multiply-add, following `re_geom`'s rules):
  - The centreline is a centripetal Catmull-Rom spline through the control points, sampled at a fixed arc spacing.
  - The surface is a ribbon with width and bank per control point (interpolated). Its UVs run across the road (u, 0 to 1) and along it (v, in metres).
  - Junctions: where centrelines cross, or end within `snap` of each other, each road is cut back by `junction_radius` (a polygon difference with the junction disc). The junction surface is the union of the road ends' footprints within the radius, triangulated flat at the mean height of those ends. Bank fades to 0 over `bank_fade` before a junction.
- **New port types and nodes (all version 1):**
  - Port types: `shapes` and `path`. They are added after the existing variants, so stored cache entries stay readable and the cache format version does not change.
  - Nodes: `path.spline` → path; `road.mesh` (variadic paths) → mesh; `road.footprint` → shapes; `shape.polygon`, `shape.boolean`, `shape.offset` → shapes; `shape.mesh` → mesh; `field.from_shapes` (a mask with falloff) → field; `points.mask` (keep inside or outside) → points; `terrain.conform` (field + paths → the field flattened under the roads with a blend width).
  - `terrain.conform` works on fields, so it lives in `re_proc`; WP23's terrain module consumes the field. The valley's roads (WP28) use these nodes.

*6. Diagnostics* (WP24's range `RE4000`–`RE4299`): `RE4018` retired node version, `RE4019` older node version in use (warning), `RE4020` migration step failed, `RE4021` bake refused or failed, `RE4100`–`RE4103` the generator context (declaration, budget, wall-clock backstop, invalid output). In `re_geom`'s range: `RE4503`–`RE4505` (out of range, invalid polygon or path, triangulation). `re_scene` keeps `RE1207` and `RE1209` for the generator and override failures it reports.

*7. Changes in other owners' paths*, each a small PR the owner reviews:
- `re_scene`:
  - `Procedural.version`, `GeneratorInfo.version`/`oldest` and `GenContext::version()` ("versioning");
  - the losing-`Procedural` rule ("bake");
  - the structural `OverrideDoc` variants, `apply_structure` and the reserved key prefixes ("overrides").
- `re_command`: `SetOverride`'s structural fields and the new refusal hints ("overrides").
- `re_app`: registering `Baked`, resolving `.heightfield` colliders, and declaring and hot-reloading Luau nodes.
- `re_cli`: `real migrate-graph`.
- `tools/xtask` (`check-graph`): the `re_script` → `re_proc` edge.
- Docs: ARCHITECTURE §2, §5 and §7; DEPENDENCIES rows for i_overlay, its four `i_*` crates and earcut; ADR-0015's "Luau never runs inside node evaluation in M1" and ADR-0017's "not in M1" list, which point here.
- Schemas are regenerated (`cargo xtask schemas`).

*8. Order.* The five PRs land in the plan's order. "versioning" carries everything frozen, including the `Baked` type, the id rule, the baked-asset paths and the `.heightfield` layout, so WP23 and WP25 can build on it before "bake" merges. WP23 waits only for "versioning".

**Alternatives.**
- **One `NodeDef` per (kind, version).** This is cleaner in principle, but it duplicates ports, schemas and docs per version, and most bumps change one branch. Rejected for one def with `ctx.version()`, with goldens as the guard.
- **Migrate on load (always run the newest version).** This contradicts the plan ("an old project opens with its recorded versions") and moves people's worlds without asking.
- **Builders default to the current version.** A builder runs on every load, so its output would shift on engine upgrades with nothing recorded. Rejected for "absent means 1" plus the `RE4019` warning.
- **Bake by despawning the source and re-spawning it.** Every reference to the source and its authored children would be cleared by `despawn` and need rebuilding, and the source's `Entity` would change. Rejected for removing two components plus one `re_scene` rule.
- **A new `Bake` op in `re_command`'s closed set.** This means more maintainer surface for what existing ops already express exactly.
- **Bake keeps `derived:` names** (or a private bake pack). The baked content could not be edited elsewhere and would depend on WP24's store at run time. Rejected for `.glb` files plus one small heightfield format.
- **Structural overrides inside the `graph` generator,** stored in a component of `re_proc`'s own. That would keep the work in WP24's paths, but it splits overrides into two lists and leaves other generators (prefab instancing, tests, compat) without them. Rejected for one list applied in `re_scene`.
- **A VM per worker thread, reused.** It saves about 112 µs per evaluation, but module-level state would then leak between evaluations and make output depend on scheduling.
- **A wall-clock budget for generators.** It is not deterministic; it stays only as the backstop.
- **geo 0.33.1 for booleans.** It pins i_overlay 4.5, and adds rstar, robust, geographiclib-rs and others we do not need.
- **clipper2 0.6.0** binds the C++ Clipper2 and needs a C++ build. **clipper2-rust 1.2.0** is BSL-1.0 and young (about 125,000 downloads). Neither has i_overlay's integer-construction stroke.
- **Our own boolean solver.** Robust polygon booleans are a project of their own.
- **i_triangle 0.50.1** (same author, 0.x, about 87,000 downloads) instead of earcut, or our own ear clipper. earcut is the widely used port of a well-known algorithm, and junction caps and fills do not need Delaunay quality.

**Consequences.**
- Version bumps become routine and safe: an engine upgrade never changes a world until `real migrate-graph` is run, and the report says what will change.
- Goldens per (kind, version) grow with every bump. They are small (one hash per entry).
- Bake output is ordinary content: scenes, `.glb` files and `.heightfield` files. It survives the engine dropping a node version.
- Structural overrides give users and agents "change one generated thing" without leaving the generator, and keep working across re-runs. Orphans stay visible.
- Luau nodes are 20–60× slower than Rust per sample on field work. They are for project-specific logic, not bulk terrain.
- Two new direct dependencies (five crates in all, plus `libm` and `num-traits`, which are already present), all MIT or Apache-2.0, used only by `re_geom`. Both contain `unsafe`; our crates keep `forbid(unsafe_code)`.

**Answers to the draft's open questions** (maintainer):
1. `^` with a non-constant exponent in generator scripts is a warning from the static scan in M3. The maintainer, who owns `re_script_check` in M3, revisits it as a load-time error only if ADR-0036 makes D2 binding for generator output.
2. Bake's undo does not delete the asset files it wrote; `real check` reports unreferenced files under `assets/baked/`.
3. Luau node kinds start with `project.`; a shared library of nodes uses `project.<library>.<name>`.

## ADR-0046: The M3 AI track: templates and recipes as data, an evaluation harness that never calls a model, and MCP over streamable HTTP on localhost

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP26). The code waits for M2's definition of done (M3_PLAN, "How M3 starts").

**Context.** ROADMAP's AI track gives M3 three items: natural-language starting points for beginners (project templates and generator recipes that an agent fills in through ordinary commands), an evaluation harness that runs scripted agent tasks against sample projects and scores them with `real verify`, and the HTTP MCP transport. ADR-0020 (tiers, policy, no shell tier), ADR-0025 (rmcp over stdio, schema hygiene, client configs) and ADR-0026 (one writer, error shape, protocol revisions) stay in force; this ADR adds to them and supersedes nothing.

What was checked for this ADR (2026-10-15, on `main` at 69ed909):
- **rmcp 3.5.1** (the version the workspace pins, Apache-2.0) has the feature `transport-streamable-http-server` = `transport-streamable-http-server-session` + `server-side-http` + `transport-worker`. It provides `StreamableHttpService`, a `tower_service::Service<http::Request<B>>`, configured by `StreamableHttpServerConfig`. It does not open a socket: an HTTP server crate must drive it.
- That config validates the `Host` header against `allowed_hosts`, which defaults to `localhost`, `127.0.0.1` and `::1` (DNS-rebinding protection), and validates `Origin` once `enforce_origin_validation()` is called (an empty list then refuses every request that carries an `Origin`). It caps POST bodies at 4 MiB by default (413 above that). `legacy_session_mode` (default on) keeps `Mcp-Session-Id` sessions for revisions before 2026-07-28; 2026-07-28 requests are always served statelessly.
- A prototype (scratch only, not committed) put `RealEngineServer` behind `StreamableHttpService`, `LocalSessionManager`, hyper 1.12 `http1` and hyper-util 0.1.21 `TowerToHyperService`, with a bearer-token check in front. Driven with curl:
  - no token: 401; wrong `Host`: 403; `Origin: http://evil.example`: 403;
  - 2025-11-25: `initialize` returned a session id, `notifications/initialized` 202, `tools/list` and `tools/call` answered as SSE;
  - 2026-07-28: `server/discover` and `tools/call` answered as plain `application/json`, with no session. rmcp requires the `Mcp-Method` header on every request and `Mcp-Name` on `tools/call` for this revision (it answers -32020 otherwise), so tests must send them;
  - the policy applied unchanged: `entity_spawn` under the default read-only policy returned the `RE8001` `isError` result, as over stdio.
- **New crates.** Enabling the feature and adding the two server crates locks 15 new packages, all inside `deny.toml`'s license allow-list:

  | Crate | Version | License | Brought by |
  |---|---|---|---|
  | hyper | 1.12.0 | MIT | us (`http1`, `server`) |
  | hyper-util | 0.1.21 | MIT | us (`tokio`, `server`, `http1`, `service`) |
  | http | 1.5.0 | MIT OR Apache-2.0 | rmcp, hyper |
  | http-body | 1.1.0 | MIT | rmcp, hyper |
  | http-body-util | 0.1.5 | MIT | rmcp |
  | httparse | 1.10.1 | MIT OR Apache-2.0 | hyper |
  | httpdate | 1.0.3 | MIT OR Apache-2.0 | hyper |
  | tower-service | 0.3.3 | MIT | rmcp, hyper-util |
  | sse-stream | 0.2.6 | MIT OR Apache-2.0 | rmcp |
  | tokio-stream | 0.1.19 | MIT | rmcp |
  | async-trait | 0.1.92 | MIT OR Apache-2.0 | rmcp |
  | rand | 0.10.3 | MIT OR Apache-2.0 | rmcp (`server-side-http`) |
  | rand_core | 0.10.1 | MIT OR Apache-2.0 | rand |
  | chacha20 | 0.10.2 | MIT OR Apache-2.0 | rand |
  | socket2 | 0.6.5 | MIT OR Apache-2.0 | tokio `net` |

  With them, `cargo deny --locked check` (0.20.2) prints "advisories ok, bans ok, licenses ok, sources ok" with the same two duplicate warnings as `main`; `cargo xtask dupes` is ok on all three targets (no new duplicate versions), and `cargo xtask check-graph` is ok. Every crate that declares a `rust-version` declares 1.85 or lower, under our 1.95.
- axum 0.8.9 would also drive the service, but it adds a router, `tower` with its layers, `matchit`, `serde_urlencoded` and more for one route. hyper with hyper-util's `TowerToHyperService` is the smallest server rmcp's service runs on.
- Claude Code's documentation (code.claude.com/docs/en/mcp) gives `.mcp.json` entries `{ "type": "http", "url", "headers" }`, with `${VAR}` and `${VAR:-default}` expanded in `url` and `headers`, and `claude mcp add --transport http NAME URL --header "Authorization: Bearer ..."`. Other clients' HTTP formats are taken from their documentation and not run, as ADR-0025 does for stdio.
- `real new --template` already exists, with a closed enum `native|roblox` in `re_cli/src/new.rs`; the native scene and materials are Rust string constants.

**Decision.**

*1. Templates and recipes are data.*
- **Templates** live in `templates/<name>/`: a `template.toml` and the files to copy. `template.toml` has `name`, `summary`, `params` (each with `name`, `type` of `string|int|float|bool|color`, `default`, `doc` and, for numbers, `min`/`max`), `recipes` (recipe applications run after the copy, with arguments) and `checks` (below). Text files are copied with `{{param}}` replaced; nothing else is evaluated, so a template cannot run code. Scenes are stored in canonical form, and a test loads and saves each one to prove it.
- The existing `native` and `roblox` templates move into `templates/` with byte-identical output (a test compares them with the current `real new`). `real new DIR --template NAME [--set k=v ...]` keeps its current flags; `--template` now takes any built-in name or a path to a template directory. `real new --list-templates` prints names, summaries and parameters, with `--json`.
- **Recipes** live in `templates/recipes/<name>.toml`: `name`, `summary`, `params` (as above), `steps` and `checks`. A step is one registered command with arguments: `{ command = "graph.set_param", args = { ... } }`. `{{param}}` in a string is replaced; a string that is exactly `"{{param}}"` takes the parameter's typed value. Steps can only name registered commands, so a recipe can do nothing an agent or `real do` could not, and the policy checks each step at its own tier.
- **Checks** are shared by templates, recipes and evaluation tasks, and are read-only:
  - `{ verify = "ok" }`: `real verify` passes with every stage ok; `{ verify_stage = "NAME" }`: that stage passes;
  - `{ command = "query", args = {...}, pointer = "/data/total", equals = 3 }` (or `at_least`, `at_most`, `contains`): a **read-tier** command's outcome at a JSON pointer. A check that names a non-read command is refused when the file loads (`RE8019`);
  - `{ diff_stage = "NAME", changed = true }` and `{ no_regressions = true }`: the `verify.diff` against the state before the steps ran.
- The data types and loaders are `re_agent::templates` (no new dependency: `toml` and `serde` are already used). The built-in files are embedded in the `real` binary by a `re_agent` build script that writes an `include_bytes!` table for `templates/`; no `include_dir` crate.
- **MCP and CLI.** Three registered commands, so they are MCP tools and `real do` commands at once:
  - `recipe.list` (read): recipes with summaries and parameter schemas; `template.list` (read): the same for templates, so an agent can tell a person which `real new` to run (creating a project writes outside the open project, so it is not a tool);
  - `recipe.apply` (edit, `dry_run`): takes a checkpoint, runs the steps in order through the same dispatch as every other command, then the checks. If a step is refused or fails, or a check fails, it restores the checkpoint and returns the step's or check's diagnostic, so it is all or nothing. A dry run returns each step's dry-run changes; steps that depend on earlier steps' results report that their preview is partial.
- Codes: `RE8019`–`RE8029` in `re_agent`'s range (invalid template or recipe, unknown parameter, value out of range, check failed, step failed).

*2. The evaluation harness never calls a model.*
- `real agent eval SUITE [--agent NAME] [--runs N] [--out DIR] [--transport http|stdio]` in `re_cli/src/tooling/agent.rs`. It is a CLI subcommand for people and CI, **not a registered command**, so no agent can start it, and ADR-0020's "no command runs arbitrary processes" still holds for everything an agent can call.
- A **task** is a TOML file: `project` (an example directory, or a template with parameters), `prompt` (the natural-language request), `budget` (tool calls, wall-clock seconds), `checks` (as above, usually including `no_regressions`) and an optional `reference` (a list of tool calls that solves the task). A suite is a directory of tasks. The built-in suites live in `crates/re_agent/evals/` and use `examples/island`, and the plaza once it is in `examples/`.
- For each task and run, the harness copies the project into a fresh directory under `--out`, writes `real agent init`'s policy there, takes the baseline verify report, and serves MCP on that copy in-process. Then one of two **drivers** acts:
  - `reference`: replays the task's `reference` calls as an MCP client over the chosen transport. It needs no model, runs in CI, and proves each task can be solved and the harness scores it.
  - `command`: runs the agent the person configured, as a child process, in the copy's directory. The configuration is a user-level or project-level `eval.toml` (`[agents.NAME] command = [...]`, optional `env`, `timeout_s`), never part of a task file. The harness gives the agent the prompt (as `{prompt}` in the arguments and as `REAL_EVAL_PROMPT`), the MCP endpoint (`REAL_MCP_URL`, `REAL_MCP_TOKEN`) and a generated client config in Claude Code's `.mcp.json` format, which is also what `{mcp_config}` names. Model, keys and provider all belong to that command; the engine does not know them.
- **Recording.** `RealEngineServer` gets an optional recorder that writes one JSON line per tool call (time, tool, arguments, outcome code, `isError`, duration). It is on for every eval run and available to people as `real mcp --record FILE`. The transcript therefore comes from the server, the same for every client. The agent's own stdout and stderr are kept as files, uninterpreted.
- **Scoring.** After the driver ends (or its budget runs out, which kills the child), the harness runs `real verify` on the copy, `verify.diff` against the baseline and the task's checks. It writes per run `transcript.jsonl`, `agent.stdout`, `agent.stderr`, `verify.json`, `verify-diff.json` and `score.json` (`passed`, each check's result, tool calls, refused calls by code, errors, wall time), plus `summary.json` and a short table for the suite. A task passes when every check passes; `--runs N` repeats each task and reports how many passed, because model agents are not deterministic.
- Nothing is uploaded and no network address other than the loopback endpoint is opened by the engine.

*3. MCP over streamable HTTP, on localhost, with a token.*
- `real mcp --http [ADDR]` serves rmcp's `StreamableHttpService` at `/mcp`, driven by hyper 1 (`http1`, `server`) and hyper-util 0.1 (`TowerToHyperService`), on the same tokio runtime and the same `RealEngineServer` as stdio. Without `--http`, `real mcp` serves stdio as today. `ADDR` defaults to `127.0.0.1:0`.
- **Loopback only in M3.** An address that is not loopback is refused (`RE8502`), because there is no TLS. A remote transport needs its own ADR.
- **Token.** Every request needs `Authorization: Bearer <token>`; otherwise 401, before rmcp sees it. The comparison is constant time. The token is read from `REAL_MCP_TOKEN` when set (at least 32 characters, else `RE8503`); otherwise it is generated from two v4 UUIDs (OS randomness through `uuid`, already a workspace dependency, 244 random bits). The URL and token are written to `.real/mcp-http.json` (mode 0600 on Unix; `.real/` is local state) and the URL to stderr; the token is never printed.
- **Browsers and rebinding.** `allowed_hosts` is the loopback names with the bound port; `enforce_origin_validation()` with no allowed origins refuses every request with an `Origin` header (403). Bodies stay capped at rmcp's 4 MiB default.
- **Same rules as stdio.** The server holds the same lock (`open_writer`), opens the same headless app or proxies to the running editor, uses the same `.real/agent-policy.toml` and tiers, and starts one agent session. Several HTTP sessions may connect; they share that one engine, policy and agent session, and calls still reach the engine one at a time through the existing `order` mutex. `legacy_session_mode` stays on for 2025-11-25, and 2026-07-28 is stateless, as rmcp serves it. The server runs until it is interrupted or its stdin closes (the harness closes it). Edits are saved by each command, as with stdio.
- `real agent init --http` adds the Claude Code entry `{ "type": "http", "url": "http://127.0.0.1:PORT/mcp", "headers": { "Authorization": "Bearer ${REAL_MCP_TOKEN}" } }` to `.mcp.json` and prints the other clients' HTTP snippets. Stdio stays the default in generated configs.
- Codes in `re_mcp`'s range: `RE8502` (address not loopback), `RE8503` (token too short), `RE8504` (cannot bind).
- **Tests.** The M1 and M2 MCP end-to-end tests (`re_cli/tests/mcp_e2e.rs`) and `re_mcp`'s wire tests run over both transports. The HTTP side uses a small blocking HTTP/1.1 client over `std::net` in the test code (POST, read JSON or SSE `data:` lines, keep `Mcp-Session-Id`, send `Mcp-Method`/`Mcp-Name` for 2026-07-28), so the tests add no client crate, and they still write the protocol exactly as a client does. Extra HTTP tests: missing or wrong token is 401, a foreign `Host` or any `Origin` is 403, a non-loopback address is `RE8502`.

*Dependencies.* rmcp gains the `transport-streamable-http-server` feature; hyper and hyper-util are new direct dependencies of `re_mcp` (a tooling crate, ADR-0027: the runtime-only binary does not link them), with `default-features = false`; tokio gains `net`. DEPENDENCIES.md gets rows for hyper and hyper-util and a note on the transitive crates in the table above.

**Alternatives.**
- **axum** for the HTTP server. Rejected: a router and tower middleware for one route and one header check.
- **No token, loopback only.** Rejected: any local process or user could drive the project, and the browser protections only stop browsers.
- **Binding beyond loopback with a token.** Deferred: without TLS the token travels in clear text.
- **Templates in code** (extending the enum). Rejected: adding one would need a Rust change, and agents could not list or read them.
- **Recipes as Luau.** Rejected for M3: a recipe would then be code that must be read to know what it does. Steps as commands are checked by the policy and dry-runnable. Luau `tools` commands (M2) remain the way to write logic, and a recipe step can call one.
- **The harness calls model APIs.** Rejected: the roadmap says the engine never calls a model, and provider keys would enter the engine.
- **An rmcp client (with reqwest) in the harness and tests.** Rejected: reqwest and its TLS stack for loopback calls the tests can write directly.

**Consequences.**
- People can start from a template, agents can list and apply recipes as ordinary undoable edits, and a suite can compare agents on the same tasks with the same scores, with transcripts as files.
- The `reference` driver keeps every built-in task solvable in CI without any model; scores for real agents are produced by whoever runs them, on their own account.
- The HTTP transport adds 15 crates to tooling builds only. Each `rmcp` upgrade must recheck the HTTP header rules, since the prototype found rmcp enforcing `Mcp-Method`/`Mcp-Name` for 2026-07-28.
- Shared files outside WP26's paths, changed through maintainer-reviewed PRs: `Cargo.toml` and `Cargo.lock` (feature and two crates), `docs/DEPENDENCIES.md`, `crates/re_cli/src/new.rs` and `main.rs` (`--template` path, `--set`, `--list-templates`), `crates/re_cli/tests/mcp_e2e.rs` (both transports), `docs/ARCHITECTURE.md` §6 and AGENTS generation for the three new commands.

## ADR-0048: Compat follow-ups: local data services, surfaces and viewports, script reload, skins and effects, and our own place-file reader

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP27). The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). Evidence: the formats and limits below were checked on 2026-10-15 against the vendor's current reference pages and the rbx-dom project's format notes (MIT); the four crates were resolved from crates.io and built and exercised together in a probe crate on the reference container (LZ4 block and ZSTD frame round trips, an XML read, a base64 decode). Nothing here is copied prose: facts are restated for this repository (ADR-0034).

**Context.** M2 left five parts of the Roblox-style layer for later (M2_PLAN "Non-goals"; M3_PLAN WP27):
- `DataStoreService` and `MemoryStoreService` raise `RE9002` on every call (`services/extra/mod.rs`, the `OFFLINE` table), so a game that saves progress cannot run its save path at all.
- `SurfaceGui` and `ViewportFrame` are `stub-error` and never drawn; the GUI only lays out `ScreenGui` and `BillboardGui` roots and paints them as egui shapes over the 3D view.
- Compat scripts do not hot reload (`re_app/src/app.rs`, before `reload_script`), and `Script`/`LocalScript` have no `Enabled`/`Disabled` yet (`classes/core.json`), so the WP11 follow-up "a script set back to `Disabled = false` restarts" is open.
- `real import` refuses `.rbxl`, `.rbxlx`, `.rbxm` and `.rbxmx` with `RE9801`. Only Rojo projects and Studio loader scripts are read.
- `MeshPart` and `Bone` have no class rows, and `BloomEffect`, `BlurEffect` and `SunRaysEffect` are `partial` ("stored; not drawn").

The engine already has what these need underneath: the scheduler's `park`/`wake` yield API (`re_script/src/scheduler.rs`), the offscreen `Capture` target and egui texture registration in `re_render`, the `notify`-based `AssetWatcher` and the native script reload path, and the import `Tree` that the Rojo and loader readers both write. WP20 (post chain) and WP21 (skins) will add the rest.

**Decision.**

**1. Data services are local, budgeted and deterministic when they must be.**
- **Storage.** Everything lives under `<project>/.real/data/`, which the project template already ignores (`.real/*`):
  - `datastore/<name>/<scope>.json`: one file per standard data store and scope, a map from key to `{ value, version, created, updated, user_ids, metadata }`;
  - `ordered/<name>/<scope>.json`: ordered data stores, integer values only;
  - `memory/{queues,sorted,hash}/<name>.json`: memory store structures, each item with its absolute expiry on the data clock;
  - `clock.json`: the data clock (below).
  Names, scopes and keys are written as percent-encoded file and JSON names, so any string the vendor accepts is a valid path on all three platforms. Writes go to a temporary file and are renamed into place, so a crash leaves the previous version. A file that does not parse is `RE9402`: the store is treated as empty for the run and the bad file is kept as `<file>.bad`, never overwritten silently.
- **Values.** A value must serialise to JSON the way the vendor's encoder accepts: nil, booleans, finite numbers, strings, arrays, and tables with string keys only; no instances, functions, userdata, mixed or sparse tables, or NaN. The serialised JSON may be at most 4,194,304 characters. Store names, scopes and keys are 1 to 50 characters; metadata keys are at most 50, a metadata value at most 250 characters and the whole metadata at most 300.
- **Methods.** `GetDataStore`, `GetGlobalDataStore`, `GetOrderedDataStore`, `GetRequestBudgetForRequestType` and `ListDataStoresAsync` on the service; `GetAsync`, `SetAsync`, `UpdateAsync`, `IncrementAsync`, `RemoveAsync` and `ListKeysAsync` on a store, returning `DataStoreKeyInfo` where the vendor does; `GetSortedAsync` with `DataStorePages` on an ordered store. The store keeps the current version only: `GetVersionAsync` and `ListVersionsAsync` see that one version and stay `partial` with that note.
- **Every `…Async` call yields** through the scheduler's `yielding_method` and resumes on a later frame (`[compat.data] latency_frames`, default 1), so code that relies on the yield behaves the same as online.
- **Request budgets** follow the vendor's current per-server formulas, with `players` the number of players in the play:

  | Request type | Budget per minute |
  |---|---|
  | standard read (`GetAsync`, version reads) | 60 + 40 × players |
  | standard write (`SetAsync`, `IncrementAsync`) | 60 + 40 × players |
  | standard remove | 60 + 40 × players |
  | ordered read | 60 + 40 × players |
  | ordered write and remove | 30 + 5 × players |
  | lists (`ListKeysAsync`, `ListDataStoresAsync`, `GetSortedAsync`) | 5 + 2 × players |
  | `UpdateAsync` | charges both the read and the write budget |

  A budget refills continuously on the data clock and starts full. A request that finds its budget empty waits in its type's queue, which holds 30 requests in arrival order; a request arriving at a full queue fails with the vendor's numeric code for that method (301 to 306). `GetRequestBudgetForRequestType` reports the live value. The per-experience limits, per-key throughput limits and storage quota apply to many servers at once and are not modelled; nor is the old same-key write cooldown, which the current reference no longer states.
- **Errors** are Luau errors whose message starts with the vendor's number and a colon, then our own wording, then our code, for example `"105: the value is 4,200,113 characters once serialised; the limit is 4,194,304 (RE9400)"`. Scripts that match on the number keep working. The numbers we raise are: 101, 102, 103, 104, 105, 106 and 107 for bad arguments (`RE9400`); 301 to 306 for full queues (`RE9401`); 403 when `[compat.data] enabled = false`, the local stand-in for Studio's "API access" switch (`RE9400`); 501 for a corrupt file (`RE9402`).
- **Memory stores.** `GetQueue`, `GetSortedMap` and `GetHashMap` return `MemoryStoreQueue` (`AddAsync`, `ReadAsync` with its visibility timeout and wait, `RemoveAsync`, `GetSizeAsync`), `MemoryStoreSortedMap` (`GetAsync`, `SetAsync`, `UpdateAsync`, `RemoveAsync`, `GetRangeAsync`, `GetSizeAsync`) and `MemoryStoreHashMap` (`GetAsync`, `SetAsync`, `UpdateAsync`, `RemoveAsync`, `ListItemsAsync`). Limits: a value of at most 32 KB serialised; expiry from 0 to 3,888,000 seconds (45 days, also the default); at most 1,000,000 items and 100 MB per structure; a memory quota of 64 KB + 1.2 KB × players and 1,000 + 120 × players request units per minute for the whole place, with the unit costs the vendor documents (one per call, one per item returned by range and read calls, at least two for a hash map update). Over the quota, calls that grow the data fail and reads and shrinking calls succeed. The vendor reports these failures by name (`TotalRequestsOverLimit`, `ItemValueSizeTooLarge`, `TotalMemoryOverLimit` and the rest); our message starts with that name, then our wording and `RE9403`.
- **The data clock.** Expiry, budgets and the `CreatedTime`/`UpdatedTime` fields read one clock. In `real run` and the editor it is wall-clock time. In `real verify`, replays and `play.run` it is a fixed start (`[compat.data] epoch`, default 2026-01-01T00:00:00Z) plus simulation time, and the run reads its data from a fresh copy of `data/seed/` in the project (committed, optional) instead of `.real/data/`, which it never writes. So D1 and D2 replays repeat, and a verify run cannot be changed by what an earlier play saved.
- **`[compat.data]`** in `real.toml`: `enabled` (default true), `latency_frames` (default 1), `epoch`, and `mode` = `"persist"` (default for `real run`: read and write `.real/data/`) or `"session"` (start from the seed and keep nothing). The `CompatConfig` field is a shared-file change the maintainer makes.

**2. `SurfaceGui` and `ViewportFrame` render to textures.**
- **One path for both.** Each visible `SurfaceGui` and `ViewportFrame` gets an offscreen `Capture` from a pool, redrawn only when its subtree, its camera or its size changed since its last draw, or when it contains something that animates. A per-frame budget (`[compat.gui] surface_redraws_per_frame`, default 8) spreads redraws over frames; anything over it keeps last frame's texture.
- **`SurfaceGui`.** Laid out by the same GUI layout code as a `ScreenGui`, into a canvas of `CanvasSize` pixels (`SizingMode` `FixedSize`) or the face's size in studs × `PixelsPerStud` (`PixelsPerStud`, default 50). The egui shapes are painted into the capture, which is drawn as a textured quad on the `Face` of the `Adornee` (or of the parent part), in the 3D pass and depth-tested. `ZOffset` becomes a small depth bias; `LightInfluence` blends between the unlit texture and the scene's lighting; `Brightness` scales the unlit term; `AlwaysOnTop` draws the quad after the 3D pass without depth testing; `MaxDistance` hides it. The texture is capped at 2048 pixels per side; a larger canvas is scaled down once with `RE9410`, as is a `SurfaceGui` with no part to sit on.
- **Input on surfaces.** A click or touch is ray-cast from the camera; when the first hit is a face that carries a `SurfaceGui` held in the local player's `PlayerGui`, the hit point becomes canvas coordinates and goes through the same GUI input path as a `ScreenGui` (`Activated`, `MouseButton1Click`, hover). A `SurfaceGui` that lives in `Workspace` draws but takes no input in this WP; its `partial` note says so.
- **`ViewportFrame`.** Its descendant parts (including those under a `WorldModel`) are collected into a separate `RenderSnapshot` with the frame's `CurrentCamera`, lit by `Ambient` plus one directional light (`LightColor`, `LightDirection`) and no shadows, particles, beams, sky or post effects, rendered by a second `Renderer::render` call into the frame's capture at its on-screen pixel size (capped at 1024 per side), and painted as an image in the GUI with `ImageColor3` and `ImageTransparency`. A frame with no `CurrentCamera` draws nothing. Parts in a viewport are never simulated. `WorldModel`'s spatial queries are `stub-error` in this WP.
- **Headless.** Captures work on lavapipe, so goldens for a surface and a viewport run in `cargo test` and in `real verify`.

**3. Changed compat scripts restart; they do not reload in place.**
- In `real run` and the editor, the existing `AssetWatcher` reports changed files under `scripts/`. For a changed `Script` or `LocalScript`, the compat runtime runs its normal `stop()` (connections, binds, callbacks and threads owned by the script go) and then `start()` with the new source. Instances the old run created stay, as they do when a script is disabled and enabled again.
- A changed `ModuleScript` is dropped from every context's module cache, and every running script that required it, directly or through other modules, restarts the same way. Restarting is the only safe choice for compat code: a module's return value is captured in closures all over the place, and the vendor's model has no reload protocol to hand state across.
- A source that does not compile leaves the old run going and reports `RE9420` with the compiler's message; the next good save restarts it.
- `Enabled` (and its deprecated inverse `Disabled`) are added to `Script` and `LocalScript`. Setting it false stops the script; setting it true starts it again from the top, so the "starts at most once" rule in `runtime/scripts.rs` becomes "at most once per enable". This closes the WP11 follow-up.
- Reload never runs in `real verify`, replays or `play.run`: they read files once, so they stay deterministic.
- Hooking compat plays into `App::apply_file_changes` is a small `re_app` change, through a maintainer-reviewed PR.

**4. Place files are read by our own reader in `re_import`.**
- `real import` accepts `.rbxl`/`.rbxm` (binary) and `.rbxlx`/`.rbxmx` (XML). A place's top-level instances become services, as in the other importers; a model's roots go under `Workspace`, or under the dotted path in `--parent`. The output is the same project the Rojo and loader imports write (`real.toml`, the scene, and each script's source as a file under `scripts/` with a `ScriptRef`), so everything after the reader is shared code.
- **Binary format facts the reader relies on** (restated for this repository):
  - a 32-byte header: the 8-byte magic `<roblox!`, the 6 bytes `89 ff 0d 0a 1a 0a`, a little-endian `u16` version 0, the class count and instance count as `i32`, and 8 reserved zero bytes;
  - then chunks, each with a 16-byte header: a 4-byte name padded with zeros, the compressed length, the uncompressed length and 4 reserved bytes. A compressed length of 0 means the body is stored raw. Otherwise the body is a ZSTD frame when it starts with `28 b5 2f fd`, and a bare LZ4 block (no frame) otherwise;
  - `META` (string pairs), `SSTR` (shared strings, each after a 16-byte hash we ignore), one `INST` per class (class id, class name, a service flag, and the instances' referents), one `PROP` per class and property (class id, the serialised property name, a type id, and one value per instance of the class), one `PRNT` (paired arrays of child and parent referents, with -1 for a root), and `END`, whose body is `</roblox>` and is never compressed;
  - value arrays use three encodings: 32- and 64-bit integers are zigzag-transformed, big-endian and byte-interleaved (all first bytes, then all second bytes, and so on); 32-bit floats move the sign bit to the lowest bit before the same interleaving; referent arrays are interleaved integers stored as running differences, so they are summed while reading;
  - a `CFrame` value is a one-byte rotation id, followed by nine raw little-endian floats only when the id is 0; the 24 non-zero ids name axis-aligned rotations, and the positions follow afterwards as one interleaved `Vector3` array for the whole column;
  - the type ids we decode are 0x01 to 0x0E, 0x10 and 0x12 to 0x22 (string, bool, int32, float32, float64, `UDim`, `UDim2`, `Ray`, `Faces`, `Axes`, `BrickColor`, `Color3`, `Vector2`, `Vector3`, `CFrame`, enum, referent, `Vector3int16`, `NumberSequence`, `ColorSequence`, `NumberRange`, `Rect`, `PhysicalProperties`, `Color3uint8`, int64, shared string, bytecode, optional `CFrame`, `UniqueId`, `Font`, and the newer content type). An unknown type id skips that property for that class with `RE9803`, never the file.
- **XML format facts:** one `roblox` element with `version="4"`; `Item` elements with `class` and a file-unique `referent` (the literal `null` is reserved for an empty reference); one `Properties` element per item whose children are typed elements carrying the property in a `name` attribute (`CoordinateFrame` with twelve children `X` to `R22`, `Ref`, `token` for enums, `BinaryString` and shared strings in base64, `ProtectedString` for script source, and so on); an optional `SharedStrings` table keyed by its `md5` attribute; `Meta` and the legacy `External` elements are read and ignored.
- **Serialised names.** Files store some properties under names scripts do not see (lower-case or legacy names, and colour stored as `Color3uint8`). `crates/re_import/data/serialized_names.json` maps them to our class properties, written by hand from the classes our data covers (ADR-0034). A property with no mapping is listed in the import report with `RE9804`, never dropped without a word.
- **What the reader does not import.** Terrain voxel data and CSG solid data are kept out of the scene and listed in the report (terrain voxels are an M3 non-goal). Asset ids stay ids and resolve through the substitute table (`RE9350` at use), as they do today.
- **Untrusted input.** Every length is checked against the bytes left before anything is allocated; a chunk may expand to at most 256 MB, a file to at most 10,000,000 instances; references to missing referents and parent cycles are errors with the chunk and offset (`RE9802`). The reader never panics on bad input: the tests feed it truncated and bit-flipped files built from our synthetic fixtures.
- **Fixtures** are synthetic: a small test-only writer in `re_import/tests/` produces binary and XML files for each type id from a hand-written instance tree, and the tests read them back. The private samples are checked by the private kit only (ADR-0033).
- **New dependencies**, rows for `docs/DEPENDENCIES.md` (versions checked against crates.io on 2026-10-15):

  | Crate | Version | License | MSRV | For | Features |
  |---|---|---|---|---|---|
  | `lz4_flex` | 0.14.0 | MIT | 1.81 | LZ4 chunk bodies | `default-features = false`, `std`, `safe-decode`, `checked-decode` |
  | `ruzstd` | 0.9.1 | MIT | 1.87 | ZSTD chunk bodies, pure Rust | `default-features = false`, `std` |
  | `quick-xml` | 0.41.0 | MIT | 1.79 | `.rbxlx`/`.rbxmx` | defaults; the version already in the lockfile through `wayland-scanner`, so no duplicate (0.42.0 is newest) |
  | `base64` | 0.23.1 | MIT OR Apache-2.0 | 1.71 | binary and shared strings in XML | the version already in the lockfile through `rmcp` |

  All four are used only by `re_import`, a tooling crate, so none ships in a packaged game (WP19).

**5. Skinned `MeshPart`s and compat post effects use WP21's and WP20's public APIs.**
- `MeshPart` and `Bone` get class rows. A `MeshPart` whose `MeshId` resolves (through the substitute table) to a glTF file with a skin becomes a skinned draw item on WP21's `SkinData`; each `Bone` under the part maps to the joint of the same name, and its `Transform` (written by the compat `Animator` or by scripts) drives that joint. A mesh without a skin draws as a rigid mesh. A bone name the skin does not have, or a joint with no bone, is `RE9440` once per part. The vendor's own mesh format is not read: offline there is no way to fetch it, and projects bring glTF through substitutes.
- `BloomEffect` (`Intensity`, `Size`, `Threshold`) maps onto WP20's bloom settings. `BlurEffect` (`Size`) and `SunRaysEffect` (`Intensity`, `Spread`) need two settings WP20's frozen post contract does not list yet; WP27 asks WP20 (through the maintainer, as a new defaulted field) for a full-screen blur radius and a sun-rays pass. If WP20 does not add them, those two stay `partial`. `DepthOfFieldEffect` gets a row as `partial` ("stored; not drawn") unless WP20 adds depth of field.
- Effects are read from `Lighting` and from the current camera. When several enabled effects of one class are present, the first in tree order is used and `RE9441` is logged once; the vendor's stacking rules are not modelled.
- Effects do not touch simulation, so D2 is unaffected.

**6. Codes and order.**
- `re_compat` block `RE9400`–`RE9449` (reserved by the maintainer in `src/codes.rs`): `RE9400` data request rejected, `RE9401` data request queue full, `RE9402` data file unreadable, `RE9403` memory store request failed, `RE9410` surface or viewport cannot be drawn as asked, `RE9420` changed script did not compile, `RE9440` skin and bones do not match, `RE9441` stacked post effects. The rest stay free.
- `re_import` keeps `RE9800`–`RE9899`: `RE9801` now covers only extensions it does not know; new are `RE9802` malformed place file, `RE9803` unknown value type, `RE9804` unmapped serialised property.
- PRs land in the order persistence, reload, place files, surfaces, skins and effects. "surfaces" needs the `re_render` change below; "skins and effects" waits on WP21 "skins" and WP20 "post".

**Changes WP27 needs from other owners.**
- Maintainer: the codes and `explain` pages; new class rows under `src/class/` where the shared code needs them (`DataStore`, `OrderedDataStore`, `DataStoreKeyInfo`, `DataStorePages`, the three memory store classes and their pages, `SurfaceGui` members, `ViewportFrame`, `WorldModel`, `MeshPart`, `Bone`, `DepthOfFieldEffect`); `CompatConfig`'s `data` and `gui` sections; the four `DEPENDENCIES.md` rows; the `re_app` hook for compat reload.
- WP20 (`re_render`): a way to use a `Capture` as the texture of a world-space quad (a texture handle from a render target, drawn depth-tested or on top), for `SurfaceGui`; the blur and sun-rays settings above.
- WP21: nothing beyond its "skins" contract.

**Alternatives.**
- **rbx_binary and rbx_xml 3.0.1** (MIT, maintained by the Rojo project). Rejected: both pull `rbx_reflection_database`, a packed copy of the vendor's API reflection data (including defaults) regenerated for each vendor release, which duplicates our class data and conflicts with ADR-0034's rule that such facts are written by us; they also bring `zstd` (C, through `zstd-sys`), `xml-rs`, `ahash`, `log` and `dirs`, and convert into their own DOM types that we would then convert again. Our reader is a few modules over two pure-Rust decompressors.
- **The `zstd` crate** for ZSTD bodies. Rejected: it builds the C library on all three platforms; `ruzstd` decodes in pure Rust, and decoding is all we need.
- **One SQLite file for data stores.** Rejected: a new C dependency for small data; per-store JSON files are readable, diffable and easy to seed.
- **Memory stores in RAM only.** Rejected: the plan asks for local persistence of both services, and a play that restarts would lose queues mid-use. Keeping expiry on the data clock makes persisting them safe.
- **Drawing `SurfaceGui` meshes straight into the 3D pass** (transforming egui's triangles onto the face). Crisper text, but it needs its own pipeline for egui's textures and clipping in 3D and does nothing for `ViewportFrame`; render-to-texture serves both, as the plan says.
- **Reloading compat modules in place**, as the native runtime does. Rejected: compat code has no hook to carry state across, and a half-replaced module graph is worse than a clean restart.

**Consequences.**
- Save and load paths in compat games run locally, with the same yields, limits and error numbers scripts check for, and verify runs stay repeatable through the seed directory and the data clock.
- Budgets match one server only. A game that is fine locally can still hit the vendor's per-experience or per-key limits online.
- Surfaces and viewports cost one offscreen render each when they change. Many animated surfaces will hit the redraw budget and update at a lower rate, by design.
- Script reload is a restart, so edits during play lose the edited script's local state; modules restart everything that uses them.
- Place files import without Studio, Rojo or network access. Anything we cannot map is in the report, not lost silently. The format is undocumented by the vendor, so a future vendor change can need a reader update; unknown chunks and type ids degrade to warnings, not failures.
- Four small MIT-family crates join the tooling build; none ships in the runtime.

## Open questions for the owner

1. **License.**
   - The repository has no `LICENSE` file. The crates declare `MIT OR Apache-2.0`, the render prototype's value.
   - Recommendation: dual MIT / Apache-2.0, the usual choice for Rust, which matches most crates' defaults and the prototype. Add `LICENSE-MIT` and `LICENSE-APACHE` at the root.
   - This also needs a decision on the exceptions in ADR-0019. MPL-2.0 code ships **in the runtime** through one crate, kira's unconditional dependency `triple_buffer` (used unmodified), not only in tooling (full_moon). kira's `wav` feature and its symphonia crates have been dropped (`re_audio` decodes WAV itself). egui's default fonts bring OFL-1.1 and Ubuntu-font-1.0, and the compat GUI bundles three OFL-1.1 font families as files (ADR-0035). Do you accept these, or should we replace kira or the default fonts?
   - Until the owner decides, the workspace `license` field keeps that value, and the README says the license is not final.
2. **Which Luau type checker do we ship?** The engine cannot type-check in process today, because mlua's vendored Luau has no Analysis module (ADR-0013). The options:
   - **(a) An external official checker** (the current default). luau-lsp 1.70.1 works unpatched and also serves editors; a patched luau-analyze 0.740 (27-line `--definitions` patch, a 220 s source build) is the alternative. Both are MIT tools, not code dependencies, so the crates.io-only rule does not apply, but users must install them (or we bundle the release binaries), and the patched analyzer needs a rebase on each Luau upgrade.
   - **(b) Vendor Luau's Analysis sources** (MIT) into a crate of our own and build them with `cc`. This gives in-process checking at mlua's exact Luau version, on every platform, with no external binary. It is a C++ build we would maintain.
   - **(c) The `luau-analyze` 0.0.1 crate.** The scripting spike rejected it: few downloads, strict-only, single-file, behind mlua's Luau version, no Windows support, and it cannot link into the same binary as mlua.
   - Recommendation: (a) with luau-lsp for M1 and M2, and revisit (b) if bundling external binaries proves painful.
3. **Must the external sample scenes' scripts run unchanged?** **Resolved (owner: "Demo first").** M1 ships the playable demo on the engine's own API; M2's goal is the sample scenes running through the Roblox-style layer (ADR-0012, Accepted). Still open: whether a mechanical migration step on the way is acceptable. The background below is kept for that part.
   - The plan is an engine-native Luau API in M1 and a Roblox-style compatibility layer in M2 (ADR-0012).
   - "Unchanged" means every global, service, class, event, enum and datatype those scripts touch must exist with matching behaviour, including yielding calls and deprecated members.
   - The alternative is "runs after a mechanical migration done by a tool". It is much cheaper, and it could be offered as a `real migrate` command.
   - The answer to the migration question sets the size of M2. Until it is answered, [M2_PLAN.md](M2_PLAN.md) plans for running unchanged.
4. **Windows and macOS priority.** When should these platforms start blocking merges? M3 is the current plan.
